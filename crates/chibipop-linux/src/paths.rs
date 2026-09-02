//! XDG path mapping with portable mode.
//! See ARCHITECTURE.md#platform-integration.
//!
//! The discovery order follows. The first match wins. The daemon never
//! reads two layouts:
//!
//! 1. `--config <path>` — this exact file is the config. Every other
//!    directory resolves by XDG below. An explicit flag overrides the
//!    config file and does not change the layout. The Windows binary
//!    does the same, where `--config` moves only the config file.
//! 2. Portable mode — a `chibipop.toml` beside the exe puts every
//!    directory beside the exe. This keeps the Windows portable identity
//!    and the AppImage convention.
//! 3. XDG — `$XDG_CONFIG_HOME/chibipop/chibipop.toml` and the matching
//!    XDG directories.
//!
//! The runtime directory holds the lock and the control socket. This
//! directory is `$XDG_RUNTIME_DIR/chibipop` in every mode. A socket does
//! not belong beside an exe. The portable directory can be a read-only
//! AppImage mount. Also, flock and socket semantics on network mounts
//! are the exact trouble that XDG_RUNTIME_DIR prevents.

use std::path::{Path, PathBuf};

/// The config file name that every mode reads.
pub const CONFIG_FILE: &str = "chibipop.toml";

/// The rung of the discovery ladder that chose the layout.
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

/// Every path that the daemon reads or writes. Startup resolves them once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub mode: Mode,
    pub config_file: PathBuf,
    /// Library archives and the built database. A rebuild costs time.
    /// Therefore, these files are data and not cache.
    pub data_dir: PathBuf,
    /// The log file that truncates at start.
    pub state_dir: PathBuf,
    /// Update downloads and OCR scratch files.
    pub cache_dir: PathBuf,
    /// The lock and the control socket. `None` when `$XDG_RUNTIME_DIR` is unset.
    pub runtime_dir: Option<PathBuf>,
}

impl Paths {
    /// The lock and socket directory, or one clear error about it.
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

    /// The directory that `actions.screenshot.save_dir` resolves to.
    ///
    /// This function keeps an absolute value without a change. A user
    /// who typed a path meant that path. A relative value resolves
    /// **beside the exe in portable mode** and **under `data_dir` in
    /// every other mode**.
    ///
    /// The portable half diverges from XDG on purpose, for parity.
    /// Windows always joins a relative `save_dir` onto the exe
    /// directory. See `crates/chibipop-windows/src/app.rs:1479-1483` and
    /// `README.md`. Portable mode promises that a copied folder carries
    /// every file with it. See ARCHITECTURE.md#platform-integration.
    /// Screenshots in `~/.local/share` would leave part of the data of
    /// the user on the machine. Under XDG, the default `screenshots`
    /// directory holds user *data* and not cache. A screenshot is the
    /// picture that a card names. The loss of the picture breaks the
    /// card.
    pub fn screenshots_dir(&self, save_dir: &str) -> PathBuf {
        let save_dir = Path::new(save_dir);
        if save_dir.is_absolute() {
            return save_dir.to_path_buf();
        }
        match self.mode {
            // Portable mode means that the config file sits beside the
            // exe. See the module header. Therefore, the parent of the
            // config is the exe directory. This code needs no second
            // probe of the environment and no knowledge of the log path.
            Mode::Portable => match self.config_file.parent() {
                Some(exe_dir) => exe_dir.join(save_dir),
                // No caller reaches this arm. `resolve` selects
                // Portable only through `exe_dir.join(CONFIG_FILE)`.
                // Return a relative path and not a panic, because a
                // screenshot path does not justify a crash.
                None => save_dir.to_path_buf(),
            },
            Mode::Explicit | Mode::Xdg => self.data_dir.join(save_dir),
        }
    }
}

/// The environment snapshot that resolution reads. This plain struct
/// lets a test inject values and not race over process-global env vars.
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

/// Resolve the whole layout. This function reads the filesystem only to
/// probe for a `chibipop.toml` beside the exe, the portable trigger.
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

