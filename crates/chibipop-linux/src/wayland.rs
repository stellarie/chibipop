//! This module probes compositor capabilities at startup.
//!
//! It lists the globals that the compositor advertises.
//! It names each absent global so a compositor upgrade can restore support.
//! It reports capabilities only. Later code binds these globals.
//! It does not capture or create surfaces.

use std::collections::BTreeMap;
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

/// One global that the registry advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertised {
    /// The registry name is required to `bind` the global later.
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

/// One global that chibipop needs and the cost of its absence.
pub struct Requirement {
    pub interface: &'static str,
    pub why: &'static str,
}

/// These globals are required. Without them, this Wayland client cannot start.
/// No lower tier can replace these globals.
pub const REQUIRED: &[Requirement] = &[
    Requirement { interface: "wl_compositor", why: "creating surfaces" },
    Requirement { interface: "wl_shm", why: "software buffers for the popup" },
    Requirement { interface: "wl_seat", why: "pointer and keyboard input" },
    Requirement { interface: "wl_output", why: "monitor geometry and scale" },
];

/// The popup shell has its own tier because its absence affects only the hover loop.
/// Stock GNOME is this case. Mutter implements no layer shell.
/// The daemon stays alive without it.
/// A failed `Popup::bind` creates a diagnostic and a down Popup channel row.
/// It does not stop the daemon.
/// The settings window, the tray, the control socket, and these diagnostics still work.
/// This global name lets a compositor upgrade restore the install.
pub const LAYER_SHELL: Requirement =
    Requirement { interface: "zwlr_layer_shell_v1", why: "the popup overlay surface" };

/// These globals are wanted but not required.
/// The popup lays out and rasters in physical pixels.
/// They let the popup raster at fractional scale and declare its logical size.
/// Without them, a 1.5x desktop gets an integer-scaled panel that looks soft.
/// `Popup::bind` binds them optionally and states this tier at startup.
/// This report runs before a surface exists.
pub const SHARPNESS: &[Requirement] = &[
    Requirement { interface: "wp_fractional_scale_manager_v1", why: "the popup's fractional scale" },
    Requirement { interface: "wp_viewporter", why: "the popup's logical size at that scale" },
];

/// The capture ladder in ARCHITECTURE.md#input-ladders accepts any one rung.
/// The portal rung is not a registry global.
/// Its absence here is a note, not a verdict.
pub const CAPTURE_RUNGS: &[Requirement] = &[
    Requirement { interface: "ext_image_copy_capture_manager_v1", why: "cursor/content capture, first rung" },
    Requirement { interface: "zwlr_screencopy_manager_v1", why: "content capture, wlr fallback rung" },
];

/// Return whether this session advertises the popup shell.
/// The daemon's final verdict comes from `Popup::bind`.
/// The tray appears before a surface exists, so its Popup row must be correct on first read.
pub fn popup_shell_advertised(globals: &[Advertised]) -> bool {
    globals.iter().any(|g| g.interface == LAYER_SHELL.interface)
}

/// Return the lock and socket key.
/// Without this value, no session exists to join.
/// Do not guess which compositor owns the session.
pub fn display_name() -> anyhow::Result<String> {
    std::env::var("WAYLAND_DISPLAY")
        .map_err(|_| anyhow::anyhow!("WAYLAND_DISPLAY is unset; chibipop needs a Wayland session"))
}

/// Collect registry globals with one roundtrip on its own queue.
pub fn collect_globals(conn: &Connection) -> anyhow::Result<Vec<Advertised>> {
    struct Snapshot(Vec<Advertised>);

    impl Dispatch<wl_registry::WlRegistry, ()> for Snapshot {
        fn event(
            state: &mut Snapshot,
            _: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Snapshot>,
        ) {
            if let wl_registry::Event::Global { name, interface, version } = event {
                state.0.push(Advertised { name, interface, version });
            }
        }
    }

    let mut queue = conn.new_event_queue::<Snapshot>();
    let _registry = conn.display().get_registry(&queue.handle(), ());
    let mut snapshot = Snapshot(Vec::new());
    queue.roundtrip(&mut snapshot)?;
    Ok(snapshot.0)
}

