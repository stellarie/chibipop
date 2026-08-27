//! Apply = save-then-`reload` (ADR-0005).
//!
//! The config file is the sole source of truth: Apply writes the whole
//! struct, then sends one `reload` verb. The socket's presence *is* the
//! ApplyMode — connectable means the daemon hot-reloads, absent means
//! config-only with a notice. No structured settings ever cross the
//! socket.

use crate::control::{self, Verb};
use anyhow::{Context, Result};
use chibipop::config::{Config, OcrClipboardConfig, PopupLayer, ScreenshotConfig};
use chibipop::settings::SettingsForm;
use std::path::Path;

/// The fields the shared form does not model: Linux platform fields
/// (ADR-0012) plus the lookup-log gate, edited straight on `Config`.
#[derive(Debug, Clone, PartialEq)]
pub struct LinuxFields {
    /// Advisory on the native channel; the portal binding later (36).
    pub trigger_key_linux: String,
    pub add_key_linux: String,
    /// The static-region chord. Native-channel only (ADR-0003's
    /// 2026-08-26 addendum): nothing registers it with the portal, so it
    /// exists to be rendered as a copyable `ctl static-region` bind.
    /// Empty means no chord, exactly as `add_key_linux` does.
    pub static_region_key_linux: String,
    /// The mining screenshot's chord, native-channel only for the same
    /// reason as the static-region one above. `Option`, not a `String`
    /// with `""` for unbound: the config field is `Option<String>`
    /// precisely so absence stays typed (`upstream-merge-fallout`
    /// ticket 06 removed that sentinel from the Windows twin), so the
    /// empty-text-box mapping lives at the UI edge and nowhere else.
    pub screenshot_key_linux: Option<String>,
    /// `actions.screenshot.save_dir` as typed. Where it resolves *to* is
    /// the daemon's (`Paths::screenshots_dir`), not this window's.
    pub screenshot_save_dir: String,
    /// The OCR-to-clipboard chord, native-channel only for the same
    /// reason as the two above. `Option` for the same reason
    /// `screenshot_key_linux` is: `actions.ocr_clipboard.hotkey_linux` is
    /// `Option<String>` precisely so absence stays typed, and the
    /// empty-text-box mapping lives at the UI edge.
    pub ocr_clipboard_key_linux: Option<String>,
    pub layer: PopupLayer,
    pub show_lookup_log: bool,
}

impl LinuxFields {
    pub fn from_config(cfg: &Config) -> LinuxFields {
        LinuxFields {
            trigger_key_linux: cfg.trigger.trigger_key_linux.clone(),
            add_key_linux: cfg.anki.add_key_linux.clone(),
            static_region_key_linux: cfg.anki.static_region_key_linux.clone(),
            screenshot_key_linux: cfg.actions.screenshot.hotkey_linux.clone(),
            screenshot_save_dir: cfg.actions.screenshot.save_dir.clone(),
            ocr_clipboard_key_linux: cfg
                .actions
                .ocr_clipboard
                .as_ref()
                .and_then(|action| action.hotkey_linux.clone()),
            layer: cfg.popup.layer,
            show_lookup_log: cfg.debug.show_lookup_log,
        }
    }

