//! The Linux cursor channel (ARCHITECTURE.md#input-ladders) has one ladder.
//! Startup selects the first available rung in probe order. It uses advertised
//! globals, portal state, and the Hyprland environment signal.
//! Every rung feeds the same seam.
//! A global-physical-pixel position becomes core `Event::CursorMoved`.
//!
//! Test hooks are documented and used by cursor-channel smoke tests:
//! - `CHIBIPOP_CURSOR_CHANNEL=auto|image-copy|portal|hyprctl|none` keeps
//!   capability selection for `auto` and forces a rung or the empty ladder
//!   for the other values.
//! - `CHIBIPOP_CURSOR_TRACE=1` logs every position sample, poll interval,
//!   and dwell deadline. The logfile shows the full hover cadence.
//!   The logfile also shows the decay.

pub mod hyprctl;
pub mod image_copy;
pub mod outputs;

use crate::wayland::Advertised;

/// The power budget (ARCHITECTURE.md#hover-cadence) stores every hover
/// cadence number. No setting controls these numbers. This matches Windows.
pub mod budget {
    use std::time::Duration;

    /// A parked cursor causes no wakeups for event-driven cursor rungs.
    pub const IDLE_WAKEUPS_PER_SEC: u32 = 0;
    /// Dwell re-check while a popup is shown. The deadline is 250 ms.
    /// Keep this budget with the other cadence values. The capture path
    /// races damage at the same deadline, and the daemon's watch fires then.
    pub const DWELL_MAX_WAKEUPS_PER_SEC: u32 = 4;
    /// Use this budget as the deadline. One period gives one wakeup.
    /// Do not define a second number that can drift from the first.
    pub const DWELL: Duration =
        Duration::from_millis(1000 / DWELL_MAX_WAKEUPS_PER_SEC as u64);
    /// hyprctl rung: poll every 20 ms while the cursor moves.
    pub const POLL_ACTIVE: Duration = Duration::from_millis(20);
    /// hyprctl rung: poll every 150 ms after the cursor stays still.
    pub const POLL_IDLE: Duration = Duration::from_millis(150);
    /// Use this quiet period before the idle interval. The first observed
    /// move returns the cadence to `POLL_ACTIVE`.
    pub const POLL_DECAY_AFTER: Duration = Duration::from_secs(5);
}

/// Rung 1 needs both capture-source globals. The unsupported diagnostic prints
/// their names, so a compositor upgrade can restore the rung
/// (ARCHITECTURE.md#input-ladders).
pub const IMAGE_COPY_CAPTURE_GLOBAL: &str = "ext_image_copy_capture_manager_v1";
pub const OUTPUT_CAPTURE_SOURCE_GLOBAL: &str = "ext_output_image_capture_source_manager_v1";

/// Define the ladder in probe order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1: ext-image-copy-capture pointer cursor session.
    /// Event-driven operation causes zero idle wakeups.
    ImageCopyCapture,
    /// Rung 2: portal ScreenCast `cursor_mode=METADATA`. The rung uses the
    /// PipeWire stream that the portal capture backend already opened.
    /// Cursor positions need no extra consent.
    PortalMetadata,
    /// Rung 3: `hyprctl cursorpos` uses an adaptive poll.
    /// A non-empty `HYPRLAND_INSTANCE_SIGNATURE` enables this rung.
    /// The variable is a signal, not proof of the current compositor identity.
    HyprctlPoll,
}

/// Describe registry globals, portal state, and Hyprland environment state
/// for the cursor ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub image_copy_capture: bool,
    pub output_capture_source: bool,
    /// `portal_metadata` is true when the selected Portal capture backend provides
    /// `METADATA` cursor mode. This state does not come from a registry global.
    pub portal_metadata: bool,
    /// `hyprland` is true when `HYPRLAND_INSTANCE_SIGNATURE` is non-empty.
    /// This value signals Hyprland, but does not prove the current compositor.
    pub hyprland: bool,
}

impl Capabilities {
    pub fn scan(globals: &[Advertised], portal_metadata: bool, hyprland: bool) -> Capabilities {
        let has = |name: &str| globals.iter().any(|g| g.interface == name);
        Capabilities {
            image_copy_capture: has(IMAGE_COPY_CAPTURE_GLOBAL),
            output_capture_source: has(OUTPUT_CAPTURE_SOURCE_GLOBAL),
            portal_metadata,
            hyprland,
        }
    }
}

