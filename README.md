# chibipop

A Japanese popup dictionary for the whole screen. Put the cursor on a
Japanese word, and chibipop shows what the word means.

chibipop reads the pixels of the screen. Thus, it works with a game, a
video, a PDF, or a picture. The other program does not need to cooperate.
chibipop runs on Windows and on Linux.

Hover Japanese text inside a popup to open a child popup. The parent popup
stays visible, so you can read a definition and keep your place.
**Look up words inside the popup** on the **Popup** tab enables or disables
this behavior.

**Dictionary search** opens from the tray menu or from **Open dictionary
search** on the **Dictionaries** tab. Type a Japanese or Chinese word, then
select a candidate to open its definition. The search windows use the popup
theme.

**Look up selected text** reads the text that you selected in another
application. Select text in a supported browser or editor, then press the
shortcut. This action does not use OCR or a browser extension. Set the
shortcut on the **Shortcuts** tab. A checkbox below the shortcut sends the
selected text to Sentence search instead of a popup. See the
[supported applications and limits](docs/REFERENCE.md#direct-search) for
Windows and Linux.

**Sentence search** opens from **Open sentence search** on the
**Dictionaries** tab. Paste a sentence, then click a word in the sentence
view. chibipop highlights the word and shows its dictionary candidates
below. The word boundaries come from your enabled dictionaries, including
Chinese entries.

The **Shortcuts** tab sets the key for each search. **Copy text from the
screen** reads a screen area that you drag and copies the text. Enable
**After copying, open sentence search** to send that text to Sentence
search.

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
8. [Changing how the popup looks](#8-changing-how-the-popup-looks)
9. [Changing the OCR engine](#9-changing-the-ocr-engine)
10. [Linux](#10-linux)
11. [Getting help](#11-getting-help)
12. [For developers](#12-for-developers)
13. [License](#13-license)

---

## 1. What you need

- **A computer with Windows 10 or 11**, or **Linux with a Wayland
  desktop**. See [Linux](#10-linux) if you are not sure what that means.
- **On Windows: Japanese language support.** Add it in Windows Settings >
  Time & language > Language & region. The OCR engine that is built into
  Windows reads the screen with this language pack.
- **At least one dictionary.** chibipop does not include one. The next
  section explains how to get one.

You do **not** need Python, a compiler, or another tool. The download
contains everything else. The optional meikiocr plugin on Windows is the
one exception. See [Changing the OCR engine](#9-changing-the-ocr-engine).

---

## 2. Get a dictionary

chibipop does not include a dictionary, and it cannot work without one.
Do this step first.

**What a dictionary is here.** One `.zip` file in **Yomitan format**.
Yomitan is a popular browser dictionary, and people share its files freely.
chibipop reads the same files. You do not need to install Yomitan.

**Where to start.** These three dictionaries are known to work:

| Dictionary | What it is | License |
|---|---|---|
| **Jitendex** | Japanese to English | free, CC BY-SA 4.0 |
| **大辞林 第四版** | Japanese to Japanese, from Sanseido | commercial |
| **jiten_freq_global** | word frequency data, see below | free |

Download the `.zip` files and keep them together in one folder. That
folder is your **library**. In the next section, you add the files from
this folder to chibipop.

**A frequency list is optional but recommended.** A frequency list tells
chibipop how common each word is. The popup then ranks the most likely
meaning first. chibipop reads the role of each archive from the archive
itself. You do not need to identify the frequency list.

---

## 3. Install chibipop

**Step 1 — Download.** Get the file for your system from
[Releases](../../releases).

| Your system | Download | Unpack it with |
|---|---|---|
| Windows | `chibipop-vX.Y.Z-windows-x64.zip` | right-click > Extract All |
| Linux | `chibipop-vX.Y.Z-linux-x64.tar.gz` | `tar xzf <file>` |

Put the unpacked folder where you want. On Arch Linux, you can install
`chibipop-bin` from the AUR instead. On Nix, you can run the flake. See
[Linux](#10-linux).

**Step 2 — Open the settings window.**

- **Windows:** double-click `chibipop.exe`. On a new install, the settings
  window opens because there is no dictionary database yet.
- **Linux:** run `chibipop settings`.

**Step 3 — Add your dictionaries.** The two platforms differ here.

- **Windows:** on the **Dictionaries** tab, click **Add…** and select the
  `.zip` files from your library. Click **Apply**. chibipop builds its
  database from the files. The status line says
  `Rebuilding your dictionary. This can take a few minutes.` When the build
  is complete, chibipop starts.
- **Linux:** on the **Dictionaries** tab, click **Browse…** and select the
  `.zip` files from your library. Or type a path and click **Add**. Then
  click **Rebuild**. The rebuild reads every archive, and it can take a few
  minutes.

**Step 4 — Read.** Put the cursor on Japanese text anywhere on the screen.
The popup follows the cursor. This is the default lookup mode,
**Follow pointer**. The **Shortcuts** tab has three other modes that use a
lookup key. See [Windows and Linux](#4-windows-and-linux). On Linux, a key
mode needs a bind in your desktop. [Linux](#10-linux) explains how.

If no popup appears, see [Getting help](#11-getting-help).

---

## 4. Windows and Linux

chibipop is one program with two builds. Most features work the same way.
This table shows the differences that matter before you start.

| | Windows | Linux |
|---|---|---|
| **Lookup modes** | **Follow pointer**, **While held**, **Turn on / off**, and **Once per press**. chibipop reads the lookup key itself. | The same four modes. Your desktop sends the lookup key to chibipop. |
| **OCR engine** | Windows OCR, built into Windows | **meikiocr**, bundled with chibipop |
| **Text languages** | any language with a Windows OCR language pack | Japanese only |
| **Other OCR engines** | yes, through plugins | no |
| **Dictionary changes** | **Apply** edits the database in place | **Rebuild** reads every archive again |
| **Settings file** | beside the program | `~/.config/chibipop/`, or beside the program in portable mode |
| **Updates** | chibipop can replace itself | chibipop reports a new version and never replaces itself |

Everything else in this guide applies to both platforms unless a section
says otherwise.

**OCR** means reading text from a picture of the screen. OCR lets chibipop
work with any program, including a game that shares no text with other
programs.

---

## 5. Settings

Use the settings window to change your preferences. Click **Apply** to
save. While chibipop runs, most changes take effect at once. A change of
the Windows OCR engine takes effect after a restart. On Windows,
`chibipop settings` opens the window without a running chibipop. There,
**Apply** saves the file, and the changes take effect when you start
chibipop.

On Windows, you can resize or maximize the settings window. The footer
shows the Apply state, the active OCR language and engine, and the Anki
state. **Show live logs** on the **Debug** tab opens the recent and live
log output in a second window. The X button of the settings window exits
chibipop. The X button of the log window closes only the log window.

**Where the file lives.** Your settings are in a file named
`chibipop.toml`. On Windows, the file is beside the program. On Linux, the
file is `~/.config/chibipop/chibipop.toml`. A `chibipop.toml` beside the
Linux program selects portable mode, and then every file stays beside the
program. You can edit the file manually. The settings window covers the
settings that your platform uses. See
[`docs/REFERENCE.md`](docs/REFERENCE.md#paths) for every option.

### The settings worth knowing

- **Screen area size** (**Text recognition** tab) — the area that chibipop
  reads around the cursor, in pixels. Linux names it **Capture width (px)**
  and **Capture height (px)**. **Read vertical text first** swaps the two
  numbers.
- **Read English letters and numbers** (**Text recognition** tab) — on by
  default. Disable it to ignore English words. Mixed text such as 「3人」
  still works.
- **Update for every character** — off by default. Enable it to look up
  each character as the cursor crosses it, instead of whole words. It works
  in **Follow pointer** mode only. Windows shows it on the
  **Text recognition** tab. Linux shows it on the **Shortcuts** tab.
- **Text language** (**Text recognition** tab) — **Windows only.** The
  language that the OCR engine reads. Add more languages in Windows
  Settings > Time & language > Language & region. Linux always reads
  Japanese.
- **Dictionaries for each language** (**Dictionaries** tab) —
  **Windows only.** When you change **Text language**, the
  **Definition dictionaries** list shows the dictionaries for that
  language. Each language keeps its own list and its own order. A language
  without a list searches every enabled dictionary.

---

## 6. Managing your dictionaries

**Windows.** Add or remove a dictionary on the **Dictionaries** tab, then
click **Apply**. chibipop edits its database in place while it runs. The
change takes effect quickly, and you can keep hovering. A frequency
list change updates the ranking in place too.

**Linux.** Add or remove a dictionary on the **Dictionaries** tab. The
change is staged. Click **Rebuild** to import the staged archives and
rebuild the database. The rebuild reads every archive again. The popup
keeps working during the rebuild.

**Frequency ranking on both platforms.** The ranking rule, the order of the
frequency lists, and their checkboxes control the ranks. A change to one of
them recalculates the ranks in place. chibipop does not read the archives
again for this change.

**The `build-dict` command (Windows only).** You can also build the
database from the command line:

```
chibipop.exe build-dict --library "<your library folder>" --out "<the database>"
```

You need this command in four cases only:

1. On a first install, if you do not want to use the settings window.
2. After a format upgrade of the database.
3. If the database is damaged.
4. To make the database agree with your library folder again, after you
   added or removed files outside chibipop.

Linux has no `build-dict` command. The **Rebuild** button in the settings
window does the same work.

---

## 7. Sending words to Anki

chibipop can make an Anki card from the word in the popup.

chibipop connects to Anki through
[AnkiConnect](https://ankiweb.net/shared/info/2055492159), a free Anki
add-on. Install AnkiConnect first, and keep Anki open. chibipop connects to
`http://localhost:8765` by default. **Connection address** on the **Anki**
tab changes this address.

### Enable Anki

1. Open the **Anki** tab.
2. Select **Enable Anki**.
3. Choose your **Deck** and your **Note type**.

On Windows, **Reload decks and fields** loads the latest decks, note types,
and fields from Anki.

### Choose the card fields

The **field map** matches each field of your Anki note to one value that
chibipop found. Set it on the **Anki** tab. Windows shows one dropdown for
each field of the note type under **Choose card fields**. Linux shows one
row for each mapped field.

| Value | What it contains |
|---|---|
| `expression` | the word |
| `reading` | the reading of the word |
| `glossary` | the numbered definitions as plain text, with an optional heading |
| `glossary_html` | the definitions with the formatting of the dictionary |
| `frequency` | how common the word is |
| `pitch_html` | the pitch accent of the word, as HTML |
| `sentence` | the sentence that contains the word |
| `screenshot` | a picture of what you read |

On Windows, `(none)` leaves a field empty.

A heading and a line separate the definitions of different dictionaries.
**Include the dictionary name** shows or hides these headings. A heading
uses HTML instead of square brackets, because Anki can read square brackets
as furigana.

**Use the first dictionary only** sends the definitions of the top
dictionary only. This setting keeps cards short when several dictionaries
match the same word.

The **Anki** tab also sets how you select text in the glossary.
**Primary click behavior** sets the primary mouse button to
`Add to selection` or `Replace selection`. The secondary button always adds
to the selection. **Join selected text with** sets the separator between
the selected fragments. The default is `Ellipsis (…)`.
**Triple-click selects** chooses a meaning, a meaning with examples, or a
complete line.

On Windows, a small notification confirms each card. **Notify after adding
a card** turns the notification off.

### Add a picture of what you read

A screenshot gives the card context — the panel, the subtitle, or the line
of the game where you found the word.

#### Choose a screenshot mode

Choose a mode with **Screenshot source** on the **Anki** tab.

- **Choose a region** is the default. You select a region for each picture.
- **Choose a window** selects one visible window for each picture.
- **Reuse one region** asks for one region on first use, then reuses its
  saved rectangle.
- **Reuse one window** asks for one visible window on first use, then finds
  that window for each picture.

On Windows, a drag selects a region, and a click selects a window. Hold
`Alt` before you start the gesture to switch between the two. On Linux,
`slurp` provides the selector. **Choose a region** accepts a region drag,
or a window click on Hyprland or Sway. **Choose a window** uses a window
click. A Linux window click needs the window list from Hyprland or Sway. On
another compositor, only a region drag works, and the window modes fail.

A saved window stores its exact `app_id` and title. Windows uses the window
class as `app_id`. Linux uses the class or `app_id` from the compositor.
Both values must match exactly one visible window. A window capture copies
the visible screen rectangle. It does not copy hidden window contents.

A saved region keeps the same rectangle after a restart. A saved window
gets fresh geometry after the window moves or changes size. A title change
breaks the match. Reset the saved target and select the window again.

To attach a picture to each card:

1. On the **Anki** tab, select **Attach a screenshot to cards**.
2. Choose a mode with **Screenshot source**, then click **Apply**.
3. Hover a word until the popup appears.
4. Press the **Add current result to Anki** key, or click the Anki button
   under the popup.
5. If the mode asks for a target, complete the drag or the click. The
   **Reuse** modes save the first successful selection.

The **Anki** tab shows the mode, a summary of each saved target, and a
reset button. Click **Clear saved targets** on Windows or
**Reset saved screenshot targets** on Linux. Click **Apply** to save the
reset. The next picture in a **Reuse** mode asks for a new target.

chibipop saves the picture and makes the card. The card needs a connected
Anki.

**To skip the picture,** press `Esc` while the screen is dimmed. On
Windows, a right-click also skips it. You still get the card without a
picture. On Linux, chibipop cancels the picture after 20 seconds without a
selection. Windows waits until you select or cancel.

**Where the picture is saved.** The default folder is `screenshots`. On
Linux, change it with the **Screenshots folder** box on the **Anki** tab.
On Windows, set `save_dir` under `[actions.screenshot]` in `chibipop.toml`.
A full path is used exactly as you type it. A plain name is placed:

- **beside the program** on Windows, and on Linux in portable mode
- **in `~/.local/share/chibipop/screenshots`** on Linux otherwise, or in
  `$XDG_DATA_HOME/chibipop/screenshots` if you set that variable

Anki copies each picture into its own media folder. The `screenshots`
folder keeps your local copies.

### Take a picture with a key

A separate key takes a screenshot for the popup that is on the screen. The
key uses the selected screenshot mode. chibipop saves the picture. If Anki
is connected, chibipop also files a card.

- **Windows:** set **Save a screenshot** on the **Shortcuts** tab. The
  default is `Ctrl+Shift+S`.
- **Linux:** set the **Screenshot shortcut** on the **Shortcuts** tab. The
  tab shows the bind line for your compositor. The bind runs
  `chibipop ctl screenshot`. See [`docs/LINUX.md`](docs/LINUX.md).

If no popup is on the screen, the key does nothing. Linux writes a line to
its log. There is nothing to photograph until you look up a word.

### Add the sentence

chibipop can also send the sentence around the word. Choose the source with
**Sentence source** on the **Anki** tab:

- **Detected sentence** — the complete sentence that contains the word.
  This is the default.
- **Line under the pointer** — the captured line that contains the word.
- **All captured lines** — everything that chibipop read around the cursor.
- **Fixed screen area** — a fixed part of the screen that you mark once.

#### Fixed screen area — for visual novels and games

Games and visual novels usually put their text in the same box every time.
Mark that box once, and chibipop reads the sentence from it instead of from
the area around the cursor.

1. Set **Sentence source** to **Fixed screen area**.
2. Press the **Set sentence area** shortcut. Set this shortcut on the
   **Shortcuts** tab. The screen dims.
3. Drag a rectangle around the text box, and release.
4. A teal outline marks the area.

**Outline the sentence area** on Windows, or **Show the static region
outline** on Linux, turns the outline off. chibipop saves the area, and the
area survives a restart. Press the shortcut again to move it.

---

## 8. Changing how the popup looks

A CSS file styles the popup. CSS is the language that web pages use. On
Windows, the popup and the search windows read `popup.css` beside
`chibipop.exe`. On Linux, Dictionary search and Sentence search read
`popup.css` beside the settings file. The Linux OCR popup does not read CSS
yet.

The repository has four ready-made themes in the [`themes/`](themes/)
folder: **midnight-purple**, **ocean-breeze**, **sakura-light**, and
**warm-paper**. The download does not include them.

On Windows:

1. On the **Popup** tab, click **Advanced popup style**.
2. Paste a theme, or make your own changes.
3. Click **Save & Apply**. The popup changes at once.

On Linux, copy a theme file to `popup.css` beside `chibipop.toml`. The
search windows use it on the next search.

Delete `popup.css` to return to the default style. The Windows editor also
has **Reset to Default**.

[`docs/CSS-THEMING.md`](docs/CSS-THEMING.md) lists everything that you can
style.

---

## 9. Changing the OCR engine

**Windows only.** By default, chibipop reads the screen with Windows OCR,
the engine that is built into Windows. You can replace it with a different
engine. The settings window calls the engine the **Text reader**, and it
calls a replacement an extension.

On Linux, there is nothing to change. The Linux build always uses
**meikiocr**, which comes with it. See [`docs/LINUX.md`](docs/LINUX.md).

A replacement engine runs as a separate program, a plugin. chibipop sends
it a picture. The plugin sends back the text lines. It can also send the
position of each word. chibipop does the rest — the dictionary, the popup,
and the highlight.

### Configure meikiocr

[meikiocr](https://github.com/rtr46/meikiocr) is a Japanese OCR engine
trained on game text. The download includes its plugin in
`plugins/meikiocr/` as the worked example, and chibipop finds the plugin
automatically. The plugin needs a Python installation that you provide.

1. **Install meikiocr.** Use the README of meikiocr. You need Python with
   meikiocr, OpenCV, and ONNX Runtime.
2. **Tell chibipop where it is.** On the **Text recognition** tab:
   1. Choose **meikiocr** in the **Text reader** dropdown.
   2. Click **Configure…**.
   3. Select your meikiocr installation folder.
3. **Click Apply, then restart chibipop.** chibipop creates the OCR engine
   once at startup.

If meikiocr cannot start, chibipop uses the built-in engine and prints the
reason to its log.

### Check which engine runs

The footer of the settings window shows the active engine, for example
`OCR: meikiocr`. To see which engine handled the latest lookup, select
**Show the active text reader** on the **Debug** tab and click **Apply**.
The status area of the settings window then names the engine.

To watch the messages of the engine, start chibipop from a terminal:

```powershell
.\chibipop.exe run 2>engine.log
Get-Content engine.log -Wait -Tail 20
```

**Show extension messages** on the **Debug** tab shows the last lines of
these messages in the settings window.

### Write your own plugin

A plugin is a folder inside `plugins/` that holds:

- `plugin.toml` — the name, version, protocol, command, and roles of the
  plugin
- a program or script that exchanges JSON over standard input and standard
  output, one message on each line

`plugins/meikiocr/adapter.py` is a working example. The protocol has two
methods: `hello` introduces the plugin, and `text/recognise` reads a
picture. Both sides ignore unknown fields. A plugin answers an unknown
method with an error. After three consecutive failed recognitions, chibipop
switches the plugin off until the next restart. One successful recognition
resets the count.

---

## 10. Linux

**Linux is supported from v0.9.9 onward.**

chibipop needs a **Wayland** desktop. Wayland is the modern display system
that most Linux distributions now use. **Hyprland** is the reference
compositor. **sway** and its relatives get the same support. **KDE Plasma**
works through the desktop portals. **GNOME** is best-effort: the popup
cannot appear on stock GNOME, because its compositor does not implement the
layer-shell protocol. The older **X11** is not a target.

### Installing

| How | What to do | Notes |
|---|---|---|
| Download | `tar xzf chibipop-vX.Y.Z-linux-x64.tar.gz` | nothing else to install |
| Arch Linux | install `chibipop-bin` from the AUR | the same build, through pacman |
| Arch, from source | install `chibipop` from the AUR | uses the ONNX Runtime of your distribution |
| Nix | `nix run github:stellarie/chibipop` | builds with the ONNX Runtime of nixpkgs |

The download needs glibc 2.39, libstdc++ 3.4.31, and a CJK font such as
Noto Sans CJK. `xdg-desktop-portal` and `pipewire` are optional. KDE and
GNOME use them for screen capture and for the lookup key. The OCR engine
and its models are inside the download. chibipop works without an internet
connection and downloads nothing on the first run.

### Nix

Run chibipop directly from the flake. The flake builds the Linux binary for
`x86_64-linux` and `aarch64-linux`. The package includes the OCR models,
the Japanese analysis model, the deconjugation rules, a desktop entry, and
a systemd user unit. It does not include dictionaries.

```bash
nix run github:stellarie/chibipop -- run
```

The default package uses the CPU ONNX Runtime. A CUDA package is also
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

Then import its Home Manager module and enable the program:

```nix
{ inputs, ... }:
{
  imports = [ inputs.chibipop.homeManagerModules.default ];

  programs.chibipop = {
    enable = true;
  };
}
```

To start chibipop with the graphical session, add the systemd option:

```nix
programs.chibipop = {
  enable = true;
  systemd.enable = true;
};
```

The systemd option is optional. You can start chibipop manually with
`chibipop run`. See [For developers](#12-for-developers) for the
development shell.

### CUDA with Home Manager

The Home Manager module uses the CPU package by default. To use the CUDA
package, set the `package` field in your `programs.chibipop` block:

```nix
programs.chibipop.package = inputs.chibipop.packages.${pkgs.system}.cuda;
```

The development shells are `nix develop` for CPU and `nix develop .#cuda`
for CUDA.

### The lookup key

Wayland does not let a program watch the keyboard in the background. This
is a deliberate security decision. Thus, **your desktop sends the key to
chibipop**. KDE and GNOME can assign a global shortcut through the desktop
portal. On another compositor, a keybind runs `chibipop ctl`, and this
command sends a verb to chibipop.

On Hyprland, two lines in your configuration set the default `ALT+F` for
the **While held** mode:

```
bind = ALT, F, exec, chibipop ctl trigger-down
bindr = ALT, F, exec, chibipop ctl trigger-up
```

[`extras/`](extras/) contains a ready-made `hyprland.conf`, and its README
has the same lines for sway. The **Shortcuts** tab writes the bind line for
the keys that you choose. It knows the syntax of Hyprland, Sway, Niri, KDE,
and GNOME.

### Two things to expect

- **Vertical text is beta.** The bundled engine reads horizontal text well.
  With the default capture area, a vertical column can return a sentence
  that is spliced from neighboring columns. Enable **Read vertical text
  first** on the **Text recognition** tab when you read vertical text. The
  setting swaps the capture width and height. Then expect a missing first
  character, or a missing `。` at the end of a column. The measured numbers
  are in
  [`docs/REFERENCE.md`](docs/REFERENCE.md#known-limits-measured-rather-than-assumed).
- **chibipop does not update itself.** **Check for updates** tells you
  that a new version exists, and it stops there. Update through your
  package manager, or download the new version.

### Everything else

[`docs/LINUX.md`](docs/LINUX.md) is the full Linux guide. It covers:

- how to start, and how to build from source
- how the lookup key works, and a Hyprland defect to know about
- support for each desktop, including KDE and GNOME
- where chibipop keeps your files
- the command line
- the differences from Windows
- what to do when something does not work

---

## 11. Getting help

Does something not work? Do you have an idea for chibipop? Open an
[issue](https://github.com/stellarie/chibipop/issues). Pull requests are
welcome too.

Give your version (`chibipop --version`), your system, and what you
pointed at.

---

## 12. For developers

**A build** needs [Rust](https://rustup.rs) (stable, with MSVC on Windows)
and nothing else. The Windows icon is a committed resource, so you do not
need the Windows SDK.

```bash
cargo build --release -p chibipop-windows    # Windows
cargo build --release -p chibipop-linux      # Linux
```

Both commands produce a binary named `chibipop`.

**One repository, two binaries.** The core library is the root package.
One binary crate for each platform lives in `crates/`. Both binaries have
the name `chibipop`. Thus, a command that spans both crates races two
linkers for one output path. Exclude the other platform:

```bash
cargo test --workspace --exclude chibipop-linux     # Windows
cargo test --workspace --exclude chibipop-windows   # Linux
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md#workspace-and-seams).

### The Linux regression suite

Install Python 3 and start Docker or Podman. Then run the suite from the
repository root:

```bash
python scripts/linux_container_regression.py
```

Use Podman explicitly, or repeat the complete schedule to find intermittent
failures:

```bash
python scripts/linux_container_regression.py --runtime podman
python scripts/linux_container_regression.py --loops 3
```

The runner mounts the repository read-only. Each loop copies the current
Git workspace, including local non-ignored changes, into a new container.
The default image is Ubuntu 24.04, and `--image` selects another image.
After the loop, the runner removes only the container that carries its own
label. `--keep-failed-container` keeps a failed container for inspection.

Results go to `linux-regression-artifacts/`. The directory contains JSON
and JUnit reports, command logs, package output, and compositor evidence.
Use `--artifacts-dir <directory>` to choose another location.

List the schedule, or inspect the generated commands without a container:

```bash
python scripts/linux_container_regression.py --list
python scripts/linux_container_regression.py --dry-run
```

This suite tests Linux inside containers, not virtual machines. See
[`docs/REGRESSION.md`](docs/REGRESSION.md) for the Windows checks and the
manual checks.

### The rest of the documentation

| Document | What is in it |
|---|---|
| [`docs/REFERENCE.md`](docs/REFERENCE.md) | every setting, the diagnostics, the tests, and the measured limits |
| [`docs/LINUX.md`](docs/LINUX.md) | the Linux build, in full |
| [`docs/CSS-THEMING.md`](docs/CSS-THEMING.md) | every selector that you can style |
| [`docs/REGRESSION.md`](docs/REGRESSION.md) | the checks that prove a build works, sorted by who can run them |
| [`docs/RELEASING.md`](docs/RELEASING.md) | how a release is made |
| [`docs/BACKLOG.md`](docs/BACKLOG.md) | known problems and deferred work, with the evidence |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | the architecture and every decision that the code cannot state itself |
| [`docs/research/`](docs/research/) | the measurements behind those decisions |

---

## 13. License

GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).

The Linux build includes the [meikiocr](https://github.com/rtr46/meikiocr)
text recognition models (`crates/chibipop-linux/models/meiki/`). The model
weights are **LGPL-3.0**, included unchanged as data files, and ONNX
Runtime is MIT. Both are compatible with the GPL. The details and the
original sources are in
[`models/meiki/LICENSE.md`](crates/chibipop-linux/models/meiki/LICENSE.md).

Both builds include the IPADIC model for Japanese analysis
(`data/ipadic/`). Its license is in
[`data/ipadic/COPYING`](data/ipadic/COPYING) and
[`data/ipadic/NOTICE`](data/ipadic/NOTICE).

The deconjugation rules (`data/deconjugator.json`) are public domain.

**Dictionaries are not included, and they are not ours to distribute.**
