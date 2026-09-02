//! Which capture backend serves this session
//! (ARCHITECTURE.md#capture-and-masking), decided by *advertised
//! capability* at startup and never by compositor identity.
//!
//! Two rungs, in this order. Rung 1 is wlr-screencopy: compositor-side
//! region capture that prompts for nothing, so on Hyprland/sway/niri a
//! hover works the instant the daemon starts. Rung 2 is the
//! xdg-desktop-portal ScreenCast + PipeWire fallback, which costs one
//! consent dialog and exists for the compositors with no screencopy at
//! all (GNOME, and anything else that only speaks the portal). The
//! order is the whole point: a wlr compositor that *also* runs a portal
//! must keep taking the promptless path, so screencopy wins whenever it
//! is advertised even when the portal is right there beside it.
//!
//! Absence is never fatal. When neither rung is present the daemon
//! stays up and [`Selection::Unsupported`] names the exact missing
//! capability by its protocol/interface name, so a compositor upgrade
//! self-heals the install with no code change.
//!
//! Test hook: `CHIBIPOP_CAPTURE_BACKEND=auto|screencopy|portal|none`
//! forces a rung (or the empty ladder, to exercise the unsupported
//! diagnostic) instead of the capability-selected one. Forcing
//! `portal` on Hyprland — which advertises screencopy and so would
//! never reach rung 2 on its own — is how ticket 34's fallback backend
//! is smoke-tested.

/// The portal interface whose presence on the session bus is the
/// fallback rung's capability probe.
pub const SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";

/// The ladder, in rung order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Rung 1: `zwlr_screencopy_manager_v1` region capture. Promptless.
    WlrScreencopy,
    /// Rung 2: portal ScreenCast negotiated once at startup, frames
    /// arriving over PipeWire. One consent dialog, then nothing.
    Portal,
}

/// What this session advertises, as far as the capture ladder cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// `zwlr_screencopy_manager_v1` and its plumbing are advertised.
    pub screencopy: bool,
    /// `org.freedesktop.portal.ScreenCast` answers on the session bus.
    pub portal_screencast: bool,
}

impl Capabilities {
    /// `globals` is the daemon's startup registry probe; `portal` is the
    /// D-Bus probe the caller already ran.
    pub fn scan(globals: &[crate::wayland::Advertised], portal: bool) -> Capabilities {
        Capabilities { screencopy: crate::capture::available(globals), portal_screencast: portal }
    }
}

/// The `CHIBIPOP_CAPTURE_BACKEND` test hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendOverride {
    /// Walk the ladder normally: capability-first, promptless rung first.
    Auto,
    /// Force rung 1 (fail rather than fall through to the portal).
    Screencopy,
    /// Force rung 2, pretending screencopy is absent — the documented
    /// way to exercise the fallback on a wlr compositor.
    Portal,
    /// Pretend the ladder is empty: exercise the unsupported path.
    None,
}

impl BackendOverride {
    /// The environment variable this hook reads.
    pub const ENV: &'static str = "CHIBIPOP_CAPTURE_BACKEND";

    /// One of `auto|screencopy|portal|none`, or `None` for anything
    /// else.
    pub fn parse(value: &str) -> Option<BackendOverride> {
        match value {
            "auto" => Some(BackendOverride::Auto),
            "screencopy" => Some(BackendOverride::Screencopy),
            "portal" => Some(BackendOverride::Portal),
            "none" => Some(BackendOverride::None),
            _ => Option::None,
        }
    }

    /// The override and, when the value was unrecognized, a diagnostic.
    pub fn from_env() -> (BackendOverride, Option<String>) {
        match std::env::var(Self::ENV) {
            Err(_) => (BackendOverride::Auto, Option::None),
            Ok(v) => match Self::parse(&v) {
                Some(ov) => (ov, Option::None),
                Option::None => (
                    BackendOverride::Auto,
                    Some(format!(
                        "capture: ignoring {}={v:?}; expected auto|screencopy|portal|none",
                        Self::ENV
                    )),
                ),
            },
        }
    }
}