/// Override the cursor ladder with `CHIBIPOP_CURSOR_CHANNEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderOverride {
    /// Keep capability-based selection.
    Auto,
    /// Force rung 1. Return `Unsupported` instead of selecting a lower rung.
    ImageCopy,
    /// Force rung 2. Return `Unsupported` when portal `METADATA` is absent.
    Portal,
    /// Force rung 3. Bypass image-copy and portal rungs.
    Hyprctl,
    /// Treat the ladder as empty. Exercise the unsupported path.
    None,
}

impl LadderOverride {
    /// Read this environment variable:
    /// `auto|image-copy|portal|hyprctl|none`.
    pub const ENV: &'static str = "CHIBIPOP_CURSOR_CHANNEL";

    pub fn parse(value: &str) -> Option<LadderOverride> {
        match value {
            "auto" => Some(LadderOverride::Auto),
            "image-copy" => Some(LadderOverride::ImageCopy),
            "portal" => Some(LadderOverride::Portal),
            "hyprctl" => Some(LadderOverride::Hyprctl),
            "none" => Some(LadderOverride::None),
            _ => Option::None,
        }
    }
    /// Return the override and a diagnostic when the value is unknown.
    pub fn from_env() -> (LadderOverride, Option<String>) {
        match std::env::var(Self::ENV) {
            Err(_) => (LadderOverride::Auto, Option::None),
            Ok(v) => match Self::parse(&v) {
                Some(ov) => (ov, Option::None),
                Option::None => (
                    LadderOverride::Auto,
                    Some(format!(
                        "cursor: ignoring {}={v:?}; expected auto|image-copy|portal|hyprctl|none",
                        Self::ENV
                    )),
                ),
            },
        }
    }
}

/// Record the result of `select`: a live rung or the exact missing capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Rung(Rung),
    /// Hover is unsupported. The app stays up. `missing` names each absent
    /// capability in the startup diagnostic.
    Unsupported { missing: Vec<String> },
}

impl Selection {
    /// Return the one startup line that the daemon logs for the cursor channel.
    pub fn startup_line(&self) -> String {
        match self {
            Selection::Rung(Rung::ImageCopyCapture) => format!(
                "cursor: rung 1 ext-image-copy-capture cursor session (event-driven, {} idle wakeups/s)",
                budget::IDLE_WAKEUPS_PER_SEC
            ),
            Selection::Rung(Rung::PortalMetadata) => format!(
                "cursor: rung 2 portal ScreenCast cursor_mode=METADATA on the capture stream (event-driven, {} idle wakeups/s; no extra consent)",
                budget::IDLE_WAKEUPS_PER_SEC
            ),
            Selection::Rung(Rung::HyprctlPoll) => format!(
                "cursor: rung 3 hyprctl cursorpos adaptive polling ({} ms active -> {} ms after {} s quiet; Hyprland identity exception)",
                budget::POLL_ACTIVE.as_millis(),
                budget::POLL_IDLE.as_millis(),
                budget::POLL_DECAY_AFTER.as_secs()
            ),
            Selection::Unsupported { missing } => format!(
                "cursor: hover unsupported - missing {}; a compositor upgrade advertising the missing capability self-heals this install",
                missing.join(", ")
            ),
        }
    }
}

/// Return rung-1 globals that `caps` lacks, with exact protocol names.
fn missing_globals(caps: &Capabilities) -> Vec<String> {
    let mut missing = Vec::new();
    if !caps.image_copy_capture {
        missing.push(IMAGE_COPY_CAPTURE_GLOBAL.to_string());
    }
    if !caps.output_capture_source {
        missing.push(OUTPUT_CAPTURE_SOURCE_GLOBAL.to_string());
    }
    missing
}

