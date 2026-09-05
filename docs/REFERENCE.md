# chibipop — technical reference

Everything the [README](../README.md) deliberately leaves out: the config
file's own reference, the diagnostic subcommands, how to run the tests, and
the limits that were measured rather than assumed.

---

## Toolchain

- **Rust** stable. MSVC on Windows (`stable-x86_64-pc-windows-msvc`), the
  default toolchain on Linux.
- `data/deconjugator.json` ships with the repository. No build step.
- The Linux OCR models ship with the repository too
  (`crates/chibipop-linux/models/meiki/`). Nothing is downloaded.

**One repository, two binaries**
([`ARCHITECTURE.md`](../ARCHITECTURE.md#workspace-and-seams)). The core
library is the root package. One bin crate per platform sits under `crates/`,
and both produce a binary named `chibipop`, so cargo uplifts both to one path.
Any command that links must exclude the foreign bin crate:

| Command | Windows | Linux |
|---|---|---|
| Test | `cargo test --workspace --exclude chibipop-linux` | `cargo test --workspace --exclude chibipop-windows` |
| Build | `cargo build --release --workspace --exclude chibipop-linux` | `cargo build --release --workspace --exclude chibipop-windows` |

Check-shaped commands link nothing, so `cargo clippy --workspace` spans every
member on either host.

The dictionary database is **not** in the repository (232 MiB). Build it from
Yomitan format-3 archives, either in the settings window or from the command
line:

```bash
chibipop build-dict --library "C:\path\to\dicts" --out data/chibipop.sqlite
```

Frequency archives are detected automatically — by `frequencyMode` in
`index.json`, or `Freq` in the filename. Everything else is treated as a term
dictionary, ranked in filename order. Roughly 70 seconds.

---

## Subcommands

The two binaries do not share a command line. Windows first, then Linux.

### Windows

| Command | What it does |
|---|---|
| `chibipop run` | The popup application. |
| `chibipop settings` | The settings window alone — no popup, no hooks, no OCR. |
| `chibipop settings --audit` | Dumps the settings window's control tree as JSON and exits. No visible window; for diffing a layout change. |
| `chibipop lookup 食べた` | Dictionary lookup only. No screen, no OCR. |
| `chibipop probe --at 1200,400` | One point, every stage printed: capture region → OCR lines and word boxes → resolved span → ranked hits → match box. Tells apart "OCR saw nothing" from "OCR saw text but nothing near the cursor". |
| `chibipop watch` | Follows the cursor and prints a lookup whenever the hovered word changes. Ctrl-C to stop. |
| `chibipop build-dict --library DIR --out FILE` | Builds `chibipop.sqlite` from a folder of Yomitan `.zip` archives, printing one line per archive. Term archives are ordered by filename, which is what assigns `dict_id`; frequency archives are detected by their `index.json`. |

### Linux

The daemon holds every capability, so the Linux commands are a daemon, a
settings process, control-socket verbs, and three diagnostics.

| Command | What it does |
|---|---|
| `chibipop run` | The daemon. The default when no subcommand is given. |
| `chibipop settings` | The settings window, in its own process ([`ARCHITECTURE.md`](../ARCHITECTURE.md#settings-and-config)). A settings crash cannot take live hover down. |
| `chibipop ctl VERB` | Sends one verb to the running daemon over its control socket. `lookup` runs one Press-mode lookup at the cursor. Bind these in your compositor. |
| `chibipop probe` | Connects to the Wayland display, prints the capability report, exits. |
| `chibipop capture-dump` | Grabs screen regions with the selected capture backend and writes PNGs. |
| `chibipop clipboard-check` | Takes the clipboard selection with a known string and holds it. |

`--config PATH` is global on Linux and skips config discovery entirely.

**The `ctl` verb set is fixed**
([`ARCHITECTURE.md`](../ARCHITECTURE.md#input-ladders)): `reload`,
`trigger-down`, `trigger-up`, `toggle`, `lookup`, `anki-add`, `screenshot`,
`ocr-clipboard`, `static-region`. One verb per global action, never a
scripting API. The settings window prints a ready-to-paste compositor bind for
each one, naming the running binary's real path.

The three diagnostics are lock-free and socket-free, so all three are safe to
run beside a live daemon. [`LINUX.md`](LINUX.md#command-line) carries the
worked examples.

`build-dict` writes to `<out>.tmp` and renames on success only, so a failed
build leaves the previous database byte-identical. It holds the whole frequency
table in memory, which is why the settings window has always run it **as a
child process** rather than in-process: that spike must not land in something
that idles at 12 MB.

### Dictionary changes are incremental, as of v0.8.0

**Adding or removing a dictionary no longer rebuilds anything.** `chibipop run`
opens a second, read-write connection to the live database and edits it in
place, on a background thread, while the worker's read connection stays open:

- a **removal** deletes `term` → `entry` → `dict` in one transaction, in that
  order, because foreign keys are on and enforced;
- an **addition** parses that one archive and inserts with ids allocated from
  `MAX(…) + 1`, so it cannot collide with rows that are already there.

The cost is proportional to the change rather than to the database — measured
on fixtures, **~105–400 ms to remove** and **~240 ms per 50,000 entries added**,
against minutes to rebuild the whole database. Nothing is staged, nothing is renamed,
the worker is never stopped and the process never restarts.

That is only safe because the database is in **WAL** journal mode, where a
writer and a reader coexist. `run` **asserts** it rather than assuming it: a
legacy `delete`-mode file is refused with a message instead of written to,
because on one a write returned `SQLITE_BUSY` after 5.5 s whenever a reader had
a cursor mid-scan.

Removals depend on an index, `idx_term_entry_id` on `term(entry_id)`, which
`build-dict` now creates for new databases and the removal path creates lazily
for existing ones. Without it a single delete is quadratic: **443.8 s** for the
*smallest* installed dictionary, about **7.6 hours** extrapolated for 大辞林.
The schema version is deliberately **not** bumped for it, since that would tell
every existing user to do the full rebuild this release exists to abolish.

`chibipop settings` — the standalone window, and the path `run` falls into when
there is no database at all — is untouched by this and still rebuilds: there is
no worker, no hooks and no open database there, and a first run needs a full
build anyway.

### When a full rebuild is still needed

`build-dict` is not going anywhere. Five things still require it, and the first
four were verified against the release binary on 2026-08-14:

| | Why incremental cannot serve it |
|---|---|
| **First run** | There is no database to edit. |
| **Schema migration** | The reader refuses a file at the wrong `schema_version` before anything can open it. |
| **A corrupt database** | "file is not a database" — there is nothing to edit. |
| **Repair / drift** | Rebuilding from the library is the definition of making them agree. |
| **Frequency changes** | **New in v0.8.0.** A frequency list ranks words across *every* dictionary, so adding or removing one means re-ranking `term.freq` across the whole `term` table — a different operation from inserting one archive. |

**The settings window refuses a frequency-archive change** rather than
pretending, with a message naming the command and your real paths. Nothing is
moved, no config is saved, and the rest of your staged changes stay in the form
so you can drop the frequency zip and apply them.

### The database can now drift from the library

Editing in place breaks an invariant that held until v0.8.0: that
`chibipop.sqlite` **is** `build-dict(library/)`. It no longer is, and cannot be
kept so without the wait this release removes. `meta.source_hashes` records
which archives built the database, and both the add and the remove path keep it
current, so a mismatch is detectable — and **detection is all it is**. When the
settings window opens it names the archives that are in your library but not in
the database, or in the database but no longer in your library, says plainly
that nothing is broken and lookups still work, and gives you the `build-dict`
command to run if you want them to match again.

**It never rebuilds by itself.** Starting a multi-minute rebuild because a
window opened would be the worst possible surprise, and it is the wait this
release exists to remove.

Three cases deliberately report nothing: a library with no term archive in it
(no rebuild is possible, so offering one is worse than silence), an unreadable
archive (those are never recorded in `source_hashes`, so one corrupt download
would pin a permanent, unfixable notice), and a database whose `source_hashes`
is absent or unparseable — a legacy or hand-built file.

`probe` and `watch` both take `--tiles N`, mirroring `[ocr] max_ocr_passes`.
**Both default to 1, matching the shipped configuration** — a diagnostic that
defaults to a setting production doesn't use measures the wrong thing. The
memory figures in [`REGRESSION.md`](REGRESSION.md) were taken with `watch` at
`--tiles 3`.

`probe --show-region [SECONDS]` draws the capture boxes **and** the match box
for a few seconds. Note `probe` always draws both, being the diagnostic view;
the app on shipped defaults draws only the match box, so a real hover shows
one box where `probe` shows several.

`chibipop settings --audit` opens the settings window off-screen and hidden,
and prints one JSON document — `{"dumps": [...]}` — instead of showing
anything: one dump per tab, plus a fifth for the Anki tab with its field map
expanded. Each dump carries the client size and every control's `id`,
`parent_id`, `class`, `text`, `rect` (`x`/`y`/`w`/`h`), `visible`, `enabled`
and `tabstop`, plus the Tab-key ring in both directions (`tab_ring`,
`tab_ring_reverse`). It sends no input and draws no pixel, so two builds can
be compared with a plain line diff on their output — a clean diff after a
change that should not move anything is the evidence, not an argument.
`--dict` and `--config` are inherited from `settings`.

### Paths

`--dict` and `--rules` look **beside the executable first**, and fall back to
the working directory when nothing is there. That serves both layouts: a
shipped chibipop keeps its data beside the exe, while a development tree has
the binary in `target/release/` and the data at the repository root.

`--config` deliberately has no fallback — it resolves **beside the executable**
only. It is created when absent, so a fallback would write a first run's
settings to whichever directory happened to launch it.

On Linux the config is discovered in a fixed order, **first match wins, never
dual-read**: the `--config` flag → **portable mode** (a `chibipop.toml` beside
the executable puts data, log, and cache beside it too) → XDG
(`~/.config/chibipop/chibipop.toml`, data in `~/.local/share/chibipop`, the log
in `~/.local/state/chibipop`, cache in `~/.cache/chibipop`). The instance lock
and control socket always live under `$XDG_RUNTIME_DIR/chibipop`, keyed per
`$WAYLAND_DISPLAY`, in every mode.

**Migrating between the portable and XDG layouts is a manual copy** — chibipop
never moves your files itself. To go portable → XDG: stop the daemon, copy
`chibipop.toml` to `~/.config/chibipop/` and the `data/` folder's contents to
`~/.local/share/chibipop/`, then remove (or rename) the `chibipop.toml` beside
the executable so portable mode stops winning. The reverse direction is the
same copy the other way. The log and cache are disposable; don't bother.

### The latest-build copy

`C:\Users\Stella\Documents\chibipop-latest` holds the latest build. Refresh it
after every release build.

> [!important] `scripts/blank-copy.ps1` does not exist — corrected 2026-08-29
> This section named that script until today, and so did `RELEASING.md` step 8
> and two tier 1 items. All of them named a command nobody could run. The
> script is absent from the repository, from every worktree, and from all
> three installs. `scripts/` stopped being gitignored on 2026-08-27, so a
> replacement would be tracked if anyone writes one. Until then the refresh is
> the manual copy below.

**Copy five files, and nothing else.** The refresh replaces the executable,
the deconjugation rules, the README, and the two plugin files that are not
machine-specific.

```powershell
$src = "C:\Users\Stella\chibipop"
$dst = "C:\Users\Stella\Documents\chibipop-latest"

Copy-Item "$src\target\release\chibipop.exe" "$dst\chibipop.exe" -Force
Copy-Item "$src\data\deconjugator.json" "$dst\data\deconjugator.json" -Force
Copy-Item "$src\README.md" "$dst\README.md" -Force

$cfg = $null
if (Test-Path "$dst\plugins\meikiocr\config.toml") {
    $cfg = Get-Content "$dst\plugins\meikiocr\config.toml" -Raw
}
Copy-Item "$src\plugins\meikiocr\plugin.toml" "$dst\plugins\meikiocr\plugin.toml" -Force
Copy-Item "$src\plugins\meikiocr\adapter.py" "$dst\plugins\meikiocr\adapter.py" -Force
if ($cfg) { Set-Content "$dst\plugins\meikiocr\config.toml" $cfg -NoNewline }
```

Then verify: `& "$dst\chibipop.exe" --version`.

**Never touch these**, in any install. Each one is the user's, not the
build's:

- `chibipop.toml` — their settings
- `library/` — their dictionary archives, hundreds of MB
- `data/chibipop.sqlite` — their built database
- `popup.css` — their theming
- `screenshots/`
- `plugins/meikiocr/config.toml` — their machine-specific paths

**Seeding a fresh folder** is the same copy into an empty directory, plus
`LICENSE`. A seeded folder carries no `chibipop.toml` and no database, so its
first launch takes the **first-run path**: the settings window, and a config
written with defaults. Seed a scratch folder to test that path, never an
install.

> [!warning] Quit chibipop first, by pid or by path — never by name
> Windows will not overwrite a running executable. Three separate installs on
> this machine are called `chibipop.exe` — `chibipop-latest`,
> `chibipop-nightly` and `chibipop-nightly-jp` — plus the repository's
> `target/` builds. `Stop-Process -Name chibipop` closes all of them,
> including the real install. See [`REGRESSION.md`](REGRESSION.md) tier 0.

**After a release, copy from the published zip instead of `target/release`.**
Unzip the asset and use its `chibipop.exe`, `data/deconjugator.json`,
`README.md` and `LICENSE`. The install then runs the exact bytes everyone else
downloaded, and `Get-FileHash` against the zip's copy proves it. Done this way
for v0.9.9 on 2026-08-29, on all three installs.

Copying from `target/release` is still right between releases, for a nightly
off a branch. That copy is a **local build**, not the released artifact: its
bytes differ from the zip's — different toolchain, and different line endings
on `LICENSE`. Verify a *release* against the zip from the draft, per
[`RELEASING.md`](RELEASING.md) step 6.

**`chibipop-latest` carries no `plugins/` folder** as of 2026-08-29, though the
shipped zip has one. A refresh does not create it: seeding a plugin into a
configured install is a change, not a refresh. Copy `plugins/meikiocr/` in by
hand to bring it level with a fresh install.

---

## Configuration file

The settings window covers every option below, and is the expected way in.
The file remains hand-editable.

Malformed TOML is a **hard error naming the file**, never a silent fallback —
that is how a typo quietly erases someone's settings. A value that parses but
is out of range is **clamped on load and named on stderr**.

This is the whole file, at its defaults, as `Config::default()` serializes it
on 2026-08-29. Both platforms read the same schema
([`ARCHITECTURE.md`](../ARCHITECTURE.md#settings-and-config)); the keys that
end in `_linux` are the Linux twin of the key above them, and Windows ignores
them.

```toml
[trigger]
mode = "live"               # "live" | "hold-key" | "toggle" | "press" | "hold-shift" (legacy)
trigger_key = "shift"       # Windows: the key for hold-key, toggle, and press mode
trigger_key_linux = "ALT+F" # Linux: the chord, in portal syntax
per_character_lookup = false

[popup]
theme = "dark"              # "dark" | "light"
exclude_from_capture = false
max_width_percent = 25      # 10-90; cap as a percentage of the monitor's width
max_height_percent = 45     # 10-90; and of its height
summary_chars = 40          # 10-200; collapsed-row summary length
font = "Yu Gothic UI"
highlight_match = true      # box the characters the popup is defining
scroll_popup = true         # let the wheel scroll a popup that overflows
edge_autoscroll = true      # auto-scroll while a selection drag reaches the edge
side_panel = false
layer = "overlay"           # Linux: "overlay" | "top" (below fullscreen clients)
layout_mode = "roomy"       # "roomy" | "compact"; how much room an entry gets
dictionary_styling = true   # apply a dictionary's own fonts, colours and boxes
show_examples = true        # example sentences
show_attributions = true    # attributions and footnotes, independent of examples
show_images = true          # a dictionary's images; off leaves their alt text
show_part_of_speech = false # part-of-speech labels inside the entry

[dictionaries]
display_order = ["大辞林", "Jitendex"]   # case-insensitive substrings, in priority order

[dictionaries.per_language]              # optional; keyed by OCR recognizer tag
"ja"         = ["大辞林", "Jitendex"]    # these, in this order, and nothing else
"zh-Hans-CN" = ["中日大辞典"]

[plugins]
enabled = []                # Windows only; discovery extends this in memory

[ocr]
max_ocr_passes = 1          # 1-5; 1 = no forward tiling (the default)
prefer_vertical = false     # swap capture_width and capture_height
capture_width = 500         # 100-1600 px, centred on the cursor
capture_height = 100        # 80-600 px
scan_alphanumeric = true
language = "ja"             # Windows recogniser tag; Linux always reads ja
engine = "builtin"          # Windows: "builtin", or a discovered plugin name

[debug]
show_scan_region = false    # outline what each hover captured
show_lookup_log = false     # a console printing each resolved hover
show_engine_log = false     # name the active OCR engine in the status bar
show_adapter_log = false    # stream the plugin adapter's stderr

[anki]
enabled = false
url = "http://localhost:8765"
deck = "Default"
model = "Lapis"
add_key = "a"               # Windows
add_key_linux = "ALT+A"     # Linux, portal syntax
notify_on_add = true
sentence_mode = "sentence"  # "sentence" | "line" | "all" | "static"
static_region_key = ""      # Windows; empty leaves it unbound
static_region_key_linux = ""    # Linux; a compositor bind, not portal syntax
show_static_overlay = true
include_dictionary_name = true
first_dict_only = false
selection_buttons = "primary-additive"  # "primary-additive" | "primary-replacing"
selection_separator = "ellipsis"       # "ellipsis" | "space" | "line-break" | "list-items"
triple_click = "sense-with-examples"   # "sense" | "sense-with-examples" | "line"

[[anki.field_map]]          # one block per Anki field
anki_field = "Expression"
source = "expression"       # see the field-map table in the README

[actions]
enabled = true

[actions.screenshot]
hotkey = "ctrl+shift+s"     # Windows
# hotkey_linux = "ALT+S"    # Linux; absent leaves it unbound
save_dir = "screenshots"
include_on_add = false
capture_mode = "region"     # "region" | "window" | "fixed-region" | "fixed-window"
# fixed_region = [100, 200, 800, 600]  # optional; x, y, width, height in physical pixels
# [actions.screenshot.fixed_window]   # optional saved window target
# app_id = "org.example.Reader"
# title = "Japanese text"

# [actions.ocr_clipboard]   # absent by default; both keys optional
# hotkey = "ctrl+shift+c"       # Windows
# hotkey_linux = "ALT+C"        # Linux
```

### `capture_mode`

`capture_mode` selects the target that the screenshot action captures. The default is
`"region"`. The mode applies to screenshot-on-add and the standalone Mining screenshot.

| Value | Selection |
|---|---|
| `"region"` | Select a region on every screenshot. |
| `"window"` | Select a visible window on every screenshot. |
| `"fixed-region"` | Select a region once, then reuse its saved physical rectangle. |
| `"fixed-window"` | Select a window once, then resolve its saved identity before each capture. |

On Linux, `slurp` provides the interactive selector. Region mode supports a click on a
supplied window or a drag for an arbitrary region. A window metadata query supplies the
window rectangles for the click. If that query fails, region drag still works wherever
`slurp` and the layer-shell selector work. Window mode uses `slurp -r` and requires a
Hyprland `hyprctl` or Sway `swaymsg` window query.

On Windows, the native selector starts a region drag in region mode and a window click in
window mode. Hold Alt before you start the selection gesture to switch between these two
target types. The selector uses the window class as `app_id`.

The first fixed-region use asks for a region. On Linux, it always uses a drag without
window rectangles. The first fixed-window use asks for a window. A Windows Alt switch can
select the other target type for one capture, but it does not save that target in the
wrong fixed-target field.
After a first fixed-mode selection, chibipop saves the target in the config. Later
captures bypass interactive selection until you reset the target.

`fixed_region` stores `[x, y, width, height]` in global physical pixels. The width and
height must be positive. The saved rectangle remains unchanged until you reset it.

`fixed_window` stores the exact `app_id` and `title` pair. Both values must match one
visible window. A missing or ambiguous match fails instead of selecting another window.
The capture resolves fresh window geometry before each use, so a move or resize changes
the captured rectangle. A title change makes the saved identity absent. Reset the target
and select the window again.

Both settings windows show the mode, a saved-target summary, and a reset control. Press
Apply to commit a reset. The reset clears both saved target fields.

Window capture copies the visible screen rectangle. It does not copy hidden or occluded
window contents. Existing OCR and static-region selectors remain region-only.

On Linux, interactive selection has a 20-second timeout. On Windows, the selector
waits until you select or cancel. Press Esc to cancel. On Windows, you can also
right-click. If screenshot-on-add selection or capture fails, chibipop files the card
without an image. A standalone Mining screenshot saves nothing in that case.

### `sentence_mode`

`sentence` is the default. On add, chibipop hides the popup before a live
capture that cannot exclude it. It reads a strip six line heights above and
below the hovered word across the output. It cuts the sentence at `。！？`; a
`」` after a terminator stays. A line with a different size, such as ruby or a
heading, ends the sentence. A paragraph gap also ends the sentence. The probe
uses one or more tiles along the output's reading axis. It uses one OCR pass
for each tile. The tile count follows the output span, and each tile's band
follows the text thickness. The sentence probe runs no OCR pass on hover. A
held `hold-key` trigger reads the frozen grab without hiding the popup. Windows
ignores the hide flag because it excludes the popup from its own captures at the
OS level. When the probe fails, the sentence falls back to the hover-time
sentence.
Older configs without this key change to `sentence` when you upgrade. A saved
config writes every key, so a file that says `sentence_mode = "line"` keeps
`line` until you change it.

`line` uses the hovered OCR line with the same `。！？` cut at the cursor. A
wide page holds several sentences on one line, and the card gets only the
sentence of the hovered word. `line` never reads outside the hover capture, so
a sentence that starts on the line above is cut short.

In every mode, the sentence field bolds the word as it appears on screen:
`山が<b>崩れたり</b>、…`. The bold is the surface form, not the headword. A
sentence that does not contain the surface form stays plain.

The five default `[[anki.field_map]]` blocks are `Expression`/`expression`,
`ExpressionReading`/`reading`, `Glossary`/`glossary`, `Frequency`/`frequency`
and `FreqSort`/`frequency`.

### Selecting glossary text

Anki must be enabled before the popup can select glossary text.

- Dragging selects grapheme clusters.
- Double-clicking selects a word. A word is a full conjugation or a compound
  noun, for example 食べられませんでした or 日本語教師.
- Triple-clicking selects the unit set by `anki.triple_click`.
- `sense` selects a Sense without its examples.
- `sense-with-examples` selects a Sense and its examples. This value is the default.
- `line` selects the nearest block or the newline-delimited text line.
- Quadruple-clicking selects an Entry.
- A drag that starts from a double-click grows by words.
- A drag that starts from a triple-click grows by the configured triple-click unit.
- The Entry checkbox selects or clears the full Entry.
- The primary button adds to the selection by default.
- The secondary button replaces the selection by default.
- A plain primary click clears the current Card after 500 ms.

The selection replaces the `glossary` and `glossary_html` fields for that Card.
When no selection exists, the card keeps the full glossary.

**Every setting is read once at startup.** Edit the file with chibipop
stopped, or use the settings window — pressing Apply there rewrites the whole
file from memory, so it would overwrite a hand-edit made while it was
running. The write goes to a temp file and is renamed into place, so an
interrupted save cannot leave a half-written config.

Two Linux exceptions. `chibipop ctl reload` makes the running daemon re-read
the file, and `anki.static_region` is picked up the moment it is written, with
no restart and no Apply.

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

A hidden wrap costs one or more extra captures and OCR passes, independent of
`max_ocr_passes`. The probe runs only when pass 1 contains the line end, leaves
lookup capacity, and has no complete visible continuation. A continuation near pass 1's
leading edge can be clipped and does not suppress a probe.

For a short span, the probe reads one bounded region from the output margin to the
hovered line end. For a long span, it reads a deterministic sequence of regions
with half-tile overlap that covers the full span from the output margin to the
hovered line end. Each region is at most 1000 physical pixels along the
reading direction. Each region starts one quarter of a line before the hovered
line on the cross axis, and it reaches six line heights after it. A candidate
line joins as the continuation only when its center lies from half a line
through 4.5 lines on the cross axis.
For horizontal text, the candidate lies below the hovered line. For vertical
text, the candidate lies in the next column to the left. Its glyphs must also
be about the same size.

A probe stops at its first failed capture and keeps pass 1. With `max_ocr_passes` above 1,
tile text found past the box replaces the joined wrap, so a hidden wrap is most reliable
at 1. If a probe does not join a continuation, chibipop keeps pass 1. The
`show_scan_region` overlay draws each region as a tile.

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

### Inflection chain

The Card shows the conjugation and auxiliary steps between the headword and the
hovered text in one dimmed row under the reading. 食べさせられた shows
`causative « passive/potential « past`. The row lists the steps from the headword
outward, so the reader rebuilds the hovered form from left to right.

A bare te-form such as 風邪をひいて shows `(te form)`. The row hides inner stems such
as `(unstressed infinitive)`, because they are plumbing between two visible steps.
The labels are the `detail` strings of `data/deconjugator.json`, verbatim. An
unconjugated headword shows no row. No setting controls the row.

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

### The render settings

Six keys under `[popup]` decide how much of an entry you see. They are a
fixed decision table, not a style engine: the popup reads them in exactly
one place and there is deliberately no per-dictionary styling
configuration. All six are portable — both settings windows render them,
and neither drops the other platform's value.

`layout_mode` is `"roomy"` (the default) or `"compact"`. Roomy gives each
glossary item its own line, marked and indented, as a browser draws a
list. Compact joins the items into one paragraph with `"; "` between them,
which is the terse one-line-per-dictionary popup chibipop drew before it
could render structure — kept as a choice.

`dictionary_styling` is on by default. Off draws every entry in your
theme's own font, size, weight and colours, and draws no boxes: it
switches off both a node's inline `style` and the dictionary's own
`styles.css`. Markup still means what it says — a `<b>` stays bold —
because that is what the entry *says* rather than how the dictionary chose
to draw it. A dictionary whose sense numbering is a `list-style-type`
declaration (Jitendex's ①②③) falls back to the browser's own counter.

`show_examples` and `show_attributions` are separate knobs on purpose, so
you can keep a source line without keeping three sentences per sense. Both
are on by default. They filter on a node's classified editorial role;
until that classifier ships, the popup still hides the editorial matter it
always hid and these two knobs change nothing you can see.

`show_images` is on by default. An image node in a Japanese dictionary is
usually a *character* rather than an illustration, so off leaves the
image's `alt` text in place rather than a hole in the word, and removes
the picture and the space it took.

`show_part_of_speech` is **off** by default, and not for density: the
labels are already the popup's own field, drawn above the glosses, so
inline they would read twice. Turn it on to see a dictionary's own labels
where it put them.

Changing any of these applies on reload — the settings window's Apply — and
lands on a popup already on screen. No restart.

### `display_order`

**As of v0.7.1 this is the fallback**, used for any OCR language that has no
entry under `per_language`. With no `per_language` at all — the shipped state
— it is still what orders every lookup, exactly as before.

Matched by **name substring**, and the settings window says so. The TOML
stores case-insensitive substrings rather than full names, because a rebuilt
Jitendex changes its own name — the release date is part of it.

The window preserves whatever substring you already have, so reordering never
rewrites your entries. If one stops matching any installed dictionary it tells
you which, because the visible symptom otherwise is that dictionary quietly
sorting last.

Blank entries are ignored: an empty string is a substring of every name, so
one would silently pin the whole order.

**One consequence worth knowing before it surprises you.** Pressing Apply
while a language that *has* a list is on screen rewrites `display_order` from
that language's split, because the settings window shows one split of your
library — *Searched* then *Not searched* — and writes both the language's list
and `display_order` from it. So the popup ordering of a language with **no**
list can change
because of which language happened to be selected when you pressed Apply. It
is only an ordering, never a filter — nothing stops being searched — but if
you care about the fallback order, set it with an unscoped language selected.

### `per_language`

Each OCR recognizer language gets its own ordered dictionary list. A language
**with** an entry searches only those dictionaries, in that order; a language
with **no** entry searches everything by `display_order`, which is v0.7.0's
behaviour and still the default. Entries are matched by name substring,
exactly as `display_order` is.

Set this in the settings window — the **Dictionaries** tab is scoped to the
OCR language selected on **OCR / Debug**, and shows that language's list in a
**Searched** box with the rest in a **Not searched** box below it. Changing the
language re-scopes the tab immediately, before Apply.

**Move up** and **Move down** do both jobs: inside a box they set search
priority, and at the boundary they move a dictionary between the boxes — down
from the bottom of *Searched* stops it being searched, up from the top of *Not
searched* starts it again. There is no separate include/exclude control, and no
divider row; both were replaced by the two boxes in v0.7.2.

**The keys must match the recognizer tag exactly.** `ocr.language` itself is
matched loosely — `zh-Hans` selects an installed `zh-Hans-CN` — but these keys
are looked up by exact string equality, so `"zh-Hans"` here does **not** cover
a running `zh-Hans-CN` and its list is silently unused. The app always
*writes* the canonical tag, so this only bites a hand-edit. Configure the list
in the UI, or copy the tag the app wrote into the file.

**A list naming nothing installed is ignored, and everything is searched.**
A typo, or a dictionary you have not imported yet, therefore never silently
kills lookups — the fallback is "search everything", not "search nothing". The
Dictionaries tab shows the same thing the runtime does in that state (every
dictionary in *Searched*, *Not searched* empty), and that agreement is
deliberate: in that state the tab must not claim a scoping the lookups are not
applying. It holds however you arrive there, including switching the OCR
language to one whose list has gone stale. The same rule is why **excluding
everything is not representable**: an empty list means the same as no entry, so
a language whose list ends up matching nothing gets every dictionary back
rather than none.

**A list is applied only while its own recognizer is the one running.** If the
selected language's pack is not installed, chibipop goes on OCRing with a
different recognizer — at startup the fallback, which it names on stderr; on
Apply whichever language was already running, which it also reports — and the
list is then not applied at all: everything is searched by `display_order`,
exactly as for a language with no entry. Otherwise the popup would filter the fallback's hits through a
list written for a language that is not reading the screen, and come back empty
with no error at all. This is the one state where the Dictionaries tab does not
match the runtime, and deliberately: the tab keeps showing the list you
configured, because that is what you are editing, and the OCR language dropdown
already labels the tag `(not installed)`.

**A hand-written entry naming a not-yet-installed dictionary is replaced on
the next Apply.** Write `"ja" = ["Daijirin"]` before importing Daijirin, open
Settings, press Apply *for any reason at all*, and the entry becomes the
current library's list — the name you typed is gone. **Configure the list
after importing the dictionary.** Known limitation as of v0.7.1, and note that
`stale_order_entries` — which warns about a `display_order` entry matching
nothing — does not look at `per_language`, so nothing reports this to you.

**A language's list cannot be cleared from the settings window.** Once a
language has an entry, the window never removes the key: emptying *Not
searched* writes a full explicit list instead. That covers an entry an earlier
Apply in the same session wrote, not just one that was already in the file when
the window opened — after each Apply the window takes back what was written, so
the next Apply rewrites the key rather than dropping it. It behaves
identically for your current library, but differs later — an explicit list
will not pick up a dictionary imported after it was written, where no entry
would have. To return a language to "no entry", delete the key from the file
by hand with chibipop stopped.

**A dictionary moved back into *Searched* lands last.** It arrives at the
bottom of the box, and does not recover its former priority; carry on pressing
**Move up** to put it back.

### `show_scan_region`

The *debug* view: a faint outline around every region a hover captured —
pass 1's box, each forward tile, and the word it resolved. Turn it on when
OCR is behaving oddly and you want to see what it actually looked at rather
than infer it.

**Off by default**, and independent of `highlight_match`: with only the
highlight on you get one box, not four.

---

## Tests

Each host runs the workspace minus the foreign bin crate. The two numbers
differ because the two bin crates carry different tests, not because either
host skips core.

```bash
cargo test --workspace --exclude chibipop-linux     # Windows
cargo test --workspace --exclude chibipop-windows   # Linux
```

| Host | Passing | Ignored | Measured |
|---|---|---|---|
| Windows | **1339** across 13 targets | 3 | 2026-08-29, and CI at `98b133c` |
| Linux | **1591** | not recorded | CI at `98b133c`, 2026-08-27 |

> [!warning] One Windows target cannot pass off the CI runner image
> `geometry_golden_full_chrome` asserts DirectWrite metrics with **no
> tolerance**, against goldens blessed on `windows-2025`. On this
> development machine it fails on one field — `variants.default.elements.3.w`,
> golden `46.43` against measured `47.03`, the reading 「ざつだん」 in Yu
> Gothic UI. Nothing else in the suite diverges, and CI is green on the same
> commit. **This is font drift between two machines, not a regression, and it
> must not be blessed locally** — blessing here would red CI. See
> [`REGRESSION.md`](REGRESSION.md) tier 0 and [`BACKLOG.md`](BACKLOG.md) §37.

One of the Windows tests only runs where a dictionary does. `golden_corpus` grades
deconjugation against a real library and resolves one the way the product does
(`$CHIBIPOP_GOLDEN_DB`, then `$XDG_DATA_HOME/chibipop/chibipop.sqlite`, then
`data/chibipop.sqlite` in a cargo tree); with none of those present it prints
every path it looked at and returns, which libtest reports as a **pass**, not as
the one ignored test. So a total measured on a tree without a built database —
a fresh clone, or CI — includes one test that did not run. See
[`BACKLOG.md`](BACKLOG.md) §27.

Tier 0 of [`REGRESSION.md`](REGRESSION.md) is the authority on this number
and on what a change to it means — a *lower* total is the thing to explain.
Keep the two in step, in the same commit that moves either. This page was
missed on all three of the v0.7.0 round's earlier re-baselines
(670 → 698 → 710 → 726), and on every one before them: it read **416**
until the first two were caught up, then **710** while Tier 0 already
read **726**. The fourth, 726 → 729, moved both pages at once, as did the
fifth, 729 → 730, and so does 730 → 767 — the per-language dictionary lists
round, which added 37 tests across five tasks and two fix rounds and removed
none. 767 → 772 is that round's branch review being closed out: five tests
across its three fixes, and again none removed.

**772 → 794 is the first move on this page where a round deleted tests**: the
rebuild-promotion / two-box Dictionaries round added 33 and removed 11, and the
eleven all tested the divider row that the two-box tab replaced. Tier 0 carries
the arithmetic and the reason it is a re-baseline rather than a regression; do
not read the deletion out of this page's summary alone.

**794 → 873 is the v0.8.0 incremental-dictionary round**, nine tasks: 92 tests
added, 13 removed, net +79. The 13 went out with the rebuild-and-promote path
they tested — there is no promote left to decide about and no staged file left
to notice — so this is a re-baseline. Tier 0 carries the per-commit arithmetic,
and it is the per-commit split that is truthful here: a whole-branch
`git diff` collapses the deletions and reports `+82 −3`.

**873 → 1339 is four rounds, not one**, and this page recorded none of them
until 2026-08-29: the v1.0.0-rc round, the action system, the v0.9.x releases,
and the Linux port (PR #29). Tier 0 carries the per-round arithmetic. Two
things changed shape at the same time and are worth separating from the
count. The workspace split means "the total" is now **two** totals, one per
host. And Tier 0's own table read **1546 across 17 targets, 0 ignored** until
today, which matches neither host: `98b133c` measures 1339 on Windows and
1591 on Linux, on CI and on this machine alike.

**After any large change, work through [`REGRESSION.md`](REGRESSION.md)** — a
cheapest-first checklist: the automated gate, then what can be verified
against real pixels with `probe`, then the dozen things only a human at the
keyboard can check. It also lists the traps that have bitten more than once.

One target is deliberately **not** in that sweep:

```bash
cargo test -p chibipop-linux --test ocr_gate -- --nocapture
```

the Linux OCR quality gate
([`ARCHITECTURE.md`](../ARCHITECTURE.md#ocr-engine)), which runs the committed
152-crop benchmark corpus through the three bundled ONNX models and prints a
measured-vs-reference table. It is deterministic and it is the slowest single
target in the tree, so CI runs it once in its own step rather than three times
inside the loop above. Run it after any change under
`crates/chibipop-linux/src/ocr/`.

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

**Linux ships from v0.9.9**, the first release carrying a Linux asset. One
shared core, one bin crate per platform
([`ARCHITECTURE.md`](../ARCHITECTURE.md#workspace-and-seams)). The Wayland
build brings its own stack at five seams:

- [the capture ladder](../ARCHITECTURE.md#capture-and-masking)
- [the input ladder](../ARCHITECTURE.md#input-ladders)
- [the popup surface and renderer](../ARCHITECTURE.md#popup-and-measurement)
- [the settings process](../ARCHITECTURE.md#settings-and-config)
- [the OCR engine](../ARCHITECTURE.md#ocr-engine)

The decisions are in [`ARCHITECTURE.md`](../ARCHITECTURE.md). The measurements
behind them are in [`research/`](research/). Vertical text is beta there. See
the limits below.

### Known limits, measured rather than assumed

- **Vertical text does not work at the shipped capture shape.** Worse than
  reading short: the 500×100 box spans several columns, so it can return a
  sentence spliced out of unrelated ones. Measured 2/6 correct hovers; a
  transposed 100×500 probe scores 6/6. The fix has its own round.
- **Forward tiling is off** (`max_ocr_passes = 1`) because it sometimes
  resolved a different character than the one under the cursor. Both
  code-level causes (re-reading the hovered word, leading-edge spillover at a
  tile seam) are now fixed and unit-tested — see [`BACKLOG.md`](BACKLOG.md)
  §1. The default has **not** been changed: the fix has not been through a
  live, human-verified acceptance sweep, only `cargo test` in a
  non-interactive environment with no screen to hover.
- **Text clipped by a window edge cannot be read** at any capture shape. The
  glyphs are physically incomplete on screen; a ceiling, not a bug.
- **Vertical text is beta on Linux.** The Linux OCR engine (meikiocr)
  reads horizontal text at **1.8 % CER with 95.1 % hit-scan** and vertical text
  at **12.5 % CER with 81.3 % hit-scan** over the 152-crop benchmark corpus.
  Both are enforced as a CI gate — horizontal CER ≤ 5 % / hit-scan ≥ 90 %,
  vertical CER ≤ 20 % / hit-scan ≥ 75 % — so vertical is degraded, not broken:
  expect a dropped first glyph or trailing `。` on a column. The known
  mitigations are all unmeasured, so none ship, and **the UI says nothing about
  it**: a hover that reads is worth more than a warning. Re-run
  `cargo test -p chibipop-linux --test ocr_gate` after any model update. The
  Linux adapter never upscales a capture in any orientation — meikiocr measured
  1× better than 2× on every slice, because its fixed 960×544 detector
  letterbox undoes an upscale.
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
  synthetic mouse movement cannot reach a global low-level hook without the
  user granting input control — see REGRESSION.md tier 2.
