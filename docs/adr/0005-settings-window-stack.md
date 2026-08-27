# Settings window stack on Linux

Research basis:
`docs/research/rust-wayland-ui.md`; ground truth from `src/ui/settings_window.rs`,
`src/settings.rs`, `src/config.rs`, `src/app.rs`.

## A separate process

The Linux settings window runs as its own process via the `chibipop settings`
subcommand — the standalone mode that already exists on Windows becomes the *only* mode
on Linux. The daemon stays pure SCTK with zero toolkit or GPU memory resident, and a
settings crash cannot take live-hover down. The daemon spawns at most one settings
child (tracked by pid; tray clicks while it lives are ignored); a directly launched
second instance finds the settings-scoped flock held and exits with a notice naming the
running window. No cross-process raise is attempted — Wayland compositors routinely
ignore self-activation, so we don't ship plumbing for behavior we can't promise.

Rejected: a settings thread inside the daemon (winit's event-loop-recreation trap on
close-and-reopen, toolkit+wgpu memory inside the resident daemon, one crash domain) and
an `iced_layershell` mixed same-process loop (the research explicitly labels it an
experiment).

## iced 0.14

The window is stock iced 0.14 on a plain `xdg_toplevel`. Decisive: iced rides
cosmic-text + fontdb + tiny-skia — the popup's exact text stack — so system Japanese
fallback works out of the box; dictionary titles are kanji, and egui has no system font
fallback (hand-loading a JP font is ~50 lines but it is the product's core failure mode
to get wrong). Secondary: one text/paint stack in the single shipped binary, Elm
architecture consonant with the core Controller, and synergy with `iced_layershell` as
the documented popup fallback. Accepted cost: iced's slow release cadence. Renderer is
iced's default (wgpu with tiny-skia fallback; `ICED_BACKEND` as escape hatch) — the
process is transient, so GPU memory is never resident. The font combo is populated from
fontdb's JP-capable families (ADR-0004's startup probe), replacing GDI
`EnumFontFamiliesExW`.

## The model lives in core

`Config` (TOML, serde, clamping) and `SettingsForm` + `from_config`/`apply_to`
(staging, validation, clamp notices, empty-library refusal) move from the Windows tree
into core verbatim — neither imports `windows` today. Bins own only widgetry: the Win32
window and the iced window are two views over one form model. The shared form carries
the full field set; which fields each platform renders is the settings-parity-audit
ticket's decision, not this ADR's.

## Live-apply is save-then-`reload`

Apply saves the TOML, then sends a single `reload` verb over ADR-0003's control socket.
The config file is the sole source of truth: the daemon re-reads it, re-derives
`LiveSettings`, and re-applies — theme re-render, `Trigger::Reload` to the Worker,
`overlay`↔`top` via `set_layer`, shortcut-session refresh. Socket absent (daemon not
running) → Apply just saves; the socket's presence *is* the ApplyMode, and the
Windows-only `Live`/`Standalone` distinction disappears on Linux. Rejected: pushing
structured settings over the socket (a second serialization of Config to keep in
lockstep — two sources of truth for zero user-visible gain).

## Dictionary rebuilds happen in the settings process

Staged dictionary/frequency changes rebuild `chibipop.sqlite` in the settings process —
the path Windows standalone mode already proves — with progress streamed in-process
into the status area. The new database is built beside the old and atomically renamed
over it; the daemon's open snapshot keeps reading the old inode unharmed, and the same
`reload` verb makes the Worker reopen (its snapshot-reload semantics exist per
ADR-0001). LibraryLock becomes a file lock preventing concurrent rebuilds. Rejected:
daemon-side rebuild (a progress-streaming IPC protocol, rebuild lifetime tied to the
daemon, library work on daemon threads).

## Hotkey controls are channel-aware

Windows' press-a-key capture buttons do not translate: on the portal channel the
compositor owns the binding UI, so the control invokes the portal's configure/rebind
flow and displays the compositor-reported current binding; on the wlr-native channel it
shows the documented `bindsym` snippet targeting the control socket, with a copy
button. Rejected: keeping key capture and translating to `preferred_trigger` — the
portal may ignore it and on wlr-native the captured key silently does nothing, a lying
UI.
