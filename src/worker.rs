//! The core-owned background pipeline uses capture -> OCR -> lookup -> present
//! (ARCHITECTURE.md#workspace-and-seams).
//! It accepts `Trigger`s and sends `WorkerResult`s over plain mpsc channels.
//! The platform bin supplies the two seams and the wake callback.
//! The platform bin also drives its own event loop.

use crate::config::SentenceMode;
use crate::controller::{LookupOutcome, RequestId};
use crate::geom::{PhysPoint, PhysRect, ScanDisplay, ScanKind, ScanRect};
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::present::{self, DictInfo, PresentConfig};
use crate::text::layout::{CaptureSize, OcrLine, Orientation, Resolved};
use crate::text::mask::CaptureMask;
use crate::text::sentence;
use crate::text::{OcrEngine, RegionCapture, SettingsSnapshot, TextSource};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread;

/// One hover contains the cursor position and the mask for its grab.
#[derive(Clone, Copy)]
pub struct Hover {
    pub at: PhysPoint,
    /// The popup that the platform cannot exclude from a grab.
    /// (ARCHITECTURE.md#capture-and-masking) `CaptureMask::NONE` applies to
    /// a frozen grab because that grab predates the popup. It also applies
    /// when the platform excludes the surface.
    pub mask: CaptureMask,
}

/// One sentence probe for an Anki add.
///
/// This trigger is separate from [`Hover`]. It needs no hit scan or presentation.
/// It must survive newer hovers until the add can answer.
#[derive(Clone, Copy)]
pub struct SentenceProbe {
    pub anchor: PhysRect,
    pub orientation: Orientation,
    pub mask: CaptureMask,
}

/// The Worker accepts `Hover`, `Sentence`, and `DrillDown` lookups, `Reload`,
/// `Freeze`, and `Thaw` state changes, and `Serve` wake requests.
pub enum TriggerKind {
    Hover(Hover),
    Sentence(SentenceProbe),
    DrillDown(String),
    Reload(Box<WorkerSettings>),
    /// A trigger press takes one full grab of the output that contains this
    /// point. The Worker reads each lookup from that grab until
    /// [`TriggerKind::Thaw`] (ARCHITECTURE.md#hover-cadence).
    /// The platform sends this again when the cursor crosses to another output
    /// while the user holds the trigger. This keeps the second monitor live.
    ///
    /// This variant returns no result because it stores state, not a lookup.
    /// If the grab fails, later lookups report the failure where the user
    /// can see it.
    Freeze(PhysPoint),
    /// A trigger release drops the frozen frame and restores live grabs.
    Thaw,
    /// This wake has no other data. The `serve` hook has a job, and the Worker
    /// waits on this channel (see [`ServeNudge`]).
    ///
    /// This variant returns no result and changes no state, so the Worker
    /// never reads its `id`.
    Serve,
}

/// The settings that the Worker owns.
pub struct WorkerSettings {
    pub max_passes: u8,
    /// The capture scale for each platform. Windows uses 2 and Linux uses 1
    /// (ARCHITECTURE.md#ocr-engine).
    pub upscale: i32,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
    pub language: String,
    pub present_cfg: PresentConfig,
    pub scan_display: ScanDisplay,
    /// The rule that builds the Anki sentence field (upstream 0.9.x
    /// sentence capture).
    pub sentence_mode: SentenceMode,
    /// The user-drawn box that [`SentenceMode::Static`] reads.
    pub static_region: Option<PhysRect>,
    /// The bin refreshes this list after every edit.
    pub dicts: Vec<DictInfo>,
}

