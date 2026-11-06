//! The commands that the page calls, and the types that cross the seam.
//!
//! `ui/types.d.ts` documents the serialized shape of every type here.

use crate::fonts;
use crate::layout::LayoutView;
use crate::session::Session;
use anyhow::Error;
use chibipop::config::{self, Config, ProfileCatalog, ResolvedConfig};
use chibipop::library::Library;
use chibipop::lookup::model::Dictionary;
use chibipop::lookup::sqlite::SqliteDictionary;
use chibipop::present::DictInfo;
use chibipop::settings::{self, DictRow, SettingsForm};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use tauri::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    Io,
    Parse,
    Dictionary,
    Validation,
    // Stage 2 raises this on a stale generation.
    #[allow(dead_code)]
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
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApplyReport {
    pub saved_to: String,
    pub notices: Vec<String>,
    pub form: FormView,
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

#[tauri::command]
pub fn save_form(
    _state: State<'_, Mutex<Session>>,
    edits: FormEdits,
) -> Result<ApplyReport, IpcError> {
    write(edits)
}

#[tauri::command]
pub fn apply_form(
    _state: State<'_, Mutex<Session>>,
    edits: FormEdits,
) -> Result<ApplyReport, IpcError> {
    write(edits)
}

fn write(_edits: FormEdits) -> Result<ApplyReport, IpcError> {
    Err(IpcError::new(
        ErrorKind::ReadOnly,
        "the settings page is read-only in this build",
    ))
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

    #[test]
    fn stage_one_rejects_writes() {
        let cfg = Config::default()
            .resolved(None)
            .expect("default config should resolve");
        let edits = FormEdits {
            generation: 0,
            cfg,
            terms: Vec::new(),
            frequency: Vec::new(),
            pitch: Vec::new(),
            background_on_close: None,
        };
        let err = write(edits).expect_err("stage one must reject writes");
        assert_eq!(err.kind, ErrorKind::ReadOnly);
    }
}
