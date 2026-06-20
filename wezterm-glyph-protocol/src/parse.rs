//! Glyph Protocol APC wire parser (spec §3–§7).
//!
//! The escape-sequence layer strips the APC introducer (`ESC _`) and
//! terminator (`ESC \`); this module parses the body, which always begins
//! with the protocol identifier `25a1` (U+25A1 WHITE SQUARE) followed by a
//! single-byte verb and `;`-separated `key=value` parameters:
//!
//! ```text
//! 25a1 ; <verb> [ ; key=value ]* [ ; <base64-payload> ]
//! ```
//!
//! Written clean-room from the v1.9 specification.

use crate::RegisteredGlyph;
use crate::sizing::{self, SizingParams};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

/// The literal identifier that prefixes every Glyph Protocol APC body.
pub const PREFIX: &[u8] = b"25a1";

/// Upper bound on a single registered payload, post-base64-decode (spec
/// §6.2 `payload_too_large`).
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Payload formats this build advertises in the `s` reply.
///
/// We render `glyf` and flat-color `colrv0`. The `colrv1` paint graph
/// (gradients/transforms/composites) is not implemented, and per spec
/// §3 / §11.5–6 a terminal MUST NOT advertise a format it cannot render,
/// so `colrv1` registrations are rejected with `malformed_payload`.
pub const SUPPORTED_FORMATS: &[&str] = &["glyf", "colrv0"];

/// Whether `cp` is in one of the three Unicode Private Use Areas (spec §4).
#[inline]
pub fn is_pua(cp: u32) -> bool {
    (0xE000..=0xF8FF).contains(&cp)
        || (0xF_0000..=0xF_FFFD).contains(&cp)
        || (0x10_0000..=0x10_FFFD).contains(&cp)
}

/// Reply-level control for the `r` verb (spec §6.1 `reply=0|1|2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplyMode {
    /// `reply=0`: emit nothing.
    None,
    /// `reply=1` (default): emit success and failure replies.
    #[default]
    All,
    /// `reply=2`: emit failure replies only.
    ErrorsOnly,
}

impl ReplyMode {
    fn from_wire(raw: &[u8]) -> Self {
        match raw {
            b"0" => ReplyMode::None,
            b"2" => ReplyMode::ErrorsOnly,
            // `1`, anything unrecognised, or absent → default (emit both),
            // per the spec's unknown-parameter rule (§11).
            _ => ReplyMode::All,
        }
    }
    pub fn emit_success(self) -> bool {
        matches!(self, ReplyMode::All)
    }
    pub fn emit_error(self) -> bool {
        matches!(self, ReplyMode::All | ReplyMode::ErrorsOnly)
    }
}

/// Coverage status for a `q` reply (spec §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryStatus {
    Free,
    System,
    Glossary,
    Both,
}

impl QueryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryStatus::Free => "",
            QueryStatus::System => "system",
            QueryStatus::Glossary => "glossary",
            QueryStatus::Both => "system,glossary",
        }
    }
}

/// Defined register-error codes (spec §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    OutOfNamespace,
    CompositeUnsupported,
    HintingUnsupported,
    MalformedPayload,
    PayloadTooLarge,
}

impl RegisterError {
    pub fn as_str(self) -> &'static str {
        match self {
            RegisterError::OutOfNamespace => "out_of_namespace",
            RegisterError::CompositeUnsupported => "composite_unsupported",
            RegisterError::HintingUnsupported => "hinting_unsupported",
            RegisterError::MalformedPayload => "malformed_payload",
            RegisterError::PayloadTooLarge => "payload_too_large",
        }
    }
}

/// A parsed Glyph Protocol command ready for the dispatcher.
///
/// Not `Eq` because `Register` carries a [`RegisteredGlyph`], whose sizing
/// parameters hold `f32` padding fractions.
#[derive(Debug, Clone, PartialEq)]
pub enum GlyphCommand {
    /// `s` — advertise supported formats / protocol-detection ping.
    Support,
    /// `q` — query coverage of a codepoint.
    Query { cp: u32 },
    /// `r` — register a glyph at a PUA codepoint.
    Register {
        cp: u32,
        glyph: RegisteredGlyph,
        reply: ReplyMode,
    },
    /// `c` — clear one slot (`Some`) or all slots (`None`).
    Clear { cp: Option<u32> },
}