impl WorkerSettings {
    /// Return the OCR settings for the facade.
    /// A bin can use this value to check what it gives to `TextSource`.
    /// The Linux crate sets `upscale: 1` through this value.
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

/// One request sent to the Worker with a [`TriggerKind`] and request id.
pub struct Trigger {
    pub kind: TriggerKind,
    pub id: RequestId,
}

/// One result with its request id.
pub struct WorkerResult {
    pub id: RequestId,
    pub outcome: LookupOutcome,
}

/// A `reload` uses this callback to get a new view of the Dictionary file.
///
/// A rebuild creates a new file beside the database and renames it over the old
/// file. The Worker handle still reads the old inode. This callback opens the
/// new file so the Worker can use it.
pub type ReopenDict = Box<dyn Fn() -> Result<Box<dyn Dictionary>>>;

/// A job runner that uses the OCR facade between lookups.
/// See `WorkerParts::serve`.
pub type ServeHook = Box<dyn FnMut(&TextSource)>;

/// The bin creates these parts on the Worker thread.
///
/// This thread owns the parts because a backend can require a thread.
/// Examples include COM apartments and per-thread caches. The `open` closure
/// runs after the thread starts, so this value does not need `Send`.
pub struct WorkerParts {
    pub capture: Box<dyn RegionCapture>,
    pub ocr: Box<dyn OcrEngine>,
    pub dict: Box<dyn Dictionary>,
    /// The bin calls this callback after each reload when it supplies one.
    ///
    /// `None` applies when a rebuild replaces the whole process.
    /// The Windows bin restarts after a build finishes, so its Worker does not
    /// outlive the database that it opened. It does not need this callback.
    /// If this callback fails, keep the current handle. An old Dictionary still
    /// answers. A dropped Dictionary answers nothing.
    pub reopen_dict: Option<ReopenDict>,
    pub engine: LookupEngine,
    /// Run one-off jobs through the OCR facade between lookups.
    /// An OCR-to-clipboard job must use this thread because the engines are
    /// thread-affine.
    ///
    /// The Worker calls this once per wake, just before it waits on the trigger
    /// channel. It never uses a timer.
    /// The hook drains a queue that belongs to the bin. The Worker cannot see
    /// that queue. The producer must queue a job, then wake the Worker with
    /// [`ServeNudge`].
    /// The idle budget is 0 wakeups/s. A poll spends that budget on
    /// nothing. `None` has no cost.
    pub serve: Option<ServeHook>,
}

/// The pipeline handle sends triggers and receives results.
pub struct Worker {
    trigger_tx: mpsc::Sender<Trigger>,
    result_rx: mpsc::Receiver<WorkerResult>,
}

impl Worker {
    /// Start the Worker and wait for startup.
    ///
    /// `open` builds the platform parts on the Worker thread.
    /// The Worker calls `wake` after it queues each result.
    /// The bin event loop then knows to drain [`Worker::results`].
    /// Return the Dictionary identities that the Worker reads at startup.
    pub fn spawn(
        settings: WorkerSettings,
        open: impl FnOnce() -> Result<WorkerParts> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Result<(Worker, Vec<DictInfo>)> {
        let (trigger_tx, trigger_rx) = mpsc::channel::<Trigger>();
        let (result_tx, result_rx) = mpsc::channel::<WorkerResult>();
        let (startup_tx, startup_rx) = mpsc::channel::<Result<Vec<DictInfo>>>();

        // Do not join this thread. The bin exits while the thread waits in recv.
        thread::spawn(move || {
            worker_main(settings, open, wake, trigger_rx, result_tx, startup_tx);
        });

        let dicts: Vec<DictInfo> = startup_rx
            .recv()
            .context("worker thread ended before completing startup")??;

        Ok((Worker { trigger_tx, result_rx }, dicts))
    }

    /// Return the sender for triggers.
    pub fn trigger(&self) -> &mpsc::Sender<Trigger> {
        &self.trigger_tx
    }

    /// Return the receiver for results. The bin drains it after `wake`.
    pub fn results(&self) -> &mpsc::Receiver<WorkerResult> {
        &self.result_rx
    }

    /// Return a handle that wakes the Worker's `serve` hook.
    ///
    /// The handle is cheap to clone. The owner of a one-off OCR job holds one.
    pub fn serve_nudge(&self) -> ServeNudge {
        ServeNudge(self.trigger_tx.clone())
    }
}

/// Wake a Worker that has a `serve` job.
///
/// The Worker waits on its trigger channel. The job queue belongs to the bin.
/// Pixels in that queue do not wake the Worker. Queue the job first, then send
/// the nudge. The hook runs before the Worker waits again.
/// A nudge that a busy batch consumes still leaves a hook run.
#[derive(Clone)]
pub struct ServeNudge(mpsc::Sender<Trigger>);

impl ServeNudge {
    /// Wake the Worker.
    pub fn nudge(&self) {
        // If the Worker stopped, this call has no error to report.
        // The job result channel reports that state to the caller.
        let _ = self.0.send(Trigger { kind: TriggerKind::Serve, id: RequestId(0) });
    }