/// What [`select`] decided: a live backend, or exactly what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The backend the daemon should open.
    Backend(Backend),
    /// No capture at all; `missing` names exactly what is absent.
    Unsupported { missing: Vec<String> },
}

impl Selection {
    /// The chosen backend, or `None` when nothing is available.
    pub fn backend(&self) -> Option<Backend> {
        match self {
            Selection::Backend(b) => Some(*b),
            Selection::Unsupported { .. } => None,
        }
    }

    /// The one startup line the daemon logs for the capture channel.
    pub fn startup_line(&self) -> String {
        match self {
            Selection::Backend(Backend::WlrScreencopy) => {
                "capture: wlr-screencopy region capture (promptless - ladder rung 1)".to_string()
            }
            Selection::Backend(Backend::Portal) => {
                "capture: portal ScreenCast + PipeWire (eager consent at startup - ladder rung 2)"
                    .to_string()
            }
            Selection::Unsupported { missing } => format!(
                "capture: unsupported - missing {}; a compositor or portal offering the missing capability self-heals this install",
                missing.join(", ")
            ),
        }
    }
}

/// The capabilities `caps` lacks, by exact protocol/interface name, so
/// a compositor upgrade self-heals the install.
fn missing(caps: &Capabilities) -> Vec<String> {
    let mut names = Vec::new();
    if !caps.screencopy {
        names.push(crate::capture::session::MANAGER_GLOBAL.to_string());
    }
    if !caps.portal_screencast {
        names.push(SCREENCAST_INTERFACE.to_string());
    }
    names
}

