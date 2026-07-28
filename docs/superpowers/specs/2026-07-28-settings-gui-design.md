# Settings GUI — a window for the config, and an executable icon on the way

**Status:** designed, not implemented.
**Date:** 2026-07-28
**Parent:** `2026-07-26-chibipop-design.md`
**Revises:** `2026-07-27-m3-popup-design.md` (M3-D3's tray menu), and the M3 statement that
"configuration is a hand-edited TOML file" — now *also* a hand-edited TOML file.

---

## 1. Problem

Every one of chibipop's **eleven** settings is reachable only by finding `chibipop.toml`, editing
it, and restarting. That was a defensible decision when there were four; it is a poor one now, and
the two most-wanted changes — the theme and the font — are the ones a user is least likely to go
hunting in a file for.

The tray menu offers Live / Hold-Shift / Quit, so the trigger mode is the single setting with a UI,
which is an accident of it being the first one that needed switching at runtime rather than a
judgement that it is the most important.

Specifically requested: the match highlight must be toggleable from the UI, because opinion on it
is genuinely split.

## 2. Goals and acceptance

1. **Right-clicking the tray shows exactly `Settings…` and `Quit`.** The Live / Hold-Shift radio
   items are **removed** from the menu — trigger mode becomes a setting in the window like every
   other one, rather than the only setting with a UI because it happened to be first. Acceptance:
   two items, both work, and mode is still changeable (from the window).
2. **Settings opens a window exposing all eleven settings**, with the two hazardous ones under a
   Debug heading. Acceptance: every setting in `config.rs` is reachable, and the values shown match
   the TOML on disk.
3. **Apply persists and restarts.** Acceptance: change the theme, Apply, and the popup is drawn in
   the new theme without touching a file or a terminal.
4. **Dictionaries can be reordered.** Acceptance: move 大辞林 above Jitendex, Apply, hover a word
   that both define, and 大辞林 is on top.
5. **chibipop keeps working while the window is open.** Acceptance: hover text with Settings up;
   the popup still appears, and the wheel still scrolls other windows.

Non-goals: adding or removing dictionaries, any upload or file-picking mechanism, editing the
deconjugation rules, a per-dictionary enable toggle, and free-text entry of any kind (§4 D3).

---

## 3. The window

### D1 — native Win32 controls, plus the manifest that makes them look current

Predefined control classes only — `BUTTON` (checkbox, radio, pushbutton), `COMBOBOX`, `LISTBOX`,
and `msctls_updown32` paired with a read-only `EDIT` for numbers. No new crate.

**This requires an application manifest**, and that is not cosmetic detail: with no manifest the
process gets comctl32 v5 and every control renders in the **Windows 95 grey** style. The repository
has no `build.rs`, no `.rc` and no manifest today (verified).

The route is already proven: `docs/BACKLOG.md` item 4 records `rc.exe` + a `build.rs` that only
emits `cargo:rustc-link-arg`, **POC'd end to end** for the executable icon and deferred solely
because nothing needed it yet. Something does now. **This round therefore also ships the executable
icon**, closing that backlog item, because it is the same three lines of build script.

⚠️ `rc.exe` fails under git-bash — MSYS2 mangles its `/flag` arguments. Run it from PowerShell.

**Rejected: drawing the settings UI in Direct2D** to match the popup's theme. The renderer exists
and the result would look like ours rather than like Windows, but it means hand-rolling focus, tab
order, keyboard access, hit-testing and control states. This is the one screen where matching the
operating system beats matching ourselves.

### D2 — modeless, on the existing message loop

`DialogBoxParamW` and friends run **their own message pump**, and this codebase has a fresh,
verified reason to refuse that: `TrackPopupMenuEx`'s nested pump discards thread-targeted messages,
so `WM_TIMER` stops arriving, which latches `SCROLL_ARMED` and kills the scroll wheel system-wide
(popup-interaction spec D9). A tray menu holds that state for a second or two. **A settings window
would hold it for minutes.**

So: an ordinary top-level window created with `CreateWindowExW`, owned by `src/ui/settings.rs`,
serviced by `run`'s existing loop. The loop gains one branch:

```rust
if let Some(hwnd) = settings_hwnd {
    if unsafe { IsDialogMessageW(hwnd, &msg) }.as_bool() {
        continue;
    }
}
```

`IsDialogMessageW` is what gives Tab, Shift-Tab, arrow keys between radios, and Enter/Escape
without a modal pump.

Unlike the popup this window is **not** `WS_EX_TOPMOST`, **not** `WS_EX_TRANSPARENT`, **not**
`WS_EX_NOACTIVATE`. It takes focus like any window, because it is one.

Only one may exist at a time; `Settings…` on an already-open window calls `SetForegroundWindow`
rather than creating a second.

### D3 — no free-text entry anywhere

A deliberate constraint, and it is what keeps this small: every control is a choice or a bounded
number, so there is nothing to validate, no error state, and no way to type something that fails to
load later.

| Setting | Control | Range / source |
|---|---|---|
| `trigger.mode` | two radios | Live / Hold Shift |
| `popup.theme` | combo | `dark`, `light` |
| `popup.font` | combo | installed families, `SHIFTJIS_CHARSET` (D4) |
| `popup.max_height_percent` | spin | 10–90 |
| `popup.summary_chars` | spin | 10–200 |
| `popup.highlight_match` | checkbox | — |
| `popup.scroll_popup` | checkbox | — |
| `popup.exclude_from_capture` | checkbox | — |
| `dictionaries.display_order` | listbox + Move up / Move down | live dictionary list (D6) |
| `ocr.max_ocr_passes` | spin, Debug | 1–5 |
| `debug.show_scan_region` | checkbox, Debug | — |

**The Debug group is a plain `BUTTON` with `BS_GROUPBOX`, always visible — not collapsible.** A
collapsible section means custom hit-testing, a rotating glyph and re-laying-out every control
below it on toggle, which is exactly the hand-rolled UI work D1 rejected. It is a labelled box.

**The dictionary listbox keeps its selection across a move**, following the item rather than the
index, so Move up can be pressed repeatedly without re-selecting. A listbox that dropped selection
after each press would make reordering three dictionaries a nine-click job.

**Move up is disabled on the first item and Move down on the last**, rather than silently doing
nothing.

### D4 — the font list is Japanese-capable fonts only

`EnumFontFamiliesExW` with `lfCharSet: SHIFTJIS_CHARSET`, so the dropdown cannot offer a font that
would render the popup's text as boxes.

**If the configured font is not installed it is inserted into the list anyway and selected.**
Otherwise opening Settings and pressing Apply without touching anything would silently change a
setting — the one behaviour a settings window must never have.

### D5 — labels are prose, and the Debug section states its own hazard

Controls read "Box the word being defined", not `highlight_match`. The TOML remains the reference
documentation; this window is for recognising a setting at a glance.

`max_ocr_passes` carries its warning **at the control**, not only in the README:

> 1 = no tiling. Higher reads further ahead but can resolve the wrong character.

Tiling measured **2/6** on the character under the cursor
(`findings/2026-07-28-vertical-text-measurement.md`, and the accuracy round before it). Putting a
spinner for it one click from the tray while hiding that fact in a README would be dishonest.

---

## 4. Applying

### D6 — the dictionary write-back must preserve substrings

**The trap this decision exists for is silent and delayed.**

`DictionariesConfig::display_order` holds **case-insensitive substrings**, not names, and
`present.rs` documents exactly why: Jitendex's stored name embeds a release date —
`Jitendex.org [2026-07-09]` — that changes whenever the dictionary is rebuilt. An exact-name config
"would silently stop applying the moment the file is refreshed, and the popup would quietly start
showing English first with no error."

A settings window that lists real names and writes real names back would reintroduce that bug for
every user who touches it, and it would work perfectly until the next dictionary update.

**The rule:**

1. For each dictionary in the new order, if an entry in the *existing* `display_order` matches it
   (the same case-insensitive substring test `present::dict_order_rank` uses), write **that entry
   back verbatim**.
2. Only for a dictionary with no existing entry, derive a key: the name truncated at the first `[`
   or `(`, then trimmed. `Jitendex.org [2026-07-09]` → `Jitendex.org`; `大辞林　第四版` → unchanged.

**Consequence, asserted by test:** reordering an existing config and applying it produces a
`display_order` containing exactly the same strings, in a new order — never rewritten values.

### D6a — say out loud that a rebuild can break the order

D6 makes the order *survive* the ordinary case — a Jitendex refresh that only moves the date. It
cannot survive every case, and the failure is silent from inside the window, so the window says so
itself rather than leaving it to a README nobody opens while reordering.

**Two tiers, because one of them is detectable and the other is not.**

**Always — a quiet hint under the list:**

> Order is matched by dictionary name. If you rebuild your dictionaries, check this list again.

**When an entry matches nothing — a visible warning naming it.** Every `display_order` entry is
tested against the live dictionary list with the same case-insensitive substring rule. An entry
matching **no** current dictionary is exactly the symptom of a rebuild having changed a name, and
it is worth reporting precisely rather than generically:

> `"Jitendex"` no longer matches any dictionary — it may have been renamed or removed. Dictionaries
> it used to order are now sorted last.

That last clause matters: `present::dict_order_rank` returns `None` for an unmatched dictionary and
the caller sorts it **last, by `dict_id`**. So the visible symptom of a broken entry is a dictionary
silently dropping to the bottom, which reads as "chibipop reordered my dictionaries by itself"
rather than as a stale config. Naming the cause is the whole point.

The unmatched entry is still **kept** in the config (§5) — it may match a dictionary that simply is
not built right now, and deleting a user's setting because it is currently inapplicable would be
worse than carrying it. The warning is advice, not an action, and there is no button to "fix" it:
reordering the list and pressing Apply replaces the stale entry naturally, because D6's rule only
preserves entries that still match.

### D7 — the dictionary names come from the worker's existing read

`SqliteDictionary` lives on the worker thread and `dicts()` is already called **once** at startup
and reused ("Decision 2"). The main thread has no connection and must not open a second one merely
to list two names.

`startup_tx`'s payload therefore changes from `Result<()>` to `Result<Vec<DictInfo>>`, and `run`
keeps the list for the lifetime of the process. It is startup-time data that never changes while
running, so there is nothing to refresh.

### D8 — Apply writes, spawns, and quits

1. Write the edited `Config` with `Config::save` to the same path `run` loaded from.
2. **If that fails, report it in the window and do not restart.** A half-applied restart — old
   settings running, new settings on disk, or worse — is the one outcome worth refusing outright.
3. `CreateProcessW` on `std::env::current_exe()`, passing **the same argv**, so `--dict`, `--rules`
   and `--config` survive the restart.
4. `PostQuitMessage(0)`, letting the existing shutdown run in its established order: `KillTimer`,
   drop the hooks, remove the tray icon, join the worker.

**Restart is imperceptible and this was measured, not assumed: 0.18 s** from launch to a live tray
icon (the tray is created last in `run`, so its presence means startup finished). That number is
what makes restart-everything the right design rather than a compromise — a mixed hot-apply /
restart split would be more code and more failure modes for no gain the user can perceive.

### D8a — say that Apply restarts, before it is pressed

The user must know a restart is coming. **Not via a message box**: `MessageBoxW` is modal and runs
its own pump, which is the §7 hazard — it would stop `WM_TIMER` and latch the wheel arm, for a
notice nobody needs to acknowledge.

And not as a transient "Restarting…" either. At 0.18 s it would be gone before it could be read, so
it would inform nobody while looking like diligence.

So the notice is **permanent and pre-emptive**:

- the button reads **`Apply & Restart`**, not `Apply`;
- a static line sits beside the buttons: *"Applying saves your settings and restarts chibipop."*

Telling the user before they commit is the useful moment; telling them during a 0.18 s window is
theatre. This also keeps the whole round free of any modal pump, which is worth more than the
message box would have been.

**Apply does not need to close the window** — step 4 ends the process, which takes it with it. The
window is not hidden first: leaving it on screen until the process actually exits is the honest
signal that something is happening, and at 0.18 s the replacement's tray icon is back before a
"restarting…" message could be read.

**Cancel** destroys the window; the edited clone is dropped. Discarding is structural, not an undo.

**Closing the window with the X is Cancel**, not Apply. `WM_CLOSE` and the Cancel button take the
same path, so there is no way to lose edits *or* to apply them by accident.

### D9 — the window edits a clone

`settings.rs` takes a `Config` and returns a `Config`. It knows nothing about TOML, file paths, or
the running application. `run` owns loading, saving, spawning and quitting.

That boundary is what makes the interesting logic testable with no screen: the control⇄field
mapping, the numeric clamps, and D6's write-back rule are all pure functions over plain data.

---

## 5. Error handling

| Failure | Response |
|---|---|
| `Config::save` fails | Message box naming the path; **no restart**; the window stays open with edits intact |
| `CreateProcessW` fails | Message box; **do not quit** — a failed restart must leave the working instance running |
| The configured font is not installed | Insert it into the combo and select it (D4); never silently substitute |
| A `display_order` entry matches no dictionary | Keep it in the config untouched (it may match a dictionary that is not currently built) **and warn in the window, naming the entry** — D6a |
| Window creation fails | Log once and continue running; Settings is not worth killing the app for (matches the overlay's rule) |
| `Settings…` clicked while open | `SetForegroundWindow` on the existing window |

## 6. Testing

**Pure, no screen:**

- **D6 write-back**: reorder-only produces the identical set of strings, in the new order; a
  never-before-seen dictionary derives a bracket-truncated key; a dictionary whose existing entry
  is a short substring (`Jitendex`) keeps that short substring rather than gaining the date.
- **D6a stale-entry detection**: an entry matching no live dictionary is reported; an entry that
  matches at least one is not. Uses the same substring rule as `present::dict_order_rank`, so a
  test pins the two against each other — a warning that disagreed with the actual ordering would be
  worse than none.
- **Round-trip identity**: `Config → control values → Config` with nothing touched yields an equal
  `Config`. This is the assertion that catches a mis-wired control silently changing a setting.
- **Numeric clamps**: each spin range rejects out-of-range values by clamping, and the clamp
  matches the type's own limits (`max_height_percent` is a `u8`).
- **Font fallback**: a configured font absent from the enumerated list still appears and is
  selected.

**Not verifiable by an agent** — and this is now a well-characterised boundary, not a guess.
Synthetic mouse input cannot reach chibipop's low-level hook (`SendInput` returns 0; see
`findings/2026-07-28-popup-interaction-acceptance.md`), so the tray menu cannot be opened and no
control can be clicked. Everything in §2's acceptance list is the user's to verify, via a numbered
manual script the implementation round must ship, covering at minimum:

1. Tray shows exactly `Settings…` and `Quit`.
2. Settings opens; values match the TOML on disk.
3. Controls look native, not Windows-95 grey (this is the manifest working).
4. Hovering still resolves while the window is open, and the wheel still scrolls other windows.
5. Change the theme, Apply — the popup redraws in the new theme.
6. Reorder the dictionaries, Apply, hover a word both define — the order changed.
7. **Open `chibipop.toml` after step 6 and confirm `display_order` still contains the original
   strings**, merely reordered. This is the D6 trap, and it is invisible from the UI.
8. Edit `display_order` in the TOML to contain a nonsense entry, restart, open Settings. **The
   window warns, naming that entry** (D6a). Reorder and Apply; the nonsense entry is gone and the
   warning does not return.
9. Open Settings, touch nothing, Apply — `chibipop.toml` is unchanged apart from formatting.
10. The executable now has chibipop's icon in Explorer and on the taskbar.

## 7. Open risks

**A settings window is the second-largest source of nested pumps after the tray menu.** `MessageBoxW`
in the error paths of §5 **is modal and does pump**. Each such call must disarm the wheel first, for
exactly the reason D2 gives. This is a footgun the codebase now has three instances of; if a fourth
appears, the disarm belongs in a shared helper rather than at each call site.

**Restart loses transient state** — the current popup, its scroll position, and the sticky hold.
Acceptable at 0.18 s, and mentioned so nobody debugs it as a fault.

**Two instances briefly, if a restart races a manual launch.** There is still no single-instance
guard (`docs/BACKLOG.md` item 5), and this round makes restarts routine rather than rare. The 0.18 s
startup means the old instance's hooks are almost always gone first, but "almost always" is not a
guarantee. **Not fixed here** — it is its own change, and adding a named mutex under a settings
round would bury it.

**`EnumFontFamiliesExW` returns families, not faces.** Weight and style variants collapse to one
entry, which is correct for `Theme::font_name`'s use but means the combo cannot express "Klee One
SemiBold". No current setting can either, so this is a ceiling, not a regression.