    /// Return a nudge for a caller that has no pipeline, such as a bin test.
    /// The caller can queue a job, but no Worker owns an engine to serve it.
    pub fn disconnected() -> Self {
        ServeNudge(mpsc::channel().0)
    }
}

/// Work that a drained batch completes before its newest hover.
/// The state includes settings and trigger-mode freeze state.
///
/// Keep state changes and sentence probes in arrival order. A reload and a press change state.
/// A sentence probe must not be coalesced.
enum Pre {
    Reload(WorkerSettings),
    Freeze(PhysPoint),
    Thaw,
    Sentence(RequestId, SentenceProbe),
}

/// Keep the newest hover, every state change, and every sentence probe.
///
/// State changes and sentence probes remain in arrival order. Hovers coalesce
/// because only the newest hover is useful.
fn drain(first: Trigger, rx: &mpsc::Receiver<Trigger>) -> (Option<Trigger>, Vec<Pre>) {
    let mut pre = Vec::new();
    let mut hover = None;
    let mut take = |t: Trigger| match t.kind {
        TriggerKind::Reload(s) => pre.push(Pre::Reload(*s)),
        TriggerKind::Freeze(at) => pre.push(Pre::Freeze(at)),
        TriggerKind::Thaw => pre.push(Pre::Thaw),
        TriggerKind::Sentence(probe) => pre.push(Pre::Sentence(t.id, probe)),
        // The wake already arrived.
        TriggerKind::Serve => {}
        _ => hover = Some(t),
    };
    take(first);
    while let Ok(next) = rx.try_recv() {
        take(next);
    }
    (hover, pre)
}

/// State that a lookup uses after a reload.
/// A `Reload` replaces all of this state except the OCR settings in
/// `TextSource` and the Dictionary handle.
struct LookupState {
    present_cfg: PresentConfig,
    scan_display: ScanDisplay,
    sentence_mode: SentenceMode,
    static_region: Option<PhysRect>,
    /// The Worker refreshes this list after each Reload.
    dicts: Vec<DictInfo>,
}

/// Apply one reload to the cache.
/// If a reopen callback exists, use it to inspect the Dictionary file again.
///
/// Without a reopen, `dicts` and the Dictionary handle become stale.
/// A bin that survives its rebuilds therefore supplies a reopen callback.
/// The reopened file supplies the final identities. The bin list contains
/// values from before the rebuild.
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
        // Keep the current handle. It still answers. A dropped handle answers nothing.
        Err(e) => eprintln!("chibipop: reopening the dictionary failed: {e:#}"),
    }
}

/// The Worker serves triggers and owns OCR.
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

    // Clone the list. An Arc adds no needed behavior.
    if startup_tx.send(Ok(state.dicts.clone())).is_err() {
        return; // The main thread no longer waits. Nothing remains to do.
    }

    // The sender dropped, so stop the Worker.
    loop {
        // Run jobs that the bin queued for the hook before the Worker waits.
        // A nudge that a busy batch consumes cannot leave its job in the queue.
        // An idle Worker with a hook waits. It does not poll
        // (ARCHITECTURE.md#hover-cadence).
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
                // Take the press-time grab: one full output before any popup exists
                // (ARCHITECTURE.md#hover-cadence). The source stores a failure,
                // so later lookups in the hold report it.
                Pre::Freeze(at) => {
                    if let Err(e) = source.freeze(at) {
                        eprintln!("chibipop: the trigger-press grab failed: {e:#}");
                    }
                }
                Pre::Thaw => source.thaw(),
                Pre::Sentence(id, probe) => {
                    let outcome = resolve_sentence(&mut source, probe);
                    if result_tx.send(WorkerResult { id, outcome }).is_err() {
                        return;
                    }
                    wake();
                }
            }
        }
        let Some(trigger) = hover else {
            continue;
        };

        // One bad frame does not stop the Worker.
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
                | TriggerKind::Sentence(_)
                | TriggerKind::Serve => {
                    LookupOutcome::Failed("a state change reached the hover path".to_string())
                }
            }
        }))
        .unwrap_or_else(|_| LookupOutcome::Failed("a hover lookup panicked".to_string()));

        if result_tx.send(WorkerResult { id: trigger.id, outcome }).is_err() {
            break; // The result receiver closed, so stop the Worker.
        }
        wake();
    }
}

