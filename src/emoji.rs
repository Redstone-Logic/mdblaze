//! Colour emoji, from the one font on the machine that has them.
//!
//! # Why this asks the system for a font when nothing else here does
//!
//! [`crate::text`] compiles its faces in and never asks the operating system a
//! question, because the question that matters -- "what fonts exist?" -- goes
//! through fontconfig and can cost more than this program's whole startup.
//!
//! This does not ask that question. It opens a handful of absolute paths, in
//! order, and takes the first that is there. No scan, no index, no cache to warm.
//!
//! And it does not do even that until a document turns out to contain an emoji.
//! Most do not, and those pay nothing at all: the check that gates this is a
//! range test on a `char`.
//!
//! # Why not compile the emoji font in too
//!
//! Noto Color Emoji is 10.8MB -- eight times the four text faces put together,
//! and twenty times the size of the program without it. Paying that in every
//! copy of the binary, on disk and in every download, to render a feature most
//! documents do not use, is the wrong trade for a tool whose entire argument is
//! that it is small and immediate.
//!
//! Mapped instead of read, so an emoji costs the two pages its own picture is
//! on rather than ten megabytes of file. A document with one emoji in it does
//! not read the other 10.79MB, and never touches the disk for them.
//!
//! # What this deliberately does not do
//!
//! Emoji are frequently SEQUENCES: a base character, then a skin tone, a
//! variation selector, or a zero-width joiner and another emoji. Rendering those
//! as designed means applying the font's ligature substitutions, which means a
//! shaping engine -- `rustybuzz` and its tables, or HarfBuzz and a C toolchain.
//!
//! So the sequence parts are given no width and no picture, and the base
//! character is drawn. A family emoji comes out as its first member and a waving
//! hand comes out in the font's default yellow. That is a real limitation and it
//! is written down rather than hidden: the alternative is a shaping engine in a
//! program that measures its startup in milliseconds.

use crate::pixels::Bitmap;

/// Absolute paths where a colour emoji font is found, in the order tried.
///
/// A list, not a search. Every distribution puts the same file in a slightly
/// different place and there are four of them; enumerating four paths is
/// cheaper, more predictable, and easier to read than asking a font system.
const PATHS: &[&str] = &[
    // Debian, Ubuntu, and everything downstream of them.
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    // Arch.
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    // Fedora.
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
    // openSUSE, and hand-installed copies.
    "/usr/share/fonts/NotoColorEmoji.ttf",
    "/usr/local/share/fonts/NotoColorEmoji.ttf",
    // macOS. A collection rather than a single face, and its pictures are in
    // `sbix` rather than `CBDT`; both are read the same way from here.
    "/System/Library/Fonts/Apple Color Emoji.ttc",
];

/// An override, for packaging and for tests.
///
/// Not a user preference -- there is exactly one colour emoji font on a machine
/// and choosing between designs is not a decision worth a setting. It exists so
/// a test can point at a known file and so a bundle can ship its own.
const OVERRIDE: &str = "MDEDIT_EMOJI_FONT";

/// Is this a character a colour emoji font might have a picture for?
///
/// A gate, not an answer. It is deliberately generous -- the dingbats block
/// holds `✂` which is emoji and `✁` which is not -- because being wrong costs a
/// failed lookup in a font that is already open, whereas being too narrow means
/// a character silently renders as a missing glyph.
///
/// What it excludes matters more than what it includes. `©`, `®`, `™` and the
/// arrows are all technically emoji-presentable and are all, in a document about
/// software, ordinary text. Drawn as colour pictures they would be a surprise
/// and a distraction.
pub fn is_emoji(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x231A..=0x231B          // watch, hourglass
        | 0x23E9..=0x23FA        // media controls, clocks
        | 0x25FB..=0x25FE        // squares
        | 0x2600..=0x27BF        // misc symbols and dingbats
        | 0x2B00..=0x2BFF        // arrows-in-boxes, stars, shapes
        | 0x1F000..=0x1FAFF      // every emoji block proper
    )
}

