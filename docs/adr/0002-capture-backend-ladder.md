# Linux capture backend ladder

`RegionCapture` on Linux ships exactly two backends in v1, selected at runtime by
advertised globals — never by compositor identity:

1. **wlr-screencopy v3** (primary) — the only protocol with compositor-side region
   capture; no consent prompt by default. It is *spec-deprecated*, and we build on it
   anyway: no compositor has announced removal, the designated successor
   (ext-image-copy-capture-v1) has no region source, and the trait boundary makes the
   backend swappable if that ever changes.
2. **Portal ScreenCast + PipeWire** (fallback) — the only path on GNOME/KDE. Consent is
   requested **eagerly at startup** (warm stream, dialog in launch context) with
   `persist_mode=2`; the rotating restore token lives in `XDG_STATE_HOME`, so later runs
   are silent. All monitors are selected in one dialog; only the stream for the monitor
   under the cursor is connected, the rest stay paused — no dead hover on monitor
   crossings. Denial never exits: hover shows one actionable error state with in-app
   retry.

**ext-image-copy-capture-v1 is a named future backend, not built** — today it uniquely
covers only COSMIC at full-frame-capture cost; it becomes the primary the day a
region-source extension merges. **KWin ScreenShot2 D-Bus is parked** as a possible KDE
optimization (unstable-by-policy API).

**Buffers are shm with CPU cropping everywhere** — no GPU plumbing inside a capture
backend; dmabuf + GPU crop is the named optimization if the portal tier's full-monitor
copies prove costly.

**Damage pacing (backend 1, internal):** plain `copy` when the requested region changed;
while dwelling on the same region, `copy_with_damage` raced against a short deadline,
serving the cached frame when static. This preserves the trait's never-block invariant,
avoids Hyprland's forced full repaint per plain copy, and yields a free "content
unchanged" signal the Worker can use to skip OCR.

Compositors offering neither global (Weston, GameScope) are unsupported.
