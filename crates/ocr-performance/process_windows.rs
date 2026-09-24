use super::{
    backend_identity, cpu_totals, resource_report, timestamp_now, unix_millis,
    CleanupOutcome, ProcessConfig, ResourceOutcome, ResourceRow,
};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::windows::io::AsRawHandle;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    CREATE_TOOLHELP_SNAPSHOT_FLAGS, PROCESSENTRY32W, THREADENTRY32, TH32CS_SNAPPROCESS,
    TH32CS_SNAPTHREAD,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GetProcessHandleCount, GetProcessTimes, OpenProcess, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, PROCESS_TERMINATE,
};
use windows::core::HRESULT;

const ERROR_NO_MORE_FILES_HRESULT: HRESULT = HRESULT::from_win32(18);

#[derive(Clone)]
struct ProcessInfo {
    pid: u32,
    parent_pid: u32,
    name: String,
    threads: u64,
}

#[derive(Clone)]
struct Metrics {
    start_ticks: u64,
    working_set_bytes: u64,
    private_bytes: u64,
    cpu_seconds: f64,
    threads: u64,
    handles: u64,
    name: String,
    parent_pid: u32,
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: The handle came from a successful Win32 creator.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn monitor(config: &ProcessConfig) -> Result<serde_json::Value> {
    let mut child = spawn(config)?;
    let root_pid = child.id();
    let root_identity = child_start_ticks(&child);
    let started_at = SystemTime::now();
    let deadline = InstantExt::now().saturating_add(config.duration);
    let logical = std::thread::available_parallelism().map_or(1, |count| count.get()) as f64;
    let mut rows = Vec::new();
    let mut observed = HashSet::new();
    let mut identities = HashMap::new();
    let mut previous_cpu = HashMap::<u32, (f64, InstantExt)>::new();
    let mut failure_categories = Vec::new();
    let mut child_exit_code = None;
    let mut child_exited = false;
    let mut timed_out = false;
    let mut sampling_error = match root_identity {
        Ok(start_ticks) => {
            observed.insert(root_pid);
            identities.insert(root_pid, start_ticks);
            None
        }
        Err(error) => Some(format!("anchoring root process identity failed: {error}")),
    };
    // Anchor precedes wait.

    while sampling_error.is_none() && !child_exited {
        match child.try_wait() {
            Ok(Some(status)) => {
                child_exit_code = status.code();
                child_exited = true;
                break;
            }
            Ok(None) => {}
            Err(error) => {
                sampling_error = Some(format!(
                    "waiting for the benchmark child failed: {error}"
                ));
                break;
            }
        }
        let now = InstantExt::now();
        let table = match process_table() {
            Ok(table) => table,
            Err(error) => {
                sampling_error = Some(format!(
                    "enumerating the Windows process table failed: {error}"
                ));
                break;
            }
        };
        let ids = tree_ids(root_pid, &table);
        for id in &ids {
            observed.insert(*id);
        }
        let identity = backend_identity(&config.identity_file);
        let phase_data = super::read_phase_events(&config.phase_file);
        if let Some(error) = phase_data.error {
            sampling_error = Some(error);
            break;
        }
        let phase = super::phase_name_at(phase_data.events, unix_millis());
        let mut live = Vec::new();
        for id in ids {
            let Some(info) = table.iter().find(|item| item.pid == id) else {
                if id == root_pid {
                    if let Ok(Some(status)) = child.try_wait() {
                        child_exit_code = status.code();
                        child_exited = true;
                        continue;
                    }
                }
                failure_categories.push("cleanup-identity-unverified".to_string());
                continue;
            };
            let metrics = match process_metrics(info) {
                Ok(metrics) => metrics,
                Err(_) => {
                    failure_categories.push("resource-metric-missing".to_string());
                    failure_categories.push("cleanup-identity-unverified".to_string());
                    live.push(missing_metrics(info));
                    continue;
                }
            };
            if let Some(previous) = identities.get(&id) {
                if *previous != metrics.start_ticks {
                    failure_categories.push("cleanup-identity-mismatch".to_string());
                    continue;
                }
            } else {
                identities.insert(id, metrics.start_ticks);
            }
            let cpu_percent_one_core = previous_cpu.get(&id).and_then(|(previous, at)| {
                let seconds = now.duration_since(*at).as_secs_f64();
                (seconds > 0.0).then(|| 100.0 * (metrics.cpu_seconds - previous) / seconds)
            });
            previous_cpu.insert(id, (metrics.cpu_seconds, now));
            live.push(ResourceRow {
                timestamp: timestamp_now(),
                phase: phase.clone(),
                backend_id: string_field(&identity, "id", &config.backend),
                backend_version: string_field(&identity, "version", &config.backend),
                language: identity
                    .get("language")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                thread_settings: compact_field(&identity, "thread_settings"),
                scales: scales_field(&identity),
                fixture_sha256: config.fixture_sha256.clone(),
                model_hashes: compact_field(&identity, "model_hashes"),
                plugin_hashes: compact_field(&identity, "plugin_hashes"),
                runner_image: config
                    .runner
                    .get("image")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("local")
                    .to_string(),
                role: if id == root_pid { "parent" } else { "descendant" }.to_string(),
                pid: id,
                parent_pid: metrics.parent_pid,
                process_name: metrics.name.clone(),
                start_ticks: Some(metrics.start_ticks),
                executable_path: Some(metrics.name.clone()),
                working_set_bytes: Some(metrics.working_set_bytes),
                private_bytes: Some(metrics.private_bytes),
                cpu_seconds: Some(metrics.cpu_seconds),
                cpu_percent_one_core,
                cpu_percent: cpu_percent_one_core.map(|value| value / logical),
                threads: Some(metrics.threads),
                handles: Some(metrics.handles),
            });
        }
        rows.extend(live.clone());
        rows.extend(total_row(&live, &phase, &identity, config));

        match child.try_wait() {
            Ok(Some(status)) => {
                child_exit_code = status.code();
                child_exited = true;
                break;
            }
            Ok(None) => {}
            Err(error) => {
                sampling_error = Some(format!(
                    "waiting for the benchmark child failed: {error}"
                ));
                break;
            }
        }
        if now >= deadline {
            timed_out = true;
            break;
        }
        std::thread::sleep(config.sample_interval);
    }

    let cleanup = cleanup(
        &mut child,
        root_pid,
        &mut observed,
        &mut identities,
        timed_out,
        sampling_error.is_some(),
    );
    if sampling_error.is_some() {
        failure_categories.push("resource-report-missing".to_string());
    }
    if timed_out {
        failure_categories.push("timeout".to_string());
    }
    if child_exited && child_exit_code != Some(0) {
        failure_categories.push("child-failure".to_string());
    }
    if !cleanup.remaining_ids.is_empty() {
        failure_categories.push("cleanup-survivor".to_string());
    }
    if !cleanup.identity_mismatch_ids.is_empty() {
        failure_categories.push("cleanup-identity-mismatch".to_string());
    }
    if !cleanup.identity_unverified_ids.is_empty() || cleanup.error.is_some() {
        failure_categories.push("cleanup-identity-unverified".to_string());
    }
    Ok(resource_report(
        config,
        ResourceOutcome {
            started_at,
            root_pid,
            child_exit_code,
            child_exited,
            timed_out,
            rows,
            cleanup,
            sampling_error,
            failure_categories,
        },
    ))
}

fn spawn(config: &ProcessConfig) -> Result<Child> {
    let mut command = Command::new(&config.program);
    command.args(&config.arguments);
    command.envs(&config.environment);
    if !config.environment.contains_key("CHIBIPOP_BENCH_PLUGIN") {
        command.env_remove("CHIBIPOP_BENCH_PLUGIN");
    }
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().with_context(|| {
        format!("launching OCR benchmark {}", super::redact_path(&config.program))
    })
}

fn process_table() -> Result<Vec<ProcessInfo>> {
    let flags = CREATE_TOOLHELP_SNAPSHOT_FLAGS(TH32CS_SNAPPROCESS.0 | TH32CS_SNAPTHREAD.0);
    // SAFETY: The flags and process id follow the ToolHelp contract.
    let snapshot = unsafe { CreateToolhelp32Snapshot(flags, 0) }?;
    let snapshot = Handle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = Vec::new();
    // SAFETY: `entry` has the required size and remains valid for the call.
    match unsafe { Process32FirstW(snapshot.0, &mut entry) } {
        Ok(()) => loop {
            processes.push(ProcessInfo {
                pid: entry.th32ProcessID,
                parent_pid: entry.th32ParentProcessID,
                name: utf16_name(&entry.szExeFile),
                threads: u64::from(entry.cntThreads),
            });
            // SAFETY: `entry` remains valid for the enumeration.
            match unsafe { Process32NextW(snapshot.0, &mut entry) } {
                Ok(()) => {}
                Err(error) if error.code() == ERROR_NO_MORE_FILES_HRESULT => break,
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.code() == ERROR_NO_MORE_FILES_HRESULT => {}
        Err(error) => return Err(error.into()),
    }
    let mut thread_counts = HashMap::<u32, u64>::new();
    let mut thread = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: `thread` has the required size and remains valid for the call.
    match unsafe { Thread32First(snapshot.0, &mut thread) } {
        Ok(()) => loop {
            *thread_counts.entry(thread.th32OwnerProcessID).or_default() += 1;
            // SAFETY: `thread` remains valid for the enumeration.
            match unsafe { Thread32Next(snapshot.0, &mut thread) } {
                Ok(()) => {}
                Err(error) if error.code() == ERROR_NO_MORE_FILES_HRESULT => break,
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) if error.code() == ERROR_NO_MORE_FILES_HRESULT => {}
        Err(error) => return Err(error.into()),
    }
    for process in &mut processes {
        if let Some(count) = thread_counts.get(&process.pid) {
            process.threads = *count;
        }
    }
    Ok(processes)
}

fn utf16_name(raw: &[u16]) -> String {
    let end = raw.iter().position(|value| *value == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..end])
}

fn tree_ids(root: u32, table: &[ProcessInfo]) -> Vec<u32> {
    let mut found = HashSet::new();
    let mut pending = VecDeque::from([root]);
    while let Some(parent) = pending.pop_front() {
        if !found.insert(parent) {
            continue;
        }
        for child in table.iter().filter(|item| item.parent_pid == parent) {
            pending.push_back(child.pid);
        }
    }
    found.into_iter().collect()
}

fn process_metrics(info: &ProcessInfo) -> Result<Metrics> {
    let desired = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
    // SAFETY: The pid comes from a current ToolHelp snapshot.
    let process = unsafe { OpenProcess(desired, false, info.pid) }?;
    let process = Handle(process);
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `counters` is the documented EX layout and its size is declared.
    unsafe {
        GetProcessMemoryInfo(
            process.0,
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            counters.cb,
        )?
    };
    let start_ticks = process_start_ticks(process.0)?;
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    // SAFETY: All FILETIME pointers are valid writable locals.
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }?;
    let mut handles = 0u32;
    // SAFETY: `handles` is a valid output pointer.
    unsafe { GetProcessHandleCount(process.0, &mut handles) }?;
    Ok(Metrics {
        start_ticks,
        working_set_bytes: counters.WorkingSetSize as u64,
        private_bytes: counters.PrivateUsage as u64,
        cpu_seconds: filetime_value(kernel)
            .saturating_add(filetime_value(user)) as f64
            / 10_000_000.0,
        threads: info.threads,
        handles: u64::from(handles),
        name: info.name.clone(),
        parent_pid: info.parent_pid,
    })
}

fn child_start_ticks(child: &Child) -> Result<u64> {
    let raw_handle = child.as_raw_handle();
    if raw_handle.is_null() {
        bail!("the child process handle is null");
    }
    let process = HANDLE(raw_handle);
    let start_ticks = process_start_ticks(process)?;
    Ok(start_ticks)
}

fn process_start_ticks(process: HANDLE) -> Result<u64> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: All FILETIME pointers are valid writable locals.
    unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }?;
    let start_ticks = filetime_value(created);
    if start_ticks == 0 {
        bail!("the process creation time is empty");
    }
    Ok(start_ticks)
}

