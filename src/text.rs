//! Glyphs, from faces compiled into the binary.
//!
//! # Why nothing here asks the operating system anything
//!
//! The obvious way to get a font is to ask the system for one. On Linux that
//! means fontconfig, whose first call scans font directories and can cost tens
//! of milliseconds on a cold cache -- more than this entire program's budget,
//! spent before a single glyph is drawn, to answer a question it does not need
//! answered. It has one document to show in one family.
//!
//! So the faces are `include_bytes!`. Startup cost is a pointer.
//!
//! The trade is real and worth stating: this renders every document in DejaVu
//! whatever the reader has installed, and it cannot show a script the embedded
//! faces lack -- CJK, Arabic, Devanagari all come out as missing glyphs. That is
//! a deliberate limit of a fast single-purpose viewer, not an oversight, and the
//! fix when it matters is to embed more coverage rather than to start asking.
//!
//! # Why the parser is lazy, and why that turned out to matter more
//!
//! The first version used an eager parser, which builds outline structures for
//! every glyph in the file when the font is loaded. Compiling the faces in made
//! that cost 36ms PER FACE -- 100ms for three, which was ninety-five percent of
//! this program's startup. Avoiding fontconfig and then paying the same budget
//! to parse the fonts anyway is not a saving.
//!
//! `ab_glyph` is zero-copy and lazy: loading is 0.008ms because it does nothing
//! but validate the tables, and a glyph's outline is built the first time it is
//! actually drawn. A document uses a few hundred distinct glyphs, so the work is
//! proportional to what is on screen rather than to what the face contains.
//!
//! # Synthetic italics
//!
//! DejaVu ships an oblique sans face; the `fonts-dejavu-core` package does not
//! include it, so shipping italics would mean vendoring a fourth file. Instead
//! italics are sheared from the regular face at blit time -- a horizontal offset
//! proportional to height, which is what an oblique face largely is. It is not
//! a true italic (no redrawn letterforms) and at small sizes nobody can tell.

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _};
use std::collections::HashMap;

/// The faces actually compiled in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Sans,
    SansBold,
    Mono,
}

/// Rise over run for synthetic italics. 0.21 is close to the angle DejaVu's own
/// oblique uses.
pub const SHEAR: f32 = 0.21;

/// Leading for prose, as a multiple of the type size.
///
/// Needed because the font's own numbers do not supply any. `ab_glyph` scales so
/// that ascent minus descent equals the requested size, and DejaVu declares a
/// line gap of zero -- so the "natural" line height at 16px is exactly 16px, and
/// consecutive lines touch. A test asserting a line is taller than the type on it
/// is what caught it; on screen it reads as a wall of text.
pub const LEADING: f32 = 1.45;

/// Leading for code. Tighter, because a code block is a shape as much as it is
/// text and loose lines break the block up.
pub const CODE_LEADING: f32 = 1.25;

const SANS: &[u8] = include_bytes!("../assets/fonts/DejaVuSans.ttf");
const SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf");
const MONO: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

/// A rasterised glyph: coverage values, and where to put them.
pub struct Glyph {
    pub bitmap: Vec<u8>,
    pub width: usize,
    pub height: usize,
    /// Offset from the pen to the bitmap's left edge.
    pub left: f32,
    /// Offset from the BASELINE to the bitmap's top edge, positive downward.
    /// So a capital letter's `top` is negative: it sits above the baseline.
    pub top: f32,
    pub advance: f32,
}

#[derive(PartialEq, Eq, Hash)]
struct Key {
    face: Face,
    ch: char,
    /// Size in 1/4 pixels, so a cache key is exact rather than a float compared
    /// for equality.
    quarters: u32,
}

pub struct Text {
    sans: FontRef<'static>,
    sans_bold: FontRef<'static>,
    mono: FontRef<'static>,
    cache: HashMap<Key, Glyph>,
}

impl Text {
    /// Reference the compiled-in faces. Validates their tables and nothing more;
    /// measured at 0.008ms per face, against 36ms for an eager parser.
    pub fn new() -> Self {
        Text {
            sans: FontRef::try_from_slice(SANS).expect("embedded sans is valid"),
            sans_bold: FontRef::try_from_slice(SANS_BOLD).expect("embedded bold is valid"),
            mono: FontRef::try_from_slice(MONO).expect("embedded mono is valid"),
            cache: HashMap::new(),
        }
    }