/// Why an APC body was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Body does not begin with `25a1` — not our protocol; the caller
    /// should let other APC dispatchers have it.
    NotGlyphProtocol,
    /// Framing recognised but malformed.
    Malformed(&'static str),
    /// `r` rejected at parse time; the dispatcher formats this as a
    /// `status=<nonzero>;reason=<code>` reply, honouring `reply`.
    RegisterFailed {
        cp: u32,
        reason: RegisterError,
        reply: ReplyMode,
    },
    /// `c;cp=<non-pua>`.
    ClearOutOfNamespace,
}

/// Parse an APC body (identifier through final parameter, no introducer or
/// terminator) into a [`GlyphCommand`].
pub fn parse(body: &[u8]) -> Result<GlyphCommand, ParseError> {
    let rest = body
        .strip_prefix(PREFIX)
        .ok_or(ParseError::NotGlyphProtocol)?;
    let rest = rest
        .strip_prefix(b";")
        .ok_or(ParseError::Malformed("missing verb separator"))?;

    let (verb, rest) = split_first(rest, b';');
    let verb = trim(verb);
    if verb.len() != 1 {
        return Err(ParseError::Malformed("verb must be a single byte"));
    }
    match verb[0] {
        b's' => Ok(GlyphCommand::Support),
        b'q' => parse_query(rest),
        b'r' => parse_register(rest),
        b'c' => parse_clear(rest),
        _ => Err(ParseError::Malformed("unknown verb")),
    }
}

fn parse_query(rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    let params = Params::parse(rest);
    let cp_raw = params
        .get("cp")
        .ok_or(ParseError::Malformed("query missing cp"))?;
    if cp_raw.contains(&b',') {
        return Err(ParseError::Malformed("cp must be a single codepoint"));
    }
    let cp = parse_hex_cp(cp_raw).ok_or(ParseError::Malformed("query cp invalid"))?;
    Ok(GlyphCommand::Query { cp })
}

fn parse_register(rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    // Control params and the base64 payload are split at the LAST `;`;
    // base64 contains no `;` so this is unambiguous.
    let (control, payload_b64) = split_last(rest, b';');
    let params = Params::parse(control);

    let cp_raw = params
        .get("cp")
        .ok_or(ParseError::Malformed("register missing cp"))?;
    if cp_raw.contains(&b',') {
        return Err(ParseError::Malformed("cp must be a single codepoint"));
    }
    let cp = parse_hex_cp(cp_raw).ok_or(ParseError::Malformed("register cp invalid"))?;

    // Extract reply before fallible validation so every error path honours
    // the requested reply level.
    let reply = params
        .get("reply")
        .map(|v| ReplyMode::from_wire(v))
        .unwrap_or_default();

    // PUA enforcement is the protocol's security contract — reject before
    // decoding anything (spec §9).
    if !is_pua(cp) {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::OutOfNamespace,
            reply,
        });
    }

    let fmt = params.get("fmt").copied().unwrap_or(b"glyf");
    // `colrv1` is a recognised format name but not advertised/rendered yet
    // (see SUPPORTED_FORMATS); reject it as malformed payload, honouring
    // the reply level, exactly as for any unsupported format (spec §3).
    if fmt == b"colrv1" {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::MalformedPayload,
            reply,
        });
    }
    if fmt != b"glyf" && fmt != b"colrv0" {
        return Err(ParseError::Malformed("register fmt unknown"));
    }

    let upm = match params.get("upm") {
        Some(raw) => parse_decimal_u16(raw).ok_or(ParseError::Malformed("register upm invalid"))?,
        None => 1000,
    };
    if upm == 0 {
        return Err(ParseError::Malformed("register upm must be non-zero"));
    }

    // Sizing/placement parameters (spec §6.1, §8.5). `aw`/`lh` default to
    // `upm`; recognised parameters with unparseable values fall back to
    // their spec default rather than failing the request (§11).
    let mut sizing = SizingParams::defaults_for_upm(upm);
    if let Some(raw) = params.get("aw") {
        if let Some(aw) = parse_decimal_u16(raw) {
            if aw != 0 {
                sizing.aw = aw;
            }
        }
    }
    if let Some(raw) = params.get("lh") {
        if let Some(lh) = parse_decimal_u16(raw) {
            if lh != 0 {
                sizing.lh = lh;
            }
        }
    }
    if let Some(raw) = params.get("width") {
        sizing.width = sizing::parse_width(raw);
    }
    if let Some(raw) = params.get("size") {
        sizing.size = sizing::SizeMode::from_wire(raw);
    }
    if let Some(raw) = params.get("align") {
        let (h, v) = sizing::parse_align(raw);
        sizing.align_h = h;
        sizing.align_v = v;
    }
    if let Some(raw) = params.get("pad") {
        sizing.pad = sizing::parse_pad(raw);
    }

    let raw = BASE64
        .decode(trim(payload_b64))
        .map_err(|_| ParseError::RegisterFailed {
            cp,
            reason: RegisterError::MalformedPayload,
            reply,
        })?;
    if raw.len() > MAX_PAYLOAD_BYTES {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::PayloadTooLarge,
            reply,
        });
    }
    if raw.is_empty() {
        return Err(ParseError::RegisterFailed {
            cp,
            reason: RegisterError::MalformedPayload,
            reply,
        });
    }

    let glyph = match fmt {
        b"glyf" => RegisteredGlyph::Glyf {
            glyf: raw,
            upm,
            sizing,
        },
        _ => {
            // Detect foreground-sentinel use once, at registration, so the
            // renderer can decide cache-keying without re-parsing per frame.
            let uses_fg = crate::colr::container_uses_foreground(&raw);
            RegisteredGlyph::Color {
                container: raw,
                upm,
                sizing,
                uses_fg,
            }
        }
    };

    Ok(GlyphCommand::Register { cp, glyph, reply })
}