fn missing_metrics(info: &ProcessInfo) -> ResourceRow {
    ResourceRow {
        timestamp: timestamp_now(),
        phase: "unknown".to_string(),
        backend_id: "unknown".to_string(),
        backend_version: "unknown".to_string(),
        language: None,
        thread_settings: None,
        scales: None,
        fixture_sha256: String::new(),
        model_hashes: None,
        plugin_hashes: None,
        runner_image: "local".to_string(),
        role: "descendant".to_string(),
        pid: info.pid,
        parent_pid: info.parent_pid,
        process_name: info.name.clone(),
        start_ticks: None,
        executable_path: None,
        working_set_bytes: None,
        private_bytes: None,
        cpu_seconds: None,
        cpu_percent_one_core: None,
        cpu_percent: None,
        threads: None,
        handles: None,
    }
}

fn total_row(
    rows: &[ResourceRow],
    phase: &str,
    identity: &serde_json::Value,
    config: &ProcessConfig,
) -> Option<ResourceRow> {
    if rows.is_empty() {
        return None;
    }
    Some(ResourceRow {
        timestamp: timestamp_now(),
        phase: phase.to_string(),
        backend_id: string_field(identity, "id", &config.backend),
        backend_version: string_field(identity, "version", &config.backend),
        language: identity.get("language").and_then(serde_json::Value::as_str).map(str::to_string),
        thread_settings: compact_field(identity, "thread_settings"),
        scales: scales_field(identity),
        fixture_sha256: config.fixture_sha256.clone(),
        model_hashes: compact_field(identity, "model_hashes"),
        plugin_hashes: compact_field(identity, "plugin_hashes"),
        runner_image: config
            .runner
            .get("image")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("local")
            .to_string(),
        role: "total".to_string(),
        pid: 0,
        parent_pid: 0,
        process_name: "process-tree-total".to_string(),
        start_ticks: None,
        executable_path: None,
        working_set_bytes: sum_u64(rows.iter().filter_map(|row| row.working_set_bytes)),
        private_bytes: sum_u64(rows.iter().filter_map(|row| row.private_bytes)),
        cpu_seconds: sum_f64(rows.iter().filter_map(|row| row.cpu_seconds)),
        cpu_percent_one_core: cpu_totals(rows).0,
        cpu_percent: cpu_totals(rows).1,
        threads: sum_u64(rows.iter().filter_map(|row| row.threads)),
        handles: sum_u64(rows.iter().filter_map(|row| row.handles)),
    })
}

