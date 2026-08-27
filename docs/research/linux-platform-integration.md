# Linux platform integration: tray, autostart, single-instance, XDG paths, helpers, logging

Facts checked 2026-08-18.
Scope: the six Windows platform idioms chibipop leans on today — `Shell_NotifyIcon` tray
(`src/ui/tray.rs`), no autostart yet, `CreateMutexW` library lock (`src/lock.rs`),
everything-beside-the-exe paths (`src/paths.rs`), `CREATE_NO_WINDOW` helper spawns
(`src/rebuild.rs`), and the show/hide lookup-log console (`src/ui/console.rs`) — and what each
becomes on Linux, wlroots/Hyprland first, GNOME last.

## Verdict

| Concern | Recommendation | Confidence |
| --- | --- | --- |
| Tray | StatusNotifierItem over D-Bus via the **ksni** crate (0.3.6, 2026-07-15; pure Rust on zbus; tokio/async-io/blocking). Works on Waybar (Hyprland), KDE, and GNOME-with-extension. Tray creation must become **non-fatal** on Linux (GNOME stock has no host). | High |
| Autostart | Ship an **XDG autostart `.desktop`** (covers GNOME, KDE, and uwsm-managed Hyprland via systemd's generator) and document a **systemd user unit** (`WantedBy=graphical-session.target`) plus the native Hyprland `hl.exec_cmd` line for bare wlroots sessions. No single mechanism covers all three. | High |
| Single instance | **`flock(LOCK_EX \| LOCK_NB)` on a file under `$XDG_RUNTIME_DIR/chibipop/`**, lock name keyed by `$WAYLAND_DISPLAY`. Kernel drops the lock on process death — no stale-lock handling needed. Reject abstract sockets (cross-user namespace); D-Bus name claim is a fine *later* addition for "activate the running instance" IPC, not the guard itself. | High |
| XDG paths | config → `$XDG_CONFIG_HOME/chibipop/chibipop.toml`; library archives + built DB → `$XDG_DATA_HOME/chibipop/`; lookup log → `$XDG_STATE_HOME/chibipop/`; downloads/scratch → `$XDG_CACHE_HOME/chibipop/`; lock/sockets → `$XDG_RUNTIME_DIR/chibipop/`. Keep beside-exe as an explicit **portable mode** (config beside exe wins if present), mirroring AppImage's `.config`-beside-the-file convention. Use the `dirs` (or `etcetera`) crate. | High |
| Detached helpers | Confirmed non-problem: Linux has no console windows, so `CREATE_NO_WINDOW` has no equivalent and needs none. What *is* needed: keep the existing pipe/file stdio discipline (default is inherit), and only reach for `process_group(0)`/`setsid` for children that must outlive or ignore the terminal, e.g. the self-restart in `app.rs::start_run`. | High |
| Lookup log | stderr when attached to a terminal (Rust ecosystem default), plus a logfile at `$XDG_STATE_HOME/chibipop/chibipop.log` (the basedir spec names logs as state data; Neovim precedent). Under a systemd user unit stderr lands in the journal automatically. The Windows show/hide-console trick has no Linux analogue and should be replaced by "open the logfile" from the tray menu. | High |

Open risks, up front:

- **GNOME stock shows no tray at all.** The AppIndicator extension is required and is
  third-party (Ubuntu-maintained). chibipop on GNOME must be fully operable with zero tray —
  `chibipop settings` / `chibipop quit` CLI verbs, or a D-Bus activation path. Given GNOME is
  lowest priority, "document the extension, don't crash" is defensible.
- **Waybar's tray module is officially "still in beta"** (its own man page, checked 2026-08-18).
  It works and is what Hyprland users run, but breaking changes are fair game upstream.
- **The SNI spec itself never left draft** on freedesktop.org (last wiki edit 2021-05-07). It is
  a de-facto standard (KDE ships it as KF6::StatusNotifierItem), not a de-jure one.
- **Hyprland's config language changed in 0.55** (hyprlang `exec-once` deprecated in favour of
  Lua `hl.on("hyprland.start", …)`), so any autostart docs we write for Hyprland users need a
  version caveat.
