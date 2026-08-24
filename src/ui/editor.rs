//! CSS theme editor window.

use crate::ui::css;
use crate::ui::theme::Theme;
use anyhow::{Context, Result};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, HFONT, LOGFONTW};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

const WIN_W: i32 = 560;
const WIN_H: i32 = 520;

const ID_EDIT: i32 = 200;
const ID_SAVE: i32 = 201;
const ID_RESET: i32 = 202;
const ID_CLOSE_BTN: i32 = 203;
const ID_STATUS: i32 = 204;

const MARGIN: i32 = 10;
const BTN_W: i32 = 110;
const BTN_H: i32 = 28;
const STATUS_H: i32 = 20;

/// What happened in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorOutcome {
    Applied,
    Closed,
}

/// Shared state the wndproc reads.
struct EditorState {
    css_path: PathBuf,
    base_theme: String,
    outcome: Cell<Option<EditorOutcome>>,
}

/// The CSS editor window.
pub struct CssEditor {
    hwnd: HWND,
    #[allow(dead_code)]
    font: Option<HFONT>,
    state: Box<EditorState>,
}

static CLASS_REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn class_name() -> PCWSTR {
    w!("ChibipopCssEditor")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Monospace font for the editor.
unsafe fn mono_font() -> Option<HFONT> {
    let name = "Consolas";
    let w: Vec<u16> = name.encode_utf16().collect();
    let len = w.len().min(31);
    let mut face = [0u16; 32];
    face[..len].copy_from_slice(&w[..len]);
    let lf = LOGFONTW {
        lfHeight: -14,
        lfFaceName: face,
        ..Default::default()
    };
    // SAFETY: `lf` is valid stack storage.
    let font = unsafe { CreateFontIndirectW(&lf) };
    if font.is_invalid() {
        None
    } else {
        Some(font)
    }
}

unsafe fn register_class(hinstance: HINSTANCE) -> Result<()> {
    if CLASS_REGISTERED.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
            (windows::Win32::Graphics::Gdi::COLOR_BTNFACE.0 + 1) as *mut _,
        ),
        lpszClassName: class_name(),
        ..Default::default()
    };
    // SAFETY: valid struct.
    if unsafe { RegisterClassExW(&wc) } == 0 {
        return Err(windows::core::Error::from_thread()).context("RegisterClassExW for CSS editor");
    }
    CLASS_REGISTERED.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Retrieve the `EditorState` from USERDATA.
