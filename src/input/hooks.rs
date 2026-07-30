//! Machine-wide input hooks.
//!
//! Only armed wheel is eaten.
//! Never logs a keystroke.
//! State is in statics only:
//! HOOKPROC cannot capture.

use crate::config::TriggerMode;
use crate::geom::PhysPoint;
use anyhow::{Context, Result};
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LSHIFT, VK_RSHIFT, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Movement gate, physical px.
const MOVEMENT_GATE_PX: i64 = 4;

/// "No point stored" sentinel.
const NO_POINT: i64 = i64::MIN;

/// Last point the gate accepted.
static LAST_ACCEPTED: AtomicI64 = AtomicI64::new(NO_POINT);

/// The one candidate, if any.
static PENDING: AtomicI64 = AtomicI64::new(NO_POINT);

/// Whether Shift is held.
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);

/// Left and right, bits 0 and 1.
static SHIFT_SIDES: AtomicU8 = AtomicU8::new(0);

/// Shift went up: drop the popup.
static HIDE_PENDING: AtomicBool = AtomicBool::new(false);

/// Stuck true kills every wheel.
///
/// Reset each main-thread tick.
static SCROLL_ARMED: AtomicBool = AtomicBool::new(false);

/// Delta banked while armed.
static PENDING_SCROLL: AtomicI32 = AtomicI32::new(0);

/// `WHEEL_DELTA`, per winuser.h.
const WHEEL_DELTA_UNITS: i32 = 120;

/// Trigger mode, packed as u8.
static MODE: AtomicU8 = AtomicU8::new(0);

/// One word: reads never tear.
fn pack(p: PhysPoint) -> i64 {
    ((p.x as i64) << 32) | (p.y as u32 as i64)
}

fn unpack(v: i64) -> PhysPoint {
    PhysPoint { x: (v >> 32) as i32, y: v as i32 }
}

fn mode_to_u8(m: TriggerMode) -> u8 {
    match m {
        TriggerMode::Live => 0,
        TriggerMode::HoldShift => 1,
    }
}

fn u8_to_mode(v: u8) -> TriggerMode {
    if v == 1 { TriggerMode::HoldShift } else { TriggerMode::Live }
}

/// Whether a move may count now.
fn mode_currently_eligible() -> bool {
    match u8_to_mode(MODE.load(Ordering::SeqCst)) {
        TriggerMode::Live => true,
        TriggerMode::HoldShift => SHIFT_DOWN.load(Ordering::SeqCst),
    }
}

/// Mouse hook's per-event work.
///
/// No alloc, no block, no I/O.
unsafe fn record_mouse_move(lparam: LPARAM) {
    // SAFETY: mouse_hook_proc only calls this when code >= 0 and
    // wparam == WM_MOUSEMOVE - the WH_MOUSE_LL contract that guarantees
    // lparam is a valid, aligned pointer to an MSLLHOOKSTRUCT for the
    // duration of this call.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let p = PhysPoint { x: data.pt.x, y: data.pt.y };

    if !mode_currently_eligible() {
        return;
    }

    let last = LAST_ACCEPTED.load(Ordering::SeqCst);
    let gate_open = last == NO_POINT || {
        let lp = unpack(last);
        // i64 so the diff cannot wrap.
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

/// Reads Shift, never the key.
///
/// From the event, not the state.
unsafe fn record_shift_state(wparam: WPARAM, lparam: LPARAM) {
    // SAFETY: keyboard_hook_proc only calls this with code >= 0, the
    // WH_KEYBOARD_LL contract under which `lparam` is a live
    // KBDLLHOOKSTRUCT owned by the OS for the duration of this call.
    let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode } as u16;
    // Its own bit: it is injected.
    let bit: u8 = match vk {
        v if v == VK_LSHIFT.0 => 1,
        v if v == VK_RSHIFT.0 => 2,
        v if v == VK_SHIFT.0 => 4,
        _ => return,
    };
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    let before = SHIFT_SIDES
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |s| Some(next_sides(s, bit, down)))
        .unwrap_or(0);
    let (now, released) = shift_transition(before, bit, down);
    SHIFT_DOWN.store(now != 0, Ordering::SeqCst);
    // Live retracts on movement.
    let hold = matches!(u8_to_mode(MODE.load(Ordering::SeqCst)), TriggerMode::HoldShift);
    if released && hold {
        LAST_ACCEPTED.store(NO_POINT, Ordering::SeqCst);
        // Or it re-shows after the hide.
        PENDING.store(NO_POINT, Ordering::SeqCst);
        HIDE_PENDING.store(true, Ordering::SeqCst);
    }
}

