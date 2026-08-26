//! Worker-level tests over fake `RegionCapture`/`OcrEngine` backends: the
//! trigger->result flow, latest-wins coalescing, and reload semantics that
//! the platform bins rely on, with no OS in the loop.

use chibipop::controller::{LookupOutcome, RequestId};
use chibipop::geom::{PhysPoint, PhysRect, ScanDisplay};
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::{FakeDictionary, Sense};
use chibipop::present::DictInfo;
use chibipop::text::layout::{CaptureSize, OcrLine, OcrWord};
use chibipop::text::mask::{CaptureMask, CaptureMode};
use chibipop::text::{Frame, OcrEngine, RegionCapture};
use chibipop::worker::{Hover, Trigger, TriggerKind, Worker, WorkerParts, WorkerSettings};
use std::sync::mpsc;
use std::time::Duration;

/// Generous: never reached on a healthy run.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Everything already logged. Every event is sent before the result that
/// follows it, so a drained snapshot after a received result is complete.
fn events(log_rx: &mpsc::Receiver<String>) -> Vec<String> {
    log_rx.try_iter().collect()
}

/// Canned frames; optionally gated so a test can hold a grab open.
struct FakeCapture {
    log: mpsc::Sender<String>,
    /// One token per grab.
    gate: Option<mpsc::Receiver<()>>,
    /// Signalled when a grab starts.
    entered_tx: Option<mpsc::Sender<()>>,
}

impl RegionCapture for FakeCapture {
    fn grab(&mut self, region: PhysRect) -> anyhow::Result<Frame> {
        let _ = self.log.send("grab".to_string());
        if let Some(tx) = &self.entered_tx {
            let _ = tx.send(());
        }
        if let Some(gate) = &self.gate {
            gate.recv_timeout(TIMEOUT).expect("the test must release the gated grab");
        }
        Ok(Frame {
            buf: vec![0u8; (region.w * region.h * 4) as usize],
            w: region.w,
            h: region.h,
            source: "fake",
            fallback: None,
            unchanged: false,
        })
    }

    fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
        PhysRect { x: p.x - 2000, y: p.y - 2000, w: 4000, h: 4000 }
    }

    fn begin_read(&mut self) {
        let _ = self.log.send("begin_read".to_string());
    }

    fn end_read(&mut self) {
        let _ = self.log.send("end_read".to_string());
    }
}

/// One whole-image word per call, or nothing.
struct FakeOcr {
    log: mpsc::Sender<String>,
    text: Option<String>,
    panics: bool,
}

impl OcrEngine for FakeOcr {
    fn recognise(&self, _bgra: &[u8], w: i32, h: i32) -> anyhow::Result<Vec<OcrLine>> {
        let _ = self.log.send("ocr".to_string());
        if self.panics {
            panic!("a deliberate OCR panic");
        }
        Ok(match &self.text {
            None => Vec::new(),
            Some(t) => vec![OcrLine {
                words: vec![OcrWord {
                    text: t.clone(),
                    rect: PhysRect { x: 0, y: 0, w, h },
                }],
            }],
        })
    }

    fn set_language(&mut self, tag: &str) {
        let _ = self.log.send(format!("set_language {tag}"));
    }

    fn name(&self) -> &str {
        "fake-ocr"
    }

    fn provides_geometry(&self) -> bool {
        true
    }
}