/// The XDG config *root* itself (`$XDG_CONFIG_HOME` or `~/.config`, no
/// `chibipop/` suffix): where spec-owned directories such as
/// `autostart/` live. `None` when neither variable can supply one.
pub fn config_home(env: &Env) -> Option<PathBuf> {
    xdg(env.xdg_config_home.as_deref())
        .map(Path::to_path_buf)
        .or_else(|| env.home.as_deref().map(|h| h.join(".config")))
}

/// The bare command name, the only guess left when the running exe
/// cannot be identified.
pub const COMMAND: &str = "chibipop";

/// The binary a generated launcher or command snippet should name: the
/// AppImage itself when running from one (`current_exe` inside an
/// AppImage points into a `/tmp` mount that is gone by the next login),
/// otherwise this exe.
///
/// Lives here rather than in `settings::autostart` because two features
/// need the same answer: the autostart entry's `Exec`, and every
/// compositor bind snippet the settings window hands out. Under
/// `cargo run` the binary is `target/debug/chibipop` and is not on
/// PATH, so a snippet naming the bare command execs nothing.
pub fn exec_path() -> std::io::Result<PathBuf> {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        let appimage = PathBuf::from(appimage);
        if appimage.is_absolute() {
            return Ok(appimage);
        }
    }
    std::env::current_exe()
}

/// [`exec_path`] for text a user will paste, with [`COMMAND`] as the
/// last resort. `current_exe` can genuinely fail (an unreadable or
/// deleted `/proc/self/exe`), and on such a host the bare name is the
/// only guess left: sometimes wrong, always better than an empty word
/// in a bind line.
pub fn exec_name() -> PathBuf {
    exec_path().unwrap_or_else(|_| PathBuf::from(COMMAND))
}

/// Whether a path can stand in a `sh` command line as itself.
///
/// Deliberately narrow: only characters that are literal to every
/// POSIX shell in every context. Anything else — a space, which a
/// checkout under a human-named directory really has, a glob
/// character, a quote — sends the path through [`shell_quote`].
fn is_bare_word(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '+' | ':' | '-'))
}

/// A path as one shell word, for command text we hand a user or a
/// compositor. Every consumer of that text (a Hyprland `exec`
/// dispatcher, a sway `exec`, a shell) splits on whitespace and then
/// runs `/bin/sh`, so the quoting rule is the shell's: wrap in single
/// quotes, inside which every character is literal, and splice an
/// embedded `'` back in as `'\''` (close, escaped quote, reopen).
/// Paths that are already bare words are left alone, so the common
/// installed case stays a config line a human can read.
///
/// One case no quoting can save: Hyprland splits a `bind =` line on
/// commas before the dispatcher ever sees it, so a path containing a
/// comma cannot be expressed there at all. Not worth a special case —
/// the sway dialect has no such rule, and the snippet is text the user
/// can still edit.
pub fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    if is_bare_word(&text) {
        return text.into_owned();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// What a typed `~` path resolved to, or why it could not.
///
/// A GUI text entry is not a shell, so nothing expands `~` for the
/// settings window's path fields; this is that expansion, kept pure so
/// the caller's tests never have to fight over process-global `HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// The path to use, `~` already replaced (or never present).
    Path(PathBuf),
    /// A `~` path with no `$HOME` to resolve it against. Probing the
    /// literal `./~/...` would only refuse for the wrong reason.
    NoHome,
    /// `~user/...`: resolving it needs passwd lookups this binary has no
    /// business doing. Refused, not probed.
    UserRelative,
}

