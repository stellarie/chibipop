//! The capture backend for this session comes from the advertised
//! capability at startup. It never comes from the compositor identity.
//! See ARCHITECTURE.md#capture-and-masking.
//!
//! The ladder has two rungs in this order. Rung 1 is wlr-screencopy.
//! It captures a region through the compositor and needs no prompt.
//! A hover works as soon as the daemon starts on Hyprland, sway, or niri.
//! Rung 2 is the xdg-desktop-portal ScreenCast + PipeWire fallback.
//! It needs one consent dialog and supports compositors without screencopy,
//! such as GNOME. A compositor can also run a portal, but screencopy
//! still wins when it is advertised.
//!
//! An absent rung is not fatal. When both rungs are absent, the daemon
//! stays up. [`Selection::Unsupported`] names the capability requirements
//! missing from the path that the selection tried. A compositor upgrade
//! self-heals the install without a code change.
//!
//! Test hook: `CHIBIPOP_CAPTURE_BACKEND=auto|screencopy|portal|none`
//! selects a rung or an empty ladder for the unsupported diagnostic.
//! The hook overrides capability selection. Hyprland advertises screencopy
//! and also runs a portal, so `portal` forces the fallback for a smoke test.

/// The portal interface that the fallback rung probes on the session bus.
pub const SCREENCAST_INTERFACE: &str = "org.freedesktop.portal.ScreenCast";

/// The capture ladder in rung order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Rung 1: `zwlr_screencopy_manager_v1` captures regions without a prompt.
    WlrScreencopy,
    /// Rung 2: portal ScreenCast starts once at startup. Frames arrive through
    /// PipeWire after one consent dialog.
    Portal,
}

/// The capabilities that this session advertises to the capture ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// The session advertises the required globals: `zwlr_screencopy_manager_v1`, `wl_shm`, and
    /// `wl_output`.
    pub screencopy: bool,
    /// The session bus answers for `org.freedesktop.portal.ScreenCast`.
    pub portal_screencast: bool,
}

impl Capabilities {
    /// `globals` is the daemon startup registry probe. `portal` is the D-Bus probe
    /// that the caller already ran.
    pub fn scan(globals: &[crate::wayland::Advertised], portal: bool) -> Capabilities {
        Capabilities { screencopy: crate::capture::available(globals), portal_screencast: portal }
    }
}

/// The test hook that reads `CHIBIPOP_CAPTURE_BACKEND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendOverride {
    /// Use the normal ladder. It selects the first available rung in fixed
    /// ladder order. The order is `WlrScreencopy`, then `Portal`.
    Auto,
    /// Force rung 1. Report it as unsupported when rung 1 is absent. Do not use
    /// the portal.
    Screencopy,
    /// Force rung 2 as if screencopy were absent. Use this path to test the
    /// fallback on a wlr compositor.
    Portal,
    /// Treat the ladder as empty and exercise the unsupported path.
    None,
}

impl BackendOverride {
    /// The environment variable that this hook reads.
    pub const ENV: &'static str = "CHIBIPOP_CAPTURE_BACKEND";

    /// The accepted values are `auto|screencopy|portal|none`. Return `None` for
    /// every other value.
    pub fn parse(value: &str) -> Option<BackendOverride> {
        match value {
            "auto" => Some(BackendOverride::Auto),
            "screencopy" => Some(BackendOverride::Screencopy),
            "portal" => Some(BackendOverride::Portal),
            "none" => Some(BackendOverride::None),
            _ => Option::None,
        }
    }

    /// Return the override and a diagnostic when the value is not recognized.
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

/// The result from [`select`]: a live backend or the capability requirements
/// missing from the attempted path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The backend that the daemon must open.
    Backend(Backend),
    /// No capture is available. `missing` lists capability requirements absent
    /// from the attempted path, not necessarily every absent capability.
    Unsupported { missing: Vec<String> },
}

impl Selection {
    /// Return the selected backend, or `None` when no backend is available.
    pub fn backend(&self) -> Option<Backend> {
        match self {
            Selection::Backend(b) => Some(*b),
            Selection::Unsupported { .. } => None,
        }
    }

    /// Return the one startup line for the capture channel.
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

/// Return the exact protocol or interface names that `caps` lacks. A compositor
/// upgrade self-heals the install.
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

/// Select a rung according to `ov`. `Auto` selects the first available rung
/// in fixed ladder order. Other values constrain or empty the ladder.
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
        // Simulate an empty ladder. Report both rung requirements as absent.
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

    /// Keep the promptless path when Hyprland advertises screencopy and also
    /// runs a portal.
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

    /// Name the exact absent capabilities so an upgrade on either side
    /// self-heals the install.
    #[test]
    fn neither_rung_names_both_missing_capabilities() {
        let Selection::Unsupported { missing } = select(&NOTHING, BackendOverride::Auto) else {
            panic!("expected Unsupported");
        };
        assert_eq!(vec![MANAGER_GLOBAL, SCREENCAST_INTERFACE], missing);
        assert_eq!(None, select(&NOTHING, BackendOverride::Auto).backend());
    }

    /// Force `portal` on a wlr compositor for the documented smoke test.
    /// If screencopy is absent, report the absence instead of a prompt.
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

    /// Let `none` simulate a compositor with no rung, regardless of this
    /// machine's advertised capabilities.
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

    /// Select by capability. Use the same screencopy probe as the backend.
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
