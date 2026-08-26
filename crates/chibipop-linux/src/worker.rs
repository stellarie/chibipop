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
use chibipop::config::Config;
use chibipop::geom::ScanDisplay;
use chibipop::lookup::deconj::Deconjugator;
use chibipop::lookup::engine::LookupEngine;
use chibipop::lookup::model::{Dictionary, Entry, TermRow};
use chibipop::lookup::rules::load_rules;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use chibipop::text::layout::CaptureSize;
use chibipop::worker::{Worker, WorkerParts, WorkerSettings};
use chibipop_linux::ocr::MeikiOcr;
use std::path::{Path, PathBuf};

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

/// What the Worker owns, from the config the daemon has loaded.
pub fn settings(config: &Config, dicts: &[DictInfo]) -> WorkerSettings {
    WorkerSettings {
        max_passes: config.ocr.max_ocr_passes,
        prefer_vertical: config.ocr.prefer_vertical,
        capture: CaptureSize { w: config.ocr.capture_width, h: config.ocr.capture_height },
        scan_alphanumeric: config.ocr.scan_alphanumeric,
        language: config.ocr.language.clone(),
        present_cfg: config.present_config(),
        scan_display: ScanDisplay {
            captures: config.debug.show_scan_region,
            highlight: config.popup.highlight_match,
        },
        dicts: dicts.to_vec(),
    }
}

/// Spawn the pipeline; answers the dictionary identities it read.
///
/// `portal` is the session the daemon's eager consent already opened
/// (ADR-0002 rung 2), handed over because the Worker thread is what
/// reads through it. The screencopy rung needs nothing here: it binds
/// its own connection inside the closure, on that thread.
pub fn spawn(
    setup: &Setup,
    settings: WorkerSettings,
    portal: Option<PortalCapture>,
    ping: calloop::ping::Ping,
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
            })
        },
        move || ping.ping(),
    )
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
}