/// Expand a leading `~` in a path the user typed. `~` and `~/` are the
/// home directory itself; anything without a leading `~` is taken
/// verbatim, so plain relative paths keep resolving against the cwd.
pub fn expand_tilde(typed: &str, home: Option<&Path>) -> Typed {
    let Some(rest) = typed.strip_prefix('~') else {
        return Typed::Path(PathBuf::from(typed));
    };
    // `~name` is user-relative; `~` and `~/…` are ours. Extra leading
    // separators are stripped so `join` cannot mistake the remainder for
    // an absolute path and throw the home directory away.
    let rest = if rest.is_empty() {
        ""
    } else if let Some(r) = rest.strip_prefix('/') {
        r.trim_start_matches('/')
    } else {
        return Typed::UserRelative;
    };
    let Some(home) = home else {
        return Typed::NoHome;
    };
    // `~/` alone is the home directory, and `join("")` would append a
    // trailing separator instead of leaving it be.
    if rest.is_empty() {
        Typed::Path(home.to_path_buf())
    } else {
        Typed::Path(home.join(rest))
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

    /// The XDG rung puts screenshots under the data dir: a card points
    /// at the file, so it is user data, never cache.
    #[test]
    fn a_relative_screenshot_dir_lands_under_the_data_dir_on_xdg() {
        let p = resolve(&env_with_home(), None);
        assert_eq!(
            PathBuf::from("/home/u/.local/share/chibipop/screenshots"),
            p.screenshots_dir("screenshots")
        );
        // The explicit rung is the XDG layout with one file moved, so it
        // resolves the same way.
        let explicit = resolve(&env_with_home(), Some(PathBuf::from("/etc/chibipop.toml")));
        assert_eq!(
            PathBuf::from("/home/u/.local/share/chibipop/shots"),
            explicit.screenshots_dir("shots")
        );
    }

    /// Portable mode keeps every artefact beside the exe, matching the
    /// Windows bin's own `save_dir` resolution: a copied folder has to
    /// carry the screenshots a user's cards point at.
    #[test]
    fn a_relative_screenshot_dir_lands_beside_the_exe_in_portable_mode() {
        let exe_dir = tmp_exe_dir("shots");
        std::fs::write(exe_dir.join(CONFIG_FILE), "").unwrap();
        let mut env = env_with_home();
        env.exe_dir = Some(exe_dir.clone());
        let p = resolve(&env, None);
        assert_eq!(Mode::Portable, p.mode);
        assert_eq!(exe_dir.join("screenshots"), p.screenshots_dir("screenshots"));
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    /// A typed absolute path is taken as-is, in every mode.
    #[test]
    fn an_absolute_screenshot_dir_is_taken_as_typed() {
        let exe_dir = tmp_exe_dir("shotsabs");
        std::fs::write(exe_dir.join(CONFIG_FILE), "").unwrap();
        let mut env = env_with_home();
        env.exe_dir = Some(exe_dir.clone());
        for p in [resolve(&env, None), resolve(&env_with_home(), None)] {
            assert_eq!(
                PathBuf::from("/home/u/Pictures/mining"),
                p.screenshots_dir("/home/u/Pictures/mining")
            );
        }
        let _ = std::fs::remove_dir_all(&exe_dir);
    }

    #[test]
    fn a_missing_runtime_dir_is_one_clear_error() {
        let p = resolve(&env_with_home(), None);
        let msg = p.runtime_dir().unwrap_err().to_string();
        assert!(msg.contains("XDG_RUNTIME_DIR"), "{msg}");
    }

    /// The one character single quotes cannot contain. `'\''` is the
    /// shell's own splice, and a snippet that got it wrong would run a
    /// truncated path.
    #[test]
    fn an_embedded_quote_is_spliced_the_way_sh_wants() {
        assert_eq!(r"'/home/u/it'\''s/chibipop'", shell_quote(Path::new("/home/u/it's/chibipop")));
    }

    /// Quoting is skipped only for paths that are literal to every
    /// shell; anything else pays for a pair of quotes.
    #[test]
    fn only_shell_safe_paths_are_left_bare() {
        assert_eq!("chibipop", shell_quote(Path::new("chibipop")));
        assert_eq!("/usr/bin/chibipop-0.1_x86", shell_quote(Path::new("/usr/bin/chibipop-0.1_x86")));
        assert_eq!("'chibipop*'", shell_quote(Path::new("chibipop*")));
        assert_eq!("'$HOME/chibipop'", shell_quote(Path::new("$HOME/chibipop")));
        assert_eq!("''", shell_quote(Path::new("")));
    }
}