fn senses() -> Vec<Sense> {
    serde_json::from_str(r#"[{"glosses":["to eat"],"pos":[],"misc":[]}]"#).unwrap()
}

/// One term, one entry, one dictionary.
fn dict() -> FakeDictionary {
    dict_named("FakeDict")
}

/// The same, under another dictionary name.
fn dict_named(name: &str) -> FakeDictionary {
    let mut d = FakeDictionary::new();
    d.add_dict(1, name);
    d.add_term("食", None, None, "", None, 10, 1);
    d.add_entry(10, 1, senses());
    d
}

fn settings() -> WorkerSettings {
    WorkerSettings {
        max_passes: 1,
        upscale: 2,
        prefer_vertical: false,
        capture: CaptureSize::default(),
        scan_alphanumeric: true,
        language: "ja".to_string(),
        present_cfg: chibipop::config::Config::default().present_config(),
        scan_display: ScanDisplay { captures: false, highlight: false },
        sentence_mode: chibipop::config::SentenceMode::Line,
        static_region: None,
        dicts: Vec::new(),
    }
}

/// A worker over the fakes; `text` is what OCR "reads" everywhere.
fn spawn(
    text: Option<&str>,
    panics: bool,
    gate: Option<mpsc::Receiver<()>>,
    entered_tx: Option<mpsc::Sender<()>>,
) -> (Worker, Vec<DictInfo>, mpsc::Receiver<String>) {
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let capture_log = log_tx.clone();
    let text = text.map(str::to_string);
    let (worker, dicts) = Worker::spawn(
        settings(),
        move || {
            Ok(WorkerParts {
                capture: Box::new(FakeCapture { log: capture_log, gate, entered_tx }),
                ocr: Box::new(FakeOcr { log: log_tx, text, panics }),
                dict: Box::new(dict()),
                reopen_dict: None,
                serve: None,
                engine: LookupEngine::new(Deconjugator::new(Vec::new())),
            })
        },
        || {},
    )
    .expect("the worker must start over healthy fakes");
    (worker, dicts, log_rx)
}

/// Where the fakes' hovers land.
const AT: PhysPoint = PhysPoint { x: 600, y: 300 };

fn hover(id: u64) -> Trigger {
    Trigger {
        kind: TriggerKind::Hover(Hover { at: AT, mask: CaptureMask::NONE }),
        id: RequestId(id),
    }
}

/// A hover with our own popup over the hovered point.
fn masked_hover(id: u64, mode: CaptureMode) -> Trigger {
    let popup = PhysRect { x: AT.x - 50, y: AT.y - 50, w: 100, h: 100 };
    Trigger {
        kind: TriggerKind::Hover(Hover { at: AT, mask: CaptureMask::for_mode(mode, Some(popup)) }),
        id: RequestId(id),
    }
}

/// The mask boundary is a capture edge: `FakeOcr`'s one word spans the
/// whole grab, so a popup anywhere in it takes the word with it rather
/// than leaving half a glyph to look up (ADR-0008).
#[test]
fn a_live_hover_under_our_own_popup_resolves_nothing() {
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, None, None);
    worker.trigger().send(masked_hover(7, CaptureMode::Live)).unwrap();
    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(7), result.id);
    assert!(
        matches!(result.outcome, LookupOutcome::Hide),
        "a word touching the mask must be dropped, not half-recognised"
    );
    assert_eq!(
        vec!["begin_read", "grab", "ocr", "end_read"],
        events(&log_rx),
        "masking is arithmetic on the grabbed pixels: no extra pass"
    );
}

/// A frozen grab predates the popup, so the same rect masks nothing.
#[test]
fn a_frozen_hover_is_maskless_and_still_resolves() {
    let (worker, _dicts, _log_rx) = spawn(Some("食"), false, None, None);
    worker.trigger().send(masked_hover(8, CaptureMode::Frozen)).unwrap();
    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(8), result.id);
    assert!(
        matches!(result.outcome, LookupOutcome::Ready { .. }),
        "trigger mode captures before the popup exists; nothing to mask"
    );
}

/// A backend that answers "unchanged" after its first grab: what a
/// damage-paced dwell looks like from above the seam (ADR-0002).
struct DwellingCapture {
    log: mpsc::Sender<String>,
    grabs: u32,
}

impl RegionCapture for DwellingCapture {
    fn grab(&mut self, region: PhysRect) -> anyhow::Result<Frame> {
        self.grabs += 1;
        let _ = self.log.send("grab".to_string());
        Ok(Frame {
            buf: vec![0u8; (region.w * region.h * 4) as usize],
            w: region.w,
            h: region.h,
            source: "fake",
            fallback: None,
            unchanged: self.grabs > 1,
        })
    }

