use super::{
    backend_identity, cpu_totals, resource_report, timestamp_now, unix_millis,
    CleanupOutcome, ProcessConfig, ResourceOutcome, ResourceRow,
};
use anyhow::{bail, Context, Result};
use nix::errno::Errno;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone)]
struct ProcessInfo {
    pid: u32,
    parent_pid: u32,
    process_group: u32,
    name: String,
    start_ticks: u64,
    cpu_ticks: u64,
}

struct Metrics {
    working_set_bytes: u64,
    private_bytes: u64,
    threads: u64,
    handles: u64,
}

pub fn monitor(config: &ProcessConfig) -> Result<serde_json::Value> {
    let mut child = spawn(config)?;
    let root_pid = child.id();
    let started_at = SystemTime::now();
    let deadline = Instant::now()
        .checked_add(config.duration)
        .unwrap_or_else(Instant::now);
    let clock_ticks = clock_ticks_per_second();
    let logical = std::thread::available_parallelism().map_or(1, |count| count.get()) as f64;
    let mut rows = Vec::new();
    let mut observed = HashSet::new();
    let mut identities = HashMap::<u32, u64>::new();
    let mut previous_cpu = HashMap::<u32, (f64, Instant)>::new();
    let mut failure_categories = Vec::new();
    let mut child_exit_code = None;
    let mut child_exited = false;
    let mut timed_out = false;
    let mut sampling_error = None;

    // Ticks precede wait.
    match process_table() {
        Ok(table) => {
            let root = table.iter().find(|item| item.pid == root_pid);
            match root {
                Some(info) => {
                    observed.insert(root_pid);
                    identities.insert(root_pid, info.start_ticks);
                }
                None => {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            child_exit_code = status.code();
                            child_exited = true;
                        }
                        Ok(None) => {
                            sampling_error = Some("spawned root is absent from /proc".to_string());
                        }
                        Err(error) => {
                            sampling_error = Some(format!(
                                "root is absent and waiting failed: {error}"
                            ));
                        }
                    }
                }
            }
        }
        Err(error) => {
            sampling_error = Some(format!("enumerating /proc failed: {error}"));
        }
    }

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
        let now = Instant::now();
        let table = match process_table() {
            Ok(table) => table,
            Err(error) => {
                sampling_error = Some(format!("enumerating /proc failed: {error}"));
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
                failure_categories.push("resource-metric-missing".to_string());
                failure_categories.push("cleanup-identity-unverified".to_string());
                continue;
            };
            if let Some(previous) = identities.get(&id) {
                if *previous != info.start_ticks {
                    failure_categories.push("cleanup-identity-mismatch".to_string());
                    continue;
                }
            } else {
                identities.insert(id, info.start_ticks);
            }
            let metrics = match process_metrics(info) {
                Ok(metrics) => metrics,
                Err(_) => {
                    failure_categories.push("resource-metric-missing".to_string());
                    failure_categories.push("cleanup-identity-unverified".to_string());
                    live.push(missing_metrics(info, id == root_pid));
                    continue;
                }
            };
            let cpu_seconds = info.cpu_ticks as f64 / clock_ticks;
            let cpu_percent_one_core = previous_cpu.get(&id).and_then(|(previous, at)| {
                let seconds = now.saturating_duration_since(*at).as_secs_f64();
                (seconds > 0.0).then(|| 100.0 * (cpu_seconds - previous) / seconds)
            });
            previous_cpu.insert(id, (cpu_seconds, now));
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
                parent_pid: info.parent_pid,
                process_name: info.name.clone(),
                start_ticks: Some(info.start_ticks),
                executable_path: Some(info.name.clone()),
                working_set_bytes: Some(metrics.working_set_bytes),
                private_bytes: Some(metrics.private_bytes),
                cpu_seconds: Some(cpu_seconds),
                cpu_percent_one_core,
                cpu_percent: cpu_percent_one_core.map(|value| value / logical),
                threads: Some(metrics.threads),
                handles: Some(metrics.handles),
            });
        }
        rows.extend(live.clone());
        if let Some(total) = total_row(&live, &phase, &identity, config) {
            rows.push(total);
        }

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
    command.process_group(0);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command.spawn().with_context(|| {
        format!("launching OCR benchmark {}", super::redact_path(&config.program))
    })
}

