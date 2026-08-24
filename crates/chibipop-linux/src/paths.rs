//! XDG path mapping with portable mode (ADR-0006).
//!
//! Discovery order, first match wins, never dual-read:
//!
//! 1. `--config <path>` — that exact file is the config; every other
//!    directory resolves by XDG below. An explicit flag is a config
//!    override, not a relayout (matches the Windows bin, where `--config`
//!    moves only the config file).
//! 2. Portable mode — `chibipop.toml` beside the exe ⇒ everything beside
//!    the exe, preserving the Windows portable identity and the AppImage
//!    convention.
//! 3. XDG — `$XDG_CONFIG_HOME/chibipop/chibipop.toml` and friends.
//!
//! The runtime dir (lock + control socket) is `$XDG_RUNTIME_DIR/chibipop`
//! in every mode: sockets don't belong beside an exe — the portable dir
//! may be a read-only AppImage mount, and flock/socket semantics on
//! network mounts are exactly the trouble XDG_RUNTIME_DIR exists to avoid.

use std::path::{Path, PathBuf};

/// The config file name every mode looks for.
pub const CONFIG_FILE: &str = "chibipop.toml";

/// Which rung of the discovery ladder chose the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Explicit,
    Portable,
    Xdg,
}

impl Mode {
    pub fn describe(self) -> &'static str {
        match self {
            Mode::Explicit => "explicit (--config)",
            Mode::Portable => "portable (config beside exe)",
            Mode::Xdg => "xdg",
        }
    }
}

/// Everywhere the daemon reads or writes, resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub mode: Mode,
    pub config_file: PathBuf,
    /// Library archives + built DB (data, not cache — rebuilds are expensive).
    pub data_dir: PathBuf,
    /// The truncate-on-start logfile.
    pub state_dir: PathBuf,
    /// Update downloads / OCR scratch.
    pub cache_dir: PathBuf,
    /// Lock + control socket; `None` when `$XDG_RUNTIME_DIR` is unset.
    pub runtime_dir: Option<PathBuf>,
}

impl Paths {
    /// The lock/socket directory, or the one clear error about it.
    pub fn runtime_dir(&self) -> anyhow::Result<&Path> {
        self.runtime_dir.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "XDG_RUNTIME_DIR is unset; chibipop needs it for its \
                 instance lock and control socket (log in with a session \
                 manager, or export it to a private tmpfs directory)"
            )
        })
    }

    pub fn log_file(&self) -> PathBuf {
        self.state_dir.join("chibipop.log")
    }
}

/// The environment snapshot resolution reads: a plain struct so tests
/// inject values instead of racing over process-global env vars.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub exe_dir: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_state_home: Option<PathBuf>,
    pub xdg_cache_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
}

impl Env {
    pub fn from_process() -> Env {
        Env {
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
            home: std::env::var_os("HOME").map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            xdg_cache_home: std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
        }
    }
}

/// Resolve the whole layout. Reads the filesystem only to probe for a
/// beside-exe `chibipop.toml` (the portable trigger).
pub fn resolve(env: &Env, explicit_config: Option<PathBuf>) -> Paths {
    let runtime_dir = xdg(env.xdg_runtime_dir.as_deref()).map(|d| d.join("chibipop"));

    if let Some(config_file) = explicit_config {
        let mut paths = xdg_paths(env, runtime_dir);
        paths.mode = Mode::Explicit;
        paths.config_file = config_file;
        return paths;
    }

    if let Some(exe_dir) = env.exe_dir.as_deref() {
        let beside = exe_dir.join(CONFIG_FILE);
        if beside.is_file() {
            return Paths {
                mode: Mode::Portable,
                config_file: beside,
                data_dir: exe_dir.join("data"),
                // The log lands directly beside the exe, like Windows.
                state_dir: exe_dir.to_path_buf(),
                cache_dir: exe_dir.join("cache"),
                runtime_dir,
            };
        }
    }

    xdg_paths(env, runtime_dir)
}

/// The XDG rung: `$XDG_*` with spec defaults under `$HOME`.
fn xdg_paths(env: &Env, runtime_dir: Option<PathBuf>) -> Paths {
    let base = |var: Option<&Path>, default: &str| -> PathBuf {
        xdg(var)
            .map(Path::to_path_buf)
            .or_else(|| env.home.as_deref().map(|h| h.join(default)))
            .unwrap_or_else(|| PathBuf::from(default))
            .join("chibipop")
    };
    let config_dir = base(env.xdg_config_home.as_deref(), ".config");
    Paths {
        mode: Mode::Xdg,
        config_file: config_dir.join(CONFIG_FILE),
        data_dir: base(env.xdg_data_home.as_deref(), ".local/share"),
        state_dir: base(env.xdg_state_home.as_deref(), ".local/state"),
        cache_dir: base(env.xdg_cache_home.as_deref(), ".cache"),
        runtime_dir,
    }
}

