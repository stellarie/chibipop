//! Linux screenshot target selection through `slurp` and compositor queries.
//!
//! Core uses global physical pixels. Compositors and `slurp` use logical layout
//! pixels, so this module converts at the bin seam with output geometry.
//! Commands run directly. No shell or `jq` process is part of this path.

use crate::cursor::outputs::OutputGeometry;
use anyhow::{bail, Context, Result};
use chibipop::config::{ScreenshotMode, ScreenshotWindow};
use chibipop::geom::PhysRect;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// One compositor window in global logical layout coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub identity: ScreenshotWindow,
    pub rect: LogicalRect,
}

/// A logical rectangle from a compositor query or `slurp`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A selected screenshot region and the window that supplied it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub rect: PhysRect,
    pub window: Option<ScreenshotWindow>,
}

/// A user cancellation is not a selection failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Cancelled,
    Selected(Selection),
}

/// Select one screenshot region according to `mode`.
///
/// Region modes do not need a compositor query. They work on a compositor that
/// supports `slurp` even when no Hyprland or Sway query environment is present.
/// Window modes require an exact identity supplied by a predefined rectangle.
pub fn select(mode: ScreenshotMode, geometries: &[OutputGeometry]) -> Result<Outcome> {
    match mode {
        ScreenshotMode::Region => {
            // A window query adds click targets, but a failed or unsupported
            // query must not disable arbitrary region drags.
            let windows = query_windows().unwrap_or_default();
            run_slurp(false, &windows, geometries)
        }
        ScreenshotMode::FixedRegion => {
            // The first fixed-region pick must be an actual region drag. Do
            // not provide predefined window rectangles for this mode.
            run_slurp(false, &[], geometries)
        }
        ScreenshotMode::Window | ScreenshotMode::FixedWindow => {
            let windows = query_windows()?;
            if windows.is_empty() {
                bail!("no visible windows were found for slurp")
            }
            run_slurp(true, &windows, geometries)
        }
    }
}

/// Resolve one exact window identity from a queried list.
pub fn match_window<'a>(windows: &'a [Window], target: &ScreenshotWindow) -> Result<&'a Window> {
    let mut matches = windows.iter().filter(|window| window.identity == *target);
    let Some(first) = matches.next() else {
        bail!("the saved screenshot window is absent")
    };
    if matches.next().is_some() {
        bail!("the saved screenshot window is ambiguous")
    }
    Ok(first)
}

/// Resolve a saved window target against fresh compositor geometry.
pub fn resolve_window(
    target: &ScreenshotWindow,
    geometries: &[OutputGeometry],
) -> Result<Selection> {
    let windows = query_windows()?;
    let window = match_window(&windows, target)?;
    let rect = logical_rect_to_physical(geometries, window.rect)
        .context("the saved screenshot window has invalid geometry")?;
    Ok(Selection { rect, window: Some(window.identity.clone()) })
}

/// Validate and convert a fixed physical target from configuration.
pub fn fixed_region(target: [i32; 4]) -> Result<PhysRect> {
    let rect = PhysRect { x: target[0], y: target[1], w: target[2], h: target[3] };
    if rect.w <= 0
        || rect.h <= 0
        || rect.x.checked_add(rect.w).is_none()
        || rect.y.checked_add(rect.h).is_none()
    {
        bail!("the saved screenshot region has invalid geometry")
    }
    Ok(rect)
}