/// Resolve one hover from OCR to the presentation.
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
        // No region exists. Continue with line mode.
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
        // `Static` reaches this point only when no region exists.
        // The `Sentence` mode reads the full sentence on add. This line is its fallback.
        SentenceMode::Line | SentenceMode::Static | SentenceMode::Sentence => {
            extract_sentence_line(&resolved.span.text, resolved.span.cursor_byte_offset).to_string()
        }
    };
    // The tiled path is the only path that draws an overlay.
    let outline = state.scan_display.highlight;
    present_lookup(dict, engine, state, &resolved, sentence, scan, outline)
}

/// Resolve one static region with one capture.
/// [`SentenceMode::Static`] makes the sentence contain all text in the
/// user-drawn box.
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
    // This path draws no capture boxes. It also draws no match outline.
    present_lookup(dict, engine, state, &resolved, || join_all_lines(&lines), Vec::new(), false)
}

/// Resolve one sentence probe for an Anki add.
///
/// The Controller keeps the hover-time sentence when this read fails or finds no anchor word.
fn resolve_sentence(source: &mut TextSource, probe: SentenceProbe) -> LookupOutcome {
    match source.read_sentence(probe.anchor, probe.orientation, probe.mask) {
        Ok(text) => LookupOutcome::Sentence(text),
        Err(e) => {
            eprintln!("chibipop: sentence probe failed, using the hovered line: {e:#}");
            LookupOutcome::Sentence(None)
        }
    }
}

/// Resolve the text under the cursor, build the presentation, and attach the
/// Anki sentence.
///
/// Add the match outline when the capture path requests it.
/// The sentence is a closure, so a miss does not build it.
/// `scan` contains the rects that the path already collected.
/// When `outline_match` is true, add the match rect last.
/// This order draws the match over the capture boxes.
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
    // `match_len` counts characters of the trimmed input, as `match_highlight` does.
    presentation.surface = presentation
        .top
        .as_ref()
        .map(|top| text.trim_start().chars().take(top.match_len).collect::<String>())
        .filter(|surface| !surface.is_empty());
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

/// Return the sentence that contains the cursor offset.
///
/// The function takes the OCR line at the cursor, then cuts that line to one
/// sentence with [`sentence::cut_sentence`]. A wide page holds several sentences
/// on one line. A card wants only the sentence of the hovered word.
fn extract_sentence_line(text: &str, cursor_offset: usize) -> &str {
    let mut pos = 0;
    for line in text.split('\n') {
        let end = pos + line.len();
        if cursor_offset >= pos && cursor_offset <= end {
            return sentence::cut_sentence(line, cursor_offset - pos);
        }
        pos = end + 1;
    }
    text
}

