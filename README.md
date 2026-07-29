# chibipop

**Hover over Japanese text anywhere on your screen and see what it means.**

It works on any Japanese your screen can show — games, videos, subtitles,
PDFs, images, screenshots. The text doesn't need to be selectable or
copyable, because chibipop reads the pixels rather than the document.

No browser extension, no copying and pasting, nothing installed into the app
you're reading.

---

## What you'll need

- **Windows 10 or 11**, with Japanese language support added.
- **Python 3.9 or newer** — used once, to build your dictionary.
- **Your own dictionary files.** chibipop doesn't include any. See
  [Dictionaries](#dictionaries).

Setup takes about five minutes, and you only do it once.

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

### 2. Install Python

From [python.org](https://www.python.org/downloads/). Tick
**"Add Python to PATH"** during the install.

You need it for the next step only — chibipop itself doesn't use Python.

### 3. Build your dictionary

Put your Yomitan dictionary `.zip` files together in one folder. Then open a
terminal **in the chibipop folder** and run:

```bash
python tools\build-dict\build.py --dicts-dir "C:\Users\You\Documents\dicts" --out data\chibipop.sqlite
```

It takes about 70 seconds, and you only do it again when you add or update a
dictionary.

*(This step can't be skipped or shipped for you — the dictionaries are 232 MB
and aren't ours to hand out. See [Dictionaries](#dictionaries).)*

---

## Using it

From a terminal opened **in the chibipop folder**, so it can find the
dictionary you built:

```bash
chibipop.exe run
```

*(To start it with a double-click instead: right-click `chibipop.exe` → **Send
to → Desktop (create shortcut)**, then right-click the new shortcut →
**Properties**, and add a space and the word `run` to the end of the Target
box.)*

Then just **hover over Japanese text**. The definition appears beside it.

A few things worth knowing:

- **The settings window opens by itself** when chibipop starts. Close it or
  press Cancel — hovering works normally underneath.
- **You can move your mouse into the popup** to read it. It stays put while
  you're inside it, and disappears when you move away.
- **Long entries scroll.** A thin bar appears on the right when there's more
  to read; the wheel scrolls it.
- **Reading a whole word works.** Hovering any character of 振り向けた shows
  one definition for the whole verb, not a different one per character.
- **To quit**, press **Quit chibipop** in the settings window.

To open settings again later without starting the popup:

```bash
chibipop.exe settings
```

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
| **Dictionaries** | Drag the order around. The one at the top is shown first. |
| **OCR passes per hover** | Leave at 1. Higher reads further ahead but can pick the wrong character. |
| **Outline what each hover captured** | A diagnostic view showing where chibipop looked. Off by default. |

Pressing **Apply** saves your settings and restarts chibipop, which takes
about a fifth of a second. Your settings live in `chibipop.toml` beside
`chibipop.exe`, and you can edit that by hand if you prefer.

---

## If something doesn't work

| What you're seeing | What's going on |
|---|---|
| **No popup appears at all** | Check `ja` shows up in the language check at the top. If chibipop won't start, it prints the reason — most often the dictionary hasn't been built yet. |
| **Vertical text gives nonsense** | Vertical Japanese isn't supported yet. It can return a sentence stitched together from several unrelated columns. Being worked on. |
| **Right-clicking the tray icon does nothing** | A known bug. Everything the menu offered is reachable anyway: settings open at startup, `chibipop settings` opens them any time, and Quit is a button in that window. |
| **Text at the edge of a window won't read** | If the characters are physically cut off on screen, nothing can recover them. This is a limit, not a bug. |
| **It defined a similar-looking word** | OCR occasionally misreads a character. Nudging the cursor a few pixels and hovering again usually fixes it. |
| **The popup covers what I'm reading** | It's placed below and to the left of the word, flipping when it would run off screen. If it still gets in the way, lower **Max height**. |
| **chibipop won't close** | Press **Quit chibipop** in the settings window. If that's gone, end the `chibipop` process in Task Manager. |

---

## Dictionaries

chibipop ships **no dictionaries** — you supply your own, and they stay on
your machine.

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

Design specs, plans and verification findings are in `docs/superpowers/`;
work that is deliberately not built yet, with its evidence, is in
[`docs/BACKLOG.md`](docs/BACKLOG.md).
