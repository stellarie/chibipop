//! ADR-0003 rung 2's arithmetic: turning a `spa_meta_cursor` sample
//! into a core cursor position.
//!
//! The portal's `cursor_mode=METADATA` rides the PipeWire stream the
//! capture backend already opened, so the rung costs no extra consent —
//! but the position it carries is in that stream's *own* buffer pixels,
//! measured from the stream's top-left. Core speaks global physical
//! pixels, and the global physical space it means is the one
//! [`crate::cursor::outputs`] defines: each output's logical origin
//! scaled by that output's own scale. Both seams must anchor
//! identically or a hover on the second monitor lands on the first, so
//! everything here funnels through the same
//! [`OutputGeometry::physical_origin`] the screencopy path uses.
//!
//! Pure arithmetic, no PipeWire and no D-Bus: the interesting cases
//! (mixed scales, a portal that omits stream placement, an off-stream
//! sample) are all testable without a compositor.

use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::PhysPoint;

/// One portal stream's placement, as the portal described it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamPlacement {
    /// The portal stream property `position`: the stream's top-left in
    /// the compositor's logical layout. Absent on backends that do not
    /// send it.
    pub logical_origin: Option<(i32, i32)>,
    /// The portal stream property `size`, logical units. Absent when
    /// the backend omits it.
    pub logical_size: Option<(i32, i32)>,
    /// The negotiated video size, in stream buffer pixels.
    pub buffer_size: (i32, i32),
}

/// Which output this stream is showing, matched on the layout
/// rectangle the portal reported. `None` when the portal sent no
/// placement and there is more than one output to guess between.
///
/// The match is exact, not nearest: the portal and `wl_output`/
/// `zxdg_output_v1` are two views of the same compositor layout, so an
/// origin that does not line up means this stream is not one of the
/// outputs we know about (a window or a virtual source), and guessing
/// would anchor a cursor to the wrong monitor.
pub fn match_output(placement: &StreamPlacement, outputs: &[OutputGeometry]) -> Option<usize> {
    let Some((lx, ly)) = placement.logical_origin else {
        // No placement at all. With one output there is nothing to be
        // ambiguous about; with several, refuse to guess.
        return if outputs.len() == 1 { Some(0) } else { None };
    };
    outputs.iter().position(|o| {
        o.logical_x == lx
            && o.logical_y == ly
            // The size only tightens the match when the portal sent it;
            // origins alone are already unique in a valid layout.
            && placement
                .logical_size
                .is_none_or(|(w, h)| o.logical_w == w && o.logical_h == h)
    })
}

/// The stream's top-left in core's global physical space.
///
/// `None` when there is no output to anchor against at all — before the
/// first `wl_output` roundtrip, or for a stream whose placement matches
/// nothing and which the portal did not even position.
pub fn anchor(placement: &StreamPlacement, outputs: &[OutputGeometry]) -> Option<PhysPoint> {
    if let Some(index) = match_output(placement, outputs) {
        let (x, y) = outputs[index].physical_origin();
        return Some(PhysPoint { x, y });
    }
    // Unmatched, but positioned: scale the portal's logical origin by
    // the first output's scale. This is the same documented
    // approximation `cursor::outputs` already makes for mixed-scale
    // layouts, where a single global physical space is not well-defined
    // — least-wrong beats no cursor at all.
    let (lx, ly) = placement.logical_origin?;
    let scale = outputs.first()?.scale();
    Some(PhysPoint {
        x: (f64::from(lx) * scale).round() as i32,
        y: (f64::from(ly) * scale).round() as i32,
    })
}

