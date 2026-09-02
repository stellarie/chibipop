//! The core-owned background pipeline: capture -> OCR -> lookup ->
//! present (ARCHITECTURE.md#workspace-and-seams). Fed `Trigger`s, yields
//! `WorkerResult`s over plain mpsc channels; the platform bin supplies
//! the two seams and a wake callback, and drives everything else from
//! its own event loop.

use crate::config::SentenceMode;
use crate::controller::{LookupOutcome, RequestId};
use crate::geom::{PhysPoint, PhysRect, ScanDisplay, ScanKind, ScanRect};
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::present::{self, DictInfo, PresentConfig};
use crate::text::layout::{CaptureSize, OcrLine, Resolved};
use crate::text::mask::CaptureMask;
use crate::text::{OcrEngine, RegionCapture, SettingsSnapshot, TextSource};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread;

/// One hover: where the cursor is, and what its grab must not read.
#[derive(Clone, Copy)]
pub struct Hover {
    pub at: PhysPoint,
    /// Our own popup where the platform cannot exclude it
    /// (ARCHITECTURE.md#capture-and-masking). `CaptureMask::NONE` on a
    /// frozen grab, which predates the popup, and on platforms that
    /// exclude the surface themselves.
    pub mask: CaptureMask,
}

/// Hover, drill-down, reload, and trigger mode's freeze.
pub enum TriggerKind {
    Hover(Hover),
    DrillDown(String),
    Reload(Box<WorkerSettings>),
    /// Trigger press: take one full grab of the output holding this
    /// point and read every lookup out of it until [`TriggerKind::Thaw`]
    /// (ARCHITECTURE.md#hover-cadence). Sent again mid-hold when the
    /// cursor crosses onto another output, which is what makes the
    /// second monitor live.
    ///
    /// Answers nothing: it is state, not a lookup. A grab that fails is
    /// reported by the lookups that follow it, which is where a user
    /// can see it.
    Freeze(PhysPoint),
    /// Trigger release: drop the frozen frame, grabs go live again.
    Thaw,
    /// A wake, and nothing else: the `serve` hook has a job waiting and
    /// the worker is blocked on this channel (see [`ServeNudge`]).
    ///
    /// Answers nothing and changes nothing, so its `id` is never read.
    Serve,
}

/// What the worker owns.
pub struct WorkerSettings {
    pub max_passes: u8,
    /// Per-platform capture upscale, Windows 2 and Linux 1
    /// (ARCHITECTURE.md#ocr-engine).
    pub upscale: i32,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
    pub language: String,
    pub present_cfg: PresentConfig,
    pub scan_display: ScanDisplay,
    /// How the Anki sentence field is assembled (upstream 0.9.x
    /// sentence capture).
    pub sentence_mode: SentenceMode,
    /// The user-drawn box [`SentenceMode::Static`] reads from.
    pub static_region: Option<PhysRect>,
    /// Refreshed by every edit.
    pub dicts: Vec<DictInfo>,
}

impl WorkerSettings {
    /// The OCR half, for the facade - and for a bin to assert what it
    /// hands `TextSource` (the Linux crate pins `upscale: 1` through it).
    pub fn snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            max_passes: self.max_passes,
            upscale: self.upscale,
            prefer_vertical: self.prefer_vertical,
            capture: self.capture,
            scan_alphanumeric: self.scan_alphanumeric,
        }
    }
}

/// One gated cursor movement.
pub struct Trigger {
    pub kind: TriggerKind,
    pub id: RequestId,
}

/// One answer, carrying its id.
pub struct WorkerResult {
    pub id: RequestId,
    pub outcome: LookupOutcome,
}

/// How a `reload` gets a fresh view of the dictionary file.
///
/// A rebuild builds beside the database and renames over it, so the
/// handle the worker holds keeps reading the inode it opened: reopening
/// is what serves the new dictionary, and this is the reopen.
pub type ReopenDict = Box<dyn Fn() -> Result<Box<dyn Dictionary>>>;

/// A between-lookups job runner, lent the OCR facade (see
/// `WorkerParts::serve`).
pub type ServeHook = Box<dyn FnMut(&TextSource)>;

