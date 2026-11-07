//! The commands that the page calls, and the types that cross the seam.
//!
//! `ui/types.d.ts` documents the serialized shape of every type here.

use crate::capture::{self, CaptureResult};
use crate::control::{self, ReloadOutcome};
use crate::fonts;
use crate::layout::LayoutView;
use crate::lock::ConfigLock;
use crate::session::Session;
use anyhow::Error;
use chibipop::anki;
use chibipop::config::{
    self, Bind, BindAction, Config, FieldMapping, Platform, ProfileCatalog, ResolvedConfig,
};
use chibipop::library::Library;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use chibipop::settings::{self, DictRow, SettingsForm};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::State;

const CAPTURE_LIMIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    Io,
    Parse,
    Dictionary,
    Validation,
    Stale,
    ReadOnly,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpcError {
    pub kind: ErrorKind,
    pub message: String,
}

impl IpcError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FormView {
    pub profile_id: String,
    pub profiles: Vec<ProfileRef>,
    pub cfg: ResolvedConfig,
    pub terms: Vec<DictRow>,
    pub frequency: Vec<DictRow>,
    pub pitch: Vec<DictRow>,
    pub dict_list_language: String,
    pub unreadable: Vec<String>,
    pub library_empty: bool,
    pub background_on_close: Option<bool>,
    pub binds: Vec<Bind>,
    pub stale: Vec<String>,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FormEdits {
    pub generation: u64,
    pub cfg: ResolvedConfig,
    pub terms: Vec<DictRow>,
    pub frequency: Vec<DictRow>,
    pub pitch: Vec<DictRow>,
    pub background_on_close: Option<bool>,
    pub binds: Vec<Bind>,
    pub field_map: Option<Vec<FieldMapping>>,
    pub screenshot_reset_targets: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnkiLists {
    pub decks: Vec<String>,
    pub models: Vec<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplyOutcomeView {
    Saved,
    Applied,
    NoReply,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApplyReport {
    pub saved_to: String,
    pub notices: Vec<String>,
    pub form: FormView,
    pub outcome: ApplyOutcomeView,
    pub reply: Option<String>,
}

struct Saved {
    saved_to: String,
    notices: Vec<String>,
    form: FormView,
}

struct Merged {
    form: SettingsForm,
    applied: settings::AppliedSettings,
    notices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CaptureResultView {
    Chord { chord: String },
    Cleared,
    Cancelled,
    Rejected { chord: String },
    TimedOut,
}

impl From<CaptureResult> for CaptureResultView {
    fn from(result: CaptureResult) -> Self {
        match result {
            CaptureResult::Chord { chord } => Self::Chord { chord },
            CaptureResult::Cleared => Self::Cleared,
            CaptureResult::Cancelled => Self::Cancelled,
            CaptureResult::Rejected { chord } => Self::Rejected { chord },
            CaptureResult::TimedOut => Self::TimedOut,
        }
    }
}

#[tauri::command]
pub fn load_layout() -> Result<LayoutView, IpcError> {
    LayoutView::embedded().map_err(|e| IpcError::new(ErrorKind::Parse, format!("{e:#}")))
}

#[tauri::command]
pub fn load_form(state: State<'_, Mutex<Session>>) -> Result<FormView, IpcError> {
    let mut session = lock(&state)?;
    load(&mut session)
}

#[tauri::command]
pub fn font_families() -> Result<Vec<String>, IpcError> {
    fonts::japanese_font_families()
}

#[tauri::command(async)]
pub fn save_form(
    state: State<'_, Mutex<Session>>,
    edits: FormEdits,
) -> Result<ApplyReport, IpcError> {
    let mut session = lock(&state)?;
    write(&mut session, edits, false)
}

#[tauri::command(async)]
pub fn apply_form(
    state: State<'_, Mutex<Session>>,
    edits: FormEdits,
) -> Result<ApplyReport, IpcError> {
    let mut session = lock(&state)?;
    write(&mut session, edits, true)
}

#[tauri::command(async)]
pub fn capture_chord() -> Result<CaptureResultView, IpcError> {
    capture::capture(CAPTURE_LIMIT)
        .map(CaptureResultView::from)
        .map_err(|e| IpcError::new(ErrorKind::Internal, e.to_string()))
}

#[tauri::command]
pub fn cancel_capture() {
    capture::cancel();
}

#[tauri::command]
pub fn new_bind(state: State<'_, Mutex<Session>>) -> Result<Bind, IpcError> {
    let mut session = lock(&state)?;
    add_bind(&mut session)
}

#[tauri::command(async)]
pub fn validate_form(
    state: State<'_, Mutex<Session>>,
    edits: FormEdits,
) -> Result<Vec<String>, IpcError> {
    let session = lock(&state)?;
    validate(&session, edits)
}

#[tauri::command(async)]
pub fn anki_refresh(url: String, model: String) -> Result<AnkiLists, IpcError> {
    anki_lists(&url, &model)
}

fn anki_lists(url: &str, model: &str) -> Result<AnkiLists, IpcError> {
    let io = |e: Error| IpcError::new(ErrorKind::Io, format!("{e:#}"));
    Ok(AnkiLists {
        decks: anki::deck_names(url).map_err(io)?,
        models: anki::model_names(url).map_err(io)?,
        fields: anki::model_field_names(url, model).map_err(io)?,
    })
}

fn add_bind(session: &mut Session) -> Result<Bind, IpcError> {
    let form = session
        .form
        .as_mut()
        .ok_or_else(|| IpcError::new(ErrorKind::Internal, "load the form first"))?;
    let id = form.catalog.next_bind_id();
    Ok(Bind::new(id, BindAction::Lookup))
}

fn check_current(session: &Session, edits: &FormEdits) -> Result<(), IpcError> {
    if session.form.is_none() {
        return Err(IpcError::new(ErrorKind::Internal, "load the form first"));
    }
    if edits.generation != session.generation {
        return Err(IpcError::new(
            ErrorKind::Stale,
            "the settings changed since this page loaded; reload the form",
        ));
    }
    Ok(())
}

fn validate(session: &Session, edits: FormEdits) -> Result<Vec<String>, IpcError> {
    check_current(session, &edits)?;
    let _guard = ConfigLock::acquire(&session.config_path)?;
    let fresh = config::load_or_create(&session.config_path).map_err(config_error)?;
    merge_edits(session, &fresh, edits).map(|merged| merged.notices)
}

fn write(session: &mut Session, edits: FormEdits, reload: bool) -> Result<ApplyReport, IpcError> {
    check_current(session, &edits)?;
    let saved = {
        let _guard = ConfigLock::acquire(&session.config_path)?;
        save_locked(session, edits)?
    };
    let (outcome, reply) = if reload {
        match control::send_reload(&session.config_path) {
            ReloadOutcome::Live { reply } => (ApplyOutcomeView::Applied, Some(reply)),
            ReloadOutcome::ConfigOnly => (ApplyOutcomeView::Saved, None),
            ReloadOutcome::NoReply => (ApplyOutcomeView::NoReply, None),
        }
    } else {
        (ApplyOutcomeView::Saved, None)
    };
    Ok(ApplyReport {
        saved_to: saved.saved_to,
        notices: saved.notices,
        form: saved.form,
        outcome,
        reply,
    })
}

fn save_locked(session: &mut Session, edits: FormEdits) -> Result<Saved, IpcError> {
    let fresh = config::load_or_create(&session.config_path).map_err(config_error)?;
    let Merged {
        mut form,
        applied,
        notices,
    } = merge_edits(session, &fresh, edits)?;
    applied.config.save(&session.config_path).map_err(|e| {
        IpcError::new(
            ErrorKind::Io,
            format!("writing {}: {e:#}", session.config_path.display()),
        )
    })?;
    session.generation += 1;
    form.accept_applied(applied, &session.dicts).map_err(|e| {
        IpcError::new(ErrorKind::Internal, format!("saved; reload the form: {e:#}"))
    })?;
    let view = view_of(&form, &form.catalog, &session.dicts, session.generation);
    session.form = Some(form);
    Ok(Saved {
        saved_to: session.config_path.display().to_string(),
        notices,
        form: view,
    })
}

// Writes nothing. `fresh` is the config that the caller loaded.
fn merge_edits(session: &Session, fresh: &Config, edits: FormEdits) -> Result<Merged, IpcError> {
    let mut form = session
        .form
        .clone()
        .ok_or_else(|| IpcError::new(ErrorKind::Internal, "load the form first"))?;
    form.cfg = edits.cfg;
    form.terms = edits.terms;
    form.frequency = edits.frequency;
    form.pitch = edits.pitch;
    form.background_on_close = edits.background_on_close;
    form.catalog.binds = edits.binds;
    form.field_map = edits.field_map;
    form.screenshot_reset_targets = edits.screenshot_reset_targets;
    let applied = settings::apply_to(&form, fresh);
    let mut before = fresh.clone();
    before.migrate_dictionary_lists(&session.dicts);
    if settings::dictionary_work(&before, &applied.config) != settings::DictionaryWork::None {
        return Err(IpcError::new(
            ErrorKind::ReadOnly,
            "this change needs a dictionary reindex, which this build lacks",
        ));
    }
    let validation = |e: Error| IpcError::new(ErrorKind::Validation, format!("{e:#}"));
    let resolved = applied
        .config
        .resolved(Some(&applied.profile_id))
        .map_err(validation)?;
    applied
        .config
        .validate_hotkeys(Platform::Windows)
        .map_err(validation)?;
    let notices: Vec<String> = settings::clamp_notice(&form, &resolved).into_iter().collect();
    Ok(Merged {
        form,
        applied,
        notices,
    })
}

fn lock(session: &Mutex<Session>) -> Result<MutexGuard<'_, Session>, IpcError> {
    session
        .lock()
        .map_err(|_| IpcError::new(ErrorKind::Internal, "the session lock is poisoned"))
}

fn load(session: &mut Session) -> Result<FormView, IpcError> {
    let cfg = config::load_or_create(&session.config_path).map_err(config_error)?;
    let dicts = read_dicts(&session.dict_path)?;
    ProfileCatalog::new(&cfg, &dicts)
        .map_err(|e| IpcError::new(ErrorKind::Validation, format!("{e:#}")))?;
    let form = form_with_library(&cfg, &dicts, &library_dir());
    let view = view_of(&form, &cfg, &dicts, session.generation);
    session.form = Some(form);
    session.dicts = dicts;
    Ok(view)
}

fn read_dicts(path: &Path) -> Result<Vec<DictInfo>, IpcError> {
    // The Dictionary does not exist on a first run.
    if !path.exists() {
        return Ok(Vec::new());
    }
    let open_error = |e: Error| {
        IpcError::new(
            ErrorKind::Dictionary,
            format!("opening {}: {e:#}", path.display()),
        )
    };
    SqliteDictionary::open(path)
        .and_then(|dictionary| dictionary.dicts())
        .map_err(open_error)
}

fn form_with_library(cfg: &Config, dicts: &[DictInfo], dir: &Path) -> SettingsForm {
    let form = settings::from_config(cfg, dicts);
    match Library::load(dir) {
        Ok(lib) => settings::with_library(form, &lib),
        Err(e) => {
            eprintln!("chibipop-settings: reading {} failed: {e:#}", dir.display());
            form
        }
    }
}

fn library_dir() -> PathBuf {
    chibipop::paths::beside_exe("library")
}

fn view_of(form: &SettingsForm, cfg: &Config, dicts: &[DictInfo], generation: u64) -> FormView {
    FormView {
        profile_id: form.profile_id.clone(),
        profiles: form
            .catalog
            .profiles
            .iter()
            .map(|p| ProfileRef {
                id: p.id.clone(),
                name: p.name.clone(),
            })
            .collect(),
        cfg: form.cfg.clone(),
        terms: form.terms.clone(),
        frequency: form.frequency.clone(),
        pitch: form.pitch.clone(),
        dict_list_language: form.dict_list_language.clone(),
        unreadable: form.unreadable.clone(),
        library_empty: form.library_empty,
        background_on_close: form.background_on_close,
        binds: form.catalog.binds.clone(),
        stale: settings::stale_order_entries(cfg, dicts),
        generation,
    }
}

fn config_error(e: Error) -> IpcError {
    let kind = if e.chain().any(|c| c.is::<toml::de::Error>()) {
        ErrorKind::Parse
    } else if e.chain().any(|c| c.is::<std::io::Error>()) {
        ErrorKind::Io
    } else {
        ErrorKind::Validation
    };
    IpcError::new(kind, format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chibipop::config::{LayoutMode, ProfileData, ScreenshotWindow, TriggerMode};
    use chibipop::dict::frequency::RankingStrategy;
    use chibipop::present::DictInfo;
    use std::sync::Barrier;

    fn temp_config(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chibipop-ipc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        dir.join("chibipop.toml")
    }

    fn session_at(path: &Path) -> Session {
        let cfg = config::load_or_create(path).expect("config should load");
        let mut session = Session::new(path.to_path_buf(), PathBuf::new());
        session.form = Some(settings::from_config(&cfg, &[]));
        session
    }

    fn edits_of(session: &Session) -> FormEdits {
        let form = session.form.as_ref().expect("form should be loaded");
        FormEdits {
            generation: session.generation,
            cfg: form.cfg.clone(),
            terms: form.terms.clone(),
            frequency: form.frequency.clone(),
            pitch: form.pitch.clone(),
            background_on_close: form.background_on_close,
            binds: form.catalog.binds.clone(),
            field_map: None,
            screenshot_reset_targets: false,
        }
    }

    fn saved(path: &Path) -> ResolvedConfig {
        let cfg = config::load_or_create(path).expect("config should load");
        cfg.resolved(None).expect("config should resolve")
    }

    fn saved_popup(path: &Path) -> chibipop::config::PopupConfig {
        let cfg = config::load_or_create(path).expect("config should load");
        cfg.resolved(None).expect("config should resolve").popup
    }

    #[test]
    fn concurrent_saves_both_survive() {
        let path = temp_config("concurrent");
        for _ in 0..20 {
            Config::default().save(&path).expect("reset should save");
            let mut left = session_at(&path);
            let mut right = session_at(&path);
            let mut left_edits = edits_of(&left);
            let mut right_edits = edits_of(&right);
            left_edits.cfg.popup.highlight_match = !left_edits.cfg.popup.highlight_match;
            right_edits.cfg.popup.layout_mode = LayoutMode::Compact;
            let start = Barrier::new(2);
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    start.wait();
                    write(&mut left, left_edits.clone(), false).expect("left save should work");
                });
                scope.spawn(|| {
                    start.wait();
                    write(&mut right, right_edits.clone(), false).expect("right save should work");
                });
            });
            let popup = saved_popup(&path);
            assert_eq!(popup.highlight_match, left_edits.cfg.popup.highlight_match);
            assert_eq!(popup.layout_mode, LayoutMode::Compact);
        }
    }

    #[test]
    fn stale_generation_rejected() {
        let path = temp_config("stale");
        let mut session = session_at(&path);
        let edits = edits_of(&session);
        session.generation = 1;
        let before = std::fs::read_to_string(&path).expect("file should read");
        let err = write(&mut session, edits, false).expect_err("a stale page must be rejected");
        assert_eq!(err.kind, ErrorKind::Stale);
        assert_eq!(std::fs::read_to_string(&path).expect("file should read"), before);
    }

    #[test]
    fn failed_save_keeps_file() {
        let path = temp_config("failed");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        edits.cfg.popup.highlight_match = !edits.cfg.popup.highlight_match;
        std::fs::create_dir(path.with_extension("toml.tmp")).expect("blocker should be created");
        let before = std::fs::read_to_string(&path).expect("file should read");
        let err = write(&mut session, edits, false).expect_err("the save must fail");
        assert_eq!(err.kind, ErrorKind::Io);
        assert_eq!(std::fs::read_to_string(&path).expect("file should read"), before);
        assert_eq!(session.generation, 0);
    }

    #[test]
    fn validation_blocks_save() {
        let path = temp_config("validation");
        let mut config = Config::default();
        assert!(config.binds.len() >= 2, "the default config should hold two binds");
        for bind in config.binds.iter_mut().take(2) {
            bind.windows = "Ctrl+Shift+F9".to_string();
            bind.enabled = true;
        }
        config.save(&path).expect("duplicate chords still save");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        edits.cfg.popup.highlight_match = !edits.cfg.popup.highlight_match;
        let before = std::fs::read_to_string(&path).expect("file should read");
        let err = write(&mut session, edits, false).expect_err("a duplicate chord must fail");
        assert_eq!(err.kind, ErrorKind::Validation);
        assert_eq!(std::fs::read_to_string(&path).expect("file should read"), before);
    }

    #[test]
    fn reindex_edits_stay_read_only() {
        let path = temp_config("reindex");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        edits.frequency.push(DictRow {
            name: "frequency".to_string(),
            enabled: true,
        });
        let err = write(&mut session, edits, false).expect_err("a reindex edit must be rejected");
        assert_eq!(err.kind, ErrorKind::ReadOnly);
    }

    #[test]
    fn ranking_strategy_change_stays_read_only() {
        let path = temp_config("ranking");
        let mut session = session_at(&path);
        let before = std::fs::read_to_string(&path).expect("file should read");
        let mut edits = edits_of(&session);
        let next = match edits.cfg.dictionaries.ranking_strategy {
            RankingStrategy::Priority => RankingStrategy::Median,
            _ => RankingStrategy::Priority,
        };
        edits.cfg.dictionaries.ranking_strategy = next;
        let err = write(&mut session, edits, false)
            .expect_err("a ranking strategy change needs a reindex");
        assert_eq!(err.kind, ErrorKind::ReadOnly);
        assert_eq!(
            std::fs::read_to_string(&path).expect("file should read"),
            before
        );
    }

    #[test]
    fn terms_edit_saves() {
        let path = temp_config("terms");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        edits.terms.push(DictRow {
            name: "Terms".to_string(),
            enabled: true,
        });
        write(&mut session, edits, false).expect("a terms edit needs no reindex");
        assert_eq!(saved(&path).dictionaries.terms, vec!["Terms".to_string()]);
    }

    #[test]
    fn legacy_config_still_saves() {
        let path = temp_config("legacy");
        let legacy = "[trigger]\n[popup]\n[dictionaries]\nfrequency = [\"Freq\"]\n";
        std::fs::write(&path, legacy).expect("legacy file should write");
        let dicts = vec![
            DictInfo {
                dict_id: 1,
                name: "Freq".to_string(),
            },
            DictInfo {
                dict_id: 2,
                name: "Other".to_string(),
            },
        ];
        let cfg = config::load_or_create(&path).expect("legacy config should load");
        let mut session = Session::new(path.clone(), PathBuf::new());
        session.form = Some(settings::from_config(&cfg, &dicts));
        session.dicts = dicts;
        let mut edits = edits_of(&session);
        edits.cfg.popup.highlight_match = !edits.cfg.popup.highlight_match;
        let flipped = edits.cfg.popup.highlight_match;
        write(&mut session, edits, false).expect("an edit that touches no Dictionary must save");
        assert_eq!(saved_popup(&path).highlight_match, flipped);
    }

    #[test]
    fn shared_branch_round_trips() {
        let path = temp_config("shared");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        edits.background_on_close = Some(true);
        edits.cfg.debug.show_engine_log = true;
        write(&mut session, edits, false).expect("a shared edit should save");
        let cfg = config::load_or_create(&path).expect("config should load");
        assert!(cfg.application.background_on_close);
        assert!(cfg.debug.show_engine_log);
    }

    #[test]
    fn trigger_mode_writes_live_lookup() {
        let path = temp_config("trigger");
        let mut session = session_at(&path);
        assert!(config::load_or_create(&path).expect("config should load").live_lookup);
        let mut edits = edits_of(&session);
        edits.cfg.trigger.mode = TriggerMode::Press;
        write(&mut session, edits, false).expect("a trigger mode change should save");
        assert!(!config::load_or_create(&path).expect("config should load").live_lookup);
    }

    #[test]
    fn screenshot_reset_clears_targets() {
        let path = temp_config("screenshot");
        let mut config = Config::default();
        let ProfileData::Full { settings } = &mut config.profiles[0].data else {
            panic!("the default profile should be Full");
        };
        settings.actions.screenshot.fixed_region = Some([1, 2, 3, 4]);
        settings.actions.screenshot.fixed_window = Some(ScreenshotWindow {
            app_id: "reader".to_string(),
            title: "book".to_string(),
        });
        config.save(&path).expect("targets should save");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        assert!(edits.cfg.actions.screenshot.fixed_region.is_some());
        edits.screenshot_reset_targets = true;
        write(&mut session, edits, false).expect("a reset should save");
        let shot = saved(&path).actions.screenshot;
        assert_eq!(shot.fixed_region, None);
        assert_eq!(shot.fixed_window, None);
    }

    #[test]
    fn anki_refresh_reports_io() {
        let path = temp_config("anki");
        let mut config = Config::default();
        let ProfileData::Full { settings } = &mut config.profiles[0].data else {
            panic!("the default profile should be Full");
        };
        let map = vec![FieldMapping {
            anki_field: "Front".to_string(),
            source: "glossary".to_string(),
        }];
        settings.anki.field_map = map.clone();
        config.save(&path).expect("field map should save");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a port should bind");
        let port = listener.local_addr().expect("address should read").port();
        drop(listener);
        let err = anki_lists(&format!("http://127.0.0.1:{port}"), "Lapis")
            .expect_err("no server answers on the port");
        assert_eq!(err.kind, ErrorKind::Io);
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        edits.cfg.popup.highlight_match = !edits.cfg.popup.highlight_match;
        write(&mut session, edits, false).expect("a save after a failed refresh should work");
        assert_eq!(saved(&path).anki.field_map, map);
    }

    #[test]
    fn new_bind_ids_are_unique() {
        let path = temp_config("newbind");
        let mut session = session_at(&path);
        let before = session.form.as_ref().expect("form should load").catalog.next_id;
        let first = add_bind(&mut session).expect("a bind should be created");
        let second = add_bind(&mut session).expect("a bind should be created");
        assert_ne!(first.id, second.id);
        let after = session.form.as_ref().expect("form should load").catalog.next_id;
        assert!(after >= before + 2, "next_id must advance for each call");
    }

    #[test]
    fn validate_form_reports_a_conflict() {
        let path = temp_config("conflict");
        let session = session_at(&path);
        let mut edits = edits_of(&session);
        let mut extra = Bind::new("extra".to_string(), BindAction::Lookup);
        extra.windows = "f9".to_string();
        edits.binds.push(extra);
        let lookup = edits
            .binds
            .iter_mut()
            .find(|bind| bind.id == "lookup")
            .expect("the default config should hold a lookup bind");
        lookup.windows = "f9".to_string();
        lookup.enabled = true;
        let before = std::fs::read_to_string(&path).expect("file should read");
        let err = validate(&session, edits).expect_err("two Lookup binds on f9 must conflict");
        assert_eq!(err.kind, ErrorKind::Validation);
        assert_eq!(std::fs::read_to_string(&path).expect("file should read"), before);
    }

    #[test]
    fn bind_row_saves() {
        let path = temp_config("bindrow");
        let mut session = session_at(&path);
        let mut edits = edits_of(&session);
        let row = edits
            .binds
            .iter_mut()
            .find(|bind| bind.id == "lookup")
            .expect("the default config should hold a lookup bind");
        row.windows = "ctrl+f9".to_string();
        row.enabled = true;
        write(&mut session, edits, false).expect("a bind row edit should save");
        let cfg = config::load_or_create(&path).expect("config should load");
        let saved = cfg
            .binds
            .iter()
            .find(|bind| bind.id == "lookup")
            .expect("the lookup bind should persist");
        assert_eq!(saved.windows, "ctrl+f9");
        assert!(saved.enabled);
    }
}
