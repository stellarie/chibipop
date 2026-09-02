//! The Linux cursor channel (ARCHITECTURE.md#input-ladders): one
//! ladder, first rung that probes successfully wins, selected by
//! *advertised capability* at startup. Every rung feeds the same seam:
//! a global-physical-pixel position becomes core `Event::CursorMoved`
//! — core vocabulary unchanged, no evdev anywhere.
//!
//! Test hooks (documented, used by the ticket-33 smoke tests):
//! - `CHIBIPOP_CURSOR_CHANNEL=auto|image-copy|portal|hyprctl|none` forces a
//!   rung (or the empty ladder, to exercise the unsupported
//!   diagnostic) instead of the capability-selected one.
//! - `CHIBIPOP_CURSOR_TRACE=1` logs every position sample, poll
//!   interval and dwell deadline, so the whole hover cadence -
//!   decay included - is observable in the logfile.

pub mod hyprctl;
pub mod image_copy;
pub mod outputs;

use crate::wayland::Advertised;

/// The power budget (ARCHITECTURE.md#hover-cadence) — the single home
/// for every hover cadence number. No settings knobs, matching
/// Windows.
pub mod budget {
    use std::time::Duration;

    /// Event-driven cursor rungs, cursor parked: nothing runs.
    pub const IDLE_WAKEUPS_PER_SEC: u32 = 0;
    /// Dwell re-check while a popup is shown (the 250 ms deadline).
    /// Budgeted here with the rest; the capture tickets (30/31) race
    /// damage on the same deadline, and the daemon's watch fires at it.
    pub const DWELL_MAX_WAKEUPS_PER_SEC: u32 = 4;
    /// That budget as the deadline itself: one wakeup per period, and
    /// no second number that could drift from the first.
    pub const DWELL: Duration =
        Duration::from_millis(1000 / DWELL_MAX_WAKEUPS_PER_SEC as u64);
    /// hyprctl rung: poll fast while the cursor moves...
    pub const POLL_ACTIVE: Duration = Duration::from_millis(20);
    /// ...and decay to a slow scan once it has been quiet...
    pub const POLL_IDLE: Duration = Duration::from_millis(150);
    /// ...for this long. The first observed move bursts back to
    /// `POLL_ACTIVE`.
    pub const POLL_DECAY_AFTER: Duration = Duration::from_secs(5);
}

/// Rung 1 needs both capture-source plumbing globals; their names are
/// what the unsupported diagnostic prints, so a compositor upgrade
/// self-heals the install (ARCHITECTURE.md#input-ladders).
pub const IMAGE_COPY_CAPTURE_GLOBAL: &str = "ext_image_copy_capture_manager_v1";
pub const OUTPUT_CAPTURE_SOURCE_GLOBAL: &str = "ext_output_image_capture_source_manager_v1";

/// The ladder, in probe order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1: ext-image-copy-capture pointer cursor session.
    /// Event-driven — zero idle wakeups.
    ImageCopyCapture,
    /// Rung 2: portal ScreenCast `cursor_mode=METADATA`, riding the
    /// PipeWire stream the portal capture backend already opened, so
    /// cursor tracking costs no extra consent.
    PortalMetadata,
    /// Rung 3: `hyprctl cursorpos` adaptive polling, gated on
    /// HYPRLAND_INSTANCE_SIGNATURE — the one deliberate identity-based
    /// exception to "never compositor identity"
    /// (ARCHITECTURE.md#capture-and-masking).
    HyprctlPoll,
}

/// What the compositor advertises, as far as the cursor ladder cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub image_copy_capture: bool,
    pub output_capture_source: bool,
    /// The portal capture backend is the selected one AND its
    /// `AvailableCursorModes` advertises METADATA — the rung rides that
    /// stream, so it cannot exist without it.
    pub portal_metadata: bool,
    /// HYPRLAND_INSTANCE_SIGNATURE is set and non-empty.
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

/// The `CHIBIPOP_CURSOR_CHANNEL` test hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderOverride {
    Auto,
    /// Force rung 1 (fail rather than fall through).
    ImageCopy,
    /// Force rung 2, pretending rung 1 is absent.
    Portal,
    /// Force rung 3, pretending rung 1 is absent.
    Hyprctl,
    /// Pretend the ladder is empty: exercise the unsupported path.
    None,
}

impl LadderOverride {
    /// The environment variable this hook reads:
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

    /// The override and, when the value was unrecognized, a diagnostic.
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

/// What `select` decided: a live rung, or exactly what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Rung(Rung),
    /// Hover unsupported. The app stays up; `missing` names the exact
    /// absent capabilities for the startup diagnostic.
    Unsupported { missing: Vec<String> },
}

impl Selection {
    /// The one startup line the daemon logs for the cursor channel.
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

/// The rung-1 globals `caps` lacks, by exact protocol name.
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

/// Walk the ladder (ARCHITECTURE.md#input-ladders). Capability-first;
/// the hyprctl rung's identity gate is the documented exception.
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
        // Simulated empty ladder: report rung 1's needs as if nothing
        // were advertised.
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
        // The daemon's dwell watch and the wlr backend's damage race
        // are one cadence, not two that happen to agree today.
        assert_eq!(crate::capture::pacing::DWELL_DEADLINE, budget::DWELL);
        assert!(budget::POLL_ACTIVE < budget::POLL_IDLE);
        // The idle scan stays within the <= 7 wakeups/s budget.
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

    /// A portal-only session (GNOME): no capture globals, no Hyprland,
    /// but the portal capture backend is up with METADATA.
    #[test]
    fn a_portal_metadata_stream_is_rung_two() {
        assert_eq!(
            Selection::Rung(Rung::PortalMetadata),
            select(&PORTAL_ONLY, LadderOverride::Auto)
        );
    }

    /// Ladder order, both seams of the new rung: the promptless
    /// capture session still outranks it, and it still outranks
    /// polling.
    #[test]
    fn rung_two_sits_between_the_capture_session_and_polling() {
        // Every rung available at once: rung 1 is still the answer.
        assert_eq!(Selection::Rung(Rung::ImageCopyCapture), select(&ALL, LadderOverride::Auto));

        // Portal METADATA on Hyprland: rung 2 beats the polling rung.
        let caps = Capabilities { portal_metadata: true, ..HYPR_ONLY };
        assert_eq!(Selection::Rung(Rung::PortalMetadata), select(&caps, LadderOverride::Auto));
    }

    /// The diagnostic names the exact missing globals so an upgrade
    /// self-heals.
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

    /// The forced portal rung fails honestly rather than falling
    /// through: the METADATA stream is the rung, so without it there is
    /// nothing to pin.
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

    /// The rung-2 line names its mechanism, its zero idle cost and its
    /// zero extra consent, on one greppable line.
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