/// What the bin supplies, built on the worker thread.
///
/// Built there because backends may be thread-affine (COM apartments,
/// per-thread caches); the `open` closure runs after the thread exists,
/// so nothing here needs to be `Send`.
pub struct WorkerParts {
    pub capture: Box<dyn RegionCapture>,
    pub ocr: Box<dyn OcrEngine>,
    pub dict: Box<dyn Dictionary>,
    /// Called on every reload, when the bin supplies one.
    ///
    /// `None` where a rebuild replaces the whole process instead - the
    /// Windows bin restarts itself on a finished build, so its worker
    /// never outlives the database it opened and has nothing to reopen.
    /// A reopen that fails keeps the handle it has: an out-of-date
    /// dictionary still answers, a dropped one answers nothing.
    pub reopen_dict: Option<ReopenDict>,
    pub engine: LookupEngine,
    /// Runs one-off jobs against the OCR facade between lookups: a job
    /// (OCR-to-clipboard) must run on this thread because engines are
    /// thread-affine.
    ///
    /// Called once per wake, immediately before the worker blocks on its
    /// trigger channel - never on a timer. The queue the hook drains is
    /// the bin's own, and the worker cannot see it, so the producer must
    /// queue the job and then wake the worker with [`ServeNudge`];
    /// the idle budget is 0 wakeups/s and a poll would spend it on
    /// nothing. `None` costs nothing.
    pub serve: Option<ServeHook>,
}

/// The pipeline's handle: trigger in, result out.
pub struct Worker {
    trigger_tx: mpsc::Sender<Trigger>,
    result_rx: mpsc::Receiver<WorkerResult>,
}

impl Worker {
    /// Spawn it, await startup.
    ///
    /// `open` builds the platform parts on the worker thread; `wake` is
    /// called after every result is queued, so the bin's event loop knows
    /// to drain [`Worker::results`]. Returns the dictionary identities the
    /// worker read at startup.
    pub fn spawn(
        settings: WorkerSettings,
        open: impl FnOnce() -> Result<WorkerParts> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(Worker, Vec<DictInfo>)> {
        let (trigger_tx, trigger_rx) = mpsc::channel::<Trigger>();
        let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
        let (startup_tx, startup_rx) = mpsc::channel::<Result<Vec<DictInfo>>>();

        // Never joined - the bin exits with the thread parked in recv.
        thread::spawn(move || {
            worker_main(settings, open, wake, trigger_rx, result_tx, startup_tx);
        });

        let dicts: Vec<DictInfo> = startup_rx
            .recv()
            .context("worker thread ended before completing startup")??;

        Ok((Worker { trigger_tx, result_rx }, dicts))
    }

    /// Where triggers go in.
    pub fn trigger(&self) -> &mpsc::Sender<Trigger> {
        &self.trigger_tx
    }

    /// Where results come out; drained on `wake`.
    pub fn results(&self) -> &mpsc::Receiver<WorkerResult> {
        &self.result_rx
    }

    /// A handle for waking this worker's `serve` hook.
    ///
    /// Cheap to clone; whoever queues one-off OCR jobs holds one.
    pub fn serve_nudge(&self) -> ServeNudge {
        ServeNudge(self.trigger_tx.clone())
    }
}

/// Wakes a worker that has a `serve` job waiting.
///
/// The worker blocks on its trigger channel indefinitely, and the job
/// queue is the bin's - so queueing pixels is only half of handing them
/// over. Queue first, then nudge: the hook runs before the worker blocks
/// again, so a nudge swallowed by a batch that was already in flight
/// still leaves a hook run behind it.
#[derive(Clone)]
pub struct ServeNudge(mpsc::Sender<Trigger>);

impl ServeNudge {
    /// Wake the worker.
    pub fn nudge(&self) {
        // A worker that is gone is not this call's error to report: the
        // job's own result channel says so, to the caller waiting on it.
        let _ = self.0.send(Trigger { kind: TriggerKind::Serve, id: RequestId(0) });
    }

