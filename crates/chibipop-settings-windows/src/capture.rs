//! Captures one chord through a low-level keyboard hook.
//!
//! `Machine` decides the chord and holds no Win32 type.
//! While armed, the hook swallows every key edge.

use chibipop::config::parse_hotkey;
use std::cell::RefCell;
use std::fmt;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_QUIT, WM_SYSKEYDOWN,
};

const STARTUP_LIMIT: Duration = Duration::from_secs(5);
const STOP_TRIES: u32 = 10;
const STOP_RETRY: Duration = Duration::from_millis(20);

const CTRL: u8 = 0b001;
const SHIFT: u8 = 0b010;
const ALT: u8 = 0b100;

const CTRL_KEYS: [u16; 3] = [0x11, 0xA2, 0xA3];
const SHIFT_KEYS: [u16; 3] = [0x10, 0xA0, 0xA1];
const ALT_KEYS: [u16; 3] = [0x12, 0xA4, 0xA5];
const WIN_KEYS: [u16; 2] = [0x5B, 0x5C];
const SIDED_MODIFIERS: [u16; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureResult {
    Chord { chord: String },
    Cleared,
    Cancelled,
    Rejected { chord: String },
    TimedOut,
}

#[derive(Debug)]
pub struct CaptureError(String);

impl CaptureError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CaptureStep {
    Cancel,
    Clear,
    Modifier,
    Chord,
}

fn capture_step(vk: u16) -> CaptureStep {
    match vk {
        0x1B => CaptureStep::Cancel,
        0x08 => CaptureStep::Clear,
        _ if is_modifier_key(vk) => CaptureStep::Modifier,
        _ => CaptureStep::Chord,
    }
}

fn is_modifier_key(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B..=0x5C | 0xA0..=0xA5)
}

fn modifier_bit(vk: u16) -> u8 {
    match vk {
        0x11 | 0xA2 | 0xA3 => CTRL,
        0x10 | 0xA0 | 0xA1 => SHIFT,
        0x12 | 0xA4 | 0xA5 => ALT,
        _ => 0,
    }
}

// Carries no Win bit, so Win alone builds no chord.
fn modifier_chord(modifiers: u8) -> String {
    let mut parts = Vec::with_capacity(3);
    if modifiers & CTRL != 0 {
        parts.push("Ctrl");
    }
    if modifiers & SHIFT != 0 {
        parts.push("Shift");
    }
    if modifiers & ALT != 0 {
        parts.push("Alt");
    }
    parts.join("+")
}

fn bind_chord(vk: u16, ctrl: bool, shift: bool, alt: bool, win: bool) -> String {
    let mut parts = Vec::new();
    for (held, name) in [(ctrl, "Ctrl"), (shift, "Shift"), (alt, "Alt"), (win, "Win")] {
        if held {
            parts.push(name.to_string());
        }
    }
    parts.push(match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(u32::from(vk)).unwrap_or('?').to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => stored_trigger_key(vk),
    });
    parts.join("+")
}

fn stored_trigger_key(vk: u16) -> String {
    match vk {
        0x10 => "shift".into(),
        0x11 => "ctrl".into(),
        0x12 => "alt".into(),
        0x70..=0x7B => format!("f{}", vk - 0x6F),
        _ => format!("0x{vk:02X}"),
    }
}

// One bit per key that is_modifier_key accepts.
fn held_slot(vk: u16) -> u16 {
    match vk {
        0x10..=0x12 => 1 << (vk - 0x10),
        0xA0..=0xA5 => 1 << (vk - 0xA0 + 3),
        0x5B..=0x5C => 1 << (vk - 0x5B + 9),
        _ => 0,
    }
}

fn any_held(held: u16, keys: &[u16]) -> bool {
    keys.iter().any(|&vk| held & held_slot(vk) != 0)
}

/// Turns key edges into one capture result.
pub struct Machine {
    held: u16,
    seen: u8,
    done: bool,
}

