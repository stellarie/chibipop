//! The `.desktop` file is the autostart state
//! (ARCHITECTURE.md#platform-integration and
//! ARCHITECTURE.md#settings-and-config).
//!
//! The checkbox reads and writes `$XDG_CONFIG_HOME/autostart/chibipop.desktop`
//! directly. No TOML field stores this state. This keeps the file and widget
//! state aligned when a user manages the file manually or uses the
//! systemd/Hyprland route
//! (`extras/`). File presence means on. File absence means off.
//! A toggle writes or removes the file, then reads it again. The widget never
//! shows a state that differs from the filesystem.
//!
//! One file supports GNOME, KDE, and uwsm-managed Hyprland
//! (systemd's xdg-autostart generator). Portable mode does not move this file.
//! The desktop environment owns the autostart directory and reads only XDG.
//! `Exec` and `TryExec` use the current binary path. A portable or AppImage
//! install therefore starts itself, not another chibipop on `PATH`.

use crate::paths::{self, Env};
use std::io;
use std::path::{Path, PathBuf};

/// Name the autostart entry file. See
/// ARCHITECTURE.md#platform-integration.
pub const FILE_NAME: &str = "chibipop.desktop";

/// Name the autostart directory relative to the config root, as the spec defines.
const DIR_NAME: &str = "autostart";

/// Startup target that identifies the entry location and executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The XDG config root, not chibipop's own config directory.
    config_home: PathBuf,
    /// The executable path for `Exec`.
    exec: PathBuf,
}

impl Target {
    /// Return `None` when neither `$XDG_CONFIG_HOME` nor `$HOME` provides a
    /// config root, or when the executable path is unknown.
    /// The window reports this case and does not guess a path.
    pub fn resolve(env: &Env) -> Option<Target> {
        Some(Target { config_home: paths::config_home(env)?, exec: paths::exec_path().ok()? })
    }

    /// Return the full path for the autostart entry.
    pub fn file(&self) -> PathBuf {
        self.config_home.join(DIR_NAME).join(FILE_NAME)
    }

    /// Read the enabled state from the filesystem on every call.
    pub fn is_enabled(&self) -> bool {
        self.file().is_file()
    }

    /// Write the entry when `on` is true. Remove it when `on` is false.
    /// Both operations are idempotent. A write replaces a stale `Exec`, and
    /// removal of an absent file succeeds.
    pub fn set(&self, on: bool) -> io::Result<()> {
        let file = self.file();
        if !on {
            return match std::fs::remove_file(&file) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            };
        }
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, entry(&self.exec))
    }
}

/// Build entry text per the desktop-entry spec. `Exec` runs the daemon verb.
/// `TryExec` lets the desktop skip an install that no longer exists.
pub fn entry(exec: &Path) -> String {
    let exec = exec.to_string_lossy();
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name=chibipop\n\
         GenericName=Japanese lookup\n\
         Comment=Hover-to-read Japanese lookup\n\
         Exec={exec_arg} run\n\
         TryExec={try_exec}\n\
         Icon=chibipop\n\
         Terminal=false\n\
         Categories=Utility;\n\
         StartupNotify=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_arg = quote_exec_arg(&exec),
        try_exec = escape_value(&exec),
    )
}

/// Quote a path as one `Exec` argument.
/// Always quote it, so spaces in the install path do not split the command.
/// The desktop-entry string pass unescapes the value first.
/// The `Exec` tokenizer unescapes each quoted argument second.
/// Write four backslashes in the file for one literal argument backslash.
/// The first pass changes four to two. The second pass changes two to one.
/// For a quote, `$`, or a backtick, write two file backslashes before it.
/// The first pass changes the pair to one. The tokenizer uses it to preserve that character.
/// Double `%` so the tokenizer does not treat it as a field code.
fn quote_exec_arg(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for c in path.chars() {
        match c {
            // One literal backslash needs four in the file. The desktop-entry pass
            // changes four to two, then the `Exec` tokenizer changes two to one.
            '\\' => out.push_str("\\\\\\\\"),
            // The tokenizer needs one backslash before each reserved character.
            // The desktop-entry pass changes two file backslashes to one for the tokenizer.
            '"' | '$' | '`' => {
                out.push_str("\\\\");
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escape a plain string value with the spec escape sequences.
/// Double literal backslashes. Escape newline, carriage return, and tab
/// characters so they stay inside the value.
fn escape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_config_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("chibipop_autostart_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn target(name: &str) -> Target {
        Target { config_home: tmp_config_home(name), exec: PathBuf::from("/opt/chibipop/chibipop") }
    }

    #[test]
    fn writing_then_removing_round_trips_through_the_file() {
        let target = target("round_trip");
        assert!(!target.is_enabled(), "a fresh config home has no entry");

        target.set(true).unwrap();
        assert!(target.is_enabled());
        assert!(target.file().is_file());
        assert_eq!(
            target.file(),
            target.file().parent().unwrap().join(FILE_NAME),
            "the entry keeps its spec name"
        );

        target.set(false).unwrap();
        assert!(!target.is_enabled());
        assert!(!target.file().exists());

        // Two `off` writes are not errors. The file already has the requested
        // off state.
        target.set(false).unwrap();
        assert!(!target.is_enabled());

        let _ = std::fs::remove_dir_all(target.file().parent().unwrap().parent().unwrap());
    }

}
