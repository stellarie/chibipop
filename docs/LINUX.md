# chibipop on Linux

Linux ships from **v0.9.9**, the first release with a Linux asset. It targets
Wayland compositors that speak the wlroots protocol family — **Hyprland is the
reference compositor**, with sway and friends first-class alongside it. KDE
Plasma works through the desktop portals. GNOME is best-effort. X11 is not a
target.

---

## Quick start (Hyprland)

### Do now

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

2. **Start the daemon.** Add `exec-once = chibipop run` to your Hyprland
   session, or start `chibipop run` by hand.

3. **Configure a Bind.** Open `chibipop settings`, select **Shortcuts**, and
   set a lookup action, mode, profile, and `ALT+F` chord.

4. **Install the native binding.** Copy the Bind's **Copy bind snippet**
   output into `~/.config/hypr/hyprland.conf`.

   Hold mode needs both `bind-down ID` and `bind-up ID`.

5. **Reload Hyprland.** Run `hyprctl reload`.

### Read

1. **Add dictionaries.** On **Dictionaries**, click **Browse…** and select
   Yomitan `.zip` archives. Press **Rebuild** to import the staged archives.

2. **Read.** Activate the configured lookup Bind over Japanese text.


Every snippet in this document writes the bare command name `chibipop`, which
assumes an installed binary on `PATH`. Running from `cargo run` or an
extracted folder? Copy the bind from the **Shortcuts** tab instead. It
names the running binary's full path, quoted.

## Settings window

`chibipop settings` opens **General**. The six tabs are **General**,
**Shortcuts**, **Popup**, **Dictionaries**, **Text recognition**, and **Anki**.
The **Shortcuts** tab edits Bind IDs, actions, modes, enabled state, and
platform chords. A Bind can select an optional profile.

Apply requests direct global shortcuts where the desktop supports them.
For native bindings, each configured Bind has a **Copy bind snippet** button.
Each snippet uses `chibipop ctl bind-down ID` or `bind-up ID`.
The compositor selector changes snippet syntax, not Bind ownership.

KDE and GNOME show **Copy daemon command** for a Press action when no portal
binding exists. Their shortcut editors cannot send a key release.
Hold mode on those desktops needs a portal binding.
Otherwise, select Toggle or Press mode.
GNOME repeats a held custom shortcut, so tap the chord.
Niri snippets go inside the existing `binds` block.
Niri supports Press bindings but has no key-release bind for Hold mode.
Screen capture exclusion stays on **Popup**.

**Ctrl+Tab** selects the next tab. **Ctrl+Shift+Tab** selects the previous tab.
Both shortcuts wrap at the ends of the tab strip.
The status line and **Apply** stay visible while a page scrolls.
**Apply** saves the form regardless of the selected tab.

Settings caches complete Dictionary role inspections in
`library/.roles-v1.json` under the data directory. Before it reuses roles,
settings checks the archive's device, inode, size, modification time, and change time.
New or changed archives require inspection.

The cache is disposable and separate from `library.json` and the archives.
A missing or invalid cache requires inspection instead.
Cache write failures do not prevent library access.

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

Wayland does not provide global key observation.
Configure a Bind in **Settings > Shortcuts**.
The Bind stores its stable ID, action, mode, profile, enabled state, and chord.
The settings window provides the compositor command for that Bind.

For native compositor bindings, use the Bind ID:

```text
bind = ALT, F, exec, chibipop ctl bind-down my-lookup
bindr = ALT, F, exec, chibipop ctl bind-up my-lookup
```

Use both commands for Hold mode.
Use only `bind-down` for Press and Toggle modes.
Use `bind-down` for one-shot actions.
The daemon rejects an ID that is absent or disabled.

The fixed socket verbs remain supported:
`trigger-down`, `trigger-up`, `toggle`, `lookup`, `anki-add`,
`ocr-clipboard`, `static-region`, `search`, `sentence-search`,
`selected-text`, and `reload`.
These verbs keep their legacy routing and do not replace configured Binds.

Hold mode freezes one full grab before the popup appears.
Each lookup during the hold reads that frozen screen.
Moving to another monitor starts a grab for that monitor.
Release drops the frame and hides the popup.