/// Per the basedir spec, a relative `$XDG_*` value is invalid: ignore it.
fn xdg(value: Option<&Path>) -> Option<&Path> {
    value.filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_home() -> Env {
        Env {
            home: Some(PathBuf::from("/home/u")),
            ..Env::default()
        }
    }

    fn tmp_exe_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chibipop_paths_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bare_env_falls_back_to_home_defaults() {
        let p = resolve(&env_with_home(), None);
        assert_eq!(Mode::Xdg, p.mode);
        assert_eq!(PathBuf::from("/home/u/.config/chibipop/chibipop.toml"), p.config_file);
        assert_eq!(PathBuf::from("/home/u/.local/share/chibipop"), p.data_dir);
        assert_eq!(PathBuf::from("/home/u/.local/state/chibipop"), p.state_dir);
        assert_eq!(PathBuf::from("/home/u/.cache/chibipop"), p.cache_dir);
        assert_eq!(None, p.runtime_dir);
    }

    #[test]
    fn xdg_vars_win_over_home_defaults() {
        let mut env = env_with_home();
        env.xdg_config_home = Some(PathBuf::from("/cfg"));
        env.xdg_state_home = Some(PathBuf::from("/st"));
        env.xdg_runtime_dir = Some(PathBuf::from("/run/user/1"));
        let p = resolve(&env, None);
        assert_eq!(PathBuf::from("/cfg/chibipop/chibipop.toml"), p.config_file);
        assert_eq!(PathBuf::from("/st/chibipop"), p.state_dir);
        assert_eq!(Some(PathBuf::from("/run/user/1/chibipop")), p.runtime_dir);
    }

    /// The basedir spec says a relative value is invalid.
    #[test]
    fn a_relative_xdg_value_is_ignored() {
        let mut env = env_with_home();
        env.xdg_config_home = Some(PathBuf::from("relative/cfg"));
        let p = resolve(&env, None);
        assert_eq!(PathBuf::from("/home/u/.config/chibipop/chibipop.toml"), p.config_file);
    }

    #[test]
    fn a_config_beside_the_exe_relocates_everything_beside_it() {
        let exe_dir = tmp_exe_dir("portable");
        std::fs::write(exe_dir.join(CONFIG_FILE), "").unwrap();
        let mut env = env_with_home();
        env.exe_dir = Some(exe_dir.clone());
        env.xdg_runtime_dir = Some(PathBuf::from("/run/user/1"));
        let p = resolve(&env, None);
        assert_eq!(Mode::Portable, p.mode);
        assert_eq!(exe_dir.join(CONFIG_FILE), p.config_file);
        assert_eq!(exe_dir.join("data"), p.data_dir);
        assert_eq!(exe_dir, p.state_dir);
        assert_eq!(exe_dir.join("cache"), p.cache_dir);
        // The runtime dir never moves beside the exe.
        assert_eq!(Some(PathBuf::from("/run/user/1/chibipop")), p.runtime_dir);
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    #[test]
    fn no_beside_exe_config_means_xdg() {
        let exe_dir = tmp_exe_dir("xdg");
        let mut env = env_with_home();
        env.exe_dir = Some(exe_dir.clone());
        let p = resolve(&env, None);
        assert_eq!(Mode::Xdg, p.mode);
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    /// The flag overrides the config file only — no relayout, no probing.
    #[test]
    fn an_explicit_flag_beats_a_portable_layout() {
        let exe_dir = tmp_exe_dir("explicit");
        std::fs::write(exe_dir.join(CONFIG_FILE), "").unwrap();
        let mut env = env_with_home();
        env.exe_dir = Some(exe_dir.clone());
        let p = resolve(&env, Some(PathBuf::from("/etc/chibipop.toml")));
        assert_eq!(Mode::Explicit, p.mode);
        assert_eq!(PathBuf::from("/etc/chibipop.toml"), p.config_file);
        assert_eq!(PathBuf::from("/home/u/.local/state/chibipop"), p.state_dir);
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    #[test]
    fn a_missing_runtime_dir_is_one_clear_error() {
        let p = resolve(&env_with_home(), None);
        let msg = p.runtime_dir().unwrap_err().to_string();
        assert!(msg.contains("XDG_RUNTIME_DIR"), "{msg}");
    }
}
