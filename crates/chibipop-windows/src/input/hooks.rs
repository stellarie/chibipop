//! Installs machine-wide input hooks.
//!
//! The hooks block an armed wheel event before the next hook receives it.
//! The hooks never log keystrokes.
//! Hook callbacks use static state only.
//! A `HOOKPROC` cannot capture state.

use crate::config::TriggerMode;
use crate::geom::PhysPoint;
use anyhow::{anyhow, Context, Result};
use std::collections::VecDeque;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicU8, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Defines the movement threshold for the gate in physical pixels.
const MOVEMENT_GATE_PX: i64 = 4;

/// Marks the state with no stored point.
const NO_POINT: i64 = i64::MIN;

/// Defines the time limit for hook thread startup.
const HOOK_STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Stores the last point that the gate accepted.
static LAST_ACCEPTED: AtomicI64 = AtomicI64::new(NO_POINT);

/// Stores one candidate point when one exists.
static PENDING: AtomicI64 = AtomicI64::new(NO_POINT);

/// Stores the configured virtual-key code for the trigger key.
static TRIGGER_VK: AtomicU16 = AtomicU16::new(0x10);

/// Stores whether the trigger is active for popup display.
static KEY_DOWN: AtomicBool = AtomicBool::new(false);

/// Tracks physical trigger edges so Windows autorepeat does not toggle the latch.
static TRIGGER_PHYSICAL: AtomicBool = AtomicBool::new(false);

/// Stores one physical Press-mode trigger edge.
static PENDING_PRESS: AtomicBool = AtomicBool::new(false);

/// The main thread resets this flag on each tick.
/// A stuck `true` value blocks every wheel event.
static SCROLL_ARMED: AtomicBool = AtomicBool::new(false);

/// Stores wheel delta while capture is armed.
static PENDING_SCROLL: AtomicI32 = AtomicI32::new(0);

/// Arms click capture on the popup area.
static CLICK_ARMED: AtomicBool = AtomicBool::new(false);

/// Watches an outside button press while a Press-mode popup is visible.
/// The observer never swallows the click because an outside click in Press mode
/// is the user's click on another window. Chibipop only adds a hide action.
static OUTSIDE_WATCH: AtomicBool = AtomicBool::new(false);

/// Stores one outside button press in screen coordinates.
static PENDING_OUTSIDE: AtomicI64 = AtomicI64::new(NO_POINT);

/// This state stores button bits while the pointer is held.
static POINTER_BUTTONS: AtomicU8 = AtomicU8::new(0);

/// This state stores the latest move while a popup button is held.
static PENDING_POINTER_MOVE: AtomicI64 = AtomicI64::new(NO_POINT);

/// This constant limits the queue of edges from the hook to the pump.
const POINTER_QUEUE_CAPACITY: usize = 32;

/// A `PointerButton` represents one physical mouse button that the popup can consume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Right,
}

/// A `PointerEvent` represents one button edge that the low-level mouse hook captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerEvent {
    pub button: PointerButton,
    pub down: bool,
    pub point: PhysPoint,
}

/// This queue stores button edges until the message loop drains them.
static POINTER_EVENTS: LazyLock<Mutex<VecDeque<PointerEvent>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(POINTER_QUEUE_CAPACITY)));

fn pointer_events() -> &'static Mutex<VecDeque<PointerEvent>> {
    &POINTER_EVENTS
}

fn pointer_button_bit(button: PointerButton) -> u8 {
    match button {
        PointerButton::Left => 1,
        PointerButton::Right => 2,
    }
}

fn queue_pointer_event(event: PointerEvent) {
    let mut queue = pointer_events().lock().unwrap_or_else(|e| e.into_inner());
    if queue.len() == POINTER_QUEUE_CAPACITY {
        queue.pop_front();
    }
    queue.push_back(event);
}

/// Defines the number of `WHEEL_DELTA` units from `winuser.h`.
const WHEEL_DELTA_UNITS: i32 = 120;

/// Stores the trigger mode as `u8`.
static MODE: AtomicU8 = AtomicU8::new(0);

/// Stores the virtual-key code for the Add-to-Anki hotkey.
static ANKI_ADD_VK: AtomicU16 = AtomicU16::new(0x41);

/// Stores whether the popup is shown and Anki integration is enabled.
static ANKI_ADD_ARMED: AtomicBool = AtomicBool::new(false);

/// One stored hotkey press.
static PENDING_ADD: AtomicBool = AtomicBool::new(false);

/// Defines the number of action hotkey slots.
pub const MAX_ACTION_SLOTS: usize = 8;

/// Stores virtual-key codes for action hotkeys.
static ACTION_VK: [AtomicU16; MAX_ACTION_SLOTS] = [
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
    AtomicU16::new(0),
];

/// Stores modifier masks for action hotkeys.
static ACTION_MODS: [AtomicU8; MAX_ACTION_SLOTS] = [
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
    AtomicU8::new(0),
];

/// Stores one action press for each slot.
static PENDING_ACTION: [AtomicBool; MAX_ACTION_SLOTS] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

/// Marks an active Region selector.
static SELECTION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks that the history contains an entry.
static BACK_ARMED: AtomicBool = AtomicBool::new(false);

/// Stores one Escape press.
static PENDING_BACK: AtomicBool = AtomicBool::new(false);

/// Defines the virtual-key code for Escape.
const VK_ESCAPE: u16 = 0x1B;

/// Packs one point into one word so readers never see a torn value.
fn pack(p: PhysPoint) -> i64 {
    ((p.x as i64) << 32) | (p.y as u32 as i64)
}