    pub fn apply_over(&self, cfg: &mut Config) {
        cfg.trigger.trigger_key_linux = self.trigger_key_linux.clone();
        cfg.anki.add_key_linux = self.add_key_linux.clone();
        cfg.anki.static_region_key_linux = self.static_region_key_linux.clone();
        cfg.actions.screenshot.hotkey_linux = self.screenshot_key_linux.clone();
        // The nested section carries *both* platforms' chords, so it may
        // only die when both are absent - the rule
        // `chibipop::settings::apply_to` already applies from the
        // Windows side (`an_unset_ocr_clipboard_key_keeps_the_linux_twin`),
        // read here off the config that call just wrote. Clearing this
        // box must not evict the Windows key with it.
        let windows_chord = cfg.actions.ocr_clipboard.as_ref().and_then(|a| a.hotkey.clone());
        cfg.actions.ocr_clipboard = match (windows_chord, self.ocr_clipboard_key_linux.clone()) {
            (None, None) => None,
            (hotkey, hotkey_linux) => Some(OcrClipboardConfig { hotkey, hotkey_linux }),
        };
        // A cleared box means the default folder, never the data dir
        // itself: a relative `save_dir` is joined onto that dir, so an
        // empty string would scatter PNGs among the database and the
        // dictionary archives.
        let dir = self.screenshot_save_dir.trim();
        cfg.actions.screenshot.save_dir = if dir.is_empty() {
            ScreenshotConfig::default().save_dir
        } else {
            dir.to_string()
        };
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

    /// The nested `[actions.ocr_clipboard]` section carries both
    /// platforms' chords, so clearing the Linux box must not evict the
    /// Windows key with it - the mirror of core's
    /// `an_unset_ocr_clipboard_key_keeps_the_linux_twin`, from this side.
    #[test]
    fn clearing_the_linux_ocr_clipboard_chord_keeps_the_windows_twin() {
        let dir = scratch("ocrclip_twin");
        let config_path = dir.join("chibipop.toml");
        let mut cfg = chibipop::config::load_or_create(&config_path).unwrap();
        cfg.actions.ocr_clipboard = Some(OcrClipboardConfig {
            hotkey: Some("f9".to_string()),
            hotkey_linux: Some("ALT+C".to_string()),
        });
        cfg.save(&config_path).unwrap();

        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut linux = LinuxFields::from_config(&cfg);
        assert_eq!(Some("ALT+C".to_string()), linux.ocr_clipboard_key_linux, "read in as typed");
        // The window's cleared text box, as `Message::OcrClipboardKey`
        // maps it: absence, never an empty string.
        linux.ocr_clipboard_key_linux = None;

        apply(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        assert_eq!(
            Some(OcrClipboardConfig { hotkey: Some("f9".to_string()), hotkey_linux: None }),
            chibipop::config::load_or_create(&config_path).unwrap().actions.ocr_clipboard,
            "the Windows chord survives a Linux Apply that cleared the Linux one"
        );
    }

    /// And with neither chord the section goes away rather than being
    /// written as a table full of `None`: absence stays absence
    /// (ADR-0012, `upstream-merge-fallout` ticket 06).
    #[test]
    fn an_ocr_clipboard_section_with_neither_chord_is_dropped() {
        let dir = scratch("ocrclip_dropped");
        let config_path = dir.join("chibipop.toml");
        let mut cfg = chibipop::config::load_or_create(&config_path).unwrap();
        cfg.actions.ocr_clipboard =
            Some(OcrClipboardConfig { hotkey: None, hotkey_linux: Some("ALT+C".to_string()) });
        cfg.save(&config_path).unwrap();

        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        let mut linux = LinuxFields::from_config(&cfg);
        linux.ocr_clipboard_key_linux = None;

        apply(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        assert_eq!(
            None,
            chibipop::config::load_or_create(&config_path).unwrap().actions.ocr_clipboard
        );
    }

    /// The other direction: a chord typed into an empty config creates
    /// the section, and a later reload reads it back.
    #[test]
    fn a_typed_ocr_clipboard_chord_creates_the_section_and_round_trips() {
        let dir = scratch("ocrclip_typed");
        let config_path = dir.join("chibipop.toml");
        let cfg = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(None, cfg.actions.ocr_clipboard, "a default config has no section");
        let mut linux = LinuxFields::from_config(&cfg);
        linux.ocr_clipboard_key_linux = Some("ALT+C".to_string());

        apply(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(
            Some(OcrClipboardConfig { hotkey: None, hotkey_linux: Some("ALT+C".to_string()) }),
            saved.actions.ocr_clipboard
        );
        assert_eq!(
            Some("ALT+C".to_string()),
            LinuxFields::from_config(&saved).ocr_clipboard_key_linux
        );
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
            static_region_key_linux: "ALT+R".into(),
            screenshot_key_linux: Some("SUPER+S".into()),
            screenshot_save_dir: "shots".into(),
            ocr_clipboard_key_linux: Some("SUPER+C".into()),
            layer: PopupLayer::Top,
            show_lookup_log: true,
        };

        apply(&form(&cfg), &linux, &config_path, &dir.join("absent.sock")).unwrap();

        let saved = chibipop::config::load_or_create(&config_path).unwrap();
        assert_eq!(LinuxFields::from_config(&saved), linux);
        // The Windows twins survived the whole-struct save untouched.
        assert_eq!(saved.trigger.trigger_key, cfg.trigger.trigger_key);
        assert_eq!(saved.anki.add_key, cfg.anki.add_key);
        assert_eq!(saved.anki.static_region_key, cfg.anki.static_region_key);
        assert_eq!(saved.actions.screenshot.hotkey, cfg.actions.screenshot.hotkey);
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
