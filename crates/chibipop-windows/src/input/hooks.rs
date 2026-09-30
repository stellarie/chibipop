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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
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


/// One configured action edge delivered to the pump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindEvent {
    pub id: Arc<str>,
    pub action: crate::config::BindAction,
    pub down: bool,
}

#[derive(Debug)]
struct HookBind {
    id: Arc<str>,
    action: crate::config::BindAction,
    mode: TriggerMode,
    vk: u16,
    modifiers: Option<u8>,
    down: bool,
    modifier_sides: u8,
    retired: bool,
}

#[derive(Debug, Default)]
struct ConfiguredBinds {
    binds: Vec<HookBind>,
    pending: VecDeque<BindEvent>,
}

static CONFIGURED_BINDS: LazyLock<Mutex<ConfiguredBinds>> =
    LazyLock::new(|| Mutex::new(ConfiguredBinds::default()));

/// Tracks physical key edges independently from the current bind list.
static PHYSICAL_KEYS: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

/// Keeps the selected-text key consumed through release after a rebind.
static SELECTED_TEXT_SWALLOWED: AtomicU16 = AtomicU16::new(0);

/// Marks whether a configured Anki-add action can run on the active popup.
static ADD_ARMED: AtomicBool = AtomicBool::new(false);

/// Keeps an eligible Anki-add key consumed through repeats and release.
static ADD_SWALLOWED_KEYS: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

/// Marks an active Region selector.
static SELECTION_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Marks that the history contains an entry.
static BACK_ARMED: AtomicBool = AtomicBool::new(false);

/// Stores one Escape press.
static PENDING_BACK: AtomicBool = AtomicBool::new(false);
static PENDING_ESCAPE: AtomicBool = AtomicBool::new(false);

/// Defines the virtual-key code for Escape.
const VK_ESCAPE: u16 = 0x1B;

static ESCAPE_GENERATION: AtomicU64 = AtomicU64::new(0);
static ESCAPE_DOWN: AtomicBool = AtomicBool::new(false);

pub(crate) struct EscapeCancellation(u64);

impl EscapeCancellation {
    pub(crate) fn new() -> Self { Self(ESCAPE_GENERATION.load(Ordering::SeqCst)) }
    pub(crate) fn cancelled(&self) -> bool { self.0 != ESCAPE_GENERATION.load(Ordering::SeqCst) }
}

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


/// Returns the left and right virtual-key codes for a modifier.
fn modifier_variants(vk: u16) -> Option<(u16, u16)> {
    match vk {
        0x10 => Some((0xA0, 0xA1)),
        0x11 => Some((0xA2, 0xA3)),
        0x12 => Some((0xA4, 0xA5)),
        _ => None,
    }
}

fn bind_key_matches(vk: u16, target: u16) -> bool {
    vk == target || modifier_variants(target).is_some_and(|(left, right)| vk == left || vk == right)
}

fn modifier_side(vk: u16, target: u16) -> Option<u8> {
    let (left, right) = modifier_variants(target)?;
    match vk {
        key if key == left => Some(1),
        key if key == right => Some(2),
        _ => None,
    }
}

fn physical_modifier_sides(target: u16) -> u8 {
    let Some((left, right)) = modifier_variants(target) else { return 0 };
    u8::from(PHYSICAL_KEYS[left as usize].load(Ordering::SeqCst))
        | (u8::from(PHYSICAL_KEYS[right as usize].load(Ordering::SeqCst)) << 1)
}

