# Popup interaction — sticky hover, scrolling, and no redundant re-render

**Status:** designed, not implemented.
**Date:** 2026-07-28
**Parent:** `2026-07-26-chibipop-design.md`
**Revises:** `2026-07-27-m3-popup-design.md` (M3-D4's truncation marker is replaced)

Three features requested together. They turn the popup from something that only *reacts* to what
is under the cursor into something you can move into and read.

---

## 1. Problem

**The popup runs away when you reach for it.** It is placed `POPUP_GAP` (12px) below the hovered
character. Moving the cursor toward it crosses that gap, chibipop resolves whatever is in the gap,
and the popup usually hides before the cursor arrives. Long entries are therefore unreadable — you
cannot get to them.

**Long entries are truncated, not scrollable.** `render.rs` clamps content to
`max_height_percent` (45%) and draws a dimmed `…`. Whatever was cut is unreachable by any means.

**The popup flickers while the cursor sits still.** Two independent causes:
- Any movement past `MOVEMENT_GATE_PX` (4px) dispatches a fresh trigger, so the capture guard
  hides and re-shows the popup around the capture even when the answer will be identical.
- `resolve` builds text for the whole recognised line, and that line re-clips at either end as the
  capture region slides, so the popup re-renders on an unchanged hovered glyph. This is the effect
  `main.rs`'s `watch` already documents and works around for its log output.

## 2. Goals and acceptance

1. **Sticky hover.** With a popup on screen, moving the cursor from the hovered word into the popup
   keeps that popup unchanged, and it stays unchanged anywhere inside it. Moving fully outside
   resumes normal hovering on the next movement. **Acceptance: reach the popup and read it without
   it changing or disappearing, at ordinary mouse speed, ten times out of ten.**
2. **Scrolling.** An entry longer than the cap is fully readable by wheel, with a visible indicator
   of position and remaining content. **Acceptance: a 大辞林 entry that overflows can be read to its
   last line and back to its first; the wheel still scrolls the app underneath when the cursor is
   not in the popup.**
3. **No redundant re-render.** Holding the cursor on one word produces **one** render, not a
   stream. **Acceptance: hovering a word and jiggling the mouse within it produces no visible
   flicker.**

Non-goals: clicking, selecting or copying from the popup; keyboard scrolling; horizontal scrolling;
resizing; a scrollbar you can drag.

### Build order is a requirement, not a convenience

**Sticky hover (D1–D3) and the dedupe (D10) land before scrolling (D4–D9).** Two reasons, and the
first is the same argument the accuracy-and-polish round used:

- Scrolling is the only one of the three that carries a real risk to the rest of the system (D9,
  the wheel swallow) and the only one with an involved acceptance test. Landing the two cheap
  features first means a scrolling problem cannot gate them.
- **Scrolling does not work without the dedupe.** See D11. Building it first would produce a
  feature that appears broken for reasons living in a different feature.

### Named constants introduced here

| Constant | Value | Where | Note |
|---|---|---|---|
| `ANCHOR_JITTER_PX` | 4 | `app.rs` | Tolerance for D10's anchor comparison |
| `SCROLLBAR_W` | 4 | `ui/theme.rs` | Track and thumb width |
| `SCROLLBAR_MIN_THUMB` | 16 | `ui/theme.rs` | Floor, so a huge entry keeps a grabbable-looking thumb |
| `SCROLL_STEP_PX` | 48 | `app.rs` | Pixels per `WHEEL_DELTA` notch. **A feel constant — starting value, expected to be tuned** (§8) |

---

## 3. Architecture

### D1 — the app remembers what is on screen

`run` currently fires and forgets: `handle_worker_outcome` shows a popup and retains nothing. All
three features need that memory.

```rust
/// What is on screen right now. `None` whenever the popup is hidden.
struct Shown {
    /// The hovered character's own box.
    anchor: PhysRect,
    /// Where `place_popup` actually put the window - stored, never re-derived.
    popup: PhysRect,
    presentation: Presentation,
    /// Vertical content offset in physical pixels; 0 is the top.
    scroll: i32,
    /// Natural content height, unclamped.
    content_h: i32,
    /// Visible height = the window's height = the cap.
    view_h: i32,
}
```

**`Shown` is set only after `Popup::show_at` has succeeded, and cleared on every path that hides
the popup.** See §6 — a `Shown` that outlives its window is the worst defect available here.

### D2 — the sticky region is three rects, not a bounding box

Pure geometry, in `geom.rs`, no `windows` dependency:

```rust
pub fn sticky_region(anchor: PhysRect, popup: PhysRect) -> [PhysRect; 3]
pub fn in_sticky(p: PhysPoint, anchor: PhysRect, popup: PhysRect) -> bool
```

The three rects are the **anchor**, the **popup**, and a **bridge**: the vertical gap between the
two, spanning their combined x-extent.

**A bounding box would be wrong, and this is the reason.** The popup is flush-aligned with the
anchor's left edge and up to `POPUP_MAX_WIDTH` (420px) wide, while the anchor is ~26px wide. A
bounding box would therefore also cover ~400px of screen *beside the hovered character*, at the
character's own height — so scanning along a line of text would freeze the popup on the previous
word. The bridge is only `POPUP_GAP` tall, so brushing it on the way to the next line holds
nothing.

The bridge is computed from the two rects generically, **not** by assuming the popup is below:
`place_popup` flips it above near the monitor's bottom edge, and flips horizontally at the right
edge. Whichever rect is upper, the bridge spans from its bottom to the other's top.

**Degenerate bridge.** When the two rects touch or overlap vertically, the bridge has zero or
negative height. `in_sticky` must treat any rect with `w <= 0 || h <= 0` as containing nothing —
the same rule `geom::inset` already applies — rather than producing a rect whose
`x + w` arithmetic reads as a containment. The anchor and popup are adjacent in that case, so
containment still works without the bridge. A test covers `gap = 0`.

**Because `PhysRect::contains` is inclusive of the top-left edge and exclusive of the
bottom-right, the three rects tile with no seam:** the anchor excludes `y = anchor.y + anchor.h`,
which is exactly the bridge's first row; the bridge excludes `y = popup.y`, which is exactly the
popup's first row. No pixel between the word and the popup is left out. Asserted by test.

### D2a — what "you cannot fall down the crack" actually guarantees

Worked out on paper before implementation, because the obvious stronger claim is **false** and it
would have been written as a test that could not pass.

With anchor `(100, 100, 26, 27)` and the popup at `y = 139`, the straight segment from the
*anchor's centre* to the *popup's centre* leaves the anchor's right edge at about `y = 126` — one
pixel before the bridge begins at `y = 127`. So:

> ❌ **NOT guaranteed:** every straight path from the anchor's centre to the popup's centre stays
> inside the sticky region.

The guarantee is directional, and the reason is a genuine tension rather than an oversight. Covering
the strip *beside* the anchor at the anchor's own height is exactly what would break scanning
sideways along a line of text (see D2). The region that a shallow diagonal needs and the region
side-scanning must not have are the same region. They cannot both be satisfied.

> ✅ **Guaranteed:** the vertical path from the anchor's centre down (or up) into the popup is
> entirely sticky, and any approach **steeper than roughly 45°** stays sticky. For the geometry
> above, staying inside the anchor until the bridge requires `dx < 13` over `dy = 14`.

**A shallower approach exits sideways onto the neighbouring character, and that is correct
behaviour, not a defect** — the cursor genuinely moved onto a different word, and showing that word
is what the user asked for. Once past the anchor's bottom edge the bridge spans the full combined
x-extent, so diagonals are unrestricted from there on.

The line below the hovered word is not a new hazard: the bridge occupies exactly the strip between
the word and the popup, and the popup itself already occludes everything below that strip. Anything
the bridge makes sticky was already hidden behind the popup.

### D3 — sticky suppresses dispatch entirely

While `in_sticky(cursor)`, `run` **does not send a `Trigger` at all**.

Not "resolve but do not hide": resolving under the popup would read whatever the popup is covering
and replace the content with an unrelated word. Skipping dispatch also stops the capture guard's
hide/reshow, which is expected to be the larger of the two flicker causes in §1.

Leaving the region resumes normal behaviour on the next accepted movement. Nothing is remembered
about having been sticky.

---

## 4. Scrolling

### D4 — the window's height never changes; only content moves

The window stays at the `max_height_percent` cap and scrolling changes the content offset inside
it.

This is not a stylistic choice. `geom::place_popup`'s guarantee that the popup never covers the
anchor — the property its 12,201-case sweep establishes — holds only while the height it is handed
does not exceed the cap. `app.rs` already carries a comment saying so at the `measure` call. A
window that grew to fit content would silently void that proof for exactly the long entries this
feature is for.

### D5 — `measure` reports the natural height instead of a bool

`Renderer::measure` returns `(w, h, clamped: bool)` today. It becomes `(w, view_h, content_h)`.

The bool was a lossy summary of precisely the number scrolling needs; `clamped` becomes
`content_h > view_h` at the call site. `Renderer::paint` gains `scroll: i32`.

`CLAMP_MARKER` (`…`) and its drawing are removed — the scrollbar replaces it. M3-D4's truncation
marker is superseded, not merely disabled.

### D6 — the scrollbar

Right edge, `SCROLLBAR_W` (4px) wide, `Theme::dimmed_text`, drawn **only** when
`content_h > view_h`. Thumb height is `view_h / content_h` of the track, floored at
`SCROLLBAR_MIN_THUMB` (16px); thumb offset is `scroll / max_scroll` of the remaining track. It is
an indicator, not a control — dragging it is a non-goal.

```rust
pub fn max_scroll(content_h: i32, view_h: i32) -> i32   // (content_h - view_h).max(0)
pub fn scrollbar_thumb(track_h: i32, content_h: i32, view_h: i32, scroll: i32)
    -> Option<(i32, i32)>                               // (top, height); None when it fits
```

Both pure, in `render.rs`, unit-tested. The thumb has a minimum height so a very long entry does
not produce a 1px sliver.

### D7 — the wheel arrives through the hook, and the main thread owns the decision

The popup is `WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOPMOST | WS_EX_TOOLWINDOW |
WS_EX_TRANSPARENT`. It is click-through and never activates, so it can never receive
`WM_MOUSEWHEEL`. The only route is the low-level mouse hook already installed.

Two statics, and deliberately **no rect in the hook and no lock**: a lock taken inside a
`WH_MOUSE_LL` callback can stall all system input.

```rust
static SCROLL_ARMED:   AtomicBool  // main thread writes, hook reads
static PENDING_SCROLL: AtomicI32   // hook accumulates, main thread drains
```

- **Main thread**, on its existing 50 Hz dispatch tick: read the live cursor with `GetCursorPos`
  and set `SCROLL_ARMED = popup.contains(p) && content_h > view_h`.

  **The popup's own rectangle, NOT `in_sticky`.** Revision 1 of this spec said `in_sticky`, and
  that was a daily-use defect: the sticky region includes the *hovered word*, and the cursor is on
  the hovered word **by construction** whenever a popup appears — that is how it got there. So
  "hover a word in a document, then wheel to scroll the page" would have found the page frozen and
  the popup scrolling instead, whenever the entry happened to overflow. Hover-then-scroll is the
  core loop of reading Japanese, and it would have failed *intermittently* — short entry, page
  scrolls; long entry, page dead — which is worse than failing consistently.

  §2's acceptance criterion already said "the wheel still scrolls the app underneath **when the
  cursor is not in the popup**". `in_sticky` and that sentence name different regions; this spec
  contradicted itself and the acceptance criterion is the one that was right.

  Dispatch suppression (D3) still uses all three rects — it must, or the popup runs away as you
  reach for it. **The two predicates are deliberately different**, and that is the point rather
  than an inconsistency: freezing the *content* while crossing the gap is wanted; capturing the
  *wheel* while merely hovering a word is not.

  **Read the live cursor, not `Hooks::take_pending`.** `take_pending` is gated at
  `MOVEMENT_GATE_PX`, so easing into the popup in small steps could leave the arm stale. From the
  live position the arm is correct within one tick regardless of movement history, and it
  self-corrects from any wrong state rather than latching.
- **Hook**, on `WM_MOUSEWHEEL`: if armed, add the delta to `PENDING_SCROLL` and **return 1** to
  swallow the event. Otherwise fall through to `CallNextHookEx` exactly as now.
- **Main thread**, on tick: drain `PENDING_SCROLL`, convert `WHEEL_DELTA` (120) units to pixels by
  `SCROLL_STEP_PX` per notch, clamp to `0..=max_scroll`, and repaint if it changed.

  **The sub-notch remainder must be carried, not discarded.** Win32 documents that finer-resolution
  wheels send deltas *smaller* than `WHEEL_DELTA` and that the application must accumulate them —
  high-resolution mice and Precision Touchpads do exactly that. A naive `notches / 120` on a tick
  that collected `40` yields zero: the event was **already swallowed**, nothing scrolls, and the
  remainder is thrown away, so the gesture vanishes entirely rather than going to either window.
  The consumption arithmetic and the swallow decision must agree — keep `notches % 120` in the
  accumulator.

  **`PENDING_SCROLL` is discarded whenever `Shown` is replaced.** Otherwise notches accumulated for
  the previous popup land on the new one's fresh `scroll: 0`, and a newly hovered word opens
  mid-definition.

Scrolling **repaints only** — no re-measure, no `show_at`, no capture, no lookup.

### D8 — this breaks a documented invariant on purpose

`hooks.rs` currently states that its "early-return-free structure below is deliberate, not
incidental: there is exactly one `return`-shaped statement in this function". Swallowing requires a
second exit.

The module documentation is **amended** to record that there is now exactly one case which does not
call `CallNextHookEx`, and why — rather than the existing comment quietly becoming false. A
comment that has silently stopped being true is worse than no comment.

### D9 — the risk this feature carries

**A stuck `SCROLL_ARMED` disables the scroll wheel system-wide** until chibipop exits. This is the
most user-hostile failure in the design.

Revision 1 claimed three independent mitigations. **There is one**, and the padding was the harm —
a three-item list reads as defence in depth and stops the reviewer asking the only question that
matters: *what if the recompute stops?*

- **The one real mechanism:** the arm is recomputed from scratch every 20 ms from the live cursor,
  so it cannot latch while the main thread is servicing its own timer.
- Clearing it on every hide path is an **eager version of the same mechanism**, not a second one.
  Worth doing — it closes the 20 ms window — but it is not independent.
- `Hooks::drop` clearing it is **decorative, and marked as such in the code.** Trace it: if `Drop`
  runs, `UnhookWindowsHookEx` in the same function removes the hook and restores the wheel whatever
  the flag says; if `Drop` does not run, the process is dying and the hook dies with it. There is no
  state of the world in which that store changes an outcome. It stays as cheap defensiveness; it is
  **not** evidence of safety.

### The exception to the one mechanism — a nested message pump

**`TrackPopupMenuEx` runs its own message pump on the main thread and discards thread-targeted
messages.** `src/ui/tray.rs`'s `show_menu` already documents this: a thread message "is picked up
and discarded with nowhere to go, not left on the queue for `run`'s own loop to find once this call
returns." `WM_TIMER` from `SetTimer(None, 0, …)` is exactly such a message.

So while the tray menu is open the recompute **stops** — while the hook keeps being called, because
a nested pump is still a message-retrieval point. That is the worst possible combination: pumping
enough to swallow, not enough to disarm. `LowLevelHooksTimeout` cannot rescue it either, since the
callback itself is fast; what is stalled is the decision loop, which Windows cannot see.

**Therefore `SCROLL_ARMED` is cleared in `show_menu`'s `before_blocking` callback**, which exists
for precisely this class of bug (recorded there as I4). Any future nested pump added to the main
thread must do the same, and this paragraph is the reason why.

**A stuck arm must also be self-reporting.** If some other nested pump is ever introduced without
that clear, the symptom is "chibipop broke my mouse wheel" with nothing in the log to correlate.
So an arm held continuously for more than `ARM_WARN_TICKS` logs **once**, naming the flag — bounded
output, and it turns a mystery into a searchable string.

`highlight_match`-style escape hatch: **`[popup] scroll_popup = false` disables the arm and the
swallow, and nothing else.** The scrollbar is still drawn.

That scope is deliberate. The toggle exists to remove the *risky* part — the only part that can
affect input outside chibipop — not to revert the feature. Turning it off must not leave content
silently truncated with no indication, which is what disabling the scrollbar too would do, and
which is worse than today's `…`. So: with it off you can still see that there is more and how much,
you just cannot reach it.

---

## 5. No redundant re-render

### D10 — key on content, with an anchor tolerance

`Presentation`, `Card`, `GlossBlock` and `CollapsedRow` all derive `PartialEq` already.

```rust
fn same_content(prev: &Shown, new: &Presentation, anchor: PhysRect) -> bool
```

True when `prev.presentation == *new` **and** the anchor is within `ANCHOR_JITTER_PX` (4) on both
axes. On true, `handle_worker_outcome` skips `show_presentation` entirely and **keeps
`prev.scroll`**.

**The tolerance is required, not slop.** `UPSCALE` is 2, so every anchor is re-measured through
`PhysRect::scaled_down`'s integer division from a differently-framed capture — ±1px per edge. An
exact-match test would fail spuriously and re-render anyway.

**The anchor must be in the key at all**, because two occurrences of the same word on screen
produce identical presentations and the popup genuinely has to move.

**Content, not `watch`'s `(anchor.x, anchor.y, char)`.** Different question: `watch` suppresses
duplicate log lines; here the question is literally "would the rendered output be identical", and
content-equality states that exactly. It also covers the case `watch`'s own comment describes — the
recognised line gaining or losing characters at either end as the region slides while the hovered
glyph is unchanged, which changes `cursor_byte_offset` but not the hits.

### D11 — the three features depend on each other

Recorded because it is not obvious and it constrains the build order:

- D3 removes dispatch while the cursor is on the word or in the popup — the largest flicker win.
- D10 catches what remains: a dispatch happened and the answer was unchanged.
- **Scrolling is unusable without both.** Any jitter that reset `scroll` would make a long entry
  impossible to read. Preserving scroll across an equal-content re-resolve is what makes D4–D7
  work at all.

So D10 is not a nice-to-have that can be dropped if time runs short; dropping it silently breaks
D4–D7.

---

## 6. Error handling

| Failure | Response |
|---|---|
| `measure` errors | Hide the popup, **clear `Shown`**, clear `SCROLL_ARMED`, log once |
| `show_at` errors | Same. `Shown` is never set from a failed show |
| `WorkerOutcome::Hide` / `Failed` | Hide, clear `Shown`, clear `SCROLL_ARMED` |
| Trigger mode changed from the tray | Clear `Shown` — the old sticky region must not survive a mode change |
| `scroll` beyond range (content shrank) | Clamp to `0..=max_scroll` before painting; never index past |
| `content_h <= view_h` | No scrollbar, arm stays false, wheel passes through untouched |
| Hook fires while `Shown` is `None` | `SCROLL_ARMED` is false, so the wheel passes through |

**The phantom-region defect, stated explicitly because it is the one to design against:** if
`Shown` outlives the visible popup, `in_sticky` keeps returning true for a region with nothing in
it, dispatch stays suppressed, and a patch of the screen silently stops responding to hovering
until something else clears the state. Every row above that says "clear `Shown`" exists for this.

## 7. Testing

**Pure, screen-free, and exhaustive where it matters:**

- `in_sticky` / `sticky_region`: every point of each of the three rects is inside; points outside
  all three are not; the bridge is correct for popup-below **and** popup-above.
- **Seam-free tiling**, per D2a: for every `y` from the anchor's top through the popup's bottom, a
  point at the anchor's centre `x` is sticky. No pixel between the word and the popup is missed.
- **The vertical-path property, by sweep** — the guarantee D2a actually establishes. For a grid of
  anchor positions and popup sizes over a virtual desktop matching this machine's (including the
  portrait secondary and negative coordinates), assert that every point on the **vertical** segment
  from the anchor's centre into the popup is sticky, for popup-below and popup-above alike.
  Modelled on the existing 12,201-case `place_popup` sweep.
