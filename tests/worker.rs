//! These tests exercise the Worker with fake `RegionCapture` and `OcrEngine`
//! backends.
//! They cover the trigger-to-result flow, latest-wins behavior, and reload
//! semantics that the platform bins use. They do not use an OS.

use chibipop::controller::{LookupOutcome, RequestId};
use chibipop::geom::{PhysPoint, PhysRect, ScanDisplay};
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::FakeDictionary;
use chibipop::present::DictInfo;
use chibipop::text::layout::{CaptureSize, OcrLine, OcrWord};
use chibipop::text::mask::{CaptureMask, CaptureMode};
use chibipop::text::{Frame, OcrEngine, RegionCapture, TextSource};
use chibipop::worker::{Hover, Trigger, TriggerKind, Worker, WorkerParts, WorkerSettings};
use std::sync::mpsc;
use std::time::Duration;

/// Set a timeout that a healthy test does not reach.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Return all events that the Worker has logged.
/// The test sends each event before its result, so the test sees a complete
/// event list after it receives that result.
fn events(log_rx: &mpsc::Receiver<String>) -> Vec<String> {
    log_rx.try_iter().collect()
}

/// Return fixed frames. A test can use a gate to keep a grab open.
struct FakeCapture {
    log: mpsc::Sender<String>,
    /// Use one token for each grab.
    gate: Option<mpsc::Receiver<()>>,
    /// Send a signal when a grab starts.
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

/// Return one whole-image word for each call, or no words.
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

/// Return one term, one Entry, and one Dictionary.
fn dict() -> FakeDictionary {
    dict_named("FakeDict")
}

/// Return the same term and Entry under another Dictionary name.
fn dict_named(name: &str) -> FakeDictionary {
    let mut d = FakeDictionary::new();
    d.add_dict(1, name);
    d.add_term("食", None, None, "", None, 10, 1);
    d.add_entry(10, 1, r#"["to eat"]"#);
    d
}

/// List each Dictionary name that these fakes can answer.
/// The config must use these exact names.
///
/// A list under `[dictionaries]` matches a Dictionary by exact name.
/// An enabled terms list with no installed Dictionary names searches nothing.
/// It has no ladder that widens the scope
/// (ARCHITECTURE.md#dictionary-and-lookup).
/// A fixture that names no Dictionary therefore shows an empty popup.
/// List every identity that these tests expect to show as a card.
/// The list includes the Dictionary from `dict()`, the two names that a reopen
/// swaps, and the name that a reload assigns.
/// Exclude `WhatTheBinKnew` on purpose.
/// The reopened file must replace this stale name.
/// A card with this name is the failure that the test detects.
const SEARCHED: [&str; 4] = ["FakeDict", "Renamed", "BeforeTheRebuild", "AfterTheRebuild"];

fn settings() -> WorkerSettings {
    let mut cfg = chibipop::config::Config::default();
    cfg.dictionaries.terms = SEARCHED.iter().map(|name| (*name).to_string()).collect();
    WorkerSettings {
        max_passes: 1,
        upscale: 2,
        prefer_vertical: false,
        capture: CaptureSize::default(),
        scan_alphanumeric: true,
        discard_furigana: true,
        language: "ja".to_string(),
        present_cfg: cfg.present_config(&[]),
        scan_display: ScanDisplay { captures: false, highlight: false },
        sentence_mode: chibipop::config::SentenceMode::Line,
        static_region: None,
        dicts: Vec::new(),
    }
}

/// Build a Worker over the fake backends.
/// OCR returns the supplied optional `text` for every capture.
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

/// Use this point for fake hovers.
const AT: PhysPoint = PhysPoint { x: 600, y: 300 };

fn hover(id: u64) -> Trigger {
    Trigger {
        kind: TriggerKind::Hover(Hover { at: AT, mask: CaptureMask::NONE }),
        id: RequestId(id),
    }
}

/// Return a hover with the Worker's popup over the hover point.
fn masked_hover(id: u64, mode: CaptureMode) -> Trigger {
    let popup = PhysRect { x: AT.x - 50, y: AT.y - 50, w: 100, h: 100 };
    Trigger {
        kind: TriggerKind::Hover(Hover { at: AT, mask: CaptureMask::for_mode(mode, Some(popup)) }),
        id: RequestId(id),
    }
}

/// The mask edge acts as a capture edge.
/// `FakeOcr` returns one word that spans the whole grab, so a popup anywhere
/// in it drops the word. It does not leave a partial glyph for lookup
/// (ARCHITECTURE.md#capture-and-masking).
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

/// A frozen grab predates the popup. The same rect therefore adds no mask.
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

/// Return `unchanged` after the first grab. This models a damage-paced dwell
/// above the seam (ARCHITECTURE.md#capture-and-masking).
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

/// Build a Worker over that backend. OCR returns the same word each time.
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

/// Test the dwell re-check and its observable cost.
///
/// An unchanged dwell uses one grab and no OCR pass.
/// The pixels, mask, and answer stay the same, so the Worker reuses the words.
/// If the popup appears between looks, the Worker asks a new question.
/// OCR reads the grab after the mask (ARCHITECTURE.md#capture-and-masking).
/// That look uses one OCR pass.
/// Each later unchanged dwell uses one grab and no OCR pass.
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

/// Test the full pipeline from trigger to lookup result.
/// The read has guards around capture and OCR.
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

    // One read: the guard surrounds capture and OCR.
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

/// A drill-down uses the Dictionary only.
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

/// One bad frame does not stop the Worker.
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

    // The Worker still serves a screen-free lookup.
    worker
        .trigger()
        .send(Trigger { kind: TriggerKind::DrillDown("食".to_string()), id: RequestId(5) })
        .unwrap();
    let next = worker.results().recv_timeout(TIMEOUT).unwrap();
    assert_eq!(RequestId(5), next.id);
    assert!(matches!(next.outcome, LookupOutcome::DrillDown(_)));
}

/// Hovers that arrive while a read runs coalesce to the newest hover.
/// The Worker does not answer the stale hover.
#[test]
fn queued_hovers_coalesce_to_the_newest() {
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, Some(gate_rx), Some(entered_tx));