/// The mask after this event.
fn next_sides(sides: u8, bit: u8, down: bool) -> u8 {
    if down { sides | bit } else { sides & !bit }
}

/// Mask, and did the last leave.
fn shift_transition(sides: u8, bit: u8, down: bool) -> (u8, bool) {
    let now = next_sides(sides, bit, down);
    (now, sides != 0 && now == 0)
}

/// Banks one event's delta.
unsafe fn record_wheel(lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` only calls this when code >= 0 and
    // wparam == WM_MOUSEWHEEL - the WH_MOUSE_LL contract that guarantees
    // lparam is a valid, aligned pointer to an MSLLHOOKSTRUCT for the
    // duration of this call, exactly as for `record_mouse_move`.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    accumulate_wheel((data.mouseData >> 16) as i16 as i32);
}

/// Notch maths, without Win32.
fn accumulate_wheel(delta: i32) {
    let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
        Some(v.saturating_add(delta))
    });
}

/// `WH_MOUSE_LL` callback.
///
/// Armed wheel: the one swallow.
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        match wparam.0 as u32 {
            // Unwinding here would be UB.
            WM_MOUSEMOVE => {
                let _ = catch_unwind(|| unsafe { record_mouse_move(lparam) });
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

/// `WH_KEYBOARD_LL` callback.
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let _ = catch_unwind(|| unsafe { record_shift_state(wparam, lparam) });
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// The two installed hooks.
pub struct Hooks {
    mouse: HHOOK,
    keyboard: HHOOK,
}

impl Hooks {
    /// Installs both, or neither.
    pub fn install() -> Result<Hooks> {
        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None)
                .context("GetModuleHandleW(None)")?
                .into();

            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), Some(hinstance), 0)
                .context("SetWindowsHookExW(WH_MOUSE_LL) failed - the mouse hook did not install")?;

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

            Ok(Hooks { mouse, keyboard })
        }
    }

    /// Arms/disarms wheel capture.
    pub fn set_scroll_armed(armed: bool) {
        SCROLL_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Whether capture is armed.
    pub fn scroll_armed() -> bool {
        SCROLL_ARMED.load(Ordering::SeqCst)
    }

    /// Did Shift just come up?
    ///
    /// Hold mode gates moves off, so
    /// no move can retract the popup.
    pub fn take_hide() -> bool {
        HIDE_PENDING.swap(false, Ordering::SeqCst)
    }

    /// Takes whole notches only.
    ///
    /// Sub-notch rest stays banked.
    pub fn take_whole_notches() -> i32 {
        let mut whole = 0;
        // Only the winning run stores.
        let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            let remainder = v % WHEEL_DELTA_UNITS;
            whole = (v - remainder) / WHEEL_DELTA_UNITS;
            Some(remainder)
        });
        whole
    }

    /// Drops everything accumulated.
    pub fn discard_scroll() {
        PENDING_SCROLL.store(0, Ordering::SeqCst);
    }

    /// Sets the gating mode.
    pub fn set_mode(m: TriggerMode) {
        MODE.store(mode_to_u8(m), Ordering::SeqCst);
    }

    /// Takes the candidate point.
    ///
    /// Swap: never handed out twice.
    pub fn take_pending() -> Option<PhysPoint> {
        let v = PENDING.swap(NO_POINT, Ordering::SeqCst);
        if v == NO_POINT {
            None
        } else {
            Some(unpack(v))
        }
    }
}

impl Drop for Hooks {
    /// Unhooks both, best effort.
    fn drop(&mut self) {
        // Redundant; unhook restores.
        SCROLL_ARMED.store(false, Ordering::SeqCst);
        unsafe {
            let _ = UnhookWindowsHookEx(self.mouse);
            let _ = UnhookWindowsHookEx(self.keyboard);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wheel statics are shared.
    static WHEEL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn wheel_guard() -> std::sync::MutexGuard<'static, ()> {
        WHEEL_STATE.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn wheel_notches_bank_their_sub_notch_remainder() {
        let _g = wheel_guard();
        Hooks::discard_scroll();

        // Exact multiples: none banked.
        accumulate_wheel(240);
        assert_eq!(2, Hooks::take_whole_notches());
        assert_eq!(0, Hooks::take_whole_notches(), "nothing should be left over");

        // Hi-res deltas must add up.
        accumulate_wheel(40);
        assert_eq!(0, Hooks::take_whole_notches(), "40 is not yet a notch");
        accumulate_wheel(40);
        assert_eq!(0, Hooks::take_whole_notches(), "80 is not yet a notch");
        accumulate_wheel(40);
        assert_eq!(1, Hooks::take_whole_notches(), "40+40+40 is one whole notch");
        assert_eq!(0, Hooks::take_whole_notches());

        // % keeps the dividend's sign.
        accumulate_wheel(-140);
        assert_eq!(-1, Hooks::take_whole_notches());
        accumulate_wheel(-100);
        assert_eq!(-1, Hooks::take_whole_notches(), "-20 banked plus -100");
        assert_eq!(0, Hooks::take_whole_notches());

        // Replacing drops the rest too.
        accumulate_wheel(80);
        Hooks::discard_scroll();
        assert_eq!(0, Hooks::take_whole_notches());
    }

    /// The riskiest line we have.
    ///
    /// Unarmed path: verified live.
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

        // SAFETY: this is the exact contract the OS provides for a
        // WM_MOUSEWHEEL delivery - code >= 0 and lparam pointing at a live,
        // aligned MSLLHOOKSTRUCT that outlives the call (it is on this
        // frame's stack). Armed, so the callback returns before ever
        // reaching `CallNextHookEx`.
        let result = unsafe {
            mouse_hook_proc(0, WPARAM(WM_MOUSEWHEEL as usize), lparam)
        };

        assert_eq!(1, result.0, "an armed wheel event must be swallowed");
        assert_eq!(1, Hooks::take_whole_notches(), "and its delta banked");

        Hooks::set_scroll_armed(false);
        Hooks::discard_scroll();
    }

    /// A long stall pins, not wraps.
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
    fn releasing_the_last_shift_asks_for_a_hide() {
        // Left down, then left up.
        let (m, hide) = shift_transition(0, 1, true);
        assert_eq!((1, false), (m, hide));
        let (m, hide) = shift_transition(m, 1, false);
        assert_eq!((0, true), (m, hide), "the popup must be retracted");
    }

    #[test]
    fn one_shift_up_while_the_other_is_held_does_not_hide() {
        let (m, _) = shift_transition(0, 1, true);
        let (m, _) = shift_transition(m, 2, true);
        assert_eq!(3, m);

        let (m, hide) = shift_transition(m, 1, false);

        assert_eq!(2, m, "the right one is still down");
        assert!(!hide, "still held, so nothing is retracted");
    }

    #[test]
    fn a_repeated_key_down_does_not_ask_for_a_hide() {
        let (m, _) = shift_transition(0, 1, true);
        let (m, hide) = shift_transition(m, 1, true);
        assert_eq!(1, m);
        assert!(!hide);
    }

    #[test]
    fn an_up_with_nothing_held_asks_for_nothing() {
        assert_eq!((0, false), shift_transition(0, 1, false));
    }

    #[test]
    fn an_injected_vk_shift_does_not_strand_a_side_bit() {
        // VK_SHIFT down, LShift up.
        let (m, _) = shift_transition(0, 4, true);
        let (m, hide) = shift_transition(m, 1, false);
        assert!(!hide, "the injected key is still down");

        let (m, hide) = shift_transition(m, 4, false);

        assert_eq!(0, m, "no phantom bit may survive");
        assert!(hide, "the popup must be retracted");
    }
}