    fn bounds_containing(&self, p: PhysPoint) -> PhysRect {
        PhysRect { x: p.x - 2000, y: p.y - 2000, w: 4000, h: 4000 }
    }

    fn begin_read(&mut self) {
        let _ = self.log.send("begin_read".to_string());
    }

    fn end_read(&mut self) {
        let _ = self.log.send("end_read".to_string());
    }
}

/// A worker over that backend; OCR reads the one word as ever.
fn spawn_dwelling() -> (Worker, mpsc::Receiver<String>) {
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let capture_log = log_tx.clone();
    let (worker, _dicts) = Worker::spawn(
        settings(),
        move || {
            Ok(WorkerParts {
                capture: Box::new(DwellingCapture { log: capture_log, grabs: 0 }),
                ocr: Box::new(FakeOcr {
                    log: log_tx,
                    text: Some("\u{98DF}".to_string()),
                    panics: false,
                }),
                dict: Box::new(dict()),
                reopen_dict: None,
                serve: None,
                engine: LookupEngine::new(Deconjugator::new(Vec::new())),
            })
        },
        || {},
    )
    .expect("the worker must start over healthy fakes");
    (worker, log_rx)
}

/// ADR-0010's dwell re-check, at the cost that makes it viable.
///
/// A second look at a still screen reuses the first read's words: same
/// pixels, same mask, same answer, no OCR pass. The popup coming up
/// between two looks *is* a new question - the pixels OCR reads are the
/// grab after masking (ADR-0008) - so that one look pays a pass, and
/// every dwell behind it is free again.
#[test]
fn a_dwell_on_unchanged_pixels_skips_the_ocr_pass() {
    let (worker, log_rx) = spawn_dwelling();
    let mut seen: Vec<String> = Vec::new();
    let count = |seen: &[String], what: &str| seen.iter().filter(|e| *e == what).count();
    let passes = |seen: &[String]| (count(seen, "grab"), count(seen, "ocr"));

    worker.trigger().send(hover(1)).unwrap();
    let first = answer(&worker);
    assert!(matches!(first.outcome, LookupOutcome::Ready { .. }));
    seen.extend(events(&log_rx));
    assert_eq!((1, 1), passes(&seen), "the first look reads the screen");

    worker.trigger().send(hover(2)).unwrap();
    answer(&worker);
    seen.extend(events(&log_rx));
    assert_eq!((2, 1), passes(&seen), "an unchanged dwell reuses the words it has");

    worker.trigger().send(masked_hover(3, CaptureMode::Live)).unwrap();
    answer(&worker);
    seen.extend(events(&log_rx));
    assert_eq!((3, 2), passes(&seen), "our own popup appearing is a new question");

    worker.trigger().send(masked_hover(4, CaptureMode::Live)).unwrap();
    answer(&worker);
    seen.extend(events(&log_rx));
    assert_eq!((4, 2), passes(&seen), "and every dwell behind it is free again");
}

/// The whole pipeline: trigger in, presented lookup out, read bracketed.
#[test]
fn a_hover_trigger_yields_a_ready_result() {
    let (worker, dicts, log_rx) = spawn(Some("食"), false, None, None);
    assert_eq!(1, dicts.len(), "startup must report the dictionary identities");
    assert_eq!("FakeDict", dicts[0].name);

    worker.trigger().send(hover(1)).unwrap();
    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(1), result.id);
    let LookupOutcome::Ready { presentation, scan, .. } = result.outcome else {
        panic!("expected Ready, got something else");
    };
    let top = presentation.top.expect("the hit must present a top card");
    assert_eq!("FakeDict", top.blocks[0].dict_name);
    assert!(scan.is_empty(), "scan rects are debug-only and were not requested");

    // One read: guard bracket around capture and OCR.
    assert_eq!(vec!["begin_read", "grab", "ocr", "end_read"], events(&log_rx));
}

