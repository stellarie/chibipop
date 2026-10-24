//! This module converts a `spa_meta_cursor` sample into a core cursor position.
//!
//! The portal's `cursor_mode=METADATA` uses the PipeWire stream that the
//! capture backend already opened, so this rung needs no extra consent. The
//! sample uses the stream buffer's pixels, with the stream top-left as origin.
//! Core uses global physical pixels. [`crate::cursor::outputs`] defines this
//! space after the code scales each output's logical origin with that output's scale.
//! Both seams must use the same anchor. Otherwise, a hover on the second
//! monitor lands on the first. This module uses
//! [`OutputGeometry::physical_origin`], as the screencopy path does.
//!
//! This module contains pure arithmetic. It uses no PipeWire or D-Bus. Tests
//! cover mixed scales, a portal without stream placement, and an off-stream
//! sample without a compositor.

use crate::cursor::outputs::OutputGeometry;
use chibipop::geom::PhysPoint;

/// Placement data for one portal stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamPlacement {
    /// The portal stream property `position`: the stream top-left in the
    /// compositor's logical layout. Some backends do not send this property.
    pub logical_origin: Option<(i32, i32)>,
    /// The portal stream property `size`, in logical units. Some backends omit
    /// this property.
    pub logical_size: Option<(i32, i32)>,
    /// The negotiated video size, in stream buffer pixels.
    pub buffer_size: (i32, i32),
}

/// Returns the output that matches the portal's layout rectangle. Returns
/// `None` when the portal gives no placement and several outputs exist.
///
/// The match is exact, not nearest. The portal and `wl_output`/
/// `zxdg_output_v1` are two views of one compositor layout. An unmatched
/// origin can describe a window or virtual source. A guess could place the
/// cursor on the wrong monitor.
pub fn match_output(placement: &StreamPlacement, outputs: &[OutputGeometry]) -> Option<usize> {
    let Some((lx, ly)) = placement.logical_origin else {
        // No placement exists. One output gives an unambiguous result. With several
        // outputs, the function does not guess.
        return if outputs.len() == 1 { Some(0) } else { None };
    };
    outputs.iter().position(|o| {
        o.logical_x == lx
            && o.logical_y == ly
            // A supplied size narrows the match. The origin alone is unique in a valid
            // layout.
            && placement
                .logical_size
                .is_none_or(|(w, h)| o.logical_w == w && o.logical_h == h)
    })
}

/// Returns the stream top-left in core's global physical space.
///
/// Returns `None` when no output can anchor the stream. This can occur before
/// the first `wl_output` roundtrip or when the portal provides no stream position.
pub fn anchor(placement: &StreamPlacement, outputs: &[OutputGeometry]) -> Option<PhysPoint> {
    if let Some(index) = match_output(placement, outputs) {
        let (x, y) = outputs[index].physical_origin();
        return Some(PhysPoint { x, y });
    }
    // The stream has a position but no exact output match. Scale the portal's
    // logical origin with the first output's scale. `cursor::outputs` uses this
    // approximation for mixed-scale layouts, where one global physical space is
    // not well-defined. This result gives a cursor position instead of none.
    let (lx, ly) = placement.logical_origin?;
    let scale = outputs.first()?.scale();
    Some(PhysPoint {
        x: (f64::from(lx) * scale).round() as i32,
        y: (f64::from(ly) * scale).round() as i32,
    })
}

/// Converts a `spa_meta_cursor` sample from stream buffer pixels to a global
/// physical position. Returns `None` for a sample outside the stream.
///
/// The portal sends `spa_meta_cursor.id == 0` and stale or out-of-range
/// positions when the cursor is not on this stream. The code rejects these
/// samples. It does not move them to an edge. An edge correction would move
/// hover to a monitor corner and keep it there. A rejected sample means "no
/// news", so the previous position remains.
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

    /// A sample on the second monitor's stream must reach the same global point
    /// as the screencopy seam.
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
        // Both seams must produce the same point. Otherwise, hover uses the wrong monitor.
        assert_eq!(
            Some(RIGHT.buffer_to_global(100, 200)),
            cursor_to_global(origin, stream.buffer_size, 100, 200)
        );
    }
}
