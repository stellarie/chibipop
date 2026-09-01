//! The core pipeline on this session: what the Worker's `open` closure
//! builds, and where its three parts come from (ADR-0001).
//!
//! The Worker owns its thread and everything thread-affine on it: the
//! capture backend the ladder picked (ADR-0002), the meikiocr engine
//! (ADR-0009), and the dictionary handle. The daemon keeps only the
//! handle, so a lookup can never stall the pump.
//!
//! Two absences are normal rather than fatal, and both are named in the
//! log instead of taking the daemon down:
//!
//! - **No database.** A fresh install has none until the first rebuild
//!   (ADR-0005), so lookups say exactly that and a `reload` after the
//!   rebuild reopens the path for real.
//! - **No deconjugation rules.** A packaging slip costs conjugated forms,
//!   not the whole pipeline: exact matches still resolve.

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

/// What a spawn of the core Worker needs from this session.
pub struct Setup {
    /// The startup capability probe. The screencopy rung binds its own
    /// connection out of this, on the worker thread.
    pub globals: Vec<Advertised>,
    /// Which rung the capture ladder picked (ADR-0002).
    pub backend: Option<Backend>,
    /// The built dictionary; may not exist yet.
    pub db: PathBuf,
}

/// Pixels handed to the Worker's OCR engine as a one-off job, and where
/// the lines go home.
///
/// The engine is thread-affine (ADR-0009: three ONNX sessions built on
/// the Worker's thread), so a job that wants OCR outside a hover has to
/// run *there*. Core owns that seam — `WorkerParts::serve` — and this is
/// what travels through it.
pub struct OcrRequest {
    /// Top-down BGRA8, at native resolution: this adapter never
    /// upscales (ADR-0009).
    pub bgra: Vec<u8>,
    pub w: i32,
    pub h: i32,
    /// The pump's own channel, so an answer is an event and never a
    /// blocked thread (ADR-0001). A failure travels as text because the
    /// log lives on the pump.
    pub answer: calloop::channel::Sender<Result<Vec<OcrLine>, String>>,
}

/// The one-off OCR queue, and the wake that makes it arrive.
///
/// The Worker owns the only OCR engine and drains this queue from its
/// `serve` hook — but it *blocks* on its own trigger channel and cannot
/// see this one, so queueing pixels is only half of handing them over.
/// One type owning both halves, so the two cannot come apart (the
/// Windows bin's `action::OcrJobs`, same reasoning).
#[derive(Clone)]
pub struct OcrJobs {
    tx: mpsc::Sender<OcrRequest>,
    nudge: ServeNudge,
}

impl OcrJobs {
    pub fn new(tx: mpsc::Sender<OcrRequest>, nudge: ServeNudge) -> OcrJobs {
        OcrJobs { tx, nudge }
    }

