//! This module shows the live Lookup log in the Windows console.

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    DeleteMenu, GetSystemMenu, ShowWindow, MF_BYCOMMAND, SC_CLOSE, SW_HIDE, SW_SHOW,
};

/// Returns the console when this process owns it alone.
///
/// A shell can share its console with this process. That window belongs to the
/// user's terminal, so this module must not show, hide, or claim it.
fn own_console() -> Option<HWND> {
    // SAFETY: GetConsoleProcessList writes at most `pids.len()` entries.
    // It returns the true count, which can exceed that length.
    // This code compares only the result with 1, so a truncated write cannot mislead.
    // GetConsoleWindow returns null when no console exists.
    // `is_invalid` handles that result.
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

/// Shows the Lookup log console.
pub fn show() {
    let Some(hwnd) = own_console() else { return };
    // SAFETY: `hwnd` came from GetConsoleWindow, and this code checked it.
    // A CTRL_CLOSE_EVENT handler that returns TRUE cannot save the process.
    // HandlerRoutine documentation says that the system terminates the process
    // regardless of the handler result. No other handler receives the event.
    // The code removes the close item, so no handler answers the event.
    // Without SC_CLOSE in the system menu, Windows grays out the X and sends
    // no close event.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let menu = GetSystemMenu(hwnd, false);
        if !menu.is_invalid() {
            let _ = DeleteMenu(menu, SC_CLOSE, MF_BYCOMMAND);
        }
    }
}

/// Hides the console. The process keeps the console for later output.
pub fn hide() {
    let Some(hwnd) = own_console() else { return };
    // SAFETY: `hwnd` came from GetConsoleWindow, and this code checked it.
    // If this code frees the console, stdout becomes invalid.
    // If a write fails, `println!` panics.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}
