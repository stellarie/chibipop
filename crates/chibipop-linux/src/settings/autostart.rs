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

    /// Each non-group line follows `Key=` or `Key[locale]=` syntax from the spec.
    /// No key appears twice.
    fn assert_valid_entry(text: &str) {
        let mut lines = text.lines();
        assert_eq!(Some("[Desktop Entry]"), lines.next(), "group header must come first");
        let mut seen: Vec<&str> = Vec::new();
        for line in lines {
            let (key, _) = line.split_once('=').unwrap_or_else(|| panic!("not a key line: {line}"));
            let name = key.split_once('[').map_or(key, |(n, _)| n);
            assert!(
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "invalid key syntax: {line}"
            );
            assert!(!seen.contains(&key), "duplicate key: {key}");
            seen.push(key);
        }
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

    #[test]
    fn the_entry_names_the_binary_and_the_run_verb() {
        let text = entry(Path::new("/opt/chibipop/chibipop"));
        assert_valid_entry(&text);
        assert!(text.contains("\nType=Application\n"), "{text}");
        assert!(text.contains("\nExec=\"/opt/chibipop/chibipop\" run\n"), "{text}");
        assert!(text.contains("\nTryExec=/opt/chibipop/chibipop\n"), "{text}");
        assert!(text.contains("\nName=chibipop\n"), "{text}");
        assert!(text.ends_with('\n'), "the file ends with a newline");
    }

    /// A space in the install path must not split the command.
    /// The spec's reserved characters must survive both escape passes.
    #[test]
    fn an_awkward_path_stays_one_quoted_argument() {
        let text = entry(Path::new("/home/u/My Apps/chibi$pop"));
        assert_valid_entry(&text);
        assert!(text.contains("\nExec=\"/home/u/My Apps/chibi\\\\$pop\" run\n"), "{text}");
        assert!(text.contains("\nTryExec=/home/u/My Apps/chibi$pop\n"), "{text}");
    }

    /// A launcher applies two passes to `Exec` text.
    /// First, the desktop-entry string pass unescapes the value.
    /// Then, the `Exec` tokenizer applies its escape rules to the quoted argument.
    /// The `exec_argv` helper panics when the text violates these rules.
    /// A wrong escape count fails before login.
    fn exec_argv(entry_text: &str) -> Vec<String> {
        let value =
            entry_text.lines().find_map(|l| l.strip_prefix("Exec=")).expect("an Exec key");

        let mut unescaped = String::new();
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                unescaped.push(c);
                continue;
            }
            match chars.next().expect("a dangling backslash") {
                's' => unescaped.push(' '),
                'n' => unescaped.push('\n'),
                't' => unescaped.push('\t'),
                'r' => unescaped.push('\r'),
                '\\' => unescaped.push('\\'),
                other => panic!("undefined escape sequence \\{other} in {value}"),
            }
        }

        let mut argv = Vec::new();
        let mut arg = String::new();
        let mut quoted = false;
        let mut chars = unescaped.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => quoted = !quoted,
                '\\' if quoted => arg.push(chars.next().expect("a dangling escape in quotes")),
                '%' => {
                    assert_eq!(Some('%'), chars.next(), "an unexpanded field code in {value}");
                    arg.push('%');
                }
                ' ' if !quoted => {
                    if !arg.is_empty() {
                        argv.push(std::mem::take(&mut arg));
                    }
                }
                _ => arg.push(c),
            }
        }
        if !arg.is_empty() {
            argv.push(arg);
        }
        argv
    }

    /// The path contains characters that either pass treats specially.
    /// A launcher must recover every byte.
    #[test]
    fn exec_survives_both_unescaping_passes() {
        let exe = r#"/opt/we ird\dir/chibi$pop 50%`x"y"#;
        let text = entry(Path::new(exe));
        assert_valid_entry(&text);
        assert_eq!(vec![exe.to_string(), "run".to_string()], exec_argv(&text));
    }

    /// A plain install path stays readable. The path always has quotes, but
    /// no other escape must appear.
    #[test]
    fn a_plain_path_is_quoted_and_otherwise_untouched() {
        let text = entry(Path::new("/usr/bin/chibipop"));
        assert!(text.contains("\nExec=\"/usr/bin/chibipop\" run\n"), "{text}");
        assert_eq!(
            vec!["/usr/bin/chibipop".to_string(), "run".to_string()],
            exec_argv(&text)
        );
    }

    /// The desktop reads the written file. If an install moves, a new write
    /// must replace the old `Exec`.
    #[test]
    fn writing_again_refreshes_a_stale_exec() {
        let home = tmp_config_home("stale");
        Target { config_home: home.clone(), exec: PathBuf::from("/old/chibipop") }.set(true).unwrap();
        let target = Target { config_home: home.clone(), exec: PathBuf::from("/new/chibipop") };
        target.set(true).unwrap();

        let text = std::fs::read_to_string(target.file()).unwrap();
        assert!(text.contains("\"/new/chibipop\" run"), "{text}");
        assert!(!text.contains("/old/chibipop"), "{text}");
        assert_valid_entry(&text);

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Read the checkbox state from this file alone.
    /// A directory that does not exist, an empty directory, or another app's
    /// entry means off.
    #[test]
    fn the_checkbox_state_is_the_file_and_nothing_else() {
        let home = tmp_config_home("state");
        let absent = Target { config_home: home.join("nothing/here"), exec: PathBuf::from("/usr/bin/chibipop") };
        assert!(!absent.is_enabled(), "a missing directory is off, not a panic");

        let target = Target { config_home: home.clone(), exec: PathBuf::from("/usr/bin/chibipop") };
        std::fs::create_dir_all(home.join(DIR_NAME)).unwrap();
        assert!(!target.is_enabled(), "an empty autostart directory is off");

        std::fs::write(home.join(DIR_NAME).join("other-app.desktop"), "x").unwrap();
        assert!(!target.is_enabled(), "another app's entry is not ours");

        // A directory at the target path is not an entry.
        let decoy = Target { config_home: home.join("decoy"), exec: PathBuf::from("/usr/bin/chibipop") };
        std::fs::create_dir_all(decoy.file()).unwrap();
        assert!(!decoy.is_enabled(), "a directory named like the entry is off");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A fresh account has no `autostart/` directory.
    /// The setting creates it.
    #[test]
    fn enabling_creates_a_missing_autostart_directory() {
        let home = tmp_config_home("nested");
        let target = Target { config_home: home.join("deep/config"), exec: PathBuf::from("/usr/bin/chibipop") };
        assert!(!target.file().parent().unwrap().exists());

        target.set(true).unwrap();
        assert!(target.is_enabled());
        assert_valid_entry(&std::fs::read_to_string(target.file()).unwrap());

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Keep the autostart file in XDG in portable mode.
    /// The desktop environment reads no other location.
    /// `Exec` follows the executable path.
    #[test]
    fn portable_mode_keeps_the_entry_in_the_xdg_config_root() {
        let env = Env {
            home: Some(PathBuf::from("/home/u")),
            exe_dir: Some(PathBuf::from("/media/stick")),
            ..Env::default()
        };
        let target = Target::resolve(&env).expect("a HOME is enough to resolve");
        assert_eq!(PathBuf::from("/home/u/.config/autostart/chibipop.desktop"), target.file());

        let mut env = env;
        env.xdg_config_home = Some(PathBuf::from("/cfg"));
        let target = Target::resolve(&env).unwrap();
        assert_eq!(PathBuf::from("/cfg/autostart/chibipop.desktop"), target.file());
    }

    #[test]
    fn without_a_config_root_there_is_no_target() {
        assert_eq!(None, Target::resolve(&Env::default()));
    }

    /// Keep `extras/` documentation valid. These tests read shipped files, so a
    /// renamed verb or changed snippet cannot leave an invalid command example in the tarball.
    /// The package ships this directory.
    fn extras(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extras").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    #[test]
    fn the_shipped_launcher_entry_is_a_valid_desktop_entry() {
        let text = extras("chibipop.desktop");
        assert_valid_entry(&text);
        assert!(text.contains("\nType=Application\n"), "{text}");
        assert!(text.contains("\nExec=chibipop run\n"), "{text}");
        assert!(text.contains("\nTryExec=chibipop\n"), "{text}");
    }

    #[test]
    fn the_shipped_unit_starts_the_daemon_with_the_session() {
        let text = extras("chibipop.service");
        assert!(text.contains("\nExecStart=/usr/bin/chibipop run\n"), "{text}");
        assert!(text.contains("\nWantedBy=graphical-session.target\n"), "{text}");
        assert!(text.contains("\nPartOf=graphical-session.target\n"), "{text}");
    }

    /// Hyprland bind lines must match the lines from the settings window for
    /// default chords. Two spellings for one bind would create support problems.
    /// The shipped file targets an *installed* chibipop, so the bare command
    /// name is correct there. A dev checkout gets its own path from
    /// `paths::exec_name` at runtime. The shipped file has the add-card bind
    /// as a comment, but it keeps the line verbatim.
    #[test]
    fn the_shipped_hyprland_snippet_matches_the_window_snippet() {
        let text = extras("hyprland.conf");
        assert!(text.contains("\nexec-once = chibipop run\n"), "{text}");

        let cfg = chibipop::config::Config::default();
        let hyprland = super::super::snippets::Compositor::Hyprland;
        let both = [
            super::super::snippets::bind_snippet(
                hyprland,
                &cfg.trigger.trigger_key_linux,
                Path::new(paths::COMMAND),
                super::super::snippets::Bind::Hold,
            ),
            super::super::snippets::bind_snippet(
                hyprland,
                &cfg.anki.add_key_linux,
                Path::new(paths::COMMAND),
                super::super::snippets::Bind::Press(crate::control::Verb::AnkiAdd),
            ),
        ];
        for line in both.iter().flat_map(|snippet| snippet.lines()) {
            assert!(text.contains(line), "extras/hyprland.conf is missing {line:?}");
        }
    }
}