    worker.trigger().send(hover(1)).unwrap();
    entered_rx.recv_timeout(TIMEOUT).expect("the first grab must start");

    // Send both while the first read remains open.
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

/// Apply a reload before the next hover.
/// The reload changes Dictionary identities, language, and scan settings.
/// The reload does not return a result.
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

/// Report startup failure from `spawn`, not from a dead Worker.
#[test]
fn a_failing_open_fails_the_spawn() {
    let Err(err) = Worker::spawn(settings(), || anyhow::bail!("no backend"), || {}) else {
        panic!("spawn must propagate the open failure");
    };
    assert!(format!("{err:#}").contains("no backend"), "{err:#}");
}

/// Wake the bin event loop after each queued result.
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

// -- trigger mode's hold, end to end through the Worker --

/// Return a hover at another point. The hold then asks for another box.
fn hover_at(id: u64, at: PhysPoint) -> Trigger {
    Trigger {
        kind: TriggerKind::Hover(Hover { at, mask: CaptureMask::NONE }),
        id: RequestId(id),
    }
}

fn freeze(id: u64, at: PhysPoint) -> Trigger {
    Trigger { kind: TriggerKind::Freeze(at), id: RequestId(id) }
}

/// Return one result or fail the test.
fn answer(worker: &Worker) -> chibipop::worker::WorkerResult {
    worker.results().recv_timeout(TIMEOUT).expect("the worker must answer")
}

/// Freeze once at press time and use that copy for every lookup in the hold.
/// The number of cursor lookups does not change this rule.
#[test]
fn a_trigger_hold_copies_once_and_serves_every_lookup_from_that_copy() {
    let (worker, _dicts, log_rx) = spawn(Some("食"), false, None, None);
    worker.trigger().send(freeze(1, AT)).unwrap();
    worker.trigger().send(hover(2)).unwrap();
    assert!(matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }));

    // The cursor moved. Ask for another box from the same frame.
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

/// Test read-through behavior.
/// A live grab cannot read a word under the popup.
/// The hold can read it because it copied the pixels before the popup existed.
#[test]
fn a_hold_resolves_the_word_the_popup_is_covering() {
    let (worker, _dicts, _log_rx) = spawn(Some("食"), false, None, None);
    // Live mode uses the popup mask, so it drops the word.
    worker.trigger().send(masked_hover(1, CaptureMode::Live)).unwrap();
    assert!(matches!(answer(&worker).outcome, LookupOutcome::Hide));

    worker.trigger().send(freeze(2, AT)).unwrap();
    worker.trigger().send(masked_hover(3, CaptureMode::Live)).unwrap();
    assert!(
        matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }),
        "a frozen hold reads through the popup, whatever mask it is handed"
    );
}

/// A release ends the hold. The next lookup copies the screen again.
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

/// When the cursor crosses to another output, trigger mode needs a new full grab.
/// The second press-time copy lets a hold read the other output.
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

/// Test the reload gap through the real seam.
/// A rebuild renames a new database over the old inode.
/// The Worker handle still reads the old file until `reload` opens the new file.
/// The popup then uses the identities from the reopened file.
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
                // This is the file state after the settings process renames the database.
                reopen_dict: Some(Box::new(|| Ok(Box::new(dict_named("AfterTheRebuild"))))),
                serve: None,
                engine: LookupEngine::new(Deconjugator::new(Vec::new())),
            })
        },
        || {},
    )
    .expect("the worker must start");
    assert_eq!("BeforeTheRebuild", dicts[0].name);

    // The bin can send only the identities that it had before the rebuild.
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

// -- the `serve` hook: one-off OCR jobs between lookups --

