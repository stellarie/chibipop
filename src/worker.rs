//! The core-owned background pipeline: capture -> OCR -> lookup -> present
//! (ADR-0001). Fed `Trigger`s, yields `WorkerResult`s over plain mpsc
//! channels; the platform bin supplies the two seams and a wake callback,
//! and drives everything else from its own event loop.

use crate::controller::{LookupOutcome, RequestId};
use crate::geom::{PhysPoint, ScanDisplay, ScanKind, ScanRect};
use crate::lookup::engine::LookupEngine;
use crate::lookup::model::Dictionary;
use crate::present::{self, DictInfo, PresentConfig};
use crate::text::layout::CaptureSize;
use crate::text::{OcrEngine, RegionCapture, SettingsSnapshot, TextSource};
use anyhow::{Context, Result};
use std::sync::mpsc;
use std::thread;

/// Hover, drill-down, reload.
pub enum TriggerKind {
    Hover(PhysPoint),
    DrillDown(String),
    Reload(Box<WorkerSettings>),
}

/// What the worker owns.
pub struct WorkerSettings {
    pub max_passes: u8,
    pub prefer_vertical: bool,
    pub capture: CaptureSize,
    pub scan_alphanumeric: bool,
    pub language: String,
    pub present_cfg: PresentConfig,
    pub scan_display: ScanDisplay,
    /// Refreshed by every edit.
    pub dicts: Vec<DictInfo>,
}