- **The documented non-property**, asserted so it cannot silently change: with the D2a geometry, a
  shallow diagonal *does* leave the sticky region, and the test says so explicitly. Pinning it
  keeps a future "improvement" that widens the bridge from silently breaking side-scanning.
- **Side-scanning is not sticky**: a point at the neighbouring character's position — same line,
  one glyph to the right of the anchor — must **not** be sticky. This is the assertion that would
  fail if anyone replaced the three rects with their bounding box.
- `max_scroll`: zero when content fits; exact when it does not; never negative.
- `scrollbar_thumb`: `None` when content fits; thumb inside the track at scroll 0 and at
  `max_scroll`; minimum thumb height honoured; thumb bottom flush with the track bottom at
  `max_scroll`.
- `same_content`: equal presentation + jittered anchor → true; equal presentation + moved anchor →
  false; different presentation + identical anchor → false.
- Config: `scroll_popup` defaults **true** via `#[serde(default = "...")]`, not a bare
  `#[serde(default)]` — same trap `highlight_match` documents, for the same reason.

**Not verifiable by an agent, and therefore assigned to the user.** Every acceptance item in §2 is
a hover behaviour, and mouse *movement* cannot be injected into a global low-level hook from an
agent environment — `SendInput` returns 0, i.e. the call is rejected outright (see
`findings/2026-07-28-accuracy-and-polish-acceptance.md`). Wheel input **can** be injected, via the
Windows-MCP server's own tools.

