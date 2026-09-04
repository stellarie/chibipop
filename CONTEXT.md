# chibipop

Hover the cursor over a word on the screen. The application captures the word, reads it
with OCR, and shows a Japanese dictionary entry in a popup. The system has one shared core
and one binary per platform.

## Language

### Architecture

**Core**:
The platform-neutral crate (`chibipop`) contains the controller, the worker pipeline,
lookup, and dictionary build. It also contains the settings model, presentation, geometry,
and the theme.
_Avoid_: shared lib, common

**Platform bin**:
A per-OS binary crate (`chibipop-windows`, `chibipop-linux`) that owns the real OS event
loop and all OS-coupled code. The bin is the platform adapter.
_Avoid_: platform layer crate, backend crate

**Controller**:
The hover and popup state machine that the core owns. The state machine receives `Event`
objects and returns `Command` objects.
_Avoid_: app, orchestrator, message loop

**Event**:
An input to the Controller that the platform bin synthesizes. Examples are ticks, cursor
moves, trigger keys, scrolls, clicks, worker results, placed popups, tray actions, and
quit events.

**Command**:
An instruction that the Controller returns for the platform bin to execute. Examples are
requests for lookup, commands to show, move, or hide popups, and cursor shape changes.
Other examples are scroll or click arming, settings commands, and exit commands.
_Avoid_: effect, action

**Worker**:
The background pipeline that the core owns (capture → OCR → lookup → present). The
pipeline receives `Trigger` objects and returns `WorkerResult` objects.

### Seams

**RegionCapture**:
The capture seam that returns the newest content of a physical-pixel region. The seam
never blocks on damage.
_Avoid_: screen grabber, screenshot

**Capture backend**:
An implementation of RegionCapture that the system selects at runtime by advertised
capability.
_Avoid_: capture provider, capture driver

**OcrEngine**:
The recognition seam that accepts an image and returns text lines with per-word geometry.

**TextSource**:
The core-internal facade over RegionCapture + OcrEngine + shared layout. The facade
accepts a point and returns a text span.

**Capture mask**:
The core blanks its own on-screen rectangles from a captured image before OCR. The engine
drops words that touch a mask edge, as at a capture edge.
_Avoid_: redaction, popup filter

**Capture guard**:
The Windows-only hide and reshow of chibipop windows around a grab. The capture guard is
not built on Linux.
_Avoid_: capture lock

**Dwell re-check**:
The damage-gated watch kept while the cursor rests on a shown popup. The popup refreshes
when the screen changes beneath it. The watch runs in live mode and in a latched Toggle mode.
_Avoid_: polling loop, refresh timer

**Frozen grab**:
The press-time full-output capture for hold-key mode. While hold-key mode holds the grab, every
lookup reads this capture. The capture happens before the popup exists, so it needs no mask and
reads through the popup. Toggle mode reads live grabs with the popup masked while latched. Press
mode reads one live masked grab for each press.
_Avoid_: snapshot, screenshot buffer

**TextMeasure**:
The measurement seam that accepts an ordered list of styled spans and a wrap width. The
seam returns per-line and per-span geometry plus a baseline per line. The TextMeasure seam
never paints.
_Avoid_: text renderer, font backend

