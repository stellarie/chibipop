//! Startup capability probe scaffold (ADR-0002/0003): list what the
//! compositor advertises, and name exactly what is missing so a
//! compositor upgrade self-heals. No capture, no surfaces yet — later
//! tickets bind these globals; this one only reports.

use std::collections::BTreeMap;
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

/// One advertised registry global.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertised {
    /// The registry name, needed to `bind` the global later.
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

/// A global chibipop wants, and what its absence costs.
pub struct Requirement {
    pub interface: &'static str,
    pub why: &'static str,
}

/// Non-negotiable: without these there is no Wayland client here at
/// all, and nothing left to degrade *to*.
pub const REQUIRED: &[Requirement] = &[
    Requirement { interface: "wl_compositor", why: "creating surfaces" },
    Requirement { interface: "wl_shm", why: "software buffers for the popup" },
    Requirement { interface: "wl_seat", why: "pointer and keyboard input" },
    Requirement { interface: "wl_output", why: "monitor geometry and scale" },
];

/// The popup's shell, on its own tier because its absence costs exactly
/// one thing: the hover loop. Stock GNOME is this case - Mutter
/// implements no layer shell - and the daemon deliberately stays up
/// without it (a failed `Popup::bind` is a diagnostic and a down Popup
/// channel row, never an exit), so the settings window, the tray, the
/// control socket and these diagnostics all still work. Naming the
/// global is what lets a compositor upgrade self-heal the install.
pub const LAYER_SHELL: Requirement =
    Requirement { interface: "zwlr_layer_shell_v1", why: "the popup overlay surface" };

/// Wanted, not needed. ADR-0004 lays out and rasters the popup in
/// physical pixels, so these two are what let it raster at the
/// fractional scale and declare the logical size that stands for;
/// without them a 1.5x desktop can only be served an integer-scaled
/// panel that looks soft. `Popup::bind` binds them optionally and says
/// the same thing in its own note - this tier is that posture stated at
/// startup, before any surface exists.
pub const SHARPNESS: &[Requirement] = &[
    Requirement { interface: "wp_fractional_scale_manager_v1", why: "the popup's fractional scale" },
    Requirement { interface: "wp_viewporter", why: "the popup's logical size at that scale" },
];

/// The capture ladder (ADR-0003): any one rung will do; the portal rung
/// is not a registry global, so its absence here is a note, not a verdict.
pub const CAPTURE_RUNGS: &[Requirement] = &[
    Requirement { interface: "ext_image_copy_capture_manager_v1", why: "cursor/content capture, first rung" },
    Requirement { interface: "zwlr_screencopy_manager_v1", why: "content capture, wlr fallback rung" },
];

/// Does this session advertise the popup's shell? The daemon's real
/// verdict is whether `Popup::bind` succeeds, but the tray is published
/// before any surface exists and its Popup row has to be true the first
/// time a user reads it.
pub fn popup_shell_advertised(globals: &[Advertised]) -> bool {
    globals.iter().any(|g| g.interface == LAYER_SHELL.interface)
}

/// The lock/socket key. Required: without it there is no session to
/// join, and "which compositor" must never be guessed.
pub fn display_name() -> anyhow::Result<String> {
    std::env::var("WAYLAND_DISPLAY")
        .map_err(|_| anyhow::anyhow!("WAYLAND_DISPLAY is unset; chibipop needs a Wayland session"))
}

/// Collect the registry via one roundtrip on its own queue.
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

/// The startup report: one line per entry, and every absence priced at
/// what it actually costs. Three tiers, because a compositor that
/// cannot run chibipop at all, one that can run everything but the
/// hover loop (stock GNOME), and one that only draws the popup softly
/// are three different messages - and telling a GNOME user the app
/// "cannot run" when its settings window is about to open is how a
/// clear diagnostic becomes a support ticket.
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

    /// Stock GNOME, which is the whole reason this tier exists: Mutter
    /// advertises everything here except the layer shell. The exact
    /// global is named (so a Mutter that grows it self-heals the
    /// install), the cost is named as the hover loop, and the line must
    /// NOT claim the app cannot run - the daemon stays up and the
    /// settings window opens, which is what docs/LINUX.md § GNOME
    /// promises.
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

    /// A hard requirement is the only absence allowed to say "cannot
    /// run", and when it does it names itself and nothing else - so the
    /// one fatal line a user reads is the one they can act on.
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

    /// The fractional-scale pair is a sharpness note, not a verdict:
    /// `Popup::bind` really does fall back to the output's integer
    /// scale, so a report calling it fatal contradicts the binary it
    /// ships in. Observed on headless cage (wlroots 0.20), which
    /// advertises `wp_viewporter` and no fractional-scale manager.
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

    /// A real degraded session, recorded rather than imagined: this is
    /// exactly what headless `cage` (wlroots 0.20) advertises to
    /// `chibipop probe` - no layer shell, no fractional-scale manager,
    /// but a viewporter and wlr-screencopy. One fatal-sounding line
    /// would be one too many: capture works, the cursor ladder still
    /// has rungs to try, and only the popup is out.
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