    /// A nudge with no worker behind it, for a caller assembled without
    /// a pipeline (a bin's test context): the job is queued and never
    /// served, which is the honest answer when nothing owns an engine.
    pub fn disconnected() -> Self {
        ServeNudge(mpsc::channel().0)
    }
}

/// What a drained batch settles before its newest hover: settings, and
/// trigger mode's freeze state.
///
/// Kept in arrival order, unlike hovers - a reload and a press are
/// state, and state cannot coalesce.
enum Pre {
    Reload(WorkerSettings),
    Freeze(PhysPoint),
    Thaw,
}

/// The reloadable state a lookup consults: everything a `Reload`
/// replaces short of the OCR settings (those live in the
/// `TextSource`) and the dictionary handle itself.
struct LookupState {
    present_cfg: PresentConfig,
    scan_display: ScanDisplay,
    sentence_mode: SentenceMode,
    static_region: Option<PhysRect>,
    /// Refreshed by every Reload.
    dicts: Vec<DictInfo>,
}

/// One reload into the cache: settings, and a fresh look at the file.
///
/// `dicts` goes stale otherwise - and so does the dictionary handle,
/// which is why a bin that survives its own rebuilds supplies a reopen.
/// The reopened file's own identities win over the ones the bin sent:
/// the bin's list is what it knew before the rebuild, this one is what
/// the database says now.
fn take_reload(
    s: WorkerSettings,
    reopen: Option<&ReopenDict>,
    dict: &mut Box<dyn Dictionary>,
    state: &mut LookupState,
) {
    state.present_cfg = s.present_cfg;
    state.scan_display = s.scan_display;
    state.sentence_mode = s.sentence_mode;
    state.static_region = s.static_region;
    state.dicts = s.dicts;
    let Some(reopen) = reopen else { return };
    match reopen().and_then(|fresh| {
        let identities = fresh.dicts().context("reading dictionary identities")?;
        Ok((fresh, identities))
    }) {
        Ok((fresh, identities)) => {
            *dict = fresh;
            state.dicts = identities;
        }
        // The handle we hold still answers; a dropped one would not.
        Err(e) => eprintln!("chibipop: reopening the dictionary failed: {e:#}"),
    }
}

/// Newest hover; every state change, in order.
fn drain(first: Trigger, rx: &mpsc::Receiver<Trigger>) -> (Option<Trigger>, Vec<Pre>) {
    let mut pre = Vec::new();
    let mut hover = None;
    let mut take = |t: Trigger| match t.kind {
        TriggerKind::Reload(s) => pre.push(Pre::Reload(*s)),
        TriggerKind::Freeze(at) => pre.push(Pre::Freeze(at)),
        TriggerKind::Thaw => pre.push(Pre::Thaw),
        // A wake, already spent by arriving.
        TriggerKind::Serve => {}
        _ => hover = Some(t),
    };
    take(first);
    while let Ok(next) = rx.try_recv() {
        take(next);
    }
    (hover, pre)
}

/// Serves triggers, owns OCR.
fn worker_main(
    settings: WorkerSettings,
    open: impl FnOnce() -> Result<WorkerParts>,
    wake: impl Fn(),
    trigger_rx: mpsc::Receiver<Trigger>,
    result_tx: mpsc::Sender<WorkerResult>,
    startup_tx: mpsc::Sender<Result<Vec<DictInfo>>>,
) {
    let WorkerParts { capture, ocr, mut dict, reopen_dict, engine, mut serve } = match open() {
        Ok(p) => p,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };

    let dicts: Vec<DictInfo> = match dict.dicts().context("reading dictionary identities") {
        Ok(d) => d,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };

    let mut source = TextSource::new(capture, ocr, settings.snapshot());
    let mut state = LookupState {
        present_cfg: settings.present_cfg,
        scan_display: settings.scan_display,
        sentence_mode: settings.sentence_mode,
        static_region: settings.static_region,
        dicts,
    };

    // An Arc would be ceremony.
    if startup_tx.send(Ok(state.dicts.clone())).is_err() {
        return; // main thread gave up waiting; nothing left to do.
    }

    // Sender dropped: shutdown.
    loop {
        // Anything the bin queued for the hook runs before we block, so
        // a nudge that a batch swallowed mid-lookup cannot leave its job
        // waiting - and an idle worker with a hook installed still
        // blocks, it does not poll (ARCHITECTURE.md#hover-cadence).
        if let Some(hook) = &mut serve {
            hook(&source);
        }
        let Ok(first) = trigger_rx.recv() else { break };
        let (hover, pre) = drain(first, &trigger_rx);
        for change in pre {
            match change {
                Pre::Reload(s) => {
                    source.apply_settings(s.snapshot(), &s.language);
                    take_reload(s, reopen_dict.as_ref(), &mut dict, &mut state);
                }
                // The press-time grab: one full output, before any
                // popup exists (ARCHITECTURE.md#hover-cadence). A
                // failure is remembered by the source, so the hold's
                // lookups report it.
                Pre::Freeze(at) => {
                    if let Err(e) = source.freeze(at) {
                        eprintln!("chibipop: the trigger-press grab failed: {e:#}");
                    }
                }
                Pre::Thaw => source.thaw(),
            }
        }
        let Some(trigger) = hover else {
            continue;
        };

        // One bad frame is not fatal.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match &trigger.kind {
                TriggerKind::Hover(h) => {
                    resolve_trigger(&mut source, dict.as_ref(), &engine, &state, *h)
                }
                TriggerKind::DrillDown(text) => resolve_drilldown(
                    dict.as_ref(),
                    &engine,
                    &state.dicts,
                    &state.present_cfg,
                    text,
                ),
                TriggerKind::Reload(_)
                | TriggerKind::Freeze(_)
                | TriggerKind::Thaw
                | TriggerKind::Serve => {
                    LookupOutcome::Failed("a state change reached the hover path".to_string())
                }
            }
        }))
        .unwrap_or_else(|_| LookupOutcome::Failed("a hover lookup panicked".to_string()));

        if result_tx.send(WorkerResult { id: trigger.id, outcome }).is_err() {
            break; // main thread gone
        }
        wake();
    }
}

