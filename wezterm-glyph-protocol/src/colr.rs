//! Color glyph support (`colrv0`, spec §8.6).
//!
//! A `colrv0` payload is a container wrapping the simple-glyph outlines each
//! layer references plus raw OpenType `COLR` v0 and `CPAL` tables. Each layer
//! is a `glyf` outline filled with a `CPAL` color, composited in painter
//! order; we rasterize the result to a straight-alpha RGBA bitmap.

use crate::glyf::{self, Outline};
use crate::place::{self, CellBox};
use crate::sizing::SizingParams;
use tiny_skia::{Color, FillRule, Paint, Pixmap, Transform};

/// A straight-alpha (non-premultiplied) RGBA bitmap, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaBitmap {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// The decoded `colrv0`/`colrv1` container (spec §8.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColrContainer {
    /// Simple-glyph outlines; COLR layer GlyphIds index into this.
    pub glyphs: Vec<Vec<u8>>,
    pub colr: Vec<u8>,
    pub cpal: Vec<u8>,
}

/// Upper bound on outlines in one container (keeps decode cost bounded;
/// sits within COLR's 16-bit GlyphId space).
const MAX_GLYPHS: u16 = 1024;

impl ColrContainer {
    /// Parse the wire container:
    /// ```text
    /// u16 n_glyphs
    /// per glyph: u16 glyf_len, glyf_len bytes
    /// u16 colr_len, colr_len bytes        (> 0)
    /// u16 cpal_len, cpal_len bytes         (may be 0)
    /// ```
    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut c = Cursor::new(data);
        let n = c.u16()?;
        if n == 0 || n > MAX_GLYPHS {
            return None;
        }
        let mut glyphs = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let len = c.u16()? as usize;
            glyphs.push(c.take(len)?.to_vec());
        }
        let colr_len = c.u16()? as usize;
        if colr_len == 0 {
            return None;
        }
        let colr = c.take(colr_len)?.to_vec();
        let cpal_len = c.u16()? as usize;
        let cpal = c.take(cpal_len)?.to_vec();
        if c.remaining() != 0 {
            return None; // trailing garbage: layout mismatch
        }
        Some(Self { glyphs, colr, cpal })
    }

    /// Whether rendering this container depends on the cell foreground
    /// color, i.e. some layer references CPAL palette index `0xFFFF`, or the
    /// container has no usable COLR v0 layer list (in which case the base
    /// outline is painted in the foreground). Such glyphs must re-rasterize
    /// when the foreground changes; all others cache once (spec §8.6 / §7.2).
    pub fn uses_foreground(&self) -> bool {
        match colrv0_layers(&self.colr, 0) {
            Some(layers) if !layers.is_empty() => {
                layers.iter().any(|(_, pal_idx)| *pal_idx == 0xFFFF)
            }
            // No usable v0 layer list → base-outline-in-foreground fallback.
            _ => true,
        }
    }
}

/// Whether a raw container payload (as registered) depends on the cell
/// foreground color. Conservatively returns `true` for payloads that don't
/// parse — those are rejected at validation, so the value is unused, and
/// `true` never wrongly caches a foreground-dependent sprite.
pub fn container_uses_foreground(raw: &[u8]) -> bool {
    ColrContainer::parse(raw)
        .map(|c| c.uses_foreground())
        .unwrap_or(true)
}

