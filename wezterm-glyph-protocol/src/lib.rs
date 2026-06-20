//! Glyph Protocol — runtime registration of custom vector glyphs at
//! Private-Use-Area codepoints, transported over APC escape sequences.
//!
//! Spec: <https://rapha.land/introducing-glyph-protocol-for-terminals/>
//!
//! This crate holds the renderer-agnostic core: the wire parser, the
//! per-session glossary of registered glyphs, and the `glyf`/COLR decoders
//! and rasterizers. It has no dependency on wezterm's terminal model or
//! GUI; `wezterm-term` owns a [`Glossary`] and `wezterm-gui` reads it to
//! rasterize.

pub mod colr;
pub mod glyf;
pub mod parse;
pub mod place;
pub mod raster;
pub mod sizing;

pub use sizing::SizingParams;

use std::collections::{HashMap, VecDeque};

/// A glyph registered by a client for a single codepoint.
///
/// `Glyf` carries a bare OpenType simple-glyph record; `Color` carries a
/// `colrv0` container. Both carry the [`SizingParams`] parsed from the `r`
/// request, which the renderer applies at draw time (spec §8.5).
///
/// Not `Eq` because [`SizingParams`] holds `f32` padding fractions.
#[derive(Debug, Clone, PartialEq)]
pub enum RegisteredGlyph {
    /// Monochrome OpenType simple-glyph outline, rendered in the cell's
    /// foreground color. `upm` is the units-per-em the outline is
    /// authored in.
    Glyf {
        glyf: Vec<u8>,
        upm: u16,
        sizing: SizingParams,
    },
    /// Layered flat-color glyph (`colrv0` container).
    Color {
        container: Vec<u8>,
        upm: u16,
        sizing: SizingParams,
        /// Whether any layer references the foreground sentinel (CPAL
        /// `0xFFFF`), so the sprite must re-rasterize on foreground change.
        uses_fg: bool,
    },
}

impl RegisteredGlyph {
    /// The units-per-em the outline(s) are authored in.
    pub fn upm(&self) -> u16 {
        match self {
            RegisteredGlyph::Glyf { upm, .. } | RegisteredGlyph::Color { upm, .. } => *upm,
        }
    }

    /// The sizing/placement parameters for this registration.
    pub fn sizing(&self) -> &SizingParams {
        match self {
            RegisteredGlyph::Glyf { sizing, .. } | RegisteredGlyph::Color { sizing, .. } => sizing,
        }
    }

    /// Whether this glyph's rasterization depends on the cell foreground
    /// color. Mono `glyf` glyphs are tinted at draw time (their sprite is a
    /// fg-independent alpha mask), so this is `false` for them. Color glyphs
    /// depend on fg only when they reference the `0xFFFF` sentinel.
    pub fn uses_foreground(&self) -> bool {
        match self {
            RegisteredGlyph::Glyf { .. } => false,
            RegisteredGlyph::Color { uses_fg, .. } => *uses_fg,
        }
    }

    /// Rasterize a monochrome (`glyf`) glyph into a cell-sized alpha mask
    /// to be tinted with the cell foreground color. `baseline` is the text
    /// baseline in pixels from the cell top (for `align=baseline`).
    /// Returns `None` for color glyphs (handled by the COLR path) or if the
    /// payload fails to decode.
    pub fn rasterize_alpha(
        &self,
        cell_w: usize,
        cell_h: usize,
        baseline: f32,
    ) -> Option<raster::AlphaBitmap> {
        match self {
            RegisteredGlyph::Glyf { glyf, sizing, .. } => {
                let outline = glyf::decode(glyf).ok()?;
                raster::rasterize(&outline, sizing, cell_w, cell_h, baseline)
            }
            RegisteredGlyph::Color { .. } => None,
        }
    }

    /// Rasterize a color (`colrv0`) glyph into a straight-alpha RGBA
    /// bitmap. `fg` resolves the foreground-color palette sentinel;
    /// `baseline` is the text baseline in pixels from the cell top.
    /// Returns `None` for mono glyphs or if the container fails to decode.
    pub fn rasterize_color(
        &self,
        cell_w: usize,
        cell_h: usize,
        baseline: f32,
        fg: [u8; 4],
    ) -> Option<colr::RgbaBitmap> {
        match self {
            RegisteredGlyph::Color {
                container, sizing, ..
            } => {
                let c = colr::ColrContainer::parse(container)?;
                colr::rasterize(&c, sizing, cell_w, cell_h, baseline, fg)
            }
            RegisteredGlyph::Glyf { .. } => None,
        }
    }

