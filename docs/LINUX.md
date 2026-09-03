# chibipop on Linux

Linux ships from **v0.9.9**, the first release with a Linux asset. It targets
Wayland compositors that speak the wlroots protocol family — **Hyprland is the
reference compositor**, with sway and friends first-class alongside it. KDE
Plasma works through the desktop portals. GNOME is best-effort. X11 is not a
target.

---

## Quick start (Hyprland)

1. **Build it.** Needs [Rust](https://rustup.rs) (stable) and nothing else:

   ```bash
   cargo build --release -p chibipop-linux
   ```

   The binary is `target/release/chibipop`. A release build looks for the OCR
   models beside itself, so give it a copy (a debug build finds the source
   tree on its own):

   ```bash
   cp -r crates/chibipop-linux/models target/release/
   ```

2. **Wire up the compositor.** Copy [`extras/hyprland.conf`](../extras/hyprland.conf)
   to `~/.config/hypr/chibipop.conf` and include it from your main config:

   ```
   source = ~/.config/hypr/chibipop.conf
   ```

   It starts the daemon (`exec-once = chibipop run`) and binds the default
   `ALT+F` trigger chord. Reload Hyprland or start the daemon by hand.

3. **Add dictionaries.** `chibipop settings` opens the settings window (its
   own process — a settings crash never takes the daemon down). On
   **Dictionaries**, **Browse…** opens the desktop's own file dialog and takes
   as many Yomitan `.zip` archives as you select at once; the entry beside it
   still takes a typed path, which is the fallback on a desktop with no
   file-chooser portal. Adds are staged — press **Rebuild** to build them in.

4. **Read.** Hold `ALT+F` and hover Japanese text anywhere on screen.

Every snippet in this document writes the bare command name `chibipop`, which
assumes an installed binary on `PATH`. Running from `cargo run` or an
extracted folder? Copy the snippet from the settings window instead — it
names the running binary's real path, quoted.

## Installing

Three routes. All three carry the OCR models, so none of them downloads
anything on first run.

**The tarball.** One `tar xzf` is the whole installation. The binary, the
deconjugation rules, the three ONNX models and the `extras/` snippets come
out in one folder, and the binary finds the models beside itself.

```bash
tar xzf chibipop-vX.Y.Z-linux-x64.tar.gz
cd chibipop-vX.Y.Z-linux-x64
./chibipop probe          # the Wayland capability report
./chibipop settings       # add dictionaries
```

**The AUR, on Arch.** `chibipop-bin` repacks that tarball; `chibipop` builds
from the release tag against the distro's ONNX Runtime. Either one installs
`/usr/bin/chibipop`, a `.desktop` entry and a systemd *user* unit. A pacman
install is never portable mode, so config, data and state go to the XDG
directories.

**From source.** See [Quick start](#quick-start-hyprland) above.

Runtime dependencies for the tarball and `-bin` builds: glibc, libstdc++ and a
CJK font such as Noto Sans CJK. Nothing else — the OCR runtime is linked in.

**The v0.9.9 tarball needs `GLIBC_2.39` and `GLIBCXX_3.4.31` or newer.** Those
are measured, not estimated: the release run reads the highest versioned symbol
the binary references from each library. They are Ubuntu 24.04's versions,
which is the image the asset is built on. A distro older than either needs the
source AUR package or a source build, both of which link against whatever the
distro ships.

Optional: `xdg-desktop-portal` (capture and trigger on KDE/GNOME) and
`pipewire` (the portal capture stream).

The packaging is built by [`scripts/package-linux.sh`](../scripts/package-linux.sh)
and templated in [`packaging/aur/`](../packaging/aur/). Both are described in
[`RELEASING.md`](RELEASING.md).

---

## The trigger key

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
([hyprwm/Hyprland#5032](https://github.com/hyprwm/Hyprland/discussions/5032),
[#7675](https://github.com/hyprwm/Hyprland/issues/7675)). The `trigger-up` is
lost and the popup sticks, following the cursor as if the chord were still
held. No bind arrangement works around it, so make a habit of releasing `F`
first; if the popup does stick, tap the chord again (releasing `F` first), or
bind the toggle instead — one press bind, nothing to release, nothing to
wedge:

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
GNOME that is the whole story. The desktop shows a consent dialog and owns the
binding.

On Hyprland, XDPH registers the shortcut but does not assign its key. Run
`hyprctl globalshortcuts` to get the active namespace. An installed desktop
launch normally registers `chibipop:trigger`, but a development launch can
inherit another app ID. For example, a launch from VS Code can register
`code:trigger`. Bind the exact name that `hyprctl` reports.

Hyprland delivers the portal release keyed to the pressed shortcut itself,
independent of the modifier state, so either release order retracts the popup.
The socket keeps serving as a namespace-free fallback.

**The add-card chord is a one-shot action.** The popup never takes focus, so
the key must be global. On Hyprland, use the control socket even when XDPH is
available:

```
bind = ALT, A, exec, chibipop ctl anki-add
```

The settings window shows and copies this line. It does not guess the portal
namespace because that namespace depends on how the daemon started. The socket
and the portal both reach the same add-card code path.

On sway, use its equivalent:

```
bindsym --no-repeat Mod1+a exec chibipop ctl anki-add
```

KDE and GNOME own the portal key through their desktop shortcut settings. The
popup's Anki button uses the same add-card code path on every desktop.

**The mining screenshot's key is the same story again.** `chibipop ctl
screenshot` grabs a region for the popup on screen and files it as that card's
context picture. Its chord (`actions.screenshot.hotkey_linux`, unset by
default) is **native-channel only** — no portal id, so the consent dialog
stays at two entries — and one press, one verb:

```
bind = SUPER, S, exec, chibipop ctl screenshot
```

```
bindsym --no-repeat Mod4+s exec chibipop ctl screenshot
```

Pressed with no popup up it writes one line to the log saying why nothing
happened: the picture is filed against the word on screen, and there is no
word without a lookup. The picture that rides an *add* needs no key of its
own — see **Include screenshot when adding** below.

Ready-to-copy snippets live in [`extras/hyprland.conf`](../extras/hyprland.conf),
and the settings window shows (and copies) the right snippet for whatever chord
you configure — one row shape for every global action, trigger chord included.

---

## How support is decided

The daemon splits its platform needs into four channels — **Capture**,
**Cursor**, **Trigger**, **Popup** — and each picks its backend at startup by
what the compositor actually advertises, never by compositor identity (one
documented exception below):

| Channel | First choice | Fallback |
|---|---|---|
| Capture | `zwlr_screencopy_manager_v1` — promptless region capture | ScreenCast portal + PipeWire — one consent dialog, then a restore token keeps later launches silent |
| Cursor | `ext-image-copy-capture` cursor sessions — event-driven, zero idle wakeups | portal cursor metadata on the capture stream; on Hyprland only, `hyprctl cursorpos` polling as a last rung |
| Trigger | GlobalShortcuts portal (needs an app id) | the control socket — always bound, so the trigger never goes fully down |
| Popup | `zwlr_layer_shell_v1` — required, no fallback | — |

A channel that cannot serve does not crash the daemon: it reports itself,
names the exact missing protocol or portal, and the rest keep working.
Three places show the verdicts:

- `chibipop probe` prints the capability report — lock-free, safe to run next
  to a live daemon.
- The tray menu lists one status row per channel and flags attention when any
  is degraded or down.
- The daemon log records every verdict at startup and on change.

## KDE Plasma

Plasma has no promptless capture protocol, so capture and cursor tracking ride
the ScreenCast portal: one consent dialog on first launch covering all
monitors, then a restore token keeps later launches silent. Deny it and
chibipop shows an error state with a retry — it never loops the dialog and
never exits over it. The trigger registers on the GlobalShortcuts portal when
chibipop is launched with an app id (desktop entry or systemd unit): Plasma
shows a consent dialog and owns the binding from then on. The popup draws on
`wlr-layer-shell`, which KWin implements, and the tray is Plasma's native
StatusNotifier.

## GNOME

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
  chibipop` for the unit in [`extras/`](../extras/), or `pkill chibipop`). The
  tray is a convenience over those, never the only way to reach them.
- **The popup cannot hide itself from screen sharing.** See the next section —
  GNOME has no equivalent, so on GNOME the popup would be visible in
  recordings and calls.
- **OCR-to-clipboard cannot work either.** Writing the selection without
  keyboard focus needs a data-control protocol, and Mutter implements neither
  `ext_data_control_manager_v1` nor `zwlr_data_control_manager_v1`. The daemon
  says so once at startup naming both, and the settings row says it instead of
  offering a bind that could only log a refusal. Run `chibipop clipboard-check`
  to see the verdict for your own session.

## Hiding the popup from screen sharing

No Wayland client can hide its own surface from third-party capture, so this
is a compositor rule, not an in-app switch (the Windows
`exclude_from_capture` setting does not apply). The settings window shows the
right instructions for your compositor:

- **Hyprland** — one line in `hyprland.conf`:

  ```
  layerrule = no_screen_share, chibipop
  ```

- **KDE** — right-click the popup's entry in the screen-share picker and
  enable *Hide from Screen Sharing*.
- **sway and others** — not available; the popup records normally.

---

## Files and paths

chibipop resolves its files in three modes, first match wins:

1. **Explicit** — `chibipop --config <path>` names the exact config file.
2. **Portable** — a `chibipop.toml` beside the executable puts `data/` and the
   log beside the executable too. An extracted-tarball or USB-stick layout.
3. **XDG** — the default:

   | What | Where |
   |---|---|
   | Config | `~/.config/chibipop/chibipop.toml` |
   | Dictionaries and database | `~/.local/share/chibipop/` |
   | Log (truncated each start) | `~/.local/state/chibipop/chibipop.log` |
   | Portal restore token | `~/.local/state/chibipop/portal-restore-token` |
   | Cache | `~/.cache/chibipop/` |

   Each row honours its `$XDG_*` override.

The instance lock and the control socket always live in
`$XDG_RUNTIME_DIR/chibipop/`, keyed by `$WAYLAND_DISPLAY` — one daemon per
compositor session, and a second launch exits with a clear message. If
`XDG_RUNTIME_DIR` is unset (it never is under a normal session manager), the
daemon and `ctl` say so and stop.

The config file format is shared with Windows — see
[`docs/REFERENCE.md`](REFERENCE.md) for the full reference. Every chord has its
own Linux key, in portal syntax: `trigger_key_linux` (default `ALT+F`),
`add_key_linux` (default `ALT+A`), `static_region_key_linux` and
`actions.ocr_clipboard.hotkey_linux` (both unset — native-channel only, so
nothing is bound until you paste the snippet).

## Command line

| Command | What it does |
|---|---|
| `chibipop run` | Starts the daemon. The default when no subcommand is given. |
| `chibipop ctl <verb>` | Sends one verb over the control socket: `trigger-down`, `trigger-up`, `toggle`, `anki-add` (add the word in the popup to Anki), `screenshot` (drag a region and file it as the popup's mining picture), `ocr-clipboard` (drag a region and copy its text to the clipboard), `static-region` (drag the box the Static sentence mode reads), or `reload` (re-read the config). Answers `OK` or `ERR` on one line. |
| `chibipop settings` | Opens the settings window as its own process. |
| `chibipop probe` | Prints `WAYLAND_DISPLAY` and the capability report for this session. |
| `chibipop capture-dump --region X,Y,W,H` | Grabs that region through the live capture backend and writes a PNG (default to `/tmp`, `--out DIR` to change). The proof tool for capture problems. |
| `chibipop clipboard-check` | Takes the clipboard selection with a known string and holds it (`--text`, `--hold SECS`), naming the data-control protocol it used. The proof tool for clipboard problems: it replaces what is on your clipboard, and on a compositor with no data-control protocol it exits non-zero naming both globals it looked for. |
| `chibipop --config <path> …` | Uses that exact config file, any subcommand. |

There is **no `build-dict` subcommand on Linux**. The frequency-list rebuild
the Windows README describes runs inside the settings window instead — add
your frequency lists there and Apply.

## Differences from Windows

- **OCR is the bundled meikiocr engine**, not Windows OCR. Three ONNX models
  ship with chibipop (`models/meiki/`), hash-pinned and verified at startup —
  a mismatch refuses the engine rather than silently reading with something
  else. Nothing is downloaded. Model location override: `CHIBIPOP_MODEL_DIR`.

  The *OCR language* setting is Windows-only and hidden on Linux, but the value
  already in your config is kept, not cleared — and it still selects the
  `[dictionaries.per_language]` list, so the **Not searched** box works here
  exactly as it does on Windows. One consequence: a config carrying a language
  meikiocr does not read (anything but `ja`) searches every dictionary, because
  that language's list was drawn up for a recogniser this build does not run.
  Clear the key, or set it to `ja`, to have your split apply.
- **The Static region sentence mode needs a compositor bind for its key.**
  The mode itself works: pick *Static region* as the Anki sentence field, draw
  a box, and every card's sentence comes from that box instead of the hovered
  line. Drawing it is the `static-region` verb — a drag on the dimmed screen,
  `Esc` or right-click to cancel — and it works in **any** sentence mode,
  because drawing the box is how you decide to switch to Static. The chord
  (`anki.static_region_key_linux`, unset by default) is **native-channel only**:
  it gets no GlobalShortcuts portal id, so the portal consent dialog stays at
  two entries — the portal shortcut id set is frozen at exactly two — and the
  compositor bind is the only way to reach it. The settings window renders the
  bind for whatever chord you type, e.g.

  ```
  bind = ALT, R, exec, chibipop ctl static-region
  ```

  ```
  bindsym --no-repeat Mod1+r exec chibipop ctl static-region
  ```

  The region is saved to your config file as `anki.static_region = [x, y, w, h]`
  in physical pixels, and the running daemon picks it up immediately — no
  restart, no Apply. *Show the static region outline* draws a teal border around
  it, on by default; that border is a layer surface, so like the popup it needs
  `zwlr_layer_shell_v1`. Without the layer shell the region still serves
  lookups, just unmarked, and the log says so once. Both the drag and the border
  need the layer shell for the same reason.
- **Updates are check-only.** The *Check for updates* button reports a newer
  release and names the Linux tarball asset; chibipop never replaces its own
  binary on Linux — update with your package manager or by download.
- **Capture exclusion is a compositor rule**, not the `exclude_from_capture`
  setting — see [Hiding the popup from screen sharing](#hiding-the-popup-from-screen-sharing).
- **Anki works the same** (AnkiConnect, same field map). The add key (default
  `ALT+A`) registers on the GlobalShortcuts portal alongside the trigger; the
  popup's own Anki button works on every compositor.
- **The mining screenshot works the same, with its own key on the native
  channel.** *Include screenshot when adding* dims the screen when you ask for
  a card, exactly as on Windows: drag the region, release to confirm, `Esc` or
  right-click to skip it (the card still goes in, without a picture), and 20
  seconds of no decision cancels the pick. The PNG lands in
  `$XDG_DATA_HOME/chibipop/screenshots` by default — `~/.local/share/chibipop/screenshots`
  where that is unset, or beside the executable in portable mode — and the
  *Anki* tab's **Screenshots folder** box changes it (absolute paths taken as
  typed). Both the drag and the grab need the same protocols the popup and
  hovering already need (`zwlr_layer_shell_v1` for the selector, the Capture
  channel for the pixels), so a session where hovering works can always take
  one. Taking a screenshot *without* asking for a card is the `screenshot`
  verb, a compositor bind — see [the trigger key](#the-trigger-key).
- **OCR-to-clipboard works, except on stock GNOME.** The `ocr-clipboard` verb
  dims the screen, reads the region you drag with the same engine and the same
  OCR settings hovering uses, and puts the text on the clipboard — one line per
  recognised line, words butted together, because the recogniser's word split
  is an artefact and a space between them would be text the screen never had.
  Nothing is upscaled: meikiocr reads native-resolution crops better than 2x
  ones, which is where the Windows twin differs. Its chord
  (`actions.ocr_clipboard.hotkey_linux`, unset by default) is native-channel
  only for the same reason `static-region`'s is:

  ```
  bind = ALT, C, exec, chibipop ctl ocr-clipboard
  ```

  ```
  bindsym --no-repeat Mod1+c exec chibipop ctl ocr-clipboard
  ```

  The one real gap is **the clipboard protocol**. Writing the selection without
  keyboard focus needs `ext_data_control_manager_v1` or the older
  `zwlr_data_control_manager_v1`; chibipop takes the first it finds, on a
  Wayland connection of its own, and holds the offer for as long as the daemon
  runs. Every wlroots compositor and KWin advertise at least one of them.
  **Mutter advertises neither**, so on stock GNOME this one action cannot work
  at all: the daemon says so once at startup, naming both globals, the settings
  row says it instead of offering a bind, and nothing else is affected. There is
  no `wl-copy` fallback on purpose — a subprocess would not survive the daemon,
  and an offer nobody services is a selection that vanishes the moment you try
  to paste it. `chibipop clipboard-check` is how you see which of the two your
  session has.

## Starting at login

The settings window has an autostart checkbox that writes (or removes)
`~/.config/autostart/chibipop.desktop` directly — the file is the whole
state, there is no config field to drift from it. GNOME, KDE, and
uwsm-managed sessions honour that entry. Bare Hyprland/sway sessions
don't read XDG autostart; [`extras/`](../extras/) ships a systemd user unit
and a Hyprland `exec-once` + trigger-bind snippet for those, each with
its install line in [`extras/README.md`](../extras/README.md). Pick one
mechanism, not several.

---

## Troubleshooting

**The popup sticks after releasing the chord (Hyprland).** You released the
modifier before the key, and Hyprland lost the release bind — see
[the trigger key](#the-trigger-key). Tap the chord again releasing `F` first,
or switch to the toggle or portal bind.

**The trigger chord does nothing.** First check the socket directly:
`chibipop ctl trigger-down` from a terminal should answer `OK`. If it does,
the compositor bind is execing the wrong thing — a bare `chibipop` bind needs
the binary on `PATH`, which `cargo run` and extracted folders don't give you.
Copy the snippet from the settings window; it names the running binary's real
path.

**Characters under the pointer flicker between misreadings.** Your compositor
is painting a *software* cursor into the frames chibipop captures, and OCR
reads the arrow as part of the glyphs. Common on NVIDIA setups with
Hyprland's `cursor:no_hardware_cursors = true`. chibipop detects it and says
so — a startup line naming the option and a degraded Capture status row —
because no capture request can remove a cursor the compositor already baked
into the frame. Fix: `hyprctl keyword cursor:no_hardware_cursors false` (make
it permanent in `hyprland.conf` if your cursor survives). Verify either way
with `chibipop capture-dump --region X,Y,W,H` around the pointer.

**Every capture refuses with "the copy went unanswered".** The output chibipop
asked about is not being repainted, which is what a display that has powered
off (DPMS) looks like from a Wayland client — a locked, unattended desktop is
the usual way to get there. `zwlr_screencopy_manager_v1` answers a `copy` with
`ready` or `failed` and nothing else, so a compositor that goes silent leaves
no third answer to report; chibipop refuses on its deadline rather than
hanging. Wake the panel (`hyprctl dispatch dpms on`, or any input) and the
same grab answers in single-digit milliseconds. Nothing to fix in chibipop, and
nothing a hover can hit: there is no hovering on a dark screen.

**The status row says the shortcuts portal refused an app id.** The daemon was
started from a bare shell, which gives the GlobalShortcuts portal nothing to
identify it by. Launch chibipop from its desktop entry or the systemd user
unit instead. The control socket keeps serving the trigger meanwhile, so
native binds are unaffected.

**Something else.** `chibipop probe` prints what this session supports;
`~/.local/state/chibipop/chibipop.log` records every channel verdict and
change. Both name the exact missing protocol or portal when a channel is
down.
