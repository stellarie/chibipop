# Research: pure-Rust Wayland stack for layer-shell popup and settings UI

Research window: all versions, dates, and issue states checked **2026-08-18**. Reference compositor: Hyprland (wlroots-protocol family first-class, GNOME lowest priority).

## Verdict

1. **The popup is fully buildable today on `zwlr_layer_shell_v1` with pure Rust; no blocker.** Hyprland implements layer-shell v5 and `wp-fractional-scale-v1`. Everything the popup needs — cursor-anchored per-frame repositioning, no keyboard-focus stealing, selective click-through with hover hit-targets, fractional-scale-crisp text — maps directly onto protocol features (`set_margin` + `set_anchor`, `keyboard_interactivity::none`, `wl_surface.set_input_region`, `wp_viewport`).
2. **Nothing winit-based can create the popup today.** winit has no layer-shell support (issue open since 2022, PR #4044 unmerged as of 2026-05, maintainer wants a redesign around the post-split `winit-core`). Therefore **eframe/egui and stock iced cannot drive the popup surface**; they remain fine for the settings window (plain `xdg_toplevel`).
3. **Two viable popup routes**, both pure Rust, both Smithay-based underneath:
   - **Route 1 (recommended): hand-rolled SCTK popup.** smithay-client-toolkit 0.21 (`shell::wlr_layer`) + `wl_shm` CPU raster (tiny-skia 0.12) + cosmic-text 0.19. Smallest resident footprint (no GPU stack in a daemon that lives in the tray), total control over input region and per-frame margins, first-party Smithay maintenance. Cost: ~1–2k lines of Wayland plumbing (SCTK's `simple_layer.rs` is the template) and hand-drawn popup chrome.
   - **Route 2: iced_layershell 0.19 (waycrate).** Delivers the entire popup feature list out of the box: runtime `MarginChange`/`AnchorSizeChange`, `KeyboardInteractivity::None`, `SetInputRegion` with add/subtract rects, fractional scale via `wp_viewport` handled internally, daemon mode. Cost: a third-party dependency chain (layershellev → iced_layershell → iced) maintained by a small team, and iced's slow release cadence (0.13 → 0.14 took 15 months).
4. **Settings window: either egui/eframe 0.36 or iced 0.14.** Both run well on Wayland as normal toplevels via winit 0.30. If Route 2 is chosen for the popup, **iced is the one toolkit that can serve both surfaces** (same widget/theme code in popup and settings). egui cannot serve the popup without a DIY SCTK integration (community references exist but are young, and the wgpu backend costs ~100 MB resident — wrong shape for an always-on daemon).
5. **JP text**: cosmic-text handles JP shaping/fallback and per-span font sizes (`Attrs::metrics_opt` — covers furigana-ish mixed sizing). **Trap:** its Han-unification fallback picks *Chinese* glyph variants unless the `FontSystem` locale is `ja` — chibipop must force the locale or supply a custom `Fallback` impl, and should document/bundle a Noto CJK JP dependency. Vertical layout is not supported (open since 2022) — irrelevant for the popup (not required), relevant only if anyone later wants vertical rendering *inside* our UI.
6. **Hyprland quirks to design for**: (a) initial `preferred_scale` can arrive as 1.0 and be corrected on a later event — scale handling must be dynamic, never latched at startup (upstream says apps that assume the first value are buggy); (b) Hyprland animates layer surfaces by default — map the popup surface **once** and move/show/hide it via margins + alpha or attach/detach, rather than remap per lookup; document `layerrule` `no_anim` matching our namespace (`chibipop`) as a user nicety.

**Concrete recommendation:** shared core exposes a `PopupScene` (text runs + hit-target rects); Linux binary renders it with **SCTK 0.21 + tiny-skia 0.12 + cosmic-text 0.19** on an `overlay`-layer surface, and ships a separate settings process/window on **egui/eframe 0.36** (taste call; swap for iced 0.14 without touching the popup). Keep Route 2 (iced_layershell) as the documented fallback if the SCTK plumbing overruns, and re-evaluate winit PR #4044 before building any second overlay surface.

---

## 1. The popup surface on `zwlr_layer_shell_v1`

Protocol: wlr-layer-shell-unstable-v1, current interface version **5**. Hyprland 0.52.1 implements v5; Sway 1.11 v4; KWin 6.6 v5; niri v5; COSMIC v5; **Mutter: not implemented** (GNOME confirmed lowest-priority — layer-shell popup simply won't run there; GNOME would need a different surface strategy later). GameScope implements v4, so the popup even works inside Steam's session. ([wayland.app compositor table][ls])

### 1.1 Layer choice

`get_layer_surface(surface, output, layer, namespace)` takes one of `background | bottom | top | overlay` (z-ordered). The spec notes "Fullscreen shell surfaces are typically rendered at the top layer" — i.e. a surface on `top` sits *under* a fullscreened app, `overlay` sits above it. chibipop's popup must appear over fullscreen manga readers/games, so:

- **Default `overlay`** (value 3), namespace `"chibipop"`.
- `set_layer` exists since v2, so a "less intrusive" `top` mode can be a runtime config toggle with no surface recreation. ([spec][ls])

Passing `output = NULL` lets the compositor pick "the one that the user most recently interacted with" — right default for a cursor-following popup; the surface must still be recreated when the `closed` event arrives (output unplug/DPMS), see §1.5.

### 1.2 Per-frame repositioning: margins beat xdg-positioner

Three candidate mechanisms, all evaluated against the spec:

1. **Anchor + margin (recommended).** Anchor `top|left`, then `set_margin(top = cursor_y, right = 0, bottom = 0, left = cursor_x)` in surface-local (logical) coordinates. Margin, anchor, size, layer, and keyboard-interactivity are all **double-buffered, applied on `wl_surface.commit`** — so a reposition rides the same commit as the next frame's buffer: one request + the commit you were doing anyway, no reallocation, no round-trip, atomic with content. This is exactly the "per-frame reposition" the ticket asks about, and it is plain protocol behavior every compositor with layer-shell must implement — nothing Hyprland-specific to break. Margins only apply to anchored edges, so anchor top-left and drive both via margin. ([spec: `set_margin`, "Margin is double-buffered, see wl_surface.commit"][ls])
   - Caveat: the compositor does **not** constrain-adjust; clamping the popup on-screen (flip below/above cursor near edges) is client-side math against the output's logical size (SCTK `OutputState` exposes logical size/position; layer-shell `configure` also reports the size the compositor granted).
2. **`xdg_popup` parented to the layer surface.** `zwlr_layer_surface_v1.get_popup` legally parents an `xdg_popup` (created with NULL parent) to a layer surface; `xdg_positioner` then gives compositor-side constraint adjustment (flip/slide), and `xdg_popup.reposition` (xdg-shell ≥ v3) allows moving it. SCTK supports this (0.21 changelog even guards `reposition` on popup version ≥ 3). Verdict: correct but heavier — an extra xdg_surface/configure cycle per reposition, and popups carry grab/dismissal semantics designed for menus, not for a surface that moves on every lookup. Use only if we ever want compositor-side constraint solving. ([spec `get_popup`][ls]; [SCTK changelog][sctk-log])
3. **Full-output transparent overlay, draw popup internally** (slurp/hyprpicker pattern). Zero protocol churn when moving, but the buffer covers the whole output (a 4K×2160 ARGB SHM buffer is ~33 MB per copy for a CPU raster) and the input region still has to be updated per move to keep click-through correct. Rejected as default; noted as fallback if margin-move animation artifacts appear on some compositor.

### 1.3 Input model: never steal focus, still clickable

Exactly matches protocol facilities:

- **Keyboard:** `keyboard_interactivity` defaults to **`none` (0)**: "this surface is not interested in keyboard events and the compositor should never assign it the keyboard focus … the default value, set for newly created layer shell surfaces." The popup therefore cannot steal keyboard focus *by construction* — the underlying game/reader keeps typing focus. `on_demand` (2, since v4) exists if a future "pin popup and search inside it" mode wants click-to-focus; `exclusive` (1) is for lock screens — never use. ([spec enum `keyboard_interactivity`][ls])
- **Pointer:** "Layer surfaces receive pointer, touch, and tablet events normally. If you do not want to receive them, set the input region on your surface to an empty region." So: `wl_surface.set_input_region(empty)` makes the popup fully click-through; adding rects for the hit-targets (copy button, pin, audio) makes exactly those clickable while the rest of the popup stays transparent to input. Input region is also double-buffered per commit, so hit-target geometry updates ride the frame too. ([spec][ls]; core `wl_surface.set_input_region`)
- **Inherent tradeoff to accept:** while the pointer is over an *interactive* region, pointer focus (hover events, scroll) goes to the popup, not the app underneath. Mitigation is layout, not protocol: keep the popup offset from the cursor and keep interactive rects small. Keyboard focus is never affected.

### 1.4 Fractional scale + viewporter

`wp-fractional-scale-v1` (v1): compositor sends `preferred_scale` as N/120; client renders the buffer at `logical × scale`, sets `wp_viewport.set_destination(logical_w, logical_h)`, and leaves `wl_surface.buffer_scale` at 1. Supported by Hyprland 0.52.1, Sway 1.11, KWin 6.6, Mutter 49.2, niri — the whole target family. This is **required** for the popup: JP glyphs at 11–14 px logical under 1.25/1.5 scale are illegible if rendered at integer scale and downsampled. ([wayland.app fractional-scale + support table][fs])

Hyprland-specific behavior (both encountered by other overlay/IME projects):

- **Initial scale race:** Hyprland may send `preferred_scale = 120` (1.0) first and the real scale afterwards ([hyprwm/Hyprland#6869, closed 2024-07 as intended behavior — "This value can change… I think it's a bug on the side of the programs" (vaxerski)][hypr6869]). Consequence: treat scale as fully dynamic; re-render and re-commit on every `preferred_scale`, never latch the first value. (fcitx5's layer-shell IME popup hit exactly this class of blurriness on Hyprland HiDPI: [hyprwm/Hyprland#5387][hypr5387].)
- **Layer animations:** Hyprland animates layer surfaces (slide/fade) by default; per-lookup map/unmap would visibly "fly in" every time. Design: map the surface once, then reposition via margins and hide by attaching a fully transparent buffer or unmapping only on idle timeout. Users can additionally set a layer rule (`layerrule` with `no_anim` matched on `match:namespace = chibipop`) — worth one line in the README. ([Hyprland wiki 0.54 Window-Rules, layer rules section][hyprwiki])

### 1.5 Lifecycle

The `closed` event means the surface is dead ("Further changes to the surface will be ignored. The client should destroy the resource… and create a new surface"): fires on output destroy and can fire around DPMS. The daemon must treat surface recreation as routine, not exceptional. This is also *the* reason winit maintainers consider naive layer-shell-in-winit broken (see §3.1) — with SCTK we own this path explicitly. ([spec `closed`][ls]; [kchibisov in winit#4044][winit4044])

## 2. Drawing stack for the popup (Route 1 details)

| Crate | Version (date) | Role / assessment |
|---|---|---|
| smithay-client-toolkit (SCTK) | **0.21.1** (2026-07-23) | `shell::wlr_layer::{LayerShell, LayerSurface, Anchor, KeyboardInteractivity, LayerShellHandler}`; SHM pools; seat/pointer/keyboard; output info. Layer-shell supported since 0.17 (2023-03). Ships `examples/simple_layer.rs` (SHM raster on layer surface — our template) and `examples/wgpu.rs` (raw-window-handle bridge from a bare `wl_surface` to wgpu — proves the GPU path if ever wanted). **Gap:** no fractional-scale helper in-tree (verified by source grep, 2026-08-18); bind `wp_fractional_scale_v1` + `wp_viewporter` directly from `wayland-protocols` (~100 lines; layershellev does exactly this and is a copyable reference). ([docs.rs][sctk-docs]; [changelog][sctk-log]) |
| wayland-client / wayland-protocols | 0.31.15 (2026-07-22) | The Smithay bindings under everything here; steady release cadence. |
| softbuffer | 0.4.8 (2025-12-13) | Wayland is a **Tier 1** platform. Only relevant if we want one raster API shared with a future non-SCTK window; with SCTK it's simpler to draw straight into SCTK's `wl_shm` `SlotPool` (softbuffer adds nothing on top of a pool we already manage). ([README platform table][softbuffer]) |
| tiny-skia | **0.12.0** (2026-02-02) | CPU 2D raster (rounded rects, strokes, blends) for popup chrome; now maintained under the **linebender** org (repo moved from RazrFalcon) — maintenance risk retired. Popup-sized buffers (~400×300 logical @2x ≈ 1 MB) are trivial per-frame CPU work. |
| wgpu | 30.0.0 (2026-07-01) | Works on a layer surface via raw-window-handle (SCTK wgpu example). **Not recommended for the popup daemon**: a Vulkan/WGPU context holds ~100 MB USS at idle (measured by the wayapp author on his SCTK+egui+wgpu examples, 2025-11) — wrong cost profile for an always-resident hover dictionary. ([Ciantic in SCTK#349][sctk349]) |
| cosmic-text | **0.19.0** (2026-04-22) | Shaping (rustybuzz) + line layout + fallback + swash rasterization, pure Rust; used in production by COSMIC and by iced itself (`iced_graphics 0.14.0` depends on `cosmic-text` — verified via crates.io deps). Per-span `Attrs::metrics_opt: Option<CacheMetrics>` + `Buffer::set_rich_text` give mixed font sizes in one paragraph → covers the "furigana-ish" reading-above-headword layout without a second layout engine. **Vertical text: not supported** ([pop-os/cosmic-text#11, open since 2022][ct11]) — fine, popup doesn't need it. |
| swash / fontdb | 0.2.10 (2026-07-17) / 0.24.0 (2026-07-29) | Pulled in by cosmic-text; both actively released. |

### 2.1 CJK font fallback — the one real trap

cosmic-text's Unix fallback list (verified in `src/font/fallback/unix.rs`, main @ 2026-08-18):

- `Script::Hiragana` / `Script::Katakana` → always `Noto Sans CJK JP`. Good.
- `Script::Han` → `han_unification(locale)`: **only** `locale == "ja"` yields `Noto Sans CJK JP`; the default arm (including `en-US`!) yields `Noto Sans CJK SC` — *Chinese* glyph variants for kanji (骨, 直, 令 etc. render wrong for Japanese readers).

A chibipop user on an English-locale desktop would get SC kanji unless we act. Fixes, in preference order: (a) construct the `FontSystem` via `FontSystem::new_with_locale_and_db_and_fallback("ja", db, PlatformFallback)` (locale is an explicit constructor argument; the `Fallback` trait is public for a fully custom list); (b) set an explicit JP font family on every dictionary-text span. Also: fallback names are Noto-centric — if no Noto CJK is installed, there is no JP-capable entry in the common fallback list at all, so the popup should verify at startup (via fontdb query) that *some* JP font exists and surface a friendly error/bundling story. ([fallback source][ct-fallback])

## 3. Settings window (plain `xdg_toplevel`)

### 3.1 winit status (blocks both toolkits from the popup, not from settings)

- Feature request [rust-windowing/winit#2582][winit2582]: **open**, label `P - low`, since 2022-12.
- Implementation PR [#4044][winit4044] (SergioRibera): **open, unmerged** (opened 2024-12-18; still being rebased 2026-03-25; last thread activity 2026-05-17). Wayland maintainer kchibisov's recorded position (2025-08-24): "fundamental issues of layer shell integration are not addressed (that it gets destroyed from time to time)… I don't want it half baked", and post-backend-split he expects layer-shell to live *outside* winit proper, on `winit-core`. winit 0.31 (beta since 2025-11) is that restructuring; **no released winit does layer-shell** as of 2026-08.
- winit 0.30.13 on Wayland cannot even position a toplevel: `Window::set_outer_position` / `outer_position` are documented "**Wayland: Unsupported / NotSupportedError**" (source docs, v0.30.13). So "frameless xdg-toplevel at cursor" is not an escape hatch either — layer-shell is genuinely required for the popup.

### 3.2 egui / eframe

- egui/eframe **0.36.1** (2026-08-07); extremely active (upstream pushed the day of this research; 30k stars). eframe = egui-winit + wgpu by default (glow opt-in). Wayland toplevel support is routine. Settings UI (multi-tab form) is a natural eframe app. ([eframe README][eframe])
- **egui on the popup?** Only via DIY integration: winit path blocked (§3.1); the community track is SCTK + egui-wgpu glue — [SCTK#349][sctk349] (open since 2023, still active 2026-01) collects the references: **Ciantic/wayapp** ("Wayland App wrapper, example with EGUI + WGPU + SCTK", created 2025-11, pushed 2026-07, 4 stars, MIT, author-described as partly vibe-coded, ~100 MB WGPU idle memory) and one 1-star egui fork (kierandrewett/egui-layer-shell, dormant since 2025-07). SergioRibera maintains egui/iced fork branches riding PR #4044. Assessment: **patterns to copy, not dependencies to take**.

### 3.3 iced

- iced **0.14.0** (2025-12-07; previous stable 0.13.1 was 2024-09-19 — 15-month gap; factor this cadence into any plan that waits on iced fixes). winit-based shell, wgpu renderer with tiny-skia software fallback, text via cosmic-text. Settings window on Wayland: fine.
- **iced on the popup: yes, via iced_layershell** (waycrate/exwlshelleventloop) — the one maintained "single toolkit for both surfaces" option:
  - **iced_layershell 0.19.1 / layershellev 0.19.1** (both 2026-07-12; releases track iced closely — 0.18.x in 2026-05, 0.19.x in 2026-07).
  - layershellev is a winit-shaped event loop speaking layer-shell natively; it binds `wp_fractional_scale_v1` **and** `wp_viewporter` itself (fractional HiDPI handled; a `WpViewport` is in fact *required* by iced_layershell's window state — "iced_layershell need viewport support to better wayland hidpi").
  - `LayerShellSettings { anchor, layer, exclusive_zone, size, margin, keyboard_interactivity (None/OnDemand/Exclusive), start_mode (Active/TargetScreen/…), events_transparent, … }`; `events_transparent` sets an **empty input region** (full click-through) at creation.
  - Runtime control messages (via `#[to_layer_message]`): **`MarginChange((top,right,bottom,left))`** (= our per-lookup reposition), `AnchorChange`/`AnchorSizeChange`, `LayerChange`, `ExclusiveZoneChange`, `KeyboardInteractivityChange`, **`SetInputRegion(callback)`** with `region.add(x,y,w,h)` / `region.subtract(…)` (= our hit-targets), `NewPopUp` (xdg_popup on the layer surface), plus a `daemon()` entry point for long-running shells. All verified in 0.19.1 source/docs.
  - layershellev even contains xdg-toplevel machinery (`Shell::XdgTopLevel` incl. zxdg-decoration negotiation), but the documented path for a *settings window* remains plain iced/winit; treat a same-process mixed setup as an experiment, or run settings as a separate process (`chibipop --settings`), which also keeps the daemon's memory profile clean. [INFERENCE: separate-process settings is our design call; no source prescribes it.]
  - Health caveats: waycrate is a small community team (Decodetalkers et al.); the iced maintainer's own layer-shell plan is "outside winit, after winit-core" (same kchibisov position), so iced_layershell likely remains the iced answer for years; its README self-describes docs as thin ("I am lazy about writing the documents… ask in discord/issues").

## 4. Smithay ecosystem health (foundation risk for every option)

wayland-rs (wayland-client 0.31.15, 2026-07-22), SCTK (0.21.x, two releases in July 2026; 0.20 in 2025-07; 0.19 in 2024-05), and the compositor-side smithay crate are all releasing steadily; SCTK's changelog shows sustained protocol additions (xdg_wm_dialog, input-method v2/xx, background-effect, presentation-time) with a roughly annual minor-version cadence and breaking changes each minor (0.19→0.20→0.21 all breaking — pin exact minors). Winit itself depends on SCTK for its Wayland backend, giving the toolkit a large indirect user base. Risk: small maintainer set (ids1024, kchibisov are the visible constants); an annual cadence means upstream fixes can take months to land in a release — acceptable for a stable protocol like layer-shell v5. ([changelog][sctk-log]; crates.io dates)

## 5. Stack options

### Option A — SCTK-direct popup + eframe settings *(recommended)*

- **Popup:** SCTK 0.21 `wlr_layer` + manual `wp_fractional_scale_v1`/`wp_viewporter` bind + SHM `SlotPool` + tiny-skia 0.12 + cosmic-text 0.19 (locale forced `ja`). Overlay layer, `keyboard_interactivity::none`, input region = hit-target rects, reposition = `set_margin` + commit.
- **Settings:** eframe 0.36 (or iced 0.14 — interchangeable here), separate process.
- **Pros:** minimal daemon footprint (SHM + CPU raster; no GPU context); every protocol knob in our hands (the input-region and scale-race subtleties are *our* code, not an upstream issue away); depends only on first-party Smithay crates + linebender/pop-os text stack; popup renderer is reusable if a GNOME fallback surface strategy is ever needed.
- **Cons:** most code to write (event loop, seat handling, buffer management — `simple_layer.rs` covers ~70% of the shape); popup chrome is hand-drawn (fine: text panel + 2–3 icon hit-targets); two UI idioms in the repo (raster popup vs. toolkit settings).

### Option B — iced everywhere (iced_layershell popup + iced settings)

- **Popup:** iced_layershell 0.19 `daemon()`, `Layer::Overlay`, `KeyboardInteractivity::None`, `events_transparent` + `SetInputRegion` for hit-targets, `MarginChange` per lookup. **Settings:** stock iced 0.14, same widget/theme code.
- **Pros:** ~10× less platform code; one toolkit, one styling system across both surfaces; fractional scale, IME, virtual-keyboard already handled; actively released against current iced.
- **Cons:** third-party chain (waycrate) with small bus factor and thin docs; iced release cadence is slow and iced_layershell rides it; iced's wgpu renderer in a resident daemon costs GPU memory (tiny-skia fallback renderer exists but is the degraded path); less control if a compositor-specific input-region or scale bug needs a protocol-level workaround.

### Option C — egui everywhere (custom SCTK+egui integration for the popup)

- **Popup:** wayapp-style glue: SCTK surface → raw-window-handle → egui-wgpu, egui input synthesized from SCTK seat events. **Settings:** eframe.
- **Pros:** single immediate-mode toolkit; egui's momentum (0.36.x, monthly-ish releases).
- **Cons:** the integration layer is ours to own (references are 1-to-4-star young repos); wgpu ≈ 100 MB idle USS in the daemon; egui's CPU-raster story on bare Wayland is unproven. **Only worth revisiting after winit #4044 (or its winit-core successor) ships**, at which point eframe-on-layer-shell may become first-class and this collapses into "Option A's settings toolkit drives everything".

Decision axis in one line: **A buys control and a lean daemon with more code; B buys speed with a dependency bet on waycrate + iced cadence; C is currently the worst of both and is parked pending winit.**

## 6. Open risks & disagreements surfaced

- **winit direction vs. community PRs:** the maintainer position (external layer-shell on winit-core, "not half-baked") conflicts with the long-queued PR #4044 that egui/iced users are patching in via forks. Nothing mergeable is imminent; do not plan around it for v1. ([#4044 thread][winit4044])
- **Hyprland's "first scale event may be 1.0" stance** is explicitly *won't-fix* upstream (apps must handle scale changes at any time) while Sway sends the right value first — a portability trap that our render loop must absorb by design. ([#6869][hypr6869])
- **SCTK has no fractional-scale abstraction** — every SCTK-based option carries ~100 lines of manual protocol binding; layershellev proves the pattern works and is copyable (MIT).
- **cosmic-text Han-unification depends on process locale** — silent wrong-glyph failure mode for the core use case; must be forced to `ja` at `FontSystem` construction. ([fallback source][ct-fallback])
- **GNOME:** no layer-shell in Mutter, full stop; every option above needs a different surface story there (xdg_toplevel "best effort", or portal-era APIs). Consistent with the map's "GNOME lowest priority".
- **Layer-shell is technically an *unstable* wlr protocol** (`zwlr_…_v1`), but at v5 with 15+ implementing compositors it is a de-facto stable target; the `closed`-event lifecycle is the only sharp edge and is handled explicitly in both viable routes.

## Sources

- wlr-layer-shell-unstable-v1 spec + compositor support table — https://wayland.app/protocols/wlr-layer-shell-unstable-v1 (retrieved 2026-08-18) [ls]
- wp-fractional-scale-v1 spec + support table — https://wayland.app/protocols/fractional-scale-v1 (retrieved 2026-08-18) [fs]
- SCTK docs 0.21.1 (`shell::wlr_layer` module) — https://docs.rs/smithay-client-toolkit/latest/smithay_client_toolkit/shell/wlr_layer/ [sctk-docs]
- SCTK changelog (0.17 layer-shell, 0.19–0.21 dates, popup reposition guard) — https://github.com/Smithay/client-toolkit/blob/master/CHANGELOG.md [sctk-log]
- SCTK examples — https://github.com/Smithay/client-toolkit/blob/master/examples/simple_layer.rs, …/examples/wgpu.rs
- SCTK source grep for fractional scale (none in-tree) — repo @ master, 2026-08-18
- winit layer-shell feature request — https://github.com/rust-windowing/winit/issues/2582 (open, P-low; retrieved 2026-08-18) [winit2582]
- winit layer-shell PR — https://github.com/rust-windowing/winit/pull/4044 (open; kchibisov comments 2025-08-24/30; rebases through 2026-03-25) [winit4044]
- winit 0.30.13 `set_outer_position` Wayland-unsupported — https://github.com/rust-windowing/winit/blob/v0.30.13/src/window.rs
- exwlshelleventloop README (project scope, subcrates) — https://github.com/waycrate/exwlshelleventloop
- iced_layershell 0.19.1 docs (example, `SetInputRegion` region add/subtract, `application`/`daemon`) — https://docs.rs/iced_layershell/latest/iced_layershell/
- iced_layershell source: `settings.rs` (LayerShellSettings incl. `events_transparent`), `actions.rs` (`MarginChange`, `SetInputRegion`, `KeyboardInteractivityChange`), layershellev `lib.rs` (`wp_fractional_scale_v1` + `wp_viewporter` binding, `Shell::XdgTopLevel`) — repo @ master, 2026-08-18
- iced changelog 0.14.0 (2025-12-07) — https://github.com/iced-rs/iced/blob/master/CHANGELOG.md
- eframe README (winit + wgpu/glow architecture) — https://github.com/emilk/egui/blob/main/crates/eframe/README.md [eframe]
- SCTK issue "egui layer shell example" (open; wayapp + memory-usage comments) — https://github.com/Smithay/client-toolkit/issues/349 [sctk349]
- Ciantic/wayapp (SCTK+egui+wgpu wrapper; created 2025-11-23, pushed 2026-07-12) — https://github.com/Ciantic/wayapp
- kierandrewett/egui-layer-shell (dormant egui fork, pushed 2025-07-24) — https://github.com/kierandrewett/egui-layer-shell
- softbuffer README (Wayland Tier 1) — https://github.com/rust-windowing/softbuffer [softbuffer]
- cosmic-text fallback source (Han unification by locale; Noto CJK lists) — https://github.com/pop-os/cosmic-text/blob/main/src/font/fallback/unix.rs, …/fallback/mod.rs (`Fallback` trait, custom constructor) [ct-fallback]
- cosmic-text `Attrs::metrics_opt` + `Buffer::set_rich_text` (per-span sizes) — https://github.com/pop-os/cosmic-text/blob/main/src/attrs.rs, …/src/buffer.rs
- cosmic-text vertical text issue (open since 2022-10) — https://github.com/pop-os/cosmic-text/issues/11 [ct11]
- iced_graphics 0.14.0 → cosmic-text dependency — https://crates.io/api/v1/crates/iced_graphics/0.14.0/dependencies
- Hyprland initial fractional-scale value (#6869, closed 2024-07, works-as-intended) — https://github.com/hyprwm/Hyprland/issues/6869 [hypr6869]
- fcitx5 layer-shell popup blurry under fractional scale (#5387) — https://github.com/hyprwm/Hyprland/issues/5387 [hypr5387]
- Hyprland wiki, layer rules (`no_anim`, `match:namespace`) — https://wiki.hypr.land/0.54.0/Configuring/Window-Rules/ [hyprwiki]
- All crate versions/dates — crates.io API, retrieved 2026-08-18: smithay-client-toolkit 0.21.1 (2026-07-23); wayland-client 0.31.15 (2026-07-22); iced 0.14.0 (2025-12-07); iced_layershell / layershellev 0.19.1 (2026-07-12); egui/eframe 0.36.1 (2026-08-07); winit 0.30.13 (2026-03-02), 0.31.0-beta.2 (2025-11-16); softbuffer 0.4.8 (2025-12-13); tiny-skia 0.12.0 (2026-02-02, linebender); cosmic-text 0.19.0 (2026-04-22); wgpu 30.0.0 (2026-07-01); glyphon 0.12.0 (2026-07-09); swash 0.2.10 (2026-07-17); fontdb 0.24.0 (2026-07-29)

[ls]: https://wayland.app/protocols/wlr-layer-shell-unstable-v1
[fs]: https://wayland.app/protocols/fractional-scale-v1
[sctk-docs]: https://docs.rs/smithay-client-toolkit/latest/smithay_client_toolkit/shell/wlr_layer/
[sctk-log]: https://github.com/Smithay/client-toolkit/blob/master/CHANGELOG.md
[sctk349]: https://github.com/Smithay/client-toolkit/issues/349
[winit2582]: https://github.com/rust-windowing/winit/issues/2582
[winit4044]: https://github.com/rust-windowing/winit/pull/4044
[eframe]: https://github.com/emilk/egui/blob/main/crates/eframe/README.md
[softbuffer]: https://github.com/rust-windowing/softbuffer
[ct-fallback]: https://github.com/pop-os/cosmic-text/blob/main/src/font/fallback/unix.rs
[ct11]: https://github.com/pop-os/cosmic-text/issues/11
[hypr6869]: https://github.com/hyprwm/Hyprland/issues/6869
[hypr5387]: https://github.com/hyprwm/Hyprland/issues/5387
[hyprwiki]: https://wiki.hypr.land/0.54.0/Configuring/Window-Rules/