/// One hover: OCR to present.
fn resolve_trigger(
    source: &mut TextSource,
    dict: &dyn Dictionary,
    engine: &LookupEngine,
    state: &LookupState,
    hover: Hover,
) -> LookupOutcome {
    if state.sentence_mode == SentenceMode::Static {
        if let Some(region) = state.static_region {
            return resolve_static(source, dict, engine, state, hover, region);
        }
        // No region yet; fall through.
        eprintln!("chibipop: static mode but no region set; using line mode");
    }
    let raw = source.resolve_at_tiled_scanned(hover.at, state.scan_display.captures, hover.mask);
    let (resolved, scan, ocr_lines) = match raw {
        Ok((Some(r), scan, lines)) => (r, scan, lines),
        Ok((None, _, _)) => return LookupOutcome::Hide,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };
    let sentence = || match state.sentence_mode {
        SentenceMode::All => join_all_lines(&ocr_lines),
        // `Static` reaches here only with no region drawn.
        SentenceMode::Line | SentenceMode::Static => {
            extract_sentence_line(&resolved.span.text, resolved.span.cursor_byte_offset).to_string()
        }
    };
    // The tiled path is the one with an overlay to draw on.
    let outline = state.scan_display.highlight;
    present_lookup(dict, engine, state, &resolved, sentence, scan, outline)
}

/// Static-region capture path ([`SentenceMode::Static`]): one read of
/// the user-drawn box, sentence = everything the box holds.
fn resolve_static(
    source: &mut TextSource,
    dict: &dyn Dictionary,
    engine: &LookupEngine,
    state: &LookupState,
    hover: Hover,
    region: PhysRect,
) -> LookupOutcome {
    let read = match source.resolve_in_region(hover.at, region, hover.mask) {
        Ok(r) => r,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };
    let Some(resolved) = read.resolved else {
        return LookupOutcome::Hide;
    };
    let lines = read.lines;
    // Nothing to draw: this path has no capture boxes to show, so it
    // grows no overlay and takes no match outline either.
    present_lookup(dict, engine, state, &resolved, || join_all_lines(&lines), Vec::new(), false)
}

/// What both capture paths do once a span is resolved: look the text
/// under the cursor up, present the hits, attach the Anki sentence, and
/// outline the match.
///
/// The sentence is a closure because a hover that hits nothing must not
/// pay for assembling one. `scan` is whatever rects the path already
/// collected; the match joins them last - drawn over the capture boxes -
/// when `outline_match` and a match rect exist.
fn present_lookup(
    dict: &dyn Dictionary,
    engine: &LookupEngine,
    state: &LookupState,
    resolved: &Resolved,
    sentence: impl FnOnce() -> String,
    mut scan: Vec<ScanRect>,
    outline_match: bool,
) -> LookupOutcome {
    let text = &resolved.span.text[resolved.span.cursor_byte_offset..];
    let hits = match engine.run(dict, text) {
        Ok(h) => h,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };
    if hits.is_empty() {
        return LookupOutcome::Hide;
    }

    let mut presentation = present::build(&hits, &state.dicts, &state.present_cfg, dict);
    presentation.sentence = Some(sentence());
    let matched = present::match_highlight(&resolved.span, presentation.top.as_ref());
    if outline_match {
        if let Some(rect) = matched {
            scan.push(ScanRect { rect, kind: ScanKind::Match });
        }
    }
    LookupOutcome::Ready {
        presentation: Box::new(presentation),
        anchor: resolved.span.anchor,
        orientation: resolved.orientation,
        matched,
        scan,
    }
}