- **Scope of "single instance" is per-user, not per-session, for every off-the-shelf mechanism.**
  `$XDG_RUNTIME_DIR` and the systemd user bus are shared by all concurrent logins of one user.
  If one user runs two seats/sessions, a naive lock blocks the second session's chibipop. Keying
  the lock file name by `$WAYLAND_DISPLAY` fixes this; decide whether we want that (Windows named
  mutexes are per-session, so per-`$WAYLAND_DISPLAY` matches today's behaviour most closely).

---

## 1. Tray: StatusNotifierItem

### Spec status

The tray on modern Linux is the **StatusNotifierItem (SNI)** protocol: three D-Bus roles —
`StatusNotifierItem` (the app), `StatusNotifierWatcher` (registry), `StatusNotifierHost` (the
bar that draws icons) — designed as the replacement for the old XEmbed systray spec
([freedesktop wiki](https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/)).
The spec page is a wiki draft, last edited 2021-05-07, and was never promoted to a versioned
freedesktop specification. In practice it *is* the standard: KDE ships a first-class
implementation as a KDE Frameworks 6 module
([KStatusNotifierItem](https://api.kde.org/kstatusnotifieritem-index.html)), and every Wayland
bar with a tray implements an SNI host. Menus ride along on the companion `com.canonical.dbusmenu`
interface (ksni bundles both, see its `spec/` directory).

There is **no Wayland-native tray protocol**; XEmbed is X11-only and dead on Wayland. SNI over
D-Bus is the only game in town.

### Rust crates

| Crate | Backend | Verdict |
| --- | --- | --- |
| [ksni](https://github.com/iovxw/ksni) | Pure Rust: zbus since 0.3.0 (2024-12-05) | **Recommended.** Implements `org.kde.StatusNotifierItem` + `com.canonical.dbusmenu` + DBus introspection/properties. 0.3.6 released 2026-07-15 ([CHANGELOG](https://github.com/iovxw/ksni/blob/master/CHANGELOG.md)); active cadence (5 releases in the last 14 months), protocol test suite run under isolated `dbus-run-session`, MSRV 1.80. Offers tokio (default), runtime-agnostic `async-io`, and a `blocking` feature — the blocking API fits chibipop's synchronous main loop without dragging in tokio. `Handle::update` mutates the live menu, which covers our `TrayCommand`{Settings, Quit} needs and more. Public domain (Unlicense) — GPL-3.0-compatible. |
| [tray-icon](https://github.com/tauri-apps/tray-icon) (tauri) | Linux: **GTK3 + libappindicator/libayatana** C libraries, needs a GTK event loop | **Rejected** — violates the pure-Rust/no-GTK constraint outright ([README](https://github.com/tauri-apps/tray-icon/blob/dev/README.md): "Linux (gtk Only)", requires `libgtk-3-dev libappindicator3-dev`). |
| `tray-item-rs`, `trayicon-rs` | Same story: libappindicator on Linux | Rejected for the same reason. |
| DIY on zbus | zbus is pure Rust | Possible but pointless: ksni is exactly this, already debugged against Plasma/GNOME-extension/libdbusmenu quirks (its repo vendors all three as reference submodules). |

Caveat to carry into implementation: ksni's README documents a zbus/tokio feature-unification
footgun ([z-galaxy/zbus#526](https://github.com/z-galaxy/zbus/issues/526)) — if any other
dependency in the Linux binary uses zbus *without* the tokio feature while ksni enables it, you
can get a runtime panic. If our capture/portal stack (per the sibling capture ticket) also uses
zbus, pick one arrangement deliberately: either all-tokio or ksni with
`default-features = false, features = ["async-io"]` (runtime-agnostic).

### Desktop coverage

- **Hyprland / wlroots bars:** the compositor itself has no tray; the bar is the
  StatusNotifierHost. **Waybar's `tray` module implements SNI** — its man page documents
  Passive/Active/NeedsAttention statuses, dbusmenu context menus, and known-working SNI applets
  (nm-applet, blueman, Electron apps), while warning "*tray* is still in beta"
  ([waybar-tray(5)](https://github.com/Alexays/Waybar/blob/master/man/waybar-tray.5.scd)).
  Other bars in that ecosystem (e.g. AGS/Astal, Quickshell, sfwbar) also ship SNI hosts; Waybar
  is the reference target since it is the de-facto default Hyprland bar. A Hyprland user with no
  bar at all gets no tray — same "must not be fatal" conclusion as GNOME.
- **KDE Plasma:** native, first-party. The protocol is KDE's own design; KF6 ships
  [KStatusNotifierItem](https://api.kde.org/kstatusnotifieritem-index.html) and Plasma's system
  tray is an SNI host. Best-supported target.
- **GNOME:** **no built-in SNI host.** GNOME removed default status-icon display in 3.26 and
  documented the design rationale (Allan Day, GNOME design,
  ["Status Icons and GNOME"](https://blogs.gnome.org/aday/2017/08/31/status-icons-and-gnome/),
  2017-08-31). Users install the Ubuntu-maintained
  ["AppIndicator and KStatusNotifierItem Support" extension](https://extensions.gnome.org/extension/615/appindicator-support/)
  (`gnome-shell-extension-appindicator`,
  [source](https://github.com/ubuntu/gnome-shell-extension-appindicator)) — 3.0M+ downloads,
  actively maintained: version 64 is Active for Shell 45–50 and v65 targets Shell 51 (version
  table checked 2026-08-18). So on GNOME the tray exists *only if* the user installed the
  extension; distros like Ubuntu preinstall it, Fedora Workstation does not.

### Consequence for chibipop

`src/ui/tray.rs` today says "Failing to create is fatal." On Linux that policy must flip: SNI
registration can fail benignly (no `StatusNotifierWatcher` on the bus — stock GNOME, bare
Hyprland without a bar) and can *appear* to fail transiently (bar starts after chibipop at
login). ksni 0.3.3+ has `assume_sni_available` to register before the watcher exists, and 0.3.0+
reports watcher online/offline transitions, so the Linux tray should be: spawn, tolerate offline,
keep the app alive regardless. Icon delivery also differs: SNI favours a **named icon per the
freedesktop icon theme** or ARGB32 pixmaps over D-Bus (`ksni::Icon`); our `.ico`-frame parser is
Windows-only and a PNG-decoded ARGB set (or an installed hicolor icon) replaces it.

## 2. Autostart

### The spec

The freedesktop [Desktop Application Autostart Specification](https://specifications.freedesktop.org/autostart-spec/latest/)
(v0.5, 2006 — itself still marked DRAFT) says: drop a Desktop Entry file in
`$XDG_CONFIG_HOME/autostart/` (default `~/.config/autostart/`) and the desktop environment
launches it at session start. Relevant keys: `Hidden=true` disables (and per-user files shadow
system ones by name), `OnlyShowIn=`/`NotShowIn=` gate by desktop, `TryExec=` skips the entry if
the binary is missing.

### Who honours it on Wayland

- **GNOME and KDE Plasma:** yes, natively — this is the mechanism their "startup applications"
  settings write. Under their systemd-managed sessions the actual execution is delegated to
  systemd: [`systemd-xdg-autostart-generator(8)`](https://www.freedesktop.org/software/systemd/man/latest/systemd-xdg-autostart-generator.html)
  converts each autostart `.desktop` into a user `.service` hooked to
  `xdg-desktop-autostart.target`, which "Desktop Environments can opt-in to use"
  ([systemd.special(7)](https://www.freedesktop.org/software/systemd/man/latest/systemd.special.html),
  target added in systemd 246). It processes `OnlyShowIn`/`NotShowIn` via `ExecCondition=`, so
  the same file behaves per-desktop under systemd too.
- **Hyprland:** does **not** implement XDG autostart itself. Native idiom: run commands on
  compositor start — since Hyprland 0.55 that is Lua, `hl.on("hyprland.start", function() hl.exec_cmd("…") end)`
  (previously hyprlang `exec-once`;
  [wiki Autostart page](https://github.com/hyprwm/hyprland-wiki/blob/main/content/Configuring/Basics/Autostart.md)).
  However, the Hyprland wiki's
  [Systemd startup page](https://github.com/hyprwm/hyprland-wiki/blob/main/content/Useful%20Utilities/Systemd-start.md)
  states plainly: "**XDG Autostart is handled by systemd, and its target is activated in
  uwsm-managed session automatically**" — i.e. Hyprland users who launch via
  [uwsm](https://github.com/Vladimir-csp/uwsm) (packaged in Arch, a NixOS `programs.hyprland.withUWSM`
  option) get our autostart `.desktop` for free. Non-uwsm users either add the `hl.exec_cmd`
  line, create a `hyprland-session.target` bound to `graphical-session.target` (same wiki page),
  or run a generic autostart runner like `dex`.
- **Sway and other bare wlroots compositors:** same shape — config `exec <shell command>` runs at
  startup ([sway(5)](https://github.com/swaywm/sway/blob/master/sway/sway.5.scd)); the man page
  contains no autostart-spec support, and users bridge to systemd the same way.

### systemd user units as the alternative

For any session that starts `graphical-session.target` (GNOME, KDE, uwsm-Hyprland,
hand-rolled `hyprland-session.target` setups), a user unit is the most robust option:

```ini
# ~/.config/systemd/user/chibipop.service
[Unit]
Description=chibipop hover dictionary
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=%h/.local/bin/chibipop run

[Install]
WantedBy=graphical-session.target
```

`systemd.special(7)` documents exactly this pattern: services tied to a graphical session should
be `PartOf=graphical-session.target` so they stop when the session ends. The Hyprland wiki
recommends per-app user services in this style, including `systemctl --user add-wants
graphical-session.target app.service` for units lacking an `[Install]` section. Bonus: stdout/
stderr of the unit land in the user journal (see §6).

### Recommendation

Ship both, prefer the spec file: a `chibipop autostart` subcommand (or settings toggle) that
writes `~/.config/autostart/chibipop.desktop` with `Exec=chibipop run` and `TryExec=chibipop` —
this covers GNOME, KDE, and uwsm-Hyprland with one file, honours the spec's per-user override
semantics, and is what a portal-free tray app can do without privileges. Document the systemd
unit and the raw `hl.exec_cmd` line for bare compositor setups. (A `--user` unit could even be
the *only* documented path for wlroots users, but the `.desktop` file must exist anyway for the
big desktops, so it is the primary artifact.)

## 3. Single instance (replacing the Win32 named mutex)

Windows today: `lock.rs` takes `CreateMutexW` + `ERROR_ALREADY_EXISTS` per library folder, and
`docs/BACKLOG.md` (2026-07-28/29 entries) records that a startup-wide guard is still wanted
because two `chibipop run` processes double-install input hooks. The Linux port needs both: the
per-library Apply lock and the app-wide guard. Three candidate mechanisms:

### a) `flock` on a file under `$XDG_RUNTIME_DIR` — recommended

[`flock(2)`](https://man7.org/linux/man-pages/man2/flock.2.html): advisory exclusive lock on an
open file description; "the lock is released either by an explicit `LOCK_UN` operation … or when
all such file descriptors have been closed." File descriptors are closed by the kernel on
process exit *however the process dies* — crash, SIGKILL, panic — so **there is no stale-lock
problem**: the lock file may persist on disk, but the lock itself cannot outlive the holder.
This is the same ownership semantics as the Win32 mutex (OS-owned, auto-released), unlike
PID-file schemes which *do* go stale and need PID-liveness heuristics.

Where the file lives matters. The [XDG Base Directory spec](https://specifications.freedesktop.org/basedir-spec/latest/)
requires `$XDG_RUNTIME_DIR` to be user-owned, mode 0700, on a local fully-featured filesystem
with file locking explicitly guaranteed, created at first login and removed at final logout.
That kills two classic flock failure modes: no other user can touch the file (no
denial-of-service by pre-creating/locking it, which a `/tmp` lock file suffers), and no
NFS/CIFS emulation weirdness (`flock(2)` documents both) because the spec forbids network
filesystems there. The spec's 6-hour cleanup caveat applies to *unused* files; a held lock's
file has an open fd and can also carry the sticky bit to be safe.

**Multi-seat/multi-session:** the spec says a user logging in more than once gets *the same*
`$XDG_RUNTIME_DIR`. So a bare `$XDG_RUNTIME_DIR/chibipop/run.lock` is **per-user**: user A on
seat 1 and user B on seat 2 never collide (different runtime dirs), but one user with two
concurrent graphical sessions would be limited to one chibipop total. A Win32 named mutex
without the `Global\` prefix lives in the per-session namespace — Microsoft documents "running
the application once per session is supported by default"
([Kernel Object Namespaces](https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces))
— so the closest behavioural match — and
the correct scope for an app whose job is bound to one compositor — is to key the lock per
display: `$XDG_RUNTIME_DIR/chibipop/run-$WAYLAND_DISPLAY.lock`. The per-library Apply lock, by
contrast, guards on-disk state shared across sessions and should stay per-user, keyed by a hash
of the library path (exactly mirroring today's per-folder mutex name).

Crate support: trivial with `rustix::fs::flock`/`nix`'s `Flock`, or the tiny
[`fd-lock`](https://docs.rs/fd-lock) wrapper; no reason this can't be ~40 lines like `lock.rs`
is today, behind the same `LibraryLock` interface plus a new startup guard.

### b) Abstract unix socket — rejected

What the [`single-instance`](https://github.com/WLBF/single-instance) crate does on Linux
("init will bind abstract unix domain socket with given name"): bind an `AF_UNIX` socket whose
name lives in the kernel-only abstract namespace; the bind fails with `EADDRINUSE` if another
instance holds it, and "abstract sockets automatically disappear when all open references … are
closed" ([unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html)) — so no stale state,
same as flock. Two disqualifiers:

1. **The namespace is system-global per network namespace, not per-user.** unix(7): "Socket
   permissions have no meaning for abstract sockets", and
   [network_namespaces(7)](https://man7.org/linux/man-pages/man7/network_namespaces.7.html)
   confirms the abstract namespace is scoped to the *netns*. Consequences: two different users
   running chibipop collide with each other (wrong), and any local user can squat
   `\0chibipop` first and deny startup (a real, if petty, DoS). Working around it means baking
   the UID into the name — at which point flock in the already-per-user runtime dir is simpler
   and portable to the BSDs, where the abstract namespace ("a nonportable Linux extension",
   unix(7)) doesn't exist.
2. Flatpak/container packaging shares or unshares netns in ways that silently change the
   guard's scope.

### c) D-Bus well-known name claim — good for IPC later, not as the guard

Claim `dev.chibipop.Chibipop` on the session bus with
[`RequestName`](https://dbus.freedesktop.org/doc/dbus-specification.html#bus-messages-request-name)
and `DBUS_NAME_FLAG_DO_NOT_QUEUE`; a `DBUS_REQUEST_NAME_REPLY_EXISTS` reply means another
instance owns it. Names are single-owner and the bus releases them when the connection drops, so
staleness is handled. zbus exposes this directly
([`Connection::request_name_with_flags`](https://docs.rs/zbus/latest/zbus/connection/struct.Connection.html)),
and chibipop will *have* a session-bus connection anyway for the ksni tray. Downsides as the
primary guard: scope is the user bus, which under systemd is explicitly **per-user, not
per-session** (the "user bus" of
[sd_bus_default(3)](https://www.freedesktop.org/software/systemd/man/latest/sd_bus_default.html),
living at `$XDG_RUNTIME_DIR/bus`) — same multi-session pinch as an unkeyed flock, but *not*
fixable by keying, since the name is one string on one shared bus; it requires a running session
bus (fine on desktops, an extra assumption in minimal wlroots setups); and it happens late-ish
in startup (after D-Bus connect) whereas the hook-double-install guard wants to be the first
thing `run` does. The genuinely valuable part — "second invocation forwards 'show settings' to
the first and exits" — is an IPC feature to build *on top of* the flock guard, whether over
D-Bus or over a plain socket at `$XDG_RUNTIME_DIR/chibipop/ipc-$WAYLAND_DISPLAY.sock`.

## 4. XDG base-directory mapping

Today everything sits beside the exe: `chibipop.toml` (`src/main.rs::default_config_path` →
`paths::beside_exe`), the built dictionary DB `data/chibipop.sqlite` and the `library/` of
dictionary archives (`paths::data_file`, `docs/REFERENCE.md`), and the self-updater writes the
replacement exe into the same folder (`src/update.rs`). That is a Windows portable-app layout;
on Linux the exe's directory (`/usr/bin`, `~/.cargo/bin`, a Nix store path…) is not writable and
the [basedir spec](https://specifications.freedesktop.org/basedir-spec/latest/) governs.

### Proposed split

| Content | Today (beside exe) | Linux home | Basedir rationale |
| --- | --- | --- | --- |
| `chibipop.toml` | `./chibipop.toml` | `$XDG_CONFIG_HOME/chibipop/chibipop.toml` (`~/.config/…`) | "user-specific configuration files" |
| Dictionary archives | `./library/*.zip` | `$XDG_DATA_HOME/chibipop/library/` (`~/.local/share/…`) | user data; irreplaceable-ish (user-curated), the source of truth for rebuilds |
| Built lookup DB | `./data/chibipop.sqlite` | `$XDG_DATA_HOME/chibipop/chibipop.sqlite` | regenerable from `library/` but expensive (a full rebuild is a long operation per `docs/REGRESSION.md`); cache semantics ("non-essential") would invite cleaners to delete it. Data, not cache. |
| Lookup log (new, §6) | console only | `$XDG_STATE_HOME/chibipop/chibipop.log` (`~/.local/state/…`) | spec names "actions history (logs, …)" as state data verbatim |
| Update downloads (`chibipop.update.zip`), OCR scratch | beside exe | `$XDG_CACHE_HOME/chibipop/` | "non-essential (cached) data"; safe to delete anytime |
| Locks, IPC socket (§3) | n/a | `$XDG_RUNTIME_DIR/chibipop/` | "communication and synchronization purposes … should not place larger files in it" |

Self-update note: on Linux the exe is typically not self-writable and belongs to a package
manager; the beside-exe `chibipop.new`/`chibipop.old` dance from `update.rs` should be
Windows-only or gated on the exe's directory being user-writable.

### Precedent

- **Neovim** is the cleanest documented model of the exact split above: `~/.config/nvim`,
  `~/.local/share/nvim`, `~/.local/state/nvim` (shada *and* `logs/nvim.log` under state),
  `~/.cache/nvim` ([:help base-directories](https://neovim.io/doc/user/starting.html)).
- **systemd's own spec text** (basedir v0.8, 2021, co-authored by Poettering) added
  `$XDG_STATE_HOME` precisely to house logs/history that used to be misfiled under data.
- **Anki** — chibipop's export target — keeps its per-user data (collections, media) in
  `~/.local/share/Anki2` on Linux, i.e. data-dir for the heavyweight corpus, matching our
  library/DB placement ([Anki manual, "File locations"](https://docs.ankiweb.net/files.html)).

Crates: [`dirs`](https://docs.rs/dirs/latest/dirs/) (6.0.0) maps all five (`config_dir`,
`data_dir`, `state_dir`, `cache_dir`, `runtime_dir`) straight to the XDG vars with spec
defaults; note `state_dir()`/`runtime_dir()` return `None` on Windows, which is fine because the
Windows build keeps its current layout. [`directories`](https://docs.rs/directories/latest/directories/)
adds the `ProjectDirs` qualifier convention; `etcetera` offers strategy choice. Any of the three
is adequate; `dirs` is the smallest.

### Migration and the portable-install story

chibipop's Windows identity is deliberately portable (README: "settings live in `chibipop.toml`
beside `chibipop.exe`"; `scripts/blank-copy.ps1` builds seedable folders). Users coming from
that world — and Linux users who run from a USB stick or a extracted tarball — will expect
beside-exe to keep working. The ecosystem precedent for "portable if a marker exists beside the
binary" is AppImage: a `MyApp.AppImage.config` directory beside the AppImage makes it
`$XDG_CONFIG_HOME` for that run
([AppImage portable mode docs](https://docs.appimage.org/user-guide/portable-mode.html)).

Proposed discovery order for the Linux binary, first match wins:

1. explicit `--config <path>` flag (already how tests drive alternate configs);
2. **portable mode:** `chibipop.toml` exists beside the exe → use beside-exe for *everything*
   (current `paths.rs` behaviour, unchanged; `data_file`'s cargo-root dev fallback keeps
   working);
3. **XDG mode:** otherwise use the table above, creating `$XDG_CONFIG_HOME/chibipop/` on first
   run with 0700 per the spec's write rule.

Migration: on first XDG-mode run, if a beside-exe layout is found *and* the exe dir is
read-only (tarball unpacked into `~/apps`, say), offer/perform a copy of `chibipop.toml`,
`library/` and `data/` into the XDG homes and log what moved — the same "prints what it kept"
courtesy the Windows refresh path already has (`docs/REFERENCE.md`). No silent dual-read of
both locations: precedence must stay decidable from one file's existence, or support burden
follows. Distro packaging can later ship defaults via `$XDG_CONFIG_DIRS/chibipop/` since the
spec's search order handles that for free.

## 5. Spawning helper processes (CREATE_NO_WINDOW equivalent)

**Confirmed non-problem.** `CREATE_NO_WINDOW` exists because a Windows GUI process spawning a
console-subsystem child (here: chibipop re-invoking itself for a rebuild,
`src/rebuild.rs` — "Or a black box flashes up") gets a console window materialised by the OS.
Linux has no console subsystem: a child of `std::process::Command` gets file descriptors and
nothing else; no window can appear. The `creation_flags(CREATE_NO_WINDOW.0)` call is already
`windows`-only API and simply has no Linux counterpart to write.

What the Linux spawn path *does* need:

- **stdio policy, unchanged in spirit.** Rust's default is "stdin, stdout and stderr are
  inherited from the parent"
  ([`Command::spawn` docs](https://doc.rust-lang.org/std/process/struct.Command.html#method.spawn)).
  The rebuild child must keep piping progress / writing to a file exactly as on Windows — the
  repo's own hard-won lesson applies verbatim on Linux ("a child that prints progress will fill
  a 4 KB pipe and stop", `docs/REGRESSION.md`; Linux pipe buffers are 64 KiB by default, bigger
  but just as finite). Helpers that shouldn't chat: `Stdio::null()`.
- **Terminal signal isolation, only where wanted.** A child inherits the process group; if
  chibipop was launched from a terminal, Ctrl-C delivers SIGINT to the whole foreground group,
  rebuild helper included — often *desired* (cancel everything). For the one spawn that must
  outlive the parent — `app.rs::start_run` re-launching `chibipop run` after Apply&Restart — the
  child should detach: `CommandExt::process_group(0)` or a full
  [`setsid(2)`](https://man7.org/linux/man-pages/man2/setsid.2.html) pre-exec (new session, no
  controlling terminal, so no SIGHUP when the old terminal closes). No double-fork daemonising
  needed; orphans reparent automatically.
- **Kill-child-with-parent, if we ever want the reverse guarantee:** Linux-only
  `prctl(PR_SET_PDEATHSIG, SIGTERM)` in a `pre_exec` hook
  ([prctl(2)](https://man7.org/linux/man-pages/man2/prctl.2.html)) — the OS otherwise leaves
  children running when the parent dies, unlike a Windows job object. Today's rebuild flow
  waits on the child, so this is optional hardening.
- **`current_exe()` keeps working**, with one documented self-update caveat: "If the executable
  is renamed while it is running, platforms may return the path at the time it was loaded
  instead of the new path" ([std docs](https://doc.rust-lang.org/std/env/fn.current_exe.html)) —
  so if the update flow survives on Linux, resolve the exe path once at startup, before any
  rename.

## 6. Lookup-log console equivalent

Windows chibipop toggles its *own* console window for the live lookup log
(`src/ui/console.rs`, careful to never claim a shell's console). Linux has no ownable console
window: if launched from a terminal, stdout/stderr go there; if launched by the session
(autostart, tray-only), there is no terminal at all, and nothing to "show". The conventional
replacement is two sinks:

- **stderr, always.** The Rust ecosystem default — `env_logger` "writes logs to stderr"
  ([docs](https://docs.rs/env_logger/latest/env_logger/)), `tracing_subscriber`'s fmt layer
  likewise defaults to stdout/stderr — and it composes with the session: when chibipop runs as a
  systemd user unit (§2), `StandardOutput=`/`StandardError=` default to `journal`
  ([systemd.exec(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html)
  via `DefaultStandardOutput=`), so `journalctl --user -u chibipop` gives the live log with
  timestamps, rotation, and rate-limiting for free. Terminal users get it interleaved in the
  terminal, exactly like the Windows console mode.
- **A logfile under `$XDG_STATE_HOME/chibipop/`**, e.g. `chibipop.log`, for the tray-launched,
  journal-less case (bare Hyprland via `hl.exec_cmd` has no unit and no terminal). The basedir
  spec names "logs" as the canonical example of state data
  ([spec §3](https://specifications.freedesktop.org/basedir-spec/latest/)); Neovim ships exactly
  this (`~/.local/state/nvim/logs/nvim.log`,
  [:help log](https://neovim.io/doc/user/starting.html)). chibipop should write the lookup log
  there (size-capped or truncate-on-start), and the tray's "Show lookup log" item becomes "open
  the logfile" (spawn `xdg-open`, or display the tail in-app) instead of ShowWindow/SW_HIDE.

Console show/hide code is thus Windows-only; the cross-platform seam is "a log sink that the
tray can reveal", with the Win32 console as one implementation and stderr+state-file as the
other.

## Sources

Specs and reference docs (all checked 2026-08-18):

- XDG Base Directory Specification v0.8 (2021-05-08) — https://specifications.freedesktop.org/basedir-spec/latest/
- Desktop Application Autostart Specification v0.5 (2006, DRAFT) — https://specifications.freedesktop.org/autostart-spec/latest/
- StatusNotifierItem spec (freedesktop wiki draft, last edited 2021-05-07) — https://www.freedesktop.org/wiki/Specifications/StatusNotifierItem/
- D-Bus Specification v0.43, `RequestName`/flags/reply codes — https://dbus.freedesktop.org/doc/dbus-specification.html#bus-messages-request-name
- systemd 261 man pages: systemd-xdg-autostart-generator(8) — https://www.freedesktop.org/software/systemd/man/latest/systemd-xdg-autostart-generator.html ; systemd.special(7) (`graphical-session.target`, `xdg-desktop-autostart.target`) — https://www.freedesktop.org/software/systemd/man/latest/systemd.special.html ; sd_bus_default(3) (per-user bus) — https://www.freedesktop.org/software/systemd/man/latest/sd_bus_default.html ; systemd.exec(5) (journal default) — https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html
- Linux man-pages 6.18: flock(2) — https://man7.org/linux/man-pages/man2/flock.2.html ; unix(7) (abstract namespace, permissions) — https://man7.org/linux/man-pages/man7/unix.7.html ; network_namespaces(7) (abstract-namespace scoping) — https://man7.org/linux/man-pages/man7/network_namespaces.7.html ; setsid(2) — https://man7.org/linux/man-pages/man2/setsid.2.html ; prctl(2) — https://man7.org/linux/man-pages/man2/prctl.2.html

Compositors, bars, desktops:

- Waybar tray module man page (SNI host, "still in beta") — https://github.com/Alexays/Waybar/blob/master/man/waybar-tray.5.scd
- Hyprland wiki, Autostart (Lua `hyprland.start`, hyprlang deprecated in 0.55) — https://github.com/hyprwm/hyprland-wiki/blob/main/content/Configuring/Basics/Autostart.md
- Hyprland wiki, Systemd startup (uwsm, `hyprland-session.target`, "XDG Autostart is handled by systemd") — https://github.com/hyprwm/hyprland-wiki/blob/main/content/Useful%20Utilities/Systemd-start.md
- sway(5) (`exec`/`exec_always`, no autostart-spec support) — https://github.com/swaywm/sway/blob/master/sway/sway.5.scd
- KDE Frameworks 6 KStatusNotifierItem — https://api.kde.org/kstatusnotifieritem-index.html
- Allan Day, "Status Icons and GNOME" (2017-08-31; GNOME 3.26 removes status icons by default) — https://blogs.gnome.org/aday/2017/08/31/status-icons-and-gnome/
- GNOME AppIndicator/KStatusNotifierItem extension (version/shell-support table; Shell 45–50 active, 51 pending as of 2026-08-18) — https://extensions.gnome.org/extension/615/appindicator-support/ ; source — https://github.com/ubuntu/gnome-shell-extension-appindicator

Crates:

- ksni (repo, README, spec/ and reference/ trees) — https://github.com/iovxw/ksni ; CHANGELOG (0.3.6 2026-07-15, zbus since 0.3.0 2024-12-05) — https://github.com/iovxw/ksni/blob/master/CHANGELOG.md ; docs — https://docs.rs/ksni/latest/ksni/
- tray-icon (Linux = GTK + libappindicator) — https://github.com/tauri-apps/tray-icon/blob/dev/README.md
- single-instance (abstract socket on Linux, mutex on Windows, flock on macOS) — https://github.com/WLBF/single-instance
- zbus `Connection::request_name_with_flags` — https://docs.rs/zbus/latest/zbus/connection/struct.Connection.html
- fd-lock — https://docs.rs/fd-lock ; dirs 6.0.0 — https://docs.rs/dirs/latest/dirs/ ; directories 6.0.0 — https://docs.rs/directories/latest/directories/
- env_logger (stderr default) — https://docs.rs/env_logger/latest/env_logger/

Precedent:

- Neovim `:help base-directories` / standard-path (state dir, `logs/nvim.log`) — https://neovim.io/doc/user/starting.html
- AppImage portable mode (`.home`/`.config` beside the file) — https://docs.appimage.org/user-guide/portable-mode.html
- Microsoft, Kernel Object Namespaces (per-session vs `Global\` named objects) — https://learn.microsoft.com/en-us/windows/win32/termserv/kernel-object-namespaces
- Anki manual, file locations (`~/.local/share/Anki2`) — https://docs.ankiweb.net/files.html
- Rust std: `Command::spawn` stdio inheritance — https://doc.rust-lang.org/std/process/struct.Command.html#method.spawn ; `env::current_exe` — https://doc.rust-lang.org/std/env/fn.current_exe.html

Repo grounding: `src/paths.rs`, `src/lock.rs`, `src/rebuild.rs`, `src/ui/tray.rs`,
`src/ui/console.rs`, `src/update.rs`, `src/app.rs::start_run`, `docs/BACKLOG.md`
(startup-guard gap), `docs/REGRESSION.md` (pipe-fill lesson), `docs/REFERENCE.md` (seeded-folder
layout), `scripts/blank-copy.ps1` (portable-install expectations).