/// Rasterize a color container into a `cell_w × cell_h` straight-alpha
/// RGBA bitmap. `fg` (RGBA) resolves palette index `0xFFFF`. `sizing`
/// drives the §8.5 placement model and `baseline` is the text baseline in
/// pixels from the cell top; all layers share one placement so they stay
/// registered with one another.
pub fn rasterize(
    container: &ColrContainer,
    sizing: &SizingParams,
    cell_w: usize,
    cell_h: usize,
    baseline: f32,
    fg: [u8; 4],
) -> Option<RgbaBitmap> {
    let span_w = cell_w * sizing.width.max(1) as usize;
    if span_w == 0 || cell_h == 0 {
        return None;
    }

    // Resolve the layers to paint, in painter order, as (outline, color).
    let layers = colrv0_layers(&container.colr, 0);
    let palette = ttf_parser::cpal::Table::parse(&container.cpal);

    let mut painted: Vec<(Outline, [u8; 4])> = Vec::new();
    match layers {
        Some(layers) if !layers.is_empty() => {
            for (gid, pal_idx) in layers {
                let Some(bytes) = container.glyphs.get(gid as usize) else {
                    continue;
                };
                let Ok(outline) = glyf::decode(bytes) else {
                    continue;
                };
                let color = resolve_color(palette.as_ref(), pal_idx, fg);
                painted.push((outline, color));
            }
        }
        // No usable COLR v0 layer list (e.g. a v1 container ttf-parser
        // can't decode): fall back to the base outline in the foreground.
        _ => {
            let outline = glyf::decode(container.glyphs.first()?).ok()?;
            painted.push((outline, fg));
        }
    }
    if painted.is_empty() {
        return None;
    }

    // Union bounding box across all layers drives one shared placement, so
    // the layers stay registered with one another.
    let (mut y_min, mut y_max) = (f32::MAX, f32::MIN);
    for (o, _) in &painted {
        y_min = y_min.min(o.y_min);
        y_max = y_max.max(o.y_max);
    }
    if y_max <= y_min {
        return None;
    }
    let cell = CellBox {
        span_w: span_w as f32,
        cell_h: cell_h as f32,
        baseline,
    };
    let placement = place::place(y_min, y_max, sizing, cell);

    let mut pixmap = Pixmap::new(span_w as u32, cell_h as u32)?;
    for (outline, color) in &painted {
        let Some(path) = crate::raster::outline_to_path_placed(outline, &placement) else {
            continue;
        };
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(color[0], color[1], color[2], color[3]));
        paint.anti_alias = true;
        // Default blend mode is source-over (painter order), per §8.6.
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    // tiny-skia stores premultiplied RGBA; the GUI atlas wants straight
    // RGBA, so demultiply each pixel.
    let mut out = Vec::with_capacity(span_w * cell_h * 4);
    for px in pixmap.pixels() {
        let c = px.demultiply();
        out.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Some(RgbaBitmap {
        data: out,
        width: span_w,
        height: cell_h,
    })
}

fn resolve_color(palette: Option<&ttf_parser::cpal::Table>, idx: u16, fg: [u8; 4]) -> [u8; 4] {
    // 0xFFFF means "use the current foreground color" (OpenType / spec §8.6).
    if idx == 0xFFFF {
        return fg;
    }
    match palette.and_then(|p| p.get(0, idx)) {
        Some(c) => [c.red, c.green, c.blue, c.alpha],
        None => fg,
    }
}

/// Parse a COLR v0 table and return the layer list `(glyph_id, palette_index)`
/// for `base_glyph` in painter order. Returns `None` for non-v0 tables or
/// when `base_glyph` has no record.
fn colrv0_layers(colr: &[u8], base_glyph: u16) -> Option<Vec<(u16, u16)>> {
    // Header: version u16, numBaseGlyphRecords u16, baseGlyphRecordsOffset
    // u32, layerRecordsOffset u32, numLayerRecords u16.
    if colr.len() < 14 || u16::from_be_bytes([colr[0], colr[1]]) != 0 {
        return None;
    }
    let num_base = u16::from_be_bytes([colr[2], colr[3]]) as usize;
    let base_off = u32::from_be_bytes([colr[4], colr[5], colr[6], colr[7]]) as usize;
    let layer_off = u32::from_be_bytes([colr[8], colr[9], colr[10], colr[11]]) as usize;
    let num_layer = u16::from_be_bytes([colr[12], colr[13]]) as usize;

    for i in 0..num_base {
        let off = base_off + i * 6;
        let rec = colr.get(off..off + 6)?;
        let gid = u16::from_be_bytes([rec[0], rec[1]]);
        if gid != base_glyph {
            continue;
        }
        let first = u16::from_be_bytes([rec[2], rec[3]]) as usize;
        let n = u16::from_be_bytes([rec[4], rec[5]]) as usize;
        let mut layers = Vec::with_capacity(n);
        for j in 0..n {
            let lidx = first + j;
            if lidx >= num_layer {
                return None;
            }
            let lo = layer_off + lidx * 4;
            let l = colr.get(lo..lo + 4)?;
            layers.push((
                u16::from_be_bytes([l[0], l[1]]),
                u16::from_be_bytes([l[2], l[3]]),
            ));
        }
        return Some(layers);
    }
    None
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.data.get(self.pos..self.pos + 2)?;
        self.pos += 2;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.data.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be16(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }

    /// A filled axis-aligned square outline as a bare simple-glyph record,
    /// spanning [x0,x1]×[y0,y1] in 1000-upm space.
    fn square_glyf(x0: i16, y0: i16, x1: i16, y1: i16) -> Vec<u8> {
        const ON: u8 = 0x01;
        let mut v = Vec::new();
        v.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        v.extend_from_slice(&x0.to_be_bytes()); // xMin
        v.extend_from_slice(&y0.to_be_bytes()); // yMin
        v.extend_from_slice(&x1.to_be_bytes()); // xMax
        v.extend_from_slice(&y1.to_be_bytes()); // yMax
        v.extend_from_slice(&3u16.to_be_bytes()); // endPts[0]=3 -> 4 points
        v.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        v.extend_from_slice(&[ON, ON, ON, ON]);
        // 4 corners, absolute coords as i16 deltas from origin.
        for (dx, _) in [(x0, 0), (x1 - x0, 0), (0, 0), (x0 - x1, 0)] {
            v.extend_from_slice(&dx.to_be_bytes());
        }
        for dy in [y0, 0, y1 - y0, 0] {
            v.extend_from_slice(&dy.to_be_bytes());
        }
        v
    }

    /// COLR v0 table: base glyph 0 → `layers` (glyphId, paletteIndex).
    fn colr_v0(layers: &[(u16, u16)]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&be16(0)); // version
        v.extend_from_slice(&be16(1)); // numBaseGlyphRecords
        v.extend_from_slice(&18u32.to_be_bytes()); // baseGlyphRecordsOffset (after 14-byte header + pad? compute)
        let base_off = 14u32;
        let layer_off = base_off + 6; // one base record (6 bytes)
        // Rewrite offsets correctly.
        v.truncate(4);
        v.extend_from_slice(&base_off.to_be_bytes());
        v.extend_from_slice(&layer_off.to_be_bytes());
        v.extend_from_slice(&be16(layers.len() as u16)); // numLayerRecords
        // base glyph record: glyphID=0, firstLayerIndex=0, numLayers
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(0));
        v.extend_from_slice(&be16(layers.len() as u16));
        for (gid, pal) in layers {
            v.extend_from_slice(&be16(*gid));
            v.extend_from_slice(&be16(*pal));
        }
        v
    }

    /// CPAL v0 with one palette of `colors` (each RGBA, stored BGRA).
    fn cpal(colors: &[[u8; 4]]) -> Vec<u8> {
        let n = colors.len() as u16;
        let mut v = Vec::new();
        v.extend_from_slice(&be16(0)); // version
        v.extend_from_slice(&be16(n)); // numPaletteEntries
        v.extend_from_slice(&be16(1)); // numPalettes
        v.extend_from_slice(&be16(n)); // numColorRecords
        let off = 12u32 + 2; // header(12) + colorRecordIndices(1 palette * 2)
        v.extend_from_slice(&off.to_be_bytes()); // offsetFirstColorRecord
        v.extend_from_slice(&be16(0)); // colorRecordIndices[0]
        for c in colors {
            // BGRA on the wire
            v.extend_from_slice(&[c[2], c[1], c[0], c[3]]);
        }
        v
    }

    fn container(glyphs: &[Vec<u8>], colr: Vec<u8>, cpal: Vec<u8>) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&be16(glyphs.len() as u16));
        for g in glyphs {
            v.extend_from_slice(&be16(g.len() as u16));
            v.extend_from_slice(g);
        }
        v.extend_from_slice(&be16(colr.len() as u16));
        v.extend_from_slice(&colr);
        v.extend_from_slice(&be16(cpal.len() as u16));
        v.extend_from_slice(&cpal);
        v
    }

    #[test]
    fn container_round_trips() {
        let g = vec![square_glyf(0, 0, 500, 500)];
        let bytes = container(&g, colr_v0(&[(0, 0)]), cpal(&[[255, 0, 0, 255]]));
        let c = ColrContainer::parse(&bytes).expect("parses");
        assert_eq!(c.glyphs.len(), 1);
        assert!(!c.colr.is_empty());
        assert!(!c.cpal.is_empty());
    }

    #[test]
    fn container_rejects_trailing_garbage() {
        let g = vec![square_glyf(0, 0, 500, 500)];
        let mut bytes = container(&g, colr_v0(&[(0, 0)]), cpal(&[[255, 0, 0, 255]]));
        bytes.push(0xFF);
        assert!(ColrContainer::parse(&bytes).is_none());
    }

    #[test]
    fn container_rejects_empty_colr() {
        let g = vec![square_glyf(0, 0, 500, 500)];
        assert!(ColrContainer::parse(&container(&g, vec![], cpal(&[[1, 2, 3, 4]]))).is_none());
    }

    #[test]
    fn rasterizes_two_color_layers() {
        // glyph 1 = red left half, glyph 2 = blue right half; base glyph 0
        // composes them. (glyph 0 is an unused placeholder outline.)
        let glyphs = vec![
            square_glyf(0, 0, 1000, 1000),   // 0: placeholder
            square_glyf(0, 0, 500, 1000),    // 1: left
            square_glyf(500, 0, 1000, 1000), // 2: right
        ];
        let colr = colr_v0(&[(1, 0), (2, 1)]);
        let pal = cpal(&[[255, 0, 0, 255], [0, 0, 255, 255]]);
        let c = ColrContainer::parse(&container(&glyphs, colr, pal)).unwrap();

        let sizing = SizingParams::defaults_for_upm(1000);
        let bmp = rasterize(&c, &sizing, 32, 32, 26.0, [0, 0, 0, 255]).expect("rasterizes");
        assert_eq!(bmp.data.len(), 32 * 32 * 4);

        // Some pixel should be predominantly red, and some predominantly blue.
        let mut has_red = false;
        let mut has_blue = false;
        for px in bmp.data.chunks_exact(4) {
            if px[3] > 128 {
                if px[0] > 150 && px[2] < 100 {
                    has_red = true;
                }
                if px[2] > 150 && px[0] < 100 {
                    has_blue = true;
                }
            }
        }
        assert!(has_red, "expected red pixels from layer 1");
        assert!(has_blue, "expected blue pixels from layer 2");
    }

    #[test]
    fn layers_stay_registered() {
        // Two abutting halves (red left, blue right) sharing one placement
        // must land registered: red strictly left of blue, a clean seam near
        // the middle, and identical vertical centers of mass (no inter-layer
        // drift). size=stretch fills the cell so the seam is unambiguous.
        let glyphs = vec![
            square_glyf(0, 0, 500, 1000),    // 0: left
            square_glyf(500, 0, 1000, 1000), // 1: right
        ];
        let c = ColrContainer::parse(&container(
            &glyphs,
            colr_v0(&[(0, 0), (1, 1)]),
            cpal(&[[255, 0, 0, 255], [0, 0, 255, 255]]),
        ))
        .unwrap();
        let mut sizing = SizingParams::defaults_for_upm(1000);
        sizing.size = crate::sizing::SizeMode::Stretch;

        let (w, h) = (32usize, 32usize);
        let bmp = rasterize(&c, &sizing, w, h, 26.0, [0, 0, 0, 255]).expect("rasterizes");

        let (mut rx, mut ry, mut rn) = (0f64, 0f64, 0u32);
        let (mut bx, mut by, mut bn) = (0f64, 0f64, 0u32);
        for (i, px) in bmp.data.chunks_exact(4).enumerate() {
            let (x, y) = ((i % w) as f64, (i / w) as f64);
            if px[3] > 128 {
                if px[0] > 150 && px[2] < 100 {
                    rx += x;
                    ry += y;
                    rn += 1;
                }
                if px[2] > 150 && px[0] < 100 {
                    bx += x;
                    by += y;
                    bn += 1;
                }
            }
        }
        assert!(rn > 0 && bn > 0, "both layers must paint");
        let (rx, ry) = (rx / rn as f64, ry / rn as f64);
        let (bx, by) = (bx / bn as f64, by / bn as f64);
        // Horizontal order: red left of blue.
        assert!(rx < bx, "red centroid {rx} must be left of blue {bx}");
        // Vertical registration: identical center of mass (within a pixel).
        assert!(
            (ry - by).abs() < 1.0,
            "layers vertically drifted: red y={ry}, blue y={by}"
        );
        // The halves are mirror images about the cell center column.
        assert!(
            ((rx + bx) / 2.0 - (w as f64 - 1.0) / 2.0).abs() < 1.0,
            "seam not centered: red {rx}, blue {bx}"
        );
    }

    #[test]
    fn detects_foreground_sentinel() {
        let glyphs = vec![square_glyf(0, 0, 1000, 1000)];
        // A layer painted with palette index 0xFFFF means "use foreground".
        let sentinel = ColrContainer::parse(&container(
            &glyphs,
            colr_v0(&[(0, 0xFFFF)]),
            cpal(&[[1, 2, 3, 4]]),
        ))
        .unwrap();
        assert!(sentinel.uses_foreground());

        // A layer with a normal palette index does not depend on fg.
        let normal = ColrContainer::parse(&container(
            &glyphs,
            colr_v0(&[(0, 0)]),
            cpal(&[[255, 0, 0, 255]]),
        ))
        .unwrap();
        assert!(!normal.uses_foreground());
    }

    #[test]
    fn sentinel_layer_renders_in_passed_foreground() {
        // One layer using 0xFFFF must paint in the fg passed to rasterize.
        let glyphs = vec![square_glyf(100, 100, 900, 900)];
        let c = ColrContainer::parse(&container(
            &glyphs,
            colr_v0(&[(0, 0xFFFF)]),
            cpal(&[[1, 2, 3, 4]]),
        ))
        .unwrap();
        let sizing = SizingParams::defaults_for_upm(1000);
        let fg = [10, 200, 30, 255];
        let bmp = rasterize(&c, &sizing, 32, 32, 26.0, fg).expect("rasterizes");
        // Some opaque pixel must match the passed foreground (allowing for
        // anti-aliased edges, check a clearly-covered green channel).
        let has_fg = bmp
            .data
            .chunks_exact(4)
            .any(|px| px[3] > 200 && px[0] < 40 && px[1] > 160 && px[2] < 60);
        assert!(has_fg, "sentinel layer should render in the passed fg");
    }
}