fn parse_clear(rest: &[u8]) -> Result<GlyphCommand, ParseError> {
    let params = Params::parse(rest);
    match params.get("cp") {
        Some(cp_raw) => {
            if cp_raw.contains(&b',') {
                return Err(ParseError::Malformed("cp must be a single codepoint"));
            }
            let cp = parse_hex_cp(cp_raw).ok_or(ParseError::Malformed("clear cp invalid"))?;
            if !is_pua(cp) {
                return Err(ParseError::ClearOutOfNamespace);
            }
            Ok(GlyphCommand::Clear { cp: Some(cp) })
        }
        None => Ok(GlyphCommand::Clear { cp: None }),
    }
}

// ---- reply formatters (spec §3.2 framing) --------------------------------

pub fn format_support_response(fmts: &[&str]) -> String {
    format!("\x1b_25a1;s;fmt={}\x1b\\", fmts.join(","))
}
pub fn format_query_response(cp: u32, status: QueryStatus) -> String {
    format!("\x1b_25a1;q;cp={:x};status={}\x1b\\", cp, status.as_str())
}
pub fn format_register_ok(cp: u32) -> String {
    format!("\x1b_25a1;r;cp={:x};status=0\x1b\\", cp)
}
pub fn format_register_error(cp: u32, reason: RegisterError) -> String {
    format!(
        "\x1b_25a1;r;cp={:x};status=1;reason={}\x1b\\",
        cp,
        reason.as_str()
    )
}
pub fn format_clear_ok(cp: Option<u32>) -> String {
    match cp {
        Some(cp) => format!("\x1b_25a1;c;cp={:x};status=0\x1b\\", cp),
        None => "\x1b_25a1;c;status=0\x1b\\".to_string(),
    }
}
pub fn format_clear_error_out_of_namespace() -> String {
    "\x1b_25a1;c;status=1;reason=out_of_namespace\x1b\\".to_string()
}

// ---- byte helpers --------------------------------------------------------

/// Semicolon-separated `key=value` parameter list. Last value wins for a
/// repeated key; unknown keys are kept and simply ignored by callers
/// (spec §11).
struct Params<'a> {
    entries: Vec<(&'a [u8], &'a [u8])>,
}