/// Convert one logical layout rectangle into global physical pixels.
///
/// Each output contributes its overlap. This keeps negative origins and
/// fractional scales correct and follows the cursor/capture geometry rule.
pub fn logical_rect_to_physical(
    geometries: &[OutputGeometry],
    rect: LogicalRect,
) -> Option<PhysRect> {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.w.is_finite()
        || !rect.h.is_finite()
        || rect.w <= 0.0
        || rect.h <= 0.0
    {
        return None;
    }
    let right = rect.x + rect.w;
    let bottom = rect.y + rect.h;
    if !right.is_finite() || !bottom.is_finite() {
        return None;
    }

    let mut result = None;
    for geometry in geometries {
        let physical_h = if geometry.transform_swaps { geometry.mode_w } else { geometry.mode_h };
        if geometry.logical_w <= 0
            || geometry.logical_h <= 0
            || geometry.physical_w() <= 0
            || physical_h <= 0
        {
            continue;
        }
        let left = rect.x.max(f64::from(geometry.logical_x));
        let top = rect.y.max(f64::from(geometry.logical_y));
        let hit_right = right.min(f64::from(geometry.logical_x + geometry.logical_w));
        let hit_bottom = bottom.min(f64::from(geometry.logical_y + geometry.logical_h));
        if hit_right <= left || hit_bottom <= top {
            continue;
        }
        let from = geometry.logical_to_global(left, top);
        let to = geometry.logical_to_global(hit_right, hit_bottom);
        let piece = PhysRect {
            x: from.x,
            y: from.y,
            w: to.x - from.x,
            h: to.y - from.y,
        };
        if piece.w <= 0 || piece.h <= 0 {
            continue;
        }
        result = Some(result.map_or(piece, |old| cover(old, piece)));
    }
    result
}

fn cover(a: PhysRect, b: PhysRect) -> PhysRect {
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    PhysRect {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        w: right - a.x.min(b.x),
        h: bottom - a.y.min(b.y),
    }
}

fn run_slurp(
    restrict_to_windows: bool,
    windows: &[Window],
    geometries: &[OutputGeometry],
) -> Result<Outcome> {
    let mut command = Command::new("slurp");
    command.args(["-f", "%x,%y %wx%h %l"]);
    if restrict_to_windows {
        command.arg("-r");
    }
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::signals::unmasked(&mut command);
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("slurp is not installed")
        } else {
            anyhow::anyhow!("starting slurp: {error}")
        }
    })?;

    if let Some(mut input) = child.stdin.take() {
        // slurp can exit before it reads stdin, for example when a second
        // selector is active. A write error must still reap the child, or
        // each retry leaves a zombie for the daemon's lifetime.
        if let Err(error) = write_windows(&mut input, windows) {
            drop(input);
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("writing window rectangles to slurp");
        }
    }

    let output = reap_slurp(child)?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stdout.trim() == "selection cancelled" || stderr.trim() == "selection cancelled" {
            return Ok(Outcome::Cancelled);
        }
        let detail = if !stderr.trim().is_empty() { stderr.trim() } else { stdout.trim() };
        if detail.is_empty() {
            bail!("slurp failed with status {}", output.status)
        }
        bail!("slurp failed: {detail}")
    }
    let text = std::str::from_utf8(&output.stdout).context("slurp returned non-UTF-8 output")?;
    let (logical, selected_label) = parse_slurp_output(text)?;
    let rect = logical_rect_to_physical(geometries, logical)
        .context("slurp returned invalid or off-screen geometry")?;
    let window = selected_label
        .filter(|value| !value.is_empty())
        .map(|value| -> Result<Option<ScreenshotWindow>> {
            let index = label_index(value)?;
            let window = windows
                .get(index)
                .context("slurp returned an unknown window label")?;
            // slurp can retain a predefined label when a drag starts over a
            // window. Only an unchanged rectangle means a window click.
            if restrict_to_windows || window.rect == logical {
                Ok(Some(window.identity.clone()))
            } else {
                Ok(None)
            }
        })
        .transpose()?
        .flatten();
    if restrict_to_windows && window.is_none() {
        bail!("slurp did not return a window target")
    }
    // A click without a drag returns a one-pixel rectangle. The old selector
    // discarded such a pick, and a fixed region must not save it.
    if window.is_none() && !crate::select::meets_threshold(rect) {
        return Ok(Outcome::Cancelled);
    }
    Ok(Outcome::Selected(Selection { rect, window }))
}

