//! Writes the settings control tree as JSON.
//!
//! The audit records each control's layout and state.
//! A diff shows changes after a tab switch or a toggle.

use crate::config::Config;
use crate::present::DictInfo;
use crate::ui::settings_window::{ApplyMode, SettingsWindow};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetClientRect, GetDlgCtrlID, GetNextDlgTabItem, GetWindow,
    GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    SetWindowPos, ShowWindow, GWL_STYLE, GW_CHILD, GW_HWNDNEXT, SWP_NOACTIVATE,
    SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, WS_DISABLED, WS_TABSTOP, WS_VISIBLE,
};

/// Keeps the hidden settings window beyond all monitors.
const OFFSCREEN: i32 = -32_000;

/// Limits the walk if the tab ring does not return to its first control.
const RING_CAP: usize = 200;

/// Creates the settings window and records each tab in JSON.
pub fn run(cfg: &Config, dicts: &[DictInfo]) -> Result<()> {
    let library = crate::app::library_dir();
    let form = crate::app::form_with_library(cfg, dicts, &library);
    let stale = crate::settings::stale_order_entries(cfg, dicts);
    let window = SettingsWindow::open(&form, &stale, ApplyMode::Standalone)
        .context("opening the settings window")?;
    let root = window.hwnd();
    conceal(root);

    let mut dumps = Vec::new();
    for tab in 0..5u32 {
        window.switch_tab(tab);
        conceal(root);
        dumps.push(labelled(root, tab, false));
    }
    // Tab 3 remains visible after this toggle.
    window.toggle_field_map();
    conceal(root);
    dumps.push(labelled(root, 3, true));

    println!("{}", serde_json::to_string_pretty(&json!({ "dumps": dumps }))?);
    Ok(())
}

/// Adds `tab` and `field_map_expanded` to one audit record.
fn labelled(root: HWND, tab: u32, expanded: bool) -> Value {
    let mut v = dump(root);
    if let Some(o) = v.as_object_mut() {
        o.insert("tab".into(), json!(tab));
        o.insert("field_map_expanded".into(), json!(expanded));
    }
    v
}

/// Records all descendants so the audit can compare complete control trees.
pub fn dump(root: HWND) -> Value {
    let mut controls = Vec::new();
    // SAFETY: This process owns `root`. `GetDlgCtrlID` checks this handle.
    // The function returns 0 for a top-level or invalid window.
    // An invalid handle can change the audit value, but it cannot cause undefined behavior.
    let root_id = unsafe { GetDlgCtrlID(root) };
    walk(root, root, root_id, 0, &mut controls);
    json!({
        "client": client_size(root),
        "controls": controls,
        "tab_ring": tab_ring(root, false),
        "tab_ring_reverse": tab_ring(root, true),
    })
}

/// Records child controls in depth-first z-order.
fn walk(root: HWND, parent: HWND, parent_id: i32, depth: u32, out: &mut Vec<Value>) {
    // SAFETY: This process owns `parent`, and the handle stays valid in the complete walk.
    // No call here creates or destroys a window.
    // Each handle from `GetWindow` stays valid until this function reads it.
    // The wrapper maps a null result to `Err`, which stops the child loop.
    let mut next = unsafe { GetWindow(parent, GW_CHILD) };
    while let Ok(cur) = next {
        // SAFETY: `GetWindow` returned `cur`, and the window stays valid in this walk.
        // `GetDlgCtrlID` only reads the handle.
        let id = unsafe { GetDlgCtrlID(cur) };
        out.push(describe(root, cur, id, parent_id, depth));
        walk(root, cur, id, depth + 1, out);
        // SAFETY: `cur` stays valid in this walk and keeps its z-order position.
        // `GetWindow` only reads it.
        next = unsafe { GetWindow(cur, GW_HWNDNEXT) };
    }
}

