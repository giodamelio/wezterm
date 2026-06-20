//! Per-registration sizing/placement parameters (spec §6.1, §8.5).
//!
//! These are the render-time inputs that decide how an authored outline
//! maps onto a terminal cell: the authored extent (`aw`/`lh`), the
//! cell-width override (`width`), the scale policy (`size`), the placement
//! (`align`), and the span insets (`pad`). The transform itself lives in
//! [`crate::place`].

/// Scale policy (spec §8.5.3). Given the authored extent `aw × lh` and the
/// effective render span `W' × H'`, each mode picks the x/y scale factors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMode {
    /// `H'/lh` on both axes; line-height drives, aspect preserved (default).
    Height,
    /// `W'/aw` on both axes; advance drives, aspect preserved.
    Advance,
    /// `min(W'/aw, H'/lh)`; fits entirely inside the span, aspect preserved.
    Contain,
    /// `max(W'/aw, H'/lh)`; fills the span, may overflow, aspect preserved.
    Cover,
    /// `W'/aw` × `H'/lh` independently; aspect not preserved.
    Stretch,
}

impl Default for SizeMode {
    fn default() -> Self {
        SizeMode::Height
    }
}

impl SizeMode {
    /// Parse a `size=` value; unknown values fall back to the default
    /// (`height`) per spec §11.
    pub fn from_wire(raw: &[u8]) -> Self {
        match raw {
            b"height" => SizeMode::Height,
            b"advance" => SizeMode::Advance,
            b"contain" => SizeMode::Contain,
            b"cover" => SizeMode::Cover,
            b"stretch" => SizeMode::Stretch,
            _ => SizeMode::default(),
        }
    }
}

/// Horizontal placement of the scaled outline within the render span
/// (spec §8.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    /// Outline `x=0` aligns with the span's left edge.
    Start,
    /// Outline horizontal midpoint aligns with the span's midpoint.
    Center,
    /// Outline `x=aw` aligns with the span's right edge.
    End,
}

impl Default for HAlign {
    fn default() -> Self {
        HAlign::Center
    }
}

impl HAlign {
    fn from_wire(raw: &[u8]) -> Self {
        match raw {
            b"start" => HAlign::Start,
            b"center" => HAlign::Center,
            b"end" => HAlign::End,
            _ => HAlign::default(),
        }
    }
}

/// Vertical placement of the scaled outline within the render span
/// (spec §8.5.4). Y-up: `start` is the bottom edge, `end` the top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    /// Outline `y=y_min` aligns with the span's bottom edge.
    Start,
    /// Outline vertical midpoint aligns with the span's midpoint.
    Center,
    /// Outline `y=y_max` aligns with the span's top edge.
    End,
    /// Outline `y=0` sits on the terminal's text baseline.
    Baseline,
}

impl Default for VAlign {
    fn default() -> Self {
        VAlign::Center
    }
}

impl VAlign {
    fn from_wire(raw: &[u8]) -> Self {
        match raw {
            b"start" => VAlign::Start,
            b"center" => VAlign::Center,
            b"end" => VAlign::End,
            b"baseline" => VAlign::Baseline,
            _ => VAlign::default(),
        }
    }
}

/// Insets from the render-span edges, as fractions in `[0,1]` (spec
/// §8.5.2). `top`/`bottom` are fractions of cell height; `left`/`right`
/// of the (width-scaled) span width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Padding {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Default for Padding {
    fn default() -> Self {
        Padding {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        }
    }
}

/// The full sizing/placement parameter set for one registration. `aw` and
/// `lh` are stored in absolute `upm` units (already resolved from their
/// defaults of `upm` at parse time).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizingParams {
    /// Authored advance width, in `upm` units (default `upm`).
    pub aw: u16,
    /// Authored line height, in `upm` units (default `upm`).
    pub lh: u16,
    /// Unicode cell width override: `1` (narrow) or `2` (wide).
    pub width: u8,
    pub size: SizeMode,
    pub align_h: HAlign,
    pub align_v: VAlign,
    pub pad: Padding,
}

impl SizingParams {
    /// Spec defaults for a glyph authored in `upm`-unit space: `aw=lh=upm`,
    /// `width=1`, `size=height`, `align=center,center`, `pad=0,0,0,0`.
    pub fn defaults_for_upm(upm: u16) -> Self {
        SizingParams {
            aw: upm,
            lh: upm,
            width: 1,
            size: SizeMode::default(),
            align_h: HAlign::default(),
            align_v: VAlign::default(),
            pad: Padding::default(),
        }
    }
}