fn reap_slurp(mut child: Child) -> Result<std::process::Output> {
    let deadline = Instant::now() + crate::select::PICK_TIMEOUT;
    loop {
        if child.try_wait().context("checking slurp status")?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait_with_output().context("reaping timed-out slurp")?;
            bail!("slurp timed out after {} seconds", crate::select::PICK_TIMEOUT.as_secs())
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().context("reaping slurp")
}

fn write_windows(input: &mut impl Write, windows: &[Window]) -> std::io::Result<()> {
    for (index, window) in windows.iter().enumerate() {
        writeln!(
            input,
            "{},{:.0} {:.0}x{:.0} w{index}",
            window.rect.x,
            window.rect.y,
            window.rect.w,
            window.rect.h
        )?;
    }
    Ok(())
}

fn label_index(value: &str) -> Result<usize> {
    value
        .strip_prefix('w')
        .context("slurp returned an invalid window label")?
        .parse()
        .context("slurp returned an invalid window label")
}

/// Parse the format passed to `slurp -f`.
pub fn parse_slurp_output(text: &str) -> Result<(LogicalRect, Option<&str>)> {
    let line = text.lines().find(|line| !line.trim().is_empty()).context("slurp returned no selection")?;
    let mut parts = line.splitn(3, char::is_whitespace);
    let origin = parts.next().context("slurp selection has no origin")?;
    let size = parts.next().context("slurp selection has no size")?;
    let label = parts.next().map(str::trim).filter(|value| !value.is_empty());
    let (x, y) = origin.split_once(',').context("slurp selection has invalid origin")?;
    let (w, h) = size.split_once('x').context("slurp selection has invalid size")?;
    let rect = LogicalRect {
        x: x.parse().context("slurp selection has invalid x")?,
        y: y.parse().context("slurp selection has invalid y")?,
        w: w.parse().context("slurp selection has invalid width")?,
        h: h.parse().context("slurp selection has invalid height")?,
    };
    Ok((rect, label))
}

fn query_windows() -> Result<Vec<Window>> {
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok_and(|value| !value.is_empty()) {
        return query_hyprland();
    }
    if std::env::var("SWAYSOCK").is_ok_and(|value| !value.is_empty()) {
        return query_sway();
    }
    bail!("unsupported compositor: screenshot windows need Hyprland or Sway")
}

fn command_json(program: &str, args: &[&str]) -> Result<Value> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    crate::signals::unmasked(&mut command);
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("{program} is not installed")
        } else {
            anyhow::anyhow!("starting {program}: {error}")
        }
    })?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("{program} returned invalid JSON"))
}

fn query_hyprland() -> Result<Vec<Window>> {
    let clients = command_json("hyprctl", &["-j", "clients"])?;
    let clients = clients.as_array().context("hyprctl clients returned a non-array")?;
    let monitors = command_json("hyprctl", &["-j", "monitors"])?;
    let monitors = monitors.as_array().context("hyprctl monitors returned a non-array")?;
    let active_workspaces: HashMap<i64, (i64, i64)> = monitors
        .iter()
        .filter_map(|monitor| {
            let id = monitor.get("id")?.as_i64()?;
            let active = monitor.get("activeWorkspace")?.get("id")?.as_i64()?;
            let special = monitor.get("specialWorkspace")?.get("id")?.as_i64().unwrap_or(0);
            Some((id, (active, special)))
        })
        .collect();
    Ok(clients
        .iter()
        .filter(|client| hypr_on_visible_workspace(client, &active_workspaces))
        .filter_map(hypr_window)
        .collect())
}

fn hypr_on_visible_workspace(value: &Value, active_workspaces: &HashMap<i64, (i64, i64)>) -> bool {
    // A fullscreen window suppresses its siblings. Hyprland reports them as
    // mapped and not hidden, but `visible` is false. slurp prefers the
    // smallest rectangle, so such a window would intercept the click.
    if !value.get("mapped").and_then(Value::as_bool).unwrap_or(false)
        || value.get("hidden").and_then(Value::as_bool).unwrap_or(true)
        || !value.get("visible").and_then(Value::as_bool).unwrap_or(true)
    {
        return false;
    }
    let monitor = value.get("monitor").and_then(Value::as_i64).unwrap_or(-1);
    let workspace = value
        .get("workspace")
        .and_then(|workspace| workspace.get("id"))
        .and_then(Value::as_i64);
    let Some((active, special)) = active_workspaces.get(&monitor) else { return false };
    let Some(workspace) = workspace else { return false };
    workspace == *active || (*special != 0 && workspace == *special)
}

