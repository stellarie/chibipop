//! System tray icon: the app's only user-facing control surface (spec
//! section 6) - a right-click menu to open Settings and to quit.
//! Once this exists, `app.rs` removes its Task 7 interim `q`+Enter console
//! quit - see that file's module docs.
//!
//! **Why `create()` failing is a hard startup failure.** Per spec section 6:
//! "Tray icon creation fails -> Hard fail - it is the only way to change
//! mode or quit." A running process with no tray is a process nobody can
//! control short of Task Manager - worse than not starting at all.
//!
//! **Why the menu's owner is not the render popup's `HWND`.** `TrackPopupMenu`
//! requires its owner to become the foreground window, or the menu will not
//! dismiss when the user clicks elsewhere - the classic Win32 trap (MS
//! KB135788): `SetForegroundWindow` before showing it, and a null message
//! posted after, are both required, not optional (see `show_menu`).
//! `ui::window::Popup`'s `HWND` is `WS_EX_NOACTIVATE`, click-through
//! (`WS_EX_TRANSPARENT`), normally hidden, and that module's entire contract
//! elsewhere in this codebase is "never take focus, ever." MSDN's own words
//! on `WS_EX_NOACTIVATE` ("To activate the window, use ... SetForegroundWindow")
//! say an explicit call still works on such a window, so reusing it would
//! probably not even misbehave - but deliberately foregrounding the one
//! window this codebase works hard to keep inert is a real coupling smell: a
//! later change to `Popup`'s activation flags, made purely for rendering
//! reasons, could silently break menu dismissal with no obvious link back to
//! this file. `Tray` instead creates and owns a second, tiny, always-invisible
//! window purely to be `TrackPopupMenu`/`SetForegroundWindow`'s target. The
//! `hwnd` passed into `create()` (the popup's own window) is still used, for
//! the one thing that genuinely is harmless to share: `NOTIFYICONDATAW.hWnd`,
//! the window Shell32 posts the tray's callback message to. That message is
//! intercepted by message ID in `app::run`'s loop before any dispatch - see
//! `handle_message` - so which real window "receives" it has no functional
//! effect.
//!
//! **Why `handle_message` needs no `wparam`.** The menu is shown and read
//! synchronously with `TrackPopupMenuEx(..., TPM_RETURNCMD | TPM_NONOTIFY,
//! ...)`: no `WM_COMMAND` is ever posted for a menu pick - that is what
//! `TPM_RETURNCMD` buys - the chosen item's id comes back as this call's own
//! return value instead. So the one thing `handle_message` needs from the
//! event that triggered it is which mouse message fired, and that arrives in
//! `lparam` for the tray's own callback message (Shell32's documented
//! "version 0" callback shape) - no `wparam` needed at all.

use anyhow::{Context, Result};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Shell32's tray callback message. `WM_APP + 1` is already
/// `app.rs`'s `WM_APP_RESULT`; `+ 2` keeps this one distinct.
const WM_TRAYICON: u32 = WM_APP + 2;

const ID_SETTINGS: u32 = 1001;
const ID_QUIT: u32 = 1003;

/// This process only ever shows one icon.
const TRAY_UID: u32 = 1;

/// chibipop's own tray icon (see `assets/chibipop.svg` for the source
/// artwork) - the `.ico` container built from it, 16/32/48/256px
/// PNG-format frames. `load_tray_icon` embeds this rather than using
/// the OS's generic `IDI_APPLICATION`.
const ICON_BYTES: &[u8] = include_bytes!("../../assets/chibipop.ico");

/// What the user picked from the tray menu. `app::run`'s loop matches on
/// this and is the only thing that touches `AppState`/`Hooks`/the TOML -
/// `Tray` itself never does.
pub enum TrayCommand {
    OpenSettings,
    Quit,
}

fn owner_class_name() -> PCWSTR {
    w!("ChibipopTrayOwnerClass")
}

/// This window is never shown and never dispatches anything but its own
/// creation/destruction and the foreground-window dance `show_menu` drives -
/// `DefWindowProcW` handles all of that correctly on its own. A trampoline is
/// needed only because `DefWindowProcW` itself is a plain `unsafe fn`
/// wrapper, not the `unsafe extern "system" fn` pointer shape `WNDPROC`
/// requires.
unsafe extern "system" fn owner_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Registers the owner-window class exactly once per process - the same
/// success-latched-only-after-`RegisterClassExW`-succeeds pattern as
/// `ui::window::register_class`, and for the same reason (Task 1's review
/// caught a real bug from latching too early: a failed first attempt would
/// otherwise stick every later `create()` with a class that was never
/// actually registered).
unsafe fn register_owner_class(hinstance: HINSTANCE) -> Result<()> {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::SeqCst) {
        return Ok(());
    }

    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(owner_wndproc),
            hInstance: hinstance,
            lpszClassName: owner_class_name(),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            return Err(Error::from_thread())
                .context("RegisterClassExW for the tray owner window");
        }
    }

    REGISTERED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Owns the notification-area icon and a small hidden window used only to
