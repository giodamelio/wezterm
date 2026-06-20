//! Rasterize a decoded [`Outline`] into an 8-bit alpha coverage bitmap
//! sized to a terminal cell. Monochrome `glyf` glyphs are rendered as a
//! coverage mask; the GUI tints the mask with the cell foreground color
//! (spec §8.4), exactly like the built-in block glyphs.

use crate::glyf::{Outline, Point};
use crate::place::{self, CellBox, Placement};
use crate::sizing::SizingParams;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

/// An 8-bit alpha coverage bitmap, row-major, `width * height` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaBitmap {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// Rasterize `outline` into an alpha mask sized to the render span.
///
/// Applies the full spec §8.5 placement model (pad → size → align) via
/// [`crate::place`]: `sizing` carries `aw`/`lh`/`size`/`align`/`pad`;
/// `baseline` is the text baseline in pixels from the cell top (for
/// `align=baseline`). The render span is `width × cell_w` wide, so a
/// `width=2` glyph produces a two-cell-wide mask that the GUI draws across
/// both cells; the height is one cell.
pub fn rasterize(
    outline: &Outline,
    sizing: &SizingParams,
    cell_w: usize,
    cell_h: usize,
    baseline: f32,
) -> Option<AlphaBitmap> {
    let span_w = cell_w * sizing.width.max(1) as usize;
    if outline.contours.is_empty() || span_w == 0 || cell_h == 0 {
        return None;
    }

    let cell = CellBox {
        span_w: span_w as f32,
        cell_h: cell_h as f32,
        baseline,
    };
    let placement = place::place(outline.y_min, outline.y_max, sizing, cell);

    let path = outline_to_path(outline, |x| placement.fx(x), |y| placement.fy(y))?;

    let mut pixmap = Pixmap::new(span_w as u32, cell_h as u32)?;
    let mut paint = Paint::default();
    paint.set_color(tiny_skia::Color::WHITE);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // We painted opaque white over a transparent pixmap, so each pixel's
    // alpha is its coverage. Extract the alpha channel as the mask.
    let data: Vec<u8> = pixmap.data().chunks_exact(4).map(|px| px[3]).collect();

    Some(AlphaBitmap {
        data,
        width: span_w,
        height: cell_h,
    })
}

/// Build a tiny-skia path directly from a computed [`Placement`]. Shared
/// convenience for the COLR rasterizer, which places several layers
/// through one placement.
pub(crate) fn outline_to_path_placed(outline: &Outline, p: &Placement) -> Option<tiny_skia::Path> {
    outline_to_path(outline, |x| p.fx(x), |y| p.fy(y))
}

/// Build a tiny-skia path from an outline, mapping authoring coordinates
/// to pixel space via `fx`/`fy`. Shared by the mono and COLR rasterizers.
pub(crate) fn outline_to_path(
    outline: &Outline,
    fx: impl Fn(f32) -> f32,
    fy: impl Fn(f32) -> f32,
) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for contour in &outline.contours {
        walk_contour(contour, &fx, &fy, &mut pb);
    }
    pb.finish()
}

#[inline]
fn mid(a: Point, b: Point) -> Point {
    Point {
        x: (a.x + b.x) / 2.0,
        y: (a.y + b.y) / 2.0,
        on_curve: true,
    }
}

/// Walk one contour following standard TrueType quadratic-Bézier rules
/// (spec §8.3): on→on is a line, an off-curve point between two on-curve
/// points is a quadratic control point, and two consecutive off-curve
/// points imply an on-curve point at their midpoint. `fx`/`fy` map
/// authoring coordinates to pixel space.
fn walk_contour(
    contour: &[Point],
    fx: &dyn Fn(f32) -> f32,
    fy: &dyn Fn(f32) -> f32,
    pb: &mut PathBuilder,
) {
    if contour.is_empty() {
        return;
    }

    // Rotate so the contour begins at an on-curve point. If there are no
    // on-curve points at all, synthesize a start at the midpoint of the
    // first and last off-curve points.
    let mut pts: Vec<Point> = contour.to_vec();
    match pts.iter().position(|p| p.on_curve) {
        Some(i) => pts.rotate_left(i),
        None => {
            let start = mid(pts[0], pts[pts.len() - 1]);
            pts.insert(0, start);
        }
    }

    let start = pts[0];
    pb.move_to(fx(start.x), fy(start.y));

    let mut ctrl: Option<Point> = None;
    // Iterate the remaining points and then wrap back to `start` to close.
    for i in 1..=pts.len() {
        let p = if i < pts.len() { pts[i] } else { start };
        if p.on_curve {
            match ctrl.take() {
                Some(c) => pb.quad_to(fx(c.x), fy(c.y), fx(p.x), fy(p.y)),
                None => pb.line_to(fx(p.x), fy(p.y)),
            }
        } else {
            match ctrl.take() {
                // Two off-curve points in a row: emit a quad to the
                // implied midpoint, then keep this point as the next ctrl.
                Some(c) => {
                    let m = mid(c, p);
                    pb.quad_to(fx(c.x), fy(c.y), fx(m.x), fy(m.y));
                    ctrl = Some(p);
                }
                None => ctrl = Some(p),
            }
        }
    }
    pb.close();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glyf::Outline;

    fn pt(x: f32, y: f32) -> Point {
        Point {
            x,
            y,
            on_curve: true,
        }
    }

    fn triangle() -> Outline {
        Outline {
            contours: vec![vec![pt(0.0, 0.0), pt(1000.0, 0.0), pt(0.0, 1000.0)]],
            x_min: 0.0,
            y_min: 0.0,
            x_max: 1000.0,
            y_max: 1000.0,
        }
    }

    fn sizing() -> SizingParams {
        SizingParams::defaults_for_upm(1000)
    }

    #[test]
    fn rasterizes_nonempty_coverage() {
        let bmp = rasterize(&triangle(), &sizing(), 24, 48, 40.0).expect("rasterizes");
        assert_eq!(bmp.width, 24);
        assert_eq!(bmp.height, 48);
        assert_eq!(bmp.data.len(), 24 * 48);
        // A triangle covering an eighth-em-plus region must paint *some*
        // pixels but not the whole cell.
        let painted = bmp.data.iter().filter(|&&a| a > 0).count();
        assert!(painted > 0, "expected some covered pixels");
        assert!(
            painted < bmp.data.len(),
            "triangle should not fill the cell"
        );
    }

    #[test]
    fn empty_outline_is_none() {
        let empty = Outline {
            contours: vec![],
            x_min: 0.0,
            y_min: 0.0,
            x_max: 0.0,
            y_max: 0.0,
        };
        assert!(rasterize(&empty, &sizing(), 24, 48, 40.0).is_none());
    }

    #[test]
    fn zero_cell_is_none() {
        assert!(rasterize(&triangle(), &sizing(), 0, 48, 40.0).is_none());
    }

    #[test]
    fn width_two_doubles_the_span() {
        let mut wide = sizing();
        wide.width = 2;
        let bmp = rasterize(&triangle(), &wide, 24, 48, 40.0).expect("rasterizes");
        // The render span is width × cell_w wide; height is one cell.
        assert_eq!(bmp.width, 48, "width=2 spans two cells");
        assert_eq!(bmp.height, 48);
        assert_eq!(bmp.data.len(), 48 * 48);
    }
}