fn hypr_window(value: &Value) -> Option<Window> {
    let at = pair(value.get("at")?)?;
    let size = pair(value.get("size")?)?;
    let identity = ScreenshotWindow {
        app_id: value.get("class")?.as_str()?.to_string(),
        title: value.get("title")?.as_str()?.to_string(),
    };
    Some(Window {
        identity,
        rect: LogicalRect { x: at.0, y: at.1, w: size.0, h: size.1 },
    })
}

fn query_sway() -> Result<Vec<Window>> {
    let value = command_json("swaymsg", &["-t", "get_tree"])?;
    let mut windows = Vec::new();
    collect_sway_windows(&value, &mut windows, true);
    Ok(windows)
}

fn collect_sway_windows(value: &Value, windows: &mut Vec<Window>, parent_visible: bool) {
    let visible = parent_visible && value.get("visible").and_then(Value::as_bool).unwrap_or(true);
    // Sway types a floating window `floating_con`, not `con`.
    let is_window = matches!(value.get("type").and_then(Value::as_str), Some("con" | "floating_con"));
    if is_window && visible {
        if let (Some(identity), Some(rect)) = (sway_identity(value), sway_rect(value)) {
            windows.push(Window { identity, rect });
        }
    }
    for field in ["nodes", "floating_nodes"] {
        if let Some(children) = value.get(field).and_then(Value::as_array) {
            for child in children {
                collect_sway_windows(child, windows, visible);
            }
        }
    }
}