    /// A queue with no pipeline behind it: the job is parked and never
    /// served, which is the honest answer while the Worker is down (no
    /// capture protocol, no OCR models, a refused portal) - the caller
    /// hears it as a dead answer channel rather than as a copy that
    /// silently never happens.
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

/// The dictionary a fresh install has: none.
///
/// Terms fail loudly and say where the database should be, because that
/// is a state the user can fix; identities and entries are simply empty,
/// so nothing above has to special-case it.
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

/// The dictionary at `db`, or the honest absence of one.
fn open_dict(db: &Path) -> Result<Box<dyn Dictionary>> {
    if !db.is_file() {
        return Ok(Box::new(NoDictionary { path: db.to_path_buf() }));
    }
    let dict = SqliteDictionary::open(db)
        .with_context(|| format!("opening the dictionary {}", db.display()))?;
    Ok(Box::new(dict))
}

/// Where the bundled deconjugation rules are: beside the binary, in a
/// distro `../share/chibipop`, or - in a debug build - in the source
/// tree, which is what `chibipop::paths::data_file` already answers.
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

/// The deconjugator this build ships.
///
/// A missing rules file costs conjugated forms and says so on stderr;
/// refusing to start over it would cost every lookup. Read on the worker
/// thread, like every other part.
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

/// The user-drawn box [`chibipop::config::SentenceMode::Static`] reads
/// from, in the coordinate space core has: physical pixels.
///
/// The TOML stores `[x, y, w, h]` because a rect is four numbers and an
/// array round-trips on every platform (`Config` is shared); everything
/// above the file wants a [`PhysRect`]. One conversion, so the region the
/// pipeline reads and the one the outline draws cannot disagree — the
/// daemon's outline predicate goes through here too.
pub fn static_region(anki: &AnkiConfig) -> Option<PhysRect> {
    anki.static_region.map(|[x, y, w, h]| PhysRect { x, y, w, h })
}

/// What the Worker owns, from the config the daemon has loaded.
pub fn settings(config: &Config, dicts: &[DictInfo]) -> WorkerSettings {
    WorkerSettings {
        max_passes: config.ocr.max_ocr_passes,
        // Never upscaled: meikiocr is strictly worse on 2x crops than
        // on native-resolution ones, on every benchmark slice
        // (ADR-0009 - "the Linux adapter never upscales").
        upscale: 1,
        prefer_vertical: config.ocr.prefer_vertical,
        capture: CaptureSize { w: config.ocr.capture_width, h: config.ocr.capture_height },
        scan_alphanumeric: config.ocr.scan_alphanumeric,
        language: config.ocr.language.clone(),
        // The enabled terms and pitch lists from the settings window,
        // exact names in priority order. No engine gate rides along any
        // more: an exact name either names an installed dictionary or it
        // does not (ADR-0014).
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

/// Spawn the pipeline; answers the dictionary identities it read.
///
/// `portal` is the session the daemon's eager consent already opened
/// (ADR-0002 rung 2), handed over because the Worker thread is what
/// reads through it. The screencopy rung needs nothing here: it binds
/// its own connection inside the closure, on that thread.
///
/// `jobs` is the receiving half of an [`OcrJobs`] queue, drained by the
/// core `serve` hook between lookups. A fresh channel per spawn, because
/// a respawn is a new thread with a new engine and the nudge that wakes
/// it is the new Worker's.
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
                // The rebuild renames a new database over this path, so
                // reopening it is what serves the new dictionary; this
                // daemon outlives its rebuilds and never restarts.
                reopen_dict: Some(Box::new(move || open_dict(&reopen_db))),
                engine: LookupEngine::new(deconjugator()),
                serve: Some(serve_jobs(jobs)),
            })
        },
        move || ping.ping(),
    )
}

/// The `serve` hook: drain the one-off OCR queue against the facade.
///
/// Named rather than written inline above so a test can install the
/// *shipped* hook over fake seams - a test that wrote its own closure
/// would prove that a hook works, not that this one does.
///
/// `try_iter` and not `iter`: the hook runs immediately before the
/// Worker blocks on its trigger channel, so one that waited for the next
/// job would be a hover pipeline stopped on a clipboard copy.
pub fn serve_jobs(jobs: mpsc::Receiver<OcrRequest>) -> chibipop::worker::ServeHook {
    Box::new(move |source| {
        for job in jobs.try_iter() {
            let lines = source.recognise(&job.bgra, job.w, job.h).map_err(|e| format!("{e:#}"));
            // A caller that gave up is not this thread's problem; the
            // next job in the queue still runs.
            let _ = job.answer.send(lines);
        }
    })
}

