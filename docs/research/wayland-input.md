# Research: global cursor tracking and trigger keys on Wayland

Researched 2026-08-18. All support-status claims are dated; expect drift, especially in the
Hyprland ecosystem, which is moving fast (its InputCapture support is ~1 month old).

## Verdict

There is **no portable Wayland equivalent** of chibipop's Windows toolkit (low-level
mouse/keyboard hooks, `GetCursorPos` polling, `GetAsyncKeyState`). Wayland's core protocol
deliberately delivers pointer and keyboard events only to the focused/hovered surface, and the
two protocols that sound relevant — `relative-pointer` and `pointer-constraints` — are
**confirmed not applicable** to a background client (§5). Every viable strategy is per-family:

**Cursor tracking (Live mode):**

| Compositor family | Best channel | Permission / onboarding cost |
| --- | --- | --- |
| Hyprland | `hyprctl` socket `cursorpos` polling (works today, zero setup) or `ext-image-copy-capture-v1` pointer-cursor session (event-driven, Hyprland ≥ 0.54) | None by default. If the user enables Hyprland's opt-in permission system, cursor-position-via-protocol prompts (`cursorpos` permission, default ASK); the IPC socket is **not** gated |
| Sway / wlroots family | `ext-image-copy-capture-v1` pointer-cursor session (Sway 1.11, labwc 0.9.2, phoc, Mir, COSMIC, Jay, Treeland) | None — no prompt, same protocol family chibipop already needs for screen capture |
| KWin (Plasma) | ScreenCast portal with `cursor_mode=METADATA` (cursor position embedded in the PipeWire stream) | One screencast consent dialog; persistable with `restore_token` |
| GNOME | Same as KWin (ScreenCast portal metadata cursor mode) | Same |

**Trigger/modifier keys:**

| Compositor family | Best channel | Permission / onboarding cost |
| --- | --- | --- |
| Hyprland | GlobalShortcuts portal via XDPH + one config line (`global` dispatcher), or a plain `bind = ..., exec` poking chibipop's own socket | No prompt; user must paste one bind line into their config (Hyprland has no shortcut-configuration GUI dialog) |
| Sway / wlroots family | **No GlobalShortcuts portal** (xdg-desktop-portal-wlr implements only ScreenCast + Screenshot). Use native `bindsym [--release] exec chibipop-ctl ...` | No prompt; one or two config lines |
| KWin | GlobalShortcuts portal (since Plasma 5.27, 2023-02-14) | Portal dialog; user-editable in System Settings |
| GNOME | GlobalShortcuts portal (since GNOME 48, 2025-03-19) | Portal dialog; revocable in Settings → Apps. GNOME ≤ 47: nothing sane |

**Hold-a-bare-modifier mode (e.g. "show popup while Shift is held") cannot be expressed through
the portals**: the XDG shortcuts spec defines a trigger as modifiers *plus a key*. Bare-modifier
hold requires raw evdev access (`input` group ≈ user-level keylogger capability) or a
compositor-side bind on both press and release. Recommend shipping key-combo triggers via
portal/binds as the default and documenting evdev as an explicit power-user opt-in.

**InputCapture portal / libei: not a fit for observe-only use** — capturing means the compositor
*stops* using the events to move the on-screen cursor, and capture can only be triggered by
pointer barriers at screen edges, not on demand (§3). Ignore it for chibipop.

**Recommended combos:**
- *Live mode:* Hyprland → poll `cursorpos` at 10–20 Hz (simple, version-proof) and/or the
  image-copy-capture cursor session (shares the capture code path); other wlroots → cursor
  session; KWin/GNOME → cursor metadata from the ScreenCast stream chibipop already opens for OCR
  (position comes "for free" with the capture consent).
- *Trigger mode:* GlobalShortcuts portal where implemented (KDE, GNOME 48+, Hyprland/XDPH), with
  a documented native-keybind fallback (`exec`-style bind → chibipop control socket) that works
  on **every** compositor including bare wlroots ones. Both give press *and* release
  (`Activated`/`Deactivated`), so hold-to-show works with combo triggers.
