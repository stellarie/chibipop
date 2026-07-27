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
Right-click the tray icon for **Live** / **Hold Shift** mode and **Quit**.

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

Hand-edited TOML, written with defaults on first run. There is no settings
GUI. Malformed TOML is a hard error naming the file, never a silent fallback.

```toml
[trigger]
mode = "live"           # "live" | "hold-shift"

[popup]
theme = "dark"          # "dark" | "light"
exclude_from_capture = false
max_height_percent = 45 # cap, as a percentage of the monitor's height
summary_chars = 40      # collapsed-row summary length
font = "Yu Gothic UI"

[dictionaries]
display_order = ["大辞林", "Jitendex"]   # case-insensitive substrings, in priority order

[ocr]
max_ocr_passes = 3      # 1 disables forward tiling - see the two-pass spec
```

**`exclude_from_capture`** is **off by default**, so the popup records normally.
Set it to `true` and the OS hides the popup from *every* capture API —
screen recorders, screenshots, screen sharing, remote desktop — while it stays
plainly visible on your own display. Either way chibipop hides the popup around
its own OCR captures, so it never reads its own rendered text back into the
next lookup. Exclusion was the original default; measured in real use the
hide/reshow guard costs no noticeable lookup latency, so being recordable won.

**`max_ocr_passes`** is how many screen captures each hover costs: one to find
the word under the cursor, the rest to read forward from it. Set it to `1` to
turn forward tiling off entirely and get the older, shorter-reaching behaviour
back — useful if OCR misbehaves on content very different from what the tile
width was tuned against.

The setting is read **once at startup** — edit the file with chibipop stopped,
since switching mode from the tray rewrites the whole file from memory and
would overwrite an edit made while it was running.

## Tests

```bash
cargo test
```

103 tests. Plus the dictionary builder, from `tools/build-dict`:

```bash
python -m unittest discover -s tests
```

39 tests.

## Status

M0–M3 are built and merged: OCR text acquisition, the lookup core
(deconjugation, ranking, SQLite), and the popup with tray and configuration.
M4 (UI Automation tier, for a cheaper path than OCR where the text is already
selectable) and M5 (DPI and Magpie polish) are not started.

The 50 MB memory target is **not** met under sustained real use — measured at
94.8 MB working set / 60 MB private. Working set is largely reclaimable
(a forced trim drops it to 0.05 MB while private bytes hold at 2.5 MB), and
the OCR→lookup→present pipeline measures as a floor rather than a leak, but
the Direct2D paint path's curve is unproven. Details in
[`docs/superpowers/findings/2026-07-27-m3-acceptance.md`](docs/superpowers/findings/2026-07-27-m3-acceptance.md).

Design specs, plans, and verification findings live in `docs/superpowers/`.

## Dictionaries

None are redistributed here. The shipped build uses Jitendex (CC BY-SA 4.0),
大辞林 第四版 (© Sanseido), and jiten_freq_global — you supply your own copies
and build the database locally.
