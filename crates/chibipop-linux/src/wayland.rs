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

/// Non-negotiable: without these the product cannot exist on this
/// compositor at all.
pub const REQUIRED: &[Requirement] = &[
    Requirement { interface: "wl_compositor", why: "creating surfaces" },
    Requirement { interface: "wl_shm", why: "software buffers for the popup" },
    Requirement { interface: "wl_seat", why: "pointer and keyboard input" },
    Requirement { interface: "wl_output", why: "monitor geometry and scale" },
    Requirement { interface: "zwlr_layer_shell_v1", why: "the popup overlay surface" },
];

/// The capture ladder (ADR-0003): any one rung will do; the portal rung
/// is not a registry global, so its absence here is a note, not a verdict.
pub const CAPTURE_RUNGS: &[Requirement] = &[
    Requirement { interface: "ext_image_copy_capture_manager_v1", why: "cursor/content capture, first rung" },
    Requirement { interface: "zwlr_screencopy_manager_v1", why: "content capture, wlr fallback rung" },
];

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

/// The startup report, one diagnostic line per entry.
pub fn report(globals: &[Advertised]) -> Vec<String> {
    let advertised: BTreeMap<&str, u32> =
        globals.iter().map(|g| (g.interface.as_str(), g.version)).collect();

    let mut lines = vec![format!("wayland: {} globals advertised", globals.len())];
    for req in REQUIRED {
        match advertised.get(req.interface) {
            Some(version) => lines.push(format!("wayland: {} v{} - ok", req.interface, version)),
            None => lines.push(format!(
                "wayland: MISSING {} - needed for {}; this compositor cannot run chibipop",
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
    ];

    #[test]
    fn a_full_compositor_reports_no_missing_globals() {
        let lines = report(&advertised(FULL));
        assert!(!lines.iter().any(|l| l.contains("MISSING")), "{lines:?}");
    }

    /// The exact missing global is named, so an upgrade self-heals.
    #[test]
    fn a_missing_layer_shell_is_named_with_its_cost() {
        let names: Vec<&str> = FULL.iter().copied().filter(|n| *n != "zwlr_layer_shell_v1").collect();
        let lines = report(&advertised(&names));
        let missing: Vec<&String> = lines.iter().filter(|l| l.contains("MISSING")).collect();
        assert_eq!(1, missing.len(), "{lines:?}");
        assert!(missing[0].contains("zwlr_layer_shell_v1"), "{missing:?}");
        assert!(missing[0].contains("popup overlay"), "{missing:?}");
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