impl Machine {
    /// `held_at_arm` lists modifiers down at arm.
    pub fn new(held_at_arm: &[u16]) -> Self {
        Self {
            held: held_at_arm.iter().fold(0, |held, &vk| held | held_slot(vk)),
            seen: 0,
            done: false,
        }
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn feed(&mut self, vk: u16, is_down: bool) -> Option<CaptureResult> {
        if self.done {
            return None;
        }
        let result = if is_down {
            self.key_down(vk)
        } else {
            self.key_up(vk)
        };
        self.done = result.is_some();
        result
    }

    fn key_down(&mut self, vk: u16) -> Option<CaptureResult> {
        match capture_step(vk) {
            CaptureStep::Cancel => Some(CaptureResult::Cancelled),
            CaptureStep::Clear => Some(CaptureResult::Cleared),
            CaptureStep::Modifier => {
                self.held |= held_slot(vk);
                self.seen |= modifier_bit(vk);
                None
            }
            CaptureStep::Chord => {
                let chord = bind_chord(
                    vk,
                    any_held(self.held, &CTRL_KEYS),
                    any_held(self.held, &SHIFT_KEYS),
                    any_held(self.held, &ALT_KEYS),
                    any_held(self.held, &WIN_KEYS),
                );
                Some(if parse_hotkey(&chord).is_some() {
                    CaptureResult::Chord { chord }
                } else {
                    CaptureResult::Rejected { chord }
                })
            }
        }
    }

    // Commits a modifier-only chord at last release.
    fn key_up(&mut self, vk: u16) -> Option<CaptureResult> {
        if !is_modifier_key(vk) {
            return None;
        }
        self.held &= !held_slot(vk);
        if self.held != 0 {
            return None;
        }
        let chord = modifier_chord(self.seen);
        if chord.is_empty() || parse_hotkey(&chord).is_none() {
            return None;
        }
        Some(CaptureResult::Chord { chord })
    }
}

struct Armed {
    machine: Machine,
    tx: Sender<CaptureResult>,
}

enum Startup {
    QueueReady(u32),
    Installed(Result<(), String>),
}

thread_local! {
    // The callback runs on the installing thread.
    static HOOK_STATE: RefCell<Option<Armed>> = const { RefCell::new(None) };
}

static BUSY: AtomicBool = AtomicBool::new(false);
static ARMED: Mutex<Option<u32>> = Mutex::new(None);

fn armed_slot() -> std::sync::MutexGuard<'static, Option<u32>> {
    ARMED.lock().unwrap_or_else(|e| e.into_inner())
}

/// True when an armed capture owns the edge.
///
/// # Safety
/// `lparam` must point to a live `KBDLLHOOKSTRUCT`.
unsafe fn swallow_edge(wparam: WPARAM, lparam: LPARAM) -> bool {
    // SAFETY: The caller guarantees that `lparam` points to a live
    // `KBDLLHOOKSTRUCT`. Under the `WH_KEYBOARD_LL` contract, the OS owns it
    // for the duration of the callback.
    let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode } as u16;
    let is_down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    HOOK_STATE.with(|state| {
        let Ok(mut state) = state.try_borrow_mut() else {
            return true;
        };
        let Some(armed) = state.as_mut() else {
            return false;
        };
        if let Some(result) = armed.machine.feed(vk, is_down) {
            let _ = armed.tx.send(result);
        }
        true
    })
}

/// Handles `WH_KEYBOARD_LL` events.
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: With `code >= 0`, `lparam` points to a live `KBDLLHOOKSTRUCT`.
    // `catch_unwind` stops an unwind before it leaves this `extern "system"`
    // function. A panic passes the key on, so a bug cannot kill the keyboard.
    if code >= 0 && catch_unwind(|| unsafe { swallow_edge(wparam, lparam) }).unwrap_or(false) {
        return LRESULT(1);
    }
    // SAFETY: The arguments come unchanged from the OS. A null hook handle is
    // allowed for this call.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

