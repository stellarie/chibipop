//! The core-owned background pipeline: capture -> OCR -> lookup -> present
//! (ADR-0001). Fed `Trigger`s, yields `WorkerResult`s over plain mpsc
//! channels; the platform bin supplies the two seams and a wake callback,
//! and drives everything else from its own event loop.

use crate::controller::{LookupOutcome, RequestId};
use crate::geom::{PhysPoint, PhysRect, ScanDisplay, ScanKind, ScanRect};
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::present::{self, DictInfo, PresentConfig};
use crate::text::layout::{CaptureSize, OcrLine};
use crate::text::mask::CaptureMask;
use crate::text::{OcrEngine, RegionCapture, SettingsSnapshot, TextSource};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// How often a worker with a `serve` hook wakes to look for one-off
/// jobs while no trigger arrives (upstream's OCR_REQUEST_POLL).
const SERVE_POLL: Duration = Duration::from_millis(20);

/// One hover: where the cursor is, and what its grab must not read.
#[derive(Clone, Copy)]
pub struct Hover {
    pub at: PhysPoint,
    /// Our own popup where the platform cannot exclude it (ADR-0008).
    /// `CaptureMask::NONE` on a frozen grab, which predates the popup,
    /// and on platforms that exclude the surface themselves.
    pub mask: CaptureMask,
}

/// Hover, drill-down, reload, and trigger mode's freeze.
pub enum TriggerKind {
    Hover(Hover),
    DrillDown(String),
    Reload(Box<WorkerSettings>),
    /// Trigger press: take one full grab of the output holding this
    /// point and read every lookup out of it until [`TriggerKind::Thaw`]
    /// (ADR-0010). Sent again mid-hold when the cursor crosses onto
    /// another output, which is what makes the second monitor live.
    ///
    /// Answers nothing: it is state, not a lookup. A grab that fails is
    /// reported by the lookups that follow it, which is where a user
    /// can see it.
    Freeze(PhysPoint),
    /// Trigger release: drop the frozen frame, grabs go live again.
    Thaw,
}

/// What the worker owns.
pub struct WorkerSettings {
    pub max_passes: u8,
    /// Per-platform capture upscale (Windows 2, Linux 1 - ADR-0009).
    pub upscale: i32,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
    pub language: String,
    pub present_cfg: PresentConfig,
    pub scan_display: ScanDisplay,
    /// `"line"`, `"all"`, or `"static"` - how the Anki sentence field
    /// is assembled (upstream 0.9.x sentence capture).
    pub sentence_mode: String,
    /// The user-drawn box `sentence_mode == "static"` reads from.
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

/// A between-lookups job runner, lent the thread-affine OCR engine
/// (see `WorkerParts::serve`).
pub type ServeHook = Box<dyn FnMut(&dyn OcrEngine)>;

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
    /// Lends the OCR engine out between lookups, polled every
    /// [`SERVE_POLL`]: a one-off OCR job (OCR-to-clipboard) must run on
    /// this thread because engines are thread-affine. `None` costs
    /// nothing - the worker blocks on its trigger channel as before.
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
    sentence_mode: String,
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
        // With a `serve` hook the wait is a poll, so a one-off OCR job
        // queued while the cursor is still never waits on a hover.
        let first = match &mut serve {
            Some(hook) => match trigger_rx.recv_timeout(SERVE_POLL) {
                Ok(first) => first,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    hook(source.engine());
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            },
            None => match trigger_rx.recv() {
                Ok(first) => first,
                Err(_) => break,
            },
        };
        if let Some(hook) = &mut serve {
            hook(source.engine());
        }
        let (hover, pre) = drain(first, &trigger_rx);
        for change in pre {
            match change {
                Pre::Reload(s) => {
                    source.apply_settings(s.snapshot(), &s.language);
                    take_reload(s, reopen_dict.as_ref(), &mut dict, &mut state);
                }
                // The press-time grab: one full output, before any
                // popup exists (ADR-0010). A failure is remembered by
                // the source, so the hold's lookups report it.
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
                TriggerKind::Reload(_) | TriggerKind::Freeze(_) | TriggerKind::Thaw => {
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
    if state.sentence_mode == "static" {
        if let Some(region) = state.static_region {
            return resolve_static(source, dict, engine, state, hover, region);
        }
        // No region yet; fall through.
        eprintln!("chibipop: static mode but no region set; using line mode");
    }
    let raw = source.resolve_at_tiled_scanned(hover.at, state.scan_display.captures, hover.mask);
    let (resolved, mut scan, ocr_lines) = match raw {
        Ok((Some(r), scan, lines)) => (r, scan, lines),
        Ok((None, _, _)) => return LookupOutcome::Hide,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };

    let text = &resolved.span.text[resolved.span.cursor_byte_offset..];
    let hits = match engine.run(dict, text) {
        Ok(h) => h,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };
    if hits.is_empty() {
        return LookupOutcome::Hide;
    }

    let mut presentation = present::build(&hits, &state.dicts, &state.present_cfg);
    let sentence = match state.sentence_mode.as_str() {
        "all" => join_all_lines(&ocr_lines),
        _ => extract_sentence_line(&resolved.span.text, resolved.span.cursor_byte_offset)
            .to_string(),
    };
    presentation.sentence = Some(sentence);
    let matched = present::match_highlight(&resolved.span, presentation.top.as_ref());
    if state.scan_display.highlight {
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

/// Static-region capture path (`sentence_mode == "static"`): one read
/// of the user-drawn box, sentence = everything the box holds.
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

    let text = &resolved.span.text[resolved.span.cursor_byte_offset..];
    let hits = match engine.run(dict, text) {
        Ok(h) => h,
        Err(e) => return LookupOutcome::Failed(format!("{e:#}")),
    };
    if hits.is_empty() {
        return LookupOutcome::Hide;
    }

    let mut presentation = present::build(&hits, &state.dicts, &state.present_cfg);
    presentation.sentence = Some(join_all_lines(&read.lines));
    let matched = present::match_highlight(&resolved.span, presentation.top.as_ref());
    LookupOutcome::Ready {
        presentation: Box::new(presentation),
        anchor: resolved.span.anchor,
        orientation: resolved.orientation,
        matched,
        scan: Vec::new(),
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
    let p = present::build(&hits, dicts, present_cfg);
    LookupOutcome::DrillDown(Box::new(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn ws(passes: u8) -> WorkerSettings {
        WorkerSettings {
            max_passes: passes,
            upscale: 2,
            prefer_vertical: false,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
            language: "ja".to_string(),
            present_cfg: Config::default().present_config(),
            scan_display: ScanDisplay { captures: false, highlight: false },
            sentence_mode: "line".to_string(),
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
    fn state_with(dicts: Vec<DictInfo>) -> LookupState {
        LookupState {
            present_cfg: Config::default().present_config(),
            scan_display: ScanDisplay { captures: false, highlight: false },
            sentence_mode: "line".to_string(),
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
}