fn unpack(v: i64) -> PhysPoint {
    PhysPoint {
        x: (v >> 32) as i32,
        y: v as i32,
    }
}

fn mode_to_u8(m: TriggerMode) -> u8 {
    match m {
        TriggerMode::Live => 0,
        TriggerMode::HoldKey | TriggerMode::HoldShift => 1,
        TriggerMode::Toggle => 2,
        TriggerMode::Press => 3,
    }
}

fn u8_to_mode(v: u8) -> TriggerMode {
    match v {
        1 => TriggerMode::HoldKey,
        2 => TriggerMode::Toggle,
        3 => TriggerMode::Press,
        _ => TriggerMode::Live,
    }
}

/// Returns whether a move can count now.
fn mode_currently_eligible() -> bool {
    match u8_to_mode(MODE.load(Ordering::SeqCst)) {
        TriggerMode::Live => true,
        TriggerMode::Press => false,
        _ => KEY_DOWN.load(Ordering::SeqCst),
    }
}

/// Updates trigger state without calling Win32 APIs.
///
/// Windows sends autorepeat key-down events, so the physical edge state filters
/// repeats. Toggle mode keeps its latch across release, and toggle-off resets the
/// movement gate before the next capture.
fn transition_trigger_state(down: bool, still_held: bool, mode: TriggerMode) {
    if down {
        if TRIGGER_PHYSICAL.swap(true, Ordering::SeqCst) {
            return;
        }
        if mode == TriggerMode::Press {
            PENDING_PRESS.store(true, Ordering::SeqCst);
            return;
        }
        if mode == TriggerMode::Toggle {
            if KEY_DOWN.fetch_xor(true, Ordering::SeqCst) {
                LAST_ACCEPTED.store(NO_POINT, Ordering::SeqCst);
                PENDING.store(NO_POINT, Ordering::SeqCst);
            }
        } else {
            KEY_DOWN.store(true, Ordering::SeqCst);
        }
    } else if !still_held {
        TRIGGER_PHYSICAL.store(false, Ordering::SeqCst);
        if mode == TriggerMode::Press {
            return;
        }
        if mode == TriggerMode::Toggle {
            return;
        }
        KEY_DOWN.store(false, Ordering::SeqCst);
        if mode != TriggerMode::Live {
            LAST_ACCEPTED.store(NO_POINT, Ordering::SeqCst);
            PENDING.store(NO_POINT, Ordering::SeqCst);
        }
    }
}

/// Returns the left and right virtual-key codes for a modifier.
fn modifier_variants(vk: u16) -> Option<(u16, u16)> {
    match vk {
        0x10 => Some((0xA0, 0xA1)),
        0x11 => Some((0xA2, 0xA3)),
        0x12 => Some((0xA4, 0xA5)),
        _ => None,
    }
}

/// Returns true when this event fires Add-to-Anki.
fn add_hotkey_hit(down: bool, vk: u16) -> bool {
    down && vk == ANKI_ADD_VK.load(Ordering::SeqCst) && ANKI_ADD_ARMED.load(Ordering::SeqCst)
}

/// Returns the current Ctrl, Shift, and Alt modifier mask.
fn current_modifiers() -> u8 {
    let mut m = 0u8;
    // SAFETY: This call has no preconditions.
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        if (GetAsyncKeyState(0x11) as u16 & 0x8000) != 0 {
            m |= crate::config::MOD_CTRL;
        }
        if (GetAsyncKeyState(0x10) as u16 & 0x8000) != 0 {
            m |= crate::config::MOD_SHIFT;
        }
        if (GetAsyncKeyState(0x12) as u16 & 0x8000) != 0 {
            m |= crate::config::MOD_ALT;
        }
    }
    m
}

/// Returns true and stores an action when the virtual-key code and modifiers match.
fn action_hotkey_hit(down: bool, vk: u16, mods: u8) -> bool {
    if !down {
        return false;
    }
    for i in 0..MAX_ACTION_SLOTS {
        let want_vk = ACTION_VK[i].load(Ordering::SeqCst);
        if want_vk == 0 {
            continue;
        }
        if vk == want_vk && mods == ACTION_MODS[i].load(Ordering::SeqCst) {
            PENDING_ACTION[i].store(true, Ordering::SeqCst);
            return true;
        }
    }
    false
}

/// Returns whether the Region selector is active.
fn selection_active() -> bool {
    SELECTION_ACTIVE.load(Ordering::SeqCst)
}

/// Returns whether this event matches the trigger key.
fn matches_trigger(vk: u16, target: u16) -> bool {
    if vk == target {
        return true;
    }
    if let Some((l, r)) = modifier_variants(target) {
        vk == l || vk == r
    } else {
        false
    }
}