#[test]
fn nothing_recognised_hides_the_popup() {
    let (worker, _dicts, _log_rx) = spawn(None, false, None, None);
    worker.trigger().send(hover(2)).unwrap();
    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(2), result.id);
    assert!(matches!(result.outcome, LookupOutcome::Hide));
}

/// Drill-down is dictionary-only.
#[test]
fn a_drilldown_never_touches_the_screen() {
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, None, None);
    worker
        .trigger()
        .send(Trigger { kind: TriggerKind::DrillDown("食".to_string()), id: RequestId(3) })
        .unwrap();
    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(3), result.id);
    assert!(matches!(result.outcome, LookupOutcome::DrillDown(_)));
    let seen = events(&log_rx);
    assert!(seen.is_empty(), "no capture, no OCR: {seen:?}");
}

/// One bad frame is not fatal.
#[test]
fn a_panicking_backend_fails_the_hover_and_the_worker_survives() {
    let (worker, _dicts, _log_rx) = spawn(Some("食"), true, None, None);
    worker.trigger().send(hover(4)).unwrap();
    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(4), result.id);
    let LookupOutcome::Failed(why) = result.outcome else {
        panic!("expected Failed");
    };
    assert_eq!("a hover lookup panicked", why);

    // Still serving: a screen-free lookup answers afterwards.
    worker
        .trigger()
        .send(Trigger { kind: TriggerKind::DrillDown("食".to_string()), id: RequestId(5) })
        .unwrap();
    let next = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(5), next.id);
    assert!(matches!(next.outcome, LookupOutcome::DrillDown(_)));
}

/// Latest-wins: hovers queued behind an in-flight read coalesce to the
/// newest; the stale one is never answered.
#[test]
fn queued_hovers_coalesce_to_the_newest() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, Some(gate_rx), Some(entered_tx));

    worker.trigger().send(hover(1)).unwrap();
    entered_rx.recv_timeout(TIMEOUT).expect("the first grab must start");

    // Both arrive while the first read is held open.
    worker.trigger().send(hover(2)).unwrap();
    worker.trigger().send(hover(3)).unwrap();
    gate_tx.send(()).unwrap();

    let first = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(1), first.id);

    entered_rx.recv_timeout(TIMEOUT).expect("the coalesced grab must start");
    gate_tx.send(()).unwrap();
    let second = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(3), second.id, "the newest queued hover wins");

    assert!(worker.results().try_recv().is_err(), "the stale hover must never answer");
    let grabs = events(&log_rx).iter().filter(|e| *e == "grab").count();
    assert_eq!(2, grabs, "the dropped hover must not have captured");
}

/// A reload is consumed before the next hover: new dictionary identities,
/// new language, new scan settings; the reload itself never answers.
#[test]
fn a_reload_is_applied_before_the_next_hover() {
    let (worker, dicts, log_rx) = spawn(Some("食"), false, None, None);
    assert_eq!("FakeDict", dicts[0].name);

    let mut reloaded = settings();
    reloaded.language = "ko".to_string();
    reloaded.scan_display = ScanDisplay { captures: true, highlight: false };
    reloaded.dicts = vec![DictInfo { dict_id: 1, name: "Renamed".to_string() }];
    worker
        .trigger()
        .send(Trigger { kind: TriggerKind::Reload(Box::new(reloaded)), id: RequestId(100) })
        .unwrap();
    worker.trigger().send(hover(101)).unwrap();

    let result = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(101), result.id, "a reload never answers on its own");
    let LookupOutcome::Ready { presentation, scan, .. } = result.outcome else {
        panic!("expected Ready after the reload");
    };
    let top = presentation.top.expect("the hit must still present");
    assert_eq!("Renamed", top.blocks[0].dict_name, "identities must come from the reload");
    assert!(!scan.is_empty(), "the reloaded scan_display asked for capture rects");
    let seen = events(&log_rx);
    assert!(
        seen.contains(&"set_language ko".to_string()),
        "the reload must reach the OCR backend: {seen:?}"
    );
}

