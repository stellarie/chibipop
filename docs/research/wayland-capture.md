# Research: Wayland screen capture for high-frequency region grabs

Research date: **2026-08-18**. All support claims are cited; version numbers are the ones the
cited source was checked against.

**Requirement under test ("hover loop"):** grab a small region (~500×100 px) around the cursor,
repeatedly (several per second, bursty), at interactive latency, with **no per-use consent
prompt**.

---

## Verdict

1. **`zwlr_screencopy_manager_v1` (wlr-screencopy v3) is the primary mechanism and the only one
   that natively does compositor-side region capture.** `capture_output_region(x, y, w, h)` grabs
   exactly the box we need, in output-logical coordinates. It ships today at protocol version 3 on
   Hyprland (0.52.1), Sway (1.11), niri (25.11), labwc, river, Wayfire, phoc, Mir, Treeland,
   Louvre, Jay, and Cage — essentially the whole wlroots family plus Hyprland's independent
   implementation. It has **no consent prompt at all by default**; Hyprland 0.49+ has an opt-in
   permission system that at worst asks once per app run, never per frame. It is spec-deprecated
   in favor of ext-image-copy-capture-v1, but nobody has removed it and it remains the only
   region-capable protocol even where the successor exists. **PASSES the hover-loop requirement**
   on the wlroots family; unavailable on GNOME (Mutter) and KDE (KWin).

