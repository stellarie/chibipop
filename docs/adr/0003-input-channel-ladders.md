# Linux input channel ladders

Wayland has no global input observation, so the Windows hook surface is replaced by two
per-family channel ladders inside `chibipop-linux`. Both synthesize the same core `Event`s
the Windows hooks do — core vocabulary is unchanged.

**Cursor channel** (both modes need global cursor position), first rung that probes
successfully wins:

1. **ext-image-copy-capture-v1 cursor session** — generic, event-driven, selected by the
   advertised global (Sway 1.11+, Hyprland ≥ 0.54, labwc, COSMIC, …). One-time prompt on
   hardened Hyprland (`ecosystem.enforce_permissions`); silent everywhere else.
2. **Portal ScreenCast `cursor_mode=METADATA`** — capability-probed; rides the PipeWire
   stream the portal capture backend already opens (KWin, GNOME), so cursor tracking
   costs no extra consent.
3. **`hyprctl cursorpos` polling** — Hyprland-specific fallback rung (env-detected),
   covering Hyprland < 0.54. The one deliberate exception to "never compositor identity"
   (ADR-0002): ~50 lines, zero permission, ungated by Hyprland's permission system.

No rung available (niri, river, Wayfire today; GNOME/KDE with portal denied) →
**unsupported**, with a startup diagnostic naming the exact missing global/portal
capability so a compositor upgrade self-heals. No degraded bespoke modes.

**Trigger channel** (HoldKey mode; press *and* release arrive on both rungs):

1. **GlobalShortcuts portal** where advertised (KDE, GNOME 48+, Hyprland/XDPH).
   Registered ids: `trigger` and `anki-add` — nothing else, to keep the consent dialog
   small. GNOME ≤ 47 is below the supported floor.
2. **Native compositor keybind → control socket** — universal fallback (sway/generic wlr)
   and power-user path everywhere: a UNIX socket at `$XDG_RUNTIME_DIR/chibipop/` fed by
   `chibipop ctl trigger-down|trigger-up|toggle` from `bindsym`/`bind` lines. Verb set is
   deliberately minimal; it is trigger transport, not a scripting API. **Amended — see the
   2026-08-26 addendum below: the verb set is now one verb per global action, and still
   not a scripting API.**

**evdev is rejected**, even as an option in settings: reading one key costs `input`-group
membership — user-level keylogger capability. At most a documented power-user page.

**Bare modifiers die on Linux defaults.** The portal shortcuts spec requires
modifiers+key, so the Windows hold-`shift` default becomes **Alt+F** (held while reading;
left-hand, mouse stays in the right) — valid on every channel. Bare-modifier hold remains
a documented one-line native bind (`bindsym --release Shift_L`, Hyprland `bindr`). The
trigger must be loudly customizable, with per-compositor snippet guidance in settings.
Known cost while bound: the compositor eats Alt+F globally (a common File-menu
accelerator); user-editable in the portal dialog, never auto-bound on bind-channel
compositors.

**Contextual interactions drop their global channel.** Wheel/click land on the popup's
pointer input region (wheel now requires cursor over the popup — accepted delta from
Windows' global swallow); ESC-back becomes a popup click affordance; Anki-add keeps a
keyboard path via the portal `anki-add` id. `keyboard_interactivity: none` is inviolable —
the popup never takes focus.

**Onboarding is eager and never fatal** (matches ADR-0002): portal registration at
startup; a missing channel visibly degrades — tray/settings show per-family status and
the exact config snippet for the detected compositor. Live mode without a trigger key is
still a working product.

## Every global action rides the control socket (amended 2026-08-26)

The portal id set above is a *consent-dialog* budget, and it was being read as a
budget on global actions. It is not. Four features arriving for Linux parity
(add-with-screenshot, the mining screenshot, OCR-to-clipboard, the static
sentence region) each want a key, and the audit found the add-card chord already
in the hole this creates: the settings window offered a copyable bind for the
trigger chord only and the socket had no add verb, so on a session with no
GlobalShortcuts portal — every plain wlr compositor — a shipped, editable chord
could not be bound at all.

So:

- **The portal id set stays at exactly two**, `trigger` and `anki-add`. The
  consent dialog does not grow, and `ShortcutId` stays the two-variant enum with
  a test on top. A fifth checkbox in a system dialog is how an app gets
  dismissed.
- **Every global action gets a control-socket verb**: `anki-add`, `screenshot`,
  `ocr-clipboard`, `static-region` join `reload`, `trigger-down`, `trigger-up`
  and `toggle`. Rung 2 is already the universal fallback and is bound on every
  session, so it is the rung that can carry an action the portal will never
  register.
- **One verb per global action — still not a scripting API.** The rule the
  original wording was protecting is intact and is now stated as a rule rather
  than as a count: a verb exists if and only if it names something a user can
  bind a key to. There is no verb that reads state, none that takes an argument,
  and none that composes. `shortcuts::state`'s status file exists precisely
  because a status *read* does not belong on this socket.
- **A portal press and its verb are one code path.** `anki-add` is deliberately
  spelled the same as the portal shortcut id, `shortcuts::action` resolves that
  id to `Verb::AnkiAdd`, and `App::apply_verb` is the only place either can land.
  Two rungs that could drift apart would be two semantics for one action.
- **Settings hands out a native bind for every chord**, not just the trigger:
  one row shape, one `bind_snippet` with a `Hold`/`Press(Verb)` distinction, and
  the verb text taken from `Verb::as_str` so a rename cannot rot a snippet. A
  chord with no bind to copy says so instead of printing an invalid line.