fn sum_u64(values: impl Iterator<Item = u64>) -> Option<u64> {
    Some(values.fold(0u64, u64::saturating_add))
}

fn sum_f64(values: impl Iterator<Item = f64>) -> Option<f64> {
    Some(values.fold(0.0, |total, value| total + value))
}

fn string_field(value: &serde_json::Value, key: &str, fallback: &serde_json::Value) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .or_else(|| fallback.get(key).and_then(serde_json::Value::as_str))
        .unwrap_or("unknown")
        .to_string()
}

fn compact_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).map(|item| serde_json::to_string(item).unwrap_or_else(|_| "null".to_string()))
}

fn scales_field(value: &serde_json::Value) -> Option<String> {
    value
        .get("scales")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_i64)
                .map(|item| item.to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
}

fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn cleanup(
    child: &mut Child,
    root_pid: u32,
    observed: &mut HashSet<u32>,
    identities: &mut HashMap<u32, u64>,
    timed_out: bool,
    force_kill: bool,
) -> CleanupOutcome {
    let mut outcome = CleanupOutcome {
        stopped_ids: Vec::new(),
        remaining_ids: Vec::new(),
        identity_mismatch_ids: Vec::new(),
        identity_unverified_ids: Vec::new(),
        error: None,
        signal_error: None,
    };
    let mut candidates = observed.clone();
    match process_table() {
        Ok(table) => {
            if discover_tree(
                root_pid,
                &table,
                observed,
                identities,
                &mut candidates,
                &mut outcome,
            ) {
                terminate_descendants(
                    &table,
                    root_pid,
                    &candidates,
                    identities,
                    &mut outcome,
                );
            }
        }
        Err(error) => record_error(
            &mut outcome,
            format!("enumerating the Windows process table failed: {error}"),
        ),
    }
    if timed_out || force_kill {
        match process_table() {
            Ok(table) => {
                if discover_tree(
                    root_pid,
                    &table,
                    observed,
                    identities,
                    &mut candidates,
                    &mut outcome,
                ) {
                    terminate_descendants(
                        &table,
                        root_pid,
                        &candidates,
                        identities,
                        &mut outcome,
                    );
                }
            }
            Err(error) => record_error(
                &mut outcome,
                format!("enumerating the Windows process table failed: {error}"),
            ),
        }
        terminate_root(child, &mut outcome);
    }
    std::thread::sleep(Duration::from_millis(100));
    let after = match process_table() {
        Ok(table) => table,
        Err(error) => {
            record_error(
                &mut outcome,
                format!("enumerating the Windows process table after cleanup failed: {error}"),
            );
            Vec::new()
        }
    };
    if !after.is_empty() {
        discover_tree(
            root_pid,
            &after,
            observed,
            identities,
            &mut candidates,
            &mut outcome,
        );
    }
    let mut candidate_ids = candidates.into_iter().collect::<Vec<_>>();
    candidate_ids.sort_unstable();
    for pid in candidate_ids {
        let Some(info) = after.iter().find(|item| item.pid == pid) else {
            continue;
        };
        let Ok(metrics) = process_metrics(info) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        match identities.get(&pid) {
            Some(expected) if *expected == metrics.start_ticks => {
                outcome.remaining_ids.push(pid);
            }
            Some(_) => outcome.identity_mismatch_ids.push(pid),
            None => outcome.identity_unverified_ids.push(pid),
        }
    }
    outcome.stopped_ids.sort_unstable();
    outcome.stopped_ids.dedup();
    outcome.remaining_ids.sort_unstable();
    outcome.remaining_ids.dedup();
    outcome.identity_mismatch_ids.sort_unstable();
    outcome.identity_mismatch_ids.dedup();
    outcome.identity_unverified_ids.sort_unstable();
    outcome.identity_unverified_ids.dedup();
    outcome
}

