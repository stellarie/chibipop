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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicU8, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Movement gate, physical px.
const MOVEMENT_GATE_PX: i64 = 4;

/// "No point stored" sentinel.
const NO_POINT: i64 = i64::MIN;

/// Last point the gate accepted.
static LAST_ACCEPTED: AtomicI64 = AtomicI64::new(NO_POINT);

/// The one candidate, if any.
static PENDING: AtomicI64 = AtomicI64::new(NO_POINT);

/// The configured trigger vkcode.
static TRIGGER_VK: AtomicU16 = AtomicU16::new(0x10);


/// Whether the trigger key is held.
static KEY_DOWN: AtomicBool = AtomicBool::new(false);

/// Key went up: drop the popup.
static HIDE_PENDING: AtomicBool = AtomicBool::new(false);

/// Stuck true kills every wheel.
///
/// Reset each main-thread tick.
static SCROLL_ARMED: AtomicBool = AtomicBool::new(false);

/// Delta banked while armed.
static PENDING_SCROLL: AtomicI32 = AtomicI32::new(0);

/// Clicks on the popup area.
static CLICK_ARMED: AtomicBool = AtomicBool::new(false);

/// Screen coords of the click.
static PENDING_CLICK: AtomicI64 = AtomicI64::new(NO_POINT);

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
        TriggerMode::HoldKey | TriggerMode::HoldShift => 1,
    }
}

fn u8_to_mode(v: u8) -> TriggerMode {
    if v == 1 { TriggerMode::HoldKey } else { TriggerMode::Live }
}

/// Whether a move may count now.
fn mode_currently_eligible() -> bool {
    match u8_to_mode(MODE.load(Ordering::SeqCst)) {
        TriggerMode::Live => true,
        _ => KEY_DOWN.load(Ordering::SeqCst),
    }
}

/// Left/right VK for a modifier.
fn modifier_variants(vk: u16) -> Option<(u16, u16)> {
    match vk {
        0x10 => Some((0xA0, 0xA1)),
        0x11 => Some((0xA2, 0xA3)),
        0x12 => Some((0xA4, 0xA5)),
        _ => None,
    }
}

/// Does this event match the key?
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

/// Tracks the configured key.
///
/// From the event, not the state.
unsafe fn record_key_state(wparam: WPARAM, lparam: LPARAM) {
    // SAFETY: keyboard_hook_proc only calls this with code >= 0, the
    // WH_KEYBOARD_LL contract under which `lparam` is a live
    // KBDLLHOOKSTRUCT owned by the OS for the duration of this call.
    let vk = unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode } as u16;
    let target = TRIGGER_VK.load(Ordering::SeqCst);
    if !matches_trigger(vk, target) {
        return;
    }
    let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
    if down {
        KEY_DOWN.store(true, Ordering::SeqCst);
    } else {
        // For modifiers with L/R variants,
        // only hide when both sides up.
        let still_held = modifier_variants(target)
            .is_some_and(|(l, r)| {
                // The hook fires before state.
                let other = if vk == l { r } else if vk == r { l } else { return false };
                // SAFETY: no preconditions.
                (unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(other as i32)
                } as u16 & 0x8000) != 0
            });
        if !still_held {
            KEY_DOWN.store(false, Ordering::SeqCst);
            let hold = u8_to_mode(MODE.load(Ordering::SeqCst)) != TriggerMode::Live;
            if hold {
                LAST_ACCEPTED.store(NO_POINT, Ordering::SeqCst);
                PENDING.store(NO_POINT, Ordering::SeqCst);
                HIDE_PENDING.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Stores the click's screen pt.
unsafe fn record_click(lparam: LPARAM) {
    // SAFETY: mouse_hook_proc only calls this when code >= 0 and
    // wparam == WM_LBUTTONDOWN - the WH_MOUSE_LL contract that
    // guarantees lparam is a valid, aligned pointer to an
    // MSLLHOOKSTRUCT for the duration of this call.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    let p = PhysPoint { x: data.pt.x, y: data.pt.y };
    PENDING_CLICK.store(pack(p), Ordering::SeqCst);
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
            WM_LBUTTONDOWN if CLICK_ARMED.load(Ordering::SeqCst) => {
                let _ = catch_unwind(|| unsafe { record_click(lparam) });
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
        let _ = catch_unwind(|| unsafe { record_key_state(wparam, lparam) });
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

    /// Did the trigger key come up?
    ///
    /// Hold mode gates moves off, so
    /// no move can retract the popup.
    pub fn take_hide() -> bool {
        HIDE_PENDING.swap(false, Ordering::SeqCst)
    }

    /// Sets the trigger vkcode.
    pub fn set_trigger_key(vk: u16) {
        TRIGGER_VK.store(vk, Ordering::SeqCst);
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

    /// Arms/disarms click capture.
    pub fn set_click_armed(armed: bool) {
        CLICK_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Takes the banked click, once.
    pub fn take_click() -> Option<PhysPoint> {
        let v = PENDING_CLICK.swap(NO_POINT, Ordering::SeqCst);
        if v == NO_POINT { None } else { Some(unpack(v)) }
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

    /// Polled fallback for the gate.
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
}

impl Drop for Hooks {
    /// Unhooks both, best effort.
    fn drop(&mut self) {
        // Redundant; unhook restores.
        SCROLL_ARMED.store(false, Ordering::SeqCst);
        CLICK_ARMED.store(false, Ordering::SeqCst);
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
    fn matches_trigger_exact_vk() {
        assert!(matches_trigger(0x10, 0x10));
        assert!(matches_trigger(0x70, 0x70));
    }

    #[test]
    fn matches_trigger_shift_variants() {
        // VK_LSHIFT and VK_RSHIFT.
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
}