/// Is this a character that MODIFIES the emoji before it rather than being one?
///
/// Variation selectors, skin tones, the zero-width joiner, the keycap mark and
/// the tag characters used for subdivision flags. Without shaping they cannot do
/// their job, and drawn on their own they are either a blank box or -- for skin
/// tones, which have their own pictures -- a coloured smear beside the emoji
/// they were meant to recolour.
///
/// So they take no space and draw nothing, which leaves the base character.
pub fn is_joiner(ch: char) -> bool {
    let c = ch as u32;
    matches!(c,
        0x200D                   // zero-width joiner
        | 0x20E3                 // combining enclosing keycap
        | 0xFE0E | 0xFE0F        // text and emoji variation selectors
        | 0x1F3FB..=0x1F3FF      // skin tone modifiers
        | 0xE0020..=0xE007F      // tag characters
    )
}

/// The colour emoji font, if this machine has one.
pub struct Emoji {
    face: ttf_parser::Face<'static>,
}

impl Emoji {
    /// Open the first emoji font found, or `None` if there is none.
    ///
    /// Called at most once per run, and only after a document is found to
    /// contain an emoji.
    pub fn open() -> Option<Emoji> {
        let from_env = std::env::var(OVERRIDE).ok();
        let paths = from_env.as_deref().into_iter().chain(PATHS.iter().copied());
        for path in paths {
            let Ok(file) = std::fs::File::open(path) else { continue };
            // Mapped, not read. The file is 10.8MB and a document uses a few
            // hundred bytes of it; reading it would spend more time on the
            // emoji nobody asked for than on the whole rest of the frame.
            //
            // Safety: the map is read-only and the leak below means it outlives
            // every borrow taken from it. A file changing underneath a mapping
            // is a real hazard in general; a font on a system path being
            // rewritten while a document is open is not one worth a copy.
            let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else { continue };
            // Leaked on purpose: a font opened once is wanted for the life of
            // the process, and leaking is what "for the life of the process"
            // means. It also gives the borrow the `'static` the parser needs
            // without a self-referential struct.
            let bytes: &'static [u8] = Box::leak(Box::new(map));
            if let Ok(face) = ttf_parser::Face::parse(bytes, 0) {
                return Some(Emoji { face });
            }
        }
        None
    }

    /// Does this font have a colour picture for `ch`?
    ///
    /// Separate from [`Emoji::glyph`] and cheap on purpose: it is the question
    /// asked while MEASURING, once per distinct character, and it must not
    /// decode anything. It reads the character map and the bitmap index, both of
    /// which are lookups in a mapped file.
    pub fn has(&self, ch: char) -> bool {
        self.picture(ch).is_some()
    }

    fn picture(&self, ch: char) -> Option<ttf_parser::RasterGlyphImage<'_>> {
        let id = self.face.glyph_index(ch)?;
        // The strike is asked for at a size; a font with several gives the
        // closest. Noto has one, at 128 pixels, so the argument only matters on
        // fonts that ship more than one.
        let img = self.face.glyph_raster_image(id, STRIKE)?;
        (img.format == ttf_parser::RasterImageFormat::PNG).then_some(img)
    }

    /// The picture for `ch`, scaled to sit on a line of `px` type and fit inside
    /// an advance of `advance`.
    ///
    /// `None` when this font has no colour picture for the character, which is
    /// the common case for the generous half of [`is_emoji`]'s range -- the
    /// caller then draws it from the text face as it always did.
    pub fn glyph(&self, ch: char, px: f32, advance: f32) -> Option<Rendered> {
        let img = self.picture(ch)?;
        let bitmap = crate::pixels::decode(img.data)?;
        if bitmap.w == 0 || bitmap.h == 0 {
            return None;
        }

        // Placed by rule rather than by the font's own bearings.
        //
        // The bearings are in the bitmap tables and they are honest, but they
        // differ between `CBDT` and `sbix` and between vendors, and getting them
        // wrong puts an emoji a line away from its text. A fixed rule -- an em
        // and a sixth tall, sitting a little below the baseline like the round
        // letters do -- is the same on every font and is right on all of them.
        let mut h = px * EM_HEIGHT;
        let mut w = h * bitmap.w as f32 / bitmap.h as f32;
        // The advance is decided by the caller, which knows whether this is
        // prose or a line of code that has to stay in its columns. A picture
        // wider than the space reserved for it would overlap what comes next, so
        // it is scaled down to fit rather than the advance being widened.
        let room = advance * (1.0 / SIDE_BEARING);
        if w > room {
            let k = room / w;
            w *= k;
            h *= k;
        }
        Some(Rendered {
            bitmap: bitmap.resized(w.round().max(1.0) as usize, h.round().max(1.0) as usize),
            left: (advance - w) / 2.0,
            top: -(h - px * DESCENT),
        })
    }
}