fn discover_tree(
    root_pid: u32,
    table: &[ProcessInfo],
    observed: &mut HashSet<u32>,
    identities: &mut HashMap<u32, u64>,
    candidates: &mut HashSet<u32>,
    outcome: &mut CleanupOutcome,
) -> bool {
    let Some(root) = table.iter().find(|info| info.pid == root_pid) else {
        return true;
    };
    let Ok(root_metrics) = process_metrics(root) else {
        outcome.identity_unverified_ids.push(root_pid);
        record_error(outcome, "root identity could not be read".to_string());
        return false;
    };
    match identities.get(&root_pid) {
        Some(expected) if *expected == root_metrics.start_ticks => {}
        Some(_) => {
            outcome.identity_mismatch_ids.push(root_pid);
            record_error(outcome, "root identity changed during cleanup".to_string());
            return false;
        }
        None => {
            outcome.identity_unverified_ids.push(root_pid);
            record_error(outcome, "root identity was not anchored".to_string());
            return false;
        }
    }
    for pid in tree_ids(root_pid, table) {
        observed.insert(pid);
        candidates.insert(pid);
        if pid == root_pid {
            continue;
        }
        let Some(info) = table.iter().find(|item| item.pid == pid) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        let Ok(metrics) = process_metrics(info) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        match identities.get(&pid) {
            Some(expected) if *expected != metrics.start_ticks => {
                outcome.identity_mismatch_ids.push(pid);
            }
            Some(_) => {}
            None => {
                identities.insert(pid, metrics.start_ticks);
            }
        }
    }
    true
}

