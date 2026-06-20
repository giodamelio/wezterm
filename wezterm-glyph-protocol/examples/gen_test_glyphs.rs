//! Generate a stream of Glyph Protocol APC registrations plus labelled
//! lines that print the registered codepoints, exercising mono outlines,
//! multi-contour holes, every `size`/`align`/`pad`, `width=2`, multi-layer
//! `colrv0`, and the `0xFFFF` foreground sentinel.
//!
//! Run it into a protocol-enabled terminal (see `run-glyph-demo.sh`):
//!     cargo run -p wezterm-glyph-protocol --example gen_test_glyphs
//!
//! Payloads use the same wire layout this crate decodes, and a test
//! asserts every one parses and rasterizes.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

const ON: u8 = 0x01;

/// Build a bare simple-glyph record from on-curve polygon contours
/// (upm space, Y-up). Flags are all on-curve with i16 coordinate deltas —
/// the standard encoding `read-fonts` decodes.
fn simple_glyph(contours: &[Vec<(i16, i16)>]) -> Vec<u8> {
    let mut pts: Vec<(i16, i16)> = Vec::new();
    let mut end_pts: Vec<u16> = Vec::new();
    for c in contours {
        pts.extend_from_slice(c);
        end_pts.push((pts.len() - 1) as u16);
    }
    let (mut x0, mut y0, mut x1, mut y1) = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    for &(x, y) in &pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let mut v = Vec::new();
    v.extend_from_slice(&(contours.len() as i16).to_be_bytes());
    v.extend_from_slice(&x0.to_be_bytes());
    v.extend_from_slice(&y0.to_be_bytes());
    v.extend_from_slice(&x1.to_be_bytes());
    v.extend_from_slice(&y1.to_be_bytes());
    for e in &end_pts {
        v.extend_from_slice(&e.to_be_bytes());
    }
    v.extend_from_slice(&0u16.to_be_bytes()); // instructionLength
    for _ in &pts {
        v.push(ON);
    }
    let mut prev = 0i16;
    for &(x, _) in &pts {
        v.extend_from_slice(&(x - prev).to_be_bytes());
        prev = x;
    }
    let mut prev = 0i16;
    for &(_, y) in &pts {
        v.extend_from_slice(&(y - prev).to_be_bytes());
        prev = y;
    }
    v
}

/// A COLR v0 table: base glyph 0 → `layers` (glyphId, paletteIndex), in
/// painter order.
fn colr_v0(layers: &[(u16, u16)]) -> Vec<u8> {
    let base_off = 14u32;
    let layer_off = base_off + 6;
    let mut v = Vec::new();
    v.extend_from_slice(&0u16.to_be_bytes()); // version 0
    v.extend_from_slice(&1u16.to_be_bytes()); // numBaseGlyphRecords
    v.extend_from_slice(&base_off.to_be_bytes());
    v.extend_from_slice(&layer_off.to_be_bytes());
    v.extend_from_slice(&(layers.len() as u16).to_be_bytes()); // numLayerRecords
    v.extend_from_slice(&0u16.to_be_bytes()); // base glyph id = 0
    v.extend_from_slice(&0u16.to_be_bytes()); // firstLayerIndex
    v.extend_from_slice(&(layers.len() as u16).to_be_bytes()); // numLayers
    for (gid, pal) in layers {
        v.extend_from_slice(&gid.to_be_bytes());
        v.extend_from_slice(&pal.to_be_bytes());
    }
    v
}

/// A CPAL v0 table with one palette of RGBA colors (stored BGRA on wire).
fn cpal(colors: &[[u8; 4]]) -> Vec<u8> {
    let n = colors.len() as u16;
    let mut v = Vec::new();
    v.extend_from_slice(&0u16.to_be_bytes()); // version
    v.extend_from_slice(&n.to_be_bytes()); // numPaletteEntries
    v.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
    v.extend_from_slice(&n.to_be_bytes()); // numColorRecords
    v.extend_from_slice(&(12u32 + 2).to_be_bytes()); // offsetFirstColorRecord
    v.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
    for c in colors {
        v.extend_from_slice(&[c[2], c[1], c[0], c[3]]); // BGRA
    }
    v
}