Toggle mode latches the trigger.
It reads live grabs with the popup masked until toggle-off.
Press mode runs one masked live lookup for each activation.
Press mode does not follow cursor movement or run a Dwell re-check.
The key release does nothing in Press mode.
Per-character lookup is inert in Press and Toggle modes.

Hovering text in an existing popup can open a child popup.
The parent stays visible.
The click catcher exists in Press mode while a popup has a rectangle.
It consumes an outside click and clears its input region when the popup hides.

Apply requests changed portal bindings without a daemon restart.
The desktop can keep a previously approved chord.
The **Current key** line shows the confirmed chord.
Hyprland registers portal action names but does not assign their keys.
Hyprland therefore uses native compositor bindings.

KDE and GNOME can assign portal shortcuts when the daemon has an app ID.
Start it from the desktop entry or systemd user unit for portal ownership.
Hold mode needs a portal binding when the editor cannot send key releases.

Hyprland can lose a release when the modifier rises before the key.
Release the key before the modifier.
If the popup stays visible, activate and release the Hold Bind again.
Toggle mode avoids this release path.

AnkiAdd and StaticRegion use the displayed profile.
Lookup, SelectedText, Search, SentenceSearch, and OcrClipboard can use a
profile override from their Bind.

### Screenshot sources

The *Anki* tab sets **Screenshot source** for **Attach a screenshot to cards**.

- **Choose a region** is the default. `slurp` lets you drag a region. On Hyprland or
  Sway, a window query also lets you click a visible window.
- **Choose a window** uses `slurp -r` and a click on a visible window.
- **Reuse one region** asks for a region drag on first use. It saves the rectangle
  as global physical pixels and reuses it on later pictures.
- **Reuse one window** asks for a visible window click on first use. It saves the
  window `app_id` and title, then queries fresh geometry for every picture.

Interactive selection needs the `slurp` command on `PATH` and a compositor that
supports layer-shell. Install `slurp` with your distribution package manager.
Choose a region still supports a drag when no Hyprland or Sway window query exists.
**Choose a window** and the first **Reuse one window** selection need `hyprctl`
on Hyprland or `swaymsg` on Sway. **Reuse one window** needs the same query later.

Linux stores the compositor class or `app_id` as `app_id`. Hyprland supplies
the compositor class. Sway supplies `app_id`, or its X11 window class when
`app_id` is empty. The saved `app_id` and title must match exactly one visible
window. A missing or ambiguous match reports an error and selects no other
window. Window capture copies the visible screen rectangle, not hidden or
occluded window contents.

**Reuse one region** keeps its rectangle after a restart. **Reuse one window** follows a
window move or resize because it gets fresh geometry. A title change breaks
the match. Reset the saved target and select the window again.

The Linux settings window shows the selected mode, saved target summaries, and
**Reset saved screenshot targets**. Press **Apply** after a reset. The next
fixed-mode picture asks for a new target. Press **Esc** to cancel a selection.
The selection times out after 20 seconds. Include-on-add still files the card
without a picture.

---

## How support is decided

The daemon splits its platform needs into four channels: **Capture**, **Cursor**,
**Trigger**, and **Popup**. Each channel selects an advertised backend at startup.
Hyprland also has a cursor polling fallback and selects native trigger bindings.

| Channel | First choice | Fallback |
|---|---|---|
| Capture | `zwlr_screencopy_manager_v1` — promptless region capture | ScreenCast portal + PipeWire — one consent dialog, then a restore token keeps later launches silent |
| Cursor | `ext-image-copy-capture` cursor sessions — event-driven, zero idle wakeups | portal cursor metadata on the capture stream; on Hyprland only, `hyprctl cursorpos` polling as a last rung |
| Trigger | GlobalShortcuts portal where the desktop assigns keys directly | the control socket, including Hyprland; always available for native bindings |
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

The config file format is shared with Windows.
See [`docs/REFERENCE.md`](REFERENCE.md) for the full profile reference.
Linux chords use portal syntax such as `ALT+F`.
Configure chords as Bind records in Settings.
Each configured row supplies its stable ID and native command.
The fixed socket verbs remain supported.


## Command line