fn terminate_descendants(
    table: &[ProcessInfo],
    root_pid: u32,
    candidates: &HashSet<u32>,
    identities: &HashMap<u32, u64>,
    outcome: &mut CleanupOutcome,
) {
    let mut pids = candidates
        .iter()
        .copied()
        .filter(|pid| *pid != root_pid)
        .collect::<Vec<_>>();
    pids.sort_unstable_by(|a, b| b.cmp(a));
    for pid in pids {
        let Some(_info) = table.iter().find(|item| item.pid == pid) else {
            continue;
        };
        let Some(expected) = identities.get(&pid) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        let desired = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: The access flags follow OpenProcess requirements.
        let Ok(handle) = (unsafe { OpenProcess(desired, false, pid) }) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        let handle = Handle(handle);
        let Ok(start_ticks) = process_start_ticks(handle.0) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        if *expected != start_ticks {
            outcome.identity_mismatch_ids.push(pid);
            continue;
        }
        // SAFETY: The handle is valid and names the identity-checked process.
        if unsafe { TerminateProcess(handle.0, 1) }.is_ok() {
            outcome.stopped_ids.push(pid);
        } else {
            outcome.identity_unverified_ids.push(pid);
        }
    }
}

fn terminate_root(child: &mut Child, outcome: &mut CleanupOutcome) {
    let kill_error = child.kill().err();
    if let Err(wait_error) = child.wait() {
        let message = kill_error.map_or_else(
            || format!("waiting for cleanup child failed: {wait_error}"),
            |kill_error| format!(
                "cleanup child terminate failed: {kill_error}; wait failed: {wait_error}"
            ),
        );
        record_error(outcome, message);
    }
}

