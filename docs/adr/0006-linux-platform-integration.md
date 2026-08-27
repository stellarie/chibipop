# Linux tray, autostart, instance lock, paths, and lookup log

The six Windows platform idioms (tray, autostart, named mutex, beside-exe paths,
CREATE_NO_WINDOW, show/hide console) become on Linux:

**Tray: ksni SNI, non-fatal, trayless-operable.** StatusNotifierItem over D-Bus via
ksni with `default-features = false, features = ["async-io"]` — the one deliberate zbus
arrangement for the whole Linux binary (ksni's documented tokio feature-unification
footgun; ADR-0001's sync/calloop posture rules out importing tokio for a tray). Windows'
"failing to create is fatal" flips: SNI registration failure or a late-starting bar is
tolerated (`assume_sni_available`, watcher transitions), and the app is fully operable
trayless — stock GNOME has no tray host, bare Hyprland has no bar. Menu: **Settings,
Quit, and disabled status rows naming each input channel's state** ("Cursor: portal
denied — see settings"); the SNI `Status` flips to NeedsAttention when a channel is
down. No log item. Icon: embedded PNG decoded to ARGB pixmaps by default, hicolor named
icon when packaged; the `.ico` frame parser stays Windows-only.

**Autostart: a settings toggle writes `~/.config/autostart/chibipop.desktop`**
(`Exec=chibipop run`, `TryExec=chibipop`) — one file covers GNOME, KDE, and uwsm-managed
Hyprland via systemd's xdg-autostart generator. Bare Hyprland/sway users get documented
snippets (systemd user unit `WantedBy=graphical-session.target`; Hyprland exec line with
a 0.55 config-language caveat), consistent with ADR-0003's native-keybind guidance. New
feature — Windows has no autostart.

**Instance lock: `flock(LOCK_EX | LOCK_NB)` on
`$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.lock`.** Kernel-released on any death —
no stale-lock handling. Keyed per display to match both the Win32 per-session mutex and
the app's job (one daemon per compositor); the ADR-0003 control socket is keyed
identically so lock and socket always name the same instance. A second `chibipop run`
exits with a clear message; forwarding "open settings" to the running instance is a
possible later verb, not v1. The per-library Apply lock carries over unchanged in
spirit: per-user flock keyed by library-path hash. Abstract sockets rejected
(system-global namespace, squattable); D-Bus name claim rejected as the guard (per-user
bus, needs a session bus, arrives too late for the hook-double-install window).

**Paths: XDG mapping with portable mode.** config →
`$XDG_CONFIG_HOME/chibipop/chibipop.toml`; library archives + built DB →
`$XDG_DATA_HOME/chibipop/` (data, not cache — rebuilds are expensive and cache cleaners
purge); log → `$XDG_STATE_HOME/chibipop/`; update downloads / OCR scratch →
`$XDG_CACHE_HOME/chibipop/`; lock + sockets → `$XDG_RUNTIME_DIR/chibipop/`. Discovery
order, first match wins: `--config` flag → **portable mode** (`chibipop.toml` beside the
exe ⇒ everything beside-exe, preserving the Windows portable identity and AppImage
convention) → XDG. Never dual-read: precedence is decidable from one file's existence.
Migration from a beside-exe layout is a **documented manual copy** — no auto-detection,
no surprise multi-GB moves. The beside-exe self-update exe-swap is Windows-only /
gated on a user-writable exe dir (packaging ticket owns the rest).

**Lookup log: diagnostics always, lookup content opt-in — exact Windows parity.**
Diagnostics go to stderr always (terminal; journal under a systemd unit) plus a logfile
at `$XDG_STATE_HOME/chibipop/chibipop.log`, truncated on start (bounded without rotation
machinery). Lookup lines — actual words read off the user's screen — are written only
when `debug.show_lookup_log` is on, matching the verified Windows gate; screen content
never touches disk un-opted-in. The Windows show/hide-console trick has no analogue and
no tray replacement: the settings window shows the log path next to the
`show_lookup_log` toggle. `CREATE_NO_WINDOW` is confirmed needless on Linux; the only
spawn nuance kept is `process_group(0)`/`setsid` for the self-restart child.
