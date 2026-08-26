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
2. Press the **Region hotkey** you set in Settings. The screen dims.
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

Each dictionary's definitions are separated by a header and a horizontal
rule. HTML glosses use numbered lists.

**First dictionary only** — check this on the Anki tab to send only the
top-priority dictionary's definitions. Keeps cards clean when multiple
dictionaries match.

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
OCR engine. It ships with chibipop as a reference plugin. Discovery enables
it automatically when its manifest parses successfully.

1. **Install meikiocr.** Follow its README. You need Python with meikiocr,
   OpenCV, and ONNX Runtime.
2. **Configure the path.** Settings > **OCR / Debug** tab > select
   **meikiocr** from the **OCR engine** dropdown > click **Configure...** >
   pick any file in your meikiocr folder.
3. **Restart chibipop.** The startup line confirms the engine:
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

## Linux

Linux support is **in development** and not yet in any release. It targets
Wayland compositors that speak the wlroots protocol family — **Hyprland is the
reference compositor**, with Sway and friends first-class alongside it. KDE
Plasma works through the desktop portals. X11 is not a target.

### The trigger key

Wayland has no global key observation, so on wlroots compositors the
**compositor's own keybind is the trigger**: it runs `chibipop ctl`, which
speaks to the daemon over a UNIX socket. Two lines, and the default `ALT+F`
chord (held while reading) looks like this on Hyprland:

```
bind  = ALT, F, exec, chibipop ctl trigger-down
bindr = ALT, F, exec, chibipop ctl trigger-up
```

and like this on sway:

```
bindsym --no-repeat Mod1+f exec chibipop ctl trigger-down
bindsym --release   Mod1+f exec chibipop ctl trigger-up
```

`trigger-down` freezes one full grab of the monitor under the cursor *before*
the popup appears, and every lookup while you hold the chord reads that frozen
screen: the popup can cover the very word it is defining and the next lookup
still reads through it. Moving onto another monitor mid-hold grabs that one.
`trigger-up` drops the frame and hides the popup. `chibipop ctl toggle` is the
hands-free version — it freezes at toggle-on and stays frozen until you toggle
off.