fn sway_identity(value: &Value) -> Option<ScreenshotWindow> {
    value.get("pid")?.as_i64()?;
    let app_id = value
        .get("app_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            value
                .get("window_properties")
                .and_then(|properties| properties.get("class"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    let title = value.get("name").and_then(Value::as_str).unwrap_or_default();
    Some(ScreenshotWindow { app_id: app_id.to_string(), title: title.to_string() })
}

fn sway_rect(value: &Value) -> Option<LogicalRect> {
    let rect = value.get("rect")?;
    Some(LogicalRect {
        x: rect.get("x")?.as_f64()?,
        y: rect.get("y")?.as_f64()?,
        w: rect.get("width")?.as_f64()?,
        h: rect.get("height")?.as_f64()?,
    })
}

fn pair(value: &Value) -> Option<(f64, f64)> {
    let values = value.as_array()?;
    Some((values.first()?.as_f64()?, values.get(1)?.as_f64()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> OutputGeometry {
        OutputGeometry {
            logical_x: -1280,
            logical_y: -100,
            logical_w: 1280,
            logical_h: 1000,
            mode_w: 1920,
            mode_h: 1500,
            transform_swaps: false,
        }
    }

    #[test]
    fn slurp_output_keeps_an_opaque_window_label() {
        let (rect, label) = parse_slurp_output("10,20 300x400 w3\n").unwrap();
        assert_eq!(LogicalRect { x: 10.0, y: 20.0, w: 300.0, h: 400.0 }, rect);
        assert_eq!(Some("w3"), label);
    }

    #[test]
    fn slurp_output_accepts_a_free_form_region_without_a_label() {
        let (_, label) = parse_slurp_output("-50,20 300x400\n").unwrap();
        assert_eq!(None, label);
    }

    #[test]
    fn logical_geometry_maps_negative_fractional_origins() {
        let rect = logical_rect_to_physical(
            &[output()],
            LogicalRect { x: -1200.0, y: 0.0, w: 100.0, h: 100.0 },
        )
        .unwrap();
        assert_eq!(PhysRect { x: -1800, y: 0, w: 150, h: 150 }, rect);
    }

    #[test]
    fn invalid_logical_geometry_is_rejected() {
        assert_eq!(None, logical_rect_to_physical(&[output()], LogicalRect { x: 0.0, y: 0.0, w: 0.0, h: 4.0 }));
        assert_eq!(None, logical_rect_to_physical(&[output()], LogicalRect { x: f64::NAN, y: 0.0, w: 4.0, h: 4.0 }));
    }

    #[test]
    fn fixed_regions_reject_non_positive_or_overflowing_sizes() {
        assert!(fixed_region([0, 0, 0, 3]).is_err());
        assert!(fixed_region([i32::MAX, 0, 1, 1]).is_err());
        assert!(fixed_region([0, i32::MAX, 1, 1]).is_err());
        assert_eq!(PhysRect { x: -3, y: 4, w: 5, h: 6 }, fixed_region([-3, 4, 5, 6]).unwrap());
    }
    #[test]
    fn exact_window_matching_rejects_absent_and_ambiguous_targets() {
        let target = ScreenshotWindow { app_id: "kitty".into(), title: "shell".into() };
        let window = Window {
            identity: target.clone(),
            rect: LogicalRect { x: 0.0, y: 0.0, w: 10.0, h: 10.0 },
        };
        assert!(match_window(&[], &target).is_err());
        assert!(match_window(&[window.clone(), window.clone()], &target).is_err());
        assert!(match_window(&[window], &target).is_ok());

    }
    #[test]
    fn sway_tree_collects_visible_leaf_windows() {
        let tree = serde_json::json!({
            "type": "root",
            "nodes": [{
                "type": "con",
                "nodes": [{
                    "type": "con",
                    "pid": 1,
                    "visible": true,
                    "app_id": "kitty",
                    "name": "shell",
                    "rect": {"x": -2, "y": 3, "width": 100, "height": 80},
                    "nodes": [], "floating_nodes": []
                }], "floating_nodes": []
            }]
        });
        let mut windows = Vec::new();
        collect_sway_windows(&tree, &mut windows, true);
        assert_eq!(1, windows.len());
        assert_eq!("kitty", windows[0].identity.app_id);
        assert_eq!("shell", windows[0].identity.title);
    }

    #[test]
    fn sway_windows_without_app_ids_remain_targets_but_containers_do_not() {
        let leaf = serde_json::json!({
            "type": "con", "pid": 42, "visible": true, "app_id": "",
            "name": "Reader", "rect": {"x": 0, "y": 0, "width": 100, "height": 80}
        });
        let tree = serde_json::json!({
            "type": "con", "name": "Container",
            "rect": {"x": 0, "y": 0, "width": 100, "height": 80},
            "nodes": [leaf]
        });
        let mut windows = Vec::new();
        collect_sway_windows(&tree, &mut windows, true);
        let target = ScreenshotWindow { app_id: String::new(), title: "Reader".into() };
        assert_eq!(LogicalRect { x: 0.0, y: 0.0, w: 100.0, h: 80.0 },
            match_window(&windows, &target).unwrap().rect);
        assert!(match_window(&windows, &ScreenshotWindow {
            app_id: String::new(), title: "Container".into(),
        }).is_err());
    }

    #[test]
    fn sway_floating_windows_are_targets() {
        let tree = serde_json::json!({
            "type": "root", "nodes": [],
            "floating_nodes": [{
                "type": "floating_con", "pid": 7, "visible": true,
                "app_id": "mpv", "name": "video",
                "rect": {"x": 40, "y": 50, "width": 640, "height": 360}
            }]
        });
        let mut windows = Vec::new();
        collect_sway_windows(&tree, &mut windows, true);
        assert_eq!(1, windows.len());
        assert_eq!("mpv", windows[0].identity.app_id);
    }

    #[test]
    fn hyprland_windows_must_be_on_the_active_workspace() {
        let active = HashMap::from([(1_i64, (2_i64, -42_i64))]);
        let visible = serde_json::json!({
            "mapped": true, "hidden": false, "monitor": 1,
            "workspace": {"id": 2}
        });
        let special = serde_json::json!({
            "mapped": true, "hidden": false, "monitor": 1,
            "workspace": {"id": -42}
        });
        let hidden_workspace = serde_json::json!({
            "mapped": true, "hidden": false, "monitor": 1,
            "workspace": {"id": 3}
        });
        assert!(hypr_on_visible_workspace(&visible, &active));
        assert!(hypr_on_visible_workspace(&special, &active));
        assert!(!hypr_on_visible_workspace(&hidden_workspace, &active));
        let under_fullscreen = serde_json::json!({
            "mapped": true, "hidden": false, "visible": false, "monitor": 1,
            "workspace": {"id": 2}
        });
        assert!(!hypr_on_visible_workspace(&under_fullscreen, &active));
    }
}
