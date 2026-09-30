//! The core pipeline for this session.
//! The `open` closure of the Worker builds three parts.
//! See ARCHITECTURE.md#workspace-and-seams.
//!
//! The Worker owns its thread and every thread-affine part on it.
//! These parts are the capture backend selected by the ladder, the meikiocr
//! engine, and the dictionary handle. The daemon keeps only the handle.
//! A lookup therefore cannot stall the pump.
//!
//! Two absences are normal and not fatal. The log names both absences, and
//! the daemon stays alive:
//!
//! - **No database.** A fresh install has no database until the first
//!   rebuild. See ARCHITECTURE.md#settings-and-config. A lookup reports
//!   the absence. A `reload` after the rebuild opens the path.
//! - **No deconjugation rules.** A package fault affects conjugated
//!   forms, not the whole pipeline. Exact matches still resolve.

use crate::capture::backend::Backend;
use crate::capture::portal::PortalCapture;
use crate::capture::WlrScreencopy;
use crate::wayland::Advertised;
use anyhow::{bail, Context, Result};
use chibipop::config::{AnkiConfig, ProfileSession};
use chibipop::geom::{PhysRect, ScanDisplay};
use chibipop::dict::pitch::PitchClaim;
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::{Dictionary, Entry, TermRow};
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use chibipop::text::layout::{CaptureSize, OcrLine};
use chibipop::worker::{ServeNudge, Worker, WorkerParts, WorkerSettings};
use chibipop_linux::ocr::MeikiOcr;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// The bundled deconjugation rules, relative to the binary.
const RULES: &str = "data/deconjugator.json";

/// The data that the core Worker needs from this session at startup.
pub struct Setup {
    /// The startup capability probe. The screencopy rung uses this probe
    /// to bind its own connection on the worker thread.
    pub globals: Vec<Advertised>,
    /// The rung that the capture ladder selected.
    /// See ARCHITECTURE.md#capture-and-masking.
    pub backend: Option<Backend>,
    /// The dictionary path that the builder wrote. The file can be absent.
    pub db: PathBuf,
}

/// Pixels for one OCR job and the channel that returns its lines.
///
/// The engine is thread-affine. It holds three ONNX sessions that the
/// Worker created on its own thread. A job that needs OCR outside a hover
/// must run on that thread. Core owns that seam as
/// `WorkerParts::serve`. This type travels through the seam.
pub struct OcrRequest {
    /// The profile captured when the user activated this action.
    pub session: ProfileSession,
    /// Top-down BGRA8 at native resolution. This adapter never upscales.
    /// See ARCHITECTURE.md#ocr-engine.
    pub bgra: Vec<u8>,
    pub w: i32,
    pub h: i32,
    /// The pump channel. An answer arrives as an event, so no thread blocks.
    /// See ARCHITECTURE.md#workspace-and-seams. A failure travels as text
    /// because the pump owns the log.
    pub answer: calloop::channel::Sender<Result<Vec<OcrLine>, String>>,
}

/// The OCR job queue and the wake that delivers each job.
///
/// The Worker owns the only OCR engine. The Worker drains this queue
/// from its `serve` hook. The Worker blocks on its trigger channel and
/// cannot watch this queue. A queued job therefore needs a wake.
/// One type owns the queue and wake together, so a caller cannot use one
/// half without the other. The `action::OcrJobs` type in the Windows binary
/// uses the same rule.
#[derive(Clone)]
pub struct OcrJobs {
    tx: mpsc::Sender<OcrRequest>,
    nudge: ServeNudge,
}

impl OcrJobs {
    pub fn new(tx: mpsc::Sender<OcrRequest>, nudge: ServeNudge) -> OcrJobs {
        OcrJobs { tx, nudge }
    }

    /// This queue has no pipeline behind it. No thread serves its job.
    /// Use this state while the Worker is absent, for example when no
    /// capture protocol or OCR model exists, or when the portal refuses
    /// a session. The caller sees a closed answer channel, not a job that
    /// waits forever without a report.
    pub fn disconnected() -> OcrJobs {
        OcrJobs { tx: mpsc::channel().0, nudge: ServeNudge::disconnected() }
    }

    /// Queue pixels, then wake the Worker to read them.
    pub fn send(&self, request: OcrRequest) -> Result<()> {
        self.tx.send(request).map_err(|_| {
            anyhow::anyhow!("the OCR pipeline is not running, so there is nothing to recognise with")
        })?;
        self.nudge.nudge();
        Ok(())
    }
}

