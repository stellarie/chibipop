//! The state that the page commands share.

use chibipop::present::DictInfo;
use chibipop::settings::SettingsForm;
use std::path::PathBuf;

pub struct Session {
    pub form: Option<SettingsForm>,
    pub config_path: PathBuf,
    pub dict_path: PathBuf,
    pub dicts: Vec<DictInfo>,
    pub generation: u64,
}

impl Session {
    pub fn new(config_path: PathBuf, dict_path: PathBuf) -> Self {
        Self {
            form: None,
            config_path,
            dict_path,
            dicts: Vec::new(),
            generation: 0,
        }
    }
}