**One Hyprland defect to know about** (present through at least 0.55.4,
verified in source and live): if you release the modifier before the key —
ALT up, then F up — Hyprland fires **no release bind at all**
([hyprwm/Hyprland#5032](https://github.com/hyprwm/Hyprland/discussions/5032)).
The `trigger-up` is lost and the popup sticks, following the cursor as if the
chord were still held. No bind arrangement works around it, so make a habit of
releasing `F` first; if the popup does stick, tap the chord again (releasing
`F` first), or bind the toggle instead — one press bind, nothing to release,
nothing to wedge:

```
bind = ALT, F, exec, chibipop ctl toggle
```

sway is not affected — it arms the release binding at press time and fires it
on whichever key of the chord comes up first.

**Holding a bare modifier** (the Windows default's hold-Shift feel) is one line
on Hyprland, which can bind a modifier as the key itself:

```
bind  = SHIFT, Shift_L, exec, chibipop ctl trigger-down
bindr = SHIFT, Shift_L, exec, chibipop ctl trigger-up
```

sway's equivalent is `bindsym --release Shift_L`. It is a real cost, not a free
upgrade: every Shift press then spawns a short-lived `chibipop ctl`, so it
suits a reading machine more than a typing one. On Hyprland it is also in the
same wedge family as above — pressing any other key while Shift is held loses
that release too, sticking the hold until the next clean Shift tap. And it is
impossible on the portal shortcuts channel (KDE, GNOME 48+), whose spec
requires modifier-plus-key — which is why `ALT+F` is the default everywhere.

**The portal route** is the alternative that cannot lose a release. Launched
with an app id — the desktop entry, the systemd user unit, or a uwsm session —
chibipop registers its trigger on the GlobalShortcuts portal. On KDE and
GNOME that is the whole story (the desktop shows a consent dialog and owns the
binding). On Hyprland, with `xdg-desktop-portal-hyprland` running, the key
stays in your config but rides the `global` dispatcher instead of `exec`:

```
bind = ALT, F, global, chibipop:trigger
```

Hyprland delivers the portal release keyed to the pressed shortcut itself,
independent of the modifier state, so either release order retracts the popup.
A daemon started from a bare shell has no app id and the portal refuses it —
chibipop's status row says so, and the socket keeps serving meanwhile.

Ready-to-copy snippets live in [`extras/hyprland.conf`](extras/hyprland.conf),
and the settings window shows (and copies) the right snippet for whatever chord
you configure.

### GNOME

GNOME is **best-effort**, and today that means something concrete:

- **The popup cannot appear on stock GNOME.** chibipop draws its popup on the
  `wlr-layer-shell` protocol, which GNOME's compositor (Mutter) does not
  implement. On GNOME, chibipop starts and keeps running, and tells you exactly
  that — a startup message naming the missing `zwlr_layer_shell_v1` capability,
  and a `Popup: unsupported — missing zwlr_layer_shell_v1` line in its
  per-channel status, not a crash. Everything that does not need an overlay
  surface still works: the settings window opens (it is an ordinary window),
  the capture, cursor and trigger channels still resolve and report themselves,
  and `chibipop ctl` still answers. What does not run is the hover-and-read
  loop, because there is nowhere to draw it. If GNOME ever gains a way to place
  an overlay surface, the rest below is what its support looks like.
- **Screen capture asks once.** GNOME has no promptless capture protocol, so
  chibipop uses the ScreenCast portal: one consent dialog on first launch
  covering all monitors, then a restore token keeps later launches silent. If
  you deny it, chibipop shows an error state with a retry — it never loops the
  dialog and never exits over it.
- **Cursor tracking rides that same capture stream** (portal cursor metadata),
  so it costs no extra prompt — but it also means a denied capture portal takes
  cursor tracking down with it, and chibipop reports itself unsupported, naming
  the missing capability.
- **The trigger key needs GNOME 48 or newer.** Trigger keys register through
  the GlobalShortcuts portal, which GNOME ships usably from version 48. On
  older GNOME there is no trigger channel at all — chibipop's per-channel
  status names it as down. Note that while bound, GNOME swallows the shortcut
  (default `Alt+F`) globally; you can edit the binding in GNOME's portal
  dialog.
- **No tray icon on stock GNOME.** GNOME removed tray (StatusNotifier) support;
  an extension such as *AppIndicator and KStatusNotifierItem Support* restores
  it. chibipop is fully operable without the tray: the settings window opens
  with `chibipop settings` (or from the autostart entry's launcher), the
  per-channel status is written to the daemon's log at startup and on every
  change — `chibipop probe` prints the capability report those verdicts come
  from — and stopping the daemon is a `SIGTERM` (`systemctl --user stop
  chibipop` for the unit in [`extras/`](extras/), or `pkill chibipop`). The tray
  is a convenience over those, never the only way to reach them.
- **The popup cannot hide itself from screen sharing.** Hyprland and KDE offer
  ways to exclude chibipop's popup from third-party capture (chibipop shows
  you the snippet); GNOME has no equivalent, so on GNOME the popup would be
  visible in recordings and calls.

### Starting at login

The settings window has an autostart checkbox that writes (or removes)
`~/.config/autostart/chibipop.desktop` directly — the file is the whole
state, there is no config field to drift from it. GNOME, KDE, and
uwsm-managed sessions honour that entry. Bare Hyprland/sway sessions
don't read XDG autostart; [`extras/`](extras/) ships a systemd user unit
and a Hyprland `exec-once` + trigger-bind snippet for those, each with
its install line in [`extras/README.md`](extras/README.md).

---

## For developers

**Building from source** needs [Rust](https://rustup.rs) (stable; MSVC on
Windows) and nothing else — `cargo build --release -p chibipop-windows` on
Windows, `cargo build --release -p chibipop-linux` on Linux (both produce a
binary named `chibipop`). The executable's icon is a committed resource, so
no Windows SDK is required.

- [`docs/REFERENCE.md`](docs/REFERENCE.md) — config reference, diagnostics,
  tests, measured limits.
- [`docs/RELEASING.md`](docs/RELEASING.md) — how to cut a release.
- [`docs/BACKLOG.md`](docs/BACKLOG.md) — deferred work with evidence.

## Licence

GNU General Public License v3.0 or later. See [`LICENSE`](LICENSE).

The Linux build bundles the [meikiocr](https://github.com/rtr46/meikiocr) text
recognition models (`crates/chibipop-linux/models/meiki/`). Their weights are
**LGPL-3.0**, redistributed unmodified as data files, and ONNX Runtime is MIT —
both compatible with the GPL. Details and upstream links are in
[`models/meiki/LICENSE.md`](crates/chibipop-linux/models/meiki/LICENSE.md).

The deconjugation rules (`data/deconjugator.json`) are public domain.
Dictionaries are not included and not ours to distribute.