/// Wrap outlines + COLR + CPAL into the protocol's colrv0 container.
fn container(glyphs: &[Vec<u8>], colr: Vec<u8>, cpal: Vec<u8>) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());
    for g in glyphs {
        v.extend_from_slice(&(g.len() as u16).to_be_bytes());
        v.extend_from_slice(g);
    }
    v.extend_from_slice(&(colr.len() as u16).to_be_bytes());
    v.extend_from_slice(&colr);
    v.extend_from_slice(&(cpal.len() as u16).to_be_bytes());
    v.extend_from_slice(&cpal);
    v
}

/// One registration in the zoo: a codepoint, its control params, and the
/// payload bytes.
struct Reg {
    cp: u32,
    params: &'static str,
    payload: Vec<u8>,
}

/// Emit a register APC for `r`. `reply=0` so no replies are written back
/// into the demo shell.
fn reg(r: &Reg) {
    let b64 = BASE64.encode(&r.payload);
    print!(
        "\x1b_25a1;r;cp={:X};reply=0;{};{}\x1b\\",
        r.cp, r.params, b64
    );
}

/// Print a labelled demo line: the registered glyph(s) followed by text.
fn line(label: &str) {
    println!("{label}");
}

// ---- shapes (upm = 1000, Y-up, baseline at y=0) --------------------------

fn triangle() -> Vec<u8> {
    simple_glyph(&[vec![(200, 100), (800, 100), (500, 850)]])
}
fn square() -> Vec<u8> {
    simple_glyph(&[vec![(100, 100), (900, 100), (900, 900), (100, 900)]])
}
fn diamond() -> Vec<u8> {
    simple_glyph(&[vec![(500, 80), (920, 500), (500, 920), (80, 500)]])
}
fn chevron() -> Vec<u8> {
    // A right-pointing arrowhead.
    simple_glyph(&[vec![(200, 150), (800, 500), (200, 850), (380, 500)]])
}
fn ring() -> Vec<u8> {
    // Outer square CCW + inner square CW → a hole (non-zero winding).
    simple_glyph(&[
        vec![(100, 100), (900, 100), (900, 900), (100, 900)],
        vec![(350, 350), (350, 650), (650, 650), (650, 350)],
    ])
}