/// Select a rung in probe order. `Auto` uses the scanned capabilities.
/// Missing portal `METADATA` skips only the portal rung.
/// A forced override checks only its named rung and does not fall through.
pub fn select(caps: &Capabilities, ov: LadderOverride) -> Selection {
    match ov {
        LadderOverride::Auto => {
            if caps.image_copy_capture && caps.output_capture_source {
                return Selection::Rung(Rung::ImageCopyCapture);
            }
            if caps.portal_metadata {
                return Selection::Rung(Rung::PortalMetadata);
            }
            if caps.hyprland {
                return Selection::Rung(Rung::HyprctlPoll);
            }
            Selection::Unsupported { missing: missing_globals(caps) }
        }
        LadderOverride::ImageCopy => {
            if caps.image_copy_capture && caps.output_capture_source {
                Selection::Rung(Rung::ImageCopyCapture)
            } else {
                Selection::Unsupported { missing: missing_globals(caps) }
            }
        }
        LadderOverride::Portal => {
            if caps.portal_metadata {
                Selection::Rung(Rung::PortalMetadata)
            } else {
                Selection::Unsupported {
                    missing: vec![
                        "org.freedesktop.portal.ScreenCast cursor_mode=METADATA (portal capture backend not selected or METADATA unavailable)".to_string(),
                    ],
                }
            }
        }
        LadderOverride::Hyprctl => {
            if caps.hyprland {
                Selection::Rung(Rung::HyprctlPoll)
            } else {
                Selection::Unsupported {
                    missing: vec!["HYPRLAND_INSTANCE_SIGNATURE (not a Hyprland session)".to_string()],
                }
            }
        }
        // Simulate an empty ladder. Report rung 1 needs as if nothing were
        // advertised.
        LadderOverride::None => Selection::Unsupported {
            missing: vec![
                IMAGE_COPY_CAPTURE_GLOBAL.to_string(),
                OUTPUT_CAPTURE_SOURCE_GLOBAL.to_string(),
            ],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Capabilities = Capabilities {
        image_copy_capture: true,
        output_capture_source: true,
        portal_metadata: true,
        hyprland: true,
    };
    const PORTAL_ONLY: Capabilities = Capabilities {
        image_copy_capture: false,
        output_capture_source: false,
        portal_metadata: true,
        hyprland: false,
    };
    const HYPR_ONLY: Capabilities = Capabilities {
        image_copy_capture: false,
        output_capture_source: false,
        portal_metadata: false,
        hyprland: true,
    };
    const NOTHING: Capabilities = Capabilities {
        image_copy_capture: false,
        output_capture_source: false,
        portal_metadata: false,
        hyprland: false,
    };

    #[test]
    fn the_budget_holds_its_documented_numbers() {
        assert_eq!(0, budget::IDLE_WAKEUPS_PER_SEC);
        assert_eq!(4, budget::DWELL_MAX_WAKEUPS_PER_SEC);
        assert_eq!(std::time::Duration::from_millis(250), budget::DWELL);
        // The daemon's dwell watch and the wlr backend's damage race share one
        // cadence. They are not two values that happen to agree today.
        assert_eq!(crate::capture::pacing::DWELL_DEADLINE, budget::DWELL);
        assert!(budget::POLL_ACTIVE < budget::POLL_IDLE);
        // Keep the idle scan within the <= 7 wakeups/s budget.
        assert!(1000 / budget::POLL_IDLE.as_millis() <= 7);
    }

    #[test]
    fn full_capabilities_select_the_capture_session() {
        assert_eq!(Selection::Rung(Rung::ImageCopyCapture), select(&ALL, LadderOverride::Auto));
    }

    #[test]
    fn hyprland_without_capture_globals_falls_to_polling() {
        assert_eq!(Selection::Rung(Rung::HyprctlPoll), select(&HYPR_ONLY, LadderOverride::Auto));
    }

    /// A portal-only session (GNOME) has neither capture globals nor Hyprland.
    /// The portal capture backend has METADATA.
    #[test]
    fn a_portal_metadata_stream_is_rung_two() {
        assert_eq!(
            Selection::Rung(Rung::PortalMetadata),
            select(&PORTAL_ONLY, LadderOverride::Auto)
        );
    }

    /// Ladder order for both seams of the new rung. The promptless capture
    /// session stays first, and the poll rung stays last.
    #[test]
    fn rung_two_sits_between_the_capture_session_and_polling() {
        // All rungs are available, so rung 1 remains the answer.
        assert_eq!(Selection::Rung(Rung::ImageCopyCapture), select(&ALL, LadderOverride::Auto));

        // Portal METADATA on Hyprland selects rung 2 instead of the poll rung.
        let caps = Capabilities { portal_metadata: true, ..HYPR_ONLY };
        assert_eq!(Selection::Rung(Rung::PortalMetadata), select(&caps, LadderOverride::Auto));
    }

    /// The diagnostic names the exact missing globals. A compositor upgrade
    /// can then restore the rung.
    #[test]
    fn no_rung_names_both_missing_globals() {
        let Selection::Unsupported { missing } = select(&NOTHING, LadderOverride::Auto) else {
            panic!("expected Unsupported");
        };
        assert_eq!(vec![IMAGE_COPY_CAPTURE_GLOBAL, OUTPUT_CAPTURE_SOURCE_GLOBAL], missing);
    }

    #[test]
    fn a_partial_rung_names_only_what_is_absent() {
        let caps = Capabilities {
            image_copy_capture: true,
            output_capture_source: false,
            portal_metadata: false,
            hyprland: false,
        };
        let Selection::Unsupported { missing } = select(&caps, LadderOverride::Auto) else {
            panic!("expected Unsupported");
        };
        assert_eq!(vec![OUTPUT_CAPTURE_SOURCE_GLOBAL], missing);
    }

    #[test]
    fn the_forced_overrides_pin_their_rung() {
        assert_eq!(Selection::Rung(Rung::HyprctlPoll), select(&ALL, LadderOverride::Hyprctl));
        assert_eq!(
            Selection::Rung(Rung::ImageCopyCapture),
            select(&ALL, LadderOverride::ImageCopy)
        );
        assert!(matches!(select(&ALL, LadderOverride::None), Selection::Unsupported { .. }));
        assert!(matches!(
            select(&NOTHING, LadderOverride::ImageCopy),
            Selection::Unsupported { .. }
        ));
        assert_eq!(Selection::Rung(Rung::PortalMetadata), select(&ALL, LadderOverride::Portal));
    }

    /// The forced portal rung fails instead of using a lower rung.
    /// The METADATA stream defines this rung. Without it, nothing can run.
    #[test]
    fn the_portal_override_names_the_absent_metadata_stream() {
        let Selection::Unsupported { missing } = select(&HYPR_ONLY, LadderOverride::Portal) else {
            panic!("expected Unsupported");
        };
        assert_eq!(
            vec![
                "org.freedesktop.portal.ScreenCast cursor_mode=METADATA (portal capture backend not selected or METADATA unavailable)"
            ],
            missing
        );
    }

    /// The rung-2 line names its mechanism, zero idle cost, and zero extra
    /// consent on one greppable line.
    #[test]
    fn the_rung_two_line_stays_greppable() {
        let line = select(&PORTAL_ONLY, LadderOverride::Auto).startup_line();
        assert!(line.contains("rung 2"), "{line}");
        assert!(line.contains("cursor_mode=METADATA"), "{line}");
        assert!(line.contains("0 idle wakeups/s"), "{line}");
        assert!(line.contains("no extra consent"), "{line}");
        assert!(!line.contains('\n'), "{line}");
    }

    #[test]
    fn the_unsupported_line_stays_greppable() {
        let line = select(&NOTHING, LadderOverride::Auto).startup_line();
        assert!(line.contains("hover unsupported"), "{line}");
        assert!(line.contains(IMAGE_COPY_CAPTURE_GLOBAL), "{line}");
    }

    #[test]
    fn override_parsing_covers_the_documented_values() {
        assert_eq!(Some(LadderOverride::Auto), LadderOverride::parse("auto"));
        assert_eq!(Some(LadderOverride::ImageCopy), LadderOverride::parse("image-copy"));
        assert_eq!(Some(LadderOverride::Hyprctl), LadderOverride::parse("hyprctl"));
        assert_eq!(Some(LadderOverride::Portal), LadderOverride::parse("portal"));
        assert_eq!(Some(LadderOverride::None), LadderOverride::parse("none"));
        assert_eq!(Option::None, LadderOverride::parse("evdev"));
    }

    #[test]
    fn capabilities_scan_matches_exact_interface_names() {
        let globals = vec![
            Advertised { name: 1, interface: IMAGE_COPY_CAPTURE_GLOBAL.into(), version: 1 },
            Advertised { name: 2, interface: OUTPUT_CAPTURE_SOURCE_GLOBAL.into(), version: 1 },
        ];
        let caps = Capabilities::scan(&globals, false, false);
        assert!(caps.image_copy_capture && caps.output_capture_source);
        assert!(!caps.portal_metadata, "rung 2 is not a registry global");
        assert!(!Capabilities::scan(&[], false, true).image_copy_capture);
        assert!(Capabilities::scan(&[], true, false).portal_metadata);
    }
}
