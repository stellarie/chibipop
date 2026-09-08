//! Read explicit selections through UI Automation without sending copy keys or
//! touching the clipboard. A bounded queue and an MTA thread keep foreign
//! accessibility providers away from the window pump. Unsupported controls
//! return no selection; clipboard fallback could silently use unrelated text.

use crate::controller::RequestId;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{CUIAutomation8, IUIAutomation2, IUIAutomationTextPattern, UIA_TextPatternId};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

const LIMIT: usize = 65_536;
const TIMEOUT: Duration = Duration::from_secs(2);

struct Request { id: RequestId, window: usize, started: Instant }

pub struct Reader {
    tx: SyncSender<Request>,
    rx: Receiver<(RequestId, Option<String>)>,
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

    pub fn poll(&mut self) -> Option<(RequestId, Option<String>)> {
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

fn read(request: &Request) -> windows::core::Result<Option<String>> {
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
                for index in 0..count {
                    if request.started.elapsed() >= TIMEOUT { return Ok(None); }
                    let part = ranges.GetElement(index)?.GetText((LIMIT + 1) as i32)?.to_string();
                    if !append_range(&mut text, &part) { return Ok(None); }
                }
                if GetForegroundWindow() != window || request.started.elapsed() >= TIMEOUT
                    || !automation.CompareElements(&focused, &automation.GetFocusedElement()?)?.as_bool()
                { return Ok(None); }
                return Ok((!text.trim().is_empty()).then_some(text));
            }
            if element.CurrentNativeWindowHandle()? == window { break; }
            let Ok(parent) = walker.GetParentElement(&element) else { break; };
            element = parent;
        }
    }
    Ok(None)
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
        results.send((RequestId(1), Some("古い".into()))).unwrap();
        assert_eq!(reader.poll(), None);
        results.send((RequestId(2), Some("猫".into()))).unwrap();
        assert_eq!(reader.poll(), Some((RequestId(2), Some("猫".into()))));
        assert_eq!(reader.poll(), None);
    }

    #[test]
    fn timed_out_capture_does_not_deliver_queued_text() {
        let (tx, _requests) = mpsc::sync_channel(1);
        let (results, rx) = mpsc::channel();
        let mut reader = Reader { tx, rx, pending: Some((RequestId(1), Instant::now() - TIMEOUT)) };
        results.send((RequestId(1), Some("猫".into()))).unwrap();
        assert_eq!(reader.poll(), Some((RequestId(1), None)));
    }
}
