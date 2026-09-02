# chibipop

Hover a word anywhere on screen; chibipop captures it, OCRs it, and pops up a Japanese
dictionary entry. One shared core, one binary per platform.

## Language

### Architecture

**Core**:
The platform-neutral crate (`chibipop`): controller, worker pipeline, lookup, dictionary
build, settings model, presentation, geometry, theme.
_Avoid_: shared lib, common

**Platform bin**:
A per-OS binary crate (`chibipop-windows`, `chibipop-linux`) owning the real OS event
loop and all OS-coupled code. The bin *is* the platform adapter.
_Avoid_: platform layer crate, backend crate

**Controller**:
The core-owned hover/popup state machine: `Event`s in, `Command`s out.
_Avoid_: app, orchestrator, message loop

**Event**:
An input to the Controller, synthesized by the platform bin: tick, cursor move, trigger
key, scroll, click, worker result, popup placed, tray action, quit.

**Command**:
An instruction the Controller returns for the platform bin to execute: request lookup,
show/move/hide popup, set cursor shape, arm scroll/click, open settings, exit.
_Avoid_: effect, action

**Worker**:
The core-owned background pipeline (capture → OCR → lookup → present); fed `Trigger`s,
yields `WorkerResult`s.

### Seams

**RegionCapture**:
The capture seam: the most recent content of a physical-pixel region, never blocking on
damage.
_Avoid_: screen grabber, screenshot

**Capture backend**:
A RegionCapture implementation, selected at runtime by advertised capability.
_Avoid_: capture provider, capture driver

**OcrEngine**:
The recognition seam: image in, lines with per-word geometry out.

**TextSource**:
The core-internal facade over RegionCapture + OcrEngine + shared layout: point in, text
span out.

**Capture mask**:
The core-side blanking of chibipop's own on-screen rects from a captured image before
OCR. Words touching a mask edge are dropped, as at a capture edge.
_Avoid_: redaction, popup filter

**Capture guard**:
The Windows-only hide/reshow of chibipop's windows around a grab. Not built on Linux.
_Avoid_: capture lock

**Dwell re-check**:
The damage-gated watch kept while the cursor rests on a shown popup; the popup refreshes
when the screen changes beneath it. Live mode only.
_Avoid_: polling loop, refresh timer

**Frozen grab**:
Trigger mode's press-time full-output capture. All lookups while held read it — taken
before the popup exists, so it needs no mask and reads through the popup.
_Avoid_: snapshot, screenshot buffer

**TextMeasure**:
The measurement seam: an ordered list of styled spans plus a wrap width in, per-line and
per-span geometry plus a baseline per line out. The seam is measure-only and never paints.
_Avoid_: text renderer, font backend