/// Startup failure surfaces at spawn, not as a dead thread.
#[test]
fn a_failing_open_fails_the_spawn() {
    let Err(err) = Worker::spawn(settings(), || anyhow::bail!("no backend"), || {}) else {
        panic!("spawn must propagate the open failure");
    };
    assert!(format!("{err:#}").contains("no backend"), "{err:#}");
}

/// The bin's event loop is woken for every queued result.
#[test]
fn wake_fires_after_each_result() {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let (log_tx, _log_rx) = mpsc::channel::<String>();
    let capture_log = log_tx.clone();
    let (worker, _dicts) = Worker::spawn(
        settings(),
        move || {
            Ok(WorkerParts {
                capture: Box::new(FakeCapture { log: capture_log, gate: None, entered_tx: None }),
                ocr: Box::new(FakeOcr {
                    log: log_tx,
                    text: Some("食".to_string()),
                    panics: false,
                }),
                dict: Box::new(dict()),
                reopen_dict: None,
                serve: None,
                engine: LookupEngine::new(Deconjugator::new(Vec::new())),
            })
        },
        move || {
            let _ = wake_tx.send(());
        },
    )
    .unwrap();

    worker.trigger().send(hover(7)).unwrap();
    wake_rx.recv_timeout(TIMEOUT).expect("a result must wake the event loop");
    assert!(worker.results().try_recv().is_ok(), "the result precedes its wake");
}

// -- trigger mode's hold, end to end through the Worker (ADR-0010) --

/// A hover somewhere else, so the hold is asked for a second box.
fn hover_at(id: u64, at: PhysPoint) -> Trigger {
    Trigger {
        kind: TriggerKind::Hover(Hover { at, mask: CaptureMask::NONE }),
        id: RequestId(id),
    }
}

fn freeze(id: u64, at: PhysPoint) -> Trigger {
    Trigger { kind: TriggerKind::Freeze(at), id: RequestId(id) }
}

/// One answer, or a test failure.
fn answer(worker: &Worker) -> chibipop::worker::WorkerResult {
    worker.results().recv_timeout(TIMEOUT).expect("the worker must answer")
}

/// The whole point of freezing: one copy at press, and every lookup in
/// the hold reads it - however many lookups the cursor asks for.
#[test]
fn a_trigger_hold_copies_once_and_serves_every_lookup_from_that_copy() {
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, None, None);
    worker.trigger().send(freeze(1, AT)).unwrap();
    worker.trigger().send(hover(2)).unwrap();
    assert!(matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }));

    // The cursor moved: another box, out of the same frame.
    worker.trigger().send(hover_at(3, PhysPoint { x: AT.x + 120, y: AT.y })).unwrap();
    assert!(matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }));

    let seen = events(&log_rx);
    assert_eq!(
        1,
        seen.iter().filter(|e| *e == "grab").count(),
        "a hold is one copy, no matter how many lookups: {seen:?}"
    );
    assert_eq!(
        vec!["begin_read", "grab", "end_read"],
        seen.iter().filter(|e| *e != "ocr").cloned().collect::<Vec<_>>(),
        "only the press-time grab brackets a read: {seen:?}"
    );
    assert_eq!(2, seen.iter().filter(|e| *e == "ocr").count(), "each box is read once");
}

/// The read-through property. The same popup rect that hides the word
/// from a live grab cannot hide it from the hold: those pixels were
/// copied before the popup existed (ADR-0008/0010).
#[test]
fn a_hold_resolves_the_word_the_popup_is_covering() {
    let (worker, _dicts, _log_rx) = spawn(Some("食"), false, None, None);
    // Live, unfrozen: our own popup takes the word with it.
    worker.trigger().send(masked_hover(1, CaptureMode::Live)).unwrap();
    assert!(matches!(answer(&worker).outcome, LookupOutcome::Hide));

    worker.trigger().send(freeze(2, AT)).unwrap();
    worker.trigger().send(masked_hover(3, CaptureMode::Live)).unwrap();
    assert!(
        matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }),
        "a frozen hold reads through the popup, whatever mask it is handed"
    );
}