/// Records one mouse move.
///
/// This path does not allocate, wait, or use I/O.
unsafe fn record_mouse_move(lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` calls this only when `code >= 0` and
    // `wparam == WM_MOUSEMOVE`. The `WH_MOUSE_LL` contract guarantees that
    // `lparam` points to a valid, aligned `MSLLHOOKSTRUCT` for this call.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let p = PhysPoint {
        x: data.pt.x,
        y: data.pt.y,
    };

    if POINTER_BUTTONS.load(Ordering::SeqCst) != 0 {
        PENDING_POINTER_MOVE.store(pack(p), Ordering::SeqCst);
    }

    if selection_active() {
        return;
    }
    if !mode_currently_eligible() {
        return;
    }

    let last = LAST_ACCEPTED.load(Ordering::SeqCst);
    let gate_open = last == NO_POINT || {
        let lp = unpack(last);
        (p.x as i64 - lp.x as i64).abs() > MOVEMENT_GATE_PX
            || (p.y as i64 - lp.y as i64).abs() > MOVEMENT_GATE_PX
    };
    if !gate_open {
        return;
    }
    let packed = pack(p);
    LAST_ACCEPTED.store(packed, Ordering::SeqCst);
    PENDING.store(packed, Ordering::SeqCst);
}

/// Tracks the configured trigger key.
///
/// It reads the event rather than current key state.
unsafe fn record_key_state(wparam: WPARAM, lparam: LPARAM) {
    // SAFETY: `keyboard_hook_proc` calls this only with `code >= 0`. Under
    // the `WH_KEYBOARD_LL` contract, `lparam` points to a live
    // `KBDLLHOOKSTRUCT` that the OS owns for the duration of this call.
    let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode } as u16;
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    if add_hotkey_hit(down, vk) {
        PENDING_ADD.store(true, Ordering::SeqCst);
    }
    action_hotkey_hit(down, vk, current_modifiers());
    if down && vk == VK_ESCAPE && BACK_ARMED.load(Ordering::SeqCst) {
        PENDING_BACK.store(true, Ordering::SeqCst);
    }

    let target = TRIGGER_VK.load(Ordering::SeqCst);
    if !matches_trigger(vk, target) {
        return;
    }
    if down {
        let mode = u8_to_mode(MODE.load(Ordering::SeqCst));
        transition_trigger_state(true, false, mode);
    } else {
        // For a modifier with left and right variants, clear the state only
        // when both sides are up.
        let still_held = modifier_variants(target).is_some_and(|(l, r)| {
            // The hook runs before Windows updates the key state.
            let other = if vk == l {
                r
            } else if vk == r {
                l
            } else {
                return false;
            };
            // SAFETY: This call has no preconditions.
            (unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(other as i32) }
                as u16
                & 0x8000)
                != 0
        });
        let mode = u8_to_mode(MODE.load(Ordering::SeqCst));
        transition_trigger_state(false, still_held, mode);
    }
}

/// This function stores one popup button edge in screen coordinates.
unsafe fn record_pointer_event(button: PointerButton, down: bool, lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` calls this only when `code >= 0` and the
    // message is one of the four button edges below. The `WH_MOUSE_LL`
    // contract guarantees a valid, aligned `MSLLHOOKSTRUCT` for this call.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let point = PhysPoint { x: data.pt.x, y: data.pt.y };
    let bit = pointer_button_bit(button);
    if down {
        POINTER_BUTTONS.fetch_or(bit, Ordering::SeqCst);
    } else {
        POINTER_BUTTONS.fetch_and(!bit, Ordering::SeqCst);
    }
    queue_pointer_event(PointerEvent { button, down, point });
}

/// Records one unarmed button press in screen coordinates.
fn record_outside_click(point: PhysPoint) {
    if !CLICK_ARMED.load(Ordering::SeqCst) && OUTSIDE_WATCH.load(Ordering::SeqCst) {
        PENDING_OUTSIDE.store(pack(point), Ordering::SeqCst);
    }
}

/// Reads and records one unarmed button press from a low-level hook event.
unsafe fn record_outside_click_from_lparam(lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` calls this only when `code >= 0` and
    // the message is a button-down edge. The `WH_MOUSE_LL` contract
    // guarantees a valid, aligned `MSLLHOOKSTRUCT` for this call.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    record_outside_click(PhysPoint { x: data.pt.x, y: data.pt.y });
}

/// Stores one wheel event's delta.
unsafe fn record_wheel(lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` calls this only when `code >= 0` and
    // `wparam == WM_MOUSEWHEEL`. The `WH_MOUSE_LL` contract guarantees that
    // `lparam` points to a valid, aligned `MSLLHOOKSTRUCT` for this call.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    accumulate_wheel((data.mouseData >> 16) as i16 as i32);
}

/// Stores wheel delta values without Win32 calls.
fn accumulate_wheel(delta: i32) {
    let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
        Some(v.saturating_add(delta))
    });
}

/// Handles `WH_MOUSE_LL` events.
///
/// An armed wheel event returns before the next hook receives it.
/// An outside Press-mode click reaches the next hook because it belongs to
/// another window. Chibipop only adds a hide action.
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        match wparam.0 as u32 {
            WM_MOUSEMOVE => {
                let _ = catch_unwind(|| unsafe { record_mouse_move(lparam) });
            }
            WM_LBUTTONDOWN if CLICK_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe {
                    record_pointer_event(PointerButton::Left, true, lparam)
                });
                return LRESULT(1);
            }
            WM_LBUTTONDOWN
                if !CLICK_ARMED.load(Ordering::SeqCst)
                    && OUTSIDE_WATCH.load(Ordering::SeqCst) =>
            {
                let _ = catch_unwind(|| unsafe { record_outside_click_from_lparam(lparam) });
            }
            WM_LBUTTONUP if CLICK_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe {
                    record_pointer_event(PointerButton::Left, false, lparam)
                });
                return LRESULT(1);
            }
            WM_RBUTTONDOWN if CLICK_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe {
                    record_pointer_event(PointerButton::Right, true, lparam)
                });
                return LRESULT(1);
            }
            WM_RBUTTONDOWN
                if !CLICK_ARMED.load(Ordering::SeqCst)
                    && OUTSIDE_WATCH.load(Ordering::SeqCst) =>
            {
                let _ = catch_unwind(|| unsafe { record_outside_click_from_lparam(lparam) });
            }
            WM_RBUTTONUP if CLICK_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe {
                    record_pointer_event(PointerButton::Right, false, lparam)
                });
                return LRESULT(1);
            }
            WM_MOUSEWHEEL if SCROLL_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe { record_wheel(lparam) });
                return LRESULT(1);
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Handles `WH_KEYBOARD_LL` events.
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let _ = catch_unwind(|| unsafe { record_key_state(wparam, lparam) });
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Owns the installed Win32 hooks.
struct InstalledHooks {
    mouse: HHOOK,
    keyboard: HHOOK,
}