/// A `spa_meta_cursor` sample (stream buffer pixels) as a global
/// physical position. `None` for a sample outside the stream, which is
/// how a hidden or off-stream cursor reads.
///
/// Silence rather than a clamp is deliberate: the portal sends
/// `spa_meta_cursor.id == 0` and stale or out-of-range positions when
/// the cursor is not on this stream, and clamping such a sample to the
/// nearest edge would drag hover to a corner of the monitor and keep it
/// there. A rejected sample simply means "no news", and the previous
/// position stands.
pub fn cursor_to_global(
    anchor: PhysPoint,
    buffer_size: (i32, i32),
    x: i32,
    y: i32,
) -> Option<PhysPoint> {
    let (w, h) = buffer_size;
    if w <= 0 || h <= 0 || x < 0 || y < 0 || x >= w || y >= h {
        return None;
    }
    Some(PhysPoint { x: anchor.x + x, y: anchor.y + y })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2560x1440 at the origin, scale 1.
    const LEFT: OutputGeometry = OutputGeometry {
        logical_x: 0,
        logical_y: 0,
        logical_w: 2560,
        logical_h: 1440,
        mode_w: 2560,
        mode_h: 1440,
        transform_swaps: false,
    };

    /// 1920x1080 logical to the right of it, scale 1.5.
    const RIGHT: OutputGeometry = OutputGeometry {
        logical_x: 2560,
        logical_y: 0,
        logical_w: 1920,
        logical_h: 1080,
        mode_w: 2880,
        mode_h: 1620,
        transform_swaps: false,
    };

    fn placed(origin: (i32, i32), size: (i32, i32), buffer: (i32, i32)) -> StreamPlacement {
        StreamPlacement {
            logical_origin: Some(origin),
            logical_size: Some(size),
            buffer_size: buffer,
        }
    }

    /// The headline case: a sample on the second monitor's stream must
    /// land where the screencopy seam would have put it.
    #[test]
    fn a_sample_on_the_second_stream_lands_at_the_right_global_point() {
        let outputs = [LEFT, RIGHT];
        let stream = placed((2560, 0), (1920, 1080), (2880, 1620));

        assert_eq!(Some(1), match_output(&stream, &outputs));
        let origin = anchor(&stream, &outputs).expect("a matched stream has an anchor");
        assert_eq!(PhysPoint { x: 3840, y: 0 }, origin);

        assert_eq!(
            Some(PhysPoint { x: 3940, y: 200 }),
            cursor_to_global(origin, stream.buffer_size, 100, 200)
        );
        // The two seams must agree exactly, or hover jumps monitors.
        assert_eq!(
            Some(RIGHT.buffer_to_global(100, 200)),
            cursor_to_global(origin, stream.buffer_size, 100, 200)
        );
    }

    #[test]
    fn the_first_stream_anchors_at_the_layout_origin() {
        let outputs = [LEFT, RIGHT];
        let stream = placed((0, 0), (2560, 1440), (2560, 1440));
        assert_eq!(Some(0), match_output(&stream, &outputs));
        assert_eq!(Some(PhysPoint { x: 0, y: 0 }), anchor(&stream, &outputs));
    }

    /// A size the portal reported must agree too: same origin, wrong
    /// rectangle, no match.
    #[test]
    fn a_reported_size_tightens_the_match() {
        let outputs = [LEFT, RIGHT];
        assert_eq!(None, match_output(&placed((2560, 0), (1280, 720), (1280, 720)), &outputs));
        // Without a size, the origin alone is enough.
        let sizeless = StreamPlacement {
            logical_origin: Some((2560, 0)),
            logical_size: None,
            buffer_size: (2880, 1620),
        };
        assert_eq!(Some(1), match_output(&sizeless, &outputs));
    }

    /// The unambiguous single-monitor case: no placement, one output.
    #[test]
    fn a_placementless_stream_matches_the_only_output() {
        let bare =
            StreamPlacement { logical_origin: None, logical_size: None, buffer_size: (2560, 1440) };
        assert_eq!(Some(0), match_output(&bare, &[LEFT]));
        assert_eq!(Some(PhysPoint { x: 0, y: 0 }), anchor(&bare, &[LEFT]));
    }

    /// With several outputs and nothing to match on, refuse to guess.
    #[test]
    fn a_placementless_stream_will_not_guess_between_outputs() {
        let bare =
            StreamPlacement { logical_origin: None, logical_size: None, buffer_size: (2560, 1440) };
        assert_eq!(None, match_output(&bare, &[LEFT, RIGHT]));
        assert_eq!(None, anchor(&bare, &[LEFT, RIGHT]));
        assert_eq!(None, anchor(&bare, &[]));
    }

    /// An unmatched but positioned stream falls back to the first
    /// output's scale — the documented mixed-scale approximation.
    #[test]
    fn an_unmatched_stream_scales_by_the_first_outputs_scale() {
        let outputs = [RIGHT, LEFT];
        let stream = placed((100, 200), (640, 360), (960, 540));
        assert_eq!(None, match_output(&stream, &outputs));
        assert_eq!(Some(PhysPoint { x: 150, y: 300 }), anchor(&stream, &outputs));
        // Nothing to scale by at all: no anchor rather than a guess.
        assert_eq!(None, anchor(&stream, &[]));
    }

    /// Out-of-range samples are silence, not an edge position: the
    /// portal sends them whenever the cursor is not on this stream.
    #[test]
    fn an_off_stream_sample_is_silence_rather_than_a_clamp() {
        let origin = PhysPoint { x: 3840, y: 0 };
        let size = (2880, 1620);
        assert_eq!(None, cursor_to_global(origin, size, -1, 10));
        assert_eq!(None, cursor_to_global(origin, size, 10, -1));
        assert_eq!(None, cursor_to_global(origin, size, 2880, 10));
        assert_eq!(None, cursor_to_global(origin, size, 10, 1620));
        // The last in-range pixel still counts.
        assert_eq!(
            Some(PhysPoint { x: 3840 + 2879, y: 1619 }),
            cursor_to_global(origin, size, 2879, 1619)
        );
    }

    /// Before the format is negotiated the buffer size is zero, and
    /// every sample against it is meaningless.
    #[test]
    fn a_degenerate_buffer_size_accepts_nothing() {
        let origin = PhysPoint { x: 0, y: 0 };
        assert_eq!(None, cursor_to_global(origin, (0, 1080), 0, 0));
        assert_eq!(None, cursor_to_global(origin, (1920, 0), 0, 0));
        assert_eq!(None, cursor_to_global(origin, (-1, -1), 0, 0));
    }
}
