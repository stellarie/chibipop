# Live-hover cadence and trigger-mode freeze

## Live mode

**Dispatch is event-paced, Worker-throttled — no timer.** A cursor event dispatches a
lookup immediately when the Worker is idle; otherwise it coalesces into a single latest
pending position (one in-flight, latest-wins, as on Windows). The effective ceiling is
OCR + lookup latency (~22 ms OCR p50 → ~30–45 Hz), so the Worker itself is the pacer.
Windows' fixed 20 ms dispatch tick is not replicated; the hyprctl polling rung feeds the
same path at its poll rate. **No settle delay and no velocity gate** — burst movement is
tamed by one-in-flight backpressure alone, exactly as on Windows; intermediate positions
drop, latency stays zero.

**Dwell re-check — a deliberate better-than-Windows divergence.** While the popup is
shown and the cursor rests inside the sticky/freeze zone, the wlr backend keeps racing
`copy_with_damage` on a 250 ms deadline (ADR-0002's damage pacing). Content change under
the still cursor — VN/game dialogue advancing — re-OCRs (capture mask applies, ADR-0008)
and flows through the normal pipeline: same content re-presents nothing, a changed hit
updates the popup, a miss retracts it. A static screen costs one deadline wakeup per
period and zero copies. Windows goes stale here until the mouse moves; we do not.
No popup shown → no re-check: a parked cursor over empty screen is fully idle. On the
portal backend the same signal is free — PipeWire delivers frames only on damage.

**Power budget** (constants in code, no settings — matching Windows, where every cadence
number is a constant):

- Event-driven cursor rungs, idle: **0 wakeups/s**.
- Dwelling on a static screen: **≤ 4 wakeups/s** (the 250 ms dwell deadline).
- hyprctl polling rung: **20 ms while active**, decaying to **~150 ms after ~5 s**
  without movement (≤ 7 wakeups/s idle), bursting back to 20 ms on the first observed
  move.
- Active scrubbing across text: **≤ one core-thread** (back-to-back OCR passes).

## Trigger mode: freeze at press

At trigger press, **one full grab of the output under the cursor** is taken — before any
popup exists, so it can never self-contaminate. While the key is held, every lookup scans
this **frozen grab**: no capture, no mask, and text the popup later covers stays
readable — recovering the read-through property of Windows' capture guard, which live
mode on Linux cannot have (ADR-0008). Release retracts the popup and drops the buffer;
each press grabs fresh. The `toggle` verb grabs at toggle-on and stays frozen until
toggle-off. Full-output extent is deliberate: presses arrive at human cadence, so the
copy is free, and no out-of-region edge case exists.

**Output crossing mid-hold** takes one fresh full grab of the entered output (masked only
if the popup intersects it — rare, the popup sits on the press output). "Hold and glance
at the other monitor" works; a dead second monitor would read as a bug.

**Stale-frame cost accepted**: the screen changing mid-hold is not reflected until the
next press — bounded by hold duration, and coherent with what freezing means. There is
no dwell re-check in trigger mode by construction.