enum HookStartup {
    QueueReady(u32),
    Installed(Result<()>),
}

impl InstalledHooks {
    /// Installs both hooks. On error, it removes the first hook before it returns.
    fn install() -> Result<InstalledHooks> {
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();

            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), Some(hinstance), 0)
                .context(
                "SetWindowsHookExW(WH_MOUSE_LL) failed - the mouse hook did not install",
            )?;

            let keyboard = match SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(keyboard_hook_proc),
                Some(hinstance),
                0,
            ) {
                Ok(h) => h,
                Err(e) => {
                    let _ = UnhookWindowsHookEx(mouse);
                    return Err(e).context(
                        "SetWindowsHookExW(WH_KEYBOARD_LL) failed - the keyboard hook did not install",
                    );
                }
            };

            Ok(InstalledHooks { mouse, keyboard })
        }
    }
}

impl Drop for InstalledHooks {
    /// Tries to remove both hooks and ignores removal errors.
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWindowsHookEx(self.mouse);
            let _ = UnhookWindowsHookEx(self.keyboard);
        }
    }
}

/// Controls the hook message thread.
pub struct Hooks {
    thread_id: u32,
    worker: Option<thread::JoinHandle<()>>,
}

impl Hooks {
    /// Starts the hook message thread.
    pub fn install() -> Result<Hooks> {
        let (startup_tx, startup_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("chibipop-hooks".to_string())
            .spawn(move || run_hook_thread(startup_tx))
            .context("spawning the low-level input hook thread")?;

        let thread_id = match startup_rx.recv_timeout(HOOK_STARTUP_TIMEOUT) {
            Ok(HookStartup::QueueReady(thread_id)) => thread_id,
            Ok(HookStartup::Installed(_)) => {
                let _ = worker.join();
                return Err(anyhow!(
                    "the low-level input hook thread reported startup out of order"
                ));
            }
            Err(RecvTimeoutError::Timeout) => {
                return Err(anyhow!(
                    "the low-level input hook thread did not create a message queue in time"
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err(anyhow!(
                    "the low-level input hook thread exited before startup completed"
                ));
            }
        };

        match startup_rx.recv_timeout(HOOK_STARTUP_TIMEOUT) {
            Ok(HookStartup::Installed(Ok(()))) => Ok(Hooks {
                thread_id,
                worker: Some(worker),
            }),
            Ok(HookStartup::Installed(Err(e))) => {
                let _ = worker.join();
                Err(e)
            }
            Ok(HookStartup::QueueReady(_)) => {
                if stop_hook_thread(thread_id) {
                    let _ = worker.join();
                }
                Err(anyhow!(
                    "the low-level input hook thread reported message queue readiness twice"
                ))
            }
            Err(RecvTimeoutError::Timeout) => {
                stop_hook_thread(thread_id);
                Err(anyhow!(
                    "the low-level input hook thread did not install hooks in time"
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                Err(anyhow!(
                    "the low-level input hook thread exited before installing hooks"
                ))
            }
        }
    }

    /// Arms or disarms wheel capture.
    pub fn set_scroll_armed(armed: bool) {
        SCROLL_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Returns whether wheel capture is armed.
    pub fn scroll_armed() -> bool {
        SCROLL_ARMED.load(Ordering::SeqCst)
    }

    /// Returns whether the trigger latch is active.
    ///
    /// Toggle mode keeps this latch active after key release.
    /// Hold mode clears it when the key changes from down to up.
    pub fn trigger_held() -> bool {
        KEY_DOWN.load(Ordering::SeqCst)
    }

    /// Delivers one Press-mode edge to the pump.
    ///
    /// Press mode uses a pulse instead of `KEY_DOWN`, so it never emits hold
    /// edges.
    pub fn take_press() -> bool {
        PENDING_PRESS.swap(false, Ordering::SeqCst)
    }

    /// Sets the virtual-key code for the trigger key.
    pub fn set_trigger_key(vk: u16) {
        TRIGGER_VK.store(vk, Ordering::SeqCst);
    }

    /// Sets the virtual-key code for the Add-to-Anki hotkey.
    pub fn set_add_hotkey(vk: u16) {
        ANKI_ADD_VK.store(vk, Ordering::SeqCst);
    }

    /// Arms or disarms the add-to-Anki hotkey.
    pub fn set_add_armed(armed: bool) {
        ANKI_ADD_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Takes one stored add-to-Anki press.
    pub fn take_add_hotkey() -> bool {
        PENDING_ADD.swap(false, Ordering::SeqCst)
    }

    /// Takes only complete wheel notches.
    ///
    /// The rest of the delta stays stored.
    pub fn take_whole_notches() -> i32 {
        let mut whole = 0;
        // Only the successful `fetch_update` call stores the remainder.
        let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            let remainder = v % WHEEL_DELTA_UNITS;
            whole = (v - remainder) / WHEEL_DELTA_UNITS;
            Some(remainder)
        });
        whole
    }

    /// Drops all accumulated wheel delta.
    pub fn discard_scroll() {
        PENDING_SCROLL.store(0, Ordering::SeqCst);
    }

    /// This function arms or disarms popup pointer capture.
    pub fn set_click_armed(armed: bool) {
        let changed = CLICK_ARMED.swap(armed, Ordering::SeqCst) != armed;
        if changed && !armed {
            POINTER_BUTTONS.store(0, Ordering::SeqCst);
        }
        if changed {
            pointer_events()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            PENDING_POINTER_MOVE.store(NO_POINT, Ordering::SeqCst);
        }
    }

    /// Watches outside button presses while a Press-mode popup is visible.
    ///
    /// The observer never swallows the click because an outside click in Press
    /// mode is the user's click on another window. Chibipop only adds a hide.
    pub fn set_outside_watch(watch: bool) {
        OUTSIDE_WATCH.store(watch, Ordering::SeqCst);
        if !watch {
            PENDING_OUTSIDE.store(NO_POINT, Ordering::SeqCst);
        }
    }

    /// Takes one observed outside button press.
    pub fn take_outside_click() -> Option<PhysPoint> {
        let v = PENDING_OUTSIDE.swap(NO_POINT, Ordering::SeqCst);
        (v != NO_POINT).then(|| unpack(v))
    }

    /// This function takes all queued popup button edges in callback order.
    pub fn take_pointer_events() -> Vec<PointerEvent> {
        pointer_events()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
    }

    /// This function takes the latest popup move when a button was held since the last tick.
    pub fn take_pointer_move() -> Option<PhysPoint> {
        let v = PENDING_POINTER_MOVE.swap(NO_POINT, Ordering::SeqCst);
        (v != NO_POINT).then(|| unpack(v))
    }

    /// Sets the mode that controls the trigger gate.
    ///
    /// A mode change clears the physical edge, latch, and pending Press state.
    /// This prevents a previous mode from delivering an edge after the change.
    pub fn set_mode(m: TriggerMode) {
        let mode = mode_to_u8(m);
        if MODE.swap(mode, Ordering::SeqCst) != mode {
            KEY_DOWN.store(false, Ordering::SeqCst);
            TRIGGER_PHYSICAL.store(false, Ordering::SeqCst);
            PENDING_PRESS.store(false, Ordering::SeqCst);
        }
    }

    /// Takes the stored candidate point.
    ///
    /// The atomic swap returns it at most once.
    pub fn take_pending() -> Option<PhysPoint> {
        let v = PENDING.swap(NO_POINT, Ordering::SeqCst);
        if v == NO_POINT {
            None
        } else {
            Some(unpack(v))
        }
    }

    /// Arms or disarms Back for the Escape key.
    pub fn set_back_armed(armed: bool) {
        BACK_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Takes one stored Back action.
    pub fn take_back() -> bool {
        PENDING_BACK.swap(false, Ordering::SeqCst)
    }

    /// Uses a polled fallback for the movement gate.
    pub fn poll_gate(p: PhysPoint) -> bool {
        if !mode_currently_eligible() {
            return false;
        }
        let last = LAST_ACCEPTED.load(Ordering::SeqCst);
        let open = last == NO_POINT || {
            let lp = unpack(last);
            (p.x as i64 - lp.x as i64).abs() > MOVEMENT_GATE_PX
                || (p.y as i64 - lp.y as i64).abs() > MOVEMENT_GATE_PX
        };
        if open {
            let packed = pack(p);
            LAST_ACCEPTED.store(packed, Ordering::SeqCst);
        }
        open
    }

    /// Sets one action hotkey slot.
    pub fn set_action_hotkey(slot: usize, vk: u16, modifiers: u8) {
        if slot < MAX_ACTION_SLOTS {
            ACTION_VK[slot].store(vk, Ordering::SeqCst);
            ACTION_MODS[slot].store(modifiers, Ordering::SeqCst);
        }
    }

    /// Takes one stored action for a slot.
    pub fn take_action_hotkey(slot: usize) -> bool {
        if slot < MAX_ACTION_SLOTS {
            PENDING_ACTION[slot].swap(false, Ordering::SeqCst)
        } else {
            false
        }
    }

    /// Sets the Region selector state.
    pub fn set_selection_active(active: bool) {
        SELECTION_ACTIVE.store(active, Ordering::SeqCst);
    }
}

impl Drop for Hooks {
    /// Stops the hook message thread.
    fn drop(&mut self) {
        SCROLL_ARMED.store(false, Ordering::SeqCst);
        CLICK_ARMED.store(false, Ordering::SeqCst);
        POINTER_BUTTONS.store(0, Ordering::SeqCst);
        pointer_events()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        PENDING_POINTER_MOVE.store(NO_POINT, Ordering::SeqCst);
        ANKI_ADD_ARMED.store(false, Ordering::SeqCst);
        BACK_ARMED.store(false, Ordering::SeqCst);
        let posted = stop_hook_thread(self.thread_id);
        if let Some(worker) = self.worker.take() {
            if posted && worker.thread().id() != thread::current().id() {
                let _ = worker.join();
            }
        }
    }
}

fn stop_hook_thread(thread_id: u32) -> bool {
    unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)).is_ok() }
}

fn run_hook_thread(startup_tx: mpsc::Sender<HookStartup>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    let mut msg = MSG::default();
    unsafe {
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
    }
    if startup_tx.send(HookStartup::QueueReady(thread_id)).is_err() {
        return;
    }

    let hooks = match InstalledHooks::install() {
        Ok(hooks) => hooks,
        Err(e) => {
            let _ = startup_tx.send(HookStartup::Installed(Err(e)));
            return;
        }
    };
    if startup_tx.send(HookStartup::Installed(Ok(()))).is_err() {
        return;
    }

    loop {
        let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if got.0 <= 0 {
            break;
        }
    }
    drop(hooks);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests share the wheel state.
    static WHEEL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn wheel_guard() -> std::sync::MutexGuard<'static, ()> {
        WHEEL_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// The tests share the popup pointer queue.
    static POINTER_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn pointer_guard() -> std::sync::MutexGuard<'static, ()> {
        POINTER_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The tests share trigger transition state.
    static TRIGGER_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn trigger_guard() -> std::sync::MutexGuard<'static, ()> {
        TRIGGER_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn toggle_transitions_latch_and_reset_gate_on_toggle_off() {
        let _g = trigger_guard();
        KEY_DOWN.store(false, Ordering::SeqCst);
        TRIGGER_PHYSICAL.store(false, Ordering::SeqCst);
        LAST_ACCEPTED.store(1, Ordering::SeqCst);
        PENDING.store(2, Ordering::SeqCst);

        transition_trigger_state(true, false, TriggerMode::Toggle);
        assert!(Hooks::trigger_held());
        transition_trigger_state(false, false, TriggerMode::Toggle);
        assert!(Hooks::trigger_held());
        assert_eq!(1, LAST_ACCEPTED.load(Ordering::SeqCst));
        assert_eq!(2, PENDING.load(Ordering::SeqCst));

        transition_trigger_state(true, false, TriggerMode::Toggle);
        assert!(!Hooks::trigger_held());
        assert_eq!(NO_POINT, LAST_ACCEPTED.load(Ordering::SeqCst));
        assert_eq!(NO_POINT, PENDING.load(Ordering::SeqCst));
        transition_trigger_state(false, false, TriggerMode::Toggle);
    }

    #[test]
    fn toggle_ignores_repeated_key_down_without_release() {
        let _g = trigger_guard();
        KEY_DOWN.store(false, Ordering::SeqCst);
        TRIGGER_PHYSICAL.store(false, Ordering::SeqCst);

        transition_trigger_state(true, false, TriggerMode::Toggle);
        transition_trigger_state(true, false, TriggerMode::Toggle);
        assert!(Hooks::trigger_held());

        transition_trigger_state(false, false, TriggerMode::Toggle);
    }

    #[test]
    fn hold_key_transitions_follow_press_and_release() {
        let _g = trigger_guard();
        KEY_DOWN.store(false, Ordering::SeqCst);
        TRIGGER_PHYSICAL.store(false, Ordering::SeqCst);

        transition_trigger_state(true, false, TriggerMode::HoldKey);
        assert!(Hooks::trigger_held());
        transition_trigger_state(false, false, TriggerMode::HoldKey);
        assert!(!Hooks::trigger_held());
    }

    #[test]
    fn press_transitions_queue_one_edge_without_latching() {
        let _g = trigger_guard();
        KEY_DOWN.store(false, Ordering::SeqCst);
        TRIGGER_PHYSICAL.store(false, Ordering::SeqCst);
        PENDING_PRESS.store(false, Ordering::SeqCst);

        transition_trigger_state(true, false, TriggerMode::Press);
        assert!(Hooks::take_press());
        assert!(!Hooks::take_press());
        assert!(!Hooks::trigger_held());

        transition_trigger_state(true, false, TriggerMode::Press);
        assert!(!Hooks::take_press(), "autorepeat must not queue another press");

        transition_trigger_state(false, false, TriggerMode::Press);
        assert!(!Hooks::take_press(), "release must not queue a press");
        assert!(!TRIGGER_PHYSICAL.load(Ordering::SeqCst));
    }

    #[test]
    fn armed_pointer_edges_are_swallowed_and_queued() {
        let _g = pointer_guard();
        Hooks::set_click_armed(false);
        Hooks::set_click_armed(true);
        let edges = [
            (WM_LBUTTONDOWN, PointerButton::Left, true, POINT { x: 10, y: 20 }),
            (WM_LBUTTONUP, PointerButton::Left, false, POINT { x: 11, y: 21 }),
            (WM_RBUTTONDOWN, PointerButton::Right, true, POINT { x: 12, y: 22 }),
            (WM_RBUTTONUP, PointerButton::Right, false, POINT { x: 13, y: 23 }),
        ];
        for &(message, _, _, point) in &edges {
            let data = MSLLHOOKSTRUCT {
                pt: point,
                ..Default::default()
            };
            let lparam = LPARAM(&data as *const MSLLHOOKSTRUCT as isize);
            // SAFETY: `data` is live and aligned for this callback, like the
            // structure supplied by the low-level mouse hook.
            let result = unsafe { mouse_hook_proc(0, WPARAM(message as usize), lparam) };
            assert_eq!(1, result.0, "an armed button edge must be swallowed");
        }
        let got = Hooks::take_pointer_events();
        let want: Vec<_> = edges
            .into_iter()
            .map(|(_, button, down, point)| PointerEvent { button, down, point: PhysPoint { x: point.x, y: point.y } })
            .collect();
        assert_eq!(want, got);
        Hooks::set_click_armed(false);
    }

    #[test]
    fn outside_watch_records_one_unarmed_button_press() {
        let _g = pointer_guard();
        Hooks::set_click_armed(false);
        Hooks::set_outside_watch(false);
        assert_eq!(None, Hooks::take_outside_click());

        Hooks::set_outside_watch(true);
        let point = PhysPoint { x: 10, y: 20 };
        record_outside_click(point);
        assert_eq!(Some(point), Hooks::take_outside_click());
        assert_eq!(None, Hooks::take_outside_click());

        Hooks::set_outside_watch(false);
        record_outside_click(point);
        assert_eq!(None, Hooks::take_outside_click());

        Hooks::set_outside_watch(true);
        record_outside_click(point);
        Hooks::set_outside_watch(false);
        assert_eq!(None, Hooks::take_outside_click());
    }

    #[test]
    fn wheel_notches_bank_their_sub_notch_remainder() {
        let _g = wheel_guard();
        Hooks::discard_scroll();

        // Exact multiples leave no remainder.
        accumulate_wheel(240);
        assert_eq!(2, Hooks::take_whole_notches());
        assert_eq!(
            0,
            Hooks::take_whole_notches(),
            "nothing should be left over"
        );

        // High-resolution deltas must combine before they form a notch.

        accumulate_wheel(40);
        assert_eq!(0, Hooks::take_whole_notches(), "40 is not yet a notch");
        accumulate_wheel(40);
        assert_eq!(0, Hooks::take_whole_notches(), "80 is not yet a notch");
        accumulate_wheel(40);
        assert_eq!(
            1,
            Hooks::take_whole_notches(),
            "40+40+40 is one whole notch"
        );
        assert_eq!(0, Hooks::take_whole_notches());

        // The remainder keeps the sign of the dividend.
        accumulate_wheel(-140);
        assert_eq!(-1, Hooks::take_whole_notches());
        accumulate_wheel(-100);
        assert_eq!(-1, Hooks::take_whole_notches(), "-20 banked plus -100");
        assert_eq!(0, Hooks::take_whole_notches());

        // The test discards the accumulator and drops the remainder.
        accumulate_wheel(80);
        Hooks::discard_scroll();
        assert_eq!(0, Hooks::take_whole_notches());
    }

    /// Tests the highest-risk callback path.
    ///
    /// This test covers the armed path only.
    #[test]
    fn an_armed_wheel_event_is_swallowed_and_banked() {
        let _g = wheel_guard();
        Hooks::discard_scroll();
        Hooks::set_scroll_armed(true);

        let data = MSLLHOOKSTRUCT {
            pt: POINT { x: 0, y: 0 },
            mouseData: (WHEEL_DELTA_UNITS as u32) << 16,
            flags: 0,
            time: 0,
            dwExtraInfo: 0,
        };
        let lparam = LPARAM(&data as *const MSLLHOOKSTRUCT as isize);

        // SAFETY: The test supplies the contract that the OS provides for a
        // `WM_MOUSEWHEEL` delivery: `code >= 0` and `lparam` points to a live,
        // aligned `MSLLHOOKSTRUCT` that stays valid for this call. The
        // structure lives on this stack frame. The event is armed, so the
        // callback returns before it reaches `CallNextHookEx`.
        let result = unsafe { mouse_hook_proc(0, WPARAM(WM_MOUSEWHEEL as usize), lparam) };

        assert_eq!(1, result.0, "an armed wheel event must be swallowed");
        assert_eq!(1, Hooks::take_whole_notches(), "and its delta banked");

        Hooks::set_scroll_armed(false);
        Hooks::discard_scroll();
    }

    /// Confirms that a large delta saturates and does not wrap.
    #[test]
    fn a_saturated_accumulator_yields_a_bounded_notch_count() {
        let _g = wheel_guard();
        Hooks::discard_scroll();
        accumulate_wheel(i32::MAX);
        accumulate_wheel(i32::MAX);
        let notches = Hooks::take_whole_notches();
        assert_eq!(i32::MAX / WHEEL_DELTA_UNITS, notches);
        assert!(notches.saturating_mul(4096) > 0, "must not wrap negative");
        Hooks::discard_scroll();
    }

    #[test]
    fn matches_trigger_exact_vk() {
        assert!(matches_trigger(0x10, 0x10));
        assert!(matches_trigger(0x70, 0x70));
    }

    #[test]
    fn matches_trigger_shift_variants() {
        // These values are the `VK_LSHIFT` and `VK_RSHIFT` virtual-key codes.
        assert!(matches_trigger(0xA0, 0x10));
        assert!(matches_trigger(0xA1, 0x10));
    }

    #[test]
    fn matches_trigger_ctrl_variants() {
        assert!(matches_trigger(0xA2, 0x11));
        assert!(matches_trigger(0xA3, 0x11));
    }

    #[test]
    fn matches_trigger_alt_variants() {
        assert!(matches_trigger(0xA4, 0x12));
        assert!(matches_trigger(0xA5, 0x12));
    }

    #[test]
    fn matches_trigger_unrelated_vk() {
        assert!(!matches_trigger(0x41, 0x10));
        assert!(!matches_trigger(0x70, 0x10));
    }

    #[test]
    fn matches_trigger_arbitrary_letter_key() {
        assert!(matches_trigger(0x41, 0x41));
        assert!(!matches_trigger(0x42, 0x41));
    }

    #[test]
    fn modifier_variants_known() {
        assert_eq!(Some((0xA0, 0xA1)), modifier_variants(0x10));
        assert_eq!(Some((0xA2, 0xA3)), modifier_variants(0x11));
        assert_eq!(Some((0xA4, 0xA5)), modifier_variants(0x12));
    }

    #[test]
    fn modifier_variants_f_key_has_none() {
        assert_eq!(None, modifier_variants(0x70));
    }

    // ---- add-to-anki hotkey ----

    /// The tests share the hotkey state.
    static ADD_HOTKEY_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn add_hotkey_guard() -> std::sync::MutexGuard<'static, ()> {
        ADD_HOTKEY_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn add_hotkey_hit_requires_a_keydown() {
        let _g = add_hotkey_guard();
        Hooks::set_add_hotkey(0x41);
        Hooks::set_add_armed(true);
        assert!(!add_hotkey_hit(false, 0x41), "a keyup must not fire");
        Hooks::set_add_armed(false);
    }

    #[test]
    fn add_hotkey_hit_requires_the_configured_vk() {
        let _g = add_hotkey_guard();
        Hooks::set_add_hotkey(0x41);
        Hooks::set_add_armed(true);
        assert!(!add_hotkey_hit(true, 0x42), "the wrong key must not fire");
        assert!(add_hotkey_hit(true, 0x41));
        Hooks::set_add_armed(false);
    }

    #[test]
    fn add_hotkey_hit_requires_arming() {
        let _g = add_hotkey_guard();
        Hooks::set_add_hotkey(0x41);
        Hooks::set_add_armed(false);
        assert!(!add_hotkey_hit(true, 0x41), "disarmed must not fire");
    }

    #[test]
    fn take_add_hotkey_is_a_one_shot_swap() {
        let _g = add_hotkey_guard();
        PENDING_ADD.store(true, Ordering::SeqCst);
        assert!(Hooks::take_add_hotkey());
        assert!(!Hooks::take_add_hotkey(), "a second take sees it cleared");
    }

    /// Exercises the real `KBDLLHOOKSTRUCT`.
    #[test]
    fn record_key_state_arms_pending_for_the_add_key() {
        let _g = add_hotkey_guard();
        Hooks::set_add_hotkey(0x41);
        Hooks::set_add_armed(true);
        let _ = Hooks::take_add_hotkey();

        let data = KBDLLHOOKSTRUCT {
            vkCode: 0x41,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: `data` is a live, aligned `KBDLLHOOKSTRUCT` on this stack
        // frame for the whole call. This matches the contract that
        // `record_key_state` receives from the real `WH_KEYBOARD_LL` hook.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(Hooks::take_add_hotkey());
        Hooks::set_add_armed(false);
    }

    /// A different key must not arm the hotkey.
    #[test]
    fn record_key_state_ignores_an_unrelated_key() {
        let _g = add_hotkey_guard();
        Hooks::set_add_hotkey(0x41);
        Hooks::set_add_armed(true);
        let _ = Hooks::take_add_hotkey();

        let data = KBDLLHOOKSTRUCT {
            vkCode: 0x42,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: This test uses the same contract as the test above.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(!Hooks::take_add_hotkey(), "a different key must not arm it");
        Hooks::set_add_armed(false);
    }

    #[test]
    fn action_hotkey_fires_on_matching_key_and_mods() {
        let _g = add_hotkey_guard();
        Hooks::set_action_hotkey(0, 0x53, 0b011);
        let _ = Hooks::take_action_hotkey(0);
        assert!(action_hotkey_hit(true, 0x53, 0b011));
        assert!(Hooks::take_action_hotkey(0));
    }

    #[test]
    fn action_hotkey_ignores_wrong_modifiers() {
        let _g = add_hotkey_guard();
        Hooks::set_action_hotkey(0, 0x53, 0b011);
        let _ = Hooks::take_action_hotkey(0);
        assert!(!action_hotkey_hit(true, 0x53, 0b001));
        assert!(!Hooks::take_action_hotkey(0));
    }

    #[test]
    fn action_hotkey_ignores_wrong_key() {
        let _g = add_hotkey_guard();
        Hooks::set_action_hotkey(0, 0x53, 0b011);
        let _ = Hooks::take_action_hotkey(0);
        assert!(!action_hotkey_hit(true, 0x41, 0b011));
        assert!(!Hooks::take_action_hotkey(0));
    }

    #[test]
    fn action_hotkey_take_is_one_shot() {
        let _g = add_hotkey_guard();
        Hooks::set_action_hotkey(0, 0x53, 0);
        let _ = Hooks::take_action_hotkey(0);
        PENDING_ACTION[0].store(true, Ordering::SeqCst);
        assert!(Hooks::take_action_hotkey(0));
        assert!(!Hooks::take_action_hotkey(0));
    }

    #[test]
    fn action_hotkey_out_of_bounds_returns_false() {
        assert!(!Hooks::take_action_hotkey(99));
    }

    #[test]
    fn selection_active_suppresses_mouse_moves() {
        Hooks::set_selection_active(true);
        assert!(selection_active());
        Hooks::set_selection_active(false);
        assert!(!selection_active());
    }

    // ---- back (Escape) ----

    static BACK_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn back_guard() -> std::sync::MutexGuard<'static, ()> {
        BACK_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn back_requires_arming() {
        let _g = back_guard();
        Hooks::set_back_armed(false);
        let _ = Hooks::take_back();

        let data = KBDLLHOOKSTRUCT {
            vkCode: VK_ESCAPE as u32,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: This test uses the same contract as the add-hotkey tests.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(!Hooks::take_back());
    }

    #[test]
    fn back_fires_on_escape_when_armed() {
        let _g = back_guard();
        Hooks::set_back_armed(true);
        let _ = Hooks::take_back();

        let data = KBDLLHOOKSTRUCT {
            vkCode: VK_ESCAPE as u32,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: This test uses the same contract as the add-hotkey tests.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(Hooks::take_back());
        assert!(!Hooks::take_back());
        Hooks::set_back_armed(false);
    }

    #[test]
    fn back_ignores_non_escape_keys() {
        let _g = back_guard();
        Hooks::set_back_armed(true);
        let _ = Hooks::take_back();

        let data = KBDLLHOOKSTRUCT {
            vkCode: 0x41,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: This test uses the same contract as the add-hotkey tests.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(!Hooks::take_back());
        Hooks::set_back_armed(false);
    }
}
