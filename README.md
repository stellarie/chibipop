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

Pressing **Apply** saves your settings and restarts chibipop, which takes
a couple of seconds. Your settings live in `chibipop.toml` beside
`chibipop.exe`, and you can edit that by hand if you prefer.

---

## If something doesn't work

Head over to [Issues](https://github.com/stellarie/chibipop/issues)!

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

