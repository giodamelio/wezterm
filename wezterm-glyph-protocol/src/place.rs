//! The shared size/align/pad placement transform (spec §8.5).
//!
//! Every registered outline passes through three transforms at render
//! time, in order: **pad** (compute the effective render span), **size**
//! (pick scale factors), **align** (position the scaled outline within the
//! span). This module computes the resulting affine map from authored
//! `upm`-unit space (Y-up, `y=0` at the baseline) into device pixels
//! (Y-down, origin at the render span's top-left). Both the mono `glyf`
//! rasterizer and the `colrv0` rasterizer build their tiny-skia paths
//! through the same [`Placement`].
//!
//! Authored extents: horizontal is the parameter rectangle `[0, aw]`
//! (fully spec-defined); vertical positioning uses the outline's own
//! bounding box `[y_min, y_max]` with the baseline at `y=0`, while the
//! `lh` parameter drives the vertical *scale*. This is the natural reading
//! of §8.5.1, which ties `lh` to the authored line height (descender to
//! ascender) without giving the terminal a separate ascender/descender
//! split: the glyph's ink is what we position, `lh` is what we scale by.

use crate::sizing::{HAlign, Padding, SizeMode, SizingParams, VAlign};

/// Pixel geometry of the cell(s) a glyph renders into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellBox {
    /// Render-span width in pixels (`width × cell_width_px`).
    pub span_w: f32,
    /// Cell height in pixels.
    pub cell_h: f32,
    /// Text baseline, in pixels from the top of the cell (for `align=baseline`).
    pub baseline: f32,
}

/// An affine placement mapping authored `upm`-space (Y-up) to device
/// pixels (Y-down). Built by [`place`]; consumed via [`Placement::fx`] /
/// [`Placement::fy`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    sx: f32,
    sy: f32,
    /// `px = px0 + (x − ax0) · sx`
    ax0: f32,
    px0: f32,
    /// `py = py0 − (y − ay0) · sy` (Y-flip)
    ay0: f32,
    py0: f32,
}

impl Placement {
    /// Map an authored x-coordinate to a device pixel x.
    #[inline]
    pub fn fx(&self, x: f32) -> f32 {
        self.px0 + (x - self.ax0) * self.sx
    }

    /// Map an authored y-coordinate (Y-up) to a device pixel y (Y-down).
    #[inline]
    pub fn fy(&self, y: f32) -> f32 {
        self.py0 - (y - self.ay0) * self.sy
    }

    /// The x/y scale factors (pixels per authored unit).
    pub fn scale(&self) -> (f32, f32) {
        (self.sx, self.sy)
    }
}