struct Hook(HHOOK);

impl Hook {
    fn install() -> Result<Self, String> {
        // SAFETY: This call takes a null module name and returns this module.
        let module = unsafe { GetModuleHandleW(None) }
            .map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
        let hinstance: HINSTANCE = module.into();
        // SAFETY: `keyboard_hook_proc` has the `HOOKPROC` signature and reads
        // thread-local state only. `hinstance` names this module. Thread id 0
        // selects a global hook. `Drop` removes it on the installing thread.
        unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook_proc), Some(hinstance), 0) }
            .map(Self)
            .map_err(|e| format!("SetWindowsHookExW(WH_KEYBOARD_LL) failed: {e}"))
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the live handle that `install` returned. `Drop`
        // runs once. A failed removal has no recovery, so the result is dropped.
        unsafe {
            let _ = UnhookWindowsHookEx(self.0);
        }
    }
}

// Clears the thread id when the thread ends.
struct Disarm(u32);

impl Drop for Disarm {
    fn drop(&mut self) {
        let mut slot = armed_slot();
        if *slot == Some(self.0) {
            *slot = None;
        }
    }
}

fn held_modifiers() -> Vec<u16> {
    SIDED_MODIFIERS
        .into_iter()
        .filter(|&vk| {
            // SAFETY: This call reads global key state and has no preconditions.
            let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
            state < 0
        })
        .collect()
}

fn post_quit(thread_id: u32) -> bool {
    // SAFETY: The call takes plain values and no pointer. It fails, not
    // faults, for a thread without a queue. Callers check the result.
    unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)).is_ok() }
}

fn run_hook_thread(startup: Sender<Startup>, results: Sender<CaptureResult>) {
    // SAFETY: This call has no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut msg = MSG::default();
    // SAFETY: `msg` is a live `MSG`. `PM_NOREMOVE` leaves the queue intact.
    // The call creates this thread's message queue.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
    }
    *armed_slot() = Some(thread_id);
    let _disarm = Disarm(thread_id);
    if startup.send(Startup::QueueReady(thread_id)).is_err() {
        return;
    }
    let hook = match Hook::install() {
        Ok(hook) => hook,
        Err(message) => {
            let _ = startup.send(Startup::Installed(Err(message)));
            return;
        }
    };
    // Edges queue until the pump below runs.
    HOOK_STATE.with(|state| {
        *state.borrow_mut() = Some(Armed {
            machine: Machine::new(&held_modifiers()),
            tx: results,
        });
    });
    if startup.send(Startup::Installed(Ok(()))).is_err() {
        return;
    }
    loop {
        // SAFETY: `msg` is a live `MSG`. The call blocks until a message or quit.
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
    }
    drop(hook);
    let ended = HOOK_STATE.with(|state| state.borrow_mut().take());
    if let Some(armed) = ended.filter(|armed| !armed.machine.is_done()) {
        let _ = armed.tx.send(CaptureResult::Cancelled);
    }
}

struct Claim;

impl Claim {
    fn take() -> Result<Self, CaptureError> {
        BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Self)
            .map_err(|_| CaptureError::new("a capture is already running"))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::SeqCst);
    }
}

// Owns the hook thread. Drop stops it.
struct Running {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl Running {
    // Joins after a checked post, or a dead thread.
    fn stop(&mut self) -> bool {
        let Some(worker) = self.worker.take() else {
            return true;
        };
        for _ in 0..STOP_TRIES {
            if worker.is_finished() || post_quit(self.thread_id) {
                let _ = worker.join();
                return true;
            }
            thread::sleep(STOP_RETRY);
        }
        false
    }