impl<'a> Params<'a> {
    fn parse(data: &'a [u8]) -> Self {
        let mut entries: Vec<(&[u8], &[u8])> = Vec::new();
        for part in data.split(|&b| b == b';') {
            let part = trim(part);
            if part.is_empty() {
                continue;
            }
            if let Some(eq) = part.iter().position(|&b| b == b'=') {
                let k = trim(&part[..eq]);
                let v = trim(&part[eq + 1..]);
                if let Some(e) = entries.iter_mut().find(|e| e.0 == k) {
                    e.1 = v;
                } else {
                    entries.push((k, v));
                }
            }
        }
        Self { entries }
    }

    fn get(&self, key: &str) -> Option<&&'a [u8]> {
        self.entries
            .iter()
            .find(|e| e.0 == key.as_bytes())
            .map(|e| &e.1)
    }
}

fn parse_hex_cp(raw: &[u8]) -> Option<u32> {
    let raw = trim(raw);
    if raw.is_empty() || raw.len() > 6 {
        return None;
    }
    let mut out: u32 = 0;
    for &b in raw {
        let d = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return None,
        } as u32;
        out = (out << 4) | d;
    }
    // Reject values above the Unicode ceiling and the surrogate range.
    if out > 0x10_FFFF || (0xD800..=0xDFFF).contains(&out) {
        return None;
    }
    Some(out)
}

fn parse_decimal_u16(raw: &[u8]) -> Option<u16> {
    let raw = trim(raw);
    if raw.is_empty() {
        return None;
    }
    let mut out: u32 = 0;
    for &b in raw {
        if !b.is_ascii_digit() {
            return None;
        }
        out = out.checked_mul(10)?.checked_add((b - b'0') as u32)?;
        if out > u16::MAX as u32 {
            return None;
        }
    }
    Some(out as u16)
}

fn split_first(data: &[u8], sep: u8) -> (&[u8], &[u8]) {
    match data.iter().position(|&b| b == sep) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[]),
    }
}

fn split_last(data: &[u8], sep: u8) -> (&[u8], &[u8]) {
    match data.iter().rposition(|&b| b == sep) {
        Some(i) => (&data[..i], &data[i + 1..]),
        None => (data, &[]),
    }
}

