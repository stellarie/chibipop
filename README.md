# chibipop

A screen-wide Japanese pop-up dictionary. Hover any Japanese text on screen.
chibipop reads the pixels with OCR, looks the word up, and shows the
definition. It runs on Windows and on Wayland Linux.

<img width="2560" height="1080" alt="image" src="https://github.com/user-attachments/assets/58834926-8563-4741-815a-94ab4c7d9c09" />

---

## Requirements

- **Windows 10 or 11** with Japanese language support, **or Linux on a
  Wayland compositor**. See [Linux](#linux).
- **Your own dictionaries.** chibipop does not include any.
  See [Dictionaries](#dictionaries).

---

## Quick start

1. Download the asset for your platform from [Releases](../../releases).

   | Platform | Asset | Unpack |
   |---|---|---|
   | Windows | `chibipop-vX.Y.Z-windows-x64.zip` | unzip it anywhere |
   | Linux | `chibipop-vX.Y.Z-linux-x64.tar.gz` | `tar xzf` it anywhere |

   Arch users install `chibipop-bin` from the AUR instead. See
   [Linux](#linux).
2. Run it. On Windows, `chibipop.exe` opens the settings window on first
   launch. On Linux, open it yourself with `chibipop settings`.
3. Add your dictionary archives. Press **Apply**.
4. Hover Japanese text anywhere on screen. On Linux, hold the trigger chord
   while you hover.

Linux needs one more step, because the compositor owns the trigger key. See
[Linux](#linux).

---

## Settings

All settings are in the settings window. Press **Apply** to save and use
them immediately. chibipop does not restart.

### Dictionaries

chibipop edits the dictionary database in place. Add or remove a
dictionary and it takes effect in under a second. Hovering keeps working
during the import.

**Frequency lists** are the exception. They rank words across all
dictionaries, so they need a full rebuild:

```
chibipop build-dict --library "<library folder>" --out "<database>"
```

Run this command on first install, after a format upgrade, or when
the database is damaged.

### Key settings

- **Capture width / height** — the area chibipop reads around your cursor,
  in pixels. Vertical mode swaps the two values.
- **Scan alphanumeric text** — on by default. Turn it off to ignore English
  words. Mixed text like 「3人」 still resolves.
- **Per-character lookup** (*OCR / Debug* tab) — off by default. Turn it on
  to look up each character as you move the cursor. Live mode only.
- **OCR language** (*OCR / Debug* tab) — **Windows only.** The Windows
  recogniser language. Add more in Windows Settings > Language & region.
  Linux always reads Japanese with the bundled engine.
- **Per-language dictionary list** (*Dictionaries* tab) — each OCR language
  can have its own dictionary order. A language with no list searches all
  dictionaries.

Settings live in `chibipop.toml`. It sits beside the executable on
Windows, and under `~/.config/chibipop/` on Linux. See
[`docs/REFERENCE.md`](docs/REFERENCE.md#paths).

---

## Mining screenshot

Take a screenshot while mining a word. The screenshot saves to disk and
attaches to the Anki card as a context image. Windows and Linux both do
this, from the same rule in the shared core.

1. **Enable it.** Settings > *Anki* tab > check **Include screenshot when
   adding**.
2. **Mine a word.** Hover Japanese text until the popup appears.
3. **Ask for the card** - press the Anki add key, or click the Anki button
   under the popup. The screen dims. Drag a rectangle around the area you
   want to capture. Release to confirm.
4. chibipop saves the PNG and, if Anki is connected, creates a card with
   the word and the screenshot.

**Where the PNG lands.** The folder is `screenshots` by default, and the
*Anki* tab's **Screenshots folder** box changes it. An absolute path is
taken as typed. A relative one resolves per platform:

- **Windows**, and **Linux in portable mode** (a `chibipop.toml` beside the
  executable): beside the executable.
- **Linux otherwise**: under `$XDG_DATA_HOME/chibipop` (`~/.local/share/chibipop`
  when that is unset), so the default is
  `~/.local/share/chibipop/screenshots`. It is data, not cache - losing it
  breaks the card that points at it.

Press **Esc** during selection - or right-click, on Linux - to skip the
screenshot. The card is still created without an image. A selection left
undecided for 20 seconds cancels itself.

**Taking one on its own.** A separate key grabs a mining screenshot for
the popup already on screen, whether or not the add key was pressed: it
writes the PNG and, when Anki is serving, files a card for it. On Windows
that key is `actions.screenshot.hotkey`; on Linux it is a compositor bind
that runs `chibipop ctl screenshot`, offered as a copyable snippet by the
*Anki* tab (see [docs/LINUX.md](docs/LINUX.md)). Pressed with no popup up,
it says so in the log rather than doing nothing: there is no mining
context without a lookup on screen.

Map the screenshot to an Anki field with `source = "screenshot"` in
your field map. The Settings dropdown shows it.

---

## Sentence capture

chibipop can send the sentence around the mined word to Anki. Add
`source = "sentence"` to your field map. The Settings dropdown shows it.

Three capture modes are on the *Anki* tab:

- **Current line** — the OCR line that contains the hovered word. Default.
- **All lines** — every line the OCR captured around the cursor.
- **Static region** — a fixed screen area you define once. Best for visual
  novels and games with a fixed text box.

### Static region

Visual novels show text in the same place every time. Set a static region
and chibipop reads from that area instead of following the cursor.

1. Set the sentence mode to **Static region** in Settings.
2. Press the **Region hotkey** you set in Settings. The screen dims.
3. Drag a rectangle around the text box. Release.
4. A teal outline marks the active region. Toggle it with **Show capture
   region outline** in Settings.

The region saves to `chibipop.toml` and survives restarts. Press the
hotkey again to redraw it.

---

## CSS theming

Style the popup with a CSS file. Four themes ship in the `themes/` folder:
midnight-purple, ocean-breeze, sakura-light, and warm-paper.

1. Settings > click **Customize CSS...** in the Popup group.
2. Edit the CSS. Click **Save & Apply**.
3. The popup repaints immediately.

The file is `popup.css`, beside `chibipop.toml`. Delete it to revert.
See [`docs/CSS-THEMING.md`](docs/CSS-THEMING.md) for the selector reference.

---

## Anki integration

chibipop creates cards through [AnkiConnect](https://ankiweb.net/shared/info/2055492159).
Enable it on the *Anki* tab in Settings.

The **field map** controls what goes on each card. Available sources:

| Source | Content |
|---|---|
| `expression` | The looked-up word. |
| `reading` | The word's reading. |
| `glossary` | Definitions, plain text. |
| `glossary_html` | Definitions, HTML-formatted. |
| `frequency` | Word frequency rank. |
| `screenshot` | Context image from the mining screenshot. |
| `sentence` | The sentence around the word, from OCR. |

Each dictionary's definitions are separated by a header and a horizontal
rule. HTML glosses use numbered lists.

**First dictionary only** — check this on the Anki tab to send only the
top-priority dictionary's definitions. Keeps cards clean when multiple
dictionaries match.

A balloon notification confirms each card. Toggle it with **Show
notification when a card is added** on the Anki tab.

---

## OCR plugins

**Windows only.** chibipop uses Windows OCR by default. You can replace it
with a different engine through the plugin system. The Linux build has no
plugin host: it always uses the bundled meikiocr engine, which ships with
it. See [`docs/LINUX.md`](docs/LINUX.md).

A plugin runs as a separate process. chibipop sends it a screenshot. The
plugin returns text with per-character bounding boxes. chibipop handles
the popup, dictionary lookup, and highlight.

### Setting up meikiocr

[meikiocr](https://github.com/rtr46/meikiocr) is a game-trained Japanese
OCR engine. It ships with chibipop as a reference plugin. Discovery enables
it automatically when its manifest parses successfully.

1. **Install meikiocr.** Follow its README. You need Python with meikiocr,
   OpenCV, and ONNX Runtime.
2. **Configure the path.** Settings > **OCR / Debug** tab > select
   **meikiocr** from the **OCR engine** dropdown > click **Configure...** >
   pick any file in your meikiocr folder.
3. **Restart chibipop.** The startup line confirms the engine:
   ```
   chibipop: OCR engine: meikiocr
   ```

If meikiocr fails to load, chibipop falls back to the built-in engine and
prints why.

### Verifying the engine

Check **Show which OCR engine is active** on the *OCR / Debug* tab. Press
**Apply**. The status bar shows the active engine.

To stream adapter logs, start from a terminal:

```powershell
.\chibipop.exe run 2>engine.log
Get-Content engine.log -Wait -Tail 20
```

### Writing your own plugin

A plugin is a folder inside `plugins/` with:

- `plugin.toml` — name, version, command, and roles.
- An executable or script that speaks newline-delimited JSON on
  stdin/stdout.

See `plugins/meikiocr/adapter.py` for a working example. The protocol has
two methods: `hello` (handshake) and `text/recognise` (OCR). Both sides
ignore unknown fields. A plugin that fails three times is disabled for the
session.

---

## Troubleshooting

Head to [Issues](https://github.com/stellarie/chibipop/issues) or raise
a PR.

---

## Dictionaries

chibipop does not include dictionaries. Add your own Yomitan-format
archives. Frequency lists are detected automatically.

Tested with:

- **Jitendex** — free, CC BY-SA 4.0. Japanese-English.
- **大辞林 第四版** — Sanseido. Japanese-Japanese.
- **jiten_freq_global** — word frequency data.

## Linux

**Linux ships from v0.9.9.** It targets Wayland compositors that speak the
wlroots protocol family. **Hyprland is the reference compositor**, with sway
and friends first-class alongside it. KDE Plasma works through the desktop
portals. GNOME is best-effort. X11 is not a target.

Three ways to install:

| Route | Command | ONNX Runtime |
|---|---|---|
| Tarball | `tar xzf chibipop-vX.Y.Z-linux-x64.tar.gz` | inside the binary |
| AUR, binary | `chibipop-bin` | inside the binary |
| AUR, source | `chibipop` | the distro's `onnxruntime` |

The tarball needs glibc, libstdc++ and a CJK font, and nothing else. It
carries the OCR models, so it works with no network and no first-run
download.

Wayland has no global key observation, so the compositor's own keybind is the
trigger. Two lines bind the default `ALT+F` chord on Hyprland:

```
bind  = ALT, F, exec, chibipop ctl trigger-down
bindr = ALT, F, exec, chibipop ctl trigger-up
```

Two differences are worth knowing before you start:

- **OCR is the bundled meikiocr engine**, not Windows OCR. Vertical text is
  beta — see [`docs/REFERENCE.md`](docs/REFERENCE.md#known-limits-measured-rather-than-assumed).
- **chibipop never replaces its own binary.** *Check for updates* reports a
  newer release and stops there. Update with your package manager.

Everything else lives in [`docs/LINUX.md`](docs/LINUX.md): quick start, the
trigger key's design (and Hyprland's release-bind defect), per-compositor
support including KDE and GNOME, file locations, the command line,
differences from Windows, and troubleshooting. Ready-to-copy session
snippets live in [`extras/`](extras/).

---

## For developers

**Building from source** needs [Rust](https://rustup.rs) (stable; MSVC on
Windows) and nothing else — `cargo build --release -p chibipop-windows` on
Windows, `cargo build --release -p chibipop-linux` on Linux (both produce a
binary named `chibipop`). The executable's icon is a committed resource, so
no Windows SDK is required.

- [`docs/REFERENCE.md`](docs/REFERENCE.md) — config reference, diagnostics,
  tests, measured limits.
- [`docs/LINUX.md`](docs/LINUX.md) — the Linux build, in full.
- [`docs/CSS-THEMING.md`](docs/CSS-THEMING.md) — the popup's CSS selectors.
- [`docs/REGRESSION.md`](docs/REGRESSION.md) — the checklist that proves a
  build still works, tiered by who can run it.
- [`docs/RELEASING.md`](docs/RELEASING.md) — how to cut a release.
- [`docs/BACKLOG.md`](docs/BACKLOG.md) — deferred work with evidence.
- [`docs/adr/`](docs/adr/) — the decisions the code cannot state itself.
- [`docs/research/`](docs/research/) — the measurements the ADRs cite.

**One repository, two binaries.** The core library is the root package, and
one bin crate per platform sits under `crates/`. Both bin crates produce a
binary named `chibipop`, so a build or test that spans both races two
linkers over one path. Exclude the foreign crate:

```bash
cargo test --workspace --exclude chibipop-linux     # Windows
cargo test --workspace --exclude chibipop-windows   # Linux
```

See [ADR-0001](docs/adr/0001-workspace-and-platform-seams.md).

## Licence

GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).

The Linux build bundles the [meikiocr](https://github.com/rtr46/meikiocr) text
recognition models (`crates/chibipop-linux/models/meiki/`). Their weights are
**LGPL-3.0**, redistributed unmodified as data files, and ONNX Runtime is MIT —
both compatible with the GPL. Details and upstream links are in
[`models/meiki/LICENSE.md`](crates/chibipop-linux/models/meiki/LICENSE.md).

The deconjugation rules (`data/deconjugator.json`) are public domain.
Dictionaries are not included and not ours to distribute.
