//! System tray icon: the app's only user-facing control surface (spec
//! section 6) - a right-click menu to change trigger mode and to quit.
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

use crate::config::TriggerMode;
use anyhow::{Context, Result};
use std::cell::Cell;
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

const ID_MODE_LIVE: u32 = 1001;
const ID_MODE_HOLD_SHIFT: u32 = 1002;
const ID_QUIT: u32 = 1003;

/// This process only ever shows one icon.
const TRAY_UID: u32 = 1;

/// What the user picked from the tray menu. `app::run`'s loop matches on
/// this and is the only thing that touches `AppState`/`Hooks`/the TOML -
/// `Tray` itself never does.
pub enum TrayCommand {
    SetMode(TriggerMode),
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
    DefWindowProcW(hwnd, msg, wparam, lparam)
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

    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(owner_wndproc),
        hInstance: hinstance,
        lpszClassName: owner_class_name(),
        ..Default::default()
    };
    let atom = RegisterClassExW(&wc);
    if atom == 0 {
        return Err(Error::from_thread()).context("RegisterClassExW for the tray owner window");
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
    /// The mode the menu currently shows checked. Updated the moment this
    /// `Tray`'s own menu produces a `SetMode` - the only path that can ever
    /// change it - so it can never drift from what the menu last offered.
    mode: Cell<TriggerMode>,
}

impl Tray {
    /// Adds the notification-area icon and creates the hidden menu-owner
    /// window. `Err` here must be treated by the caller as a hard startup
    /// failure (spec section 6) - see the module docs.
    pub fn create(hwnd: HWND, initial_mode: TriggerMode) -> Result<Tray> {
        unsafe {
            let hinstance: HINSTANCE =
                GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();

            // Loaded before the owner window is created (below) specifically
            // so that a failure here has nothing to clean up yet - swapping
            // this order would leak the owner window on this particular
            // error path, since it would then be the only fallible step
            // between a successful CreateWindowExW and the NIM_ADD check
            // that already handles its own cleanup.
            let hicon = LoadIconW(None, IDI_APPLICATION).context("LoadIconW(IDI_APPLICATION)")?;

            register_owner_class(hinstance)?;

            let menu_owner = CreateWindowExW(
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
            .context("CreateWindowExW for the tray owner window")?;

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
                anyhow::bail!(
                    "Shell_NotifyIconW(NIM_ADD) failed - no tray icon, and the tray is the only \
                     way to change mode or quit"
                );
            }

            Ok(Tray { notify_hwnd: hwnd, uid: TRAY_UID, menu_owner, mode: Cell::new(initial_mode) })
        }
    }

    /// Called by `app::run`'s loop for every message it does not already
    /// claim itself (`WM_TIMER`, `WM_APP_RESULT`). Returns `None` for
    /// anything that is not this tray's own callback message, or that is,
    /// but produced no selection (dismissed, or a left-click) - the caller
    /// dispatches those normally either way.
    pub fn handle_message(&self, msg: u32, lparam: LPARAM) -> Option<TrayCommand> {
        if msg != WM_TRAYICON {
            return None;
        }
        match lparam.0 as u32 {
            WM_RBUTTONUP => self.show_menu(),
            _ => None,
        }
    }

    /// Builds a fresh menu (so the radio state always matches the current
    /// mode), shows it, and translates the user's pick into a `TrayCommand`.
    /// Blocks until the menu closes - `TrackPopupMenuEx`'s own message pump.
    fn show_menu(&self) -> Option<TrayCommand> {
        unsafe {
            let hmenu = match build_menu(self.mode.get()) {
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

            let flags = (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0;
            let cmd = TrackPopupMenuEx(hmenu, flags, pt.x, pt.y, self.menu_owner, None);

            let _ = PostMessageW(Some(self.menu_owner), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(hmenu);

            match cmd.0 as u32 {
                ID_MODE_LIVE => {
                    self.mode.set(TriggerMode::Live);
                    Some(TrayCommand::SetMode(TriggerMode::Live))
                }
                ID_MODE_HOLD_SHIFT => {
                    self.mode.set(TriggerMode::HoldShift);
                    Some(TrayCommand::SetMode(TriggerMode::HoldShift))
                }
                ID_QUIT => Some(TrayCommand::Quit),
                _ => None, // dismissed with no selection
            }
        }
    }
}

impl Drop for Tray {
    /// Removes the notification-area icon and destroys the owner window.
    /// Deliberately best-effort (errors swallowed): by the time `Drop` runs
    /// there is nothing left to hand an `Err` to, matching `input::hooks`'s
    /// own `Drop` (see that module's comment for the fuller argument). This
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
        }
    }
}

/// Copies `text` into `nid.szTip`, UTF-16 + NUL-terminated, truncated to fit
/// if ever necessary (it never is for `"chibipop"`, but `szTip` is a fixed
/// 128-`u16` array and a silent overrun is not an acceptable way to find
/// that out).
fn set_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let n = wide.len().min(nid.szTip.len());
    nid.szTip[..n].copy_from_slice(&wide[..n]);
}

/// Builds the right-click menu fresh: two radio-style mode items reflecting
/// `mode`, a separator, and Quit. Destroys the partially-built `HMENU` on any
/// failure so a construction error can never leak it.
unsafe fn build_menu(mode: TriggerMode) -> Result<HMENU> {
    let hmenu = CreatePopupMenu().context("CreatePopupMenu")?;
    if let Err(e) = populate_menu(hmenu, mode) {
        let _ = DestroyMenu(hmenu);
        return Err(e);
    }
    Ok(hmenu)
}

unsafe fn populate_menu(hmenu: HMENU, mode: TriggerMode) -> Result<()> {
    append_radio_item(hmenu, ID_MODE_LIVE, w!("Live"), mode == TriggerMode::Live)?;
    append_radio_item(
        hmenu,
        ID_MODE_HOLD_SHIFT,
        w!("Hold Shift"),
        mode == TriggerMode::HoldShift,
    )?;
    AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null()).context("AppendMenuW separator")?;
    AppendMenuW(hmenu, MF_STRING, ID_QUIT as usize, w!("Quit")).context("AppendMenuW Quit")?;
    Ok(())
}

/// Appends a string item, then marks it `MFT_RADIOCHECK` (a bullet, not a
/// checkmark - these two items are mutually exclusive, not independent
/// toggles) via a second call rather than `InsertMenuItemW`, so the item's
/// text never has to be re-encoded into a `PWSTR` this function would have
/// to keep alive - `AppendMenuW` already owns that via its own `PCWSTR`
/// parameter, which Windows copies internally.
unsafe fn append_radio_item(hmenu: HMENU, id: u32, label: PCWSTR, checked: bool) -> Result<()> {
    AppendMenuW(hmenu, MF_STRING, id as usize, label).context("AppendMenuW mode item")?;
    let mii = MENUITEMINFOW {
        cbSize: size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE | MIIM_STATE,
        fType: MFT_STRING | MFT_RADIOCHECK,
        fState: if checked { MFS_CHECKED } else { MFS_UNCHECKED },
        ..Default::default()
    };
    SetMenuItemInfoW(hmenu, id, false, &mii).context("SetMenuItemInfoW radio state")?;
    Ok(())
}
