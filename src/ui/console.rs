//! The live lookup log console.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    DeleteMenu, GetSystemMenu, ShowWindow, MF_BYCOMMAND, SC_CLOSE, SW_HIDE, SW_SHOW,
};

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

/// Shows the log window.
pub fn show() {
    let Some(hwnd) = own_console() else { return };
    // SAFETY: `hwnd` came from GetConsoleWindow and was checked valid.
    // A CTRL_CLOSE_EVENT handler returning TRUE does not save the
    // process - HandlerRoutine's own docs say the system terminates it
    // regardless, no other handler called. So the close item is removed
    // instead of handled: with SC_CLOSE gone from the system menu, the X
    // is greyed out and there is no close event to ever answer.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let menu = GetSystemMenu(hwnd, false);
        if !menu.is_invalid() {
            let _ = DeleteMenu(menu, SC_CLOSE, MF_BYCOMMAND);
        }
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
