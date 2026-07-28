# chibipop — technical reference

Everything the [README](../README.md) deliberately leaves out: the config
file's own reference, the diagnostic subcommands, how to run the tests, and
the limits that were measured rather than assumed.

---

## Toolchain

- **Rust** stable, MSVC toolchain (`stable-x86_64-pc-windows-msvc`).
- **Python 3.9+**, standard library only — no `pip install`. It builds the
  dictionary and nothing else.
- `data/deconjugator.json` ships with the repository. No build step.

The dictionary database is **not** in the repository (232 MiB). Build it from
Yomitan format-3 archives:

```bash
python tools/build-dict/build.py --dicts-dir "C:\path\to\dicts" --out data/chibipop.sqlite
```

Frequency archives are detected automatically — by `frequencyMode` in
`index.json`, or `Freq` in the filename. Everything else is treated as a term
dictionary, ranked in filename order. Roughly 70 seconds.

---

## Subcommands

| Command | What it does |
|---|---|
| `chibipop run` | The popup application. |
| `chibipop settings` | The settings window alone — no popup, no hooks, no OCR. |
| `chibipop lookup 食べた` | Dictionary lookup only. No screen, no OCR. |
| `chibipop probe --at 1200,400` | One point, every stage printed: capture region → OCR lines and word boxes → resolved span → ranked hits → match box. Tells apart "OCR saw nothing" from "OCR saw text but nothing near the cursor". |
| `chibipop watch` | Follows the cursor and prints a lookup whenever the hovered word changes. Ctrl-C to stop. |

`probe` and `watch` both take `--tiles N`, mirroring `[ocr] max_ocr_passes`.
**Both default to 1, matching the shipped configuration** — a diagnostic that
defaults to a setting production doesn't use measures the wrong thing. The
memory figures in [`REGRESSION.md`](REGRESSION.md) were taken with `watch` at
`--tiles 3`.

`probe --show-region [SECONDS]` draws the capture boxes **and** the match box
for a few seconds. Note `probe` always draws both, being the diagnostic view;
the app on shipped defaults draws only the match box, so a real hover shows
one box where `probe` shows several.

### Paths

`--dict` and `--rules` default to `data/chibipop.sqlite` and
`data/deconjugator.json` **relative to the working directory**, so run from
the repository root or pass absolute paths.

`--config` is different on purpose: it defaults to `chibipop.toml` **beside
the executable**, so a shortcut-launched `chibipop.exe` still finds its
settings.

---

## Configuration file

The settings window covers every option below, and is the expected way in.
The file remains hand-editable.

Malformed TOML is a **hard error naming the file**, never a silent fallback —
that is how a typo quietly erases someone's settings. A value that parses but
is out of range is **clamped on load and named on stderr**.

```toml
[trigger]
mode = "live"           # "live" | "hold-shift"

[popup]
theme = "dark"          # "dark" | "light"
exclude_from_capture = false
max_height_percent = 45 # 10-90; cap as a percentage of the monitor's height
summary_chars = 40      # 10-200; collapsed-row summary length
font = "Yu Gothic UI"
highlight_match = true  # box the characters the popup is defining
scroll_popup = true     # let the wheel scroll a popup that overflows

[dictionaries]
display_order = ["大辞林", "Jitendex"]   # case-insensitive substrings, in priority order

[ocr]
max_ocr_passes = 1      # 1-5; 1 = no forward tiling (the default)

[debug]
show_scan_region = false   # outline what each hover captured
```

**Every setting is read once at startup.** Edit the file with chibipop
stopped, or use the settings window — pressing Apply there rewrites the whole
file from memory, so it would overwrite a hand-edit made while it was
running. The write goes to a temp file and is renamed into place, so an
interrupted save cannot leave a half-written config.

### `exclude_from_capture`

**Off by default**, so the popup records normally. Set it to `true` and the OS
hides the popup from *every* capture API — screen recorders, screenshots,
screen sharing, remote desktop — while it stays plainly visible on your own
display.

Either way chibipop hides the popup around its own OCR captures, so it never
reads its own rendered text back into the next lookup. Exclusion was the
original default; measured in real use the hide/reshow guard costs no
noticeable lookup latency, so being recordable won.

### `max_ocr_passes`

How many screen captures each hover costs: one to find the word under the
cursor, the rest to read forward from it. **Defaults to `1`, meaning no
forward tiling.**

Tiling reads further ahead — roughly 10 characters past the cursor rather
than 7 — but measured on 2026-07-28 it sometimes resolves *a different
character than the one you are pointing at*, and a longer reading of the
wrong word is worse than a short reading of the right one. Over nine hovers
on one line, **single-pass got the character right 9 times out of 9; three
passes managed 4.**

### `highlight_match`

Draws **one** faint box around the characters the popup is currently
defining. **On by default** — it is the visible answer to "is it defining the
word I am pointing at?", which otherwise means reading numbers out of
`probe`.

It follows the *match*, so a deconjugated verb is boxed across everything it
consumed (食べさせられた, not just 食べ). It does not draw where the match
cannot be located honestly: no dictionary hit, or `max_ocr_passes >= 2`, where
text is stitched from several captures that do not carry their character
boxes with them. An absent box is the designed behaviour there; a misplaced
one would not be.

