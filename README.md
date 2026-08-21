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