| Command | What it does |
|---|---|
| `chibipop run` | Starts the daemon. The default when no subcommand is given. |
| `chibipop ctl <verb>` | Sends one fixed verb over the control socket. Fixed verbs include `trigger-down`, `trigger-up`, `toggle`, `lookup`, `anki-add`, `search`, `sentence-search`, `selected-text`, `ocr-clipboard`, `static-region`, and `reload`. |
| `chibipop ctl bind-down <id>` | Activates an enabled configured Bind ID. |
| `chibipop ctl bind-up <id>` | Releases an enabled configured Bind ID. Use it for Hold mode. |
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

  The OCR language field belongs to the selected profile.
  Linux keeps that field even though the bundled engine reads Japanese.
  The selected profile's
  `[profiles.settings.dictionaries.per_language]` map controls its lists.
  A Derived profile can replace the complete map.
  A missing map searches every enabled terms Dictionary.
- **The Fixed screen area sentence mode has a configured Bind.**
  Select *Fixed screen area* as the Anki sentence field.
  Create or enable a `StaticRegion` Bind in **Shortcuts**.
  Use its `bind-down` command to select the region.
  Use `Esc` or right-click to cancel the selection.
  The region is saved under the displayed profile's
  `[profiles.settings.anki]` settings.
  The daemon uses the saved region without a restart.
  *Show the static region outline* draws a teal border around it.
  The border needs `zwlr_layer_shell_v1`.
- **Updates are check-only.** The *Check for updates* button reports a newer
  release and names the Linux tarball asset; chibipop never replaces its own
  binary on Linux — update with your package manager or by download.
- **Capture exclusion is a compositor rule**, not the `exclude_from_capture`
  setting — see [Hiding the popup from screen sharing](#hiding-the-popup-from-screen-sharing).
- **Anki works the same** (AnkiConnect, same field map). Its add key defaults to `ALT+A`.
  The key uses direct portal registration where supported or a native daemon bind.
  The popup's own Anki button works on every compositor.
- **Screenshots attach during Anki adds.**
  *Include screenshot when adding* uses the selected mode described above.
  The PNG lands in
  `$XDG_DATA_HOME/chibipop/screenshots` by default —
  `~/.local/share/chibipop/screenshots` when that is unset, or beside the
  executable in portable mode. The *Anki* tab's **Screenshots folder** box
  changes this path. Absolute paths stay exactly as typed.

  Interactive modes need `slurp` and layer-shell support. The Capture channel
  supplies the pixels. A saved fixed region bypasses the selector. A saved
  fixed window still needs fresh Hyprland or Sway window metadata.
- **OCR-to-clipboard works, except on stock GNOME.** The `ocr-clipboard`
  action uses the displayed profile's OCR settings.
  Create or enable an `OcrClipboard` Bind in **Shortcuts**.
  Use its configured `bind-down` command.
  The clipboard protocol rules below remain the same.

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
uwsm-managed sessions honour that entry. Bare Hyprland and Sway sessions do
not read XDG autostart. [`extras/`](../extras/) ships a systemd user unit.
Configure Binds in Settings, then copy the generated compositor commands.
Pick one startup mechanism.

---

## Troubleshooting

**The popup sticks after releasing the chord (Hyprland).** You released the
modifier before the key, and Hyprland lost the release bind — see
[the trigger key](#the-trigger-key). Tap the chord again releasing `F` first,
or switch to the toggle bind.

**The trigger chord does nothing.** Check the configured Bind ID.
Run `chibipop ctl bind-down <id>` from a terminal.
The daemon must answer `OK`.
If it does, copy the complete command from **Settings > Shortcuts**.
The fixed `chibipop ctl trigger-down` verb also remains supported.

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

## Selected application text

Set **Look up selected text** in **Settings > Shortcuts**. The action uses
application text directly and does not run OCR or change clipboard contents.
Browser selections need no extension.

On Linux, the action reads PRIMARY through ext-data-control or wlr-data-control
version 2. A native compositor binding can run `chibipop ctl selected-text`.
The source application controls PRIMARY's lifetime; it can outlive visible
highlighting. Applications without PRIMARY export and unsupported compositors
cannot supply a selection. The normal clipboard is never used as fallback.

The checkbox below **Look up selected text** can open Sentence search with the
selection. PRIMARY does not provide word bounds, so popup placement uses the
cursor. Outside clicks dismiss selected-text popups in every trigger mode.
