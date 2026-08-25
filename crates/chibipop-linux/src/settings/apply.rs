//! Apply = save-then-`reload` (ADR-0005).
//!
//! The config file is the sole source of truth: Apply writes the whole
//! struct, then sends one `reload` verb. The socket's presence *is* the
//! ApplyMode — connectable means the daemon hot-reloads, absent means
//! config-only with a notice. No structured settings ever cross the
//! socket.

use crate::control::{self, Verb};
use anyhow::{Context, Result};
use chibipop::config::{Config, PopupLayer};
use chibipop::settings::SettingsForm;
use std::path::Path;

/// The fields the shared form does not model: Linux platform fields
/// (ADR-0012) plus the lookup-log gate, edited straight on `Config`.
#[derive(Debug, Clone, PartialEq)]
pub struct LinuxFields {
    /// Advisory on the native channel; the portal binding later (36).
    pub trigger_key_linux: String,
    pub add_key_linux: String,
    pub layer: PopupLayer,
    pub show_lookup_log: bool,
}

impl LinuxFields {
    pub fn from_config(cfg: &Config) -> LinuxFields {
        LinuxFields {
            trigger_key_linux: cfg.trigger.trigger_key_linux.clone(),
            add_key_linux: cfg.anki.add_key_linux.clone(),
            layer: cfg.popup.layer,
            show_lookup_log: cfg.debug.show_lookup_log,
        }
    }

    pub fn apply_over(&self, cfg: &mut Config) {
        cfg.trigger.trigger_key_linux = self.trigger_key_linux.clone();
        cfg.anki.add_key_linux = self.add_key_linux.clone();
        cfg.popup.layer = self.layer;
        cfg.debug.show_lookup_log = self.show_lookup_log;
    }
}

/// Which mode Apply turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The daemon took the `reload`; `reply` is its one line.
    Live { reply: String },
    /// No daemon: saved only.
    ConfigOnly,
}

/// One Apply, start to finish.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub outcome: ApplyOutcome,
    /// Clamp notices `apply_to` produced, for the status area.
    pub notices: Vec<String>,
}

/// Save the whole struct, then one `reload`.
pub fn apply(
    form: &SettingsForm,
    linux: &LinuxFields,
    config_path: &Path,
    socket_path: &Path,
) -> Result<Applied> {
    let cfg = chibipop::config::load_or_create(config_path)
        .with_context(|| format!("re-reading {}", config_path.display()))?;
    let mut out = chibipop::settings::apply_to(form, &cfg);
    linux.apply_over(&mut out);
    let notices = chibipop::settings::clamp_notice(form, &out).into_iter().collect();
    out.save(config_path)?;
    let outcome = match control::send_to(socket_path, Verb::Reload) {
        Ok(reply) => ApplyOutcome::Live { reply },
        Err(_) => ApplyOutcome::ConfigOnly,
    };
    Ok(Applied { outcome, notices })
}

/// The status line an outcome earns.
pub fn describe(applied: &Applied) -> String {
    let mut line = match &applied.outcome {
        ApplyOutcome::Live { reply } => format!("Saved; daemon reloaded ({reply})."),
        ApplyOutcome::ConfigOnly => {
            "Saved. The daemon is not running - settings take effect when it starts.".to_string()
        }
    };
    for notice in &applied.notices {
        line.push(' ');
        line.push_str(notice);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chibipop_apply_test").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn form(cfg: &Config) -> SettingsForm {
        chibipop::settings::from_config(cfg, &[])
    }

    #[test]
    fn without_a_socket_apply_is_config_only() {
        let dir = scratch("config_only");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut linux = LinuxFields::from_config(&cfg);
        linux.show_lookup_log = true;

        let applied =
            apply(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        assert_eq!(applied.outcome, ApplyOutcome::ConfigOnly);
        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert!(saved.debug.show_lookup_log, "the flip must reach the file");
        assert!(describe(&applied).contains("daemon is not running"));
    }

    #[test]
    fn with_a_socket_apply_sends_exactly_one_reload() {
        let dir = scratch("live");
        let config_path = dir.join("chibipop.toml");
        let socket_path = dir.join("run.sock");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();

        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut stream = reader.into_inner();
            stream.write_all(b"OK reload\n").unwrap();
            line
        });

        let applied =
            apply(&form(&cfg), &LinuxFields::from_config(&cfg), &config_path, &socket_path)
                .unwrap();

        assert_eq!(applied.outcome, ApplyOutcome::Live { reply: "OK reload".into() });
        assert_eq!(server.join().unwrap(), "reload\n", "one verb, nothing else");
        assert!(describe(&applied).contains("daemon reloaded"));
    }

    #[test]
    fn linux_fields_ride_along_and_round_trip() {
        let dir = scratch("linux_fields");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let linux = LinuxFields {
            trigger_key_linux: "CTRL+SHIFT+K".into(),
            add_key_linux: "ALT+B".into(),
            layer: PopupLayer::Top,
            show_lookup_log: true,
        };

        apply(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(LinuxFields::from_config(&saved), linux);
        // The Windows twins survived the whole-struct save untouched.
        assert_eq!(saved.trigger.trigger_key, cfg.trigger.trigger_key);
        assert_eq!(saved.anki.add_key, cfg.anki.add_key);
        assert_eq!(saved.ocr.language, cfg.ocr.language);
    }

    #[test]
    fn clamp_notices_surface_in_the_description() {
        let dir = scratch("clamp");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut f = form(&cfg);
        f.capture_width = 5; // below the 100px floor

        let applied = apply(&f, &LinuxFields::from_config(&cfg), &config_path, &dir.join("no"))
            .unwrap();

        assert_eq!(applied.notices.len(), 1);
        assert!(describe(&applied).contains("raised to the 100px minimum"));
    }
}
