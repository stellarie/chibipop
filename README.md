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
hover a word to see how it feels, and change it again. If you added or removed
a dictionary the button reads **Apply & Restart** instead — swapping the
dictionary out needs a restart, and that takes a couple of seconds.

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
  the text. Only the languages you actually have installed are offered; to add
  more, use Windows Settings → Time & language → Language & region.

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

