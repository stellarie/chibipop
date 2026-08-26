//! Dumps the settings tree.

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

/// Beyond any monitor.
const OFFSCREEN: i32 = -32_000;

/// A broken ring must stop.
const RING_CAP: usize = 200;

/// Build, dump, print.
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
    // Leaves tab 3 showing.
    window.toggle_field_map();
    conceal(root);
    dumps.push(labelled(root, 3, true));

    println!("{}", serde_json::to_string_pretty(&json!({ "dumps": dumps }))?);
    Ok(())
}

/// One dump, tagged.
fn labelled(root: HWND, tab: u32, expanded: bool) -> Value {
    let mut v = dump(root);
    if let Some(o) = v.as_object_mut() {
        o.insert("tab".into(), json!(tab));
        o.insert("field_map_expanded".into(), json!(expanded));
    }
    v
}

/// Every descendant of `root`.
pub fn dump(root: HWND) -> Value {
    let mut controls = Vec::new();
    // SAFETY: `root` is a window handle owned by this process. GetDlgCtrlID
    // validates the handle itself and returns 0 for a top-level window or a
    // dead one, so a wrong handle yields a wrong number, never unsoundness.
    let root_id = unsafe { GetDlgCtrlID(root) };
    walk(root, root, root_id, 0, &mut controls);
    json!({
        "client": client_size(root),
        "controls": controls,
        "tab_ring": tab_ring(root, false),
        "tab_ring_reverse": tab_ring(root, true),
    })
}

/// Depth-first, in z-order.
fn walk(root: HWND, parent: HWND, parent_id: i32, depth: u32, out: &mut Vec<Value>) {
    // SAFETY: `parent` is a live window owned by this process for the whole
    // walk - nothing here creates or destroys a window - so every handle
    // GetWindow returns is live at the moment it is read. The wrapper maps a
    // null result to `Err`, which is what ends the sibling loop.
    let mut next = unsafe { GetWindow(parent, GW_CHILD) };
    while let Ok(cur) = next {
        // SAFETY: `cur` came from GetWindow above and is live, as argued
        // there; GetDlgCtrlID only reads it.
        let id = unsafe { GetDlgCtrlID(cur) };
        out.push(describe(root, cur, id, parent_id, depth));
        walk(root, cur, id, depth + 1, out);
        // SAFETY: as above - `cur` is still live and unmoved in z-order.
        next = unsafe { GetWindow(cur, GW_HWNDNEXT) };
    }
}

/// What a diff needs.
fn describe(root: HWND, ctrl: HWND, id: i32, parent_id: i32, depth: u32) -> Value {
    // SAFETY: `ctrl` is live, from `walk`'s GetWindow chain. The style word
    // is the control's own, so it is unaffected by a hidden ancestor - which
    // IsWindowVisible and IsWindowEnabled would not be.
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

/// Client pixels of `root`.
fn client_rect(root: HWND, ctrl: HWND) -> Value {
    let mut rc = RECT::default();
    // SAFETY: `ctrl` is live and `rc` is this call's own stack storage, which
    // GetWindowRect only writes through.
    if unsafe { GetWindowRect(ctrl, &mut rc) }.is_err() {
        return Value::Null;
    }
    let mut tl = POINT { x: rc.left, y: rc.top };
    let mut br = POINT { x: rc.right, y: rc.bottom };
    // SAFETY: `root` is live and both points are local storage the call only
    // writes through. A failure leaves them in screen coordinates, which the
    // dump then reports as an obviously wrong rect rather than hiding.
    unsafe {
        let _ = ScreenToClient(root, &mut tl);
        let _ = ScreenToClient(root, &mut br);
    }
    json!({ "x": tl.x, "y": tl.y, "w": br.x - tl.x, "h": br.y - tl.y })
}

/// The window's own client box.
fn client_size(root: HWND) -> Value {
    let mut rc = RECT::default();
    // SAFETY: `root` is live and `rc` is local storage the call only writes.
    if unsafe { GetClientRect(root, &mut rc) }.is_err() {
        return Value::Null;
    }
    json!({ "w": rc.right - rc.left, "h": rc.bottom - rc.top })
}

/// The window class name.
fn class_of(ctrl: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: the wrapper passes `buf.len()` as the capacity, so the call
    // cannot write past it, and returns the count written excluding the NUL.
    let n = unsafe { GetClassNameW(ctrl, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The control's own text.
fn text_of(ctrl: HWND) -> String {
    // SAFETY: the buffer is sized to the length GetWindowTextLengthW itself
    // reported plus the NUL, which is the contract GetWindowTextW writes
    // against; the wrapper passes that same length as the capacity.
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

/// Tab stops, in ring order.
fn tab_ring(root: HWND, previous: bool) -> Vec<i32> {
    let mut ids = Vec::new();
    // SAFETY: `root` is this process's settings window, live for the call.
    // GetNextDlgTabItem is a tree query: it neither focuses nor shows.
    // A null `hctl` only answers forward, so both directions seed from the
    // same control and the reverse ring is the forward one mirrored.
    let Ok(first) = (unsafe { GetNextDlgTabItem(root, None, false) }) else {
        return ids;
    };
    let mut cur = first;
    loop {
        // SAFETY: `cur` came from GetNextDlgTabItem and is a live descendant
        // of `root`; both calls only read it.
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

/// Off-screen and hidden.
fn conceal(root: HWND) {
    // SAFETY: `root` is the settings window this process created and owns.
    // Both calls only move and hide it, and a failure leaves it on screen,
    // which is cosmetic rather than unsound. The move matters because
    // `ensure_room_for` re-shows the window whenever it resizes it.
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