/// Join OCR lines with newline characters.
fn join_all_lines(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .map(|l| l.words.iter().map(|w| w.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run a Dictionary lookup without OCR.
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

    /// Include the line end.
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

    /// The reported case: one wide line held three sentences, and the card got all three.
    #[test]
    fn extract_sentence_line_cuts_a_wide_line_to_the_sentence_at_the_cursor() {
        let text = "日本のいろいろな場所で、雨がたくさん降りそうだと言っています。山が崩れたり、低い所に水が入ったりするかもしれません。気をつけてください。";
        let offset = text.find('山').unwrap();
        assert_eq!(
            "山が崩れたり、低い所に水が入ったりするかもしれません。",
            extract_sentence_line(text, offset)
        );
    }

    #[test]
    fn extract_sentence_line_cuts_only_the_line_that_holds_the_cursor() {
        let text = "前の行。\n一つ目。二つ目の文。\n次の行。";
        let offset = text.find('二').unwrap();
        assert_eq!("二つ目の文。", extract_sentence_line(text, offset));
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

    /// Keep the newest hover and each state change in arrival order.
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

    #[test]
    fn drain_keeps_a_sentence_between_hovers_and_the_newest_hover() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        let first_at = PhysPoint { x: 1, y: 1 };
        let second_at = PhysPoint { x: 9, y: 9 };
        tx.send(Trigger {
            kind: TriggerKind::Sentence(SentenceProbe {
                anchor: PhysRect { x: 20, y: 30, w: 40, h: 40 },
                orientation: Orientation::Horizontal,
                mask: CaptureMask::NONE,
            }),
            id: RequestId(2),
        })
        .unwrap();
        tx.send(Trigger {
            kind: TriggerKind::Hover(Hover { at: second_at, mask: CaptureMask::NONE }),
            id: RequestId(3),
        })
        .unwrap();
        let first = Trigger {
            kind: TriggerKind::Hover(Hover { at: first_at, mask: CaptureMask::NONE }),
            id: RequestId(1),
        };

        let (hover, pre) = drain(first, &rx);

        assert!(matches!(hover.map(|t| t.kind), Some(TriggerKind::Hover(h)) if h.at == second_at));
        assert!(matches!(
            pre.as_slice(),
            [Pre::Sentence(id, probe)]
                if *id == RequestId(2)
                    && probe.anchor == (PhysRect { x: 20, y: 30, w: 40, h: 40 })
                    && probe.orientation == Orientation::Horizontal
                    && probe.mask == CaptureMask::NONE
        ));
    }

    /// Treat `Freeze` and `Thaw` as state changes, not as lookups.
    /// Keep `Freeze` and `Thaw` in arrival order.
    /// Select the newest hover separately.
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

    /// A reload without a hover still reaches the Worker.
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

    /// Return one Dictionary with the name that the test supplies.
    fn one_dict(name: &str) -> Box<dyn Dictionary> {
        let mut d = crate::lookup::model::FakeDictionary::new();
        d.add_dict(7, name);
        Box::new(d)
    }

    /// Create a cache with these identities and no other reload state.
    ///
    /// Resolve the scope against these identities.
    /// A config with no Dictionary names enables every Dictionary that it finds.
    fn state_with(dicts: Vec<DictInfo>) -> LookupState {
        LookupState {
            present_cfg: Config::default().present_config(&dicts),
            scan_display: ScanDisplay { captures: false, highlight: false },
            sentence_mode: SentenceMode::Line,
            static_region: None,
            dicts,
        }
    }

    /// Keep the id and replace the Dictionary name.
    #[test]
    fn a_reload_replaces_the_cached_dictionary_identities() {
        let mut state = state_with(vec![di(7, "Removed")]);
        let mut dict = one_dict("Removed");
        let mut s = ws(2);
        s.dicts = vec![di(7, "Added")];

        take_reload(s, None, &mut dict, &mut state);

        assert_eq!(vec![di(7, "Added")], state.dicts, "the removed name must not answer");
    }

    /// A rebuild renames a new database over the old inode.
    /// Only a reopen then serves the new file.
    /// The reopened file supplies its identities because the bin knows only
    /// the identities from before the rebuild.
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

    /// If a reopen fails, keep the open handle.
    /// An old Dictionary still answers lookups. A dropped handle answers nothing.
    #[test]
    fn a_failed_reopen_keeps_the_dictionary_already_open() {
        let mut state = state_with(vec![di(7, "StillHere")]);
        let mut dict = one_dict("StillHere");
        let reopen: ReopenDict = Box::new(|| anyhow::bail!("the database is a directory"));

        take_reload(ws(2), Some(&reopen), &mut dict, &mut state);

        assert_eq!(vec![di(7, "StillHere")], dict.dicts().unwrap());
    }

    /// Return a Dictionary with an entry for 食.
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

    /// Return one word under the cursor with geometry for a match outline.
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

    /// Test the shared tail for a path that draws an overlay.
    /// It keeps the sentence and adds the match rect after the capture rects.
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
        assert_eq!(Some("食".to_string()), presentation.surface, "the on-screen form the card matched");
        assert!(matched.is_some(), "a hit with geometry has a rect to outline");
        assert_eq!(
            vec![ScanKind::Pass1, ScanKind::Match],
            scan.iter().map(|r| r.kind).collect::<Vec<_>>(),
            "the match draws last, over the capture boxes"
        );
    }

    /// Test the shared tail for a path that draws no overlay.
    /// It still returns the rect that the popup uses for its highlight.
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

    /// A Dictionary miss hides the popup.
    /// A miss also does not build the sentence.
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
