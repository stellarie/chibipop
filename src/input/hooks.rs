//! System-wide low-level mouse and keyboard hooks: the input layer that
//! decides *when* a hover becomes a candidate for a popup lookup.
//!
//! A `WH_MOUSE_LL`/`WH_KEYBOARD_LL` pair sees **every** mouse and keyboard
//! event on the machine, not just chibipop's own window - see
//! `docs/superpowers/findings/2026-07-27-m3-win32-d2d-spike.md` §6 for the
//! verified mechanics this module is built on. Three consequences follow,
//! non-negotiably:
//!
//! - **`CallNextHookEx` runs on every path out of both callbacks except one**,
//!   error paths included. A low-level hook that swallows an event makes the
//!   whole machine unusable - no clicks, no typing, in any application -
//!   until this process dies.
//!
//!   The one exception is deliberate: a **wheel** event is consumed while
//!   [`SCROLL_ARMED`], because otherwise the window under the popup would
//!   scroll at the same time as the popup itself. That is the single most
//!   dangerous line in this module - read `SCROLL_ARMED`'s own documentation
//!   for what keeps it from latching, and for why any future nested message
//!   pump on the main thread has to disarm it. Nothing else is ever swallowed:
//!   not clicks, not keys, not horizontal wheel.
//! - **`catch_unwind` wraps the working half of both callbacks.** A Rust
//!   panic unwinding across this `extern "system"` boundary is undefined
//!   behaviour, and it would unwind into whatever application the user
//!   happens to be using at that instant, not just this process.
//! - **No keystroke is ever recorded.** The keyboard hook only ever asks
//!   "is Shift down right now" via `GetAsyncKeyState` - never which key
//!   fired the event, never a char, never anything written to a file or
//!   stdout.
//!
//! State reaches the callbacks only through `static` atomics - `HOOKPROC`
//! is a bare `extern "system" fn` and cannot capture an environment (spike
//! verified fact 4); there is no other channel.
//!
//! The 4-pixel movement gate and the `Live`/`HoldShift` mode gate both live
//! here, not in a consumer poll loop - the hook fires on every raw mouse
//! event, far more often than a poll, so re-resolving on a one-pixel tremor
//! (or on every move while Shift is up in `HoldShift` mode) would be far
//! worse than the cost this gate replaces.

use crate::config::TriggerMode;
use crate::geom::PhysPoint;
use anyhow::{Context, Result};
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Movement gate, in physical pixels. A move must exceed this on at least
/// one axis to become a new candidate - matches the tolerance M2's poll
/// loop used (`main.rs`'s `Watch` command: `.abs() <= 4 && .abs() <= 4` =>
/// skip), so this hook-driven gate keeps the same felt behaviour at a much
/// higher event rate.
const MOVEMENT_GATE_PX: i64 = 4;

// ---- global state the callbacks reach (HOOKPROC cannot capture) ----

/// Sentinel for "no point stored". Requires `x == i32::MIN` and `y == 0` (see
/// `pack`), a coordinate no real virtual-desktop position can reach -
/// physical cursor coordinates are bounded by `SM_XVIRTUALSCREEN` /
/// `SM_CXVIRTUALSCREEN`, always many orders of magnitude smaller than
/// `i32::MIN`. Reserving this one value keeps `PENDING` a single atomic
/// word instead of a word-plus-flag pair - see `take_pending` for why that
/// matters.
const NO_POINT: i64 = i64::MIN;

/// The last point the movement gate accepted, regardless of whether it has
/// been consumed yet. Only ever touched by the mouse hook callback - always
/// the same thread, the one that installed the hook and pumps its messages
/// - so a plain `SeqCst` load/store on a single atomic is already
/// race-free without needing any cross-thread argument.
static LAST_ACCEPTED: AtomicI64 = AtomicI64::new(NO_POINT);

/// The one outstanding candidate point, if any. Written by the mouse hook,
/// read-and-cleared by `take_pending` - see that function's doc comment for
/// why packing `x`/`y` into one `AtomicI64` (rather than two separate
/// atomics plus a flag) is what makes this coherent under a concurrent
/// reader.
static PENDING: AtomicI64 = AtomicI64::new(NO_POINT);

/// Whether Shift is currently held, per the keyboard hook's last
/// observation of `GetAsyncKeyState`.
static SHIFT_DOWN: AtomicBool = AtomicBool::new(false);

