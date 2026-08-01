# chibipop

<img width="2560" height="1080" alt="image" src="https://github.com/user-attachments/assets/58834926-8563-4741-815a-94ab4c7d9c09" />

---

## What you'll need

- **Windows 10 or 11**, with Japanese language support added.
- **Your own dictionary files.** chibipop doesn't include any. See
  [Dictionaries](#dictionaries).

Setup takes about five minutes, and you only do it once. Nothing else to
install — no Python, no toolchain.

**Check your Japanese support first.** Paste this into PowerShell:

```powershell
[Windows.Media.Ocr.OcrEngine,Windows.Media,ContentType=WindowsRuntime] | Out-Null; [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages | ForEach-Object { $_.LanguageTag }
```

If `ja` appears in the list, you're ready. If it doesn't, open **Settings →
Time & language → Language & region**, add **日本語 (Japanese)**, and run the
check again.

---

## Setting it up

### 1. Download chibipop

Get the latest **`chibipop-vX.Y.Z-windows-x64.zip`** from the
[Releases page](../../releases), and unzip it anywhere you like — your
Documents folder is fine.

There's no installer. Everything chibipop needs lives in that one folder, and
it doesn't write anything outside it.

### 2. Add your dictionaries

Start chibipop. The settings window opens by itself.

### 3. Enjoy!

---

## Settings

Everything is in the settings window — you shouldn't need to edit any files.

| Setting | What it does |
|---|---|
| **Trigger** | *Live* shows definitions as you hover. *Hold Shift* only shows them while Shift is held. |
| **Theme** | Dark or light. Dark is the default, since most reading happens on dark screens. |
| **Font** | Which font the popup uses. Only fonts that can display Japanese are listed. |
| **Max height** | How tall the popup may grow, as a share of your screen. Longer entries scroll instead of growing past it. |
| **Summary length** | How much of each extra definition is shown on its one-line row. |
| **Box the word being defined** | Draws a faint outline around exactly the characters being defined, so you can see it picked the right word. |
| **Scroll long entries with the wheel** | Lets the wheel scroll the popup while your cursor is inside it. |
| **Hide the popup from screen capture** | Makes the popup invisible to screen recorders, screenshots and screen sharing — visible only to you. Off by default. |
| **Dictionaries** | Add and remove your dictionaries, and set their order — the one at the top is shown first. |
| **Frequency data** | Word-frequency lists, which rank the results. No order: a rank is a lookup, not a display order. |
| **OCR passes per hover** | Leave at 1. Higher reads further ahead but can pick the wrong character. |
| **Outline what each hover captured** | A diagnostic view showing where chibipop looked. Off by default. |

Pressing **Apply** saves your settings and restarts chibipop, which takes
about a fifth of a second. Your settings live in `chibipop.toml` beside
`chibipop.exe`, and you can edit that by hand if you prefer.

If you added or removed a dictionary, Apply rebuilds `data\chibipop.sqlite`
first and that takes a minute or so. Your old dictionary keeps working the
whole time, and stays in place untouched if the rebuild fails.

---

## If something doesn't work

Head over to [Issues](https://github.com/stellarie/chibipop/issues) ~

---

## Dictionaries

Add your own dictionaries - as this does not include any dictionaries at all.

The build these instructions were written against used:

- **Jitendex** — free, CC BY-SA 4.0. A good Japanese→English starting point.
- **大辞林 第四版** — © Sanseido. Japanese→Japanese.
- **jiten_freq_global** — word-frequency data, used to rank results.

Any Yomitan-format dictionary should work. Frequency lists are detected
automatically.

---

## For developers

**Building from source** needs [Rust](https://rustup.rs) (stable, MSVC) and
nothing else — `cargo build --release`. The executable's icon is a committed
resource, so no Windows SDK is required.

The configuration file reference, the diagnostic subcommands, how to run the
tests, and the measured limits all live in
[`docs/REFERENCE.md`](docs/REFERENCE.md). How releases are cut is in
[`docs/RELEASING.md`](docs/RELEASING.md).

Work that is deliberately not built yet, with the evidence behind each
decision, is in [`docs/BACKLOG.md`](docs/BACKLOG.md).

## Licence

chibipop is free software under the **GNU General Public License v3.0 or
later** — see [`LICENSE`](LICENSE). You may use, study, share and modify it;
if you distribute a modified version, it has to come with its source under
the same terms.

The bundled deconjugation rules (`data/deconjugator.json`) are public domain.
Dictionaries are not included and are not ours to give: you supply your own
Yomitan archives.

