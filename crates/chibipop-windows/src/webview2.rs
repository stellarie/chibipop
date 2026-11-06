//! WebView2 runtime detection and child launch.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::{PCWSTR, w};

const CLIENTS_WOW64: &str =
    r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const CLIENTS_NATIVE: &str =
    r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const CLIENTS_USER: &str =
    r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

const SETTINGS_EXE: &str = "chibipop-settings.exe";

/// Return the path of the settings child beside the daemon.
pub fn settings_exe() -> PathBuf {
    chibipop::paths::beside_exe(SETTINGS_EXE)
}

/// Return true when a WebView2 runtime is registered.
pub fn runtime_present() -> bool {
    [
        (HKEY_LOCAL_MACHINE, CLIENTS_WOW64),
        (HKEY_LOCAL_MACHINE, CLIENTS_NATIVE),
        (HKEY_CURRENT_USER, CLIENTS_USER),
    ]
    .into_iter()
    .filter_map(|(root, subkey)| read_pv(root, subkey))
    .any(|version| is_usable_version(&version))
}

/// Run the settings child and wait for it to exit.
pub fn spawn_settings(exe: &Path, config: &Path, dict: &Path) -> Result<ExitStatus> {
    Command::new(exe)
        .arg("--config")
        .arg(config)
        .arg("--dict")
        .arg(dict)
        .status()
        .with_context(|| format!("running {}", exe.display()))
}

fn is_usable_version(version: &str) -> bool {
    let version = version.trim();
    !version.is_empty() && version != "null" && version != "0.0.0.0"
}

fn read_pv(root: HKEY, subkey: &str) -> Option<String> {
    let subkey: Vec<u16> = subkey.encode_utf16().chain(Some(0)).collect();
    let subkey = PCWSTR(subkey.as_ptr());
    let mut bytes = 0u32;
    // SAFETY: `subkey` is null-terminated and outlives both calls.
    // The first call writes only `bytes`. The second call writes at most
    // `bytes` bytes into `buf`, which holds `bytes` bytes rounded up.
    unsafe {
        let probe = RegGetValueW(root, subkey, w!("pv"), RRF_RT_REG_SZ, None, None, Some(&mut bytes));
        if probe != ERROR_SUCCESS || bytes == 0 {
            return None;
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
        let read = RegGetValueW(
            root,
            subkey,
            w!("pv"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        );
        if read != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}