**Styled span**:
A run of text with one resolved style: family, size, weight, italic, and color. It is the
finest unit that the measurement seam addresses. Color travels with the span for the paint
walk of the bin. No geometry depends on color.
_Avoid_: text run (ambiguous with a PopupScene's positioned runs)

**Line box**:
The geometry of one wrapped line that the measurer reports: top, width, height, and the
baseline for its spans. A line is as tall as its tallest span.
_Avoid_: line metrics (that names the whole reply, not one line's share)

**Block box**:
A block tag draws margin, border, padding, and fill around **all** of its content, as a
browser does. For example, a bordered `div` that holds three paragraphs draws one border
around all three paragraphs.
_Avoid_: paragraph box, block_box on a run (a box belongs to the block, not to a line)

### Selection

**Card selection**:
The ranges that a user selects in one Card. `Selections` stores one `CardSelection` for
each `all_cards` index.
_Avoid_: selected text, text selection

**Text address**:
A position in a Card. It combines an Entry ordinal with a `DocAddr` in document order.
_Avoid_: screen coordinate, cursor position

**Click chain**:
A sequence of presses within 500 ms and 4 physical pixels. Each stage selects a grapheme,
word, Sense, or Entry.
_Avoid_: multi-click

**Selection highlight**:
A rectangle that layout derives from selected source bytes. The platform bin paints it with
the theme `accent`.
_Avoid_: text marker, selection overlay

**Entry check**:
The tri-state checkbox for one Entry. It shows None, Partial, or All coverage.
_Avoid_: selection button, checkbox

### Analysis

**Japanese analysis**:
The core module that uses Vibrato and the IPADIC model to return Morphemes and Word groups
for glossary selection.
_Avoid_: tokenizer, segmentation

**Morpheme**:
A fine token from Japanese analysis. It stores a text range, surface, lemma, reading,
inflection, part of speech, and labels.
_Avoid_: word

**Word group**:
A range that Japanese analysis builds from morphemes for double-click selection. It is a
full conjugation or a compound noun. It includes auxiliaries, suffixes, bound verbs, and
conjunctive particles.
_Avoid_: token, morpheme

### Dictionary

**Dictionary**:
One installed archive that its exact name identifies. A user orders and enables this
archive. A Dictionary is distinct from an Entry, which is one record in a Dictionary for
one headword.
_Avoid_: dict (fine in code, not in prose), source, library (that's all of them)

**Dictionary role**:
A capability that a Dictionary provides: terms, frequency, or pitch. A role is a set and
not one value. An archive that carries both a term bank and frequency data holds two
roles. The system always reads the role set from the archive banks and never from a
filename. A user never declares a role.
_Avoid_: kind, type, Editorial role (that classifies GlossDoc nodes, not Dictionaries)

**Dictionary list**:
The ordered and independently managed list of Dictionaries that hold one role. Position
sets priority. Each list carries its own enabled state. Thus, one Dictionary can lead the
frequency list while disabled in the terms list.
_Avoid_: display order (one list among three), priority list, section (a UI word)

**Entry**:
One record in a Dictionary for one headword. A lookup returns this unit. The Entry carries
the glosses and the part-of-speech labels of the headword.
_Avoid_: definition, sense (an Entry contains senses; it is not one)

**Sense**:
One distinct meaning in an Entry. Senses live inside the gloss content as sibling blocks,
not as separate Entries. A headword lookup returns one Entry that holds every sense. Most
Dictionaries number a sense with `①`, `❶`, `㋐`, or `1`. A nested number is a sub-sense.
_Avoid_: gloss (a Sense may have several), definition

**Gloss**:
One phrasing of the meaning of a Sense. A Sense with three synonyms has three glosses.

**GlossDoc**:
The gloss content of an Entry as a typed tree. The core parses this tree once from the
structured content of the dictionary. This tree is the only gloss representation in core.
The popup, the Anki card, and plain text are all renderers over it.
_Avoid_: glossary HTML, gloss string, structured content (that's the source format)

**Editorial role**:
The classified purpose of a GlossDoc node: example, attribution, part-of-speech, or
ordinary content. The render settings filter on this purpose.
_Avoid_: drop list

**Media store**:
The assets that the build extracts from dictionary archives, with their intrinsic
dimensions recorded. Inline gaiji resolve against this store.
_Avoid_: image cache (that's the per-bin decoded-surface one)

**Reported frequency**:
The claim of one Dictionary about how common a headword is. The Reported frequency the
popup shows is always a real Dictionary's number and never a computed number.
_Avoid_: freq (ambiguous with Frequency rank), rank

**Frequency rank**:
The single commonness number that a lookup uses to rank results. The Ranking strategy
reduces every Reported frequency to this number.
_Avoid_: frequency (that's one Dictionary's claim), score (that's the ranker's output)

**Ranking strategy**:
The rule that reduces many Reported frequencies to one Frequency rank: best rank,
priority, or median. A Dictionary that lacks the word is not a data point.
_Avoid_: frequency mode (Yomitan's archive field), algorithm

**Reindex**:
The recalculation of every Frequency rank from Reported frequencies already in the
database. Reindex is one in-place transaction and never reads an archive. This transaction
is the cost of a change to a Ranking strategy, order, or enabled state. A reindex is never
a rebuild.
_Avoid_: rebuild (that reads archives and swaps the file), refresh, rerank

**Pitch pattern**:
The accent that one Dictionary records for one reading. This pattern includes the mora
after which the pitch drops, plus the nasal and devoiced moras recorded with it. A reading
can carry several patterns. A Dictionary can report several patterns for one reading.
_Avoid_: accent (overloaded), pitch accent data, downstep (that's one field of it)

### Actions

**Mining screenshot**:
The user-drawn PNG file saved beside a mined card, attached to the Anki note as a picture.
Core owns the rule with its guards, filename, picture field, and the AnkiConnect call.
Each bin only picks a region and grabs pixels.
_Avoid_: capture (that's RegionCapture), context image

**Region selector**:
The full-output surface where a user drags a rectangle. The surface shows a dimmed screen,
clears the selection, and Esc or right-click cancels. On Windows, this is a modal picker
window. On Linux, this is an `Overlay`-layer surface, and it is the one surface allowed
keyboard focus.
_Avoid_: crosshair, snipping tool

**Outline overlay**:
The click-through frame-only surface that draws rects over the screen. It draws scan
boxes, the matched word, and the static region. It never takes input.
_Avoid_: highlight window, debug overlay

**Static region**:
The fixed rect where `SentenceMode::Static` reads its sentence. The user draws the rect
once, and the software saves it in the configuration. It is independent of the mode, so a
mode change cannot cause data loss.
_Avoid_: sentence box, capture region

**Clipboard rung**:
The Wayland protocol that chibipop uses to take the selection. It tries
`ext-data-control-v1`, and then it tries `wlr-data-control-unstable-v1`. If the compositor
advertises neither protocol, this is a state and not an error. The action reports this
state and stays off.
_Avoid_: clipboard backend

### Popup

**PopupScene**:
The measured popup that contains positioned text runs, rects, and hit targets in one
uniform pixel space. It is unit-agnostic and uses the units that the platform provides.
These units are physical pixels on Linux and DIPs on Windows. Core never sees a scale
factor.
_Avoid_: draw list, render tree, draw-command IR

**Layer surface**:
The Wayland surface of the popup.
_Avoid_: popup window (that's the Win32 HWND)

**Panel**:
The visible rounded rect of the popup.

**Hit target**:
A rect in a PopupScene that accepts a click.
_Avoid_: button, widget

### Input

**Cursor channel**:
The source of the global cursor position on Linux.
_Avoid_: mouse hook, pointer tracker

**Trigger channel**:
The source of trigger-key press and trigger-key release events on Linux.
_Avoid_: keyboard hook, hotkey listener

**Toggle mode**:
One trigger-key press shows the popup and latches the trigger. While the latch is on, each lookup
reads a live grab with the popup masked, so lookups see screen changes. The next press hides the
popup and turns off the latch. Per-character lookup is inert in this mode, as in hold-key mode.
_Avoid_: latch mode, sticky mode

**Press mode**:
One trigger-key press runs one lookup at the cursor. The lookup reads a live grab with the popup masked.
If it finds text, the popup shows and stays. Hover never follows the cursor, and no Dwell re-check runs.
If a press finds no text, the popup hides. A press over the popup also hides it because the mask gives
no text. A button press outside the popup also hides it. The key release does nothing. Per-character
lookup is inert in this mode.
On Windows, the low-level mouse hook observes an outside button press without swallowing it, so the
press reaches the window under the popup.
_Avoid_: one-shot mode, sticky popup

**Click catcher**:
The transparent full-output layer surface that the daemon gives every output while a Press-mode popup is placed.
Its input region covers the output except a hole over the popup rectangle. It is never unmapped. When
the daemon hides the popup, it clears the input region. It swallows the outside button press, so that
press does not reach the window under it.
Wayland gives a client no other way to observe the click because there is no evdev path and the popup
takes no focus.
_Avoid_: dismiss layer, shield

**Control socket**:
The UNIX socket where verbs arrive from outside the daemon. Keybinds from the compositor
send `chibipop ctl`, and the settings process sends `reload`. One verb exists for each
global action. The portal registers only `trigger` and `anki-add`, so the system binds
each other action natively. The socket is a transport mechanism, not a scripting API.
_Avoid_: IPC server, command socket

**Settings process**:
The separate `chibipop settings` process that owns the settings window on Linux. It
applies changes when it saves the configuration and sends `reload`.
_Avoid_: settings thread, settings dialog

### Platform

**Portable mode**:
A mode where all files stay beside the executable. The existence of a configuration file
beside the executable activates this mode. It never mixes with XDG mode. A system uses one
mode or the other mode, which the configuration file alone determines.
_Avoid_: beside-exe fallback

**Instance lock**:
The per-display guard that permits only one daemon for each compositor session.
_Avoid_: mutex, pidfile

**Lookup log**:
The opt-in record of looked-up words that contains screen content. The software never
writes this record without the opt-in setting. The log is distinct from diagnostics, which
are always on.
_Avoid_: debug log (that's diagnostics)

**Portable field**:
A setting that every platform renders and honors as one shared value.

**Platform field**:
A setting that only one platform renders and honors. Other platforms preserve the setting
untouched so that a configuration file round-trips.
_Avoid_: windows-only setting, linux extension

### Verification

**Geometry snapshot**:
A committed per-fixture record of measured popup geometry. It contains element rects, hit
targets, and content height. The verification process compares this record exactly against
a later build.
_Avoid_: screenshot test, image golden

**Bless run**:
The CI dispatch that rewrites geometry snapshots from the current build for human review.
A bless run is the only way a golden changes.
_Avoid_: golden update script

**Render parity**:
Agreement with the intended rendering semantics of Yomitan. It matches what the CSS and
defaults of Yomitan define for an entry. It is not pixel equality with a browser.
_Avoid_: pixel parity, screenshot parity

**Sweep**:
A local-only run that renders real corpus entries headlessly. It records every render
invariant violation as a candidate. The corpus never enters the repository or CI.
_Avoid_: fuzzer, crawler

**Candidate**:
A deduplicated invariant violation that waits for adjudication. It contains one shape
signature, one exemplar entry, and an occurrence count. A candidate is never committed.
_Avoid_: finding, hit

**Shape signature**:
The fingerprint that identifies two violations as the same candidate. It contains the
violated invariant, the structural path, and the resolved selectors around the violation.
_Avoid_: hash, dedupe key

**Suppression list**:
The committed memory of adjudicated non-bugs. It is keyed by shape signature, and each
entry has a one-line reason. The sweep skips and counts these entries.
_Avoid_: ignore list, allowlist

**Render invariant**:
A self-consistency property that every rendered scene must obey. Required properties
include no dropped text, no orphan fragment, bounded gaps, and no overflow. A violation is
a candidate and is not yet a bug.
_Avoid_: assertion, lint

**Adjudication**:
The judgment that decides if a candidate is a bug or a non-bug. A developer reasons from
render parity and the clear intent of the dictionary author.
_Avoid_: triage (that word is the issue-tracker role flow)

**Fragment test**:
The regression form that represents each confirmed bug. It uses minimal verbatim JSON and
CSS taken from the real archive, and it pins geometry to one number.
_Avoid_: corpus golden, snapshot test

### Geometry

**Physical pixels**:
The only coordinate space (`PhysPoint`/`PhysRect`) in core. These are the pixels that OCR
actually sees.
_Avoid_: logical coordinates (in core), DIPs