    /// Validate a payload at registration time, mapping decode failures to
    /// the spec's register error codes (§6.2). Catches composite and
    /// hinted `glyf` records, which v1 rejects.
    pub fn validate(&self) -> Result<(), parse::RegisterError> {
        match self {
            RegisteredGlyph::Glyf { glyf, .. } => match crate::glyf::decode(glyf) {
                Ok(_) => Ok(()),
                Err(crate::glyf::DecodeError::Composite) => {
                    Err(parse::RegisterError::CompositeUnsupported)
                }
                Err(crate::glyf::DecodeError::Hinted) => {
                    Err(parse::RegisterError::HintingUnsupported)
                }
                Err(crate::glyf::DecodeError::Malformed) => {
                    Err(parse::RegisterError::MalformedPayload)
                }
            },
            RegisteredGlyph::Color { container, .. } => {
                match crate::colr::ColrContainer::parse(container) {
                    Some(_) => Ok(()),
                    None => Err(parse::RegisterError::MalformedPayload),
                }
            }
        }
    }
}

/// A registered glyph paired with its monotonically-increasing version.
///
/// The version is bumped every time a codepoint is (re)registered, so the
/// GUI atlas can key cached sprites by `(cp, version, size)` and discard
/// stale sprites when a codepoint is overwritten, evicted, or cleared and
/// later re-registered (spec §7.3).
#[derive(Debug, Clone, PartialEq)]
pub struct VersionedGlyph {
    pub version: u32,
    pub glyph: RegisteredGlyph,
}

/// Maximum number of live registrations per session (spec §7.3). Reaching
/// this and registering a new codepoint evicts the oldest registration.
pub const GLOSSARY_CAPACITY: usize = 1024;

/// Per-session store of registered glyphs, keyed by codepoint.
///
/// Enforces the spec's 1024-slot cap with FIFO eviction (by first
/// insertion order), and assigns every registration a fresh version for
/// atlas cache invalidation.
#[derive(Debug, Default)]
pub struct Glossary {
    entries: HashMap<u32, VersionedGlyph>,
    /// Codepoints in first-insertion order; drives FIFO eviction. A
    /// codepoint appears at most once — overwriting an existing entry
    /// replaces it in place and does not change its eviction position.
    order: VecDeque<u32>,
    /// Source of monotonically-increasing version numbers. Using one
    /// global counter (rather than per-cp) means a codepoint re-registered
    /// after eviction always gets a strictly newer version, so a stale
    /// atlas sprite can never be mistaken for the fresh one.
    next_version: u32,
}

impl Glossary {
    /// Create an empty glossary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or overwrite) the glyph for `cp`, returning its new
    /// version. Overwriting an existing codepoint replaces it in place and
    /// bumps its version, keeping its FIFO position. Registering a new
    /// codepoint while at capacity evicts the oldest registration first.
    pub fn insert(&mut self, cp: u32, glyph: RegisteredGlyph) -> u32 {
        let version = self.next_version;
        self.next_version = self.next_version.wrapping_add(1);

        if self.entries.contains_key(&cp) {
            // Overwrite in place: keep the existing FIFO position.
            self.entries.insert(cp, VersionedGlyph { version, glyph });
        } else {
            if self.entries.len() >= GLOSSARY_CAPACITY {
                // Evict the oldest distinct registration.
                while let Some(old) = self.order.pop_front() {
                    if self.entries.remove(&old).is_some() {
                        break;
                    }
                }
            }
            self.entries.insert(cp, VersionedGlyph { version, glyph });
            self.order.push_back(cp);
        }
        version
    }

    /// Remove the registration for `cp`. Clearing an empty slot is a no-op
    /// (spec §7.2).
    pub fn clear_one(&mut self, cp: u32) {
        if self.entries.remove(&cp).is_some() {
            self.order.retain(|&c| c != cp);
        }
    }