fn process_table() -> Result<Vec<ProcessInfo>> {
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc").context("reading /proc")? {
        let entry = entry.context("reading /proc entry")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let path = entry.path();
        match process_info(&path, pid) {
            Ok(process) => processes.push(process),
            Err(error) if process_disappeared(&path, &error) => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading process {pid} from /proc")
                });
            }
        }
    }
    Ok(processes)
}

fn process_disappeared(path: &Path, error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
        && !path.exists()
}

fn process_info(path: &Path, pid: u32) -> Result<ProcessInfo> {
    let stat = std::fs::read_to_string(path.join("stat"))?;
    let close = stat.rfind(')').context("malformed /proc stat")?;
    let name = stat[1..close].to_string();
    let fields = stat
        .get(close + 2..)
        .context("missing /proc stat fields")?
        .split_whitespace()
        .collect::<Vec<_>>();
    if fields.len() <= 19 {
        bail!("short /proc stat");
    }
    let parent_pid = fields[1].parse::<u32>()?;
    let cpu_ticks = fields[11].parse::<u64>()?.saturating_add(fields[12].parse::<u64>()?);
    let start_ticks = fields[19].parse::<u64>()?;
    Ok(ProcessInfo {
        pid,
        parent_pid,
        process_group: fields[2].parse::<u32>()?,
        name,
        start_ticks,
        cpu_ticks,
    })
}

fn process_metrics(info: &ProcessInfo) -> Result<Metrics> {
    let path = Path::new("/proc").join(info.pid.to_string());
    let status = std::fs::read_to_string(path.join("status"))?;
    let working_set_bytes = status_value(&status, "VmRSS")?;
    let threads = status_value(&status, "Threads")?;
    let smaps = std::fs::read_to_string(path.join("smaps_rollup"))?;
    let private_bytes = private_bytes(&smaps)?;
    let handles = u64::try_from(std::fs::read_dir(path.join("fd"))?.count())?;
    Ok(Metrics {
        working_set_bytes,
        private_bytes,
        threads,
        handles,
    })
}