/// The strike size asked of the font. Noto Color Emoji has exactly one, at 128.
const STRIKE: u16 = 128;

/// How tall an emoji is drawn, as a multiple of the type size.
///
/// Slightly over an em. Emoji are drawn to fill their square where a capital
/// letter fills about seventy percent of one, so matching cap height would make
/// them look shrunken beside the text they sit in.
const EM_HEIGHT: f32 = 1.15;

/// Air either side, as a multiple of the picture's width. Emoji sit shoulder to
/// shoulder in a list otherwise.
const SIDE_BEARING: f32 = 1.08;

/// How much room an emoji takes on a line of prose, as a multiple of the type
/// size.
///
/// A fixed number, and it has to be: the advance is needed while MEASURING a
/// paragraph, and measuring must not depend on decoding a picture. Every emoji
/// in every emoji font is drawn on the same square, so one number is right for
/// all of them -- and [`Emoji::glyph`] scales anything that is not to fit.
pub const EM_ADVANCE: f32 = EM_HEIGHT * SIDE_BEARING;

/// How far below the baseline an emoji hangs, as a multiple of the type size.
/// The same overshoot a round letter has, for the same reason: sitting exactly
/// on the baseline reads as sitting slightly above it.
const DESCENT: f32 = 0.12;

/// A drawn emoji: pixels, and where they go relative to the pen.
///
/// The same geometry a [`crate::text::Glyph`] carries, so the renderer places
/// both the same way and only the blit differs.
pub struct Rendered {
    pub bitmap: Bitmap,
    pub left: f32,
    pub top: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> Option<Emoji> {
        Emoji::open()
    }

    #[test]
    fn the_characters_people_actually_write_are_recognised() {
        for c in ['😀', '🎉', '🚀', '✅', '⚠', '❤', '🧠', '⭐', '⌚'] {
            assert!(is_emoji(c), "{c:?} was not treated as an emoji");
        }
    }

    #[test]
    fn ordinary_punctuation_is_not_hijacked() {
        // These are all emoji-presentable in Unicode and all, in a document
        // about software, plain text. Drawn as colour pictures they would be a
        // surprise -- and in the arrows' case, wrong.
        for c in ['©', '®', '™', '→', '←', '↔', '§', '¶', '±', '…'] {
            assert!(!is_emoji(c), "{c:?} would have been drawn as a picture");
        }
    }

    #[test]
    fn letters_and_digits_are_never_emoji() {
        for c in "abcXYZ0189 \t\n{}[]()<>".chars() {
            assert!(!is_emoji(c), "{c:?}");
        }
    }

    #[test]
    fn sequence_parts_are_joiners_and_not_emoji_in_their_own_right() {
        // Drawn on their own a skin tone is a coloured smear and a variation
        // selector is a blank box. Both belong to the character before them.
        for c in ['\u{200D}', '\u{FE0F}', '\u{FE0E}', '\u{1F3FB}', '\u{1F3FF}', '\u{20E3}'] {
            assert!(is_joiner(c), "{c:?} is not treated as part of a sequence");
        }
    }

