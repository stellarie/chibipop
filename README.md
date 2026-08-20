# chibipop

<img width="2560" height="1080" alt="image" src="https://github.com/user-attachments/assets/58834926-8563-4741-815a-94ab4c7d9c09" />

---

## What you'll need

- **Windows 10 or 11**, with Japanese language support added.
- **Your own dictionary files.** chibipop doesn't include any. See
  [Dictionaries](#dictionaries).

---

## Setting it up

### 1. Download chibipop

Get the latest **`chibipop-vX.Y.Z-windows-x64.zip`** from the
[Releases page](../../releases), and unzip it anywhere you like.

### 2. Add your dictionaries

Start chibipop. The settings window opens by itself.

### 3. Enjoy!

---

## Settings

Everything is in the settings window — you shouldn't need to edit any files.

Pressing **Apply** saves your settings and starts using them straight away.
chibipop keeps running and the window stays open, so you can change something,
hover a word to see how it feels, and change it again.

That includes adding and removing dictionaries. **As of v0.8.0 chibipop edits
its dictionary database in place instead of rebuilding it**, so a change costs
about as much as the change itself — usually under a second, a couple of seconds
for a very large dictionary — rather than the minute-plus it used to take to
rebuild everything. **It does not restart, and it does not go deaf while it
works**: hovering keeps answering right through the import, and the new
dictionary starts answering the moment it lands. If a change cannot be made it
tells you and leaves everything as it was.

**Frequency lists are the exception.** A frequency list ranks words across every
dictionary at once, so adding or removing one really does need the whole
database rebuilt, and chibipop cannot do that to a database it is holding open.
It will tell you so and give you the command to run — quit chibipop, run it,
start chibipop again:

```
chibipop build-dict --library "<your library folder>" --out "<your database>"
```

You will also want that command for a first run, after upgrading to a build
with a new dictionary format, or if the database is ever damaged.

**Because changes are made in place, the database can drift from your library
folder** — for instance if you drop a `.zip` in by hand rather than using
**Add…**. chibipop notices and says so when you open the settings window,
naming what is missing from which side. Nothing is broken when it does; your
lookups keep working, and the dictionaries the database has keep answering. It
**never** starts a rebuild on its own — running the command above is your call.

Your settings live in `chibipop.toml` beside `chibipop.exe`, and you can edit
that by hand if you prefer.

A few worth knowing about:

- **Capture width / height** — how large an area chibipop reads around your
  cursor, in pixels. Wider takes in more of a long line; taller is more
  forgiving about exactly where you point. **Vertical mode swaps the two**, so
  the 500 × 100 that reads across becomes 100 × 500 reading down. Very small or
  very large values are pulled back to something workable, and the window tells
  you when it does that.
- **Scan alphanumeric text** — on by default. Turn it off and chibipop stops
  reacting to English words, so hovering a menu or a button does nothing.
  Japanese with numbers or Latin mixed into it still works: 「3人」 still looks
  up, with the 3 in place.
- **Look up each character as you hover** (on the *OCR / Debug* tab) — off by
  default. Normally the popup holds still while your cursor stays anywhere on
  the word it matched; turn this on and moving to the next character looks that
  character up instead. Handy for reading kanji one at a time. It only applies
  in **Live** trigger mode, so the checkbox is greyed out if you use a trigger
  key.
- **OCR language** (on the *OCR / Debug* tab) — which Windows recognizer reads
  the text. The languages you have installed are listed; if the one your config
  names is not among them it is still shown, marked **(not installed)**, so you
  can see what is set rather than having it silently changed. To add more, use
  Windows Settings → Time & language → Language & region. If the recognizer you
  chose is gone by the time chibipop next starts, it starts in Japanese instead
  of refusing to start, and says so on stderr.
- **A dictionary list per language** (on the *Dictionaries* tab) — each OCR
  language can have its own dictionaries in its own order, so switching to
  Chinese searches your Chinese dictionaries rather than your Japanese ones.
  The tab always shows the list for the language selected on *OCR / Debug*. **A
  language you have not given a list searches all of your dictionaries**, which
  is what everything does until you change it.

  The tab holds two lists, **Searched** and **Not searched**. **Move up** and
  **Move down** set the priority within a list, and at the top and bottom edges
  they carry a dictionary from one list to the other — move it down out of
  *Searched* to stop searching it, up out of *Not searched* to start again.

---

## OCR plugins

chibipop reads text with the Windows OCR engine by default. You can replace it
with a different engine through the plugin system.

### What a plugin does

A plugin runs as a separate process. chibipop sends it a screenshot. The plugin
returns the text it found, with a bounding box for each character. chibipop
handles everything else — the popup, the dictionary lookup, the highlight.

### Setting up meikiocr

[meikiocr](https://github.com/rtr46/meikiocr) is a game-trained Japanese
OCR engine. It ships with chibipop as a reference plugin, disabled by default.

**Step 1 — Install meikiocr.** Follow its own README. You need a Python
environment with meikiocr, OpenCV and ONNX Runtime installed.

**Step 2 — Enable the plugin.** Open the chibipop settings window. Go to the
**Plugins** tab. Check the box next to **meikiocr**. Press **Apply**.

**Step 3 — Point it at your installation.** Go to the **OCR / Debug** tab.
Select **meikiocr** from the **OCR engine** dropdown. Click **Configure…** and
pick any file inside your meikiocr folder. chibipop saves the path to the
plugin's `config.toml`.

**Step 4 — Restart chibipop.** The next start loads meikiocr. The startup line
confirms which engine is active:

```
chibipop: OCR engine: meikiocr
```

If meikiocr fails to load, chibipop falls back to the built-in engine and says
so:

```
chibipop: OCR plugin "meikiocr" failed, falling back to builtin: ...
```

### Verifying the engine

Open the settings window. Check **Show which OCR engine is active** on the
**OCR / Debug** tab. Press **Apply**. The status bar shows the active engine.

Check **Show adapter log in status bar** to see the plugin's last recognise
timing. For live streaming, start chibipop from a terminal and read stderr:

```powershell
.\chibipop.exe run 2>engine.log
Get-Content engine.log -Wait -Tail 20
```

Each hover produces a line like `[meikiocr-adapter] recognise 95ms 2 line(s)`.

### Writing your own plugin

A plugin is a folder inside `plugins/` beside `chibipop.exe`. It contains:

- `plugin.toml` — declares the plugin name, version, command, and roles.
- An executable or script that speaks newline-delimited JSON on stdin/stdout.

The meikiocr adapter (`plugins/meikiocr/adapter.py`) is a working example. The
protocol uses two methods: `hello` (handshake) and `text/recognise` (OCR). Both
sides ignore unknown fields, so the protocol can grow without breaking existing
plugins.

A plugin that fails three times in a row is disabled for the rest of the
session.

---

## If something doesn't work or if a game seems to be not compatible

Head over to [Issues](https://github.com/stellarie/chibipop/issues) or raise a PR!

---

## Dictionaries

Add your own dictionaries - as this does not include any dictionaries at all.

The build these instructions were written against used:

- **Jitendex** - free, CC BY-SA 4.0. A good Japanese→English starting point.
- **大辞林 第四版** - © Sanseido. Japanese→Japanese.
- **jiten_freq_global** - word-frequency data, used to rank results.

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