/// Release ends the hold: the next lookup copies the screen again.
#[test]
fn a_thaw_returns_the_worker_to_live_grabs() {
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, None, None);
    worker.trigger().send(freeze(1, AT)).unwrap();
    worker.trigger().send(hover(2)).unwrap();
    answer(&worker);
    worker.trigger().send(Trigger { kind: TriggerKind::Thaw, id: RequestId(3) }).unwrap();
    worker.trigger().send(hover(4)).unwrap();
    answer(&worker);

    let seen = events(&log_rx);
    assert_eq!(
        2,
        seen.iter().filter(|e| *e == "grab").count(),
        "the press-time copy, then a live one after the release: {seen:?}"
    );
}

/// A crossed output takes a fresh full grab: the second press-time copy
/// is what makes "hold and glance at the other monitor" work.
#[test]
fn a_second_freeze_mid_hold_copies_again() {
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, None, None);
    worker.trigger().send(freeze(1, AT)).unwrap();
    worker.trigger().send(hover(2)).unwrap();
    answer(&worker);
    let entered = PhysPoint { x: AT.x + 5000, y: AT.y };
    worker.trigger().send(freeze(3, entered)).unwrap();
    worker.trigger().send(hover_at(4, entered)).unwrap();
    assert!(matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }));

    let seen = events(&log_rx);
    assert_eq!(
        2,
        seen.iter().filter(|e| *e == "grab").count(),
        "one copy per press, and crossing outputs is a press: {seen:?}"
    );
}

/// The reload gap ticket 41 pinned, through the real seam: a rebuild
/// renames a new database over the old inode, so the handle the worker
/// holds keeps reading the old one until a `reload` reopens it - and the
/// reopened file's identities are what the popup then names.
#[test]
fn a_reload_reopens_the_dictionary_the_worker_reads() {
    let (log_tx, _log_rx) = mpsc::channel::<String>();
    let capture_log = log_tx.clone();
    let (worker, dicts) = Worker::spawn(
        settings(),
        move || {
            Ok(WorkerParts {
                capture: Box::new(FakeCapture {
                    log: capture_log,
                    gate: None,
                    entered_tx: None,
                }),
                ocr: Box::new(FakeOcr {
                    log: log_tx,
                    text: Some("食".to_string()),
                    panics: false,
                }),
                dict: Box::new(dict_named("BeforeTheRebuild")),
                // What the settings process's rename leaves behind.
                reopen_dict: Some(Box::new(|| Ok(Box::new(dict_named("AfterTheRebuild"))))),
                serve: None,
                engine: LookupEngine::new(Deconjugator::new(Vec::new())),
            })
        },
        || {},
    )
    .expect("the worker must start");
    assert_eq!("BeforeTheRebuild", dicts[0].name);

    // The bin can only send what it knew before the rebuild.
    let mut reloaded = settings();
    reloaded.dicts = vec![DictInfo { dict_id: 1, name: "WhatTheBinKnew".to_string() }];
    worker
        .trigger()
        .send(Trigger { kind: TriggerKind::Reload(Box::new(reloaded)), id: RequestId(1) })
        .unwrap();
    worker.trigger().send(hover(2)).unwrap();

    let result = answer(&worker);
    let LookupOutcome::Ready { presentation, .. } = result.outcome else {
        panic!("expected a hit after the reload");
    };
    let top = presentation.top.expect("the hit must present");
    assert_eq!(
        "AfterTheRebuild", top.blocks[0].dict_name,
        "the reopened database's identities must win over the bin's stale list"
    );
}