/// Build the full set of registrations the demo emits. Kept as data so the
/// test module can verify every one parses, validates, and rasterizes.
fn zoo() -> Vec<Reg> {
    let tri = triangle();
    let sq = square();
    let di = diamond();

    // Red base with the blue right-half painted ON TOP. Overlaying (rather
    // than abutting two halves) means the blue edge sits over solid red, so
    // the seam has no blended anti-aliased column — a crisp red|blue split.
    let full = simple_glyph(&[vec![(80, 100), (920, 100), (920, 900), (80, 900)]]);
    let rhalf = simple_glyph(&[vec![(500, 100), (920, 100), (920, 900), (500, 900)]]);
    let split = container(
        &[full, rhalf],
        colr_v0(&[(0, 0), (1, 1)]),
        cpal(&[[220, 40, 40, 255], [40, 80, 220, 255]]),
    );
    // A bright diamond clearly concentric inside a red square — shows layer
    // stacking with obvious alignment and high contrast.
    let small_diamond = simple_glyph(&[vec![(500, 300), (700, 500), (500, 700), (300, 500)]]);
    let inside = container(
        &[sq.clone(), small_diamond],
        colr_v0(&[(0, 0), (1, 1)]),
        cpal(&[[210, 45, 45, 255], [250, 225, 70, 255]]),
    );
    // Three horizontal stripes (a tiny flag).
    let bar = |y0: i16, y1: i16| simple_glyph(&[vec![(80, y0), (920, y0), (920, y1), (80, y1)]]);
    let flag = container(
        &[bar(100, 366), bar(367, 633), bar(634, 900)],
        colr_v0(&[(0, 0), (1, 1), (2, 2)]),
        cpal(&[[40, 160, 70, 255], [240, 220, 60, 255], [200, 50, 50, 255]]),
    );
    // A ring in the sentinel color (0xFFFF → fg) with a fixed orange center.
    let dot = simple_glyph(&[vec![(420, 420), (580, 420), (580, 580), (420, 580)]]);
    let sentinel = container(
        &[ring(), dot],
        colr_v0(&[(0, 0xFFFF), (1, 0)]),
        cpal(&[[255, 140, 0, 255]]),
    );

    vec![
        // 1. mono outline variety
        Reg {
            cp: 0xE000,
            params: "upm=1000",
            payload: tri.clone(),
        },
        Reg {
            cp: 0xE001,
            params: "upm=1000;size=stretch",
            payload: sq.clone(),
        },
        Reg {
            cp: 0xE002,
            params: "upm=1000;size=contain",
            payload: di.clone(),
        },
        Reg {
            cp: 0xE003,
            params: "upm=1000;size=contain",
            payload: ring(),
        },
        Reg {
            cp: 0xE004,
            params: "upm=1000;size=contain",
            payload: chevron(),
        },
        // 2. size modes (same diamond)
        Reg {
            cp: 0xE010,
            params: "upm=1000;size=height",
            payload: di.clone(),
        },
        Reg {
            cp: 0xE011,
            params: "upm=1000;size=advance",
            payload: di.clone(),
        },
        Reg {
            cp: 0xE012,
            params: "upm=1000;size=contain",
            payload: di.clone(),
        },
        Reg {
            cp: 0xE013,
            params: "upm=1000;size=cover",
            payload: di.clone(),
        },
        Reg {
            cp: 0xE014,
            params: "upm=1000;size=stretch",
            payload: di.clone(),
        },
        // 3. align
        Reg {
            cp: 0xE020,
            params: "upm=1000;size=contain;align=start,start",
            payload: tri.clone(),
        },
        Reg {
            cp: 0xE021,
            params: "upm=1000;size=contain;align=center,center",
            payload: tri.clone(),
        },
        Reg {
            cp: 0xE022,
            params: "upm=1000;size=contain;align=end,end",
            payload: tri.clone(),
        },
        Reg {
            cp: 0xE023,
            params: "upm=1000;size=contain;align=center,baseline",
            payload: tri.clone(),
        },
        // 4. pad
        Reg {
            cp: 0xE030,
            params: "upm=1000;size=stretch",
            payload: sq.clone(),
        },
        Reg {
            cp: 0xE031,
            params: "upm=1000;size=stretch;pad=0.25,0.25,0.25,0.25",
            payload: sq.clone(),
        },
        Reg {
            cp: 0xE032,
            params: "upm=1000;size=stretch;pad=0.4,0,0.4,0",
            payload: sq.clone(),
        },
        // 5. width=2 (mono and color, each spanning two cells)
        Reg {
            cp: 0xE040,
            params: "upm=1000;width=2;size=contain",
            payload: di.clone(),
        },
        Reg {
            cp: 0xE041,
            params: "fmt=colrv0;upm=1000;width=2;size=stretch",
            payload: split.clone(),
        },
        // 6. colrv0 layered color & registration
        Reg {
            cp: 0xE050,
            params: "fmt=colrv0;upm=1000;size=stretch",
            payload: split,
        },
        Reg {
            cp: 0xE051,
            params: "fmt=colrv0;upm=1000;size=stretch",
            payload: flag,
        },
        Reg {
            cp: 0xE052,
            params: "fmt=colrv0;upm=1000;size=contain",
            payload: inside,
        },
        // 7. foreground sentinel
        Reg {
            cp: 0xE060,
            params: "fmt=colrv0;upm=1000;size=contain",
            payload: sentinel,
        },
    ]
}