The implementation round must ship a **numbered manual test script**. It must include, beyond the
obvious items:

- **Hover a long entry, do not move the cursor, and wheel — the page underneath must scroll.**
  This is the item that catches the `in_sticky`-versus-`popup.contains` defect above, and revision
  1's script could not fail on it: it tested "cursor in the popup" and "cursor off the popup", never
  "cursor on the word", which is where a popup always starts.
- **Open the tray menu while a long entry's popup is up, then wheel** — the D9 nested-pump case.
- **Quit chibipop, then wheel** — the one failure that would outlive the app.
- If a high-resolution wheel or a Precision Touchpad is available, scroll slowly enough to send
  sub-`WHEEL_DELTA` deltas and confirm the popup still moves.

These are reported as **user-verified or unverified**, never as passing on the strength of the unit
tests.

## 8. Open risks

**`Shown.popup` can go stale against reality.** It records where `place_popup` put the window. If
anything else moves or resizes that window, the sticky region silently describes the wrong place.
Today nothing does — `show_at` is the only mover — so this is an invariant to preserve, not a bug
to fix.

**`SCROLL_STEP_PX` is unmeasured.** One wheel notch to pixels is a feel constant and will need
tuning against a real overflowing entry, like `REGION_W/H` before it.

**The capture guard's cost is unchanged, only avoided more often.** D3 skips captures while sticky,
which reduces how often the guard runs but does nothing about `ACK_TIMEOUT` proceeding anyway under
load. That risk stands as the overlay spec §8 recorded it.

