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
use chibipop::config::{AnkiConfig, Config};
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

/// Return the Worker settings from the configuration that the daemon read.
pub fn settings(config: &Config, dicts: &[DictInfo]) -> WorkerSettings {
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
        language: config.ocr.language.clone(),
        // The settings window supplies active terms and pitch lists.
        // The names stay exact and retain priority order.
        // The engine does not filter this list. An exact name either
        // identifies an installed dictionary or identifies no dictionary.
        // See ARCHITECTURE.md#dictionary-and-lookup.
        present_cfg: config.present_config(dicts),
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
    Box::new(move |source| {
        for job in jobs.try_iter() {
            let lines = source.recognise(&job.bgra, job.w, job.h).map_err(|e| format!("{e:#}"));
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
        assert!(format!("{e:#}").contains("Rebuild"), "{e:#}");
    }

    #[test]
    fn the_dictionary_line_names_what_was_found() {
        let db = Path::new("/tmp/chibipop.sqlite");
        assert!(dict_line(db, &[]).contains("no dictionary at /tmp/chibipop.sqlite"));
        let dicts = vec![DictInfo { dict_id: 1, name: "Jitendex".to_string() }];
        assert!(dict_line(db, &dicts).contains("Jitendex"));
    }

    /// The search must name the file that every layout installs.
    /// A typo would create a pipeline with no deconjugation and no error.
    /// The install chooses one of three candidates.
    /// A test binary in `target/debug/deps` matches none of them.
    /// Therefore, this test pins the file name, not the directory.
    #[test]
    fn the_rules_search_looks_for_the_file_this_build_ships() {
        assert!(rules_file().ends_with(RULES), "{}", rules_file().display());
    }

    /// The file contains deconjugation rules, not an empty array.
    /// Every layout installs this copy.
    #[test]
    fn the_shipped_rules_parse_and_are_not_empty() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(RULES);
        let rules = load_rules(&repo).expect("the shipped deconjugation rules must parse");
        assert!(!rules.is_empty());
    }

    /// The Linux adapter never upscales. See ARCHITECTURE.md#ocr-engine.
    /// Native crops score better than 2x crops on every benchmark slice.
    /// The shared Worker must receive factor 1 from this daemon.
    /// The OCR gate feeds native meiki fixtures directly.
    /// It cannot catch a runtime factor change. This test pins the factor
    /// at the seam.
    #[test]
    fn the_linux_worker_never_upscales() {
        let s = settings(&Config::default(), &[]);
        assert_eq!(1, s.upscale);
        assert_eq!(1, s.snapshot().upscale, "and the snapshot carries it to TextSource");
    }

    /// Earlier code hardcoded `static_region: None`.
    /// A comment there called the overlay a Windows surface.
    /// Therefore, `SentenceMode::Static` did not act here after the user drew
    /// a box. Core's `resolve_static` function is platform agnostic and reads
    /// only this field. This field enables the feature.
    /// This test pins the field at the seam that builds `WorkerSettings` for
    /// this daemon.
    #[test]
    fn a_configured_static_region_reaches_the_worker_settings() {
        let mut config = Config::default();
        config.anki.sentence_mode = chibipop::config::SentenceMode::Static;
        config.anki.static_region = Some([120, 240, 800, 300]);

        let s = settings(&config, &[]);
        // Check both values because `resolve_static` requires the pair.
        // The mode alone selects line mode. A region alone stays unread.
        assert_eq!(chibipop::config::SentenceMode::Static, s.sentence_mode);
        assert_eq!(Some(PhysRect { x: 120, y: 240, w: 800, h: 300 }), s.static_region);
    }

    /// The default has no drawn box and no invented box.
    /// Core selects line mode when the region is absent.
    /// Therefore, the correct answer is `None`, not a whole-screen stand-in.
    #[test]
    fn no_drawn_region_stays_absent_rather_than_becoming_the_screen() {
        assert_eq!(None, settings(&Config::default(), &[]).static_region);
    }

    /// Dictionary exclusion has one purpose.
    /// An unchecked row in the Terms section of the settings window must
    /// reach the pipeline.
    /// `settings` is the only place where the daemon builds `WorkerSettings`.
    /// Both `reload` and `spawn` call it. This test pins the scope at that seam.
    #[test]
    fn an_excluded_dictionary_is_dropped_from_the_worker_settings() {
        let dicts = vec![
            DictInfo { dict_id: 1, name: "大辞林　第四版".to_string() },
            DictInfo { dict_id: 2, name: "Jitendex.org [2026-07-09]".to_string() },
        ];
        let mut config = Config::default();
        config.ocr.language = "ja".to_string();
        config
            .dictionaries
            .per_language
            .insert("ja".to_string(), vec!["大辞林　第四版".to_string()]);

        let cfg = settings(&config, &dicts).present_cfg;
        // The scope carries the exact names from the configuration.
        // The pipeline searches no dictionary outside this scope.
        assert_eq!(vec!["大辞林　第四版".to_string()], cfg.terms);
        let keeps = |name: &str| chibipop::present::keeps_dict(name, &cfg.terms);
        assert!(keeps("大辞林　第四版"));
        assert!(!keeps("Jitendex.org [2026-07-09]"), "excluded, so not searched");
    }

    /// The shipped default has no split and no filter.
    /// It searches every dictionary.
    ///
    /// A default config names no dictionary in either array.
    /// Every installed dictionary is therefore new and starts active.
    #[test]
    fn a_config_with_no_split_searches_every_dictionary() {
        let dicts = vec![DictInfo { dict_id: 1, name: "Jitendex.org".to_string() }];
        let cfg = settings(&Config::default(), &dicts).present_cfg;
        assert_eq!(vec!["Jitendex.org".to_string()], cfg.terms);
        assert!(chibipop::present::keeps_dict("Jitendex.org", &cfg.terms));
    }

    /// The Linux settings UI hides `ocr.language` and keeps its stored value.
    /// A Windows install can share a config that names a language meikiocr
    /// does not read. No engine probe answers that question.
    /// One question remains: does the configured language have its own list?
    /// When it has one, the pipeline searches that list.
    #[test]
    fn the_ocr_languages_own_list_is_searched_and_the_global_list_stands_in_without_one() {
        let dicts = vec![
            DictInfo { dict_id: 1, name: "大辞林　第四版".to_string() },
            DictInfo { dict_id: 2, name: "Jitendex.org [2026-07-09]".to_string() },
        ];
        let mut config = Config::default();
        config.ocr.language = "zh-Hans-CN".to_string();
        config
            .dictionaries
            .per_language
            .insert("zh-Hans-CN".to_string(), vec!["大辞林　第四版".to_string()]);

        let scoped = settings(&config, &dicts).present_cfg;
        assert_eq!(vec!["大辞林　第四版".to_string()], scoped.terms);

        // The same entry exists under a language that does not match the config language.
        // The global terms list decides instead. An empty global list means every
        // installed dictionary.
        config.ocr.language = "ja".to_string();
        let global = settings(&config, &dicts).present_cfg;
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            global.terms
        );
    }
}