/// Whether the popup currently wants wheel events. Written by the main thread
/// from the live cursor position on every dispatch tick; read by the mouse
/// hook, which **swallows** the event when this is true.
///
/// A stuck `true` disables the scroll wheel for every application until
/// chibipop exits. There is exactly **one** mechanism preventing that - the
/// main thread recomputing it from scratch every tick - so anything that stops
/// the main thread servicing its timer while still pumping messages must clear
/// this explicitly. `ui::tray::show_menu`'s `before_blocking` is the known
/// case: `TrackPopupMenuEx` runs its own pump and discards thread-targeted
/// messages, so `WM_TIMER` never arrives while that menu is open even though
/// the hook keeps being called.
static SCROLL_ARMED: AtomicBool = AtomicBool::new(false);

/// Wheel delta accumulated while armed, in raw `WHEEL_DELTA` units. Drained by
/// [`Hooks::take_whole_notches`].
static PENDING_SCROLL: AtomicI32 = AtomicI32::new(0);

/// One wheel notch, per `WHEEL_DELTA` in winuser.h. Named here rather than
/// imported because it is arithmetic on our own accumulator, not a Win32 call.
const WHEEL_DELTA_UNITS: i32 = 120;

/// Current trigger mode, with `TriggerMode` packed into a `u8` because
/// `TriggerMode` has no atomic form of its own. 0 = `Live` (matches
/// `Config::default()`), 1 = `HoldShift`.
static MODE: AtomicU8 = AtomicU8::new(0);

/// Packs a point into one 64-bit word so it can be stored and loaded
/// atomically without ever exposing a torn read (one event's `x` spliced
/// with a different event's `y`) to a concurrent caller.
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

/// Whether the current mode/Shift combination allows a move to become a
/// candidate at all - checked *before* the movement gate, so `HoldShift`
/// with Shift up never even touches `LAST_ACCEPTED`.
fn mode_currently_eligible() -> bool {
    match u8_to_mode(MODE.load(Ordering::SeqCst)) {
        TriggerMode::Live => true,
        TriggerMode::HoldShift => SHIFT_DOWN.load(Ordering::SeqCst),
    }
}

/// The actual per-event work for the mouse hook, split out so it can be run
/// inside `catch_unwind` from `mouse_hook_proc`. Reads the event, applies
/// the mode gate then the movement gate, and updates the two statics -
/// nothing here allocates, blocks, or touches I/O.
unsafe fn record_mouse_move(lparam: LPARAM) {
    // SAFETY: mouse_hook_proc only calls this when code >= 0 and
    // wparam == WM_MOUSEMOVE - the WH_MOUSE_LL contract that guarantees
    // lparam is a valid, aligned pointer to an MSLLHOOKSTRUCT for the
    // duration of this call.
    let data = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    let p = PhysPoint { x: data.pt.x, y: data.pt.y };

    if !mode_currently_eligible() {
        return;
    }

    let last = LAST_ACCEPTED.load(Ordering::SeqCst);
    let gate_open = last == NO_POINT || {
        let lp = unpack(last);
        // i64 throughout: the difference of two i32s always fits, so this
        // can never overflow the way a raw i32 subtraction theoretically
        // could at the extreme ends of the coordinate range - a panic here
        // would be caught by mouse_hook_proc's catch_unwind regardless, but
        // there is no reason to depend on that safety net for arithmetic
        // this easy to make unconditionally safe.
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

/// The actual per-event work for the keyboard hook. Deliberately reads
/// nothing from the event itself - not which key, not up/down - only the
/// live Shift state via `GetAsyncKeyState`, exactly as the spike verified
/// ("legal/cheap to call from an LL hook"). That is what "never record
/// which keys were pressed" means in code, not just in the module doc
/// comment above.
unsafe fn record_shift_state() {
    let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
    SHIFT_DOWN.store(shift, Ordering::SeqCst);
}

/// Accumulate one wheel event's delta.
///
/// `mouseData`'s high word carries a signed multiple of `WHEEL_DELTA`; the low
/// word is unused for wheel events. `saturating_add` because this accumulates
/// without bound between drains if the main thread ever stalls.
unsafe fn record_wheel(lparam: LPARAM) {
    // SAFETY: `mouse_hook_proc` only calls this when code >= 0 and
    // wparam == WM_MOUSEWHEEL - the WH_MOUSE_LL contract that guarantees
    // lparam is a valid, aligned pointer to an MSLLHOOKSTRUCT for the
    // duration of this call, exactly as for `record_mouse_move`.
    let data = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    accumulate_wheel((data.mouseData >> 16) as i16 as i32);
}

/// The arithmetic half of [`record_wheel`], split out so the notch accounting
/// is testable without fabricating an `MSLLHOOKSTRUCT`.
fn accumulate_wheel(delta: i32) {
    let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
        Some(v.saturating_add(delta))
    });
}

