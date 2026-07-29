//! The live lookup log console.

use windows::core::BOOL;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Console::{
    GetConsoleProcessList, GetConsoleWindow, SetConsoleCtrlHandler,
};
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOW};

/// The console, if it is ours alone.
///
/// A shell shares its console with us; that window belongs to the user's
/// terminal and must never be shown, hidden, or claimed.
fn own_console() -> Option<HWND> {
    // SAFETY: GetConsoleProcessList writes at most `pids.len()` entries and
    // returns the true count, which may exceed it - only the comparison with
    // 1 matters, so a truncated write cannot mislead. GetConsoleWindow
    // returns null when there is no console, covered by is_invalid.
    unsafe {
        let mut pids = [0u32; 4];
        if GetConsoleProcessList(&mut pids) != 1 {
            return None;
        }
        let hwnd = GetConsoleWindow();
        if hwnd.is_invalid() {
            None
        } else {
            Some(hwnd)
        }
    }
}

/// Close must hide, not exit.
unsafe extern "system" fn ctrl_handler(_event: u32) -> BOOL {
    hide();
    BOOL(1)
}

/// Shows the log window.
pub fn show() {
    let Some(hwnd) = own_console() else { return };
    // SAFETY: `hwnd` came from GetConsoleWindow and was checked valid.
    // Registering the handler more than once is harmless - Windows keeps a
    // list and ours is idempotent.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

/// Hides it. Never frees it.
pub fn hide() {
    let Some(hwnd) = own_console() else { return };
    // SAFETY: `hwnd` came from GetConsoleWindow and was checked valid.
    // Freeing instead would invalidate stdout, and println! aborts on a
    // failed write.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}