fn modifier_bit(vk: u16) -> u8 {
    match vk {
        0x10 | 0xA0 | 0xA1 => crate::config::MOD_SHIFT,
        0x11 | 0xA2 | 0xA3 => crate::config::MOD_CTRL,
        0x12 | 0xA4 | 0xA5 => crate::config::MOD_ALT,
        _ => 0,
    }
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

/// Returns true and stores one selected-text edge when a configured chord matches.
fn action_hotkey_hit(down: bool, vk: u16, mods: u8) -> bool {
    let Some(physical_key) = PHYSICAL_KEYS.get(vk as usize) else {
        return false;
    };
    let edge = physical_key.swap(down, Ordering::SeqCst) != down;
    if !edge {
        return false;
    }

    let mut state = CONFIGURED_BINDS.lock().unwrap_or_else(|e| e.into_inner());
    let ConfiguredBinds { binds, pending } = &mut *state;
    let mut selected_text = false;
    for bind in binds {
        if !bind_key_matches(vk, bind.vk) {
            continue;
        }
        let side = modifier_side(vk, bind.vk);
        if down {
            if let Some(side) = side {
                let already_held = bind.modifier_sides != 0;
                bind.modifier_sides |= side;
                if bind.down || already_held {
                    continue;
                }
            } else if bind.down {
                continue;
            }
            if bind.action == crate::config::BindAction::AnkiAdd && !ADD_ARMED.load(Ordering::SeqCst) {
                continue;
            }
            let active_modifiers = mods & !modifier_bit(bind.vk);
            if !bind.retired && bind.modifiers.is_none_or(|expected| active_modifiers == expected) {
                bind.down = true;
                if bind.action == crate::config::BindAction::AnkiAdd {
                    ADD_SWALLOWED_KEYS[vk as usize].store(true, Ordering::SeqCst);
                }
                pending.push_back(BindEvent {
                    id: Arc::clone(&bind.id),
                    action: bind.action,
                    down: true,
                });
                selected_text |= bind.action == crate::config::BindAction::SelectedText;
            }
        } else {
            if let Some(side) = side {
                bind.modifier_sides &= !side;
                if bind.modifier_sides != 0 {
                    continue;
                }
            } else {
                bind.modifier_sides = 0;
            }
            if bind.down {
                bind.down = false;
                if bind.action == crate::config::BindAction::Lookup && bind.mode == TriggerMode::HoldKey {
                    pending.push_back(BindEvent {
                        id: Arc::clone(&bind.id),
                        action: bind.action,
                        down: false,
                    });
                }
            }
        }
    }
    selected_text
}

fn add_key_swallowed(down: bool, vk: u16) -> bool {
    let Some(swallowed) = ADD_SWALLOWED_KEYS.get(vk as usize) else { return false };
    if down {
        swallowed.load(Ordering::SeqCst)
    } else {
        swallowed.swap(false, Ordering::SeqCst)
    }
}

/// The selection action consumes its key through release, even after a rebind.
fn selected_text_key(down: bool, vk: u16, eligible: bool, matched: bool) -> bool {
    if vk != 0 && SELECTED_TEXT_SWALLOWED.load(Ordering::SeqCst) == vk {
        if !down {
            SELECTED_TEXT_SWALLOWED.store(0, Ordering::SeqCst);
        }
        return true;
    }
    if !down || !eligible || !matched {
        return false;
    }
    SELECTED_TEXT_SWALLOWED.store(vk, Ordering::SeqCst);
    true
}

fn own_foreground() -> bool {
    // SAFETY: These queries retain no window or process resource.
    unsafe {
        let window = GetForegroundWindow();
        let mut process = 0;
        GetWindowThreadProcessId(window, Some(&mut process));
        window.0.is_null() || process == GetCurrentProcessId()
    }
}

/// Returns whether the Region selector is active.
fn selection_active() -> bool {
    SELECTION_ACTIVE.load(Ordering::SeqCst)
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

/// Tracks keyboard edges and dispatches configured binds.
///
/// It reads the event rather than current key state.
unsafe fn record_key_state(wparam: WPARAM, lparam: LPARAM) -> bool {
    // SAFETY: `keyboard_hook_proc` calls this only with `code >= 0`. Under
    // the `WH_KEYBOARD_LL` contract, `lparam` points to a live
    // `KBDLLHOOKSTRUCT` that the OS owns for the duration of this call.
    let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode } as u16;
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let escape = vk == VK_ESCAPE && !ESCAPE_DOWN.swap(down, Ordering::SeqCst) && down;
    if escape { ESCAPE_GENERATION.fetch_add(1, Ordering::SeqCst); }
    let selecting = selection_active();
    let mods = current_modifiers();
    let own = own_foreground();
    let selected_text_hit = action_hotkey_hit(down, vk, mods);
    let swallow = selected_text_key(down, vk, !selecting && !own, selected_text_hit);
    let add_swallow = add_key_swallowed(down, vk);
    if own {
        CONFIGURED_BINDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .retain(|event| event.action != crate::config::BindAction::SelectedText);
    }
    if selecting {
        discard_keyboard_actions();
        return swallow || add_swallow;
    }
    if crate::ui::search_window::is_foreground() {
        cancel_keyboard_actions();
        return swallow || add_swallow;
    }
    if escape { PENDING_ESCAPE.store(true, Ordering::SeqCst); }
    if escape && BACK_ARMED.load(Ordering::SeqCst) {
        PENDING_BACK.store(true, Ordering::SeqCst);
    }
    swallow || add_swallow
}

pub fn clear_keyboard_actions() {
    discard_keyboard_actions();
}

pub fn cancel_keyboard_actions() {
    discard_keyboard_actions();
}

pub fn discard_keyboard_actions() {
    PENDING.store(NO_POINT, Ordering::SeqCst);
    PENDING_BACK.store(false, Ordering::SeqCst);
    PENDING_ESCAPE.store(false, Ordering::SeqCst);
    CONFIGURED_BINDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .pending
        .clear();
}

#[cfg(test)]
pub(crate) fn search_keyboard_test_guard() -> impl Sized {
    tests::keyboard_guard()
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

/// The pump checks actual rectangles because arming can lag cursor motion.
fn record_outside_click(point: PhysPoint) {
    if OUTSIDE_WATCH.load(Ordering::SeqCst) {
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
            WM_MBUTTONDOWN | WM_XBUTTONDOWN if OUTSIDE_WATCH.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe { record_outside_click_from_lparam(lparam) });
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
    if code >= 0 && catch_unwind(|| unsafe { record_key_state(wparam, lparam) }).unwrap_or(false) {
        return LRESULT(1);
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
        if SCROLL_ARMED.swap(armed, Ordering::SeqCst) != armed {
            eprintln!("chibipop: action=set_scroll_armed armed={armed}");
        }
    }

    /// Returns whether wheel capture is armed.
    pub fn scroll_armed() -> bool {
        SCROLL_ARMED.load(Ordering::SeqCst)
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
        if changed {
            eprintln!("chibipop: action=set_click_armed armed={armed}");
        }
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

    /// Clears held pointer state.
    pub fn discard_pointer_state() {
        POINTER_BUTTONS.store(0, Ordering::SeqCst);
        pointer_events()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        PENDING_POINTER_MOVE.store(NO_POINT, Ordering::SeqCst);
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

    /// Arms or disarms configured Anki-add keys.
    pub fn set_add_armed(armed: bool) {
        if ADD_ARMED.swap(armed, Ordering::SeqCst) != armed {
            eprintln!("chibipop: action=set_add_armed armed={armed}");
        }
    }

    /// Arms or disarms Back for the Escape key.
    pub fn set_back_armed(armed: bool) {
        if BACK_ARMED.swap(armed, Ordering::SeqCst) != armed {
            eprintln!("chibipop: action=set_back_armed armed={armed}");
        }
    }

    /// Takes one stored Back action.
    pub fn take_back() -> bool {
        PENDING_BACK.swap(false, Ordering::SeqCst)
    }

    pub fn take_escape() -> bool {
        PENDING_ESCAPE.swap(false, Ordering::SeqCst)
    }

    /// Uses a polled fallback for the movement gate.
    pub fn poll_gate(p: PhysPoint) -> bool {
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

    /// Replaces the Windows chord table, drops queued activations, and keeps queued lookup releases.
    pub fn set_configured_binds(binds: &[crate::config::Bind]) {
        let mut state = CONFIGURED_BINDS.lock().unwrap_or_else(|e| e.into_inner());
        let mut held: Vec<_> = state
            .binds
            .drain(..)
            .filter(|bind| {
                bind.down
                    && bind.action == crate::config::BindAction::Lookup
                    && bind.mode == TriggerMode::HoldKey
            })
            .collect();
        state
            .pending
            .retain(|edge| edge.action == crate::config::BindAction::Lookup && !edge.down);
        state.pending.reserve(binds.len().saturating_mul(2));
        state.binds.reserve(binds.len() + held.len());
        for bind in binds.iter().filter(|bind| bind.enabled) {
            let Some((vk, modifiers)) = bind.windows_key() else {
                continue;
            };
            if vk == 0 || vk >= PHYSICAL_KEYS.len() as u16 {
                continue;
            }
            let carried = held.iter().position(|old| {
                old.id.as_ref() == bind.id
                    && old.action == bind.action
                    && old.mode == bind.mode
                    && old.vk == vk
                    && old.modifiers == modifiers
            });
            let (down, modifier_sides) = carried
                .map(|index| {
                    let old = held.swap_remove(index);
                    (old.down, old.modifier_sides)
                })
                .unwrap_or_else(|| (false, physical_modifier_sides(vk)));
            state.binds.push(HookBind {
                id: Arc::from(bind.id.as_str()),
                action: bind.action,
                mode: bind.mode,
                vk,
                modifiers,
                down,
                modifier_sides,
                retired: false,
            });
        }
        for mut bind in held {
            bind.retired = true;
            state.binds.push(bind);
        }
    }

    /// Takes all configured action edges in callback order.
    pub fn take_configured_binds() -> Vec<BindEvent> {
        CONFIGURED_BINDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .drain(..)
            .collect()
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

    static KEYBOARD_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(super) fn keyboard_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = KEYBOARD_STATE.lock().unwrap_or_else(|e| e.into_inner());
        for key in &PHYSICAL_KEYS {
            key.store(false, Ordering::SeqCst);
        }
        SELECTED_TEXT_SWALLOWED.store(0, Ordering::SeqCst);
        ESCAPE_DOWN.store(false, Ordering::SeqCst);
        BACK_ARMED.store(false, Ordering::SeqCst);
        ADD_ARMED.store(false, Ordering::SeqCst);
        for key in &ADD_SWALLOWED_KEYS {
            key.store(false, Ordering::SeqCst);
        }
        SELECTION_ACTIVE.store(false, Ordering::SeqCst);
        clear_keyboard_actions();
        let mut state = CONFIGURED_BINDS.lock().unwrap_or_else(|e| e.into_inner());
        state.binds.clear();
        state.pending.clear();
        drop(state);
        guard
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
    fn discard_pointer_state_drops_buttons_edges_and_move() {
        let _g = pointer_guard();
        Hooks::set_click_armed(false);
        Hooks::set_click_armed(true);
        let data = MSLLHOOKSTRUCT {
            pt: POINT { x: 10, y: 20 },
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const MSLLHOOKSTRUCT as isize);
        // SAFETY: `data` is live and aligned for this callback, like the
        // structure supplied by the low-level mouse hook.
        let result = unsafe { mouse_hook_proc(0, WPARAM(WM_LBUTTONDOWN as usize), lparam) };
        assert_eq!(1, result.0, "an armed button edge must be swallowed");
        assert_eq!(1, POINTER_BUTTONS.load(Ordering::SeqCst));
        assert_eq!(1, Hooks::take_pointer_events().len(), "the edge is queued");

        queue_pointer_event(PointerEvent {
            button: PointerButton::Left,
            down: true,
            point: PhysPoint { x: 10, y: 20 },
        });
        PENDING_POINTER_MOVE.store(pack(PhysPoint { x: 11, y: 21 }), Ordering::SeqCst);

        Hooks::discard_pointer_state();

        assert_eq!(
            0,
            POINTER_BUTTONS.load(Ordering::SeqCst),
            "buttons are dropped"
        );
        assert!(
            Hooks::take_pointer_events().is_empty(),
            "queued edges are dropped"
        );
        assert_eq!(None, Hooks::take_pointer_move(), "the move is dropped");
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
    fn configured_modifier_keys_match_both_physical_sides() {
        for (generic, left, right) in [(0x10, 0xA0, 0xA1), (0x11, 0xA2, 0xA3), (0x12, 0xA4, 0xA5)] {
            assert!(bind_key_matches(left, generic));
            assert!(bind_key_matches(right, generic));
            assert_eq!(Some(1), modifier_side(left, generic));
            assert_eq!(Some(2), modifier_side(right, generic));
        }
        assert!(bind_key_matches(0x70, 0x70));
        assert!(!bind_key_matches(0x41, 0x10));
        assert_eq!(None, modifier_side(0x70, 0x70));
    }

    #[test]
    fn a_modifier_key_bind_does_not_count_its_own_key_as_a_chord_modifier() {
        let _guard = keyboard_guard();
        let bind = configured_bind(
            "search-control",
            crate::config::BindAction::Search,
            "Ctrl",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));

        assert!(!action_hotkey_hit(true, 0xA2, crate::config::MOD_CTRL));
        assert_eq!(
            vec![BindEvent { id: Arc::from("search-control"), action: crate::config::BindAction::Search, down: true }],
            Hooks::take_configured_binds(),
        );
        assert!(!action_hotkey_hit(false, 0xA2, 0));
        let side_bind = configured_bind(
            "search-left-control",
            crate::config::BindAction::Search,
            "0xA2",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&side_bind));
        assert!(!action_hotkey_hit(true, 0xA2, crate::config::MOD_CTRL));
        assert_eq!(
            vec![BindEvent { id: Arc::from("search-left-control"), action: crate::config::BindAction::Search, down: true }],
            Hooks::take_configured_binds(),
        );
        Hooks::set_configured_binds(&[]);
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


    fn configured_bind(
        id: &str,
        action: crate::config::BindAction,
        chord: &str,
        mode: TriggerMode,
    ) -> crate::config::Bind {
        let mut bind = crate::config::Bind::new(id.to_string(), action);
        bind.windows = chord.to_string();
        bind.mode = mode;
        bind
    }
    #[test]
    fn a_held_lookup_modifier_stays_owned_until_both_sides_are_up() {
        let _guard = keyboard_guard();
        let bind = configured_bind(
            "lookup-control",
            crate::config::BindAction::Lookup,
            "Ctrl",
            TriggerMode::HoldKey,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));

        assert!(!action_hotkey_hit(true, 0xA2, 0));
        assert_eq!(
            vec![BindEvent { id: Arc::from("lookup-control"), action: crate::config::BindAction::Lookup, down: true }],
            Hooks::take_configured_binds(),
        );
        assert!(!action_hotkey_hit(true, 0xA3, crate::config::MOD_CTRL));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0xA2, crate::config::MOD_CTRL));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0xA3, crate::config::MOD_CTRL));
        assert_eq!(
            vec![BindEvent { id: Arc::from("lookup-control"), action: crate::config::BindAction::Lookup, down: false }],
            Hooks::take_configured_binds(),
        );
    }

    #[test]
    fn a_rebound_modifier_bind_does_not_claim_an_already_held_chord() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0xA2].store(true, Ordering::SeqCst);
        let bind = configured_bind(
            "search-control",
            crate::config::BindAction::Search,
            "Ctrl",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));

        assert!(!action_hotkey_hit(true, 0xA3, crate::config::MOD_CTRL));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0xA2, crate::config::MOD_CTRL));
        assert!(!action_hotkey_hit(false, 0xA3, 0));
        assert!(!action_hotkey_hit(true, 0xA2, 0));
        assert_eq!(
            vec![BindEvent { id: Arc::from("search-control"), action: crate::config::BindAction::Search, down: true }],
            Hooks::take_configured_binds(),
        );
    }

    #[test]
    fn displaced_lookup_release_keeps_its_original_bind_id() {
        let _guard = keyboard_guard();
        let first = configured_bind(
            "lookup-first",
            crate::config::BindAction::Lookup,
            "F6",
            TriggerMode::HoldKey,
        );
        let second = configured_bind(
            "lookup-second",
            crate::config::BindAction::Lookup,
            "F7",
            TriggerMode::HoldKey,
        );
        Hooks::set_configured_binds(&[first, second]);

        assert!(!action_hotkey_hit(true, 0x75, 0));
        assert!(!action_hotkey_hit(true, 0x76, 0));
        assert_eq!(
            vec![
                BindEvent { id: Arc::from("lookup-first"), action: crate::config::BindAction::Lookup, down: true },
                BindEvent { id: Arc::from("lookup-second"), action: crate::config::BindAction::Lookup, down: true },
            ],
            Hooks::take_configured_binds(),
        );
        assert!(!action_hotkey_hit(false, 0x75, 0));
        assert_eq!(
            vec![BindEvent { id: Arc::from("lookup-first"), action: crate::config::BindAction::Lookup, down: false }],
            Hooks::take_configured_binds(),
        );
        assert!(!action_hotkey_hit(false, 0x76, 0));
        assert_eq!(
            vec![BindEvent { id: Arc::from("lookup-second"), action: crate::config::BindAction::Lookup, down: false }],
            Hooks::take_configured_binds(),
        );
    }

    #[test]
    fn configured_binds_are_dynamic_and_fire_once_per_physical_press() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0x53].store(false, Ordering::SeqCst);
        let mut binds: Vec<_> = (0..12)
            .map(|index| configured_bind(
                &format!("bind-{index}"),
                crate::config::BindAction::Search,
                "",
                TriggerMode::Press,
            ))
            .collect();
        binds[11].windows = "Ctrl+S".into();
        Hooks::set_configured_binds(&binds);

        assert!(!action_hotkey_hit(true, 0x53, 0));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x53, 0));
        assert!(!action_hotkey_hit(
            true,
            0x53,
            crate::config::MOD_CTRL | crate::config::MOD_SHIFT,
        ));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x53, 0));
        assert!(!action_hotkey_hit(true, 0x53, crate::config::MOD_CTRL));
        assert_eq!(
            vec![BindEvent {
                id: Arc::from("bind-11"),
                action: crate::config::BindAction::Search,
                down: true,
            }],
            Hooks::take_configured_binds(),
        );
        assert!(!action_hotkey_hit(true, 0x53, crate::config::MOD_CTRL));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x53, 0));
        assert!(!action_hotkey_hit(true, 0x53, crate::config::MOD_CTRL));
        assert_eq!(1, Hooks::take_configured_binds().len());

        Hooks::set_configured_binds(&[]);
        PHYSICAL_KEYS[0x53].store(false, Ordering::SeqCst);
    }

    #[test]
    fn anki_add_keys_pass_through_unarmed_and_stay_consumed_through_release() {
        let _guard = keyboard_guard();
        let bind = configured_bind(
            "anki-add",
            crate::config::BindAction::AnkiAdd,
            "A",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));

        Hooks::set_add_armed(false);
        assert!(!action_hotkey_hit(true, 0x41, 0));
        assert!(!add_key_swallowed(true, 0x41));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x41, 0));
        assert!(!add_key_swallowed(false, 0x41));

        Hooks::set_add_armed(true);
        assert!(!action_hotkey_hit(true, 0x41, 0));
        assert!(add_key_swallowed(true, 0x41));
        assert!(!action_hotkey_hit(true, 0x41, 0));
        assert!(add_key_swallowed(true, 0x41));
        Hooks::set_add_armed(false);
        assert!(!action_hotkey_hit(false, 0x41, 0));
        assert!(add_key_swallowed(false, 0x41));
        assert!(!add_key_swallowed(false, 0x41));
        assert_eq!(
            vec![BindEvent { id: Arc::from("anki-add"), action: crate::config::BindAction::AnkiAdd, down: true }],
            Hooks::take_configured_binds(),
        );
        Hooks::set_configured_binds(&[]);
    }

    #[test]
    fn lookup_hold_release_survives_rebind_and_keeps_its_identity() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0x75].store(false, Ordering::SeqCst);
        let bind = configured_bind(
            "lookup-old",
            crate::config::BindAction::Lookup,
            "F6",
            TriggerMode::HoldKey,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));
        assert!(!action_hotkey_hit(true, 0x75, 0));
        assert_eq!(
            vec![BindEvent {
                id: Arc::from("lookup-old"),
                action: crate::config::BindAction::Lookup,
                down: true,
            }],
            Hooks::take_configured_binds(),
        );

        Hooks::set_configured_binds(&[]);
        assert!(!action_hotkey_hit(false, 0x75, 0));
        assert_eq!(
            vec![BindEvent {
                id: Arc::from("lookup-old"),
                action: crate::config::BindAction::Lookup,
                down: false,
            }],
            Hooks::take_configured_binds(),
        );
        assert!(!action_hotkey_hit(true, 0x75, 0), "a retired bind cannot activate again");
        assert!(Hooks::take_configured_binds().is_empty());
        Hooks::set_configured_binds(&[]);
        PHYSICAL_KEYS[0x75].store(false, Ordering::SeqCst);
    }

    #[test]
    fn bind_replacement_keeps_queued_lookup_release_and_drops_queued_activation() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0x75].store(false, Ordering::SeqCst);
        PHYSICAL_KEYS[0x76].store(false, Ordering::SeqCst);
        let lookup = configured_bind(
            "lookup",
            crate::config::BindAction::Lookup,
            "F6",
            TriggerMode::HoldKey,
        );
        let search = configured_bind(
            "search",
            crate::config::BindAction::Search,
            "F7",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(&[lookup, search]);
        assert!(!action_hotkey_hit(true, 0x75, 0));
        assert_eq!(
            vec![BindEvent {
                id: Arc::from("lookup"),
                action: crate::config::BindAction::Lookup,
                down: true,
            }],
            Hooks::take_configured_binds(),
        );

        assert!(!action_hotkey_hit(false, 0x75, 0));
        assert!(!action_hotkey_hit(true, 0x76, 0));
        Hooks::set_configured_binds(&[]);
        assert_eq!(
            vec![BindEvent {
                id: Arc::from("lookup"),
                action: crate::config::BindAction::Lookup,
                down: false,
            }],
            Hooks::take_configured_binds(),
        );
        PHYSICAL_KEYS[0x75].store(false, Ordering::SeqCst);
        PHYSICAL_KEYS[0x76].store(false, Ordering::SeqCst);
    }

    #[test]
    fn a_mode_change_keeps_the_displaced_hold_release() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0x75].store(false, Ordering::SeqCst);
        let hold = configured_bind(
            "lookup",
            crate::config::BindAction::Lookup,
            "F6",
            TriggerMode::HoldKey,
        );
        let press = configured_bind(
            "lookup",
            crate::config::BindAction::Lookup,
            "F6",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&hold));
        assert!(!action_hotkey_hit(true, 0x75, 0));
        assert_eq!(1, Hooks::take_configured_binds().len());

        Hooks::set_configured_binds(std::slice::from_ref(&press));
        assert!(!action_hotkey_hit(false, 0x75, 0));
        assert_eq!(
            vec![BindEvent {
                id: Arc::from("lookup"),
                action: crate::config::BindAction::Lookup,
                down: false,
            }],
            Hooks::take_configured_binds(),
        );
        Hooks::set_configured_binds(&[]);
        PHYSICAL_KEYS[0x75].store(false, Ordering::SeqCst);
    }

    #[test]
    fn a_held_key_does_not_retrigger_after_bind_table_replacement() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0x74].store(false, Ordering::SeqCst);
        let bind = configured_bind(
            "search",
            crate::config::BindAction::Search,
            "F5",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));
        assert!(!action_hotkey_hit(true, 0x74, 0));
        assert_eq!(1, Hooks::take_configured_binds().len());

        Hooks::set_configured_binds(std::slice::from_ref(&bind));
        assert!(!action_hotkey_hit(true, 0x74, 0));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x74, 0));
        assert!(!action_hotkey_hit(true, 0x74, 0));
        assert_eq!(1, Hooks::take_configured_binds().len());

        Hooks::set_configured_binds(&[]);
        PHYSICAL_KEYS[0x74].store(false, Ordering::SeqCst);
    }

    #[test]
    fn selected_text_consumes_repeats_and_release_after_rebinding() {
        let _guard = keyboard_guard();
        PHYSICAL_KEYS[0x47].store(false, Ordering::SeqCst);
        SELECTED_TEXT_SWALLOWED.store(0, Ordering::SeqCst);
        let bind = configured_bind(
            "selected",
            crate::config::BindAction::SelectedText,
            "Ctrl+G",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));

        let matched = action_hotkey_hit(true, 0x47, crate::config::MOD_CTRL);
        assert!(selected_text_key(true, 0x47, true, matched));
        assert!(selected_text_key(true, 0x47, false, false));
        Hooks::set_configured_binds(&[]);
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x47, 0));
        assert!(selected_text_key(false, 0x47, false, false));
        assert!(!selected_text_key(false, 0x47, false, false));

        PHYSICAL_KEYS[0x47].store(false, Ordering::SeqCst);
    }

    #[test]
    fn selection_active_suppresses_mouse_moves() {
        let _guard = keyboard_guard();
        LAST_ACCEPTED.store(NO_POINT, Ordering::SeqCst);
        PENDING.store(NO_POINT, Ordering::SeqCst);
        Hooks::set_selection_active(true);
        let data = MSLLHOOKSTRUCT { pt: POINT { x: 12, y: 34 }, ..Default::default() };
        // SAFETY: The test keeps the Win32 hook payload live for the callback.
        unsafe { record_mouse_move(LPARAM(&data as *const MSLLHOOKSTRUCT as isize)); }
        assert_eq!(None, Hooks::take_pending());
        Hooks::set_selection_active(false);
        // SAFETY: The same live payload is valid for this second callback.
        unsafe { record_mouse_move(LPARAM(&data as *const MSLLHOOKSTRUCT as isize)); }
        assert_eq!(Some(PhysPoint { x: 12, y: 34 }), Hooks::take_pending());
    }

    // ---- back (Escape) ----


    #[test]
    fn back_requires_arming() {
        let _g = keyboard_guard();
        Hooks::set_back_armed(false);
        let _ = Hooks::take_back();

        let cancel = EscapeCancellation::new();
        assert!(!cancel.cancelled());
        let data = KBDLLHOOKSTRUCT {
            vkCode: VK_ESCAPE as u32,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: The test keeps a valid keyboard payload alive for the callback.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(!Hooks::take_back());
        assert!(cancel.cancelled(), "operations cancel even when no popup arms Back");
        assert!(Hooks::take_escape());
        assert!(!Hooks::take_escape());
        // SAFETY: The hook payload remains alive for the repeated keydown.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam); }
        assert!(!Hooks::take_escape(), "holding Escape must not cancel another level");
    }

    #[test]
    fn cancellation_blocks_action_repeats_until_release() {
        let _guard = search_keyboard_test_guard();
        PHYSICAL_KEYS[0x79].store(false, Ordering::SeqCst);
        let bind = configured_bind(
            "repeat",
            crate::config::BindAction::Search,
            "F10",
            TriggerMode::Press,
        );
        Hooks::set_configured_binds(std::slice::from_ref(&bind));
        assert!(!action_hotkey_hit(true, 0x79, 0));
        assert_eq!(1, Hooks::take_configured_binds().len());
        cancel_keyboard_actions();
        assert!(!action_hotkey_hit(true, 0x79, 0));
        assert!(Hooks::take_configured_binds().is_empty());
        assert!(!action_hotkey_hit(false, 0x79, 0));
        assert!(!action_hotkey_hit(true, 0x79, 0));
        assert_eq!(1, Hooks::take_configured_binds().len());
        Hooks::set_configured_binds(&[]);
        clear_keyboard_actions();
        PHYSICAL_KEYS[0x79].store(false, Ordering::SeqCst);
    }

    #[test]
    fn back_fires_on_escape_when_armed() {
        let _g = keyboard_guard();
        Hooks::set_back_armed(true);
        let _ = Hooks::take_back();

        let data = KBDLLHOOKSTRUCT {
            vkCode: VK_ESCAPE as u32,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: The test keeps a valid keyboard payload alive for the callback.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(Hooks::take_back());
        assert!(!Hooks::take_back());
        Hooks::set_back_armed(false);
    }

    #[test]
    fn back_ignores_non_escape_keys() {
        let _g = keyboard_guard();
        Hooks::set_back_armed(true);
        let _ = Hooks::take_back();

        let data = KBDLLHOOKSTRUCT {
            vkCode: 0x41,
            ..Default::default()
        };
        let lparam = LPARAM(&data as *const KBDLLHOOKSTRUCT as isize);
        // SAFETY: The test keeps a valid keyboard payload alive for the callback.
        unsafe { record_key_state(WPARAM(WM_KEYDOWN as usize), lparam) };

        assert!(!Hooks::take_back());
        Hooks::set_back_armed(false);
    }
}