/// host its callback message and its right-click menu. See the module docs
/// for why that window is not the render popup's own `HWND`.
pub struct Tray {
    /// `NOTIFYICONDATAW.hWnd` - see the module docs on why this may safely
    /// be (and in practice always is) the popup's own window.
    notify_hwnd: HWND,
    uid: u32,
    /// `TrackPopupMenu`/`SetForegroundWindow`'s target. Owned by this
    /// `Tray`, destroyed in `Drop`.
    menu_owner: HWND,
    /// The notification-area icon's handle - chibipop's own mascot when
    /// `hicon_owned`, the OS's shared default otherwise. Destroyed in
    /// `Drop`, but only when owned - see that impl for why.
    hicon: HICON,
    /// Whether `hicon` came from `CreateIconFromResourceEx` (an owned
    /// handle, which must be destroyed) rather than `LoadIconW` (a
    /// shared handle, which must not be - see `load_tray_icon`).
    hicon_owned: bool,
}

impl Tray {
    /// Adds the notification-area icon and creates the hidden menu-owner
    /// window. `Err` here must be treated by the caller as a hard startup
    /// failure (spec section 6) - see the module docs.
    ///
    /// **Cleanup ordering is load-bearing on the error paths.** The icon is
    /// loaded *first*, before the owner window exists, so a failure to load
    /// has nothing else to clean up; swapping the two would leak the owner
    /// window on that one path. In exchange, because `load_tray_icon` can
    /// return an **owned** handle, every fallible step after it must destroy
    /// that handle on its own error path - `create` returns `Err` before any
    /// `Tray` exists, so `Drop` never runs to do it for them.
    pub fn create(hwnd: HWND) -> Result<Tray> {
        unsafe {
            let hinstance: HINSTANCE =
                GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();

            // First: nothing to unwind yet.
            let (hicon, hicon_owned) = load_tray_icon()?;

            if let Err(e) = register_owner_class(hinstance) {
                if hicon_owned {
                    let _ = DestroyIcon(hicon);
                }
                return Err(e);
            }

            let menu_owner_result = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                owner_class_name(),
                w!("chibipop tray"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW for the tray owner window");
            let menu_owner = match menu_owner_result {
                Ok(h) => h,
                Err(e) => {
                    if hicon_owned {
                        let _ = DestroyIcon(hicon);
                    }
                    return Err(e);
                }
            };

            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                uFlags: NIF_ICON | NIF_TIP | NIF_MESSAGE,
                uCallbackMessage: WM_TRAYICON,
                hIcon: hicon,
                ..Default::default()
            };
            set_tip(&mut nid, "chibipop");

            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                let _ = DestroyWindow(menu_owner);
                if hicon_owned {
                    let _ = DestroyIcon(hicon);
                }
                anyhow::bail!(
                    "Shell_NotifyIconW(NIM_ADD) failed - no tray icon, and the tray is the only \
                     way to change mode or quit"
                );
            }

            Ok(Tray {
                notify_hwnd: hwnd,
                uid: TRAY_UID,
                menu_owner,
                hicon,
                hicon_owned,
            })
        }
    }

    /// Called by `app::run`'s loop for every message it does not already
    /// claim itself (`WM_TIMER`, `WM_APP_RESULT`). Returns `None` for
    /// anything that is not this tray's own callback message, or that is,
    /// but produced no selection (dismissed, or a left-click) - the caller
    /// dispatches those normally either way.
    ///
    /// `before_blocking` runs immediately before `TrackPopupMenuEx`'s own
    /// message pump takes over this thread - see `show_menu` for why.
    pub fn handle_message(
        &self,
        msg: u32,
        lparam: LPARAM,
        before_blocking: impl FnOnce(),
    ) -> Option<TrayCommand> {
        if msg != WM_TRAYICON {
            return None;
        }
        match lparam.0 as u32 {
            WM_RBUTTONUP => self.show_menu(before_blocking),
            _ => None,
        }
    }

    /// Builds a fresh menu (so the radio state always matches the current
    /// mode), shows it, and translates the user's pick into a `TrayCommand`.
    /// Blocks until the menu closes - `TrackPopupMenuEx`'s own message pump.
    ///
    /// That pump is `GetMessage`/`DispatchMessage` too, but it only ever
    /// routes messages to a real `HWND` - a thread message (`hwnd` NULL,
    /// same shape as `app.rs`'s `WM_APP_CAPTURE_GUARD` wake-up) is picked up
    /// and discarded with nowhere to go, not left on the queue for `run`'s
    /// own loop to find once this call returns. `before_blocking` is the
    /// caller's chance to service anything that wake-up would otherwise
    /// have triggered, *before* this call can swallow it - see I4.
    fn show_menu(&self, before_blocking: impl FnOnce()) -> Option<TrayCommand> {
        unsafe {
            let hmenu = match build_menu() {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("chibipop: building the tray menu failed: {e:#}");
                    return None;
                }
            };

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);

            // The foreground-window trap (MS KB135788): TrackPopupMenu's
            // owner must be the foreground window or the menu will not
            // dismiss when the user clicks away from it. SetForegroundWindow
            // before, a null message posted after - both required, neither
            // optional. See the module docs for why `menu_owner` is a
            // dedicated window rather than the render popup's `HWND`.
            let _ = SetForegroundWindow(self.menu_owner);

            before_blocking();

            let flags = (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0;
            let cmd = TrackPopupMenuEx(hmenu, flags, pt.x, pt.y, self.menu_owner, None);

            let _ = PostMessageW(Some(self.menu_owner), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(hmenu);

            match cmd.0 as u32 {
                ID_SETTINGS => Some(TrayCommand::OpenSettings),
                ID_QUIT => Some(TrayCommand::Quit),
                _ => None, // dismissed with no selection
            }
        }
    }
}