/// Walk the capture ladder. Capability-first, promptless rung first:
/// screencopy wins whenever it is advertised, even on a session that
/// also runs a portal.
pub fn select(caps: &Capabilities, ov: BackendOverride) -> Selection {
    match ov {
        BackendOverride::Auto => {
            if caps.screencopy {
                return Selection::Backend(Backend::WlrScreencopy);
            }
            if caps.portal_screencast {
                return Selection::Backend(Backend::Portal);
            }
            Selection::Unsupported { missing: missing(caps) }
        }
        BackendOverride::Screencopy => {
            if caps.screencopy {
                Selection::Backend(Backend::WlrScreencopy)
            } else {
                Selection::Unsupported {
                    missing: vec![crate::capture::session::MANAGER_GLOBAL.to_string()],
                }
            }
        }
        BackendOverride::Portal => {
            if caps.portal_screencast {
                Selection::Backend(Backend::Portal)
            } else {
                Selection::Unsupported { missing: vec![SCREENCAST_INTERFACE.to_string()] }
            }
        }
        // Simulated empty ladder: report both rungs' needs as if
        // nothing were advertised anywhere.
        BackendOverride::None => Selection::Unsupported {
            missing: missing(&Capabilities { screencopy: false, portal_screencast: false }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::session::MANAGER_GLOBAL;
    use crate::wayland::Advertised;

    const BOTH: Capabilities = Capabilities { screencopy: true, portal_screencast: true };
    const PORTAL_ONLY: Capabilities = Capabilities { screencopy: false, portal_screencast: true };
    const SCREENCOPY_ONLY: Capabilities =
        Capabilities { screencopy: true, portal_screencast: false };
    const NOTHING: Capabilities = Capabilities { screencopy: false, portal_screencast: false };

    fn advertised(interface: &str) -> Advertised {
        Advertised { name: 1, interface: interface.to_string(), version: 1 }
    }

    /// The acceptance criterion: Hyprland advertises screencopy *and*
    /// runs a portal, and it must keep taking the promptless path.
    #[test]
    fn screencopy_beats_the_portal_even_when_both_are_present() {
        assert_eq!(
            Selection::Backend(Backend::WlrScreencopy),
            select(&BOTH, BackendOverride::Auto)
        );
        assert_eq!(
            Selection::Backend(Backend::WlrScreencopy),
            select(&SCREENCOPY_ONLY, BackendOverride::Auto)
        );
    }

    #[test]
    fn a_portal_only_session_falls_back_to_the_portal() {
        assert_eq!(
            Selection::Backend(Backend::Portal),
            select(&PORTAL_ONLY, BackendOverride::Auto)
        );
    }

    /// The diagnostic names the exact absent capabilities so an upgrade
    /// on either side self-heals.
    #[test]
    fn neither_rung_names_both_missing_capabilities() {
        let Selection::Unsupported { missing } = select(&NOTHING, BackendOverride::Auto) else {
            panic!("expected Unsupported");
        };
        assert_eq!(vec![MANAGER_GLOBAL, SCREENCAST_INTERFACE], missing);
        assert_eq!(None, select(&NOTHING, BackendOverride::Auto).backend());
    }

    /// Forcing `portal` on a wlr compositor is the documented smoke
    /// test; forcing `screencopy` where it is absent must fail honestly
    /// rather than quietly prompting.
    #[test]
    fn the_forced_overrides_pin_their_backend() {
        assert_eq!(Selection::Backend(Backend::Portal), select(&BOTH, BackendOverride::Portal));
        assert_eq!(
            Selection::Backend(Backend::WlrScreencopy),
            select(&BOTH, BackendOverride::Screencopy)
        );
        assert_eq!(
            Selection::Unsupported { missing: vec![MANAGER_GLOBAL.to_string()] },
            select(&PORTAL_ONLY, BackendOverride::Screencopy)
        );
        assert_eq!(
            Selection::Unsupported { missing: vec![SCREENCAST_INTERFACE.to_string()] },
            select(&SCREENCOPY_ONLY, BackendOverride::Portal)
        );
    }

    /// `none` simulates a compositor with neither rung, whatever this
    /// machine actually advertises.
    #[test]
    fn the_none_override_lists_both_capabilities() {
        let Selection::Unsupported { missing } = select(&BOTH, BackendOverride::None) else {
            panic!("expected Unsupported");
        };
        assert_eq!(vec![MANAGER_GLOBAL, SCREENCAST_INTERFACE], missing);
    }

    #[test]
    fn every_startup_line_is_one_greppable_line_naming_its_rung_or_the_way_back() {
        for selection in [
            Selection::Backend(Backend::WlrScreencopy),
            Selection::Backend(Backend::Portal),
            select(&NOTHING, BackendOverride::Auto),
        ] {
            let line = selection.startup_line();
            assert!(line.starts_with("capture: "), "{line}");
            assert!(!line.contains('\n'), "{line}");
            assert!(line.contains("rung ") || line.contains("self-heals"), "{line}");
        }
        assert!(
            Selection::Backend(Backend::WlrScreencopy).startup_line().contains("promptless"),
            "rung 1's promptlessness is the reason it is rung 1"
        );
        assert!(
            Selection::Backend(Backend::Portal).startup_line().contains("consent"),
            "rung 2's cost must be visible in the log"
        );
        assert!(select(&NOTHING, BackendOverride::Auto)
            .startup_line()
            .contains("unsupported - missing"));
    }

    #[test]
    fn override_parsing_covers_the_documented_values() {
        assert_eq!(Some(BackendOverride::Auto), BackendOverride::parse("auto"));
        assert_eq!(Some(BackendOverride::Screencopy), BackendOverride::parse("screencopy"));
        assert_eq!(Some(BackendOverride::Portal), BackendOverride::parse("portal"));
        assert_eq!(Some(BackendOverride::None), BackendOverride::parse("none"));
        assert_eq!(Option::None, BackendOverride::parse("pipewire"));
    }

    /// Selection is by capability, so the screencopy half comes from
    /// the same probe the backend itself uses.
    #[test]
    fn capabilities_scan_uses_the_screencopy_probe_and_the_portal_answer() {
        let globals = [advertised(MANAGER_GLOBAL), advertised("wl_shm"), advertised("wl_output")];
        let caps = Capabilities::scan(&globals, true);
        assert!(caps.screencopy && caps.portal_screencast);

        let caps = Capabilities::scan(&globals[1..], true);
        assert!(!caps.screencopy, "screencopy without its manager is not a rung");
        assert!(!Capabilities::scan(&globals, false).portal_screencast);
    }
}