2. **`ext-image-copy-capture-v1` is the designated successor but is NOT usable as our primary
   mechanism today.** (a) v1 has **no region/crop capability** — capture sources are whole outputs
   or whole toplevels and the client buffer must match the full source size, so a 500×100 crop
   means capturing the full monitor and cropping client-side; (b) coverage is still partial:
   **Hyprland implemented it only in 0.54.0 (released 2026-02-27, PR #11709)**, and niri (open
   issue #1558), KWin (open KDE bug 513785), and Mutter still lack it. Adopters as of 2026-08:
   Hyprland ≥0.54, COSMIC, Sway 1.11 (wlroots 0.19), labwc, Mir, phoc, Treeland, Jay. Its session
   model (persistent buffers, bidirectional damage tracking) is architecturally *better* for a
   capture loop than wlr-screencopy — build the abstraction so this backend can slot in, but the
   missing region capture keeps wlr-screencopy primary. No removal/obsolescence timeline exists
   for wlr-screencopy beyond the spec's deprecation note.

3. **xdg-desktop-portal ScreenCast + PipeWire is the only path on GNOME and KDE, and it is
   workable but second-class for this use case.** One consent dialog on `Start`; with
   `persist_mode=2` + `restore_token` the dialog is skipped on subsequent runs (token rotates on
   each use). GNOME has supported restoration since GNOME 42 (2022); KDE implements it (with a
   reported reboot-persistence bug in early 2024 for remote-desktop sessions);
   xdg-desktop-portal-hyprland supports tokens (`allow_token_by_default`);
   xdg-desktop-portal-wlr (Sway) only supports *transient* restore via config — but on Sway we
   would use wlr-screencopy anyway. The portal has **no region source type** (MONITOR / WINDOW /
   VIRTUAL only), so we must stream the full monitor over PipeWire and crop client-side, and the
   stream must be kept open permanently because session setup (D-Bus + PipeWire negotiation) is
   far too slow to do per burst. **Acceptable as the GNOME/KDE fallback tier; not per-use-prompt
   free on first run.**

4. **Recommended architecture for chibipop-linux:** a `RegionCapture` trait with three backends,
   selected at runtime by advertised globals / desktop environment:
   - **Backend A (primary): wlr-screencopy region capture** — Hyprland (reference), Sway, niri,
     and the rest of the wlroots family. Cheap: one ~200 KB shm copy per grab.
   - **Backend B: ext-image-copy-capture session + client-side crop** — Hyprland ≥0.54
     (2026-02-27), Sway 1.11, COSMIC, and others as they adopt; becomes primary once a
     region-source extension appears.
   - **Backend C (fallback): portal ScreenCast + PipeWire persistent stream + client-side crop** —
     GNOME/KDE, one consent dialog on first run, token-restored afterwards.
   This ordering also matches the map's priorities (wlroots-family first-class, GNOME lowest).

---

## 1. zwlr_screencopy_manager_v1 (wlr-screencopy, protocol version 3)

### Region capture semantics

- `capture_output_region(frame, overlay_cursor, output, x, y, width, height)` captures "the next
  frame of an output's region. The region is given in output logical coordinates, see
  xdg_output.logical_size. The region will be clipped to the output's extents."
  ([spec][wlr-screencopy]) Logical coordinates mean the client must translate a cursor position
  (surface/global coords) into the target output's logical space and handle fractional scale →
  the returned buffer is in *pixel* dimensions announced by the `buffer` event.
- Flow per grab: create `zwlr_screencopy_frame_v1` → compositor advertises buffer parameters
  (`buffer` for wl_shm, `linux_dmabuf` since v3, `buffer_done`) → client submits a matching
  `wl_buffer` via `copy` (or `copy_with_damage`) → compositor replies `flags` + `ready`
  (with presentation timestamp) or `failed`. Frame objects are single-use; the client destroys and
  recreates per grab. ([spec][wlr-screencopy])
- **Damage variant:** `copy_with_damage` (since v2) "waits until there is damage to copy"; the
  `damage` event then reports damaged rectangles accumulated since the last copy request "derived
  from the current screencopy manager instance". ([spec][wlr-screencopy]) For a hover loop this is
  a double-edged sword: it is exactly the "only OCR when the text changed" primitive, but if the
  screen is static the copy blocks indefinitely — so the loop needs a plain-`copy` path (or a
  timeout/race) when the cursor moves to a *new* region whose contents haven't changed.

### Per-frame cost (Hyprland, current `main`, post-wlroots/aquamarine)

Hyprland has been fully independent of wlroots since v0.42 (aquamarine backend; wlroots removal
completed July 2024) and implements wlr-screencopy in-tree
(`src/protocols/Screencopy.cpp`). ([Hyprland announcement][hypr-independent],
[Phoronix on 0.42][phoronix-042], [Screencopy.cpp][hypr-screencopy-src]) Reading the source
(fetched 2026-08-18):

- `capture_output_region` is routed to a **managed screenshare session keyed on
  (client, monitor, region box)** (`Screenshare::mgr()->getManagedSession(client, monitor, box)`),
  so repeated grabs of the same region by the same client reuse a session and its damage
  accounting. ([Screencopy.cpp][hypr-screencopy-src])
- A plain `copy` (no damage-wait) calls `g_pHyprRenderer->damageMonitor(...)` — i.e. **each
  non-damage grab forces a full-monitor repaint** so a fresh frame exists to copy from, then blits
  the requested region into the client buffer. Cost per grab ≈ one compositor frame render + one
  region-sized copy. At several grabs/second this is trivial on desktop GPUs but is a measurable
  wakeup/power cost on laptops — worth preferring `copy_with_damage` with a fallback when
  idle-blocked. ([Screencopy.cpp][hypr-screencopy-src])
- shm and dmabuf buffer types are both offered (`sendBuffer` + `sendLinuxDmabuf` at v3). For a
  500×100 region an shm ARGB copy is ~200 KB — memcpy noise; no GPU readback path needed on the
  client. ([Screencopy.cpp][hypr-screencopy-src])
- Latency: the copy completes at the next output commit after the (forced) repaint — bounded by
  one refresh cycle (≈7–16 ms at 60–144 Hz), well within "interactive".
  [INFERENCE from the damage→render→share flow in the source; not benchmarked.]

### Consent model

- The protocol itself has **no consent mechanism**: any client that can bind the global can
  capture. This is the historical wlroots-family stance (privilege = access to the socket).
- **Hyprland 0.49.0 (released 2025-05-08)** added an opt-in permission system:
  `ecosystem:enforce_permissions = 1` plus `permission = <binary-regex>, screencopy, allow|ask|deny`.
  Default is **off** (everything allowed, no prompts). With `ask`, a dialog appears per app; the
  user can grant "until the app exits" or "until Hyprland exits" — i.e. **never per-frame**.
  Config-set permissions require a Hyprland restart. ([Hyprland wiki: Permissions][hypr-perms],
  [Hyprland 0.49 release notes, PR #9930][hypr-049], [tracking issue #9915][hypr-9915])
- For packaging: chibipop should document a recommended
  `permission = .*/chibipop, screencopy, allow` line for users who enable enforcement.
- Sway/wlroots: no permission gating at all as of Sway 1.11 (no prompt, ever).

### Who ships it (wayland.app compositor matrix, site data for the versions listed, checked 2026-08-18)

Version 3 on: Cage 0.2.0, Hyprland 0.52.1, Jay 1.12.0, Labwc 0.9.2, Louvre 2.14.1, Mir 2.26,
niri 25.11, phoc 0.52, river 0.3.13, Sway 1.11, Treeland 0.8.0, Wayfire 0.9.0.
**Not** shipped by: COSMIC 1.0.0~beta.8 (dropped in favor of ext/cosmic protocols), GameScope,
KWin 6.6, Muffin, Mutter 49.2, Weston 14.0.2. ([wayland.app matrix][wlr-screencopy])

### Deprecation status

The wlr-protocols XML now carries: "This protocol is deprecated and not intended for production
use. The ext-image-copy-capture-v1 protocol should be used instead." ([spec][wlr-screencopy])
No compositor has announced removal; Hyprland, Sway, and niri all still ship v3 (see matrix
above), and Hyprland's only path *is* wlr-screencopy since it lacks the successor (see §2).
Treat the deprecation as "frozen, will not gain features", not "going away".

### Rust implementation evidence

- `wayland-protocols-wlr` crate exposes generated bindings for
  `zwlr_screencopy_manager_v1`/`frame_v1`. ([docs.rs][wpw-docs])
- `libwayshot` (the library behind `wayshot`) is a working Rust implementation of
  wlr-screencopy region capture — useful as a reference or dependency.
  ([libwayshot docs.rs][libwayshot])

**Hover-loop rating: PASS** (wlroots family + Hyprland). Region-native, no prompt by default,
worst case one prompt per app run under Hyprland's opt-in permissions, per-grab cost ≈ one
region-sized copy + (Hyprland, plain copy) one forced repaint.

---

## 2. ext-image-copy-capture-v1 + ext-image-capture-source-v1

### Design (why it's better for a loop, in principle)

Both protocols are in the wayland-protocols **staging** ("testing phase") tier, merged August 2024
after ~3 years of iteration; authored by Andri Yngvason (wayvnc) and Simon Ser, explicitly to
supersede wlr-screencopy. ([spec][ext-icc], [source spec][ext-ics],
[Yngvason's design writeup, 2024-08-10][andri-blog], [Phoronix merge report][phoronix-merge])

- **Sources are opaque objects** created by factory managers:
  `ext_output_image_capture_source_manager_v1` (whole `wl_output`) and
  `ext_foreign_toplevel_image_capture_source_manager_v1` (whole toplevel/window). ([spec][ext-ics])
- **Session-based capture**: `create_session(source)` once → compositor advertises buffer
  constraints (shm formats, dmabuf device/formats, `buffer_size`) → per frame the client does
  `create_frame` → `attach_buffer` → `damage_buffer` → `capture`. Buffers are **reused** across
  frames and the client reports which parts of *its* buffer are stale, so the compositor copies
  only `client_damage ∪ source_damage`. This is strictly less copying than wlr-screencopy's
  fresh-frame-object-per-grab model. ([spec][ext-icc], [andri-blog])
- **Damage-driven pacing**: "Unless this is the first successful captured frame performed in this
  session, the compositor may wait an indefinite amount of time for the source content to change
  before performing the copy." ([spec][ext-icc]) Same static-screen blocking caveat as
  `copy_with_damage`; a hover loop must treat "no new frame" as "previous pixels still valid".
- Dedicated **cursor sessions** (`create_pointer_cursor_session`) deliver cursor image/hotspot/
  position separately — irrelevant for OCR but shows the protocol's remote-desktop orientation.

### The region/cropping story — the blocker

**v1 has no region capture.** The only sources are whole outputs and whole toplevels, and "The
client must attach buffers that match this size" (`buffer_size` = full source dimensions).
([spec][ext-icc], [ext-ics]) Consequences for chibipop:

- A 500×100 crop requires capturing the **full monitor** into a client buffer and cropping
  client-side. With shm on a 4K monitor that's ~33 MB per frame (~130–330 MB/s at 4–10 Hz) —
  feasible but wasteful; damage-tracking reduces steady-state copies but not the first frame after
  each change. A dmabuf session + GPU crop avoids the CPU copy at the cost of GL/Vulkan plumbing
  in the capture backend.
- The source-factory architecture was explicitly designed so **new source types can be added by
  separate protocols without touching the capture protocol** ([spec preamble][ext-ics],
  [andri-blog]) — a region/"output-rect" source manager is the natural future extension, but **no
  such extension is merged in wayland-protocols as of 2026-08-18** (the staging XML contains only
  the two factories above; no region-source protocol appears in the wayland.app index).

### Adoption status (checked 2026-08-18)

`ext_image_copy_capture_manager_v1` v1 shipped by: **COSMIC** 1.0.0~beta.8, **Jay** 1.12.0,
**Labwc** 0.9.2, **Mir** 2.26, **phoc** 0.52, **Sway** 1.11, **Treeland** 0.8.0.
**Not** shipped by: KWin 6.6, Mutter 49.2, niri 25.11, river, Wayfire, Weston, GameScope, Cage,
Louvre, Muffin. ([wayland.app matrix][ext-icc], which lists Hyprland 0.52.1 as lacking it — stale;
Hyprland ships it since 0.54.0, see below.)

- wlroots implemented it in **0.19.0** (released May 2025 per Simon Ser's status update); Sway
  exposes it as of **Sway 1.11** (sway PR #7976). ([emersion status update 2025-05][emersion-0519],
  [sway PR #7976][sway-7976])
- **Hyprland: implemented in v0.54.0 (released 2026-02-27)** — "protocols: implement
  image-capture-source-v1 and image-copy-capture-v1 (#11709)" ([v0.54.0 release
  notes][hypr-054]); feature request [#9916][hypr-9916] (filed 2025-04-06) is now **closed**.
  The in-tree implementation (`src/protocols/ImageCopyCapture.cpp`, fetched 2026-08-18) routes
  through the same ScreenshareManager as wlr-screencopy, supports output *and* toplevel sources
  plus cursor sessions, and is wired into the DynamicPermissionManager (0.49+ permission system
  applies). ([ImageCopyCapture.cpp][hypr-icc-src]) Note: the wayland.app matrix cell still says
  "x" because the site tested Hyprland 0.52.1 — the matrix lags reality here.
- **niri: open issue #1558** (filed 2025-05-11 by the maintainer, still open as of a 2026-03-05
  update; labeled blocking other work). ([niri #1558][niri-1558])
- **KWin: open feature request KDE bug 513785** ("Implement wayland staging protocols
  ext_image_copy_capture_v1 and ext_image_capture_source_v1"). No implementation in KWin 6.6.
  ([KDE bug 513785][kde-513785], [wayland.app matrix][ext-icc])
- **Mutter: nothing** — GNOME's position remains portal-only (see §3); the protocol author himself
  wrote "It would be nice if it were adopted by the likes of Mutter or KWin, but I wouldn't hold
  my breath". ([andri-blog])

### Does it obsolete wlr-screencopy, and when?

Spec-wise yes (deprecation note in wlr-protocols, §1). Practically: the *reference* wlroots family
ships both side by side (Sway 1.11), and Hyprland does likewise since 0.54.0. No compositor has
announced removing wlr-screencopy, and it remains the only region-capable option. There is no
dated timeline anywhere. Plan for coexistence for multiple years; gate backend selection on
advertised globals, not on compositor identity.

**Hover-loop rating: PARTIAL.** Architecturally the best capture loop (session + two-way damage),
no consent prompt (same privileged-global model as wlr), available on Hyprland ≥0.54 (2026-02-27),
Sway 1.11, and COSMIC — but: no region capture (full-frame + client crop), and absent on niri,
KWin, Mutter. Ship as a second backend; revisit as primary when a region-source extension lands.

---

## 3. xdg-desktop-portal ScreenCast + PipeWire

Interface: `org.freedesktop.portal.ScreenCast`, currently **version 6**. Flow: `CreateSession` →
`SelectSources(session, {types, cursor_mode, restore_token, persist_mode})` → `Start` → returns
PipeWire stream node(s) → `OpenPipeWireRemote` fd → consume frames via PipeWire.
([portal docs][portal-sc])

### Consent UX & persistence

- `Start` "will typically result in the portal presenting a dialog letting the user do the
  selection set up by SelectSources". One dialog per session start, not per frame.
  ([portal docs][portal-sc])
- **Persistence (interface v4+):** `persist_mode` = 0 (none) / 1 (while app is running) /
  2 (until explicitly revoked). If granted, `Start`'s response carries a `restore_token`; passing
  it to the next `SelectSources` skips the dialog. **Tokens are single-use and rotate on every
  restore** — chibipop must persist the *new* token after every start. If restore fails (monitor
  gone, permission revoked) the user is re-prompted. ([portal docs][portal-sc])
- Token storage design (transient = in-memory, persistent = permission store on disk) per the
  implementing PR flatpak/xdg-desktop-portal#638. ([xdp PR 638][xdp-638])
- **Backend support differences:**
  - **GNOME:** implemented since GNOME 42 (xdg-desktop-portal-gnome 42.rc: "Implement screencast
    restoration", March 2022). Works across reboots (persistent mode uses the permission store).
    ([xdpg NEWS][xdpg-news])
  - **KDE:** implemented in xdg-desktop-portal-kde (persist_mode/restore_token honored); KDE bug
    480235 (2024-02) reported *remote-desktop* session tokens not surviving reboots — screencast
    persistence is the supported path, but treat KDE reboot persistence as "verify at integration
    time". ([KDE bug 480235][kde-480235], [xdpk MR 223][xdpk-223])
  - **Hyprland (XDPH):** supports restore tokens; `screencopy:allow_token_by_default = true` in
    `xdph.conf` auto-grants persistence so no dialog on restart. Known token bugs were filed and
    fixed over 2023–2025 (issues #123, #350). ([XDPH wiki][xdph-wiki], [XDPH #350][xdph-350])
  - **Sway (xdg-desktop-portal-wlr):** restore tokens landed in v0.7.0 but only **transient**
    sessions can be restored (no consent-UI support for persistent grants; env var opt-in;
    PR #234). Irrelevant in practice because Sway offers wlr-screencopy directly.
    ([xdpw releases][xdpw-rel], [xdpw PR 234][xdpw-234])
  - **niri:** implements Mutter's ScreenCast D-Bus API so screencasting runs "through
    xdg-desktop-portal-gnome" — inherits GNOME's token behavior. ([niri README][niri-readme])

### Region cropping

**None.** `AvailableSourceTypes` is MONITOR (1) / WINDOW (2) / VIRTUAL (4); `SelectSources` takes
no geometry. ([portal docs][portal-sc]) A hover loop must stream the **whole monitor** (or a
window) and crop client-side. Note the stream's `position`/`size` metadata are in *compositor
logical* coordinates while the PipeWire buffers are in pixels — cursor→crop math must handle
scale, same as §1 but with an extra logical↔pixel mapping across the portal boundary.

### Latency & burst suitability

- Once the stream is running, frames are pushed by the compositor as content changes (damage-
  driven on GNOME/Mutter; cursor can be delivered as stream metadata via `cursor_mode=4` to avoid
  cursor-only frames). Steady-state latency is on the order of one compositor frame.
  [INFERENCE from the streaming model; not benchmarked in this pass.]
- **Session setup is expensive**: three D-Bus round trips with a Request/Response signal dance,
  potential dialog, then PipeWire fd handoff and format negotiation. This is hundreds of ms —
  unusable per burst. Consequence: **keep one full-monitor stream open for the app's lifetime**,
  which means the compositor is doing screencast work continuously even when the user isn't
  hovering anything (power cost; on multi-monitor setups either N streams or restarting the
  session when the cursor changes monitors — token rotation makes restarts prompt-free but still
  slow). PipeWire delivers dmabuf or shm; dmabuf avoids CPU copies until the crop.
- Multi-monitor: `SelectSources(multiple=true)` can return several streams in one consent dialog.
  Since v6, prefer `pipewire-serial` + `PW_KEY_TARGET_OBJECT` over raw node IDs (node IDs can be
  reused across hotplug/suspend). ([portal docs][portal-sc])

**Hover-loop rating: PARTIAL (fallback tier).** Universal (GNOME, KDE, everywhere), one
consent dialog on first run then token-silent, but: no region source, permanent full-monitor
stream, slow session setup, and backend-specific persistence quirks. This is the only option on
GNOME/KDE and therefore mandatory for coverage; it should never be selected where a wlr/ext
protocol global exists.

---

## 4. Other mechanisms (load-bearing or explicitly rejected)

- **`zwlr_export_dmabuf_manager_v1` (wlr-export-dmabuf):** legacy zero-copy *output* export;
  no region parameter, no damage protocol, v1 only. Hyprland does **not** ship it (0.52.1);
  still present on Sway 1.11, labwc, phoc, river, Wayfire, Cage. Nothing it offers helps the
  hover loop (we'd still crop client-side, with more fragile dmabuf lifetimes). **Rejected.**
  ([spec + matrix][wlr-export])
- **KWin `org.kde.KWin.ScreenShot2` D-Bus API:** KWin-private screenshot interface with a
  `CaptureArea` concept; access is gated by desktop-file pre-authorization
  (`X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2`), i.e. **no runtime prompt** for
  authorized apps. KDE-only, unstable-by-policy, requires the app to be installed with the right
  .desktop file. Worth prototyping later as a KDE fast path if portal overhead proves painful;
  not part of the initial plan. ([KDE portal pre-authorization docs][kde-preauth],
  [ksnip discussion showing the mechanism][ksnip-884])
- **KDE `zkde_screencast_unstable_v1`:** "a desktop environment implementation detail. Regular
  clients must not use this protocol" (drives KWin's own portal backend into PipeWire).
  **Rejected** per its own spec text. ([protocol XML][zkde-xml])
- **Hyprland `hyprland-toplevel-export-v1`:** Hyprland-specific *window* capture analogous to
  screencopy; covered by the same 0.49+ permission system ("toplevel export" permission class).
  Not needed for cursor-region grabs (outputs suffice) but available if a window-scoped mode is
  ever wanted on Hyprland. ([Hyprland wiki: Permissions][hypr-perms])
- **GNOME `org.gnome.Shell.Screenshot` / Mutter ScreenCast D-Bus:** private GNOME interfaces;
  the public, supported surface is the portal (§3). Not pursued.

---

## 5. Support matrix (checked 2026-08-18; versions are the ones verified against)

Sources: wayland.app per-protocol compositor tables ([wlr][wlr-screencopy], [ext][ext-icc],
[export-dmabuf][wlr-export]) unless otherwise cited.

| Compositor (version) | wlr-screencopy v3 (region ✓) | ext-image-copy-capture v1 (no region) | Portal ScreenCast + persist tokens | Notes |
|---|---|---|---|---|
| **Hyprland 0.54.3** (current) | ✅ v3, in-tree impl (post-wlroots since 0.42, 2024-07) | ✅ since **0.54.0** (2026-02-27, [PR #11709][hypr-054]; [#9916][hypr-9916] closed) | ✅ XDPH, tokens + `allow_token_by_default` | Opt-in permission system since 0.49 (2025-05-08): allow/ask/deny per binary; default = no prompts; applies to both protocols |
| **Sway 1.11** (wlroots 0.19, 2025-05) | ✅ v3 | ✅ v1 ([PR #7976][sway-7976]) | ⚠️ xdpw: transient restore only | No permission gating; both wlr+ext globals open |
| **niri 25.11** | ✅ v3 | ❌ open [#1558][niri-1558] (2025-05) | ✅ via xdg-desktop-portal-gnome ([README][niri-readme]) | |
| **KWin 6.6 (Plasma)** | ❌ (never; KDE policy) | ❌ open [KDE bug 513785][kde-513785] | ✅ xdpk; reboot-persistence caveat ([bug 480235][kde-480235]) | Alt: ScreenShot2 D-Bus w/ desktop-file pre-auth |
| **Mutter 49.2 (GNOME)** | ❌ (never; GNOME policy) | ❌ | ✅ since GNOME 42 (2022-03) ([NEWS][xdpg-news]) | Portal is the only mechanism |
| **COSMIC 1.0.0~beta.8** | ❌ (dropped) | ✅ v1 | ✅ (cosmic portal) | Only major compositor where ext is the *only* wlr-style path |
| labwc 0.9.2 / phoc 0.52 / Mir 2.26 / Treeland 0.8.0 / Jay 1.12 | ✅ v3 | ✅ v1 | varies | Both protocols |
| river 0.3.13 / Wayfire 0.9.0 / Louvre 2.14.1 / Cage 0.2.0 | ✅ v3 | ❌ | varies | wlr only |
| Weston 14.0.2 / GameScope / Muffin | ❌ | ❌ | Weston: portal n/a | Out of scope |

### Mechanism ratings vs. the hover-loop requirement

| Mechanism | Region grab | No per-use prompt | Latency | Burst cost | Coverage | Verdict |
|---|---|---|---|---|---|---|
| wlr-screencopy v3 | ✅ compositor-side | ✅ (Hyprland: at most once per app run, opt-in) | ≤1 refresh | ~200 KB copy (+1 forced repaint on Hyprland plain copy) | wlroots family + Hyprland | **PASS — primary** |
| ext-image-copy-capture v1 | ❌ full source + client crop | ✅ | ≤1 refresh; blocks on static content | full-frame copy on change (shm) or dmabuf+GPU crop | Hyprland ≥0.54, Sway, COSMIC, labwc, Mir, phoc… — **not niri/KDE/GNOME** | **PARTIAL — backend B, future primary** |
| Portal ScreenCast + PipeWire | ❌ full monitor stream + client crop | ⚠️ 1 dialog first run; token-silent after (backend-dependent) | ~1 frame once streaming; session setup ≫100 ms | continuous stream while app runs | Universal (GNOME/KDE incl.) | **PARTIAL — fallback tier** |
| wlr-export-dmabuf | ❌ | ✅ | ok | dmabuf lifetime pain | shrinking (no Hyprland) | **REJECT** |
| KWin ScreenShot2 D-Bus | ✅ (CaptureArea) | ✅ w/ desktop-file pre-auth | untested | untested | KWin only | park as KDE optimization |

---

## 6. Open risks & disagreements between sources

1. **Deprecation vs. reality:** the wlr-screencopy spec says "not intended for production use"
   ([spec][wlr-screencopy]) while it remains the only region-capable protocol — even Hyprland's
   new ext implementation (0.54.0) cannot replace it for region grabs. We must build on a
   spec-deprecated protocol; mitigate with a capture trait so backends are swappable.
2. **No region source in the successor protocol:** if/when a region extension is proposed for
   ext-image-capture-source (the factory architecture is designed for it, [andri-blog]), our
   backend B becomes strictly better than A. Nothing is merged upstream as of 2026-08-18 — watch
   wayland-protocols staging.
3. **Hyprland permission enforcement drift:** the permission system is young (0.49, 2025-05) and
   default-off today; Hyprland has an open issue contemplating tighter non-portal screencopy
   controls ([#9915][hypr-9915]). If a future Hyprland flips enforcement on by default, chibipop's
   UX becomes "one dialog per run" unless the user adds an `allow` rule — document it.
4. **`copy_with_damage`/ext `capture` block on static screens.** The hover loop must never rely
   solely on damage-gated capture: moving the cursor to a new region over unchanged content
   produces no event. Design: plain copy on region change, damage-gated re-copy while dwelling.
5. **KDE portal reboot persistence** was buggy for remote-desktop sessions (KDE bug 480235,
   2024-02); screencast-session persistence needs an integration-time test on current Plasma.
6. **Coordinate-space traps:** wlr region is in output-logical coords; buffers come back in
   pixels; portal `position`/`size` are compositor-logical while PipeWire buffers are pixels.
   Fractional scaling (Hyprland default-friendly) must be part of backend A/C test plans.
7. **wayland.app matrix freshness:** the support matrix reflects the versions listed on the site
   at fetch time (e.g. KWin 6.6, Mutter 49.2, Hyprland 0.52.1) and demonstrably lags: it still
   shows Hyprland without ext-image-copy-capture even though 0.54.0 (2026-02-27) shipped it.
   Cross-check compositor release notes/source before locking the backend priority order.

---

## Sources

- wlr-screencopy-unstable-v1 spec + compositor matrix (wayland.app, fetched 2026-08-18): <https://wayland.app/protocols/wlr-screencopy-unstable-v1>
- ext-image-copy-capture-v1 spec + matrix (fetched 2026-08-18): <https://wayland.app/protocols/ext-image-copy-capture-v1>
- ext-image-capture-source-v1 spec + matrix (fetched 2026-08-18): <https://wayland.app/protocols/ext-image-capture-source-v1>
- wlr-export-dmabuf-unstable-v1 spec + matrix (fetched 2026-08-18): <https://wayland.app/protocols/wlr-export-dmabuf-unstable-v1>
- org.freedesktop.portal.ScreenCast v6 docs (fetched 2026-08-18): <https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html>
- Hyprland screencopy implementation, `src/protocols/Screencopy.cpp` @ main (fetched 2026-08-18): <https://github.com/hyprwm/Hyprland/blob/main/src/protocols/Screencopy.cpp>
- Hyprland wiki — Permissions: <https://wiki.hypr.land/Configuring/Advanced-and-Cool/Permissions/>
- Hyprland v0.49.0 release notes (2025-05-08; permission manager for screencopy, PR #9930): <https://github.com/hyprwm/Hyprland/releases/tag/v0.49.0>
- Hyprland issue #9916 — Implement ext-image-copy-capture-v1 (2025-04-06; **closed**, checked 2026-08-18): <https://github.com/hyprwm/Hyprland/issues/9916>
- Hyprland v0.54.0 release notes (2026-02-27; "protocols: implement image-capture-source-v1 and image-copy-capture-v1 (#11709)"): <https://github.com/hyprwm/Hyprland/releases/tag/v0.54.0>
- Hyprland ext implementation, `src/protocols/ImageCopyCapture.cpp` @ main (fetched 2026-08-18): <https://github.com/hyprwm/Hyprland/blob/main/src/protocols/ImageCopyCapture.cpp>
- Hyprland issue #7478 — implement ext-image-capture-source/copy-capture (2024-08-23): <https://github.com/hyprwm/Hyprland/issues/7478>
- Hyprland issue #9915 — permission controls for non-portal screencopy (2025-04-06): <https://github.com/hyprwm/Hyprland/issues/9915>
- Hyprland independence announcement (2024-07-21): <https://hypr.land/news/independentHyprland/>
- Phoronix — Hyprland 0.42 drops wlroots for aquamarine: <https://www.phoronix.com/news/Hyprland-0.42-Wayland>
- Andri Yngvason — "Making a Wayland Screen Capturing Protocol" (2024-08-10, protocol author): <https://andri.yngvason.is/making-a-wayland-screen-capturing-protocol.html>
- Phoronix — Wayland merges new screen capture protocols (2024-08): <https://www.phoronix.com/news/Wayland-Merges-Screen-Capture>
- emersion status update 2025-05 — wlroots 0.19.0 release incl. ext-image-copy-capture: <https://emersion.fr/blog/2025/status-update-76/>
- sway PR #7976 — add ext-image-copy-capture + capture-source: <https://github.com/swaywm/sway/pull/7976>
- niri issue #1558 — Implement ext-image-copy-capture (2025-05-11, open; state via GitHub API 2026-08-18): <https://github.com/niri-wm/niri/issues/1558>
- niri README — screencasting via xdg-desktop-portal-gnome (fetched 2026-08-18): <https://github.com/niri-wm/niri/blob/main/README.md>
- KDE bug 513785 — implement ext_image_copy_capture_v1 in KWin (open): <https://bugs.kde.org/show_bug.cgi?id=513785>
- KDE bug 480235 — portal persistence across reboots (2024-02): <https://bugs.kde.org/show_bug.cgi?id=480235>
- xdg-desktop-portal-kde MR 223 — RemoteDesktop v2 session persistence: <https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/merge_requests/223>
- KDE developer docs — XDG portal pre-authorization (`X-KDE-DBUS-Restricted-Interfaces`): <https://develop.kde.org/docs/administration/portal-permissions/>
- ksnip discussion #884 — rectangular area capture on KDE Wayland via ScreenShot2: <https://github.com/ksnip/ksnip/discussions/884>
- zkde-screencast-unstable-v1 XML ("desktop environment implementation detail"): <https://github.com/KDE/plasma-wayland-protocols/blob/master/src/protocols/zkde-screencast-unstable-v1.xml>
- flatpak/xdg-desktop-portal PR #638 — screencast restore design (persist modes, token storage): <https://github.com/flatpak/xdg-desktop-portal/pull/638>
- xdg-desktop-portal-gnome NEWS — 42.rc "Implement screencast restoration": <https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/NEWS>
- xdg-desktop-portal-wlr releases (v0.7.0 restore tokens) and PR #234 (transient restore): <https://github.com/emersion/xdg-desktop-portal-wlr/releases>, <https://github.com/emersion/xdg-desktop-portal-wlr/pull/234>
- xdg-desktop-portal-hyprland wiki (restore tokens, `allow_token_by_default`) and issue #350: <https://wiki.hypr.land/Hypr-Ecosystem/xdg-desktop-portal-hyprland/>, <https://github.com/hyprwm/xdg-desktop-portal-hyprland/issues/350>
- wayland-protocols-wlr crate (screencopy bindings): <https://docs.rs/wayland-protocols-wlr/latest/wayland_protocols_wlr/screencopy/index.html>
- libwayshot (Rust wlr-screencopy library): <https://docs.rs/libwayshot>

[wlr-screencopy]: https://wayland.app/protocols/wlr-screencopy-unstable-v1
[ext-icc]: https://wayland.app/protocols/ext-image-copy-capture-v1
[ext-ics]: https://wayland.app/protocols/ext-image-capture-source-v1
[wlr-export]: https://wayland.app/protocols/wlr-export-dmabuf-unstable-v1
[portal-sc]: https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html
[hypr-screencopy-src]: https://github.com/hyprwm/Hyprland/blob/main/src/protocols/Screencopy.cpp
[hypr-perms]: https://wiki.hypr.land/Configuring/Advanced-and-Cool/Permissions/
[hypr-049]: https://github.com/hyprwm/Hyprland/releases/tag/v0.49.0
[hypr-9916]: https://github.com/hyprwm/Hyprland/issues/9916
[hypr-054]: https://github.com/hyprwm/Hyprland/releases/tag/v0.54.0
[hypr-icc-src]: https://github.com/hyprwm/Hyprland/blob/main/src/protocols/ImageCopyCapture.cpp
[hypr-7478]: https://github.com/hyprwm/Hyprland/issues/7478
[hypr-9915]: https://github.com/hyprwm/Hyprland/issues/9915
[hypr-independent]: https://hypr.land/news/independentHyprland/
[phoronix-042]: https://www.phoronix.com/news/Hyprland-0.42-Wayland
[andri-blog]: https://andri.yngvason.is/making-a-wayland-screen-capturing-protocol.html
[phoronix-merge]: https://www.phoronix.com/news/Wayland-Merges-Screen-Capture
[emersion-0519]: https://emersion.fr/blog/2025/status-update-76/
[sway-7976]: https://github.com/swaywm/sway/pull/7976
[niri-1558]: https://github.com/niri-wm/niri/issues/1558
[niri-readme]: https://github.com/niri-wm/niri/blob/main/README.md
[kde-513785]: https://bugs.kde.org/show_bug.cgi?id=513785
[kde-480235]: https://bugs.kde.org/show_bug.cgi?id=480235
[xdpk-223]: https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/merge_requests/223
[kde-preauth]: https://develop.kde.org/docs/administration/portal-permissions/
[ksnip-884]: https://github.com/ksnip/ksnip/discussions/884
[zkde-xml]: https://github.com/KDE/plasma-wayland-protocols/blob/master/src/protocols/zkde-screencast-unstable-v1.xml
[xdp-638]: https://github.com/flatpak/xdg-desktop-portal/pull/638
[xdpg-news]: https://github.com/GNOME/xdg-desktop-portal-gnome/blob/main/NEWS
[xdpw-rel]: https://github.com/emersion/xdg-desktop-portal-wlr/releases
[xdpw-234]: https://github.com/emersion/xdg-desktop-portal-wlr/pull/234
[xdph-wiki]: https://wiki.hypr.land/Hypr-Ecosystem/xdg-desktop-portal-hyprland/
[xdph-350]: https://github.com/hyprwm/xdg-desktop-portal-hyprland/issues/350
[wpw-docs]: https://docs.rs/wayland-protocols-wlr/latest/wayland_protocols_wlr/screencopy/index.html
[libwayshot]: https://docs.rs/libwayshot