/// Build the startup report.
/// Add one line for each registry entry.
/// State the actual cost of each absent capability.
/// The report has three tiers:
/// - a compositor that cannot run chibipop.
/// - a compositor that supports all channels except the hover loop.
/// - a compositor that supports the popup only at an integer scale.
///
/// These cases need different messages.
/// A GNOME user must not see "cannot run" when only the hover loop lacks support.
pub fn report(globals: &[Advertised]) -> Vec<String> {
    let advertised: BTreeMap<&str, u32> =
        globals.iter().map(|g| (g.interface.as_str(), g.version)).collect();

    let mut lines = vec![format!("wayland: {} globals advertised", globals.len())];
    for req in REQUIRED {
        match advertised.get(req.interface) {
            Some(version) => lines.push(format!("wayland: {} v{} - ok", req.interface, version)),
            None => lines.push(format!(
                "wayland: MISSING {} - needed for {}; chibipop cannot run on this compositor",
                req.interface, req.why
            )),
        }
    }
    match advertised.get(LAYER_SHELL.interface) {
        Some(version) => {
            lines.push(format!("wayland: {} v{version} - ok", LAYER_SHELL.interface));
        }
        None => lines.push(format!(
            "wayland: MISSING {} - needed for {}; the hover loop is unsupported on this \
             compositor (settings, tray, trigger and these diagnostics still run), and a \
             compositor that adds it self-heals this install",
            LAYER_SHELL.interface, LAYER_SHELL.why
        )),
    }
    for req in SHARPNESS {
        match advertised.get(req.interface) {
            Some(version) => lines.push(format!("wayland: {} v{} - ok", req.interface, version)),
            None => lines.push(format!(
                "wayland: {} missing - wanted for {}; the popup falls back to the output's \
                 integer scale and a fractional-scale desktop will look soft",
                req.interface, req.why
            )),
        }
    }
    let mut any_rung = false;
    for rung in CAPTURE_RUNGS {
        if let Some(version) = advertised.get(rung.interface) {
            lines.push(format!("wayland: {} v{} - {}", rung.interface, version, rung.why));
            any_rung = true;
        }
    }
    if !any_rung {
        lines.push(format!(
            "wayland: no capture global advertised ({}) - the portal ladder will be the only capture option",
            CAPTURE_RUNGS.iter().map(|r| r.interface).collect::<Vec<_>>().join(", ")
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advertised(names: &[&str]) -> Vec<Advertised> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| Advertised { name: i as u32 + 1, interface: n.to_string(), version: 1 })
            .collect()
    }

    const FULL: &[&str] = &[
        "wl_compositor",
        "wl_shm",
        "wl_seat",
        "wl_output",
        "zwlr_layer_shell_v1",
        "zwlr_screencopy_manager_v1",
        "wp_fractional_scale_manager_v1",
        "wp_viewporter",
    ];

    #[test]
    fn a_full_compositor_reports_no_missing_globals() {
        let lines = report(&advertised(FULL));
        assert!(!lines.iter().any(|l| l.contains("MISSING")), "{lines:?}");
    }

    /// This test models stock GNOME.
    /// Mutter advertises every listed global except the layer shell.
    /// The test names that global and its cost.
    /// The report must not claim that the app cannot run.
    /// The daemon stays up and the settings window opens.
    /// That behavior matches docs/LINUX.md § GNOME.
    #[test]
    fn a_missing_layer_shell_costs_the_hover_loop_and_not_the_app() {
        let names: Vec<&str> = FULL.iter().copied().filter(|n| *n != LAYER_SHELL.interface).collect();
        let lines = report(&advertised(&names));
        let missing: Vec<&String> = lines.iter().filter(|l| l.contains("MISSING")).collect();
        assert_eq!(1, missing.len(), "{lines:?}");
        assert!(missing[0].contains("zwlr_layer_shell_v1"), "{missing:?}");
        assert!(missing[0].contains("popup overlay"), "{missing:?}");
        assert!(missing[0].contains("hover loop is unsupported"), "{missing:?}");
        assert!(missing[0].contains("settings"), "{missing:?}");
        assert!(
            !lines.iter().any(|l| l.contains("cannot run")),
            "a layer-shell-less compositor still runs chibipop: {lines:?}"
        );
        assert!(!popup_shell_advertised(&advertised(&names)));
        assert!(popup_shell_advertised(&advertised(FULL)));
    }

    /// A hard requirement is the only absence that can produce "cannot run".
    /// The fatal line names that requirement and no other detail.
    /// The user can then act on that line.
    #[test]
    fn only_a_missing_hard_requirement_is_a_fatal_verdict() {
        for req in REQUIRED {
            let names: Vec<&str> = FULL.iter().copied().filter(|n| *n != req.interface).collect();
            let lines = report(&advertised(&names));
            let fatal: Vec<&String> = lines.iter().filter(|l| l.contains("cannot run")).collect();
            assert_eq!(1, fatal.len(), "{lines:?}");
            assert!(fatal[0].contains(req.interface), "{fatal:?}");
            assert!(fatal[0].contains(req.why), "{fatal:?}");
        }
    }

    /// The fractional-scale pair produces a sharpness note, not a verdict.
    /// `Popup::bind` falls back to the output's integer scale.
    /// A fatal report would contradict the binary.
    /// Headless cage with wlroots 0.20 showed this case.
    /// It advertises `wp_viewporter` without a fractional-scale manager.
    #[test]
    fn a_missing_fractional_scale_is_a_softness_note_not_a_verdict() {
        let names: Vec<&str> = FULL
            .iter()
            .copied()
            .filter(|n| *n != "wp_fractional_scale_manager_v1")
            .collect();
        let lines = report(&advertised(&names));
        let note = lines
            .iter()
            .find(|l| l.contains("wp_fractional_scale_manager_v1"))
            .unwrap_or_else(|| panic!("{lines:?}"));
        assert!(note.contains("missing"), "{note}");
        assert!(note.contains("look soft"), "{note}");
        assert!(!note.contains("MISSING"), "a soft fallback is not a MISSING: {note}");
        assert!(!lines.iter().any(|l| l.contains("cannot run")), "{lines:?}");
    }

    /// This test records a degraded session instead of an imagined one.
    /// Headless `cage` with wlroots 0.20 reports this result for `chibipop probe`:
    /// no layer shell, no fractional-scale manager, a viewporter, and wlr-screencopy.
    /// One fatal line would be incorrect.
    /// Capture works, the cursor ladder still has rungs, and only the popup is unavailable.
    #[test]
    fn the_observed_cage_session_reads_as_hover_unsupported_and_nothing_worse() {
        let lines = report(&advertised(&[
            "wl_compositor",
            "wl_shm",
            "wl_seat",
            "wl_output",
            "wp_viewporter",
            "zwlr_screencopy_manager_v1",
        ]));
        assert!(!lines.iter().any(|l| l.contains("cannot run")), "{lines:?}");
        let missing: Vec<&String> = lines.iter().filter(|l| l.contains("MISSING")).collect();
        assert_eq!(1, missing.len(), "only the layer shell is a MISSING here: {lines:?}");
        assert!(missing[0].contains(LAYER_SHELL.interface), "{missing:?}");
        assert!(
            lines.iter().any(|l| l.contains("zwlr_screencopy_manager_v1") && l.contains("fallback rung")),
            "{lines:?}"
        );
    }

    #[test]
    fn no_capture_rung_points_at_the_portal_ladder() {
        let lines = report(&advertised(&["wl_compositor", "wl_shm", "wl_seat", "wl_output", "zwlr_layer_shell_v1"]));
        assert!(
            lines.iter().any(|l| l.contains("portal ladder") && l.contains("ext_image_copy_capture_manager_v1")),
            "{lines:?}"
        );
    }
}
