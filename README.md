# chibipop

A screen-wide Japanese pop-up dictionary for Windows. Hover any Japanese text
— in a game, a PDF, a video, a screenshot, anything the display can render —
and its definition appears beside it.

Text is acquired by OCR of a small region around the cursor, so it works on
pixels, not on selectable text. No browser extension, no clipboard, no
injection into the target application.

## Requirements

- **Windows 10/11** with the Japanese OCR recognizer installed. Verify:

  ```bash
  powershell -NoProfile -Command "[Windows.Media.Ocr.OcrEngine,Windows.Media,ContentType=WindowsRuntime] | Out-Null; [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | ForEach-Object { $_.LanguageTag }"
  ```

  `ja` must appear in the output. If it does not, add the Japanese language
  pack in Settings → Time & language.
- **Rust** stable, MSVC toolchain (`stable-x86_64-pc-windows-msvc`).
- **Python 3.9+** — only to build the dictionary database. Standard library
  only, no pip install.

## Build

```bash
cargo build --release
```

Produces `target/release/chibipop.exe`.

### Build the dictionary

The database is **not** in the repository (232 MiB). Build it from Yomitan
format-3 archives — put the `.zip` files in one directory and point the
builder at it:

```bash
python tools/build-dict/build.py --dicts-dir "C:\Users\Stella\Documents\dicts" --out data/chibipop.sqlite
```

Frequency archives are detected automatically (by `frequencyMode` in
`index.json`, or `Freq` in the filename); everything else is treated as a term
dictionary, ranked in filename order. Takes roughly 70 seconds.

`data/deconjugator.json` ships with the repository — no build step.

## Usage

```bash
chibipop run
```

Starts the popup. Hover Japanese text; the definition appears beside it.
Right-click the tray icon for **Settings…** and **Quit**.

Three diagnostic subcommands, useful when something isn't working:

| Command | What it does |
|---|---|
| `chibipop lookup 食べた` | Dictionary lookup only. No screen, no OCR. |
| `chibipop probe --at 1200,400` | One point, every stage printed: capture region → OCR lines and word boxes → resolved span → hits. Tells apart "OCR saw nothing" from "OCR saw text but nothing near the cursor". |
| `chibipop watch` | Follows the cursor and prints a lookup whenever the hovered word changes. Ctrl-C to stop. |

**Paths.** `--dict` and `--rules` default to `data/chibipop.sqlite` and
`data/deconjugator.json` *relative to the working directory*, so run from the
repository root or pass absolute paths. `--config` is different on purpose: it
defaults to `chibipop.toml` **beside the executable**, so a shortcut-launched
`chibipop.exe` still finds its settings.

## Configuration

**Right-click the tray icon for `Settings…` and `Quit`.** The settings window
covers every option below — trigger mode, theme, font, the popup's size caps,
the three popup toggles, the dictionary order, and a Debug group for OCR passes
and the scan overlay. **Apply saves and restarts chibipop**, which takes about
0.18 s; the button says so, and so does the line beside it.

The window offers no free-text fields anywhere: every value is a choice from a
list, so nothing you can select will fail to load later. Two deliberate
consequences — a font that is configured but not installed is still offered and
selected, and a hand-edited number that is off the list's step is offered as-is
rather than snapped. Opening Settings and pressing Apply without touching
anything never changes a setting.

The TOML below remains the reference, and is still hand-editable — it is simply
no longer the only way in. Malformed TOML is a hard error naming the file,
never a silent fallback.

```toml
[trigger]
mode = "live"           # "live" | "hold-shift"

[popup]
theme = "dark"          # "dark" | "light"
exclude_from_capture = false
max_height_percent = 45 # cap, as a percentage of the monitor's height
summary_chars = 40      # collapsed-row summary length
font = "Yu Gothic UI"
highlight_match = true  # box the characters the popup is defining
scroll_popup = true     # let the wheel scroll a popup that overflows

[dictionaries]
display_order = ["大辞林", "Jitendex"]   # case-insensitive substrings, in priority order

[ocr]
max_ocr_passes = 1      # 1 = no forward tiling (the default); 2+ enables it

[debug]
show_scan_region = false   # outline what each hover captured - see the scan-overlay spec
```

**`exclude_from_capture`** is **off by default**, so the popup records normally.
Set it to `true` and the OS hides the popup from *every* capture API —
screen recorders, screenshots, screen sharing, remote desktop — while it stays
plainly visible on your own display. Either way chibipop hides the popup around
its own OCR captures, so it never reads its own rendered text back into the
next lookup. Exclusion was the original default; measured in real use the
hide/reshow guard costs no noticeable lookup latency, so being recordable won.

**`max_ocr_passes`** is how many screen captures each hover costs: one to find
the word under the cursor, the rest to read forward from it. **It defaults to
`1`, meaning no forward tiling.**

Tiling reads further ahead — roughly 10 characters past the cursor rather than
7 — but measured on 2026-07-28 it sometimes resolves *a different character
than the one you are pointing at*, and a longer reading of the wrong word is
worse than a short reading of the right one. Over nine hovers on one line,
single-pass got the character right 9 times out of 9; three passes managed 4.
Set it to `2` or more to turn tiling back on.

Every setting is read **once at startup**. Edit the TOML with chibipop stopped,
or use the settings window — pressing Apply there rewrites the whole file from
memory, so it would overwrite a hand-edit made while it was running.