### Sticky hover

The popup stays put while you read it. Move the cursor from the word into the
popup and it holds still — the hovered character, the gap, and the popup
itself count as one region, and while the cursor is anywhere in it chibipop
does no new lookup at all. A word whose answer has not changed is not
redrawn either, so holding still does not flicker.

The held region is the characters the entry actually *matched*, not just the
one glyph under the cursor, so scanning along a conjugated verb shows one
popup rather than one per character. It also allows the few pixels of
vertical slack that resolve the same character anyway, which is what stopped
small kana from flickering.

One deliberate exception: moving *sideways* past the end of the matched word
shows the next word rather than holding the previous popup. That is what you
want when scanning a line, and it is why the held area is the word plus the
popup rather than the box enclosing both.

### `scroll_popup`

Lets the wheel scroll a popup whose content overflows the `max_height_percent`
cap. **On by default.** A thin scrollbar appears at the right edge when there
is more to read; the window itself never grows.

While the cursor is inside such a popup, chibipop takes the wheel so the
window underneath does not scroll at the same time. It takes it **only** then
— cursor on the word rather than the popup, or content that fits, and the
wheel behaves normally. Setting `scroll_popup = false` stops chibipop
touching the wheel at all while still drawing the scrollbar, so you can see
there is more even though you cannot reach it.

### `display_order`

Matched by **name substring**, and the settings window says so. The TOML
stores case-insensitive substrings rather than full names, because a rebuilt
Jitendex changes its own name — the release date is part of it.

The window preserves whatever substring you already have, so reordering never
rewrites your entries. If one stops matching any installed dictionary it tells
you which, because the visible symptom otherwise is that dictionary quietly
sorting last.

Blank entries are ignored: an empty string is a substring of every name, so
one would silently pin the whole order.

### `show_scan_region`

The *debug* view: a faint outline around every region a hover captured —
pass 1's box, each forward tile, and the word it resolved. Turn it on when
OCR is behaving oddly and you want to see what it actually looked at rather
than infer it.

**Off by default**, and independent of `highlight_match`: with only the
highlight on you get one box, not four.

---

## Tests

```bash
cargo test
```

**245 tests.** Plus the dictionary builder, from `tools/build-dict`:

```bash
python -m unittest discover -s tests
```

**57 tests.**

**After any large change, work through [`REGRESSION.md`](REGRESSION.md)** — a
cheapest-first checklist: the automated gate, then what can be verified
against real pixels with `probe`, then the dozen things only a human at the
keyboard can check. It also lists the traps that have bitten more than once.

Three allow-by-default lints are enabled at both crate roots
(`missing_unsafe_on_extern`, `unsafe_attr_outside_unsafe`,
`unsafe_op_in_unsafe_fn`), so every unsafe operation is written inside a
visible `unsafe` block rather than relying on an `unsafe fn`'s blanket.

---

## Status

**M0–M3 are built and merged:** OCR text acquisition, the lookup core
(deconjugation, ranking, SQLite), the popup, and a native settings window
covering every option plus dictionary order.

**M4** (UI Automation tier, for a cheaper path than OCR where the text is
already selectable) and **M5** (DPI and Magpie polish) are not started.

### Known limits, measured rather than assumed

- **Vertical text does not work at the shipped capture shape.** Worse than
  reading short: the 500×100 box spans several columns, so it can return a
  sentence spliced out of unrelated ones. Measured 2/6 correct hovers; a
  transposed 100×500 probe scores 6/6. The fix has its own round —
  [measurement](superpowers/findings/2026-07-28-vertical-text-measurement.md).
- **Forward tiling is off** (`max_ocr_passes = 1`) because it sometimes
  resolved a different character than the one under the cursor. The rework is
  designed and reviewed, not built — see [`BACKLOG.md`](BACKLOG.md).
- **Text clipped by a window edge cannot be read** at any capture shape. The
  glyphs are physically incomplete on screen; a ceiling, not a bug.
- **Right-clicking the tray icon does not open its menu.** A real bug with two
  unproven suspects and a one-click diagnosis, written up in
  [`BACKLOG.md`](BACKLOG.md) §7. Everything the menu offered is reachable
  anyway. The lesson it cost: posting `WM_TRAYICON` by hand proves the
  *handler* works and says nothing about whether Windows *delivers* a real
  click.
- **The 50 MB memory target is not confirmed met.** The OCR→lookup path
  measures at 37 MB working set / 15 MB private, flat over 417 hovers with no
  handle growth, and the app idles at 12 MB. But sustained real hovering also
  drives the Direct2D text renderer, and that configuration measured 94.8 MB /
  60 MB at M3 and has not been re-measured since. Text formats are now cached
  per (font, size) rather than rebuilt per line per frame, which is a *lead*
  on that number and not a verified fix. Binary size is 3.4 MB; CPU is under
  1% while hovering and 0.000% idle.
- **Sticky hover, scrolling and the anti-flicker check are shipped but not yet
  accepted.** Their unit coverage is thorough and the wheel's swallow decision
  is verified on both branches, but every *hover* behaviour needs a human —
  synthetic mouse movement cannot reach a global low-level hook. The nine-item
  script is in
  [the acceptance findings](superpowers/findings/2026-07-28-popup-interaction-acceptance.md).