fn trim(data: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = data.len();
    while start < end && matches!(data[start], b' ' | b'\t' | b'\r' | b'\n') {
        start += 1;
    }
    while end > start && matches!(data[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }
    &data[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(data: &[u8]) -> String {
        BASE64.encode(data)
    }

    #[test]
    fn rejects_non_glyph_protocol() {
        assert_eq!(parse(b"G,a=1;x"), Err(ParseError::NotGlyphProtocol));
        assert_eq!(parse(b""), Err(ParseError::NotGlyphProtocol));
    }

    #[test]
    fn pua_ranges() {
        for cp in [
            0xE000, 0xE0A0, 0xF8FF, 0xF_0000, 0xF_FFFD, 0x10_0000, 0x10_FFFD,
        ] {
            assert!(is_pua(cp), "{cp:x} should be PUA");
        }
        for cp in [0x61, 0x2D, 0x1F600, 0xFFFE, 0xF_FFFE, 0x10_FFFF] {
            assert!(!is_pua(cp), "{cp:x} should not be PUA");
        }
    }

    #[test]
    fn parses_support() {
        assert_eq!(parse(b"25a1;s").unwrap(), GlyphCommand::Support);
        // unknown params ignored
        assert_eq!(parse(b"25a1;s;future=1").unwrap(), GlyphCommand::Support);
    }

    #[test]
    fn parses_query() {
        assert_eq!(
            parse(b"25a1;q;cp=E0A0").unwrap(),
            GlyphCommand::Query { cp: 0xE0A0 }
        );
        // query accepts non-PUA codepoints (probing system coverage)
        assert_eq!(
            parse(b"25a1;q;cp=61").unwrap(),
            GlyphCommand::Query { cp: 0x61 }
        );
    }

    #[test]
    fn query_rejects_surrogate_and_sequence() {
        assert!(matches!(
            parse(b"25a1;q;cp=D800"),
            Err(ParseError::Malformed(_))
        ));
        assert!(matches!(
            parse(b"25a1;q;cp=E0A0,E0A1"),
            Err(ParseError::Malformed(_))
        ));
    }

    #[test]
    fn parses_register_glyf_default() {
        let payload = b64(&[1, 2, 3]);
        let body = format!("25a1;r;cp=E000;upm=1000;{payload}");
        assert_eq!(
            parse(body.as_bytes()).unwrap(),
            GlyphCommand::Register {
                cp: 0xE000,
                glyph: RegisteredGlyph::Glyf {
                    glyf: vec![1, 2, 3],
                    upm: 1000,
                    sizing: SizingParams::defaults_for_upm(1000),
                },
                reply: ReplyMode::All,
            }
        );
    }

    #[test]
    fn register_defaults_upm_and_fmt() {
        let body = format!("25a1;r;cp=E000;{}", b64(&[9]));
        match parse(body.as_bytes()).unwrap() {
            GlyphCommand::Register {
                glyph: RegisteredGlyph::Glyf { upm, .. },
                ..
            } => assert_eq!(upm, 1000),
            other => panic!("expected glyf register, got {other:?}"),
        }
    }

    #[test]
    fn register_color_fmt_makes_color_payload() {
        let body = format!("25a1;r;cp=E000;fmt=colrv0;upm=2048;{}", b64(&[7, 7]));
        match parse(body.as_bytes()).unwrap() {
            GlyphCommand::Register {
                glyph: RegisteredGlyph::Color { container, upm, .. },
                ..
            } => {
                assert_eq!(container, vec![7, 7]);
                assert_eq!(upm, 2048);
            }
            other => panic!("expected color register, got {other:?}"),
        }
    }

    #[test]
    fn register_colrv1_is_rejected() {
        // colrv1 is recognised but not advertised/rendered yet; it must be
        // rejected as malformed payload, honouring the reply level.
        let body = format!("25a1;r;cp=E000;fmt=colrv1;upm=1000;{}", b64(&[1]));
        assert_eq!(
            parse(body.as_bytes()),
            Err(ParseError::RegisterFailed {
                cp: 0xE000,
                reason: RegisterError::MalformedPayload,
                reply: ReplyMode::All,
            })
        );
        // colrv1 is not in the advertised format set.
        assert!(!SUPPORTED_FORMATS.contains(&"colrv1"));
        assert_eq!(SUPPORTED_FORMATS, &["glyf", "colrv0"]);
    }

    #[test]
    fn register_parses_sizing_params() {
        use crate::sizing::{HAlign, SizeMode, VAlign};
        let body = format!(
            "25a1;r;cp=E000;upm=1000;aw=600;lh=1200;width=2;size=contain;align=start,baseline;pad=0.1,0.2,0.3,0.4;{}",
            b64(&[1])
        );
        match parse(body.as_bytes()).unwrap() {
            GlyphCommand::Register {
                glyph: RegisteredGlyph::Glyf { sizing, .. },
                ..
            } => {
                assert_eq!(sizing.aw, 600);
                assert_eq!(sizing.lh, 1200);
                assert_eq!(sizing.width, 2);
                assert_eq!(sizing.size, SizeMode::Contain);
                assert_eq!(sizing.align_h, HAlign::Start);
                assert_eq!(sizing.align_v, VAlign::Baseline);
                assert_eq!(
                    (
                        sizing.pad.top,
                        sizing.pad.right,
                        sizing.pad.bottom,
                        sizing.pad.left
                    ),
                    (0.1, 0.2, 0.3, 0.4)
                );
            }
            other => panic!("expected glyf register, got {other:?}"),
        }
    }

    #[test]
    fn register_sizing_defaults_and_bad_values_fall_back() {
        use crate::sizing::{HAlign, SizeMode, VAlign};
        // No sizing params: everything takes its spec default (aw=lh=upm).
        let body = format!("25a1;r;cp=E000;upm=2048;{}", b64(&[1]));
        let GlyphCommand::Register {
            glyph: RegisteredGlyph::Glyf { sizing, .. },
            ..
        } = parse(body.as_bytes()).unwrap()
        else {
            panic!("expected glyf register");
        };
        assert_eq!((sizing.aw, sizing.lh), (2048, 2048));
        assert_eq!(sizing.size, SizeMode::Height);
        assert_eq!(
            (sizing.align_h, sizing.align_v),
            (HAlign::Center, VAlign::Center)
        );

        // Recognised params with bad values fall back to defaults (§11),
        // they do not fail the registration.
        let body = format!(
            "25a1;r;cp=E000;upm=1000;aw=0;width=9;size=bogus;align=bad,worse;pad=oops;{}",
            b64(&[1])
        );
        let GlyphCommand::Register {
            glyph: RegisteredGlyph::Glyf { sizing, .. },
            ..
        } = parse(body.as_bytes()).unwrap()
        else {
            panic!("expected glyf register");
        };
        assert_eq!(sizing.aw, 1000, "aw=0 ignored, defaults to upm");
        assert_eq!(sizing.width, 1, "width=9 falls back to 1");
        assert_eq!(sizing.size, SizeMode::Height);
        assert_eq!(
            (sizing.align_h, sizing.align_v),
            (HAlign::Center, VAlign::Center)
        );
        assert_eq!(sizing.pad, crate::sizing::Padding::default());
    }

    #[test]
    fn register_rejects_non_pua() {
        let body = format!("25a1;r;cp=61;upm=1000;{}", b64(&[1]));
        assert_eq!(
            parse(body.as_bytes()),
            Err(ParseError::RegisterFailed {
                cp: 0x61,
                reason: RegisterError::OutOfNamespace,
                reply: ReplyMode::All,
            })
        );
    }

    #[test]
    fn register_propagates_reply_on_failure() {
        let body = format!("25a1;r;cp=61;reply=0;upm=1000;{}", b64(&[1]));
        assert_eq!(
            parse(body.as_bytes()),
            Err(ParseError::RegisterFailed {
                cp: 0x61,
                reason: RegisterError::OutOfNamespace,
                reply: ReplyMode::None,
            })
        );
    }

    #[test]
    fn register_reply_levels() {
        for (raw, want) in [
            ("0", ReplyMode::None),
            ("1", ReplyMode::All),
            ("2", ReplyMode::ErrorsOnly),
        ] {
            let body = format!("25a1;r;cp=E000;reply={raw};upm=1000;{}", b64(&[1]));
            match parse(body.as_bytes()).unwrap() {
                GlyphCommand::Register { reply, .. } => assert_eq!(reply, want),
                other => panic!("got {other:?}"),
            }
        }
        // unknown reply value falls back to All
        let body = format!("25a1;r;cp=E000;reply=9;upm=1000;{}", b64(&[1]));
        match parse(body.as_bytes()).unwrap() {
            GlyphCommand::Register { reply, .. } => assert_eq!(reply, ReplyMode::All),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn register_bad_base64_is_malformed() {
        let body = b"25a1;r;cp=E000;upm=1000;$$$notb64";
        assert!(matches!(
            parse(body),
            Err(ParseError::RegisterFailed {
                reason: RegisterError::MalformedPayload,
                ..
            })
        ));
    }

    #[test]
    fn register_oversized_is_too_large() {
        let body = format!(
            "25a1;r;cp=E000;upm=1000;{}",
            b64(&vec![0u8; MAX_PAYLOAD_BYTES + 1])
        );
        assert!(matches!(
            parse(body.as_bytes()),
            Err(ParseError::RegisterFailed {
                reason: RegisterError::PayloadTooLarge,
                ..
            })
        ));
    }

    #[test]
    fn register_empty_payload_is_malformed() {
        assert!(matches!(
            parse(b"25a1;r;cp=E000;upm=1000;"),
            Err(ParseError::RegisterFailed {
                reason: RegisterError::MalformedPayload,
                ..
            })
        ));
        // no payload separator at all
        assert!(matches!(
            parse(b"25a1;r;cp=E000"),
            Err(ParseError::RegisterFailed {
                reason: RegisterError::MalformedPayload,
                ..
            })
        ));
    }

    #[test]
    fn register_zero_or_bad_upm_rejected() {
        let body = format!("25a1;r;cp=E000;upm=0;{}", b64(&[1]));
        assert!(matches!(
            parse(body.as_bytes()),
            Err(ParseError::Malformed(_))
        ));
    }

    #[test]
    fn register_unknown_fmt_rejected() {
        let body = format!("25a1;r;cp=E000;fmt=svg;{}", b64(&[1]));
        assert!(matches!(
            parse(body.as_bytes()),
            Err(ParseError::Malformed(_))
        ));
    }

    #[test]
    fn clear_one_all_and_non_pua() {
        assert_eq!(parse(b"25a1;c").unwrap(), GlyphCommand::Clear { cp: None });
        assert_eq!(
            parse(b"25a1;c;cp=E000").unwrap(),
            GlyphCommand::Clear { cp: Some(0xE000) }
        );
        assert_eq!(parse(b"25a1;c;cp=61"), Err(ParseError::ClearOutOfNamespace));
    }

    #[test]
    fn parser_never_panics_on_arbitrary_input() {
        // A hostile or truncated stream must never panic the parser; it may
        // only return Ok or Err. Sweep a range of adversarial byte patterns.
        let mut cases: Vec<Vec<u8>> = vec![
            vec![],
            b"25a1".to_vec(),
            b"25a1;".to_vec(),
            b"25a1;;".to_vec(),
            b"25a1;r".to_vec(),
            b"25a1;r;".to_vec(),
            b"25a1;r;cp=".to_vec(),
            b"25a1;r;cp=;".to_vec(),
            b"25a1;r;cp=ZZZZ;AAAA".to_vec(),
            b"25a1;q".to_vec(),
            b"25a1;q;cp=".to_vec(),
            b"25a1;c;cp".to_vec(),
            b"25a1;\x00\xff;cp=E000".to_vec(),
            b"25a1;r;cp=E000;pad=,,,,,,,;AA==".to_vec(),
            b"25a1;r;cp=E000;align=,;AA==".to_vec(),
            b"25a1;r;cp=E000;size=;aw=;lh=;width=;AA==".to_vec(),
        ];
        // Truncations of a valid register at every length.
        let valid = format!("25a1;r;cp=E000;upm=1000;{}", b64(&[1, 2, 3, 4]));
        for i in 0..=valid.len() {
            cases.push(valid.as_bytes()[..i].to_vec());
        }
        // Repeated separators and long junk.
        cases.push(vec![b';'; 4096]);
        cases.push([b"25a1;r;cp=E000;".as_ref(), &vec![b'x'; 4096]].concat());
        for c in cases {
            let _ = parse(&c); // must not panic
        }
    }

    #[test]
    fn reply_emit_matrix() {
        assert!(ReplyMode::All.emit_success() && ReplyMode::All.emit_error());
        assert!(!ReplyMode::ErrorsOnly.emit_success() && ReplyMode::ErrorsOnly.emit_error());
        assert!(!ReplyMode::None.emit_success() && !ReplyMode::None.emit_error());
    }

    #[test]
    fn reply_formatters() {
        assert_eq!(
            format_support_response(SUPPORTED_FORMATS),
            "\x1b_25a1;s;fmt=glyf,colrv0\x1b\\"
        );
        assert_eq!(
            format_query_response(0xE0A0, QueryStatus::Both),
            "\x1b_25a1;q;cp=e0a0;status=system,glossary\x1b\\"
        );
        assert_eq!(
            format_query_response(0xE0A0, QueryStatus::Free),
            "\x1b_25a1;q;cp=e0a0;status=\x1b\\"
        );
        assert_eq!(
            format_register_ok(0xE000),
            "\x1b_25a1;r;cp=e000;status=0\x1b\\"
        );
        assert_eq!(
            format_register_error(0x61, RegisterError::OutOfNamespace),
            "\x1b_25a1;r;cp=61;status=1;reason=out_of_namespace\x1b\\"
        );
        assert_eq!(format_clear_ok(None), "\x1b_25a1;c;status=0\x1b\\");
        assert_eq!(
            format_clear_ok(Some(0xE000)),
            "\x1b_25a1;c;cp=e000;status=0\x1b\\"
        );
    }
}
