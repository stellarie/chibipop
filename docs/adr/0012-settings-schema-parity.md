# One settings schema, per-platform key fields

The shared core owns `Config` and saves it by serializing the whole struct, so any key
absent from the struct dies on the next save from either platform. That forces one rule:
**everything that must round-trip lives as a field on the shared `Config`**, and each bin
renders only its platform's subset.

**Decision: keys get per-platform fields; everything else stays one shared field placed
in its natural section.**

- **Keys**: `trigger.trigger_key` / `anki.add_key` stay Windows VK names, untouched.
  New `trigger.trigger_key_linux` (default `ALT+F`, per ADR-0003) and
  `anki.add_key_linux` (default `ALT+A`) hold XDG GlobalShortcuts preferred-binding
  syntax. Each bin reads and renders only its own field; both serialize on both
  platforms, so a config file round-trips losslessly. On the wlr-native channel the
  Linux fields are advisory (the compositor bind is the truth, per ADR-0003/0005).
  Bare `a` was fine as a Windows hook key scoped to a shown popup, but a portal binding
  is a system-wide grab, so the Linux add default is a chord mirroring the trigger's
  modifier family.
- **Linux-only fields sit in their natural sections**, not a `[linux]` table:
  `popup.layer = "overlay" | "top"` (ADR-0004's runtime toggle, default `overlay`)
  beside the other popup knobs, key fields beside their Windows twins. Windows ignores
  them but preserves them on save (shared struct).
- **`popup.font` stays a dumb literal**: config creation writes the platform default
  (`Yu Gothic UI` / `Noto Sans CJK JP`); a carried literal that fontdb can't resolve
  falls back to the platform default with ADR-0004's visible warning. No empty-string
  sentinel semantics.
- **`ocr.language` is hidden on Linux, value preserved**: meikiocr (ADR-0009) is
  Japanese-only, so there is nothing to select; the Linux OCR engine logs one diagnostic
  unless the tag is `ja` or a `ja-*` subtag, then proceeds with `ja` regardless.
  `scan_alphanumeric` is a core-side lookup filter, engine-independent, rendered on both
  platforms.
- **Autostart is stateless**: the checkbox reads and writes
  `~/.config/autostart/chibipop.desktop` directly (ADR-0006); no TOML field, so it never
  desyncs from users who hand-manage the file or use the systemd/Hyprland routes.

Rejected:

- **One key field, platform-interpreted** — `shift` is illegal portal syntax and
  `ALT+f` is garbage to the VK parser; round-trip is lossy exactly where the defaults
  differ.
- **A canonical chibipop key syntax** — a mini-language to define, parse, test, and
  document, for two keys.
- **A `[linux]` section** — makes the platform boundary visible in raw TOML but splits
  related settings across tables and parts key fields from their Windows twins.
- **Font sentinel (`""` = platform default)** — teaches every `popup.font` consumer
  sentinel semantics while existing configs carry literals anyway.
- **TOML `autostart` bool reconciled on start** — invents a reconciliation loop that
  lies whenever autostart is managed outside chibipop.