/// Records the control fields that the audit diff compares.
fn describe(root: HWND, ctrl: HWND, id: i32, parent_id: i32, depth: u32) -> Value {
    // SAFETY: `walk` supplies `ctrl` from its `GetWindow` chain.
    // A hidden ancestor does not change this control's style word.
    // `IsWindowVisible` and `IsWindowEnabled` include ancestor state.
    // This code therefore reads the style word.
    let style = unsafe { GetWindowLongW(ctrl, GWL_STYLE) } as u32;
    json!({
        "depth": depth,
        "id": id,
        "parent_id": parent_id,
        "class": class_of(ctrl),
        "text": text_of(ctrl),
        "rect": client_rect(root, ctrl),
        "visible": style & WS_VISIBLE.0 != 0,
        "enabled": style & WS_DISABLED.0 == 0,
        "tabstop": style & WS_TABSTOP.0 != 0,
    })
}

/// Converts the control rect to client pixels in `root`.
fn client_rect(root: HWND, ctrl: HWND) -> Value {
    let mut rc = RECT::default();
    // SAFETY: `ctrl` stays valid. `rc` is local storage, and `GetWindowRect` only writes to it.
    if unsafe { GetWindowRect(ctrl, &mut rc) }.is_err() {
        return Value::Null;
    }
    let mut tl = POINT { x: rc.left, y: rc.top };
    let mut br = POINT { x: rc.right, y: rc.bottom };
    // SAFETY: `root` stays valid, and both points are local storage.
    // `ScreenToClient` only writes to these points.
    // A failure leaves screen coordinates in the points. This result changes only the audit.
    unsafe {
        let _ = ScreenToClient(root, &mut tl);
        let _ = ScreenToClient(root, &mut br);
    }
    json!({ "x": tl.x, "y": tl.y, "w": br.x - tl.x, "h": br.y - tl.y })
}

/// Records the client size of `root`.
fn client_size(root: HWND) -> Value {
    let mut rc = RECT::default();
    // SAFETY: `root` stays valid. `rc` is local storage, and `GetClientRect` only writes to `rc`.
    if unsafe { GetClientRect(root, &mut rc) }.is_err() {
        return Value::Null;
    }
    json!({ "w": rc.right - rc.left, "h": rc.bottom - rc.top })
}

/// Reads the Win32 class name for a control.
fn class_of(ctrl: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: The wrapper gives `buf.len()` as the capacity, so `GetClassNameW` cannot write
    // past the buffer. `GetClassNameW` returns the character count without the NUL.
    let n = unsafe { GetClassNameW(ctrl, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// Reads the text that belongs to the control.
fn text_of(ctrl: HWND) -> String {
    // SAFETY: The buffer length equals `GetWindowTextLengthW` plus one slot for the NUL.
    // The wrapper gives that length to `GetWindowTextW` as its capacity.
    // `GetWindowTextW` writes within this capacity.
    unsafe {
        let len = GetWindowTextLengthW(ctrl);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(ctrl, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

/// Records tab stops in the order that the tab ring returns.
fn tab_ring(root: HWND, previous: bool) -> Vec<i32> {
    let mut ids = Vec::new();
    // SAFETY: This process owns `root`, and the handle stays valid for this call.
    // `GetNextDlgTabItem` only reads the window tree. It does not set focus or show a window.
    // A null `hctl` selects forward order. Both directions start at the same control, so the
    // reverse ring reverses the forward ring.
    let Ok(first) = (unsafe { GetNextDlgTabItem(root, None, false) }) else {
        return ids;
    };
    let mut cur = first;
    loop {
        // SAFETY: `GetNextDlgTabItem` returned `cur`, which is a valid descendant of `root`.
        // `GetDlgCtrlID` and the next `GetNextDlgTabItem` call only read this handle.
        ids.push(unsafe { GetDlgCtrlID(cur) });
        let Ok(next) = (unsafe { GetNextDlgTabItem(root, Some(cur), previous) }) else {
            break;
        };
        if next == first || ids.len() >= RING_CAP {
            break;
        }
        cur = next;
    }
    ids
}

/// Moves the window beyond all monitors and hides it.
fn conceal(root: HWND) {
    // SAFETY: This process created and owns the `root` settings window.
    // Both calls only move or hide this window.
    // A failure can leave the window visible, but it cannot cause undefined behavior.
    // The move is necessary because `ensure_room_for` shows the window after each resize.
    unsafe {
        let _ = SetWindowPos(
            root,
            None,
            OFFSCREEN,
            OFFSCREEN,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
        let _ = ShowWindow(root, SW_HIDE);
    }
}