unsafe fn state_of(hwnd: HWND) -> Option<&'static EditorState> {
    // SAFETY: set once in `open`, lives until `CssEditor` drops.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if ptr == 0 {
        return None;
    }
    Some(unsafe { &*(ptr as *const EditorState) })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            match id {
                ID_SAVE => {
                    // SAFETY: delegates to safe helpers.
                    unsafe { save_and_apply(hwnd) };
                }
                ID_RESET => {
                    // SAFETY: delegates to safe helpers.
                    unsafe { reset_to_default(hwnd) };
                }
                ID_CLOSE_BTN => {
                    // SAFETY: standard close.
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: standard close.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(st) = unsafe { state_of(hwnd) } {
                st.outcome.set(Some(EditorOutcome::Closed));
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Save the text and re-parse.
unsafe fn save_and_apply(hwnd: HWND) {
    let Some(st) = (unsafe { state_of(hwnd) }) else {
        return;
    };
    // SAFETY: `ID_EDIT` created in `build`.
    let Ok(edit) = (unsafe { GetDlgItem(Some(hwnd), ID_EDIT) }) else {
        return;
    };
    // SAFETY: reading the edit control text.
    let text = unsafe { window_text(edit) };

    let mut theme = match st.base_theme.as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };

    let errors = css::parse(&text, &mut theme);
    if !errors.is_empty() {
        let msg = errors
            .iter()
            .map(|e| format!("Line {}: {}", e.line, e.message))
            .collect::<Vec<_>>()
            .join("; ");
        set_status(hwnd, &msg);
        return;
    }

    if let Err(e) = std::fs::write(&st.css_path, &text) {
        set_status(hwnd, &format!("Save failed: {e}"));
        return;
    }

    set_status(hwnd, "Saved and applied.");
    st.outcome.set(Some(EditorOutcome::Applied));
}

/// Reset the editor to the base theme's CSS.
unsafe fn reset_to_default(hwnd: HWND) {
    let Some(st) = (unsafe { state_of(hwnd) }) else {
        return;
    };
    let theme = match st.base_theme.as_str() {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };
    let css_text = css::to_css(&theme);
    if let Ok(edit) = unsafe { GetDlgItem(Some(hwnd), ID_EDIT) } {
        // SAFETY: valid window.
        unsafe {
            let _ = SetWindowTextW(edit, PCWSTR(wide(&css_text).as_ptr()));
        }
    }
    set_status(hwnd, "Reset to default.");
}

fn set_status(hwnd: HWND, text: &str) {
    if let Ok(ctrl) = unsafe { GetDlgItem(Some(hwnd), ID_STATUS) } {
        // SAFETY: valid window.
        unsafe {
            let _ = SetWindowTextW(ctrl, PCWSTR(wide(text).as_ptr()));
        }
    }
}

/// Read a control's text.
unsafe fn window_text(ctrl: HWND) -> String {
    // SAFETY: `ctrl` is a live handle.
    unsafe {
        let len = GetWindowTextLengthW(ctrl);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(ctrl, &mut buf);
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

impl CssEditor {
    /// Open the CSS editor.
    pub fn open(css_path: &Path, base_theme: &str, current_font: &str) -> Result<CssEditor> {
        // SAFETY: standard Win32 window creation; every
        // handle is checked before use.
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();
            register_class(hinstance)?;

            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name(),
                w!("chibipop \u{2014} CSS theme editor"),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                WIN_W,
                WIN_H,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW for CSS editor")?;

            let mono = mono_font();

            let state = Box::new(EditorState {
                css_path: css_path.to_path_buf(),
                base_theme: base_theme.to_string(),
                outcome: Cell::new(None),
            });

            // SAFETY: `state` lives in the returned `CssEditor`
            // which outlives the window.
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*state as *const _ as isize);

            let editor = CssEditor {
                hwnd,
                font: mono,
                state,
            };
            build_controls(hwnd, hinstance, mono, css_path, base_theme, current_font)?;

            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(editor)
        }
    }

    /// The window handle.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Consume the outcome flag.
    pub fn take_outcome(&self) -> Option<EditorOutcome> {
        self.state.outcome.take()
    }

    /// Is it still showing?
    pub fn is_visible(&self) -> bool {
        // SAFETY: valid HWND.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }
}

/// Create the child controls.
unsafe fn build_controls(
    hwnd: HWND,
    hinstance: HINSTANCE,
    mono: Option<HFONT>,
    css_path: &Path,
    base_theme: &str,
    current_font: &str,
) -> Result<()> {
    let mut rc = RECT::default();
    // SAFETY: hwnd is valid.
    unsafe {
        GetClientRect(hwnd, &mut rc).ok();
    }
    let cw = rc.right - rc.left;
    let ch = rc.bottom - rc.top;
    let edit_h = ch - MARGIN * 4 - BTN_H - STATUS_H;

    // SAFETY: creating standard child controls.
    unsafe {
        let edit = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE(
                (WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_HSCROLL).0
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_AUTOHSCROLL as u32
                    | ES_WANTRETURN as u32,
            ),
            MARGIN,
            MARGIN,
            cw - MARGIN * 2,
            edit_h,
            Some(hwnd),
            Some(HMENU(ID_EDIT as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )
        .context("creating the CSS text area")?;

        if let Some(f) = mono {
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(f.0 as usize)),
                Some(LPARAM(1)),
            );
        }

        let btn_y = MARGIN * 2 + edit_h;
        let mut btn_x = MARGIN;

        create_button(hwnd, hinstance, "Save && Apply", ID_SAVE, btn_x, btn_y)?;
        btn_x += BTN_W + MARGIN;

        create_button(hwnd, hinstance, "Reset to Default", ID_RESET, btn_x, btn_y)?;
        btn_x = cw - MARGIN - BTN_W;

        create_button(hwnd, hinstance, "Close", ID_CLOSE_BTN, btn_x, btn_y)?;

        let status_y = btn_y + BTN_H + MARGIN;
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WS_CHILD | WS_VISIBLE,
            MARGIN,
            status_y,
            cw - MARGIN * 2,
            STATUS_H,
            Some(hwnd),
            Some(HMENU(ID_STATUS as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )
        .context("creating the status label")?;
    }

    let css_text = load_or_generate(css_path, base_theme, current_font);
    if let Ok(edit) = unsafe { GetDlgItem(Some(hwnd), ID_EDIT) } {
        // SAFETY: valid window.
        unsafe {
            let _ = SetWindowTextW(edit, PCWSTR(wide(&css_text).as_ptr()));
        }
    }

    Ok(())
}

/// Load CSS or generate from theme.
fn load_or_generate(css_path: &Path, base_theme: &str, current_font: &str) -> String {
    if let Ok(text) = std::fs::read_to_string(css_path) {
        if !text.trim().is_empty() {
            return to_crlf(&text);
        }
    }
    let mut theme = match base_theme {
        "light" => Theme::light(),
        _ => Theme::dark(),
    };
    theme.font_name = current_font.to_string();
    css::to_css(&theme)
}

/// Normalize to CRLF for the EDIT control.
fn to_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\n' && !out.ends_with('\r') {
            out.push('\r');
        }
        out.push(c);
    }
    out
}

unsafe fn create_button(
    parent: HWND,
    hinstance: HINSTANCE,
    text: &str,
    id: i32,
    x: i32,
    y: i32,
) -> Result<HWND> {
    // SAFETY: standard child control.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            PCWSTR(wide(text).as_ptr()),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP,
            x,
            y,
            BTN_W,
            BTN_H,
            Some(parent),
            Some(HMENU(id as *mut core::ffi::c_void)),
            Some(hinstance),
            None,
        )
        .context("creating a button")
    }
}