    fn font(&self, face: Face) -> &FontRef<'static> {
        match face {
            Face::Sans => &self.sans,
            Face::SansBold => &self.sans_bold,
            Face::Mono => &self.mono,
        }
    }

    /// How far the pen moves. Cheap enough to call per character while wrapping,
    /// and it does not rasterise -- measuring a paragraph should not fill the
    /// cache with glyphs that turn out to be off screen.
    pub fn advance(&self, face: Face, ch: char, px: f32) -> f32 {
        let f = self.font(face).as_scaled(PxScale::from(px));
        f.h_advance(f.scaled_glyph(ch).id)
    }

    /// Width of a string at one size. What line breaking is decided against.
    pub fn width(&self, face: Face, s: &str, px: f32) -> f32 {
        s.chars().map(|c| self.advance(face, c, px)).sum()
    }

    /// A rasterised glyph, from the cache or freshly drawn into it.
    ///
    /// Cached because a document reuses a few hundred distinct glyphs across
    /// thousands of positions, and rasterising is the expensive half.
    pub fn glyph(&mut self, face: Face, ch: char, px: f32) -> &Glyph {
        let key = Key { face, ch, quarters: (px * 4.0).round() as u32 };
        if !self.cache.contains_key(&key) {
            let font = self.font(face);
            let scaled = font.as_scaled(PxScale::from(px));
            let glyph = scaled.scaled_glyph(ch);
            let advance = scaled.h_advance(glyph.id);
            let g = match font.outline_glyph(glyph) {
                Some(outline) => {
                    // Bounds are in pixels relative to the pen on the baseline,
                    // y positive downward -- so `min.y` is negative for anything
                    // that rises above the baseline, which is most of the Latin
                    // alphabet.
                    let b = outline.px_bounds();
                    let w = b.width().ceil().max(0.0) as usize;
                    let h = b.height().ceil().max(0.0) as usize;
                    let mut bitmap = vec![0u8; w * h];
                    outline.draw(|x, y, c| {
                        let (x, y) = (x as usize, y as usize);
                        if x < w && y < h {
                            bitmap[y * w + x] = (c * 255.0).round().clamp(0.0, 255.0) as u8;
                        }
                    });
                    Glyph { bitmap, width: w, height: h, left: b.min.x, top: b.min.y, advance }
                }
                // A space, or a character this face has no outline for. It still
                // advances the pen, which is the whole of its contribution.
                None => Glyph {
                    bitmap: Vec::new(),
                    width: 0,
                    height: 0,
                    left: 0.0,
                    top: 0.0,
                    advance,
                },
            };
            self.cache.insert(Key { face, ch, quarters: key.quarters }, g);
        }
        self.cache.get(&key).expect("just inserted")
    }

    /// Distance between baselines, for prose.
    pub fn line_height(&self, face: Face, px: f32) -> f32 {
        self.line_height_with(face, px, LEADING)
    }

    /// Distance between baselines at a chosen leading.
    pub fn line_height_with(&self, face: Face, px: f32, leading: f32) -> f32 {
        let f = self.font(face).as_scaled(PxScale::from(px));
        (f.height() + f.line_gap()) * leading
    }

    /// Distance from the top of a line to its baseline.
    pub fn ascent(&self, face: Face, px: f32) -> f32 {
        self.font(face).as_scaled(PxScale::from(px)).ascent()
    }

    /// How many glyphs are held. Exposed so a test can prove the cache is one.
    pub fn cached(&self) -> usize {
        self.cache.len()
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_faces_parse() {
        // If this fails the binary shipped a broken font and every document is
        // blank, which is worth failing loudly and early for.
        let t = Text::new();
        for f in [Face::Sans, Face::SansBold, Face::Mono] {
            assert!(t.advance(f, 'M', 16.0) > 0.0, "{f:?} has no advance for M");
        }
    }

    #[test]
    fn mono_is_actually_monospaced() {
        // Code blocks are laid out on the assumption that every character is the
        // same width. If that is ever false the alignment is silently wrong.
        let t = Text::new();
        let w = t.advance(Face::Mono, 'i', 14.0);
        for c in ['M', 'W', '.', '0', 'l'] {
            assert!(
                (t.advance(Face::Mono, c, 14.0) - w).abs() < 0.01,
                "{c:?} is a different width to 'i' in mono"
            );
        }
    }

    #[test]
    fn the_proportional_face_is_not_monospaced() {
        // The mirror of the test above: if the sans face were fixed-width, the
        // test above would pass for the wrong reason.
        let t = Text::new();
        assert!(
            (t.advance(Face::Sans, 'i', 14.0) - t.advance(Face::Sans, 'M', 14.0)).abs() > 0.5
        );
    }

    #[test]
    fn bold_is_wider_than_regular() {
        // Cheap proof that Face::SansBold really is a different face rather than
        // the regular one loaded twice, which would silently lose emphasis.
        let t = Text::new();
        let s = "the quick brown fox";
        assert!(t.width(Face::SansBold, s, 16.0) > t.width(Face::Sans, s, 16.0));
    }

    #[test]
    fn a_glyph_is_rasterised_once_and_reused() {
        let mut t = Text::new();
        assert_eq!(t.cached(), 0);
        for _ in 0..50 {
            let _ = t.glyph(Face::Sans, 'a', 16.0);
        }
        assert_eq!(t.cached(), 1, "fifty draws of one glyph is one raster");
        let _ = t.glyph(Face::Sans, 'a', 24.0);
        assert_eq!(t.cached(), 2, "a different size is a different glyph");
    }

    #[test]
    fn width_is_the_sum_of_its_characters() {
        let t = Text::new();
        let a = t.width(Face::Sans, "ab", 16.0);
        let b = t.advance(Face::Sans, 'a', 16.0) + t.advance(Face::Sans, 'b', 16.0);
        assert!((a - b).abs() < 0.001);
    }

    #[test]
    fn a_line_is_taller_than_the_type_on_it() {
        // Not a tautology: the font supplies no leading at all. Scaled so that
        // ascent minus descent IS the type size, with a declared line gap of
        // zero, the natural line height is exactly the size and lines touch.
        // This asserts the leading is applied rather than assumed.
        let t = Text::new();
        let lh = t.line_height(Face::Sans, 16.0);
        assert!(lh > 16.0 * 1.2, "no leading: line height {lh} at 16px");
        assert!(t.ascent(Face::Sans, 16.0) > 0.0);
    }

    #[test]
    fn code_lines_sit_closer_than_prose_lines() {
        let t = Text::new();
        assert!(
            t.line_height_with(Face::Mono, 14.0, CODE_LEADING) < t.line_height(Face::Mono, 14.0)
        );
    }
}