impl Drop for Tray {
    /// Removes the notification-area icon, destroys the owner window,
    /// and - if owned - the icon resource itself. Deliberately
    /// best-effort (errors swallowed): by the time `Drop` runs there is
    /// nothing left to hand an `Err` to, matching `input::hooks`'s own
    /// `Drop` (see that module's comment for the fuller argument). This
    /// is the fix for the exact bug the brief calls out: without it, the
    /// icon outlives the process and sits as a ghost in the notification
    /// area until the user happens to hover it.
    fn drop(&mut self) {
        unsafe {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.notify_hwnd,
                uID: self.uid,
                ..Default::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = DestroyWindow(self.menu_owner);

            if self.hicon_owned {
                // SAFETY: reached only after Shell_NotifyIconW(NIM_DELETE)
                // above, so the shell can no longer be displaying this
                // icon - destroying it any earlier would be the exact
                // ordering bug this function exists to avoid. `hicon_owned`
                // is true only when `hicon` came from
                // CreateIconFromResourceEx (an owned handle this process
                // must free); LoadIconW's shared handle always leaves it
                // false, so this branch can never DestroyIcon a shared one.
                let _ = DestroyIcon(self.hicon);
            }
        }
    }
}

/// Copies `text` into `nid.szTip`, UTF-16 + NUL-terminated, truncated to fit
/// if ever necessary (it never is for `"chibipop"`, but `szTip` is a fixed
/// 128-`u16` array and a silent overrun is not an acceptable way to find
/// that out).
fn set_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    // Truncating must not eat the NUL Shell32 reads to.
    let cap = nid.szTip.len();
    let mut wide: Vec<u16> = text.encode_utf16().take(cap - 1).collect();
    wide.push(0);
    nid.szTip[..wide.len()].copy_from_slice(&wide);
}

/// Builds the right-click menu fresh: Settings, a separator, and Quit.
///
/// The trigger mode used to live here as two radio items, and moved into the
/// settings window when that window gained the other ten settings - it was
/// only ever on the menu because it happened to be the first setting that
/// needed changing at runtime, not because it is the most important.
///
/// Destroys the partially-built `HMENU` on any
/// failure so a construction error can never leak it.
unsafe fn build_menu() -> Result<HMENU> {
    unsafe {
        let hmenu = CreatePopupMenu().context("CreatePopupMenu")?;
        if let Err(e) = populate_menu(hmenu) {
            let _ = DestroyMenu(hmenu);
            return Err(e);
        }
        Ok(hmenu)
    }
}