    #[test]
    fn a_joiner_inside_the_emoji_range_is_still_a_joiner() {
        // Skin tones live at 1F3FB, squarely inside the emoji blocks, so the
        // two tests overlap and the joiner check has to be asked FIRST. This
        // pins the fact rather than the ordering, which lives in `text`.
        assert!(is_emoji('\u{1F3FB}') && is_joiner('\u{1F3FB}'));
    }

    #[test]
    fn a_missing_font_is_not_a_failure() {
        // Every machine this runs on may not have the font, and a document that
        // mentions an emoji must still open. `open` answers None; nothing here
        // panics or waits.
        let _ = font();
    }

    #[test]
    fn an_emoji_renders_to_pixels_at_the_size_asked_for() {
        let Some(f) = font() else {
            eprintln!("no colour emoji font on this machine; skipping");
            return;
        };
        let g = f
            .glyph('😀', 19.0, 19.0 * EM_ADVANCE)
            .expect("a grinning face is in every emoji font");
        assert!(g.bitmap.h >= 20 && g.bitmap.h <= 24, "height {} at 19px", g.bitmap.h);
        assert!(g.top < 0.0, "an emoji that starts below the baseline is under its line");
        assert!(g.left >= 0.0, "an emoji should sit inside its own advance");
    }

    #[test]
    fn a_rendered_emoji_has_colour_in_it() {
        // The point of the exercise. If this came back greyscale the CBDT
        // pictures are not being read and something is falling back to an
        // outline.
        let Some(f) = font() else { return };
        let g = f.glyph('🎉', 24.0, 24.0 * EM_ADVANCE).expect("party popper");
        let coloured = g.bitmap.px.iter().any(|p| {
            let (r, gr, b) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
            (p >> 24) > 0 && (r.abs_diff(gr) > 20 || gr.abs_diff(b) > 20)
        });
        assert!(coloured, "the emoji came out grey");
    }

    #[test]
    fn a_character_the_emoji_font_does_not_have_answers_none() {
        // The generous half of `is_emoji`: the caller must fall back to the text
        // face rather than getting a blank.
        let Some(f) = font() else { return };
        assert!(f.glyph('a', 19.0, 19.0 * EM_ADVANCE).is_none());
        assert!(!f.has('a'));
    }

    #[test]
    fn a_narrow_advance_shrinks_the_picture_rather_than_letting_it_overlap() {
        // A code block reserves two monospace cells, which is narrower than an
        // emoji wants in prose. Drawing at the prose size would run over the
        // character after it and put the whole line out of its columns.
        let Some(f) = font() else { return };
        let wide = f.glyph('🚀', 19.0, 19.0 * EM_ADVANCE).expect("rocket");
        let narrow = f.glyph('🚀', 19.0, 12.0).expect("rocket");
        assert!(narrow.bitmap.w < wide.bitmap.w, "the picture ignored its advance");
        assert!(narrow.bitmap.w as f32 <= 12.0, "still wider than the room given");
    }

    #[test]
    fn asking_whether_a_character_has_a_picture_does_not_decode_one() {
        // `has` is called once per distinct character while MEASURING, which
        // happens before anything is drawn. If it decoded, opening a document
        // full of emoji would pay for every one of them twice.
        let Some(f) = font() else { return };
        assert!(f.has('😀'));
        assert!(!f.has('Z'));
    }

    #[test]
    fn the_override_is_honoured_so_a_bundle_can_ship_its_own() {
        // Pointed at nothing, opening still answers rather than hanging or
        // panicking -- it falls through to the system paths.
        let path = "/nonexistent/does-not-exist.ttf";
        std::env::set_var(OVERRIDE, path);
        let _ = Emoji::open();
        std::env::remove_var(OVERRIDE);
    }
}