/// `WH_MOUSE_LL` callback.
///
/// **Exactly one path out of this function does not call `CallNextHookEx`: a
/// wheel event consumed while [`SCROLL_ARMED`].** That consumption is the
/// point - without it the window underneath would scroll at the same time as
/// the popup. Every other path, including both panic paths, still chains.
///
/// This function previously had no early return at all and said so here. The
/// exception is stated rather than left quietly false.
unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        match wparam.0 as u32 {
            // A panic must never unwind across this callback boundary - see
            // the module docs. Swallowing it here leaves the atomics exactly
            // as they were before this call (see record_mouse_move's own
            // comment on why nothing between its stores can plausibly
            // panic), so there is no torn state introduced by the panic path
            // either.
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
    CallNextHookEx(None, code, wparam, lparam)
}

/// `WH_KEYBOARD_LL` callback. Same structure and the same guarantee as
/// `mouse_hook_proc`: one path out, `CallNextHookEx` always runs.
unsafe extern "system" fn keyboard_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let _ = catch_unwind(|| unsafe { record_shift_state() });
    }
    CallNextHookEx(None, code, wparam, lparam)
}

/// Owns the two installed low-level hooks. `Drop` unhooks both - see
/// `install` for why a partial-install failure can never leak the other
/// one.
pub struct Hooks {
    mouse: HHOOK,
    keyboard: HHOOK,
}

impl Hooks {
    /// Installs both hooks. Per spec §6 this is a **hard failure at
    /// startup** for the caller - without hooks there is no product - so
    /// the error names exactly which hook failed rather than a generic
    /// message.
    ///
    /// If the mouse hook installs but the keyboard hook does not, the mouse
    /// hook is unhooked before returning `Err`: `Hooks` is only ever
    /// constructed fully installed, so there is no partial state for `Drop`
    /// to reason about and no window where a lone hook is left running with
    /// no owner to unhook it later.
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

    /// Arms or disarms wheel capture - see [`SCROLL_ARMED`], including why
    /// every nested message pump on the main thread must disarm.
    pub fn set_scroll_armed(armed: bool) {
        SCROLL_ARMED.store(armed, Ordering::SeqCst);
    }

    /// Whether wheel capture is currently armed.
    pub fn scroll_armed() -> bool {
        SCROLL_ARMED.load(Ordering::SeqCst)
    }

