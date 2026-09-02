//! The tray icon and its menu.
//!
//! The tray is the only way to change mode or to quit. A failure to create
//! the icon is therefore fatal.

use anyhow::{Context, Result};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// The callback message that Shell32 sends to the window.
const WM_TRAYICON: u32 = WM_APP + 2;

const ID_SETTINGS: u32 = 1001;
const ID_QUIT: u32 = 1003;

/// The icon id. This process adds one icon only.
const TRAY_UID: u32 = 1;

/// The tray icon of chibipop.
const ICON_BYTES: &[u8] = include_bytes!("../../assets/chibipop.ico");

/// The menu item that the user picked.
pub enum TrayCommand {
    OpenSettings,
    Quit,
}

fn owner_class_name() -> PCWSTR {
    w!("ChibipopTrayOwnerClass")
}

/// Passes every message to `DefWindowProcW`.
///
/// The owner window only holds the menu, so it needs no message handling.
unsafe extern "system" fn owner_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Registers the owner window class one time for each process.
///
/// The latch turns on only after a success, so a failed call can retry.
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

/// Owns the tray icon and its menu.
pub struct Tray {
    /// The value of `NOTIFYICONDATAW.hWnd`. Shell32 sends the callback here.
    notify_hwnd: HWND,
    uid: u32,
    /// The target window of `TrackPopupMenu`.
    menu_owner: HWND,
    /// The icon handle.
    hicon: HICON,
    /// True when this process owns `hicon` and must destroy it.
    hicon_owned: bool,
}

impl Tray {
    /// Adds the icon to the tray. A failure is fatal.
    ///
    /// Every error path frees the icon and the owner window first.
    pub fn create(hwnd: HWND) -> Result<Tray> {
        unsafe {
            let hinstance: HINSTANCE =
                GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();

            // No resource exists yet, so an early error needs no cleanup.
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

    /// Handles the callback message of the tray.
    ///
    /// The menu blocks the thread, so this call runs `before_blocking` first.
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

    /// Shows a balloon notification.
    pub fn notify(&self, title: &str, message: &str) {
        // SAFETY: `self.notify_hwnd` is the live window handle that the
        // caller gave to `Tray::create`. The existing icon uses that same
        // handle. `NIM_MODIFY` with `NIF_INFO` tells Shell32 to show a
        // balloon on the icon that this `uID` and `hWnd` already registered.
        // The code writes the `szInfoTitle` and `szInfo` buffers from owned
        // stack data that outlives the call.
        unsafe {
            let mut nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.notify_hwnd,
                uID: self.uid,
                uFlags: NIF_INFO,
                ..Default::default()
            };
            set_info_title(&mut nid, title);
            set_info(&mut nid, message);
            nid.Anonymous.uTimeout = 5000;
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    /// Shows the menu and blocks until the user picks an item.
    ///
    /// The menu runs its own message pump, and that pump consumes thread
    /// messages.
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

            // MS KB135788 requires the owner window in the foreground first.
            let _ = SetForegroundWindow(self.menu_owner);

            before_blocking();

            let flags = (TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY).0;
            let cmd = TrackPopupMenuEx(hmenu, flags, pt.x, pt.y, self.menu_owner, None);

            let _ = PostMessageW(Some(self.menu_owner), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(hmenu);

            match cmd.0 as u32 {
                ID_SETTINGS => Some(TrayCommand::OpenSettings),
                ID_QUIT => Some(TrayCommand::Quit),
                _ => None, // the user dismissed the menu
            }
        }
    }
}

impl Drop for Tray {
    /// Removes the icon. Each call ignores a failure.
    ///
    /// A missed removal leaves a dead icon in the tray.
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
                // SAFETY: this line runs only after the
                // Shell_NotifyIconW(NIM_DELETE) call above, so the shell no
                // longer shows this icon. An earlier destroy would cause the
                // exact ordering bug that this order prevents.
                // `hicon_owned` is true only when `hicon` came from
                // CreateIconFromResourceEx, which gives an owned handle that
                // this process must free. The shared handle from LoadIconW
                // always leaves the flag false, so this branch never
                // destroys a shared icon.
                let _ = DestroyIcon(self.hicon);
            }
        }
    }
}