fn missing_metrics(info: &ProcessInfo, parent: bool) -> ResourceRow {
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
        role: if parent { "parent" } else { "descendant" }.to_string(),
        pid: info.pid,
        parent_pid: info.parent_pid,
        process_name: info.name.clone(),
        start_ticks: Some(info.start_ticks),
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

fn clock_ticks_per_second() -> f64 {
    // SAFETY: sysconf reads immutable process configuration.
    let value = unsafe { nix::libc::sysconf(nix::libc::_SC_CLK_TCK) };
    if value > 0 { value as f64 } else { 100.0 }
}

fn status_value(status: &str, key: &str) -> Result<u64> {
    let line = status.lines().find(|line| line.starts_with(key))
        .with_context(|| format!("missing /proc status field {key}"))?;
    let value = line.split_whitespace().nth(1).context("missing status value")?.parse::<u64>()?;
    if key == "VmRSS" {
        value.checked_mul(1024).context("status byte count overflow")
    } else {
        Ok(value)
    }
}

fn private_bytes(smaps: &str) -> Result<u64> {
    let clean = smaps.lines().find(|line| line.starts_with("Private_Clean:"))
        .context("missing Private_Clean")?.split_whitespace().nth(1)
        .context("missing Private_Clean value")?.parse::<u64>()?;
    let dirty = smaps.lines().find(|line| line.starts_with("Private_Dirty:"))
        .context("missing Private_Dirty")?.split_whitespace().nth(1)
        .context("missing Private_Dirty value")?.parse::<u64>()?;
    clean
        .checked_add(dirty)
        .and_then(|value| value.checked_mul(1024))
        .context("private byte count overflow")
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
        working_set_bytes: Some(rows.iter().filter_map(|row| row.working_set_bytes).sum()),
        private_bytes: Some(rows.iter().filter_map(|row| row.private_bytes).sum()),
        cpu_seconds: Some(rows.iter().filter_map(|row| row.cpu_seconds).sum()),
        cpu_percent_one_core: cpu_totals(rows).0,
        cpu_percent: cpu_totals(rows).1,
        threads: Some(rows.iter().filter_map(|row| row.threads).sum()),
        handles: Some(rows.iter().filter_map(|row| row.handles).sum()),
    })
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
            let root_safe = inspect_current_tree(
                root_pid,
                &table,
                observed,
                identities,
                &mut candidates,
                &mut outcome,
            );
            if root_identity_allows_group_signal(root_pid, identities, root_safe) {
                let signal = signal_process_group(root_pid, Some(&table));
                if let Err(error) = signal {
                    outcome.signal_error = Some(error.to_string());
                    record_error(&mut outcome, error.to_string());
                }
            } else {
                record_error(
                    &mut outcome,
                    "root identity is not safe for process-group cleanup".to_string(),
                );
            }
            terminate_descendants(
                &table,
                root_pid,
                &candidates,
                identities,
                &mut outcome,
            );
            if (timed_out || force_kill) && root_safe {
                terminate_root(child, &mut outcome);
            } else if timed_out || force_kill {
                let _ = child.kill();
                if let Err(error) = child.wait() {
                    record_error(
                        &mut outcome,
                        format!("waiting for cleanup child failed: {error}"),
                    );
                }
            }
        }
        Err(error) => {
            if root_identity_allows_group_signal(root_pid, identities, true) {
                let signal = signal_process_group(root_pid, None);
                if let Err(signal_error) = signal {
                    outcome.signal_error = Some(signal_error.to_string());
                    record_error(&mut outcome, signal_error.to_string());
                }
            } else {
                record_error(
                    &mut outcome,
                    "root identity is not anchored for cleanup".to_string(),
                );
            }
            record_error(
                &mut outcome,
                format!("enumerating /proc during cleanup failed: {error}"),
            );
            if timed_out || force_kill {
                terminate_root(child, &mut outcome);
            }
        }
    }
    std::thread::sleep(Duration::from_millis(100));
    let after = match process_table() {
        Ok(table) => table,
        Err(error) => {
            record_error(
                &mut outcome,
                format!("enumerating /proc after cleanup failed: {error}"),
            );
            Vec::new()
        }
    };
    if !after.is_empty() {
        inspect_current_tree(
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
        match identities.get(&pid) {
            Some(expected) if *expected == info.start_ticks => {
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

fn inspect_current_tree(
    root_pid: u32,
    table: &[ProcessInfo],
    observed: &mut HashSet<u32>,
    identities: &mut HashMap<u32, u64>,
    candidates: &mut HashSet<u32>,
    outcome: &mut CleanupOutcome,
) -> bool {
    let Some(root) = table.iter().find(|info| info.pid == root_pid) else {
        for info in table.iter().filter(|info| info.process_group == root_pid) {
            observed.insert(info.pid);
            candidates.insert(info.pid);
            check_identity(info, identities, outcome);
        }
        return true;
    };
    let Some(expected) = identities.get(&root_pid) else {
        outcome.identity_unverified_ids.push(root_pid);
        record_error(outcome, "root identity was not anchored".to_string());
        return false;
    };
    if *expected != root.start_ticks {
        outcome.identity_mismatch_ids.push(root_pid);
        record_error(outcome, "root identity changed during cleanup".to_string());
        return false;
    }
    let mut ids = tree_ids(root_pid, table);
    for info in table.iter().filter(|info| info.process_group == root_pid) {
        if !ids.contains(&info.pid) {
            ids.push(info.pid);
        }
    }
    for pid in ids {
        observed.insert(pid);
        candidates.insert(pid);
        if let Some(info) = table.iter().find(|item| item.pid == pid) {
            check_identity(info, identities, outcome);
        }
    }
    true
}

fn root_identity_allows_group_signal(
    root_pid: u32,
    identities: &HashMap<u32, u64>,
    root_safe: bool,
) -> bool {
    root_safe && identities.contains_key(&root_pid)
}

fn check_identity(
    info: &ProcessInfo,
    identities: &mut HashMap<u32, u64>,
    outcome: &mut CleanupOutcome,
) {
    match identities.get(&info.pid) {
        Some(expected) if *expected != info.start_ticks => {
            outcome.identity_mismatch_ids.push(info.pid);
        }
        Some(_) => {}
        None => {
            identities.insert(info.pid, info.start_ticks);
        }
    }
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
        let Some(info) = table.iter().find(|item| item.pid == pid) else {
            continue;
        };
        let Some(expected) = identities.get(&pid) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        if *expected != info.start_ticks {
            outcome.identity_mismatch_ids.push(pid);
            continue;
        }
        let Ok(pid_i32) = i32::try_from(pid) else {
            outcome.identity_unverified_ids.push(pid);
            continue;
        };
        match kill(Pid::from_raw(pid_i32), Signal::SIGKILL) {
            Ok(()) => outcome.stopped_ids.push(pid),
            Err(Errno::ESRCH) => {}
            Err(error) => {
                outcome.identity_unverified_ids.push(pid);
                record_error(outcome, format!("terminating process {pid} failed: {error}"));
            }
        }
    }
}

fn terminate_root(child: &mut Child, outcome: &mut CleanupOutcome) {
    let kill_error = child.kill().err();
    if let Err(wait_error) = child.wait() {
        let message = kill_error.map_or_else(
            || format!("waiting for cleanup child failed: {wait_error}"),
            |kill_error| {
                format!(
                    "cleanup child terminate failed: {kill_error}; wait failed: {wait_error}"
                )
            },
        );
        record_error(outcome, message);
    }
}

fn signal_process_group(root_pid: u32, table: Option<&[ProcessInfo]>) -> Result<()> {
    let root = i32::try_from(root_pid).context("process group id exceeds i32")?;
    let group = root.checked_neg().context("process group id is invalid")?;
    match kill(Pid::from_raw(group), Signal::SIGKILL) {
        Ok(()) => Ok(()),
        Err(Errno::ESRCH)
            if table.is_some_and(|items| {
                !items.iter().any(|info| info.process_group == root_pid)
            }) => Ok(()),
        Err(error) => Err(anyhow::anyhow!(
            "signaling OCR process group {root_pid} failed: {error}"
        )),
    }
}

fn record_error(outcome: &mut CleanupOutcome, error: String) {
    if outcome.error.is_none() {
        outcome.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_bytes_add_clean_and_dirty_pages() {
        let bytes = private_bytes("Private_Clean: 2 kB\nPrivate_Dirty: 3 kB\n").unwrap();
        assert_eq!(bytes, 5 * 1024);
    }

    #[test]
    fn process_stat_parser_reads_the_current_process() {
        let process = process_info(Path::new("/proc/self"), std::process::id()).unwrap();
        assert_eq!(process.pid, std::process::id());
        assert!(process.start_ticks > 0);
    }

    #[test]
    fn process_tree_walk_keeps_only_descendants() {
        let info = |pid, parent_pid| ProcessInfo {
            pid,
            parent_pid,
            process_group: 1,
            name: "test".to_string(),
            start_ticks: 1,
            cpu_ticks: 1,
        };
        assert_eq!(tree_ids(10, &[info(10, 1), info(11, 10), info(12, 11), info(13, 2)]).len(), 3);
    }

    #[test]
    fn root_anchor_source_precedes_child_wait() {
        let source = include_str!("process_linux.rs");
        let anchor = source
            .find("identities.insert(root_pid, info.start_ticks)")
            .expect("root anchor");
        let wait = source.find("child.try_wait").expect("child wait");
        assert!(anchor < wait);
    }

    #[test]
    fn mismatched_root_blocks_process_group_signal() {
        let table = [ProcessInfo {
            pid: 7,
            parent_pid: 1,
            process_group: 7,
            name: "root".to_string(),
            start_ticks: 2,
            cpu_ticks: 0,
        }];
        let mut observed = HashSet::new();
        let mut identities = HashMap::from([(7, 1)]);
        let mut candidates = HashSet::new();
        let mut outcome = CleanupOutcome {
            stopped_ids: Vec::new(),
            remaining_ids: Vec::new(),
            identity_mismatch_ids: Vec::new(),
            identity_unverified_ids: Vec::new(),
            error: None,
            signal_error: None,
        };
        let root_safe = inspect_current_tree(
            7,
            &table,
            &mut observed,
            &mut identities,
            &mut candidates,
            &mut outcome,
        );
        assert!(!root_safe);
        assert!(outcome.identity_mismatch_ids.contains(&7));
        assert!(!root_identity_allows_group_signal(7, &identities, root_safe));
    }
}
