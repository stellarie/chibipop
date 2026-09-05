# chibipop

A Japanese pop-up dictionary for your whole screen. Point at a Japanese
word and chibipop tells you what it means.

It reads the pixels, so it works on anything — a game, a video, a PDF, a
picture. Nothing needs to cooperate with it. It runs on Windows and on
Linux.

<img width="2560" height="1080" alt="image" src="https://github.com/user-attachments/assets/58834926-8563-4741-815a-94ab4c7d9c09" />

---

## Contents

1. [What you need](#1-what-you-need)
2. [Get a dictionary](#2-get-a-dictionary)
3. [Install chibipop](#3-install-chibipop)
4. [Windows and Linux](#4-windows-and-linux)
5. [Settings](#5-settings)
6. [Managing your dictionaries](#6-managing-your-dictionaries)
7. [Sending words to Anki](#7-sending-words-to-anki)
8. [Changing how the pop-up looks](#8-changing-how-the-pop-up-looks)
9. [Changing the OCR engine](#9-changing-the-ocr-engine)
10. [Linux](#10-linux)
11. [Getting help](#11-getting-help)
12. [For developers](#12-for-developers)
13. [Licence](#13-licence)

---

## 1. What you need

- **A computer running Windows 10 or 11**, or **Linux with a Wayland
  desktop**. See [Linux](#10-linux) if you are not sure what that means.
- **On Windows: Japanese language support.** Add it in Windows Settings >
  Language & region. chibipop reads the screen with the recogniser that
  comes with it.
- **At least one dictionary.** chibipop does not include one. The next
  section explains how to get one.

You do **not** need Python, a compiler, or any other tool. The download
contains everything else.

---

## 2. Get a dictionary

chibipop does not ship with a dictionary, and it cannot work without one.
Get this out of the way first.

**What a dictionary is here.** A single `.zip` file in **Yomitan format**.
Yomitan is a popular browser dictionary, and its files are shared freely.
chibipop reads the same files. You do not need to install Yomitan itself.

**Where to start.** These three are known to work:

| Dictionary | What it is | Licence |
|---|---|---|
| **Jitendex** | Japanese to English | free, CC BY-SA 4.0 |
| **大辞林 第四版** | Japanese to Japanese, from Sanseido | commercial |
| **jiten_freq_global** | word frequency data — see below | free |

Download the `.zip` files and keep them together in one folder. That
folder is your **library**. You will point chibipop at it in the next
section.

**A frequency list is optional but recommended.** It tells chibipop how
common each word is, so the pop-up can rank the likely meaning first.
chibipop spots a frequency list on its own — you do not have to say which
file it is.

---

## 3. Install chibipop

**Step 1 — download.** Get the file for your system from
[Releases](../../releases).

| Your system | Download | Unpack it with |
|---|---|---|
| Windows | `chibipop-vX.Y.Z-windows-x64.zip` | right-click > Extract All |
| Linux | `chibipop-vX.Y.Z-linux-x64.tar.gz` | `tar xzf <file>` |

Put the unpacked folder anywhere you like. On Arch Linux you can install
`chibipop-bin` from the AUR instead; see [Linux](#10-linux).

**Step 2 — open the settings window.**

- **Windows:** double-click `chibipop.exe`. The settings window opens by
  itself the first time.
- **Linux:** run `chibipop settings`.

**Step 3 — add your dictionaries.** On the *Dictionaries* tab, add the
`.zip` files from your library folder. Press **Apply**.

chibipop now builds its own database from those files. This takes about a
minute the first time. You only do it once.

**Step 4 — read something.** Point at Japanese text anywhere on screen.

- **Windows:** just hover. The pop-up follows your cursor.
- **Linux:** hold the trigger keys — `ALT+F` by default — while you hover.
  [Linux](#10-linux) explains why, and how to set that up.

If nothing appears, see [Getting help](#11-getting-help).

---

## 4. Windows and Linux

chibipop is one program with two builds. Almost everything works the same
way. These are the differences worth knowing before you start.

| | Windows | Linux |
|---|---|---|
| **How you trigger a lookup** | hover, or hold a key you choose | hold a key combination your desktop passes on |
| **Which OCR engine reads the screen** | the one built into Windows | **meikiocr**, bundled with chibipop |
| **Reading languages other than Japanese** | yes, any recogniser Windows has | Japanese only |
| **Other OCR engines** | yes, through plugins | no |
| **Where your settings file lives** | beside the program | `~/.config/chibipop/` |
| **Updating** | chibipop can replace itself | chibipop tells you, and never replaces itself |

Everything else in this guide applies to both unless it says otherwise.

**OCR** means reading text out of a picture of a screen. It is how
chibipop works on anything at all, including games that share no text with
other programs.

---

## 5. Settings

Everything is in the settings window. Press **Apply** to save. Your
changes take effect at once — chibipop does not restart, and you do not
lose the pop-up you were looking at.

**Where the file lives.** Your settings are in a file called
`chibipop.toml`. On Windows it sits beside the program. On Linux it is in
`~/.config/chibipop/`. You can edit it by hand, but the settings window
covers everything in it. See
[`docs/REFERENCE.md`](docs/REFERENCE.md#paths) for every option.

### The settings worth knowing

- **Capture width / height** — how large an area chibipop reads around
  your cursor, in pixels. Vertical mode swaps the two numbers.
- **Scan alphanumeric text** — on by default. Turn it off to ignore
  English words. Mixed text like 「3人」 still works either way.
- **Per-character lookup** (*OCR / Debug* tab) — off by default. Turn it
  on to look up every character as you move the cursor, rather than whole
  words. Live mode only.
- **OCR language** (*OCR / Debug* tab) — **Windows only.** Which language
  the recogniser reads. Add more languages in Windows Settings > Language
  & region. Linux always reads Japanese.
- **Per-language dictionary list** (*Dictionaries* tab) — give each
  reading language its own set of dictionaries, in its own order. A
  language you have not set up searches all of them.

---

## 6. Managing your dictionaries

**Adding and removing is instant.** chibipop edits its database in place.
Add a dictionary or remove one, press **Apply**, and it takes effect in
under a second. You can keep hovering while it works.

**Frequency lists are the one exception.** A frequency list ranks words
across *every* dictionary at once, so changing one means re-ranking
everything. chibipop rebuilds the database instead, which takes about a
minute. It tells you when this is about to happen.

You can also run that rebuild yourself:

```
chibipop build-dict --library "<your library folder>" --out "<the database>"
```

You need this in four cases, and no others:

1. Your first install, if you would rather not use the settings window.
2. After a format upgrade.
3. If the database is damaged.
4. To make the database match your library folder again, after adding or
   removing files outside chibipop.

---

## 7. Sending words to Anki

chibipop can make an Anki card from the word you are looking at.

It talks to Anki through
[AnkiConnect](https://ankiweb.net/shared/info/2055492159), a free Anki
add-on. Install that first, and leave Anki running.

### Turn it on

Open the *Anki* tab in Settings and tick the box. Choose your deck and
your note type.

### Decide what goes on the card

The **field map** matches each field of your Anki note to a piece of what
chibipop found. Set it on the *Anki* tab; the dropdown lists everything
below.

| Put this in a field | And you get |
|---|---|
| `expression` | the word itself |
| `reading` | how it is read |
| `glossary` | numbered definitions, with only the optional heading formatted |
| `glossary_html` | the definitions, with Dictionary formatting |
| `frequency` | how common the word is |
| `sentence` | the sentence the word came from |
| `screenshot` | a picture of what you were reading |

Definitions from different Dictionaries are separated by a heading and a line.
Use **Include dictionary name** to show or hide these headings. A heading uses
HTML instead of square brackets, which Anki can interpret as furigana.

Use **First dictionary only** to send only the top Dictionary's definitions.
This setting keeps cards short when several Dictionaries match the same word.

The *Anki* tab also sets how glossary selection works. The default makes the
primary button additive and the secondary button replacing. It joins selected
fragments with an ellipsis. Choose another button mode or separator when needed.

A small notification confirms each card. Turn it off with **Show
notification when a card is added**.

### Add a picture of what you were reading

A screenshot gives the card context — the panel, the subtitle, the line of
the game you found the word in.

1. **Turn it on.** *Anki* tab > tick **Include screenshot when adding**.
2. **Find a word.** Hover until the pop-up appears.
3. **Ask for the card.** Press the Anki key, or click the Anki button
   under the pop-up. The screen dims.
4. **Choose the area.** Drag a rectangle around what you want to keep, and
   release.

chibipop saves the picture and, if Anki is running, makes the card.

**To skip the picture,** press **Esc** while the screen is dimmed — or
right-click, on Linux. You still get the card, without an image. If you
decide nothing for 20 seconds, chibipop cancels the picture by itself.

**Where the picture is saved.** In a folder called `screenshots`. Change
it with the **Screenshots folder** box on the *Anki* tab. A full path is
used exactly as you type it. A plain name is placed:

- **beside the program**, on Windows, and on Linux in portable mode — that
  is, when a `chibipop.toml` sits beside the program;
- **in `~/.local/share/chibipop/screenshots`** on Linux otherwise — or
  under `$XDG_DATA_HOME/chibipop` if you have set that variable.

Keep that folder. It is not temporary — deleting it breaks the cards that
point at the pictures.

### Take a picture without making a card

A separate key takes a screenshot for the pop-up already on screen,
whether or not you asked for a card. chibipop saves the picture. If Anki
is running, it also files a card.

- **Windows:** set the key as `actions.screenshot.hotkey`.
- **Linux:** it is a key combination you set in your desktop, bound to
  `chibipop ctl screenshot`. The *Anki* tab writes the line for you to
  copy. See [`docs/LINUX.md`](docs/LINUX.md).

Press it with no pop-up on screen and chibipop says so in its log rather
than doing nothing quietly. There is nothing to take a picture *of* until
you have looked a word up.

### Add the sentence

chibipop can send the sentence around the word as well. Choose where it
comes from on the *Anki* tab:

- **Current line** — the line of text the word is on. The default, and the
  right answer most of the time.
- **All lines** — everything chibipop read around your cursor.
- **Static region** — a fixed part of the screen that you mark once.

#### Static region — for visual novels and games

Games and visual novels usually put their text in the same box every time.
Mark that box once and chibipop reads from it, instead of from wherever
your cursor happens to be.

1. Set the sentence source to **Static region** in Settings.
2. Press the **Region hotkey** you chose. The screen dims.
3. Drag a rectangle around the text box, and release.
4. A teal outline marks it.

The outline can be turned off with **Show capture region outline**. Your
region is saved and survives a restart. Press the hotkey again to move it.

---

## 8. Changing how the pop-up looks

The pop-up is styled with a CSS file, the same language web pages use.
Four ready-made themes come with chibipop, in the `themes/` folder:
**midnight-purple**, **ocean-breeze**, **sakura-light** and **warm-paper**.

1. In Settings, click **Customize CSS...** in the Pop-up group.
2. Paste in a theme, or make your own changes.
3. Click **Save & Apply**. The pop-up changes immediately.

Your version is saved as `popup.css`, beside `chibipop.toml`. Delete that
file to go back to the default.

[`docs/CSS-THEMING.md`](docs/CSS-THEMING.md) lists everything you can
style.

---

## 9. Changing the OCR engine

**Windows only.** By default chibipop reads the screen with the recogniser
built into Windows. You can swap in a different one.

On Linux there is nothing to change: the build always uses **meikiocr**,
which comes with it. See [`docs/LINUX.md`](docs/LINUX.md).

A replacement engine runs as a separate program. chibipop sends it a
picture; it sends back the text and the position of every character.
chibipop still does the rest — the dictionary, the pop-up, the highlight.

### Setting up meikiocr

[meikiocr](https://github.com/rtr46/meikiocr) is a Japanese OCR engine
trained on game text. It comes with chibipop as the worked example, and
chibipop finds it on its own.

1. **Install meikiocr.** Follow its own README. You need Python, with
   meikiocr, OpenCV and ONNX Runtime.
2. **Tell chibipop where it is.** In Settings, on the *OCR / Debug* tab:
   1. choose **meikiocr** in the **OCR engine** dropdown;
   2. click **Configure...**;
   3. pick any file inside your meikiocr folder.
3. **Restart chibipop.** This line confirms it worked:
   ```
   chibipop: OCR engine: meikiocr
   ```

If meikiocr cannot start, chibipop goes back to the built-in engine and
prints the reason.

### Checking which engine is running

Tick **Show which OCR engine is active** on the *OCR / Debug* tab and
press **Apply**. The status bar names it.

To watch the engine's own messages, start chibipop from a terminal:

```powershell
.\chibipop.exe run 2>engine.log
Get-Content engine.log -Wait -Tail 20
```

### Writing your own

A plugin is a folder inside `plugins/` holding:

- `plugin.toml` — its name, version, command and roles;
- a program or script that exchanges JSON over standard input and output,
  one message per line.

`plugins/meikiocr/adapter.py` is a working example. There are two
messages: `hello` to introduce itself, and `text/recognise` to read a
picture. Both sides ignore anything they do not recognise. A plugin that
fails three times is switched off until the next restart.

---

## 10. Linux

**Linux is supported from v0.9.9 onward.**

chibipop needs a **Wayland** desktop — the modern display system most
Linux distributions now use. **Hyprland** is the one it is developed
against, with **sway** and its relatives equally supported. **KDE Plasma**
works. **GNOME** mostly works. The older **X11** is not supported.

### Installing

| How | What to do | Notes |
|---|---|---|
| Download | `tar xzf chibipop-vX.Y.Z-linux-x64.tar.gz` | nothing else to install |
| Arch Linux | install `chibipop-bin` from the AUR | the same build, through pacman |
| Arch, from source | install `chibipop` from the AUR | uses your distribution's ONNX Runtime |
| Nix | `nix run github:stellarie/chibipop` | builds with nixpkgs' ONNX Runtime |

The download needs glibc 2.39, libstdc++ 3.4.31 and a Japanese font, and
nothing else. The OCR engine is inside it, so it works with no internet
connection and downloads nothing on first run.

### Nix

Run chibipop directly from the flake. It builds the Linux binary and includes
its models, dictionaries, desktop entry, and systemd user unit:

```bash
nix run github:stellarie/chibipop -- run
```

The default package uses CPU ONNX Runtime. A CUDA-enabled package is also
available:

```bash
nix run github:stellarie/chibipop#cuda -- run
```

To use chibipop from Home Manager, add the flake as an input:

```nix
inputs.chibipop = {
  url = "github:stellarie/chibipop";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then import its Home Manager module in and enable the program:

```nix
{ inputs, ... }:
{
  imports = [ inputs.chibipop.homeManagerModules.default ];

  programs.chibipop = {
    enable = true;
  };
}
```

To start chibipop automatically with the graphical session, use:

```nix
programs.chibipop = {
  enable = true;
  systemd.enable = true;
};
```

The systemd option is not required. You can start it manually with
`chibipop run`. See [For developers](#12-for-developers) for the development
shell.

### Using CUDA with Home Manager

The Home Manager module uses the CPU package by default. To use the CUDA
variant, set the `package` field in your existing `programs.chibipop` block:

```nix
programs.chibipop.package = inputs.chibipop.packages.${pkgs.system}.cuda;
```

The development shells are available as `nix develop` for CPU or `nix develop
.#cuda` for CUDA.

### Setting up the trigger key

Wayland does not let a program watch your keyboard in the background — a
deliberate security decision. So **your desktop sends chibipop the
signal**, rather than chibipop listening for it.

On Hyprland, two lines in your config set up the default `ALT+F`:

```
bind  = ALT, F, exec, chibipop ctl trigger-down
bindr = ALT, F, exec, chibipop ctl trigger-up
```

Ready-made snippets for other desktops are in [`extras/`](extras/), and
the settings window writes the right line for whatever keys you choose.

### Two things to expect

- **Vertical text is not as accurate as horizontal.** The bundled engine
  reads sideways text well and vertical columns less well. Expect an
  occasional missing first character. Measured numbers are in
  [`docs/REFERENCE.md`](docs/REFERENCE.md#known-limits-measured-rather-than-assumed).
- **chibipop will not update itself.** *Check for updates* tells you a new
  version exists and stops there. Update through your package manager, or
  download the new version.

### Everything else

[`docs/LINUX.md`](docs/LINUX.md) is the full Linux guide. It covers:

- getting started, and building from source;
- how the trigger key works, and a Hyprland quirk to know about;
- support for each desktop, including KDE and GNOME;
- where your files are kept;
- the command line;
- what differs from Windows;
- what to do when something does not work.

---

## 11. Getting help

Something not working, or an idea for what chibipop should do next? Open
an [issue](https://github.com/stellarie/chibipop/issues). Pull requests
are welcome too.

It helps to say which version you are on (`chibipop --version`), which
system, and what you were pointing at.

---

## 12. For developers

**Building** needs [Rust](https://rustup.rs) (stable, MSVC on Windows) and
nothing else. The Windows icon is a committed resource, so no Windows SDK
is required.

```bash
cargo build --release -p chibipop-windows    # Windows
cargo build --release -p chibipop-linux      # Linux
```

Both produce a binary called `chibipop`.

**One repository, two binaries.** The core library is the root package,
and one binary crate per platform lives in `crates/`. Because both are
called `chibipop`, a command that spans them races two linkers over one
output path. Exclude the other platform:

```bash
cargo test --workspace --exclude chibipop-linux     # Windows
cargo test --workspace --exclude chibipop-windows   # Linux
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md#workspace-and-seams).

### The rest of the documentation

| Document | What is in it |
|---|---|
| [`docs/REFERENCE.md`](docs/REFERENCE.md) | every setting, the diagnostics, the tests, the measured limits |
| [`docs/LINUX.md`](docs/LINUX.md) | the Linux build, in full |
| [`docs/CSS-THEMING.md`](docs/CSS-THEMING.md) | every selector you can style |
| [`docs/REGRESSION.md`](docs/REGRESSION.md) | the checklist that proves a build works, sorted by who can run it |
| [`docs/RELEASING.md`](docs/RELEASING.md) | how a release is cut |
| [`docs/BACKLOG.md`](docs/BACKLOG.md) | known problems and deferred work, with the evidence |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | the architecture and every decision the code cannot state itself |
| [`docs/research/`](docs/research/) | the measurements those decisions rest on |

---

## 13. Licence

GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).

The Linux build includes the [meikiocr](https://github.com/rtr46/meikiocr)
text recognition models
(`crates/chibipop-linux/models/meiki/`). Their weights are **LGPL-3.0**,
included unchanged as data files, and ONNX Runtime is MIT. Both are
compatible with the GPL. The details and the original sources are in
[`models/meiki/LICENSE.md`](crates/chibipop-linux/models/meiki/LICENSE.md).

The deconjugation rules (`data/deconjugator.json`) are public domain.

**Dictionaries are not included, and are not ours to distribute.**