    /// Remove every registration in this session (spec §7.1).
    pub fn clear_all(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    /// Look up the glyph registered for `cp`, if any.
    pub fn get(&self, cp: u32) -> Option<&RegisteredGlyph> {
        self.entries.get(&cp).map(|v| &v.glyph)
    }

    /// The current version of `cp`'s registration, if any.
    pub fn version(&self, cp: u32) -> Option<u32> {
        self.entries.get(&cp).map(|v| v.version)
    }

    /// Whether `cp` has a live registration in this session.
    pub fn contains(&self, cp: u32) -> bool {
        self.entries.contains_key(&cp)
    }

    /// Number of live registrations.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the glossary holds no registrations.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The set of registered codepoints (unordered).
    pub fn codepoints(&self) -> Vec<u32> {
        self.entries.keys().copied().collect()
    }

    /// A cheap-to-share snapshot of the full registration map. The
    /// renderer takes this once per pane paint (before the terminal
    /// render lock) so it can rasterize registered glyphs without locking
    /// during paint. Each entry carries its version for cache keying.
    pub fn snapshot(&self) -> GlossarySnapshot {
        std::sync::Arc::new(self.entries.clone())
    }
}

/// An immutable, shareable snapshot of a glossary's registrations, keyed
/// by codepoint, each carrying its version. Handed to the GUI renderer per
/// frame.
pub type GlossarySnapshot = std::sync::Arc<HashMap<u32, VersionedGlyph>>;

/// A glossary shared between the terminal model (which registers glyphs
/// from escape sequences) and the GUI (which reads it to rasterize).
/// Plain `std::sync::Mutex` so the core crate stays free of renderer
/// dependencies.
pub type SharedGlossary = std::sync::Arc<std::sync::Mutex<Glossary>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn glyf(tag: u8) -> RegisteredGlyph {
        RegisteredGlyph::Glyf {
            glyf: vec![tag],
            upm: 1000,
            sizing: SizingParams::defaults_for_upm(1000),
        }
    }

    #[test]
    fn insert_then_get_returns_glyph() {
        let mut g = Glossary::new();
        let glyph = RegisteredGlyph::Glyf {
            glyf: vec![1, 2, 3],
            upm: 1000,
            sizing: SizingParams::defaults_for_upm(1000),
        };
        g.insert(0xE000, glyph.clone());

        assert_eq!(g.get(0xE000), Some(&glyph));
        assert_eq!(g.get(0xE001), None);
        assert!(g.contains(0xE000));
        assert!(!g.contains(0xE001));
    }

    #[test]
    fn version_bumps_on_each_insert_and_overwrite() {
        let mut g = Glossary::new();
        let v0 = g.insert(0xE000, glyf(1));
        let v1 = g.insert(0xE001, glyf(2));
        // Overwrite E000 with a different glyph; version must advance.
        let v2 = g.insert(0xE000, glyf(9));
        assert!(v1 > v0);
        assert!(v2 > v1, "overwrite must bump version");
        assert_eq!(g.version(0xE000), Some(v2));
        assert_eq!(g.get(0xE000), Some(&glyf(9)));
        // Overwrite does not change the eviction order (E000 stays oldest).
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn fifo_eviction_at_capacity() {
        let mut g = Glossary::new();
        // Fill to capacity with distinct PUA codepoints.
        for i in 0..GLOSSARY_CAPACITY as u32 {
            g.insert(0xE000 + i, glyf(0));
        }
        assert_eq!(g.len(), GLOSSARY_CAPACITY);
        assert!(g.contains(0xE000), "oldest still present at capacity");

        // The next distinct registration evicts the oldest (0xE000).
        g.insert(0xF000, glyf(0));
        assert_eq!(g.len(), GLOSSARY_CAPACITY);
        assert!(!g.contains(0xE000), "oldest must be evicted (FIFO)");
        assert!(g.contains(0xE001), "second-oldest survives");
        assert!(g.contains(0xF000), "newest present");
    }

    #[test]
    fn overwrite_does_not_refresh_fifo_position() {
        let mut g = Glossary::new();
        for i in 0..GLOSSARY_CAPACITY as u32 {
            g.insert(0xE000 + i, glyf(0));
        }
        // Overwrite the oldest; it must remain the eviction target.
        g.insert(0xE000, glyf(7));
        g.insert(0xF000, glyf(0));
        assert!(!g.contains(0xE000), "overwrite must not save it from FIFO");
    }

    #[test]
    fn clear_one_then_reinsert_gets_fresh_version() {
        let mut g = Glossary::new();
        let v0 = g.insert(0xE000, glyf(1));
        g.clear_one(0xE000);
        assert!(!g.contains(0xE000));
        let v1 = g.insert(0xE000, glyf(2));
        assert!(v1 > v0, "re-registration after clear gets a newer version");
    }

    #[test]
    fn clear_all_empties_and_frees_slots() {
        let mut g = Glossary::new();
        for i in 0..10 {
            g.insert(0xE000 + i, glyf(0));
        }
        g.clear_all();
        assert!(g.is_empty());
        // Order is cleared too, so a fresh fill still evicts correctly.
        for i in 0..GLOSSARY_CAPACITY as u32 {
            g.insert(0xF000 + i, glyf(0));
        }
        g.insert(0x10_0000, glyf(0));
        assert!(!g.contains(0xF000));
    }
}