/// The `\n`-delimited OCR line the cursor offset falls in.
fn extract_sentence_line(text: &str, cursor_offset: usize) -> &str {
    let mut pos = 0;
    for line in text.split('\n') {
        let end = pos + line.len();
        if cursor_offset >= pos && cursor_offset <= end {
            return line;
        }
        pos = end + 1;
    }
    text
}

/// OCR lines, newline-joined.
fn join_all_lines(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .map(|l| l.words.iter().map(|w| w.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Dict lookup without OCR.
fn resolve_drilldown(
    dict: &dyn Dictionary,
    engine: &LookupEngine,
    dicts: &[DictInfo],
    present_cfg: &PresentConfig,
    text: &str,
) -> LookupOutcome {
    let hits = match engine.run(dict, text) {
        Ok(h) => h,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };
    if hits.is_empty() {
        return LookupOutcome::Hide;
    }
    let p = present::build(&hits, dicts, present_cfg, dict);
    LookupOutcome::DrillDown(Box::new(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::lookup::deconj::Deconjugator;
    use crate::lookup::model::FakeDictionary;
    use crate::text::layout::{Orientation, TextGeom};
    use crate::text::TextSpan;

    fn ws(passes: u8) -> WorkerSettings {
        WorkerSettings {
            max_passes: passes,
            upscale: 2,
            prefer_vertical: false,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
            language: "ja".to_string(),
            present_cfg: Config::default().present_config(&[]),
            scan_display: ScanDisplay { captures: false, highlight: false },
            sentence_mode: SentenceMode::Line,
            static_region: None,
            dicts: Vec::new(),
        }
    }

    #[test]
    fn extract_sentence_line_single_line_returns_it_all() {
        assert_eq!("hello world", extract_sentence_line("hello world", 5));
    }

    #[test]
    fn extract_sentence_line_picks_the_containing_line() {
        let text = "abc\ndef\nghi";
        assert_eq!("def", extract_sentence_line(text, 5));
    }

    /// Inclusive of the line end.
    #[test]
    fn extract_sentence_line_boundary_offset_stays_on_that_line() {
        let text = "abc\ndef";
        assert_eq!("abc", extract_sentence_line(text, 3));
    }

    #[test]
    fn extract_sentence_line_past_the_end_falls_back_to_all() {
        let text = "abc\ndef";
        assert_eq!(text, extract_sentence_line(text, 999));
    }

    fn ocr_word(text: &str) -> crate::text::layout::OcrWord {
        crate::text::layout::OcrWord {
            text: text.to_string(),
            rect: PhysRect { x: 0, y: 0, w: 0, h: 0 },
        }
    }

    #[test]
    fn join_all_lines_joins_multiple_lines_with_newlines() {
        let lines = vec![
            OcrLine { words: vec![ocr_word("これは"), ocr_word("テスト")] },
            OcrLine { words: vec![ocr_word("二行目")] },
            OcrLine { words: vec![ocr_word("三"), ocr_word("行目")] },
        ];
        assert_eq!("これはテスト\n二行目\n三行目", join_all_lines(&lines));
    }

    #[test]
    fn join_all_lines_single_line_has_no_newline() {
        let lines = vec![OcrLine { words: vec![ocr_word("only")] }];
        assert_eq!("only", join_all_lines(&lines));
    }

    #[test]
    fn join_all_lines_empty_input_is_empty_string() {
        assert_eq!("", join_all_lines(&[]));
    }

    /// Newest hover; every state change, in order.
    #[test]
    fn drain_keeps_the_newest_hover_and_every_state_change() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        let reload = TriggerKind::Reload(Box::new(ws(2)));
        tx.send(Trigger { kind: reload, id: RequestId(2) }).unwrap();
        let at = PhysPoint { x: 9, y: 9 };
        let newer = TriggerKind::Hover(Hover { at, mask: CaptureMask::NONE });
        tx.send(Trigger { kind: newer, id: RequestId(3) }).unwrap();
        let second = TriggerKind::Reload(Box::new(ws(4)));
        tx.send(Trigger { kind: second, id: RequestId(4) }).unwrap();
        let at = PhysPoint { x: 1, y: 1 };
        let older = TriggerKind::Hover(Hover { at, mask: CaptureMask::NONE });
        let first = Trigger { kind: older, id: RequestId(1) };
        let (hover, pre) = drain(first, &rx);
        let hover = hover.expect("a hover survives");
        assert!(matches!(hover.kind, TriggerKind::Hover(h) if h.at.x == 9), "newest hover wins");
        let passes: Vec<u8> = pre
            .iter()
            .filter_map(|p| match p {
                Pre::Reload(s) => Some(s.max_passes),
                _ => None,
            })
            .collect();
        assert_eq!(vec![2, 4], passes, "neither reload may be swallowed, and order holds");
    }

    /// A press and its release are state, not lookups: both survive a
    /// batch that also carries a hover, and in the order they arrived.
    #[test]
    fn drain_keeps_a_freeze_and_a_thaw_in_arrival_order() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        let at = PhysPoint { x: 40, y: 50 };
        tx.send(Trigger { kind: TriggerKind::Hover(Hover { at, mask: CaptureMask::NONE }), id: RequestId(2) })
            .unwrap();
        tx.send(Trigger { kind: TriggerKind::Thaw, id: RequestId(3) }).unwrap();
        drop(tx);
        let first = Trigger { kind: TriggerKind::Freeze(at), id: RequestId(1) };
        let (hover, pre) = drain(first, &rx);
        assert!(hover.is_some(), "the hover between them still runs");
        assert!(
            matches!(pre.as_slice(), [Pre::Freeze(p), Pre::Thaw] if *p == at),
            "a freeze and a thaw must not coalesce"
        );
    }

    /// A reload alone still arrives.
    #[test]
    fn drain_returns_no_hover_when_only_a_reload_queued() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        drop(tx);
        let first = Trigger { kind: TriggerKind::Reload(Box::new(ws(3))), id: RequestId(1) };
        let (hover, pre) = drain(first, &rx);
        assert!(hover.is_none());
        assert!(matches!(pre.as_slice(), [Pre::Reload(s)] if s.max_passes == 3));
    }

    fn di(id: i64, name: &str) -> DictInfo {
        DictInfo { dict_id: id, name: name.to_string() }
    }

    /// One dictionary, named whatever the test says.
    fn one_dict(name: &str) -> Box<dyn Dictionary> {
        let mut d = crate::lookup::model::FakeDictionary::new();
        d.add_dict(7, name);
        Box::new(d)
    }

    /// A cache holding these identities, and nothing else a reload cares
    /// about.
    ///
    /// The scope is resolved against those same identities, because that is
    /// what a daemon does: a config naming no Dictionary enables every one
    /// it finds, and resolving against an empty library instead would leave
    /// every one of them switched off.
    fn state_with(dicts: Vec<DictInfo>) -> LookupState {
        LookupState {
            present_cfg: Config::default().present_config(&dicts),
            scan_display: ScanDisplay { captures: false, highlight: false },
            sentence_mode: SentenceMode::Line,
            static_region: None,
            dicts,
        }
    }

    /// Same id, new dictionary.
    #[test]
    fn a_reload_replaces_the_cached_dictionary_identities() {
        let mut state = state_with(vec![di(7, "Removed")]);
        let mut dict = one_dict("Removed");
        let mut s = ws(2);
        s.dicts = vec![di(7, "Added")];

        take_reload(s, None, &mut dict, &mut state);

        assert_eq!(vec![di(7, "Added")], state.dicts, "the removed name must not answer");
    }

    /// The reload gap ticket 41 pinned: a rebuild renames a new database
    /// over the old inode, so only reopening serves it. The reopened
    /// file's identities win over the ones the bin sent, because the bin
    /// only knows what it read before the rebuild.
    #[test]
    fn a_reload_reopens_the_dictionary_and_takes_its_identities() {
        let mut state = state_with(vec![di(7, "BeforeTheRebuild")]);
        let mut dict = one_dict("BeforeTheRebuild");
        let reopen: ReopenDict = Box::new(|| Ok(one_dict("AfterTheRebuild")));

        take_reload(ws(2), Some(&reopen), &mut dict, &mut state);

        assert_eq!(vec![di(7, "AfterTheRebuild")], state.dicts);
        assert_eq!(
            vec![di(7, "AfterTheRebuild")],
            dict.dicts().unwrap(),
            "the handle itself must be the reopened one"
        );
    }

    /// A reopen that fails keeps the handle we have: an out-of-date
    /// dictionary still answers lookups, a dropped one answers nothing.
    #[test]
    fn a_failed_reopen_keeps_the_dictionary_already_open() {
        let mut state = state_with(vec![di(7, "StillHere")]);
        let mut dict = one_dict("StillHere");
        let reopen: ReopenDict = Box::new(|| anyhow::bail!("the database is a directory"));

        take_reload(ws(2), Some(&reopen), &mut dict, &mut state);

        assert_eq!(vec![di(7, "StillHere")], dict.dicts().unwrap());
    }

    /// A dictionary that answers 食, and an engine to ask it with.
    fn eating_dict() -> FakeDictionary {
        let mut d = FakeDictionary::new();
        d.add_dict(1, "FakeDict");
        d.add_term("食", Some("食"), None, "", None, 10, 1);
        d.add_entry(10, 1, r#"["to eat"]"#);
        d
    }

    fn engine() -> LookupEngine {
        LookupEngine::new(Deconjugator::new(Vec::new()))
    }

    /// One word under the cursor, with the geometry a match needs to be
    /// outlined at all.
    fn resolved(text: &str) -> Resolved {
        let rect = PhysRect { x: 10, y: 20, w: 30, h: 40 };
        Resolved {
            span: TextSpan {
                text: text.to_string(),
                cursor_byte_offset: 0,
                anchor: rect,
                geom: vec![TextGeom { char_count: text.chars().count(), rect }],
            },
            orientation: Orientation::Horizontal,
        }
    }

    /// The tail both capture paths share, on a path that draws an
    /// overlay: the sentence it was handed rides along, and the match
    /// joins the rects last, over the capture boxes.
    #[test]
    fn the_shared_tail_attaches_the_sentence_and_outlines_the_match() {
        let state = state_with(vec![di(1, "FakeDict")]);
        let hit = resolved("食");
        let pass1 = vec![ScanRect { rect: hit.span.anchor, kind: ScanKind::Pass1 }];

        let outcome = present_lookup(
            &eating_dict(),
            &engine(),
            &state,
            &hit,
            || "食べた。".to_string(),
            pass1,
            true,
        );

        let LookupOutcome::Ready { presentation, matched, scan, .. } = outcome else {
            panic!("a hit must present something")
        };
        assert_eq!(Some("食べた。".to_string()), presentation.sentence);
        assert!(matched.is_some(), "a hit with geometry has a rect to outline");
        assert_eq!(
            vec![ScanKind::Pass1, ScanKind::Match],
            scan.iter().map(|r| r.kind).collect::<Vec<_>>(),
            "the match draws last, over the capture boxes"
        );
    }

    /// The static-region path draws no overlay, so nothing joins one -
    /// and it still reports the rect the popup highlights with.
    #[test]
    fn the_shared_tail_grows_no_overlay_for_a_path_that_draws_none() {
        let state = state_with(vec![di(1, "FakeDict")]);
        let hit = resolved("食");

        let outcome = present_lookup(
            &eating_dict(),
            &engine(),
            &state,
            &hit,
            || "食".to_string(),
            Vec::new(),
            false,
        );

        let LookupOutcome::Ready { matched, scan, .. } = outcome else {
            panic!("a hit must present something")
        };
        assert!(matched.is_some(), "the popup still gets its highlight rect");
        assert!(scan.is_empty(), "an overlay nobody draws stays empty");
    }

    /// Nothing in the dictionary hides the popup - and a hover that hits
    /// nothing never pays for assembling a sentence.
    #[test]
    fn the_shared_tail_hides_without_assembling_a_sentence_when_nothing_hits() {
        let state = state_with(vec![di(1, "FakeDict")]);
        let miss = resolved("ヽ");

        let outcome = present_lookup(
            &eating_dict(),
            &engine(),
            &state,
            &miss,
            || panic!("a miss must not assemble a sentence"),
            Vec::new(),
            true,
        );

        assert!(matches!(outcome, LookupOutcome::Hide));
    }
}
