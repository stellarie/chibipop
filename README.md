# chibipop

A Japanese popup dictionary for the whole screen. Put the pointer on a
Japanese word, and chibipop shows what the word means.

chibipop reads screen pixels through **optical character recognition
(OCR)**. It works with games, videos, PDFs, and images on Windows and Linux.

<img width="2560" height="1080" alt="chibipop shows Japanese and English definitions over a Japanese game menu" src="https://github.com/user-attachments/assets/58834926-8563-4741-815a-94ab4c7d9c09" />

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
- **On Windows: Japanese OCR language support.** Install Japanese through
  Windows Settings > Time & language. Make sure the OCR feature is installed.
- **At least one definition dictionary.** chibipop does not include one.
  The next section explains how to get one.

Linux needs the [system packages listed below](#10-linux).
The optional Windows meikiocr plugin needs Python. See
[Changing the OCR engine](#9-changing-the-ocr-engine).

---

## 2. Get a dictionary

chibipop needs at least one definition dictionary to show word meanings.
Get a dictionary before you start chibipop.

Each dictionary is a `.zip` file in **Yomitan format**. chibipop reads
these files directly. You do not need to install Yomitan.

**Where to start.** Choose a definition dictionary from this table.

| Dictionary | Language | License |
|---|---|---|
| **[Jitendex](https://jitendex.org/pages/downloads.html)** | Japanese to English | CC BY-SA 4.0 |
| **大辞林 第四版** | Japanese to Japanese, from Sanseido | commercial |

For Jitendex, download **Jitendex for Yomitan**, not the MDict version.
Keep your dictionary `.zip` files together in a download folder. Do not
extract them. The next section explains how to import the files into
chibipop.

**Frequency data is optional.** A frequency dictionary reports how common
each word is. chibipop uses this data to rank matching words, not to choose
the correct meaning for a sentence.

The **[jiten_freq_global](https://jiten.moe/frequency-dictionaries)** list
provides frequency data under CC BY-SA 4.0. Download the **Global** list in
Yomitan format. This list supplements a definition dictionary. It does not
replace one. chibipop detects each archive's roles (definition, frequency, pitch)
automatically. A dictionary archive can fill multiple roles, but most are made 
for a single role.

---

## 3. Install chibipop

### Download

Get the file for your system from [Releases](../../releases).

| Your system | Download | Unpack it with |
|---|---|---|
| Windows | `chibipop-vX.Y.Z-windows-x64.zip` | right-click > Extract All |
| Linux | `chibipop-vX.Y.Z-linux-x64.tar.gz` | `tar xzf <file>` |

Extract the download to a folder where your account can write files.
On Nix, you can use the [flake](#nix) instead.

### Windows

1. Double-click `chibipop.exe` in the extracted folder.
2. On the **Dictionaries** tab, click **Add…**.
3. Select the dictionary `.zip` files from your download folder.
4. Click **Apply**.

On a new install, chibipop opens settings because no dictionary database
exists. The first build can take a few minutes. chibipop starts lookup
after the build completes.

### Linux

The commands below assume that `chibipop` is on your `PATH`. For the
release download, open a terminal in the extracted folder. Use
`./chibipop` instead of `chibipop`.

1. Run `chibipop settings`.
2. On the **Dictionaries** tab, click **Browse…**.
3. Select the dictionary `.zip` files from your download folder.
4. Click **Rebuild**.
5. After the rebuild completes, close the settings window.

The settings window does not start screen lookup. Run `chibipop run` in
the terminal to start it. Keep this terminal open while you use chibipop.
For automatic startup and portal shortcuts, see the [Linux guide](docs/LINUX.md).

### Read a word

Put the pointer on Japanese text on the screen. In the default **Follow
pointer** mode, chibipop looks up text as you move the pointer.

The **Shortcuts** tab has three other modes that use a lookup key.
See [Windows and Linux](#4-windows-and-linux). On Linux, a lookup
shortcut must be configured differently depending on your desktop
environment/compositor. [Linux](#10-linux) explains how to configure it.

If no popup appears, see [Getting help](#11-getting-help).

---

## 4. Windows and Linux

chibipop is one program with two builds. Most features work the same way.
This table shows the differences that matter before you start.

| | Windows | Linux |
|---|---|---|
| **Lookup modes** | **Follow pointer**, **While held**, **Turn on / off**, and **Once per press**. chibipop reads the lookup key itself. | The same four modes, but your desktop must send the lookup key to chibipop. |
| **Default OCR engine** | Windows OCR, built into Windows | **meikiocr**, bundled with chibipop |
| **OCR languages** | languages supported by the selected engine. Windows OCR needs an installed OCR language pack. | Japanese |
| **Other OCR engines** | yes, through plugins | not currently |
| **Dictionary changes** | **Apply** edits the database in place | **Rebuild** reads every archive again |
| **Settings file** | beside the program | `~/.config/chibipop/`, or beside the program in portable mode |
| **Updates** | chibipop can replace itself | chibipop reports a new version and never replaces itself |

Everything else in this guide applies to both platforms unless a section
says otherwise.

---

## 5. Settings

Use the settings window to change your preferences. Click **Apply** to
save. While chibipop runs, most changes take effect at once. A change of
the Windows OCR engine takes effect after a restart.

On Windows, `.\chibipop.exe settings` opens standalone settings. **Apply** saves
preferences for the next start. Use settings in the running application for
dictionary and frequency changes.

**Show live logs** on the **Debug** tab opens recent and live log output.
The X button of the settings window exits chibipop. The X button of the
log window closes only the log window.

**Settings file.** chibipop saves your settings in `chibipop.toml`.
On Windows, the default location is beside the program. On Linux, it is
`$XDG_CONFIG_HOME/chibipop/chibipop.toml`, usually
`~/.config/chibipop/chibipop.toml`.

On Linux, a `chibipop.toml` beside the program selects portable mode unless
you pass `--config`. Portable mode keeps data and logs beside the program.
The instance lock and control socket still use `$XDG_RUNTIME_DIR/chibipop`.

You can edit the file manually. The settings window covers most options.
See the [configuration reference](docs/REFERENCE.md#configuration-file)
for more options.

### Important settings

- **Screen area size** (**Text recognition** tab) — the area that chibipop
  reads around the pointer, in pixels. Linux names it **Capture width (px)**
  and **Capture height (px)**. **Read vertical text first** swaps the two
  numbers.
- **Read English letters and numbers** (**Text recognition** tab) — on by
  default. Disable it to ignore English words. Mixed text such as 「3人」
  still works.
- **Update for every character** — off by default. Enable it to look up
  each character as the pointer crosses it, instead of whole words. It works
  in **Follow pointer** mode only. Windows shows it on the
  **Text recognition** tab. Linux shows it on the **Shortcuts** tab.
- **Text language** (**Text recognition** tab) — **Windows OCR only.**
  This setting selects an installed OCR language. A plugin uses its own
  language and disables this control. Linux uses Japanese OCR.
- **Dictionaries for each language** — **Windows only.** Separate language
  lists require entries under `[dictionaries.per_language]` in
  `chibipop.toml`. The **Definition dictionaries** list edits an existing
  language list when you change **Text language**. Without a language
  list, chibipop uses the global definition dictionary list.

### Other ways to search

#### Dictionary search

1. Open **Dictionary search** from the tray menu or the **Dictionaries** tab.
2. Type a word or expression from an enabled dictionary.
3. Select a candidate to open its definition.

The search windows use the popup theme.

#### Selected text

**Look up selected text** reads a selection without OCR.
See the [supported applications and limits](docs/REFERENCE.md#direct-search) for Windows
and Linux.

On Linux, this action needs PRIMARY selection support through a data-control
protocol.

1. Set **Look up selected text** on the **Shortcuts** tab.
2. Select text in a supported browser or editor.
3. Press the shortcut keys.

The checkbox below the shortcut sends the selected text to Sentence search
instead of a popup.

#### Sentence search

1. Click **Open sentence search** on the **Dictionaries** tab.
2. Paste a sentence.
3. Click a word in the sentence view.

chibipop highlights the word and shows its dictionary candidates below.
The word boundaries come from enabled definition dictionaries.

#### Copy screen text

**Copy text from the screen** reads text from a region that you select and
copies it to the clipboard. Set its shortcut on the **Shortcuts** tab.
Enable **After copying, open sentence search** to open the captured text in
Sentence search.

On Linux, copying needs clipboard access through a data-control protocol.
See the [Linux clipboard requirements](docs/LINUX.md).

---

## 6. Managing your dictionaries

chibipop copies imported archives into its own **library** folder. The
original files in your download folder stay unchanged.

**Windows.** Change the dictionary list on the **Dictionaries** tab.
Click **Apply** to update the database in place. You can continue to use
the popup during the update. A frequency dictionary change also updates
the ranking.

**Linux.** Change the dictionary list on the **Dictionaries** tab.
Click **Rebuild** to import the listed archives. The rebuild reads every
archive again. The popup keeps working during the rebuild.

**Frequency ranking on both platforms.** The ranking rule, frequency
dictionary order, and enabled state control the ranks. A change to these
settings recalculates the ranks from data already in the database.
chibipop does not read the archives again.

On Windows, change frequency settings in the running application.
Standalone settings saves these preferences but does not recalculate the
stored ranks.

**The `build-dict` command (Windows only).** Quit chibipop before you
replace its active database. Windows cannot replace a database that
chibipop still has open.

On Windows, the library folder is `library` beside `chibipop.exe`.
For a first command-line build, put your dictionary archives in this folder.

Run this command from the folder that contains `chibipop.exe`:

```powershell
.\chibipop.exe build-dict --library ".\library" --out ".\data\chibipop.sqlite"
```

If you start chibipop with a custom `--dict` path, use that path for
`--out` too.

A full rebuild is useful in these cases:

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
add-on. Install AnkiConnect before you configure chibipop. Keep Anki open
while you configure it or add cards. chibipop connects to
`http://localhost:8765` by default. **Connection address** on the **Anki**
tab changes this address.

### Enable Anki

1. Open the **Anki** tab.
2. Select **Enable Anki**.
3. Choose your **Deck**.
4. Choose your **Note type**.
5. Click **Apply**.

On Windows, **Reload decks and fields** loads the latest decks, note types,
and fields from Anki.

### Choose the card fields

The **field map** matches each field of your Anki note to one value that
chibipop found. Set it on the **Anki** tab. Windows shows one dropdown for
each field of the note type under **Choose card fields**. Linux shows one
row for each mapped field.

Map at least one field to `expression`. chibipop cannot add a card without
this mapping.

| Value | What it contains |
|---|---|
| `expression` | the word |
| `reading` | the reading of the word |
| `glossary` | numbered definitions with basic HTML separators and an optional heading |
| `glossary_html` | the definitions with the formatting of the dictionary |
| `frequency` | how common the word is |
| `pitch_html` | the pitch accent of the word, as HTML |
| `sentence` | the sentence that contains the word |
| `screenshot` | a screenshot of what you read |

Click **Apply** after you change the field map. To add the current word,
click the Anki button under the popup. The **Add current result to Anki**
shortcut performs the same action.

A heading and a line separate the definitions of different dictionaries.
**Include the dictionary name** shows or hides these headings.

**Use the first dictionary only** sends the definitions of the top
dictionary only. This setting keeps cards short when several dictionaries
match the same word.

### Update matching duplicate notes

**Update matching duplicate notes** is off by default. When it is off,
Anki keeps its existing duplicate rejection behavior.

When it is on, chibipop resolves the note type's first field at write time.
It updates one exact matching note across the note type, even when that note
is outside the selected deck. Multiple exact matches fail without adding or
changing a note. A missing match uses the normal new-note path.

An update replaces only fields in the field map. Unmapped fields, tags,
cards, scheduling, and deck placement stay unchanged. A new screenshot
replaces the mapped screenshot field. A pictureless update clears that field.

Do not view the target note in Anki Browser during an update. AnkiConnect has
no atomic find-and-update request, so a timed-out write can have an uncertain
result. A stored screenshot can remain unused if the field update fails.

The **Anki** tab also sets how you select text in the glossary.
**Primary click behavior** sets the primary mouse button to
`Add to selection` or `Replace selection`. The secondary button always adds
to the selection. **Join selected text with** sets the separator between
the selected fragments. The default is `Ellipsis (…)`.
**Triple-click selects** chooses a meaning, a meaning with examples, or a
complete line.

On Windows, a small notification confirms each card. Clear **Notify after
adding a card** to disable these notifications.

### Attach a screenshot

A screenshot records the panel, subtitle, or game text where you found the
word.

#### Choose a screenshot mode

Choose a mode with **Screenshot source** on the **Anki** tab.

- **Choose a region** is the default. You select a region for each screenshot.
- **Choose a window** selects one visible window for each screenshot.
- **Reuse one region** asks for one region on first use, then reuses its
  saved rectangle.
- **Reuse one window** asks for one visible window on first use, then finds
  that window for each screenshot.

On Windows, a drag selects a region, and a click selects a window. Hold
`Alt` before you start the gesture to switch between the two.

Interactive Linux screenshots need `slurp` and layer-shell support.
**Choose a region** accepts a region drag or a window click on Hyprland
or Sway. **Choose a window** uses a window click. A window click needs
the Hyprland or Sway window list. On another compositor, use a region mode.

A saved window stores its exact `app_id` and title. Windows uses the window
class as `app_id`. Linux uses the class or `app_id` from the compositor.
Both values must match exactly one visible window. A window capture copies
the visible screen rectangle. It does not copy hidden window contents.

A saved region keeps the same rectangle after a restart. A saved window
gets fresh geometry after the window moves or changes size. A title change
breaks the match. Reset the saved target before you select the window again.

Before you add a card, configure screenshots on the **Anki** tab:

1. Map an Anki field to `screenshot`.
2. Select **Attach a screenshot to cards**.
3. Choose a mode with **Screenshot source**.
4. Click **Apply**.

To add a card with a screenshot:

1. Put the pointer on a word until the popup appears.
2. Click the Anki button under the popup.
3. If chibipop asks for a target, select a region or window.

The **Reuse** modes save the first successful selection. You can also use
the **Add current result to Anki** shortcut instead of the Anki button.

The **Anki** tab shows the mode, a summary of each saved target, and a
reset button. Click **Clear saved targets** on Windows or
**Reset saved screenshot targets** on Linux. Click **Apply** to save the
reset. The next screenshot in a **Reuse** mode asks for a new target.

chibipop saves the screenshot and sends the card to Anki.

**To skip an interactive selection, push `Esc` while the selector is open.**
On Windows, a right-click also skips it. chibipop still adds the card
without a screenshot. Linux cancels the selection after 20 seconds.
Windows waits until you select or cancel.

A **Reuse** mode with a saved target captures immediately, without a
selector.

**Screenshot folder.** The default folder is `screenshots`. On Linux,
change it with the **Screenshots folder** box on the **Anki** tab.
On Windows, set `save_dir` under `[actions.screenshot]` in `chibipop.toml`.
A full path selects that exact folder. chibipop resolves a relative path
from these locations:

- **beside the program** on Windows, and on Linux in portable mode
- **in `~/.local/share/chibipop/`** on Linux otherwise, or in
  `$XDG_DATA_HOME/chibipop/` if you set that variable

For example, the default Linux folder is
`~/.local/share/chibipop/screenshots/`.

Anki copies each screenshot into its own media folder. The `screenshots`
folder keeps your local copies.

### Add the sentence

Map an Anki field to `sentence` before you choose a sentence source.

chibipop can also send the sentence around the word. Choose the source with
**Sentence source** on the **Anki** tab:

- **Detected sentence** — chibipop reads a larger area when you add the card.
  If that read fails, it uses the sentence from the lookup. This is the default.
- **Line under the pointer** — the captured line that contains the word.
- **All captured lines** — everything that chibipop read around the pointer.
- **Fixed screen area** — a fixed part of the screen that you mark once.

Click **Apply** after you change the sentence source or field map.

#### Fixed screen area

Games and visual novels usually put their text in the same box every time.
Mark that box once to read the sentence from it. **Fixed screen area**
also makes screen lookups use this area instead of the area around the
pointer.

On Linux, the area selector and outline need layer-shell support.

1. Set the sentence-area shortcut on the **Shortcuts** tab.
2. On the **Anki** tab, set **Sentence source** to **Fixed screen area**.
3. Click **Apply**.
4. Push the sentence-area shortcut keys.
5. On the dimmed screen, drag a rectangle around the text box.

chibipop saves the area for use after a restart. Push the shortcut keys
again to change it.

If the outline is enabled and your desktop supports it, a teal outline
marks the area. To hide it, clear **Outline the sentence area** on Windows
or **Show the static region outline** on Linux.

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

On Linux, copy a theme file to `popup.css` beside the active configuration
file. The search windows use it on the next search.

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
it an image. For hover lookup, the plugin must return each word's text and
position in that image. Text-only lines cannot produce a hover match.
chibipop handles dictionary lookup, the popup, and the highlight.

### Configure meikiocr

[meikiocr](https://github.com/rtr46/meikiocr) is an OCR engine for Japanese
game text. The Windows download includes its adapter in `plugins/meikiocr/`.
It does not include Python, the Python packages, or the plugin's model
cache.

#### Prepare Python and the models

Open PowerShell in the extracted chibipop folder. The following steps need
an internet connection. chibipop must find the same Python environment
through `PATH` when it starts the plugin.

1. Install meikiocr and its dependencies:

   ```powershell
   python -m pip install meikiocr
   ```

2. Select the plugin's model cache for this PowerShell session:

   ```powershell
   $env:HF_HOME = Join-Path $PWD "plugins\meikiocr\hf-cache"
   ```

3. Download the models before you enable the plugin:

   ```powershell
   python -c "from meikiocr import MeikiOCR; MeikiOCR()"
   ```

The plugin uses offline mode by default. Its default model cache is
`plugins/meikiocr/hf-cache/`. In `plugins/meikiocr/config.toml`, `hf_home`
can select another populated cache. An existing `HF_HOME` environment
variable takes priority.

#### Select the plugin

1. On the **Text recognition** tab, choose **meikiocr** under **Text reader**.
2. Click **Apply**.
3. Restart chibipop.

If you use a virtual environment, start chibipop from that environment.
For an existing installation, `meikiocr_path` must name the package's import
folder, such as `Lib\site-packages`. This setting does not select a different
Python executable. See
[`plugins/meikiocr/config.toml`](plugins/meikiocr/config.toml) for path and
cache settings.

If meikiocr cannot start, chibipop uses the built-in engine and prints the
reason to its log.

### Check which engine runs

During screen lookup, the settings footer shows the active OCR language and
engine.
To check the engine that handles a lookup:

1. Select **Show the active text reader** on the **Debug** tab.
2. Click **Apply**.
3. Look up a word.

The status area then names the engine. **Show live logs** on the
**Debug** tab opens the live log window.

To save engine messages, run this command in PowerShell from the program
folder:

```powershell
.\chibipop.exe run 2>engine.log
```

In a second PowerShell window, read the log from the same folder:

```powershell
Get-Content engine.log -Wait -Tail 20
```

**Show extension messages** on the **Debug** tab shows the last five plugin
messages when you click **Apply**. Use the live log window for later messages.

### Write your own plugin

A plugin is a folder inside `plugins/` that holds:

- `plugin.toml` — the name, version, protocol, command, and roles of the
  plugin
- a program or script that exchanges JSON over standard input and standard
  output, one message on each line

A text-provider plugin also needs a `[text_provider]` section in its
manifest. For hover lookup, set `provides_geometry = true` and return
`words` with text and image-local bounding rectangles.
[`plugins/meikiocr/plugin.toml`](plugins/meikiocr/plugin.toml) shows the
required structure.

`plugins/meikiocr/adapter.py` is a working example. The protocol has two
methods: `hello` introduces the plugin, and `text/recognise` reads a
screen image. Both sides ignore unknown fields. A plugin answers an unknown
method with an error. After three consecutive failed recognitions, chibipop
switches the plugin off until the next restart. One successful recognition
resets the count.

---

## 10. Linux

chibipop needs a **Wayland** desktop with layer-shell support.
**Hyprland** is the reference compositor. **Sway** and **KDE Plasma** also
support the popup. KDE uses desktop portals for capture and global
shortcuts.

**Stock GNOME cannot show the popup** because it lacks layer-shell support.
The older **X11** display system is not supported.

### Installing

| How | What to do | Notes |
|---|---|---|
| Download | `tar xzf chibipop-vX.Y.Z-linux-x64.tar.gz` | requires the system libraries and font listed below |
| Nix | `nix run github:stellarie/chibipop` | builds with the ONNX Runtime of nixpkgs |

The download needs glibc 2.39 or later, a libstdc++ that provides
`GLIBCXX_3.4.31`, and a Japanese font such as Noto Sans CJK.
KDE capture needs PipeWire and `xdg-desktop-portal` with a compatible
desktop backend. Portal shortcuts also need that backend.

The download includes the OCR engine and its models. Screen lookup works
without an internet connection. chibipop does not download models on the
first run.

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

These examples assume that Home Manager receives the flake's `inputs`.
For standalone Home Manager, set `extraSpecialArgs = { inherit inputs; };`
in `homeManagerConfiguration`. For the NixOS module, set
`home-manager.extraSpecialArgs = { inherit inputs; };`.

Then import its Home Manager module and enable the program:

```nix
{ inputs, pkgs, ... }:
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

You can also start chibipop manually with `chibipop run`.

#### CUDA with Home Manager

The Home Manager module uses the CPU package by default. To use the CUDA
package, set the `package` field in your `programs.chibipop` block:

```nix
programs.chibipop.package = inputs.chibipop.packages.${pkgs.system}.cuda;
```

### The lookup key

Wayland applications cannot use the Windows keyboard-hook method.
**Your desktop must send lookup shortcuts to chibipop.** On KDE, the
desktop portal can assign shortcuts. Start chibipop through its desktop
entry or systemd user unit so that the portal can identify it.

On other supported compositors, a desktop shortcut runs a `chibipop ctl`
command. The **Shortcuts** tab provides the command for each action.

**While held** needs key-release events. Niri and desktop shortcut editors
that send only key presses cannot use this mode. Use **Turn on / off** or
**Once per press** instead.

On Hyprland, two lines in your configuration set the default `ALT+F` for
the **While held** mode:

```
bind = ALT, F, exec, chibipop ctl trigger-down
bindr = ALT, F, exec, chibipop ctl trigger-up
```

[`extras/`](extras/) contains a ready-made `hyprland.conf`.
[`extras/README.md`](extras/README.md) also has Sway examples. The
**Shortcuts** tab shows instructions for Hyprland, Sway, Niri, KDE,
and GNOME.

### Two things to expect

- **Vertical text is beta.** A capture area can include neighboring columns.
  When you read vertical text, enable **Read vertical text first** on the
  **Text recognition** tab. The setting swaps the capture width and height.
  The engine can still miss the first character or a final `。`.
  See the [measured results](docs/REFERENCE.md#known-limits-measured-rather-than-assumed).
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

If no popup appears, check these items before you report a problem:

1. Make sure that at least one definition dictionary is enabled.
2. Make sure that the dictionary build is complete.
3. Check the lookup mode on the **Shortcuts** tab.
4. On Linux, check the [desktop requirements](#10-linux).

If the problem continues, open an
[issue](https://github.com/stellarie/chibipop/issues). Include your version
number, operating system, lookup mode, and steps to reproduce
the problem. On Linux, include your compositor. A screenshot of the text
can help explain the problem. Remove private information before you attach
screenshots or logs.

To get the version number, run `.\chibipop.exe --version` on Windows or
`chibipop --version` on Linux.

You can also use an issue to suggest a feature. Pull requests are welcome.

---

## 12. For developers

Install [Rust](https://rustup.rs) stable and the tools for your platform:

- **Windows:** the MSVC C++ build tools and a Windows SDK.
  See the [Rust MSVC prerequisites](https://rust-lang.github.io/rustup/installation/windows-msvc.html).
- **Linux:** a C/C++ toolchain and the platform libraries.
  See the [Linux quick start](docs/LINUX.md#quick-start-hyprland), or use
  `nix develop` from the repository root.

Run the command for your platform from the repository root:

```bash
cargo build --release -p chibipop-windows    # Windows
cargo build --release -p chibipop-linux      # Linux
```

Both commands produce a binary named `chibipop`.

A Linux release binary also needs its model files. The
[Linux build instructions](docs/LINUX.md#quick-start-hyprland) include the
required model-copy step.

**One repository, two binaries.** The core library is the root package.
One binary crate for each platform lives in `crates/`. Both binaries have
the name `chibipop`. A command that links both crates can write to the same
output path. Exclude the other platform:

```bash
cargo test --workspace --exclude chibipop-linux     # Windows
cargo test --workspace --exclude chibipop-windows   # Linux
```

See [`ARCHITECTURE.md`](ARCHITECTURE.md#workspace-and-seams).

The development shells are `nix develop` for CPU and `nix develop .#cuda`
for CUDA.

### The Linux regression suite

The runner needs Python 3.9 or later and a working Docker or Podman installation.
A full run needs network access to build the image and download dependencies.
Run this command from the repository root:

```bash
python3 scripts/linux_container_regression.py
```

Use Podman explicitly, or repeat the complete schedule to find intermittent
failures:

```bash
python3 scripts/linux_container_regression.py --runtime podman
python3 scripts/linux_container_regression.py --loops 3
```

The runner mounts the repository read-only. Each loop copies the current
Git workspace, including local non-ignored changes, into a new container.
The runner builds an Ubuntu 24.04 image by default. `--image` sets the
image tag. To use an existing image without rebuilding it, also pass
`--skip-image-build`.

After each loop, the runner removes only its own labeled container.
`--keep-failed-container` keeps a failed container for inspection.

Results go to `linux-regression-artifacts/`. The directory contains JSON
and JUnit reports, command logs, package output, and compositor evidence.
Use `--artifacts-dir <directory>` to choose another location.

List the schedule:

```bash
python3 scripts/linux_container_regression.py --list
```

Inspect the generated commands without a container:

```bash
python3 scripts/linux_container_regression.py --dry-run
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

**Dictionaries are not included.** Each dictionary has its own license.