/// Hold one-off pixels that the Windows bin queues for OCR-to-clipboard.
struct Job {
    bgra: Vec<u8>,
    w: i32,
    h: i32,
    done: mpsc::Sender<Result<Vec<OcrLine>, String>>,
}

/// Build a Worker with a `serve` hook over fake backends.
///
/// The hook logs each call and drains the bin's job queue through the facade.
/// It cannot access the engine. The Worker cannot see the bin's queue.
fn spawn_serving(
    gate: Option<mpsc::Receiver<()>>,
    entered_tx: Option<mpsc::Sender<()>>,
) -> (Worker, mpsc::Sender<Job>, mpsc::Receiver<String>) {
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let capture_log = log_tx.clone();
    let hook_log = log_tx.clone();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (worker, _dicts) = Worker::spawn(
        settings(),
        move || {
            Ok(WorkerParts {
                capture: Box::new(FakeCapture { log: capture_log, gate, entered_tx }),
                ocr: Box::new(FakeOcr { log: log_tx, text: Some("食".to_string()), panics: false }),
                dict: Box::new(dict()),
                reopen_dict: None,
                serve: Some(Box::new(move |source: &TextSource| {
                    let _ = hook_log.send("serve".to_string());
                    while let Ok(job) = job_rx.try_recv() {
                        let lines = source
                            .recognise(&job.bgra, job.w, job.h)
                            .map_err(|e| format!("{e:#}"));
                        let _ = job.done.send(lines);
                    }
                })),
                engine: LookupEngine::new(Deconjugator::new(Vec::new())),
            })
        },
        || {},
    )
    .expect("the worker must start with a serve hook installed");
    (worker, job_tx, log_rx)
}

/// Return one BGRA pixel and a channel for its result.
fn job() -> (Job, mpsc::Receiver<Result<Vec<OcrLine>, String>>) {
    let (done, done_rx) = mpsc::channel();
    (Job { bgra: vec![0u8; 4], w: 1, h: 1, done }, done_rx)
}

/// The Worker calls the hook once before it waits.
/// Consume that call before a test checks the next call.
fn wait_for_a_hook_run(log_rx: &mpsc::Receiver<String>) {
    assert_eq!(
        Some("serve".to_string()),
        log_rx.recv_timeout(TIMEOUT).ok(),
        "the hook must run before the worker blocks"
    );
}

/// Test the nudge path and the OCR facade together.
/// A queued job wakes a Worker that waits on the trigger channel.
/// The Worker does not poll for the job.
/// The hook reads the pixels through the facade on the Worker thread.
/// No trigger carries the job.
#[test]
fn a_nudged_job_wakes_a_blocked_worker_and_is_read_through_the_facade() {
    let (worker, jobs, log_rx) = spawn_serving(None, None);
    wait_for_a_hook_run(&log_rx);
    let (job, answered) = job();

    jobs.send(job).unwrap();
    worker.serve_nudge().nudge();

    let lines = answered
        .recv_timeout(TIMEOUT)
        .expect("the nudge must wake the worker")
        .expect("the facade must answer");
    assert_eq!("食", lines[0].words[0].text);
    assert!(events(&log_rx).contains(&"ocr".to_string()), "the worker's own engine read it");
    assert!(
        worker.results().try_recv().is_err(),
        "a nudge is not a lookup: it must answer nothing"
    );
}

/// The idle budget is 0 wakeups/s. The Worker must wait when no job exists.
/// A 20 ms poll calls the hook about 15 times in this window.
#[test]
fn an_idle_worker_with_a_serve_hook_never_wakes_itself() {
    let (_worker, _jobs, log_rx) = spawn_serving(None, None);
    wait_for_a_hook_run(&log_rx);

    std::thread::sleep(Duration::from_millis(300));

    assert!(events(&log_rx).is_empty(), "an idle worker must do nothing at all");
}

/// Do not lose a wake.
/// If a job enters the queue while a lookup runs, the Worker serves it after
/// the lookup ends.
/// The Worker serves it before it waits again.
/// A nudge that the drain consumes still costs no extra poll.
#[test]
fn a_job_queued_during_a_lookup_is_served_before_the_worker_blocks_again() {
    let (release_tx, gate) = mpsc::channel::<()>();
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (worker, jobs, log_rx) = spawn_serving(Some(gate), Some(entered_tx));
    wait_for_a_hook_run(&log_rx);

    worker.trigger().send(hover(1)).unwrap();
    entered.recv_timeout(TIMEOUT).expect("the worker must reach the gated grab");
    let (job, answered) = job();
    jobs.send(job).unwrap();
    worker.serve_nudge().nudge();
    release_tx.send(()).unwrap();

    assert!(matches!(answer(&worker).outcome, LookupOutcome::Ready { .. }), "the hover answers");
    let lines = answered
        .recv_timeout(TIMEOUT)
        .expect("the job must be served when the lookup ends")
        .expect("the facade must answer");
    assert_eq!("食", lines[0].words[0].text);
}