    // Posts without a join. Startup may still run.
    fn abandon(&mut self) {
        post_quit(self.thread_id);
        self.worker = None;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Arms the hook and waits for one result.
pub fn capture(limit: Duration) -> Result<CaptureResult, CaptureError> {
    let _claim = Claim::take()?;
    let (startup_tx, startup_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let worker = thread::Builder::new()
        .name("chibipop-capture".to_string())
        .spawn(move || run_hook_thread(startup_tx, result_tx))
        .map_err(|e| CaptureError::new(format!("spawning the capture thread: {e}")))?;
    let thread_id = match startup_rx.recv_timeout(STARTUP_LIMIT) {
        Ok(Startup::QueueReady(thread_id)) => thread_id,
        Ok(Startup::Installed(_)) => {
            return Err(CaptureError::new("the capture thread started out of order"));
        }
        Err(RecvTimeoutError::Timeout) => {
            return Err(CaptureError::new("the capture thread made no queue in time"));
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = worker.join();
            return Err(CaptureError::new("the capture thread ended during startup"));
        }
    };
    let mut running = Running {
        thread_id,
        worker: Some(worker),
    };
    match startup_rx.recv_timeout(STARTUP_LIMIT) {
        Ok(Startup::Installed(Ok(()))) => {}
        Ok(Startup::Installed(Err(message))) => return Err(CaptureError::new(message)),
        Ok(Startup::QueueReady(_)) | Err(RecvTimeoutError::Timeout) => {
            running.abandon();
            return Err(CaptureError::new("the capture hook did not install in time"));
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err(CaptureError::new("the capture thread ended during startup"));
        }
    }
    let outcome = match result_rx.recv_timeout(limit) {
        Ok(result) => Ok(result),
        Err(RecvTimeoutError::Timeout) => Ok(CaptureResult::TimedOut),
        Err(RecvTimeoutError::Disconnected) => {
            Err(CaptureError::new("the capture thread ended without a result"))
        }
    };
    if !running.stop() {
        return Err(CaptureError::new("the keyboard hook did not stop"));
    }
    outcome
}

/// Ends the armed capture, if one runs.
pub fn cancel() {
    if let Some(thread_id) = *armed_slot() {
        post_quit(thread_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT_CTRL: u16 = 0xA2;
    const LEFT_WIN: u16 = 0x5B;
    const F9: u16 = 0x78;
    const ESCAPE: u16 = 0x1B;

    fn first_result(machine: &mut Machine, edges: &[(u16, bool)]) -> Option<CaptureResult> {
        edges
            .iter()
            .find_map(|&(vk, is_down)| machine.feed(vk, is_down))
    }

    #[test]
    fn chord_from_edges() {
        let mut machine = Machine::new(&[]);
        let edges = [(LEFT_CTRL, true), (F9, true), (F9, false), (LEFT_CTRL, false)];
        assert_eq!(
            first_result(&mut machine, &edges),
            Some(CaptureResult::Chord {
                chord: "Ctrl+F9".to_string()
            })
        );
    }

    #[test]
    fn win_alone_commits_nothing() {
        let mut machine = Machine::new(&[]);
        assert_eq!(machine.feed(LEFT_WIN, true), None);
        assert_eq!(machine.feed(LEFT_WIN, false), None);
        assert!(!machine.is_done());
        assert_eq!(machine.feed(ESCAPE, true), Some(CaptureResult::Cancelled));
    }

    #[test]
    fn rejected_chord_carries_text() {
        let mut machine = Machine::new(&[]);
        let edges = [(LEFT_WIN, true), (F9, true)];
        assert_eq!(
            first_result(&mut machine, &edges),
            Some(CaptureResult::Rejected {
                chord: "Win+F9".to_string()
            })
        );
    }

    #[test]
    fn seeded_modifier_counts() {
        let mut machine = Machine::new(&[LEFT_CTRL]);
        assert_eq!(
            machine.feed(F9, true),
            Some(CaptureResult::Chord {
                chord: "Ctrl+F9".to_string()
            })
        );
    }
}