/// The dictionary for a fresh install has no file.
///
/// A term lookup fails and reports the expected database path, so the
/// user can correct this state. Identities and entries stay empty.
/// No caller above needs a special case.
struct NoDictionary {
    path: PathBuf,
}

impl Dictionary for NoDictionary {
    fn terms_for(&self, _surface: &str) -> Result<Vec<TermRow>> {
        bail!(
            "no dictionary at {} - add Yomitan archives and press Rebuild in `chibipop settings`",
            self.path.display()
        )
    }

    fn entries(&self, _ids: &[i64]) -> Result<Vec<Entry>> {
        Ok(Vec::new())
    }

    fn dicts(&self) -> Result<Vec<DictInfo>> {
        Ok(Vec::new())
    }

    fn pitch_for(&self, _term: &str, _reading: &str) -> Vec<PitchClaim> {
        Vec::new()
    }
}

/// Open the dictionary at `db`. Return a `NoDictionary` when the file is absent.
fn open_dict(db: &Path) -> Result<Box<dyn Dictionary>> {
    if !db.is_file() {
        return Ok(Box::new(NoDictionary { path: db.to_path_buf() }));
    }
    let dict = SqliteDictionary::open(db)
        .with_context(|| format!("opening the dictionary {}", db.display()))?;
    Ok(Box::new(dict))
}

/// Return the path of the bundled deconjugation rules.
/// The rules sit beside the binary, in a distro `../share/chibipop`, or
/// in the source tree of a debug build. `chibipop::paths::data_file` handles
/// the last case.
fn rules_file() -> PathBuf {
    let beside = chibipop::paths::beside_exe(RULES);
    if beside.is_file() {
        return beside;
    }
    let shared = chibipop::paths::beside_exe(&format!("../share/chibipop/{RULES}"));
    if shared.is_file() {
        return shared;
    }
    chibipop::paths::data_file(RULES)
}

/// Return the deconjugator for this build.
///
/// An absent rules file affects conjugated forms, and this function reports
/// the absence on stderr.
/// A refusal to start would affect every lookup.
/// The Worker thread reads the file with its other parts.
fn deconjugator() -> Deconjugator {
    let path = rules_file();
    match load_rules(&path) {
        Ok(rules) => Deconjugator::new(rules),
        Err(e) => {
            eprintln!(
                "chibipop: no deconjugation rules at {} ({e:#}); only exact forms will resolve",
                path.display()
            );
            Deconjugator::new(Vec::new())
        }
    }
}

/// Return the box that the user drew in core's coordinate space.
/// [`chibipop::config::SentenceMode::Static`] reads this box in physical
/// pixels.
///
/// The TOML file stores `[x, y, w, h]`. A rectangle has four numbers, and
/// an array round trips on every platform. `Config` is shared. Every caller
/// above this function needs a [`PhysRect`]. One conversion keeps the region
/// that the pipeline reads equal to the region that the outline draws. The
/// outline predicate of the daemon also calls this function.
pub fn static_region(anki: &AnkiConfig) -> Option<PhysRect> {
    anki.static_region.map(|[x, y, w, h]| PhysRect { x, y, w, h })
}

/// Return Worker settings from the retained profile session.
pub fn settings(session: &ProfileSession, dicts: &[DictInfo]) -> WorkerSettings {
    let config = session.config();
    WorkerSettings {
        max_passes: config.ocr.max_ocr_passes,
        // Never upscale. meikiocr scores worse on 2x crops than on
        // native-resolution crops on every benchmark slice.
        // See ARCHITECTURE.md#ocr-engine.
        upscale: 1,
        prefer_vertical: config.ocr.prefer_vertical,
        capture: CaptureSize { w: config.ocr.capture_width, h: config.ocr.capture_height },
        scan_alphanumeric: config.ocr.scan_alphanumeric,
        discard_furigana: config.ocr.discard_furigana,
        show_lookup_log: config.debug.show_lookup_log,
        language: config.ocr.language.clone(),
        // The settings window supplies active terms and pitch lists.
        // The names stay exact and retain priority order.
        // The engine does not filter this list. An exact name either
        // identifies an installed dictionary or identifies no dictionary.
        // See ARCHITECTURE.md#dictionary-and-lookup.
        present_cfg: session.present_config().clone(),
        scan_display: ScanDisplay {
            captures: config.debug.show_scan_region,
            highlight: config.popup.highlight_match,
        },
        sentence_mode: config.anki.sentence_mode,
        static_region: static_region(&config.anki),
        dicts: dicts.to_vec(),
    }
}