**`highlight_match`** draws **one** faint box around the characters the popup
is currently defining, and is **on by default**. Hover 可哀想 and the box
surrounds 可哀想 — the visible answer to "is it defining the word I am pointing
at?", which otherwise means reading numbers out of `probe`. It follows the
*match*, so a deconjugated verb is boxed across everything it consumed
(食べさせられた, not just 食べ).

It does not draw on a hover where the match cannot be located honestly: no
dictionary hit, or `max_ocr_passes ≥ 2`, where text is stitched from several
captures that do not carry their character boxes with them. An absent box is
the designed behaviour there; a misplaced one would not be.

**The popup stays put while you read it.** Move the cursor from the word into
the popup and it holds still — the hovered character, the gap, and the popup
itself count as one region, and while the cursor is anywhere in it chibipop
does no new lookup at all. Move fully off and hovering resumes. A word whose
answer has not changed is not redrawn either, so holding still does not flicker.

The held region is the characters the entry actually *matched*, not just the one
glyph under the cursor, so scanning along a conjugated verb shows one popup
rather than one per character. It also allows the few pixels of vertical slack
that resolve the same character anyway, which is what stopped small kana from
flickering.

One deliberate exception: moving *sideways* past the end of the matched word
shows the next word, rather than holding the previous popup. That is what you
want when scanning a line, and it is why the held area is the word plus the
popup rather than the box enclosing both.

**`scroll_popup`** lets the wheel scroll a popup whose content overflows the
`max_height_percent` cap, and is **on by default**. A thin scrollbar appears at
the right edge when there is more to read; the window itself never grows.

While the cursor is inside such a popup, chibipop takes the wheel so the window
underneath does not scroll at the same time. It takes it **only** then — cursor
on the word rather than the popup, or content that fits, and the wheel behaves
normally. Setting `scroll_popup = false` stops chibipop touching the wheel at
all while still drawing the scrollbar, so you can see there is more even though
you cannot reach it.

**Dictionary order is matched by *name*, and the settings window says so.** The
TOML stores case-insensitive substrings rather than full names, because a
rebuilt Jitendex changes its own name (the release date is part of it). The
window preserves whatever substring you already have, so reordering never
rewrites your entries — and if one of them stops matching any installed
dictionary, it tells you which, because the visible symptom otherwise is that
dictionary quietly sorting last.

**`show_scan_region`** is the separate *debug* view: a faint outline around
every region a hover captured — pass 1's box, each forward tile, and the word
it resolved. Turn it on when OCR is behaving oddly and you want to see what it
actually looked at rather than infer it. It is **off by default**, and the two
settings are independent: with only `highlight_match` on you get one box, not
four. `probe --show-region` shows the capture outlines for a single coordinate
without running the app.

## Tests

```bash
cargo test
```

237 tests. Plus the dictionary builder, from `tools/build-dict`:

```bash
python -m unittest discover -s tests
```

48 tests.

## Status

M0–M3 are built and merged: OCR text acquisition, the lookup core
(deconjugation, ranking, SQLite), and the popup with tray and configuration.
M4 (UI Automation tier, for a cheaper path than OCR where the text is already
selectable) and M5 (DPI and Magpie polish) are not started.

**Known limits**, measured rather than assumed:

- **Vertical text does not work at the shipped capture shape.** Worse than reading
  short: the 500×100 box spans several columns, so it can return a sentence spliced
  out of unrelated ones. Measured 2/6 correct hovers; a transposed 100×500 probe
  scores 6/6. The fix has its own round —
  [measurement](docs/superpowers/findings/2026-07-28-vertical-text-measurement.md).
- **Forward tiling is off** (`max_ocr_passes = 1`) because it sometimes resolved a
  different character than the one under the cursor. The rework is designed and
  reviewed, not built — see [`docs/BACKLOG.md`](docs/BACKLOG.md).
- **Text clipped by a window edge cannot be read** at any capture shape. The glyphs
  are physically incomplete on screen; this is a ceiling, not a bug.
- **The 50 MB memory target is not confirmed met.** The OCR→lookup path measures at
  37 MB working set / 15 MB private, flat over 417 hovers with no handle growth, and
  the app idles at 12 MB. But sustained real hovering also drives the Direct2D text
  renderer, and that configuration measured 94.8 MB / 60 MB at M3 and has not been
  re-measured since. Binary size is 3.3 MB; CPU is under 1% while hovering and
  0.000% idle.
- **Sticky hover, scrolling and the anti-flicker check are shipped but not yet
  accepted.** Their unit coverage is thorough and the wheel's swallow decision is
  verified on both branches, but every *hover* behaviour needs a human — synthetic
  mouse movement cannot reach a global low-level hook. The nine-item script is in
  [the acceptance findings](docs/superpowers/findings/2026-07-28-popup-interaction-acceptance.md).

Design specs, plans, and verification findings live in `docs/superpowers/`;
deferred work with its evidence lives in [`docs/BACKLOG.md`](docs/BACKLOG.md).

## Dictionaries

None are redistributed here. The shipped build uses Jitendex (CC BY-SA 4.0),
大辞林 第四版 (© Sanseido), and jiten_freq_global — you supply your own copies
and build the database locally.
