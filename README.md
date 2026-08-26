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

**Holding a bare modifier** (the Windows default's hold-Shift feel) is one line
on Hyprland, which can bind a modifier as the key itself:

```
bind  = SHIFT, Shift_L, exec, chibipop ctl trigger-down
bindr = SHIFT, Shift_L, exec, chibipop ctl trigger-up
```

sway's equivalent is `bindsym --release Shift_L`. It is a real cost, not a free
upgrade: every Shift press then spawns a short-lived `chibipop ctl`, so it
suits a reading machine more than a typing one. It is also impossible on the
portal shortcuts channel (KDE, GNOME 48+), whose spec requires
modifier-plus-key — which is why `ALT+F` is the default everywhere.

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

The Linux build bundles the [meikiocr](https://github.com/rtr46/meikiocr) text
recognition models (`crates/chibipop-linux/models/meiki/`). Their weights are
**LGPL-3.0**, redistributed unmodified as data files, and ONNX Runtime is MIT —
both compatible with the GPL. Details and upstream links are in
[`models/meiki/LICENSE.md`](crates/chibipop-linux/models/meiki/LICENSE.md).

