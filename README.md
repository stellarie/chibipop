# chibipop

<img width="2560" height="1080" alt="image" src="https://github.com/user-attachments/assets/58834926-8563-4741-815a-94ab4c7d9c09" />

---

## Requirements

- **Windows 10 or 11** with Japanese language support.
- **Your own dictionaries.** chibipop does not include any.
  See [Dictionaries](#dictionaries).

---

## Quick start

1. Download the latest **`chibipop-vX.Y.Z-windows-x64.zip`** from
   [Releases](../../releases). Unzip it anywhere.
2. Run `chibipop.exe`. The settings window opens on first launch.
3. Add your dictionary archives. Press **Apply**.
4. Hover Japanese text anywhere on screen.

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
- **OCR language** (*OCR / Debug* tab) — the Windows recogniser language.
  Add more in Windows Settings > Language & region.
- **Per-language dictionary list** (*Dictionaries* tab) — each OCR language
  can have its own dictionary order. A language with no list searches all
  dictionaries.

Settings live in `chibipop.toml` beside the executable.

---

## Mining screenshot

Take a screenshot while mining a word. The screenshot saves to disk and
attaches to the Anki card as a context image.

1. **Enable it.** Settings > *Anki* tab > check **Include screenshot when
   adding**.
2. **Mine a word.** Hover Japanese text until the popup appears.
3. **Press the Anki add key.** The screen dims. Drag a rectangle around the
   area you want to capture. Release to confirm.
4. chibipop saves the PNG to `screenshots/` beside the executable. If Anki
   is connected, it creates a card with the word and the screenshot.

Press **Esc** during selection to skip the screenshot. The card is still
created without an image.

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
2. Press **R** (configurable). The screen dims.
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

The file is `popup.css` beside the executable. Delete it to revert.
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

Each dictionary's definitions are separated by a header and a blank line.

A balloon notification confirms each card. Toggle it with **Show
notification when a card is added** on the Anki tab.

---

## OCR plugins

chibipop uses Windows OCR by default. You can replace it with a different
engine through the plugin system.

A plugin runs as a separate process. chibipop sends it a screenshot. The
plugin returns text with per-character bounding boxes. chibipop handles
the popup, dictionary lookup, and highlight.

### Setting up meikiocr

[meikiocr](https://github.com/rtr46/meikiocr) is a game-trained Japanese
OCR engine. It ships with chibipop as a reference plugin, disabled by
default.

1. **Install meikiocr.** Follow its README. You need Python with meikiocr,
   OpenCV, and ONNX Runtime.
2. **Enable the plugin.** Settings > **Plugins** tab > check **meikiocr** >
   **Apply**.
3. **Configure the path.** Settings > **OCR / Debug** tab > select
   **meikiocr** from the **OCR engine** dropdown > click **Configure...** >
   pick any file in your meikiocr folder.
4. **Restart chibipop.** The startup line confirms the engine:
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

---

## For developers

Build from source with [Rust](https://rustup.rs) (stable, MSVC):

```
cargo build --release
```

No Windows SDK required. The icon is a committed resource.

- [`docs/REFERENCE.md`](docs/REFERENCE.md) — config reference, diagnostics,
  tests, measured limits.
- [`docs/RELEASING.md`](docs/RELEASING.md) — how to cut a release.
- [`docs/BACKLOG.md`](docs/BACKLOG.md) — deferred work with evidence.

## Licence

GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).

The deconjugation rules (`data/deconjugator.json`) are public domain.
Dictionaries are not included and not ours to distribute.