/// Compute the placement for an outline whose authored vertical bounds are
/// `[oy_min, oy_max]` (upm units, Y-up), under `sizing`, into `cell`.
pub fn place(oy_min: f32, oy_max: f32, sizing: &SizingParams, cell: CellBox) -> Placement {
    let aw = (sizing.aw.max(1)) as f32;
    let lh = (sizing.lh.max(1)) as f32;

    // --- pad: shrink the span to the effective render box (§8.5.2) ---
    // A degenerate span (l+r ≥ 1 or t+b ≥ 1) is treated as no padding.
    let mut p = sizing.pad;
    if p.left + p.right >= 1.0 || p.top + p.bottom >= 1.0 {
        p = Padding::default();
    }
    let lx = cell.span_w * p.left;
    let rx = cell.span_w * (1.0 - p.right);
    let ty = cell.cell_h * p.top;
    let by = cell.cell_h * (1.0 - p.bottom);
    let wp = (rx - lx).max(0.0);
    let hp = (by - ty).max(0.0);

    // --- size: scale factors per mode (§8.5.3) ---
    let (sx, sy) = match sizing.size {
        SizeMode::Height => {
            let s = hp / lh;
            (s, s)
        }
        SizeMode::Advance => {
            let s = wp / aw;
            (s, s)
        }
        SizeMode::Contain => {
            let s = (wp / aw).min(hp / lh);
            (s, s)
        }
        SizeMode::Cover => {
            let s = (wp / aw).max(hp / lh);
            (s, s)
        }
        SizeMode::Stretch => (wp / aw, hp / lh),
    };

    // --- align: anchor the scaled extent within the span (§8.5.4) ---
    // Horizontal authored extent is the parameter rectangle [0, aw].
    let (ax0, px0) = match sizing.align_h {
        HAlign::Start => (0.0, lx),
        HAlign::Center => (aw / 2.0, (lx + rx) / 2.0),
        HAlign::End => (aw, rx),
    };
    // Vertical positioning uses the outline's bbox; baseline is y=0.
    let (ay0, py0) = match sizing.align_v {
        VAlign::Start => (oy_min, by),
        VAlign::Center => ((oy_min + oy_max) / 2.0, (ty + by) / 2.0),
        VAlign::End => (oy_max, ty),
        VAlign::Baseline => (0.0, cell.baseline),
    };

    Placement {
        sx,
        sy,
        ax0,
        px0,
        ay0,
        py0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell() -> CellBox {
        CellBox {
            span_w: 20.0,
            cell_h: 40.0,
            baseline: 32.0,
        }
    }

    // A 1000-upm glyph whose ink spans y in [0, 700], x in [0, 1000].
    fn sizing(upm: u16) -> SizingParams {
        SizingParams::defaults_for_upm(upm)
    }

    #[test]
    fn height_mode_scales_by_cell_height_over_lh() {
        // Default size=height, lh=1000, cell_h=40 → s = 40/1000 = 0.04.
        let p = place(0.0, 700.0, &sizing(1000), cell());
        assert_eq!(p.scale(), (0.04, 0.04));
    }

    #[test]
    fn advance_mode_scales_by_span_width_over_aw() {
        let mut s = sizing(1000);
        s.size = SizeMode::Advance; // s = span_w/aw = 20/1000 = 0.02
        let p = place(0.0, 700.0, &s, cell());
        assert_eq!(p.scale(), (0.02, 0.02));
    }

    #[test]
    fn contain_fits_inside_on_both_axes() {
        let mut s = sizing(1000);
        s.size = SizeMode::Contain; // min(20/1000, 40/1000) = 0.02
        let p = place(0.0, 1000.0, &s, cell());
        assert_eq!(p.scale(), (0.02, 0.02));
    }

    #[test]
    fn cover_fills_both_axes() {
        let mut s = sizing(1000);
        s.size = SizeMode::Cover; // max(20/1000, 40/1000) = 0.04
        let p = place(0.0, 1000.0, &s, cell());
        assert_eq!(p.scale(), (0.04, 0.04));
    }

    #[test]
    fn stretch_uses_independent_axes() {
        let mut s = sizing(1000);
        s.size = SizeMode::Stretch; // (20/1000, 40/1000)
        let p = place(0.0, 1000.0, &s, cell());
        assert_eq!(p.scale(), (0.02, 0.04));
    }

    #[test]
    fn baseline_align_puts_y0_on_the_baseline() {
        let mut s = sizing(1000);
        s.align_v = VAlign::Baseline;
        let p = place(0.0, 700.0, &s, cell());
        // authored y=0 must map exactly to the baseline pixel row.
        assert!((p.fy(0.0) - 32.0).abs() < 1e-4);
        // a point above the baseline maps higher up (smaller py).
        assert!(p.fy(700.0) < p.fy(0.0));
    }

    #[test]
    fn start_align_anchors_edges() {
        let mut s = sizing(1000);
        s.align_h = HAlign::Start;
        s.align_v = VAlign::Start;
        let p = place(0.0, 700.0, &s, cell());
        // x=0 → left edge (0), y=y_min(0) → bottom edge (cell_h=40).
        assert!((p.fx(0.0) - 0.0).abs() < 1e-4);
        assert!((p.fy(0.0) - 40.0).abs() < 1e-4);
    }

    #[test]
    fn end_align_anchors_far_edges() {
        let mut s = sizing(1000);
        s.align_h = HAlign::End;
        s.align_v = VAlign::End;
        let p = place(0.0, 700.0, &s, cell());
        // x=aw(1000) → right edge (span_w=20), y=y_max(700) → top edge (0).
        assert!((p.fx(1000.0) - 20.0).abs() < 1e-4);
        assert!((p.fy(700.0) - 0.0).abs() < 1e-4);
    }

    #[test]
    fn center_align_centers_extent() {
        // Default align=center,center. Horizontal midpoint aw/2 → span mid.
        let p = place(0.0, 700.0, &sizing(1000), cell());
        assert!((p.fx(500.0) - 10.0).abs() < 1e-4); // span_w/2
        assert!((p.fy(350.0) - 20.0).abs() < 1e-4); // cell_h/2, bbox mid=350
    }

    #[test]
    fn degenerate_pad_is_ignored() {
        let mut s = sizing(1000);
        s.pad.left = 0.6;
        s.pad.right = 0.6; // l+r ≥ 1 → treated as no padding
        let p = place(0.0, 700.0, &s, cell());
        // Falls back to the unpadded height scale 40/1000.
        assert_eq!(p.scale(), (0.04, 0.04));
    }

    #[test]
    fn pad_shrinks_the_span() {
        let mut s = sizing(1000);
        s.pad.top = 0.25;
        s.pad.bottom = 0.25; // H' = 40 * 0.5 = 20 → s = 20/1000 = 0.02
        let p = place(0.0, 700.0, &s, cell());
        assert_eq!(p.scale(), (0.02, 0.02));
    }
}