- *Do not build on:* evdev cursor reconstruction (impossible, §2), InputCapture (§3),
  relative-pointer/pointer-constraints (§5).

---

## 1. Hyprland IPC (`hyprctl` socket)

**Mechanism.** Hyprland exposes two UNIX sockets under
`$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`: `.socket.sock` for hyprctl-style
request/response commands and `.socket2.sock` for a push event stream
([IPC wiki](https://wiki.hypr.land/IPC/), read 2026-08-18). A client writes
`[flag(s)]/command args` and reads the reply; `hyprctl -j cursorpos` returns JSON.

**`cursorpos` capabilities.** Documented as "gets the current cursor position in global layout
coordinates" ([Using hyprctl wiki](https://wiki.hypr.land/Configuring/Advanced-and-Cool/Using-hyprctl/)).
Source (v0.52.1 `src/debug/HyprCtl.cpp:1350`, identical on main in `src/ipc/s1/Commands.cpp:1237`):
returns `g_pInputManager->getMouseCoordsInternal().floor()` — i.e. **integer-floored logical
(layout) coordinates**, not physical pixels. Mapping to a capture crop must account for monitor
position/scale/transform (cross-reference the capture research doc). Known caveat: with
touchscreens the value is the touch-down position, not the current finger position
([Hyprland discussion #13735](https://github.com/hyprwm/Hyprland/discussions/13735)).

**Events?** The `.socket2.sock` event list contains window/workspace/monitor events only — **no
pointer-motion event** ([IPC wiki events table](https://wiki.hypr.land/IPC/)). Cursor tracking on
Hyprland is therefore poll-only.

**Poll cost.** Two documented warnings:
- "Hyprland evaluates connections to this socket completely synchronously … any unclosed
  connections *will cause Hyprland to freeze* until the five-second timeout is reached. Ensure
  that you always open the socket immediately before writing requests and close it afterward"
  ([IPC wiki](https://wiki.hypr.land/IPC/)).
- "hyprctl calls will be dispatched by the compositor *synchronously*, meaning any spam of the
  utility will cause slowdowns" ([Using hyprctl wiki](https://wiki.hypr.land/Configuring/Advanced-and-Cool/Using-hyprctl/)).

Each poll is a connect → write → read → close round trip serviced on the compositor loop. The
request itself is trivial (formats two ints), so a 10–30 Hz poll is realistic for hover-idle
detection; do not poll at input rate (hundreds of Hz), and never hold the connection open.

**Permissions.** None. Any process with the user's `$XDG_RUNTIME_DIR` can query it. Notably,
Hyprland's newer opt-in permission system (see below) does **not** gate the IPC socket: the
`PERMISSION_TYPE_CURSOR_POS` check appears only in `src/protocols/ImageCopyCapture.cpp` and
`src/managers/screenshare/CursorshareSession.cpp` (main branch, checked 2026-08-18), i.e. it
gates *Wayland-protocol* cursor exposure, not `hyprctl cursorpos`.

**Stability guarantees.** None formally; Hyprland is pre-1.0 (v0.56.2, 2026-08-05). Evidence of
churn: the whole hyprctl server implementation moved from `src/debug/HyprCtl.cpp` (≤ 0.52.x) to
`src/ipc/s1/` (main), and the config language switched from hyprlang to Lua ("Since Hyprland
0.55, hyprlang is deprecated in favor of lua" — [Permissions wiki](https://wiki.hypr.land/Configuring/Advanced-and-Cool/Permissions/)).
The `cursorpos` *command surface* has stayed stable through the rewrite and is wiki-documented,
so treat the command as stable-ish but pin integration tests against Hyprland releases. Rust
binding exists: [`hyprland-rs` 0.3.13](https://crates.io/crates/hyprland) (2025-09-24, unofficial).

**Other wlroots-family IPCs.** Sway's IPC has **no cursor-position query** — the full message
list is RUN_COMMAND, GET_WORKSPACES, SUBSCRIBE, GET_OUTPUTS, GET_TREE, GET_MARKS, GET_BAR_CONFIG,
GET_VERSION, GET_BINDING_MODES, GET_CONFIG, SEND_TICK, SYNC, GET_BINDING_STATE, GET_INPUTS,
GET_SEATS ([sway-ipc(7)](https://github.com/swaywm/sway/blob/master/sway/sway-ipc.7.scd), read
2026-08-18). `hyprctl cursorpos` is a Hyprland-specific luxury.

### 1b. The better Hyprland/wlroots channel: `ext-image-copy-capture-v1` cursor sessions

The staging protocol `ext-image-copy-capture-v1` has, besides frame capture, a
`create_pointer_cursor_session` request whose `ext_image_copy_capture_cursor_session_v1` emits
`enter`, `leave`, **`position`** and `hotspot` events; `position` is "the position of the
cursor's hotspot … relative to the main buffer's top left corner in transformed buffer pixel
coordinates" ([protocol spec](https://wayland.app/protocols/ext-image-copy-capture-v1)). That is
an **event-driven global cursor tracker** scoped to a capture source (e.g. an output) — exactly
what Live mode wants, and it rides the same protocol chibipop will use for capture.

Support (wayland.app matrix, snapshot read 2026-08-18): Sway 1.11, labwc 0.9.2, phoc 0.52,
Mir 2.26, COSMIC 1.0.0-beta.8, Jay, Treeland implement `ext_image_copy_capture_manager_v1` v1;
KWin 6.6, Mutter 49.2, niri 25.11, river 0.3.13, Wayfire 0.9.0 do **not**. The matrix shows
Hyprland 0.52.1 as unsupported, but that snapshot lags: Hyprland implemented
`image-capture-source-v1` and `image-copy-capture-v1` in **v0.54.0 (2026-02-27)** (release notes,
PR [#11709](https://github.com/hyprwm/Hyprland/pull/11709)).

Permission cost: zero prompt on Sway et al. On Hyprland, cursor sessions call
`clientPermissionMode(..., PERMISSION_TYPE_CURSOR_POS)` (`src/protocols/ImageCopyCapture.cpp:308,479`).
The permission system is **off by default** (`ecosystem.enforce_permissions`, default disabled);
when a user enables it, `cursorpos` defaults to ASK — "Access to your cursor position and cursor
image via Wayland protocols directly … Prevents apps from silently tracking where your cursor is"
([Permissions wiki](https://wiki.hypr.land/Configuring/Advanced-and-Cool/Permissions/), read
2026-08-18). Plan for a one-time allow prompt on hardened Hyprland setups.

### 1c. ScreenCast portal cursor metadata (the KWin/GNOME channel)

The ScreenCast portal's `cursor_mode` value `4` (METADATA) transmits "the cursor as metadata in
the PipeWire stream" (`spa_meta_cursor`, includes position) — this is how OBS et al. draw remote
cursors. Backend reality, verified at source level 2026-08-18:
- **xdg-desktop-portal-wlr: unsupported.** `src/screencast/screencast.c` cancels the request:
  `if (cursor_mode & METADATA) { "dbus: unsupported cursor mode requested, cancelling" }`;
  default is EMBEDDED.
- **xdg-desktop-portal-hyprland: unsupported.** `src/portals/Screencopy.cpp` logs
  "unsupported cursor_mode {}, fallback" for METADATA (config `screencopy:cursor_mode` exists
  since XDPH v1.4.1, 2026-07-29, but METADATA still falls back).
- **GNOME (mutter) and KDE (KWin)** advertise metadata cursor mode; on those desktops the OCR
  screencast chibipop opens anyway doubles as the cursor tracker at no extra permission cost.
  (Detailed per-backend cursor-mode matrix belongs to the capture research doc; treat that doc
  as authoritative for capture specifics.)

Net: cursor-in-stream works exactly where `ext-image-copy-capture` does **not** (KWin, GNOME),
and vice versa. The two channels together cover every target family.

## 2. Direct libinput/evdev reading

**What it yields.** Kernel evdev devices (`/dev/input/event*`) deliver raw device events: mice
emit **relative** deltas (`EV_REL`/`REL_X`/`REL_Y`), touchscreens/tablets absolute device
coordinates, keyboards `EV_KEY`. For trigger keys this is *full Windows parity*: the
`EVIOCGKEY` ioctl returns the "global key state" snapshot (`include/uapi/linux/input.h:173`) —
a direct `GetAsyncKeyState` equivalent — and press/release events stream in real time regardless
of focus.

**The cursor-position reconstruction problem.** There is no cursor in evdev. The on-screen
cursor position is a *compositor* artifact: libinput applies device-specific acceleration
("Pointer motion acceleration is device- and configuration-specific" —
[relative-pointer spec](https://wayland.app/protocols/relative-pointer-unstable-v1) makes the
same accel/normalization distinction), then the compositor applies monitor layout, scale,
transforms, edge clamping, pointer constraints, and programmatic warps (e.g.
`hyprctl dispatch movecursor`, portal remote-desktop injection). Integrating raw deltas in a
second process cannot reproduce that pipeline and has no resync channel, so reconstruction
drifts immediately and permanently. **Verdict: evdev is unusable for cursor position, usable
(with caveats) for key state.**

**Permissions.** Device nodes are `root:input`, mode 0660: systemd udev default rule
`SUBSYSTEM=="input", GROUP="input"` (`rules.d/50-udev-default.rules.in:51`, systemd main,
read 2026-08-18); verified locally on Arch (kernel 7.0.11-zen1): `crw-rw---- root input
/dev/input/event0`, and the default desktop user is *not* in the `input` group. Onboarding cost:
`usermod -aG input $USER` + relogin, or a root helper daemon. The clean logind route is closed:
`TakeDevice()` may only be called by the session controller, and "Only one D-Bus connection can
be a controller for a given session at any time" — that connection is the compositor
([org.freedesktop.login1 D-Bus API](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html)).
Using libinput-the-library changes nothing: its udev/path contexts still need read access to the
same nodes. Flatpak can expose them with `--device=input` (Flatpak ≥ 1.15.6,
[sandbox permissions docs](https://docs.flatpak.org/en/latest/sandbox-permissions.html)).

**Security implications.** `input`-group membership grants every process running as the user
read access to **all** keyboards — i.e. system-wide keylogging, precisely the capability
Wayland's design removes (KDE's portal rationale: "on Wayland … we are not able to run a
system-wide keylogger to get the global shortcuts triggers",
[mumble PR #5976](https://github.com/mumble-voip/mumble/pull/5976)). There is no
granularity: reading one modifier key costs the same trust as reading passwords. If chibipop
ships this at all, it must be opt-in, loudly documented, and never the default path.

## 3. libei/EIS and the InputCapture portal

**Architecture.** libei is the emulated-input transport for the Wayland stack: `libei` (client),
`libeis` (compositor side), UNIX-socket binary protocol. Since v0.3 a libei context can be a
*receiver* — "the libei context gets a list of devices from the EIS implementation and
*receives* events from those. This allows for input capture, provided the EIS implementation
supports it." libei explicitly "does not provide a method for deciding when events should be
captured, it merely provides the transport layer"
([libei README](https://gitlab.freedesktop.org/libinput/libei/-/blob/main/README.md)). The
decision logic is the **InputCapture portal** (`org.freedesktop.portal.InputCapture`,
introduced in xdg-desktop-portal 1.17.0, 2023-08-04, per
[NEWS](https://github.com/flatpak/xdg-desktop-portal/blob/main/NEWS.md); interface now at v2 with
session persistence/restore tokens).

**Why it does not fit observe-only use.** From the portal spec
([docs](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)):
- "Once the compositor activates input capturing, events … are sent directly to the application
  **instead of using those events to update the pointer position on-screen**." Capturing steals
  the user's input; chibipop must leave input flowing normally.
- Activation is trigger-based: "the only defined trigger are pointer barriers … lines on the
  screen" and "There is currently no way for an application to activate immediate input
  capture." Designed for InputLeap/Synergy edge-crossing, not for a hover dictionary.

**Compositor support today (2026-08-18).**
- GNOME: backend since xdg-desktop-portal-gnome 45.beta / mutter 45 EI support
  (mutter MR [!2628](https://gitlab.gnome.org/GNOME/mutter/-/merge_requests/2628)); commonly
  stated as solid from GNOME 46. Known wart: `Disable` left sessions broken
  (mutter issue [#3908](https://gitlab.gnome.org/GNOME/mutter/-/work_items/3908), 2024-03) —
  maturity is "works for InputLeap", not bulletproof.
- KDE: Plasma **6.1** (2024-06-18) "Add support for input capturing for the portal"
  ([Plasma 6.1.0 changelog](https://kde.org/announcements/changelogs/plasma/6/6.0.5-6.1.0/));
  KWin's libeis backend deliberately exposes no public socket — clients must come through the
  portal (KWin MR [!5496](https://invent.kde.org/plasma/kwin/-/merge_requests/5496)).
- Hyprland: **brand new** — compositor protocol `hyprland_input_capture_v1` ("close reproduction
  of the xdg input capture portal", barriers on screen edges, events over an EIS socket,
  [hyprland-protocols XML](https://github.com/hyprwm/hyprland-protocols/blob/main/protocols/hyprland-input-capture-v1.xml))
  landed in Hyprland **v0.56.0 (2026-07-20)** (release notes: "input-capture: impl protocol
  (#7919)"), portal side in XDPH **v1.4.0 (2026-07-18)** ("Implement Input Capture Desktop
  Portal", PR [#268](https://github.com/hyprwm/xdg-desktop-portal-hyprland/pull/268)).
- Sway/wlroots: **not implemented** — xdg-desktop-portal-wlr still ships only ScreenCast +
  Screenshot ([README](https://github.com/emersion/xdg-desktop-portal-wlr), read 2026-08-18);
  feature request [#278](https://github.com/emersion/xdg-desktop-portal-wlr/issues/278) open
  since 2023-08. COSMIC likewise open
  ([xdg-desktop-portal-cosmic #217](https://github.com/pop-os/xdg-desktop-portal-cosmic/issues/217)).

**Rust support:** [`reis` 0.7.1](https://crates.io/crates/reis) (2026-07-30) is a pure-Rust
EI/EIS implementation ("usable for both clients and servers, but the API is subject to change" —
[README](https://github.com/ids1024/reis)); [`ashpd` 0.13.13](https://docs.rs/ashpd) (2026-07-17)
wraps the `input_capture` portal. So the plumbing exists in Rust — the semantics just don't fit.

**Conclusion:** skip for chibipop v1. Re-evaluate only if a future "immediate capture" /
observe-mode extension appears upstream.

## 4. GlobalShortcuts portal

**Semantics.** `org.freedesktop.portal.GlobalShortcuts` (introduced in xdg-desktop-portal
1.16.0, 2022-12-12, per [NEWS](https://github.com/flatpak/xdg-desktop-portal/blob/main/NEWS.md);
interface v2 adds `ConfigureShortcuts`): an app creates a session, `BindShortcuts` with ids +
optional `preferred_trigger`, "typically result[ing in] the portal presenting a dialog"; then the
app receives **`Activated` and `Deactivated` signals with millisecond timestamps**
([spec](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)).
Press *and* release delivery means hold-to-show trigger mode works. Shortcuts survive per-app in
the portal permission store (`ListShortcuts` returns previously bound ones).

**Trigger syntax limitation.** `preferred_trigger` follows the freedesktop
[shortcuts spec](https://specifications.freedesktop.org/shortcuts-spec/latest/) (v0.1 DRAFT,
2022-05-11): "A shortcut is comprised by a set of modifiers (CTRL, ALT, SHIFT, NUM, LOGO) …
**together with a key identifier**". A bare modifier ("hold Shift") is not a valid trigger; the
Windows-style bare-modifier trigger must be dropped or routed through evdev (§2).

**Compositor coverage (2026-08-18).**
- **KDE**: first implementer — xdg-desktop-portal-kde MR
  [!80](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/merge_requests/80) (merged
  2022-11-30), shipped in Plasma 5.27 (2023-02-14); built on KGlobalAccel + System Settings;
  since Plasma 6.3-era MR [!368](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/merge_requests/368)
  a bind shows a customization dialog instead of dumping users into System Settings.
- **GNOME**: xdg-desktop-portal-gnome 48 "Add global shortcuts portal backend"
  ([NEWS](https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/NEWS)); GNOME 48 release
  notes (2025-03-19): apps must request permission, shortcuts editable/revocable in Settings →
  Apps ([release notes](https://release.gnome.org/48/)).
- **Hyprland**: implemented in XDPH (`src/portals/GlobalShortcuts.cpp`), backed by the
  compositor protocol `hyprland-global-shortcuts-v1` — "A global shortcut is anonymous, meaning
  the app does not know what key(s) trigger it. The shortcut's keybinding shall be dealt with by
  the compositor" ([protocol](https://wayland.app/protocols/hyprland-global-shortcuts-v1);
  Hyprland-only in the wayland.app matrix). **No dialog**: the user lists registered shortcuts
  with `hyprctl globalshortcuts` and binds them manually, e.g.
  `hl.bind("SUPER + SHIFT + A", hl.dsp.global("chibipop:lookup"))` — "this function will *only*
  work with XDPH" ([Binds wiki](https://wiki.hypr.land/Configuring/Basics/Binds/)). Onboarding =
  one config line; zero prompts.
- **Sway / generic wlroots (river, Wayfire, labwc, …)**: **no support** — xdpw implements only
  ScreenCast/Screenshot (README, read 2026-08-18) and no other wlr backend fills the gap.
  Community tracker: [Are We GlobalShortcuts Yet?](https://areweglobalshortcutsyet.github.io/).

**Fit for chibipop's trigger-key mode:** good wherever implemented (combo triggers, press +
release, standard consent UX, works in Flatpak). Rust: `ashpd::desktop::global_shortcuts`
([docs.rs 0.13.13](https://docs.rs/ashpd/latest/ashpd/desktop/global_shortcuts/index.html)).

**Universal fallback: native compositor keybinds.** Every target compositor can bind a key to
run a command; point it at chibipop's own control socket (`chibipop-ctl toggle` / `lookup`):
- Sway: `bindsym` with `--release` for hold semantics
  ([sway(5)](https://github.com/swaywm/sway/blob/master/sway/sway.5.scd): `*bindsym*
  [--release] … <key combo> <command>`).
- Hyprland: `hl.bind(...)` / bind flags incl. release variants ([Binds wiki](https://wiki.hypr.land/Configuring/Basics/Binds/)).
Zero permission, works offline of any portal; cost = documented config snippet per compositor.

## 5. Generic Wayland protocols for a client without focus — confirmed NOT applicable

- **`relative-pointer-unstable-v1`**: the spec is explicit — a `wp_relative_pointer` "shares the
  same focus as wl_pointer objects of the same seat and **will only emit events when it has
  focus**" ([spec](https://wayland.app/protocols/relative-pointer-unstable-v1)). A background
  chibipop never has pointer focus ⇒ no events. Also relative-only (deltas), so even with focus
  it wouldn't give a global position. **Not applicable — confirmed.**
- **`pointer-constraints-unstable-v1`**: lock/confine requests are bound to *your own surface*
  and activate only when that surface has pointer focus ("Whenever the lock is activated, it is
  guaranteed that the locked surface will already have received pointer focus",
  [spec](https://wayland.app/protocols/pointer-constraints-unstable-v1)). It is a tool for games
  to trap the cursor, not to observe it. **Not applicable — confirmed.**
- **Core `wl_pointer`/`wl_keyboard`**: events are delivered on surface enter/focus only; a
  background client receives nothing ([core protocol](https://wayland.app/protocols/wayland)).
- **Transparent full-screen layer-shell overlay** (considered and rejected): a surface only
  receives pointer events inside its input region, and events delivered to it are *not* passed
  to surfaces below — so a tracking overlay would swallow the user's clicks, and an
  empty-input-region overlay receives nothing. [INFERENCE from core input-region semantics;
  no compositor delivers pointer events to two surfaces.] Not viable.
- Injection-side protocols (`wlr-virtual-pointer`, `virtual-keyboard`, KDE fake-input) are
  write-only equivalents of `SendInput` — irrelevant to observation.

## 6. KWin-specific notes

- **Cursor position without a screencast**: KWin has no public "cursorpos" D-Bus call, but the
  KWin *scripting* API can read it; [kdotool](https://github.com/jinliu/kdotool) implements
  `getmouselocation` by injecting temporary KWin scripts over D-Bus (README, read 2026-08-18).
  Works on X11 and Wayland Plasma 5/6, no prompt — but it is an automation hack with no
  stability contract; prefer the ScreenCast metadata channel chibipop needs anyway.
- **Trigger keys**: the GlobalShortcuts portal *is* the KDE-native path (backed by KGlobalAccel,
  §4). No reason to bind KGlobalAccel directly from Rust.
- **InputCapture**: supported since Plasma 6.1 (§3) — same observe-only mismatch as everywhere.
- KWin 6.6 does **not** implement `ext-image-copy-capture-v1` (wayland.app matrix, 2026-08-18),
  so the wlroots cursor-session trick does not port.

## Open risks and disagreements

1. **wayland.app lag vs. reality**: its matrix (scanned at Hyprland 0.52.1) says Hyprland lacks
   `ext-image-copy-capture-v1`; Hyprland release notes prove support since v0.54.0. Where the
   two disagreed, this doc follows release notes/source.
2. **Hyprland churn**: IPC internals were rewritten (0.52 → main), config language replaced
   (0.55), permission system is new and off-by-default but explicitly aimed at cursor-position
   privacy. Assume Hyprland may eventually gate or prompt more of these channels; keep the
   portal/ScreenCast-metadata path as strategic fallback.
3. **Phoronix vs. NEWS on InputCapture intro** (1.18 vs 1.17.0): xdg-desktop-portal NEWS.md says
   1.17.0 (2023-08-04); trusted over press coverage.
4. **GNOME < 48 trigger keys**: no portal, no IPC; only evdev or "no trigger mode on old GNOME".
   Given GNOME is lowest priority, recommend documenting GNOME 48+ as the supported floor.
5. **Sway/wlr trigger UX**: no portal means onboarding is manual config editing. Acceptable for
   the target audience, but it is a real papercut to document.
6. **Hyprland `cursorpos` IPC ungated**: current fact (main, 2026-08-18) verified in source, but
   it is exactly the kind of hole the new permission system exists to close — re-verify per
   release.
7. **reis API instability**: self-declared "API is subject to change"; pin the version.

## Sources

- Hyprland IPC wiki — https://wiki.hypr.land/IPC/ (read 2026-08-18)
- Hyprland "Using hyprctl" wiki — https://wiki.hypr.land/Configuring/Advanced-and-Cool/Using-hyprctl/
- Hyprland Permissions wiki — https://wiki.hypr.land/Configuring/Advanced-and-Cool/Permissions/
- Hyprland Binds wiki (global dispatcher) — https://wiki.hypr.land/Configuring/Basics/Binds/
- Hyprland source: `src/debug/HyprCtl.cpp` (v0.52.1), `src/ipc/s1/Commands.cpp`,
  `src/protocols/ImageCopyCapture.cpp`, `src/managers/screenshare/CursorshareSession.cpp`,
  `src/managers/permissions/DynamicPermissionManager.cpp` (main, 2026-08-18) —
  https://github.com/hyprwm/Hyprland
- Hyprland releases v0.54.0 (image-copy-capture), v0.56.0 (input-capture), v0.56.2 —
  https://github.com/hyprwm/Hyprland/releases
- hyprctl cursorpos touchscreen caveat — https://github.com/hyprwm/Hyprland/discussions/13735
- sway-ipc(7) — https://github.com/swaywm/sway/blob/master/sway/sway-ipc.7.scd
- sway(5) bindsym — https://github.com/swaywm/sway/blob/master/sway/sway.5.scd
- relative-pointer-unstable-v1 — https://wayland.app/protocols/relative-pointer-unstable-v1
- pointer-constraints-unstable-v1 — https://wayland.app/protocols/pointer-constraints-unstable-v1
- ext-image-copy-capture-v1 (cursor session + support matrix) —
  https://wayland.app/protocols/ext-image-copy-capture-v1
- hyprland-global-shortcuts-v1 — https://wayland.app/protocols/hyprland-global-shortcuts-v1
- hyprland-input-capture-v1 XML —
  https://github.com/hyprwm/hyprland-protocols/blob/main/protocols/hyprland-input-capture-v1.xml
- GlobalShortcuts portal spec —
  https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html
- InputCapture portal spec —
  https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html
- xdg-desktop-portal NEWS (1.16.0 GlobalShortcuts, 1.17.0 InputCapture) —
  https://github.com/flatpak/xdg-desktop-portal/blob/main/NEWS.md
- xdg-desktop-portal-wlr README + screencast source (METADATA rejected) —
  https://github.com/emersion/xdg-desktop-portal-wlr ; issue #278 (InputCapture request)
- xdg-desktop-portal-hyprland: portals list, Screencopy.cpp, InputCapture.hpp, releases v1.4.0 /
  v1.4.1 — https://github.com/hyprwm/xdg-desktop-portal-hyprland
- xdg-desktop-portal-kde GlobalShortcuts MR !80 —
  https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/merge_requests/80 ; workflow MR !368
- KWin libeis backend MR !5496 — https://invent.kde.org/plasma/kwin/-/merge_requests/5496
- Plasma 5.27 announcement (2023-02-14) — https://kde.org/announcements/plasma/5/5.27.0/
- Plasma 6.1.0 changelog (input capture) —
  https://kde.org/announcements/changelogs/plasma/6/6.0.5-6.1.0/
- GNOME 48 release notes — https://release.gnome.org/48/ and
  https://release.gnome.org/48/developers/
- xdg-desktop-portal-gnome NEWS — https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/NEWS
- mutter EI MR !2628 — https://gitlab.gnome.org/GNOME/mutter/-/merge_requests/2628 ;
  issue #3908 — https://gitlab.gnome.org/GNOME/mutter/-/work_items/3908
- libei README — https://gitlab.freedesktop.org/libinput/libei/-/blob/main/README.md
- freedesktop shortcuts spec v0.1 — https://specifications.freedesktop.org/shortcuts-spec/latest/
- systemd udev rules (input group) —
  https://github.com/systemd/systemd/blob/main/rules.d/50-udev-default.rules.in
- Linux `include/uapi/linux/input.h` (EVIOCGKEY) —
  https://github.com/torvalds/linux/blob/master/include/uapi/linux/input.h
- logind D-Bus API (TakeControl/TakeDevice) —
  https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html
- Flatpak sandbox permissions (`--device=input`, 1.15.6) —
  https://docs.flatpak.org/en/latest/sandbox-permissions.html
- kdotool README — https://github.com/jinliu/kdotool
- reis crate — https://crates.io/crates/reis ; https://github.com/ids1024/reis
- ashpd crate — https://docs.rs/ashpd/latest/ashpd/desktop/
- hyprland-rs crate — https://crates.io/crates/hyprland
- Are We GlobalShortcuts Yet (tracker) — https://areweglobalshortcutsyet.github.io/
- Local verification (Arch Linux, 2026-08-18): `/dev/input/event0` = `crw-rw---- root input`;
  default user not in `input` group.