    /// Takes the **whole** wheel notches accumulated so far, leaving any
    /// sub-notch remainder banked for the next call.
    ///
    /// Banking the remainder is required, not tidy. Finer-resolution wheels and
    /// Precision Touchpads send deltas *smaller* than `WHEEL_DELTA`, and Win32
    /// requires the application to accumulate them. Draining the accumulator
    /// whole and then dividing would throw those away every tick, so such a
    /// device could **never** reach one notch however long the user scrolled -
    /// and because the hook has already swallowed the event, the gesture would
    /// move neither the popup nor the window underneath. A plain mechanical
    /// wheel only ever sends exact multiples, so the bug would be invisible on
    /// one and total on the other.
    ///
    /// Positive is a scroll up, away from the user - Win32's own convention.
    /// Rust's `%` keeps the sign of the dividend, so downward scrolling banks a
    /// negative remainder and accumulates the same way.
    pub fn take_whole_notches() -> i32 {
        let mut whole = 0;
        // fetch_update may call this closure more than once under contention;
        // only the iteration whose compare-exchange succeeds is the one that
        // stores, and it is the same iteration that set `whole`.
        let _ = PENDING_SCROLL.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
            let remainder = v % WHEEL_DELTA_UNITS;
            whole = (v - remainder) / WHEEL_DELTA_UNITS;
            Some(remainder)
        });
        whole
    }

    /// Drops everything accumulated so far, remainder included. Used when the
    /// popup is replaced, so notches meant for the previous one cannot scroll
    /// the new one to a position its user never asked for.
    pub fn discard_scroll() {
        PENDING_SCROLL.store(0, Ordering::SeqCst);
    }

    /// Changes the trigger mode the mouse hook gates on. Takes effect on
    /// the very next mouse event - there is no separate "apply" step, since
    /// the mode lives in the same kind of static the hook already reads
    /// every time.
    pub fn set_mode(m: TriggerMode) {
        MODE.store(mode_to_u8(m), Ordering::SeqCst);
    }

    /// Consumes and returns the one outstanding candidate point, if any.
    ///
    /// "Consumes" means a second call with no new accepted move in between
    /// returns `None`: this is a single atomic `swap` back to `NO_POINT`,
    /// not a load, so the same movement can never be handed out twice.
    ///
    /// Packing `x`/`y` into one `AtomicI64` (rather than two separate
    /// atomics plus a flag) is what makes this fully race-free even if a
    /// caller on another thread raced the hook. `PENDING` has exactly two
    /// access sites - the hook's plain `store` and this `swap` - and a
    /// hardware read-modify-write like `swap` is indivisible with respect
    /// to every other operation on the same atomic: the hook's store is
    /// therefore always either fully-before or fully-after this swap in
    /// some real total order, never "during" it. That rules out both
    /// failure modes a naive design risks: tearing (this swap can only ever
    /// observe a complete value some single store actually wrote, never one
    /// event's `x` spliced with a different event's `y`) and duplication
    /// (there is no separate flag-then-value read step for a fresh point to
    /// land inside of, so a consumed point can never be handed out a second
    /// time). In the actual deployment (Task 7) this is only ever called
    /// from the same thread that pumps the hook's messages, so the two
    /// never truly run concurrently at all - the analysis above is
    /// deliberately the more conservative, genuinely-concurrent case.
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
    /// Unhooks both. Errors are deliberately swallowed - by the time `Drop`
    /// runs there is nothing left to hand an `Err` to and nothing useful to
    /// do with one; the alternative (panicking in a destructor, or making
    /// teardown fallible) is worse than a best-effort unhook.
    fn drop(&mut self) {
        // Redundant, kept only as cheap defensiveness: the unhook below
        // restores the wheel on its own, and any path where this `Drop` does
        // not run is a path where the process is dying and taking the hook
        // with it. NOT evidence that a stuck arm is recoverable - see
        // `SCROLL_ARMED` for the one mechanism that is.
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

    /// `PENDING_SCROLL` is process-global, so these run as one test rather
    /// than racing each other for it.
    #[test]
    fn wheel_notches_bank_their_sub_notch_remainder() {
        Hooks::discard_scroll();

        // A plain mechanical wheel: exact multiples, nothing banked.
        accumulate_wheel(240);
        assert_eq!(2, Hooks::take_whole_notches());
        assert_eq!(0, Hooks::take_whole_notches(), "nothing should be left over");

        // A high-resolution wheel: sub-notch deltas must accumulate rather
        // than being discarded. Draining and dividing would leave this at
        // zero forever and the gesture would never move anything - the hook
        // has already swallowed the event by then.
        accumulate_wheel(40);
        assert_eq!(0, Hooks::take_whole_notches(), "40 is not yet a notch");
        accumulate_wheel(40);
        assert_eq!(0, Hooks::take_whole_notches(), "80 is not yet a notch");
        accumulate_wheel(40);
        assert_eq!(1, Hooks::take_whole_notches(), "40+40+40 is one whole notch");
        assert_eq!(0, Hooks::take_whole_notches());

        // Downward: Rust's % keeps the dividend's sign, so the remainder
        // banks negative and accumulates the same way.
        accumulate_wheel(-140);
        assert_eq!(-1, Hooks::take_whole_notches());
        accumulate_wheel(-100);
        assert_eq!(-1, Hooks::take_whole_notches(), "-20 banked plus -100");
        assert_eq!(0, Hooks::take_whole_notches());

        // Replacing the popup drops the remainder too, so a new entry cannot
        // open already scrolled.
        accumulate_wheel(80);
        Hooks::discard_scroll();
        assert_eq!(0, Hooks::take_whole_notches());
    }

    /// The single riskiest line in this crate: an armed wheel event must be
    /// consumed (`LRESULT(1)`) and its delta banked.
    ///
    /// Only the **armed** path is exercised here, because that path returns
    /// before `CallNextHookEx` and so touches no hook chain. The unarmed path
    /// is verified live instead - chibipop running, wheel over a browser, page
    /// still scrolls - see
    /// `docs/superpowers/findings/2026-07-28-popup-interaction-acceptance.md`.
    #[test]
    fn an_armed_wheel_event_is_swallowed_and_banked() {
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

    /// A long stall pins the accumulator instead of wrapping; the notch count
    /// derived from it must still be sane, and the caller's pixel conversion
    /// saturates rather than overflowing.
    #[test]
    fn a_saturated_accumulator_yields_a_bounded_notch_count() {
        Hooks::discard_scroll();
        accumulate_wheel(i32::MAX);
        accumulate_wheel(i32::MAX);
        let notches = Hooks::take_whole_notches();
        assert_eq!(i32::MAX / WHEEL_DELTA_UNITS, notches);
        assert!(notches.saturating_mul(4096) > 0, "must not wrap negative");
        Hooks::discard_scroll();
    }
}
