# Popup interaction — acceptance

**Task:** popup-interaction plan, Task 6 (spec §2). **Measures; does not tune.**
**Date:** 2026-07-28
**Spec:** `2026-07-28-popup-interaction-design.md` at `427919e` (including the D7/D9 corrections)
**Build:** release at `84680e6`+. 216 Rust tests, clippy at the 5 accepted pre-existing.

---

## 1. What was verified, and how

### ✅ The wheel still works when chibipop is running (live)

The failure this guards against is the worst one available: chibipop swallowing wheel events for
every application on the machine. Verified against real injected input, not reasoning.

- `chibipop run` started against the portrait secondary, hook installed.
- Pointer parked at (3100, 300) over the ttsu-reader.
- `mcp__Windows-MCP__Scroll` — which produces **genuine** injected input, unlike `SendInput` from a
  tool shell (see §3).
- Full-resolution before/after captures compared on a sampled grid: **854 of 3741 sampled pixels
  differed (22.8%)** — the page scrolled.

**PASS.** With the hook live and no popup on screen, `SCROLL_ARMED` is false, `mouse_hook_proc`
takes the `CallNextHookEx` path, and the wheel reaches the application underneath untouched.

### ✅ An armed wheel event is swallowed and banked (unit)

The armed path returns before `CallNextHookEx`, so it can be driven directly with a fabricated
`MSLLHOOKSTRUCT` without touching any hook chain —
`hooks::tests::an_armed_wheel_event_is_swallowed_and_banked`.

**Proved load-bearing.** Deleting `return LRESULT(1)` so the callback falls through makes it fail
with `left: 1, right: 0` — `CallNextHookEx` returned 0. Restored and re-verified.

Between this and the live test above, **both branches of the swallow decision are covered**: armed
by unit test, unarmed by real injected input.

### ✅ Three rects, not a bounding box (unit, falsified)

`geom::tests::the_next_character_along_the_line_is_not_sticky`. Substituting the bounding box for
the bridge makes it fail at "one glyph right of the anchor", plus two more. Restored and
re-verified. This is the assertion that keeps scanning sideways along a line working.

### ✅ The sub-notch remainder is banked (unit)

`hooks::tests::wheel_notches_bank_their_sub_notch_remainder`, covering exact multiples, three
sub-notch deltas summing to one notch, and the negative direction. Without this a high-resolution
wheel or Precision Touchpad could **never** reach a notch — and since the hook has already
swallowed the event, the gesture would move neither the popup nor the window beneath.

### ✅ The pure geometry and scroll arithmetic (unit)

- The vertical-path sweep across a grid of anchor positions and popup sizes, including negative
  coordinates and the portrait secondary's x-range.
- Seam-free tiling: every row from the anchor's top to the popup's bottom is sticky.
- The documented **non**-property is pinned: a shallow diagonal *does* leave the region (spec D2a),
  so a future widening of the bridge cannot silently break side-scanning.
- `max_scroll` / `scrollbar_thumb`: proportional thumb, floor honoured, flush with the track at
  full scroll, and a track shorter than the floor still yields a thumb that fits.
- `same_content`: jittered anchor is the same, moved anchor is not, different card is not, and the
  tolerance is inclusive at 4 and exclusive at 5.
- `scroll_popup` defaults on, round-trips, and a `[popup]` section written before the field existed
  loads with it **on**.

---

## 2. ⚠️ NOT verified — assigned to the user

**Everything that needs a hover.** Mouse *movement* cannot be injected into a global low-level hook
from an agent environment, so no popup can be put on screen, and every behaviour that follows from
one is unverified by anyone yet:

- the popup holding still while the cursor moves into it;
- normal hovering resuming on exit;
- absence of flicker on a held hover;
- the scrollbar appearing on an overflowing entry;
- **the armed wheel actually scrolling that popup** — the mechanism is unit-tested, the end-to-end
  behaviour is not;
- `SCROLL_STEP_PX = 48` feeling right. It is a guess, and it is the number most likely to need
  changing.

Work through §4. Until then this round is **implemented and unit-verified, not accepted.**

---

## 3. The verification boundary, measured

Re-investigated this session now that Windows-MCP is available. The findings are sharper than the
previous round's:

| Route | Result |
|---|---|
| `SendInput` from any tool-spawned shell | **Rejected — returns 0**, zero events accepted |
| `SendInput` from `mcp__Windows-MCP__PowerShell` | **Also returns 0** — its child shell is restricted too |
| `SetCursorPos` from a tool shell | Succeeds and **moves the pointer**, but generates no hook event |
| `mcp__Windows-MCP__Scroll` (native, in-server) | ✅ **Real injected input** — the browser scrolled |
| `mcp__Windows-MCP__Move` / `Click` | ❌ unusable here — `loc` has an untyped schema and this harness serialises it as a string, so `[x, y]` arrives as `'[x, y]'` and fails validation |

**The tell for anyone re-investigating: print `SendInput`'s return value first.** A 0 settles it in
seconds. `SetCursorPos` is the trap — the pointer visibly obeys while nothing else happens.

Untested idea for a future round: `Move(label: "…")` may work, since `label` is a string and would
survive the stringification that breaks `loc`.

---

## 4. Manual test script — for oniichan

Nine items, a couple of minutes. Items 4, 7 and 8 are the ones that catch the defects an
independent review found in the design, so please do not skip them.

1. **Reach the popup.** Hover a word with a short definition, then move the cursor down into the
   popup at ordinary speed.
   **Pass:** it does not change or vanish, and you can move around inside it freely.
2. **Leave it.** From inside the popup, move fully off it onto other text.
   **Pass:** normal hovering resumes immediately, no dead patch of screen.
3. **No flicker.** Hover a word and jiggle the mouse slightly on it.
   **Pass:** nothing redraws.
4. **Scan sideways.** Hover a word, then move sideways to the next character on the same line.
   **Pass:** the next character resolves — the popup does **not** stay stuck on the previous word.
   *(This fails if the sticky region is ever widened to a bounding box.)*
5. **Find an overflowing entry** — a 大辞林 entry for a common word.
   **Pass:** a thin scrollbar appears at the right edge of the popup.
6. **Scroll it.** With the cursor inside that popup, wheel down to the last line and back to the
   first.
   **Pass:** content moves, and the thumb travels and ends flush with the bottom.
7. **Hover, do not move, wheel.** Keep the cursor **on the word** — not on the popup — with an
   overflowing entry showing, and wheel.
   **Pass:** the page underneath scrolls. The popup must **not** scroll.
   *(This is the D7 defect: the first draft armed on the whole sticky region, which includes the
   word, so this would have frozen the page every time you hovered while reading.)*
8. **Tray menu while a popup is up.** With an overflowing entry showing and the cursor inside the
   popup, right-click the tray icon, leave the menu open a few seconds, and wheel.
   **Pass:** the wheel works normally. Close the menu.
   *(This is the D9 defect: `TrackPopupMenuEx` pumps its own loop and discards `WM_TIMER`, so the
   arm cannot be recomputed while the menu is open.)*
9. **Quit, then wheel.** Quit chibipop from the tray, then use the wheel anywhere.
   **Pass:** it works. *(The one failure that would outlive the app.)*

Optional, if you have a high-resolution wheel or use the touchpad: scroll a popup **slowly**, in
sub-notch increments. **Pass:** it still moves.

Also worth a glance: `scroll_popup = false` in the config, restart, hover the same long entry.
**Pass:** the scrollbar still draws, and the wheel scrolls the window underneath instead.

If item 8 fails, `scroll_popup = false` is the immediate escape hatch and the wheel returns at once.