fn record_error(outcome: &mut CleanupOutcome, error: String) {
    if outcome.error.is_none() {
        outcome.error = Some(error);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct InstantExt(std::time::Instant);

impl InstantExt {
    fn now() -> Self {
        Self(std::time::Instant::now())
    }

    fn saturating_add(self, duration: Duration) -> Self {
        Self(self.0.checked_add(duration).unwrap_or(self.0))
    }

    fn duration_since(self, earlier: Self) -> Duration {
        self.0.saturating_duration_since(earlier.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_handle_exposes_creation_identity_before_wait() {
        let mut child = Command::new("cmd.exe")
            .args(["/c", "ping", "127.0.0.1", "-n", "3"])
            .spawn()
            .expect("spawn identity test child");
        let start_ticks = child_start_ticks(&child).expect("child creation identity");
        assert!(start_ticks > 0);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn descendant_termination_keeps_identity_handle_open() {
        let source = include_str!("process_windows.rs");
        let start = source.find("fn terminate_descendants").expect("terminator");
        let end = source[start..].find("fn terminate_root").expect("terminator end");
        let body = &source[start..start + end];
        assert_eq!(body.matches("unsafe { OpenProcess").count(), 1);
        let identity = body.find("process_start_ticks(handle.0)").expect("identity");
        let terminate = body.find("TerminateProcess(handle.0, 1)").expect("terminate");
        assert!(identity < terminate);
        assert!(!body.contains("process_metrics(info)"));
    }
}
