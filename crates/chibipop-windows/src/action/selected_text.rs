//! Read explicit selections through UI Automation without sending copy keys or
//! touching the clipboard. A bounded queue and an MTA thread keep foreign
//! accessibility providers away from the window pump. Unsupported controls
//! return no selection; clipboard fallback could silently use unrelated text.

use crate::controller::RequestId;
use crate::geom::PhysRect;
use windows::Win32::System::Variant::VT_R8;
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetDim, SafeArrayGetElement, SafeArrayGetElemsize, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayGetVartype};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{CUIAutomation8, IUIAutomation2, IUIAutomationTextPattern, UIA_TextPatternId};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

const LIMIT: usize = 65_536;
const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub struct Selection { pub text: String, pub bounds: Option<PhysRect> }

struct Request { id: RequestId, window: usize, started: Instant }

pub struct Reader {
    tx: SyncSender<Request>,
    rx: Receiver<(RequestId, Option<Selection>)>,
    pending: Option<(RequestId, Instant)>,
}

impl Reader {
    pub fn new() -> Self {
        let (tx, requests) = mpsc::sync_channel::<Request>(1);
        let (results, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: This dedicated thread owns and drops every COM interface.
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            while let Ok(request) = requests.recv() {
                let text = if initialized && request.started.elapsed() < TIMEOUT {
                    match read(&request) {
                        Ok(text) => text,
                        Err(error) => {
                            eprintln!("chibipop: selected text capture failed code={}", error.code());
                            None
                        }
                    }
                } else { None };
                if results.send((request.id, text)).is_err() { break; }
            }
            if initialized {
                // SAFETY: This balances the successful initialization above.
                unsafe { CoUninitialize() };
            }
        });
        Self { tx, rx, pending: None }
    }

    pub fn request(&mut self, id: RequestId) {
        let started = Instant::now();
        // SAFETY: This query does not retain or mutate a window resource.
        let window = unsafe { GetForegroundWindow() }.0 as usize;
        self.pending = Some((id, started));
        if self.tx.try_send(Request { id, window, started }).is_err() {
            self.pending = Some((id, started - TIMEOUT));
        }
    }

    pub fn poll(&mut self) -> Option<(RequestId, Option<Selection>)> {
        while let Ok((id, text)) = self.rx.try_recv() {
            if self.pending.is_some_and(|(pending, _)| pending == id) {
                let (_, started) = self.pending.take()?;
                return Some((id, if started.elapsed() < TIMEOUT { text } else { None }));
            }
        }
        if self.pending.is_some_and(|(_, started)| started.elapsed() >= TIMEOUT) {
            return self.pending.take().map(|(id, _)| (id, None));
        }
        None
    }
}

impl Default for Reader {
    fn default() -> Self { Self::new() }
}

fn read(request: &Request) -> windows::core::Result<Option<Selection>> {
    let window = HWND(request.window as *mut std::ffi::c_void);
    // SAFETY: COM is initialized on this thread. UIA interfaces stay here;
    // the foreground handle is compared only, never dereferenced or retained.
    unsafe {
        if window.0.is_null() || GetForegroundWindow() != window { return Ok(None); }
        let automation: IUIAutomation2 = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
        automation.SetConnectionTimeout(250)?;
        automation.SetTransactionTimeout(250)?;
        let focused = automation.GetFocusedElement()?;
        if focused.CurrentIsPassword()?.as_bool() { return Ok(None); }
        let walker = automation.ControlViewWalker()?;
        let mut element = focused.clone();
        for _ in 0..8 {
            if request.started.elapsed() >= TIMEOUT || GetForegroundWindow() != window { return Ok(None); }
            if element.CurrentIsPassword()?.as_bool() { return Ok(None); }
            if let Ok(pattern) = element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) {
                let ranges = pattern.GetSelection()?;
                let count = ranges.Length()?;
                if !(1..=32).contains(&count) { return Ok(None); }
                let mut text = String::new();
                let mut bounds = None;
                for index in 0..count {
                    if request.started.elapsed() >= TIMEOUT { return Ok(None); }
                    let range = ranges.GetElement(index)?;
                    let part = range.GetText((LIMIT + 1) as i32)?.to_string();
                    if !part.is_empty() {
                        if let Ok(array) = range.GetBoundingRectangles() {
                            bounds = merge_bounds(bounds, rectangle_array(array));
                        }
                    }
                    if !append_range(&mut text, &part) { return Ok(None); }
                }
                if GetForegroundWindow() != window || request.started.elapsed() >= TIMEOUT
                    || !automation.CompareElements(&focused, &automation.GetFocusedElement()?)?.as_bool()
                { return Ok(None); }
                return Ok((!text.trim().is_empty()).then_some(Selection { text, bounds }));
            }
            if element.CurrentNativeWindowHandle()? == window { break; }
            let Ok(parent) = walker.GetParentElement(&element) else { break; };
            element = parent;
        }
    }
    Ok(None)
}