unsafe fn populate_menu(hmenu: HMENU) -> Result<()> {
    unsafe {
        AppendMenuW(hmenu, MF_STRING, ID_SETTINGS as usize, w!("Settings…"))
            .context("AppendMenuW Settings")?;
        AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).context("AppendMenuW separator")?;
        AppendMenuW(hmenu, MF_STRING, ID_QUIT as usize, w!("Quit")).context("AppendMenuW Quit")?;
    }
    Ok(())
}

/// Loads chibipop's own tray icon, falling back to the OS's shared
/// default on any failure - only the fallback itself failing is a hard
/// error here, matching the contract `create()` already had for this
/// step before this icon existed (module docs: tray creation failing
/// is the one hard startup failure). Returns whether *this* call owns
/// the returned handle: `true` from `CreateIconFromResourceEx` (an
/// owned handle, which `Drop` must destroy), `false` from `LoadIconW`
/// (a shared handle, which `Drop` must never destroy).
unsafe fn load_tray_icon() -> Result<(HICON, bool)> {
    if let Some(hicon) = unsafe { load_embedded_icon() } {
        return Ok((hicon, true));
    }
    eprintln!("chibipop: could not load the tray's own icon, using the default");
    let hicon =
        unsafe { LoadIconW(None, IDI_APPLICATION) }.context("LoadIconW(IDI_APPLICATION)")?;
    Ok((hicon, false))
}

/// Picks the embedded `.ico`'s frame closest to the system's small-icon
/// size and turns it into an owned `HICON`. `None` on any failure -
/// malformed directory, out-of-range offsets, or `CreateIconFromResourceEx`
/// itself rejecting the bytes - so the caller can fall back cleanly.
unsafe fn load_embedded_icon() -> Option<HICON> {
    let desired = unsafe { GetSystemMetrics(SM_CXSMICON) } as u32;
    let bytes = ico_frame_bytes(ICON_BYTES, desired)?;
    // SAFETY: `bytes` is a sub-slice of `ICON_BYTES`, a `'static`
    // buffer embedded in this binary, so the pointer and length
    // `CreateIconFromResourceEx` reads from stay valid for the whole
    // call - the only precondition its docs place on `presbits` /
    // `dwResSize`. `0x0003_0000` is `dwVer`'s documented "generally
    // set to" value (MS Learn, CreateIconFromResourceEx).
    let icon =
        unsafe { CreateIconFromResourceEx(bytes, true, 0x0003_0000, 0, 0, LR_DEFAULTCOLOR) };
    icon.ok()
}

/// Finds the `.ico` frame closest in size to `desired` and returns its
/// raw bytes. `.ico` files and `RT_ICON` resources share one directory
/// format - a 6-byte header then one 16-byte entry per frame (MS Learn,
/// "Resource File Formats") - read directly here rather than pulling in
/// a crate for it. `None` on any malformed input rather than panicking,
/// so a bad asset degrades to the caller's fallback icon instead of a
/// crash.
fn ico_frame_bytes(ico: &[u8], desired: u32) -> Option<&[u8]> {
    let header = ico.get(0..6)?;
    if header[0..4] != [0, 0, 1, 0] {
        return None; // reserved=0, type=1
    }
    let count = u16::from_le_bytes([header[4], header[5]]);

    let mut best: Option<(u32, &[u8])> = None;
    for i in 0..count {
        let off = 6 + usize::from(i) * 16;
        let entry = ico.get(off..off + 16)?;
        let width = if entry[0] == 0 { 256 } else { u32::from(entry[0]) };
        let nbytes = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        let end = offset.checked_add(nbytes)?;
        let bytes = ico.get(offset..end)?;
        let is_closer = best.is_none_or(|(w, _)| width.abs_diff(desired) < w.abs_diff(desired));
        if is_closer {
            best = Some((width, bytes));
        }
    }
    best.map(|(_, bytes)| bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shell32 reads szTip up to a NUL.
    #[test]
    fn a_tip_too_long_to_fit_is_still_terminated() {
        let mut nid = NOTIFYICONDATAW::default();
        set_tip(&mut nid, &"あ".repeat(500));
        assert_eq!(
            0,
            nid.szTip[nid.szTip.len() - 1],
            "a truncated tip must not cost the terminator"
        );
    }

    #[test]
    fn a_short_tip_round_trips() {
        let mut nid = NOTIFYICONDATAW::default();
        set_tip(&mut nid, "chibipop");
        let n = nid.szTip.iter().position(|&c| c == 0).expect("terminated");
        assert_eq!("chibipop", String::from_utf16_lossy(&nid.szTip[..n]));
    }
}