/// Parse a `width=` value to `1` or `2`; anything else is `1` (spec §6.1).
pub fn parse_width(raw: &[u8]) -> u8 {
    match raw {
        b"2" => 2,
        _ => 1,
    }
}

/// Parse a `pad=<t>,<r>,<b>,<l>` value. Each component is a fraction in
/// `[0,1]`; missing or unparseable components default to `0`, and values
/// are clamped into range. The degenerate-span guard (`l+r≥1` / `t+b≥1`
/// → no padding) is applied at render time per §8.5.2, not here.
pub fn parse_pad(raw: &[u8]) -> Padding {
    let mut vals = [0.0f32; 4];
    for (i, part) in raw.split(|&b| b == b',').take(4).enumerate() {
        if let Some(v) = parse_fraction(part) {
            vals[i] = v;
        }
    }
    Padding {
        top: vals[0],
        right: vals[1],
        bottom: vals[2],
        left: vals[3],
    }
}

/// Parse `align=<h>,<v>`; either component may be absent (keeps its
/// default).
pub fn parse_align(raw: &[u8]) -> (HAlign, VAlign) {
    let mut it = raw.split(|&b| b == b',');
    let h = it.next().map(HAlign::from_wire).unwrap_or_default();
    let v = it.next().map(VAlign::from_wire).unwrap_or_default();
    (h, v)
}

/// Parse an ASCII decimal fraction in `[0,1]` (e.g. `0.25`, `1`, `.5`).
/// Returns `None` for unparseable input; clamps to `[0,1]`.
fn parse_fraction(raw: &[u8]) -> Option<f32> {
    let s = core::str::from_utf8(raw).ok()?.trim();
    if s.is_empty() {
        return None;
    }
    let v: f32 = s.parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_mode_defaults_on_unknown() {
        assert_eq!(SizeMode::from_wire(b"advance"), SizeMode::Advance);
        assert_eq!(SizeMode::from_wire(b"stretch"), SizeMode::Stretch);
        assert_eq!(SizeMode::from_wire(b"bogus"), SizeMode::Height);
        assert_eq!(SizeMode::from_wire(b""), SizeMode::Height);
    }

    #[test]
    fn align_pair_and_defaults() {
        assert_eq!(
            parse_align(b"start,baseline"),
            (HAlign::Start, VAlign::Baseline)
        );
        assert_eq!(parse_align(b"end"), (HAlign::End, VAlign::Center));
        // unknown h falls back to center, present v honored
        assert_eq!(parse_align(b"bogus,end"), (HAlign::Center, VAlign::End));
        assert_eq!(parse_align(b""), (HAlign::Center, VAlign::Center));
    }

    #[test]
    fn width_only_one_or_two() {
        assert_eq!(parse_width(b"1"), 1);
        assert_eq!(parse_width(b"2"), 2);
        assert_eq!(parse_width(b"3"), 1);
        assert_eq!(parse_width(b"x"), 1);
    }

    #[test]
    fn pad_parses_clamps_and_defaults() {
        let p = parse_pad(b"0.1,0.2,0.3,0.4");
        assert_eq!((p.top, p.right, p.bottom, p.left), (0.1, 0.2, 0.3, 0.4));
        // missing trailing components default to 0
        let p = parse_pad(b"0.5");
        assert_eq!((p.top, p.right, p.bottom, p.left), (0.5, 0.0, 0.0, 0.0));
        // out-of-range clamps; garbage component → 0
        let p = parse_pad(b"2.0,x,-1,0.5");
        assert_eq!((p.top, p.right, p.bottom, p.left), (1.0, 0.0, 0.0, 0.5));
    }

    #[test]
    fn defaults_for_upm_uses_upm_for_aw_lh() {
        let s = SizingParams::defaults_for_upm(2048);
        assert_eq!(s.aw, 2048);
        assert_eq!(s.lh, 2048);
        assert_eq!(s.width, 1);
        assert_eq!(s.size, SizeMode::Height);
        assert_eq!((s.align_h, s.align_v), (HAlign::Center, VAlign::Center));
        assert_eq!(s.pad, Padding::default());
    }
}
