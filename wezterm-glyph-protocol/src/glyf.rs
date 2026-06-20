//! Decoder for a bare OpenType `glyf` simple-glyph record — the wire
//! payload of a `fmt=glyf` registration (Glyph Protocol spec §8).
//!
//! The protocol ships a *standalone* simple-glyph record (not a whole
//! font), so we parse it with `read-fonts`' low-level table reader.
//! Composite glyphs and hinted glyphs are rejected per spec §8.2.

use read_fonts::tables::glyf::SimpleGlyph;
use read_fonts::{FontData, FontRead};

/// A single contour point in the glyph's authoring coordinate space
/// (Y-up, `upm` units per em).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

/// A decoded simple glyph: closed contours plus the authored bounding box.
#[derive(Debug, Clone, PartialEq)]
pub struct Outline {
    pub contours: Vec<Vec<Point>>,
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

/// Why a `glyf` payload was rejected. Maps onto the spec's register
/// error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// Payload failed to parse as a simple-glyph record.
    Malformed,
    /// `numberOfContours < 0` — a composite glyph (unsupported in v1).
    Composite,
    /// `instructionLength != 0` — hinting bytecode (unsupported in v1).
    Hinted,
}

/// Decode a bare simple-glyph record.
pub fn decode(data: &[u8]) -> Result<Outline, DecodeError> {
    // A glyph record begins with a signed 16-bit numberOfContours; a
    // negative value marks a composite glyph. read-fonts' SimpleGlyph
    // reader will misparse a composite record rather than reject it, so
    // peek at the sign first. The minimum simple-glyph header is 10
    // bytes (numberOfContours + 4×bbox).
    if data.len() < 10 {
        return Err(DecodeError::Malformed);
    }
    if i16::from_be_bytes([data[0], data[1]]) < 0 {
        return Err(DecodeError::Composite);
    }

    let glyph = SimpleGlyph::read(FontData::new(data)).map_err(|_| DecodeError::Malformed)?;

    if glyph.instruction_length() != 0 {
        return Err(DecodeError::Hinted);
    }

    let x_min = glyph.x_min() as f32;
    let y_min = glyph.y_min() as f32;
    let x_max = glyph.x_max() as f32;
    let y_max = glyph.y_max() as f32;

    let end_pts: Vec<usize> = glyph
        .end_pts_of_contours()
        .iter()
        .map(|v| v.get() as usize)
        .collect();

    if end_pts.is_empty() {
        return Ok(Outline {
            contours: Vec::new(),
            x_min,
            y_min,
            x_max,
            y_max,
        });
    }

    // Contour end-points must be strictly increasing.
    for w in end_pts.windows(2) {
        if w[1] <= w[0] {
            return Err(DecodeError::Malformed);
        }
    }
    let num_points = end_pts[end_pts.len() - 1] + 1;

    let pts: Vec<_> = glyph.points().collect();
    if pts.len() != num_points {
        return Err(DecodeError::Malformed);
    }

    let mut contours = Vec::with_capacity(end_pts.len());
    let mut start = 0usize;
    for &end in &end_pts {
        let mut contour = Vec::with_capacity(end - start + 1);
        for p in &pts[start..=end] {
            contour.push(Point {
                x: p.x as f32,
                y: p.y as f32,
                on_curve: p.on_curve,
            });
        }
        contours.push(contour);
        start = end + 1;
    }

    Ok(Outline {
        contours,
        x_min,
        y_min,
        x_max,
        y_max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal simple-glyph record for a single triangle contour
    /// with three on-curve points. Mirrors the `glyf` wire layout
    /// (OpenType spec §8.1): header, endPtsOfContours, instructionLength,
    /// flags, then x- and y-coordinate deltas.
    fn triangle_bytes() -> Vec<u8> {
        const ON_CURVE: u8 = 0x01;
        let mut v = Vec::new();
        v.extend_from_slice(&1i16.to_be_bytes()); // numberOfContours
        v.extend_from_slice(&0i16.to_be_bytes()); // xMin
        v.extend_from_slice(&0i16.to_be_bytes()); // yMin
        v.extend_from_slice(&100i16.to_be_bytes()); // xMax
        v.extend_from_slice(&100i16.to_be_bytes()); // yMax
        v.extend_from_slice(&2u16.to_be_bytes()); // endPtsOfContours[0] = 2 (3 points)
        v.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
        v.push(ON_CURVE); // flags p0
        v.push(ON_CURVE); // flags p1
        v.push(ON_CURVE); // flags p2
        // x coords as signed shorts: flag bit for short-x is not set, so
        // each is a signed 16-bit delta.
        v.extend_from_slice(&0i16.to_be_bytes()); // x0 delta
        v.extend_from_slice(&100i16.to_be_bytes()); // x1 delta -> x=100
        v.extend_from_slice(&(-100i16).to_be_bytes()); // x2 delta -> x=0
        v.extend_from_slice(&0i16.to_be_bytes()); // y0 delta
        v.extend_from_slice(&0i16.to_be_bytes()); // y1 delta -> y=0
        v.extend_from_slice(&100i16.to_be_bytes()); // y2 delta -> y=100
        v
    }

    #[test]
    fn decodes_triangle_contour() {
        let out = decode(&triangle_bytes()).expect("triangle decodes");
        assert_eq!(out.contours.len(), 1);
        let c = &out.contours[0];
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|p| p.on_curve));
        assert_eq!((out.x_max, out.y_max), (100.0, 100.0));
        // points: (0,0) (100,0) (0,100)
        assert_eq!((c[0].x, c[0].y), (0.0, 0.0));
        assert_eq!((c[1].x, c[1].y), (100.0, 0.0));
        assert_eq!((c[2].x, c[2].y), (0.0, 100.0));
    }

    #[test]
    fn rejects_composite() {
        let mut v = vec![0u8; 12];
        v[0] = 0xFF; // numberOfContours = -1
        v[1] = 0xFF;
        assert_eq!(decode(&v), Err(DecodeError::Composite));
    }

    #[test]
    fn rejects_too_short() {
        assert_eq!(decode(&[0, 1, 0, 0]), Err(DecodeError::Malformed));
    }
}
