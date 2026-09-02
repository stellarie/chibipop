//! Autostart: the `.desktop` file *is* the state
//! (ARCHITECTURE.md#platform-integration and
//! ARCHITECTURE.md#settings-and-config).
//!
//! The checkbox reads and writes `$XDG_CONFIG_HOME/autostart/chibipop.desktop`
//! directly — there is no TOML field, so nothing can desync from a user
//! who hand-manages the file or takes the systemd/Hyprland route
//! (`extras/`). Presence means on, absence means off; a toggle writes or
//! removes the file and then re-reads it, so the widget never shows a
//! state the filesystem does not have.
//!
//! One file covers GNOME, KDE, and uwsm-managed Hyprland (systemd's
//! xdg-autostart generator). Portable mode does *not* move it: the
//! autostart directory belongs to the desktop environment, which reads
//! only XDG — only `Exec`/`TryExec` follow the running binary, so a
//! portable or AppImage install autostarts itself rather than some other
//! chibipop on `PATH`.

use crate::paths::{self, Env};
use std::io;
use std::path::{Path, PathBuf};

/// The autostart entry's file name, per
/// ARCHITECTURE.md#platform-integration.
pub const FILE_NAME: &str = "chibipop.desktop";

/// The autostart directory the spec defines, relative to the config root.
const DIR_NAME: &str = "autostart";

/// Where the entry goes and what it launches, resolved once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The XDG config root (*not* chibipop's own config dir).
    config_home: PathBuf,
    /// The binary `Exec` points at.
    exec: PathBuf,
}

impl Target {
    /// `None` when there is no config root to write into (neither
    /// `$XDG_CONFIG_HOME` nor `$HOME`) or the exe path is unknowable —
    /// the window then says so instead of guessing a path.
    pub fn resolve(env: &Env) -> Option<Target> {
        Some(Target { config_home: paths::config_home(env)?, exec: paths::exec_path().ok()? })
    }

    /// The full path of the autostart entry.
    pub fn file(&self) -> PathBuf {
        self.config_home.join(DIR_NAME).join(FILE_NAME)
    }

    /// The state, read off the filesystem every time it is asked for.
    pub fn is_enabled(&self) -> bool {
        self.file().is_file()
    }

    /// Write the entry (`on`) or remove it (`off`). Both directions are
    /// idempotent: writing overwrites a stale `Exec`, removing an absent
    /// file succeeds.
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

/// The entry text, per the desktop-entry spec: `Exec` runs the daemon
/// verb, `TryExec` lets the desktop skip a removed install.
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

/// A path as one quoted `Exec` argument. Quoting is unconditional so a
/// space in the install path cannot split the command. Two unescapings
/// run over this text — the desktop file's string rules first, then
/// `Exec`'s own tokenizer — so every backslash the tokenizer must see
/// has to be written twice, and `%` doubles so it is never read as a
/// field code.
fn quote_exec_arg(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for c in path.chars() {
        match c {
            // One literal backslash needs four: halved to two by the
            // string unescaping, then to one by the tokenizer.
            '\\' => out.push_str("\\\\\\\\"),
            // The tokenizer's reserved characters need one preceding
            // backslash, which two in the file unescape to.
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

/// A plain string value: only the spec's own escape sequences apply, so a
/// literal backslash doubles. Newlines cannot occur in a path component
/// that reached us from `current_exe`, but a `\n` in one would forge a
/// key line, so it is escaped too.
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

    /// Every non-group line is `Key=` or `Key[locale]=`, per the spec's
    /// key syntax, and no key is written twice.
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

        // Off twice is not an error: the file is the state, and it is
        // already in the asked-for state.
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

    /// A space in the install path must not split the command, and the
    /// spec's reserved characters must survive unescaping.
    #[test]
    fn an_awkward_path_stays_one_quoted_argument() {
        let text = entry(Path::new("/home/u/My Apps/chibi$pop"));
        assert_valid_entry(&text);
        assert!(text.contains("\nExec=\"/home/u/My Apps/chibi\\\\$pop\" run\n"), "{text}");
        assert!(text.contains("\nTryExec=/home/u/My Apps/chibi$pop\n"), "{text}");
    }

    /// The spec's two passes over `Exec`, in the order a launcher runs
    /// them: the string-value unescaping every entry gets, then the
    /// argument tokenizer. Panics on anything the spec leaves undefined,
    /// so a wrong escape count fails here rather than at login.
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

    /// Every character the two unescaping passes treat specially, in one
    /// path: a launcher must recover it byte for byte.
    #[test]
    fn exec_survives_both_unescaping_passes() {
        let exe = r#"/opt/we ird\dir/chibi$pop 50%`x"y"#;
        let text = entry(Path::new(exe));
        assert_valid_entry(&text);
        assert_eq!(vec![exe.to_string(), "run".to_string()], exec_argv(&text));
    }

    /// The plain path a real install has must stay readable — quoting is
    /// unconditional, but nothing else may creep in.
    #[test]
    fn a_plain_path_is_quoted_and_otherwise_untouched() {
        let text = entry(Path::new("/usr/bin/chibipop"));
        assert!(text.contains("\nExec=\"/usr/bin/chibipop\" run\n"), "{text}");
        assert_eq!(
            vec!["/usr/bin/chibipop".to_string(), "run".to_string()],
            exec_argv(&text)
        );
    }

    /// The written file is what a desktop reads, so an install that moved
    /// must not leave the old `Exec` behind.
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

    /// The checkbox state comes from the file alone: no directory, an
    /// empty directory, or someone else's entry all read as off.
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

        // A directory at our path is not an entry either.
        let decoy = Target { config_home: home.join("decoy"), exec: PathBuf::from("/usr/bin/chibipop") };
        std::fs::create_dir_all(decoy.file()).unwrap();
        assert!(!decoy.is_enabled(), "a directory named like the entry is off");

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A fresh account has no `autostart/` yet; the toggle creates it.
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

    /// The autostart file is XDG even in portable mode — the desktop
    /// environment reads nowhere else — while `Exec` follows the exe.
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

    /// `extras/` is documentation that has to keep working: the shipped
    /// files are read here so a renamed verb or a changed snippet cannot
    /// silently leave a broken copy-paste in the tarball (packaging
    /// ships this directory).
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

    /// The Hyprland snippet's bind lines must be the same lines the
    /// settings window hands out for the default chords - two spellings
    /// of one binding would be a support trap. The shipped file is for
    /// an *installed* chibipop, so the bare command name is the right
    /// exe there; a dev checkout gets its own path from
    /// `paths::exec_name` at runtime. The add-card bind is commented out
    /// in the shipped file, which still contains its line verbatim.
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