**Styled span**:
A run of text with one resolved style — family, size, weight, italic, colour — and the
finest unit the measurement seam addresses. Colour rides along for the bin's
paint walk; no geometry depends on it.
_Avoid_: text run (ambiguous with a PopupScene's positioned runs)

**Line box**:
One wrapped line's geometry as the measurer reports it: top, width, height, and the
baseline its spans hang from. A line is as tall as its tallest span.
_Avoid_: line metrics (that names the whole reply, not one line's share)

**Block box**:
The margin, border, padding and fill a block tag draws around **all** of its content,
as a browser does: a bordered `div` holding three paragraphs draws one border around
all three.
_Avoid_: paragraph box, block_box on a run (a box belongs to the block, not to a line)

### Dictionary

**Dictionary**:
One installed archive, identified by its exact name — the thing a user orders and
enables. Distinct from an Entry, which is one Dictionary's record for one headword.
_Avoid_: dict (fine in code, not in prose), source, library (that's all of them)

**Dictionary role**:
What a Dictionary provides: terms, frequency, or pitch. A *set*, not one value — an
archive carrying both a term bank and frequency data holds two roles — and always read
from the archive's banks, never from its filename and never declared by the user.
_Avoid_: kind, type, Editorial role (that classifies GlossDoc nodes, not Dictionaries)

**Dictionary list**:
The ordered, independently-managed list of Dictionaries holding one role. Position is
priority; each list carries its own enabled state, so one Dictionary can lead the
frequency list while sitting disabled in the terms list.
_Avoid_: display order (one list among three), priority list, section (a UI word)

**Entry**:
One dictionary's record for one headword — the unit a lookup returns. Carries the
headword's glosses and its part-of-speech labels.
_Avoid_: definition, sense (an Entry contains senses; it is not one)

**Sense**:
One distinct meaning within an Entry. Senses live *inside* the gloss content as
sibling blocks, not as separate Entries: a headword lookup returns one Entry whose
content holds every sense.
_Avoid_: gloss (a Sense may have several), definition

**Gloss**:
One phrasing of one Sense's meaning. A Sense with three synonyms has three glosses.

**GlossDoc**:
An Entry's gloss content as a typed tree, parsed once from the dictionary's structured
content. Core's only gloss representation; the popup, the Anki card, and plain text are
all renderers over it.
_Avoid_: glossary HTML, gloss string, structured content (that's the source format)

**Editorial role**:
A GlossDoc node's classified purpose — example, attribution, part-of-speech, or
ordinary content. What the render settings filter on.
_Avoid_: drop list

**Media store**:
The assets extracted from dictionary archives at build time, with their intrinsic
dimensions recorded. Inline gaiji resolve against it.
_Avoid_: image cache (that's the per-bin decoded-surface one)

**Reported frequency**:
One Dictionary's own claim about how common a headword is. What the popup shows — always
a real Dictionary's number, never a computed one.
_Avoid_: freq (ambiguous with Frequency rank), rank

**Frequency rank**:
The single commonness number a lookup ranks results by, reduced from every Reported
frequency by the Ranking strategy.
_Avoid_: frequency (that's one Dictionary's claim), score (that's the ranker's output)

**Ranking strategy**:
The rule reducing many Reported frequencies to one Frequency rank: best rank, priority,
or median. A Dictionary that lacks the word is not a data point.
_Avoid_: frequency mode (Yomitan's archive field), algorithm

**Reindex**:
The recompute of every Frequency rank from Reported frequencies already in the database —
one in-place transaction, no archive read. What a Ranking strategy, order, or enable
change costs. Never a rebuild.
_Avoid_: rebuild (that reads archives and swaps the file), refresh, rerank

**Pitch pattern**:
One Dictionary's accent for one reading: the mora the pitch drops after, plus the nasal
and devoiced moras recorded alongside it. A reading may carry several, and a Dictionary
may report several for one reading.
_Avoid_: accent (overloaded), pitch accent data, downstep (that's one field of it)

### Actions

**Mining screenshot**:
The user-drawn PNG saved beside a mined card, attached to the Anki note as a picture.
Core owns the rule (guards, filename, picture field, the AnkiConnect call); each bin only
picks a region and grabs pixels.
_Avoid_: capture (that's RegionCapture), context image

**Region selector**:
The full-output surface a user drags a rectangle on: dimmed screen, cleared selection,
Esc or right-click cancels. Windows' modal picker window; Linux's `Overlay`-layer surface,
the one surface allowed keyboard focus.
_Avoid_: crosshair, snipping tool

**Outline overlay**:
The click-through frame-only surface that draws rects over the screen: scan boxes, the
matched word, the static region. Never takes input.
_Avoid_: highlight window, debug overlay

**Static region**:
The fixed rect `SentenceMode::Static` reads its sentence from, drawn once by the user and
persisted in config. Independent of the mode, so switching modes cannot lose it.
_Avoid_: sentence box, capture region

**Clipboard rung**:
The Wayland protocol chibipop takes the selection with — `ext-data-control-v1`, then
`wlr-data-control-unstable-v1`. Neither advertised is a state, not an error: the action
says so and stays off.
_Avoid_: clipboard backend

### Popup

**PopupScene**:
The measured popup: positioned text runs, rects, and hit targets in one uniform pixel
space, unit-agnostic — whatever the platform hands in (physical pixels on Linux, DIPs on
Windows); core never sees a scale factor.
_Avoid_: draw list, render tree, draw-command IR

**Layer surface**:
The popup's Wayland surface.
_Avoid_: popup window (that's the Win32 HWND)

**Panel**:
The popup's visible rounded rect.

**Hit target**:
A rect in a PopupScene that accepts a click.
_Avoid_: button, widget

### Input

**Cursor channel**:
The source of global cursor position on Linux.
_Avoid_: mouse hook, pointer tracker

**Trigger channel**:
The source of trigger-key press/release on Linux.
_Avoid_: keyboard hook, hotkey listener

**Control socket**:
The UNIX socket where verbs arrive from outside the daemon: `chibipop ctl` from compositor
keybinds, `reload` from the settings process. One verb per global action — the portal
registers only `trigger` and `anki-add`, so every other action is bound natively. Still
transport, not a scripting API.
_Avoid_: IPC server, command socket

**Settings process**:
The separate `chibipop settings` process that owns the settings window on Linux; applies
by saving config and sending `reload`.
_Avoid_: settings thread, settings dialog

### Platform

**Portable mode**:
Everything lives beside the executable, selected by a config file existing there.
Never mixed with XDG mode — one or the other, decidable from that file alone.
_Avoid_: beside-exe fallback

**Instance lock**:
The per-display guard ensuring one daemon per compositor session.
_Avoid_: mutex, pidfile

**Lookup log**:
The opt-in record of looked-up words — screen content, never written without the
opt-in. Distinct from diagnostics, which are always on.
_Avoid_: debug log (that's diagnostics)

**Portable field**:
A setting rendered and honored on every platform; one shared value.

**Platform field**:
A setting only one platform renders and honors; the others carry it untouched so a
config file round-trips.
_Avoid_: windows-only setting, linux extension

### Verification

**Geometry snapshot**:
A committed per-fixture record of measured popup geometry — element rects, hit targets,
content height — compared exactly against a later build.
_Avoid_: screenshot test, image golden

**Bless run**:
The CI dispatch that rewrites geometry snapshots from the current build for human review;
the only way goldens change.
_Avoid_: golden update script

**Render parity**:
Agreement with Yomitan's intended rendering semantics — what its CSS and defaults say an
entry should look like — not pixel equality with a browser.
_Avoid_: pixel parity, screenshot parity

**Sweep**:
A local-only run that renders real corpus entries headlessly and flags every render
invariant violation as a candidate. The corpus never enters the repo or CI.
_Avoid_: fuzzer, crawler

**Candidate**:
A deduplicated invariant violation awaiting adjudication: one shape signature, one
exemplar entry, an occurrence count. Never committed.
_Avoid_: finding, hit

**Shape signature**:
The fingerprint that makes two violations the same candidate: the violated invariant
plus the structural path and resolved selectors around the violation.
_Avoid_: hash, dedupe key

**Suppression list**:
The committed memory of adjudicated non-bugs, keyed by shape signature with a one-line
reason each; the sweep skips and counts them.
_Avoid_: ignore list, allowlist

**Render invariant**:
A self-consistency property every rendered scene must hold — no dropped text, no orphan
fragment, bounded gaps, no overflow. A violation is a candidate, not yet a bug.
_Avoid_: assertion, lint

**Adjudication**:
The judgment that turns a candidate into a bug or a non-bug, reasoned from render
parity and the dictionary author's evident intent.
_Avoid_: triage (that word is the issue-tracker role flow)

**Fragment test**:
The regression form every confirmed bug distills to: the minimal verbatim JSON and CSS
quoted from the real archive, geometry pinned to one number.
_Avoid_: corpus golden, snapshot test

### Geometry

**Physical pixels**:
Core's only coordinate space (`PhysPoint`/`PhysRect`) — the pixels OCR actually sees.
_Avoid_: logical coordinates (in core), DIPs