impl WorkerSettings {
    /// The OCR half, for the facade.
    fn snapshot(&self) -> SettingsSnapshot {
        SettingsSnapshot {
            max_passes: self.max_passes,
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

/// What the bin supplies, built on the worker thread.
///
/// Built there because backends may be thread-affine (COM apartments,
/// per-thread caches); the `open` closure runs after the thread exists,
/// so nothing here needs to be `Send`.
pub struct WorkerParts {
    pub capture: Box<dyn RegionCapture>,
    pub ocr: Box<dyn OcrEngine>,
    pub dict: Box<dyn Dictionary>,
    pub engine: LookupEngine,
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

/// One reload into the cache.
///
/// dicts goes stale otherwise.
fn take_reload(
    s: WorkerSettings,
    present_cfg: &mut PresentConfig,
    scan_display: &mut ScanDisplay,
    dicts: &mut Vec<DictInfo>,
) {
    *present_cfg = s.present_cfg;
    *scan_display = s.scan_display;
    *dicts = s.dicts;
}

/// Newest hover; all reloads.
fn drain(first: Trigger, rx: &mpsc::Receiver<Trigger>) -> (Option<Trigger>, Vec<WorkerSettings>) {
    let mut reloads = Vec::new();
    let mut hover = None;
    let mut take = |t: Trigger| match t.kind {
        TriggerKind::Reload(s) => reloads.push(*s),
        _ => hover = Some(t),
    };
    take(first);
    while let Ok(next) = rx.try_recv() {
        take(next);
    }
    (hover, reloads)
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
    let WorkerParts { capture, ocr, dict, engine } = match open() {
        Ok(p) => p,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };

    // Refreshed by every Reload.
    let mut dicts: Vec<DictInfo> = match dict.dicts().context("reading dictionary identities") {
        Ok(d) => d,
        Err(e) => {
            let _ = startup_tx.send(Err(e));
            return;
        }
    };

    let mut source = TextSource::new(capture, ocr, settings.snapshot());
    let mut present_cfg = settings.present_cfg;
    let mut scan_display = settings.scan_display;

    // An Arc would be ceremony.
    if startup_tx.send(Ok(dicts.clone())).is_err() {
        return; // main thread gave up waiting; nothing left to do.
    }

    // Sender dropped: shutdown.
    while let Ok(first) = trigger_rx.recv() {
        let (hover, reloads) = drain(first, &trigger_rx);
        for s in reloads {
            source.apply_settings(s.snapshot(), &s.language);
            take_reload(s, &mut present_cfg, &mut scan_display, &mut dicts);
        }
        let Some(trigger) = hover else {
            continue;
        };

        // One bad frame is not fatal.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match &trigger.kind {
                TriggerKind::Hover(cursor) => resolve_trigger(
                    &mut source,
                    dict.as_ref(),
                    &engine,
                    &dicts,
                    &present_cfg,
                    *cursor,
                    scan_display,
                ),
                TriggerKind::DrillDown(text) => resolve_drilldown(
                    dict.as_ref(),
                    &engine,
                    &dicts,
                    &present_cfg,
                    text,
                ),
                TriggerKind::Reload(_) => {
                    LookupOutcome::Failed("a reload reached the hover path".to_string())
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
    dicts: &[DictInfo],
    present_cfg: &PresentConfig,
    cursor: PhysPoint,
    scan_display: ScanDisplay,
) -> LookupOutcome {
    let raw = source.resolve_at_tiled_scanned(cursor, scan_display.captures);
    let (resolved, mut scan) = match raw {
        Ok((Some(r), scan)) => (r, scan),
        Ok((None, _)) => return LookupOutcome::Hide,
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

    let presentation = present::build(&hits, dicts, present_cfg);
    let matched = present::match_highlight(&resolved.span, presentation.top.as_ref());
    if scan_display.highlight {
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
            prefer_vertical: false,
            capture: CaptureSize::default(),
            scan_alphanumeric: true,
            language: "ja".to_string(),
            present_cfg: Config::default().present_config(),
            scan_display: ScanDisplay { captures: false, highlight: false },
            dicts: Vec::new(),
        }
    }

    /// Newest hover; every reload.
    #[test]
    fn drain_keeps_the_newest_hover_and_every_reload() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        let reload = TriggerKind::Reload(Box::new(ws(2)));
        tx.send(Trigger { kind: reload, id: RequestId(2) }).unwrap();
        let newer = TriggerKind::Hover(PhysPoint { x: 9, y: 9 });
        tx.send(Trigger { kind: newer, id: RequestId(3) }).unwrap();
        let second = TriggerKind::Reload(Box::new(ws(4)));
        tx.send(Trigger { kind: second, id: RequestId(4) }).unwrap();
        let older = TriggerKind::Hover(PhysPoint { x: 1, y: 1 });
        let first = Trigger { kind: older, id: RequestId(1) };
        let (hover, reloads) = drain(first, &rx);
        let hover = hover.expect("a hover survives");
        assert!(matches!(hover.kind, TriggerKind::Hover(p) if p.x == 9), "newest hover wins");
        assert_eq!(2, reloads.len(), "neither reload may be swallowed");
        let passes: Vec<u8> = reloads.iter().map(|r| r.max_passes).collect();
        assert_eq!(vec![2, 4], passes, "reloads keep the order they were sent");
    }

    /// A reload alone still arrives.
    #[test]
    fn drain_returns_no_hover_when_only_a_reload_queued() {
        let (tx, rx) = mpsc::channel::<Trigger>();
        drop(tx);
        let first = Trigger { kind: TriggerKind::Reload(Box::new(ws(3))), id: RequestId(1) };
        let (hover, reloads) = drain(first, &rx);
        assert!(hover.is_none());
        assert_eq!(1, reloads.len());
        assert_eq!(3, reloads[0].max_passes);
    }

    fn di(id: i64, name: &str) -> DictInfo {
        DictInfo { dict_id: id, name: name.to_string() }
    }

    /// Same id, new dictionary.
    #[test]
    fn a_reload_replaces_the_cached_dictionary_identities() {
        let mut present_cfg = Config::default().present_config();
        let mut scan_display = ScanDisplay { captures: false, highlight: false };
        let mut dicts = vec![di(7, "Removed")];
        let mut s = ws(2);
        s.dicts = vec![di(7, "Added")];

        take_reload(s, &mut present_cfg, &mut scan_display, &mut dicts);

        assert_eq!(vec![di(7, "Added")], dicts, "the removed name must not answer");
    }
}