/// What the log says about the dictionaries a spawn found.
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

    /// A fresh install: the daemon comes up, and the lookup that finds
    /// no database says which one it wanted.
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

    /// Wherever the search lands, it must be looking for the file every
    /// layout installs - a typo here is a pipeline with no deconjugation
    /// and no error. (Which of the three candidates wins depends on the
    /// install; a test binary in `target/debug/deps` matches none of
    /// them, which is why this pins the name and not the directory.)
    #[test]
    fn the_rules_search_looks_for_the_file_this_build_ships() {
        assert!(rules_file().ends_with(RULES), "{}", rules_file().display());
    }

    /// And the file itself is real rules, not an empty array: this is
    /// the copy every layout installs.
    #[test]
    fn the_shipped_rules_parse_and_are_not_empty() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(RULES);
        let rules = load_rules(&repo).expect("the shipped deconjugation rules must parse");
        assert!(!rules.is_empty());
    }

    /// ADR-0009: "the Linux adapter never upscales" - native crops beat
    /// 2x on every benchmark slice, so the settings this daemon hands
    /// the shared Worker must carry factor 1. The OCR gate feeds meiki
    /// native fixtures directly and cannot catch a runtime factor
    /// drift; this pins it at the seam instead.
    #[test]
    fn the_linux_worker_never_upscales() {
        let s = settings(&Config::default(), &[]);
        assert_eq!(1, s.upscale);
        assert_eq!(1, s.snapshot().upscale, "and the snapshot carries it to TextSource");
    }

    /// `settings` used to hardcode `static_region: None` with a comment
    /// calling the overlay a Windows surface, so `SentenceMode::Static`
    /// could never do anything here however carefully the user drew a
    /// box. Core's `resolve_static` is platform-agnostic and reads only
    /// this field, so carrying it through is the whole feature — pinned
    /// at the one seam that builds `WorkerSettings` for this daemon.
    #[test]
    fn a_configured_static_region_reaches_the_worker_settings() {
        let mut config = Config::default();
        config.anki.sentence_mode = chibipop::config::SentenceMode::Static;
        config.anki.static_region = Some([120, 240, 800, 300]);

        let s = settings(&config, &[]);
        // Both halves, because `resolve_static` gates on the pair: the
        // mode alone falls through to line mode, and a region alone is
        // never read.
        assert_eq!(chibipop::config::SentenceMode::Static, s.sentence_mode);
        assert_eq!(Some(PhysRect { x: 120, y: 240, w: 800, h: 300 }), s.static_region);
    }

    /// The default: no box drawn, nothing invented. Core falls through
    /// to line mode on its own when the region is absent, so the honest
    /// answer here is `None` rather than a whole-screen stand-in.
    #[test]
    fn no_drawn_region_stays_absent_rather_than_becoming_the_screen() {
        assert_eq!(None, settings(&Config::default(), &[]).static_region);
    }

    /// The whole point of ticket 08: an unchecked row in the settings
    /// window's Terms section has to reach the pipeline. `settings` is the
    /// only place the daemon builds `WorkerSettings` (reload and spawn both
    /// go through it), so the scoping is pinned here at that seam.
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
        // The scope arrives as the exact names it was written with, and
        // it is the whole of what the pipeline searches.
        assert_eq!(vec!["大辞林　第四版".to_string()], cfg.terms);
        let keeps = |name: &str| chibipop::present::keeps_dict(name, &cfg.terms);
        assert!(keeps("大辞林　第四版"));
        assert!(!keeps("Jitendex.org [2026-07-09]"), "excluded, so not searched");
    }

    /// No split, no filter: the shipped default searches everything.
    ///
    /// A default config names no dictionary in either array, so every
    /// installed one is new and lands enabled.
    #[test]
    fn a_config_with_no_split_searches_every_dictionary() {
        let dicts = vec![DictInfo { dict_id: 1, name: "Jitendex.org".to_string() }];
        let cfg = settings(&Config::default(), &dicts).present_cfg;
        assert_eq!(vec!["Jitendex.org".to_string()], cfg.terms);
        assert!(chibipop::present::keeps_dict("Jitendex.org", &cfg.terms));
    }

    /// ADR-0012 hides `ocr.language` but keeps whatever is stored, so a
    /// config shared with a Windows install can name a language meikiocr
    /// does not read. There is no engine probe left to ask about that: the
    /// only question is whether the configured language was given a list
    /// of its own, and if it was, that list is what gets searched.
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

        // The same per-language entry, under a language it is not keyed
        // to: the global terms list decides instead, and an empty one
        // means every installed dictionary.
        config.ocr.language = "ja".to_string();
        let global = settings(&config, &dicts).present_cfg;
        assert_eq!(
            vec!["大辞林　第四版".to_string(), "Jitendex.org [2026-07-09]".to_string()],
            global.terms
        );
    }
}