**Scroll position is not preserved across leaving and re-entering a word.** Leaving the sticky
region clears `Shown`, so returning to the same word starts at the top. Deliberate: keeping
per-word scroll memory needs a cache with an eviction rule, and the value is speculative.

**The arm is stale for up to one tick (20 ms) after the cursor leaves the popup.** In that window a
wheel event aimed at the window underneath is swallowed and applied to the popup instead. Accepted:
a one-frame glitch, and the alternative — deciding in the hook — needs the popup rect inside the
callback, which means either a lock (forbidden there) or a torn multi-atomic read.

**The wheel stays armed while the capture guard has the popup hidden.** `drain_capture_guard` hides
the window without touching `Shown`, correctly, since the hide is paired with a restore. But the arm
keys on `Shown`, not on visibility. So with the capture guard active — `exclude_from_capture = false`,
**or** the OS refusing `WDA_EXCLUDEFROMCAPTURE`, which is not the user's choice — a capture in flight
while the cursor sits inside the popup's rectangle leaves the wheel swallowed for the duration of
that hide, which `resolve_trigger` spans across the OCR step too. The user would see no popup and a
wheel that does nothing. Not fixed by gating the arm on `IsWindowVisible`: that would hand the wheel
to the window underneath mid-read every time a capture ran, jumping the page while the user is
reading the popup. The narrow dead-wheel window is the better of the two.

**Two chibipop processes would install two hooks**, and the armed one swallows for both. There is no
single-instance guard today (no `CreateMutexW` anywhere in `src/`). Pre-existing, but this feature
upgrades it from harmless to wheel-killing — recorded in `docs/BACKLOG.md`.

**Injected wheel input is not filtered.** `LLMHF_INJECTED` is unchecked, so synthetic wheel events
from accessibility tools or automation are swallowed like any other. Deliberate for now: it is also
the only reason an agent can verify this feature at all.
