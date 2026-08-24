//! The tray icon and its menu.
//!
//! Failing to create is fatal.

use anyhow::{Context, Result};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Shell32's callback message.
const WM_TRAYICON: u32 = WM_APP + 2;

const ID_SETTINGS: u32 = 1001;
const ID_QUIT: u32 = 1003;

/// One icon per process.
const TRAY_UID: u32 = 1;

/// chibipop's own tray icon.
const ICON_BYTES: &[u8] = include_bytes!("../../assets/chibipop.ico");

/// What the user picked.
pub enum TrayCommand {
    OpenSettings,
    Quit,
}

fn owner_class_name() -> PCWSTR {
    w!("ChibipopTrayOwnerClass")
}

/// A `DefWindowProcW` trampoline.
unsafe extern "system" fn owner_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Once per process.
///
/// Latch only after success.
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

/// Owns the icon and its menu.
pub struct Tray {
    /// `NOTIFYICONDATAW.hWnd`.
    notify_hwnd: HWND,
    uid: u32,
    /// `TrackPopupMenu`'s target.
    menu_owner: HWND,
    /// The icon handle.
    hicon: HICON,
    /// Owned handles need destroying.
    hicon_owned: bool,
}

impl Tray {
    /// Adds the icon; hard-fails.
    ///
    /// Error paths free the icon.
    pub fn create(hwnd: HWND) -> Result<Tray> {
        unsafe {
            let hinstance: HINSTANCE =
                GetModuleHandleW(None).context("GetModuleHandleW(None)")?.into();

            // Nothing to unwind yet.
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

    /// The tray's callback message.
    ///
    /// Runs `before_blocking` first.
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

    /// Shows the menu; blocks.
    ///
    /// Its pump eats thread messages.
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

            // MS KB135788: must foreground.
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
    /// Removes the icon; best-effort.
    ///
    /// Or it ghosts the tray.
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

/// UTF-16 into `szTip`, NUL-term.
fn set_tip(nid: &mut NOTIFYICONDATAW, text: &str) {
    // Must keep the NUL.
    let cap = nid.szTip.len();
    let mut wide: Vec<u16> = text.encode_utf16().take(cap - 1).collect();
    wide.push(0);
    nid.szTip[..wide.len()].copy_from_slice(&wide);
}

/// The right-click menu.
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

/// The icon, or the OS default.
///
/// `true` = owned, must destroy.
unsafe fn load_tray_icon() -> Result<(HICON, bool)> {
    if let Some(hicon) = unsafe { load_embedded_icon() } {
        return Ok((hicon, true));
    }
    eprintln!("chibipop: could not load the tray's own icon, using the default");
    let hicon =
        unsafe { LoadIconW(None, IDI_APPLICATION) }.context("LoadIconW(IDI_APPLICATION)")?;
    Ok((hicon, false))
}

/// The closest `.ico` frame.
///
/// `None` on any failure.
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

/// The frame nearest `desired`.
///
/// 6-byte head, 16-byte entries.
/// `None` if malformed.
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

    /// Shell32 reads up to the NUL.
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