/// Writes `text` into `szTip` as UTF-16 and adds the NUL.
fn set_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    // Keep one slot for the NUL.
    let cap = nid.szTip.len();
    let mut wide: Vec<u16> = text.encode_utf16().take(cap - 1).collect();
    wide.push(0);
    nid.szTip[..wide.len()].copy_from_slice(&wide);
}

/// Writes `text` into `szInfo` as UTF-16 and adds the NUL.
fn set_info(nid: &mut NOTIFYICONDATAW, text: &str) {
    let cap = nid.szInfo.len();
    let mut wide: Vec<u16> = text.encode_utf16().take(cap - 1).collect();
    wide.push(0);
    nid.szInfo[..wide.len()].copy_from_slice(&wide);
}

/// Writes `text` into `szInfoTitle` as UTF-16 and adds the NUL.
fn set_info_title(nid: &mut NOTIFYICONDATAW, text: &str) {
    let cap = nid.szInfoTitle.len();
    let mut wide: Vec<u16> = text.encode_utf16().take(cap - 1).collect();
    wide.push(0);
    nid.szInfoTitle[..wide.len()].copy_from_slice(&wide);
}

/// Builds the right-click menu.
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

/// Loads the embedded icon, or the default icon of the operating system.
///
/// A `true` flag means this process owns the handle and must destroy it.
unsafe fn load_tray_icon() -> Result<(HICON, bool)> {
    if let Some(hicon) = unsafe { load_embedded_icon() } {
        return Ok((hicon, true));
    }
    eprintln!("chibipop: could not load the tray's own icon, using the default");
    let hicon =
        unsafe { LoadIconW(None, IDI_APPLICATION) }.context("LoadIconW(IDI_APPLICATION)")?;
    Ok((hicon, false))
}

/// Loads the `.ico` frame that is closest to the small icon size.
///
/// Returns `None` after any failure.
unsafe fn load_embedded_icon() -> Option<HICON> {
    let desired = unsafe { GetSystemMetrics(SM_CXSMICON) } as u32;
    let bytes = ico_frame_bytes(ICON_BYTES, desired)?;
    // SAFETY: `bytes` is a sub-slice of `ICON_BYTES`, a `'static` buffer
    // that this binary embeds. The pointer and the length that
    // CreateIconFromResourceEx reads therefore stay valid for the whole
    // call. That is the only precondition the docs place on `presbits` and
    // `dwResSize`. The docs give `0x0003_0000` as the value to set `dwVer`
    // to (MS Learn, CreateIconFromResourceEx).
    let icon =
        unsafe { CreateIconFromResourceEx(bytes, true, 0x0003_0000, 0, 0, LR_DEFAULTCOLOR) };
    icon.ok()
}

/// Finds the frame with the width nearest to `desired`.
///
/// The `.ico` format starts with a 6-byte header. A 16-byte entry follows
/// for each frame. Returns `None` when the file is malformed.
fn ico_frame_bytes(ico: &[u8], desired: u32) -> Option<&[u8]> {
    let header = ico.get(0..6)?;
    if header[0..4] != [0, 0, 1, 0] {
        return None; // the header must hold reserved=0 and type=1
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

    /// Shell32 reads the tip up to the NUL.
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

    #[test]
    fn a_short_info_round_trips() {
        let mut nid = NOTIFYICONDATAW::default();
        set_info(&mut nid, "added to Anki");
        let n = nid.szInfo.iter().position(|&c| c == 0).expect("terminated");
        assert_eq!("added to Anki", String::from_utf16_lossy(&nid.szInfo[..n]));
    }

    #[test]
    fn a_long_info_is_still_terminated() {
        let mut nid = NOTIFYICONDATAW::default();
        set_info(&mut nid, &"あ".repeat(500));
        assert_eq!(0, nid.szInfo[nid.szInfo.len() - 1]);
    }

    #[test]
    fn a_short_info_title_round_trips() {
        let mut nid = NOTIFYICONDATAW::default();
        set_info_title(&mut nid, "chibipop");
        let n = nid.szInfoTitle.iter().position(|&c| c == 0).expect("terminated");
        assert_eq!("chibipop", String::from_utf16_lossy(&nid.szInfoTitle[..n]));
    }

    #[test]
    fn a_long_info_title_is_truncated() {
        let mut nid = NOTIFYICONDATAW::default();
        set_info_title(&mut nid, &"x".repeat(200));
        assert_eq!(0, nid.szInfoTitle[nid.szInfoTitle.len() - 1]);
    }
}