/// UIA allocates the SAFEARRAY. Always release it, including malformed results.
unsafe fn rectangle_array(array: *mut SAFEARRAY) -> Option<PhysRect> {
    if array.is_null() { return None; }
    // SAFETY: The caller transfers the array returned by GetBoundingRectangles.
    // Element copies are bounds-checked by Ole; the owned array is destroyed once.
    unsafe {
        let result = (|| {
            if SafeArrayGetDim(array) != 1 || SafeArrayGetElemsize(array) != 8
                || SafeArrayGetVartype(array).ok()? != VT_R8 { return None; }
            let low = SafeArrayGetLBound(array, 1).ok()?;
            let high = SafeArrayGetUBound(array, 1).ok()?;
            let count = i64::from(high) - i64::from(low) + 1;
            if !(4..=16_384).contains(&count) || count % 4 != 0 { return None; }
            let mut values = Vec::with_capacity(count as usize);
            for index in low..=high {
                let mut value = 0.0f64;
                SafeArrayGetElement(array, &index, &mut value as *mut f64 as *mut std::ffi::c_void).ok()?;
                values.push(value);
            }
            selection_bounds(&values)
        })();
        let _ = SafeArrayDestroy(array);
        result
    }
}

fn selection_bounds(values: &[f64]) -> Option<PhysRect> {
    if !values.len().is_multiple_of(4) { return None; }
    let mut bounds = None;
    for rect in values.as_chunks::<4>().0 {
        if rect.iter().any(|value| !value.is_finite()) { return None; }
        if rect[2] <= 0.0 || rect[3] <= 0.0 { continue; }
        let (left, top, right, bottom) = (rect[0].floor(), rect[1].floor(),
            (rect[0] + rect[2]).ceil(), (rect[1] + rect[3]).ceil());
        if [left, top, right, bottom].iter().any(|value|
            *value < f64::from(i32::MIN) || *value > f64::from(i32::MAX))
        { return None; }
        let width = right - left;
        let height = bottom - top;
        if width > f64::from(i32::MAX) || height > f64::from(i32::MAX) { return None; }
        bounds = merge_bounds(bounds, Some(PhysRect { x: left as i32, y: top as i32, w: width as i32, h: height as i32 }));
    }
    bounds
}

fn merge_bounds(first: Option<PhysRect>, second: Option<PhysRect>) -> Option<PhysRect> {
    let (a, b) = match (first, second) { (Some(a), Some(b)) => (a, b), (a, None) => return a, (None, b) => return b };
    let left = a.x.min(b.x);
    let top = a.y.min(b.y);
    let right = (i64::from(a.x) + i64::from(a.w)).max(i64::from(b.x) + i64::from(b.w));
    let bottom = (i64::from(a.y) + i64::from(a.h)).max(i64::from(b.y) + i64::from(b.h));
    Some(PhysRect { x: left, y: top, w: i32::try_from(right - i64::from(left)).ok()?, h: i32::try_from(bottom - i64::from(top)).ok()? })
}

fn append_range(text: &mut String, part: &str) -> bool {
    if part.is_empty() { return true; }
    let separator = usize::from(!text.is_empty());
    if text.len().saturating_add(part.len()).saturating_add(separator) > LIMIT { return false; }
    if separator != 0 { text.push('\n'); }
    text.push_str(part);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_rectangles_round_outward_and_union_visible_lines() {
        assert_eq!(selection_bounds(&[-20.4, 40.2, 30.1, 18.2, 5.0, 61.0, 10.0, 20.0]),
            Some(PhysRect { x: -21, y: 40, w: 36, h: 41 }));
        assert_eq!(selection_bounds(&[0.0, f64::NAN, 5.0, 8.0]), None);
        assert_eq!(selection_bounds(&[0.0, 0.0, 0.0, 0.0]), None);
        assert_eq!(selection_bounds(&[0.0, 0.0, f64::INFINITY, 2.0]), None);
    }

    #[test]
    fn selected_ranges_keep_unicode_and_boundaries() {
        let mut text = String::new();
        assert!(append_range(&mut text, "食べる"));
        assert!(append_range(&mut text, ""));
        assert!(append_range(&mut text, "猫"));
        assert_eq!(text, "食べる\n猫");
    }

    #[test]
    fn oversized_selection_is_rejected_without_partial_append() {
        let mut text = "猫".to_string();
        assert!(!append_range(&mut text, &"x".repeat(LIMIT)));
        assert_eq!(text, "猫");
    }

    #[test]
    fn late_capture_cannot_replace_a_newer_selection() {
        let (tx, _requests) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::channel();
        let mut reader = Reader { tx, rx, pending: Some((RequestId(2), Instant::now())) };
        results.send((RequestId(1), Some(Selection { text: "古い".into(), bounds: None }))).unwrap();
        assert_eq!(reader.poll(), None);
        results.send((RequestId(2), Some(Selection { text: "猫".into(), bounds: None }))).unwrap();
        assert_eq!(reader.poll(), Some((RequestId(2), Some(Selection { text: "猫".into(), bounds: None }))));
        assert_eq!(reader.poll(), None);
    }

    #[test]
    fn timed_out_capture_does_not_deliver_queued_text() {
        let (tx, _requests) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::channel();
        let mut reader = Reader { tx, rx, pending: Some((RequestId(1), Instant::now() - TIMEOUT)) };
        results.send((RequestId(1), Some(Selection { text: "猫".into(), bounds: None }))).unwrap();
        assert_eq!(reader.poll(), Some((RequestId(1), None)));
    }
}