fn main() {
    // Register everything first, then describe it. Registration-before-print
    // means each labelled codepoint is live by the time it is emitted.
    for r in zoo() {
        reg(&r);
    }

    line("");
    line("=== Glyph Protocol test zoo =======================================");
    line("");
    line("If a cell shows a notdef box (tofu) where a glyph is described, that");
    line("codepoint is NOT registering/rendering. Selecting any glyph and");
    line("copying must yield the raw PUA codepoint, never pixels.");
    line("");
    line("-- 1. Monochrome outlines (foreground-tinted) --");
    line(
        "   triangle \u{E000}   square(stretch) \u{E001}   diamond \u{E002}   ring(hole) \u{E003}   chevron \u{E004}",
    );
    line("");
    line("-- 2. size modes (same diamond) --");
    line(
        "   height \u{E010}   advance \u{E011}   contain \u{E012}   cover \u{E013}   stretch \u{E014}",
    );
    line("");
    line("-- 3. align h,v (size=contain) --");
    line("   start,start \u{E020}   center \u{E021}   end,end \u{E022}   baseline \u{E023}");
    line("   (E023 should sit on the text baseline, like this x: x\u{E023}x)");
    line("");
    line("-- 4. pad (fractional insets) --");
    line("   no pad \u{E030}   even 0.25 \u{E031}   vertical 0.4 \u{E032}");
    line("");
    line("-- 5. width=2 (glyph spans two cells) --");
    // Same diamond at width 1 vs 2, each wrapped in | markers. The width=2
    // rows' closing | sits one column further right — proof the glyph
    // occupies two cells (and that the cursor advanced by two).
    line("   |\u{E002}| width=1  (one cell)");
    line("   |\u{E040}| width=2  (two cells — closing | shifts one column right)");
    line("   |\u{E041}| width=2 color split  (red cell | blue cell)");
    line("");
    line("-- 6. colrv0 layered color & registration --");
    line(
        "   red|blue split \u{E050}   green/yellow/red stripes \u{E051}   yellow-diamond-in-red \u{E052}",
    );
    line("   (E050's seam and E052's diamond should be centered, not offset)");
    line("");
    line("-- 7. colrv0 foreground sentinel (0xFFFF follows the cell fg) --");
    print!("   same glyph, different cell fg: ");
    print!("\x1b[31m\u{E060}\x1b[0m "); // red fg
    print!("\x1b[32m\u{E060}\x1b[0m "); // green fg
    print!("\x1b[34m\u{E060}\x1b[0m "); // blue fg
    print!("\x1b[37m\u{E060}\x1b[0m");
    line("");
    line("   (the ring should recolor with the fg; the center dot stays orange)");
    line("");
    line("=== end zoo =======================================================");
    line("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_glyph_protocol::parse::{GlyphCommand, parse};

    /// Every registration the demo emits must parse, validate, and
    /// rasterize through this crate — otherwise the zoo would silently show
    /// tofu and mislead manual verification.
    #[test]
    fn every_zoo_glyph_parses_and_rasterizes() {
        for r in zoo() {
            let b64 = BASE64.encode(&r.payload);
            let body = format!("25a1;r;cp={:X};{};{}", r.cp, r.params, b64);
            let cmd = parse(body.as_bytes())
                .unwrap_or_else(|e| panic!("cp {:X} failed to parse: {e:?}", r.cp));
            let GlyphCommand::Register { glyph, .. } = cmd else {
                panic!("cp {:X} did not parse as a register", r.cp);
            };
            glyph
                .validate()
                .unwrap_or_else(|e| panic!("cp {:X} failed validation: {e:?}", r.cp));
            // Rasterize into a typical cell; one of the two paths must yield.
            let mono = glyph.rasterize_alpha(12, 24, 19.0).is_some();
            let color = glyph
                .rasterize_color(12, 24, 19.0, [255, 255, 255, 255])
                .is_some();
            assert!(mono || color, "cp {:X} produced no bitmap", r.cp);
        }
    }
}