/// Start the pipeline. Return the dictionary identities that it read.
///
/// `portal` is the session that the daemon opened for eager consent.
/// This session is rung 2 of the capture ladder. The caller gives the
/// session to the Worker thread because that thread reads through it.
/// The screencopy rung needs no session here. It binds its own connection
/// inside the closure on that thread.
///
/// `jobs` is the receiver half of an [`OcrJobs`] queue. The core
/// `serve` hook drains the queue between lookups. Each spawn needs a
/// fresh channel because a respawn creates a new thread with a new engine.
/// The wake belongs to the new Worker.
pub fn spawn(
    setup: &Setup,
    settings: WorkerSettings,
    portal: Option<PortalCapture>,
    ping: calloop::ping::Ping,
    jobs: mpsc::Receiver<OcrRequest>,
) -> Result<(Worker, Vec<DictInfo>)> {
    let globals = setup.globals.clone();
    let backend = setup.backend;
    let db = setup.db.clone();
    let reopen_db = setup.db.clone();

    Worker::spawn(
        settings,
        move || {
            let capture: Box<dyn chibipop::text::RegionCapture> = match (backend, portal) {
                (Some(Backend::WlrScreencopy), _) => Box::new(
                    WlrScreencopy::open(&globals).context("opening the screencopy backend")?,
                ),
                (Some(Backend::Portal), Some(session)) => Box::new(session),
                (Some(Backend::Portal), None) => bail!(
                    "the portal capture session was refused; grant it and run \
                     `chibipop ctl reload`"
                ),
                (None, _) => {
                    bail!("this compositor advertises no capture protocol chibipop can use")
                }
            };
            let ocr = MeikiOcr::new().context("opening the bundled OCR models")?;
            let dict = open_dict(&db)?;
            Ok(WorkerParts {
                capture,
                ocr: Box::new(ocr),
                dict,
                // The rebuild renames a new database over this path.
                // A second open therefore serves the new dictionary.
                // This daemon outlives its rebuilds and never restarts.
                reopen_dict: Some(Box::new(move || open_dict(&reopen_db))),
                engine: LookupEngine::new(deconjugator()),
                serve: Some(serve_jobs(jobs)),
            })
        },
        move || ping.ping(),
    )
}

/// Serve OCR jobs through the `serve` hook.
///
/// This hook has a name instead of an inline closure.
/// A test can install the shipped hook over fake seams.
/// A test with its own closure would prove only that a hook works, not that
/// this hook works.
///
/// This hook calls `try_iter`, not `iter`.
/// It runs immediately before the Worker blocks on its trigger channel.
/// A hook that waits for the next job would stop the hover pipeline after a
/// clipboard copy.
pub fn serve_jobs(jobs: mpsc::Receiver<OcrRequest>) -> chibipop::worker::ServeHook {
    Box::new(move |source, _lookup_session| {
        for job in jobs.try_iter() {
            let lines = source
                .recognise_for_session(&job.session, &job.bgra, job.w, job.h)
                .map_err(|e| format!("{e:#}"));
            // A stopped caller does not affect this thread.
            // The next job in the queue still runs.
            let _ = job.answer.send(lines);
        }
    })
}

/// The log line about the dictionaries that a spawn found.
pub fn dict_line(db: &Path, dicts: &[DictInfo]) -> String {
    if dicts.is_empty() {
        return format!(
            "no dictionary at {} - lookups will say so until a rebuild",
            db.display()
        );
    }
    let names: Vec<&str> = dicts.iter().map(|d| d.name.as_str()).collect();
    format!("{} dictionary/ies: {}", dicts.len(), names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh install has no database.
    /// The daemon still starts, and a lookup reports the required path.
    #[test]
    fn a_missing_database_opens_as_the_absence_of_one() {
        let dict = open_dict(Path::new("/nonexistent/chibipop.sqlite")).expect("absence is not an error");
        assert!(dict.dicts().unwrap().is_empty());
        assert!(dict.entries(&[1]).unwrap().is_empty());
        let e = dict.terms_for("食").expect_err("a lookup must say what is missing");
        assert!(format!("{e:#}").contains("/nonexistent/chibipop.sqlite"), "{e:#}");
    }
}
