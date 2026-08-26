//! Laid-out runs to pixels, in software.
//!
//! Deliberately not a GPU path. Creating a graphics context costs more than
//! everything else this program does put together, and the work here is blitting
//! a few thousand small coverage bitmaps into a buffer once per frame. A CPU does
//! that faster than a GPU can be asked to.
//!
//! Only what is on screen is drawn: runs above or below the viewport are skipped
//! before any glyph is touched, so scrolling a long document costs the same as
//! scrolling a short one.

use crate::layout::{Ink, Laid, Picture, PAD};
use crate::text::{Face, Glyph, Pixels, Text, SHEAR};

/// Colours, as `0x00RRGGBB` to match softbuffer's format.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: u32,
    pub body: u32,
    pub strong: u32,
    pub dim: u32,
    pub link: u32,
    pub code: u32,
    pub code_bg: u32,
    pub keyword: u32,
    pub string: u32,
    pub number: u32,
    pub comment: u32,
    /// The status line's ground, and the caret.
    pub bar: u32,
    pub caret: u32,
    /// The status bar while unsaved work is one keypress from being discarded.
    pub alarm_bg: u32,
    /// The highlighted row in the file listing.
    ///
    /// Its own token rather than reusing the caret's crimson: the caret is a
    /// two-pixel line and reads as an accent, while the same colour across a
    /// whole row reads as an alarm. This is the accent with most of the
    /// saturation taken out, so it says "here" without saying "careful".
    pub select: u32,
}

impl Theme {
    /// Redstone Logic on Refresh Black. The same ten tokens the console theme
    /// uses, so the two products do not look like different companies.
    pub const DARK: Theme = Theme {
        bg: 0x0012_1212,
        body: 0x00e8_e8e8,
        strong: 0x00ff_ffff,
        dim: 0x00a8_a8a8,
        link: 0x00c9_6a63,
        code: 0x00d6_d6d6,
        code_bg: 0x0017_1717,
        // Enough hues to tell the four apart and no more. A code block with six
        // colours in it is decoration; these are the distinctions that change
        // what you understand -- what runs, what is text, what is a number, and
        // what the machine ignores.
        keyword: 0x00d8_8a84,
        string: 0x008f_b878,
        number: 0x00d2_b078,
        comment: 0x0080_8080,
        bar: 0x001b_1b1b,
        caret: 0x00b6_3c35,
        alarm_bg: 0x0053_1f1c,
        select: 0x0035_2320,
    };

    fn of(&self, ink: Ink) -> u32 {
        match ink {
            Ink::Body => self.body,
            Ink::Strong => self.strong,
            Ink::Dim => self.dim,
            Ink::Link => self.link,
            Ink::Code => self.code,
            Ink::Keyword => self.keyword,
            Ink::Str => self.string,
            Ink::Number => self.number,
            Ink::Comment => self.comment,
        }
    }
}

/// Mix `fg` over `bg` by `a` (0-255). Per channel, no gamma correction: correct
/// blending would need a linear round trip per pixel, and at text sizes on a dark
/// ground the difference is not visible.
#[inline]
fn blend(bg: u32, fg: u32, a: u8) -> u32 {
    if a == 0 {
        return bg;
    }
    if a == 255 {
        return fg;
    }
    let a = u32::from(a);
    let inv = 255 - a;
    let mix = |shift: u32| {
        let f = (fg >> shift) & 0xff;
        let b = (bg >> shift) & 0xff;
        ((f * a + b * inv) / 255) & 0xff
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

/// Put one glyph's pixels into the buffer at `gx`, `gy` -- the top-left of its
/// bitmap, in buffer coordinates.
///
/// One function for both kinds of glyph. A coverage mask is painted in
/// `colour`; a colour glyph carries its own and `colour` is not used, because
/// there is no meaningful way to tint a picture of a rocket.
///
/// `shear` leans each row by its distance above the bottom of the glyph, for a
/// synthetic italic. It is zero everywhere now that a real italic face is
/// compiled in, and the parameter stays because a face without one would want
/// it back rather than losing emphasis.
#[allow(clippy::too_many_arguments)]
fn blit(
    buf: &mut [u32],
    width: usize,
    height: usize,
    g: &Glyph,
    gx: f32,
    gy: f32,
    colour: u32,
    shear: f32,
) {
    for row in 0..g.height {
        let py = gy.round() as i64 + row as i64;
        if py < 0 || py >= height as i64 {
            continue;
        }
        let lean = if shear == 0.0 { 0.0 } else { (g.height as f32 - row as f32) * shear };
        let base = py as usize * width;
        for col in 0..g.width {
            let (fg, a) = match &g.pixels {
                Pixels::Mask(m) => (colour, m[row * g.width + col]),
                Pixels::Colour(c) => {
                    let p = c[row * g.width + col];
                    (p & 0x00ff_ffff, ((p >> 24) & 0xff) as u8)
                }
            };
            if a == 0 {
                continue;
            }
            let px = (gx + lean).round() as i64 + col as i64;
            if px < 0 || px >= width as i64 {
                continue;
            }
            let i = base + px as usize;
            buf[i] = blend(buf[i], fg, a);
        }
    }
}

/// Pictures at the size they are being drawn, kept between frames.
///
/// Not an optimisation to be tidied away later. Scaling is where the work in a
/// picture is -- a full-window screenshot down to the column is a million source
/// pixels averaged -- and it happens once per FRAME, which is once per keystroke
/// and once per scroll step. Measured at 15.5ms a frame against 0.4ms for the
/// same page without a picture on it. Typing beside a screenshot was visibly
/// behind the keyboard.
///
/// The size only changes when the window does, so almost every frame is a hit.
/// Keyed on which picture and what size, and the picture is identified by its
/// address -- the same decoded bytes are shared by every use of one URL, so this
/// is exactly the identity wanted and it costs nothing to take.
#[derive(Default)]
pub struct Scaled {
    at: std::collections::HashMap<usize, Entry>,
}

/// One picture's scaled copy, and the source it came from.
///
/// The source `Rc` is held here rather than only borrowed, and that is what
/// makes an ADDRESS a sound key: while this entry lives, the allocation it names
/// cannot be freed, so the address cannot be handed to a different picture. Key
/// on a raw pointer without holding the thing it points at and a freed-then-
/// reallocated bitmap silently answers with somebody else's pixels.
struct Entry {
    /// Never read, and that is the point: holding it keeps the allocation the
    /// key names alive, so the address cannot be reused by a different picture.
    /// Dropping this field would compile and would silently reintroduce that.
    #[allow(dead_code)]
    source: std::rc::Rc<crate::pixels::Bitmap>,
    w: usize,
    h: usize,
    scaled: std::rc::Rc<crate::pixels::Bitmap>,
}

impl Scaled {
    fn get(
        &mut self,
        art: &std::rc::Rc<crate::pixels::Bitmap>,
        w: usize,
        h: usize,
    ) -> std::rc::Rc<crate::pixels::Bitmap> {
        let key = std::rc::Rc::as_ptr(art) as usize;
        // ONE entry per picture, replaced when the size changes -- not one per
        // size ever seen.
        //
        // Keeping every size looks like a better cache and is a memory leak with
        // a slow fuse: winit delivers a resize event per frame, so dragging a
        // window edge once asks for a few hundred different widths and every one
        // of them is kept for ever. Measured: 2.3MB at rest, 217MB after a
        // single drag across a document with one screenshot in it, and it never
        // came back down. A picture is only ever on screen at one size, so the
        // other several hundred copies were unreachable the moment they were
        // made.
        if let Some(e) = self.at.get(&key) {
            if e.w == w && e.h == h {
                return e.scaled.clone();
            }
        }
        let scaled = std::rc::Rc::new(art.resized(w, h));
        self.at.insert(key, Entry { source: art.clone(), w, h, scaled: scaled.clone() });
        scaled
    }

    /// How many pictures are held. For a test to prove this is a cache, and that
    /// it is bounded by the document rather than by how long the window has been
    /// dragged about.
    pub fn held(&self) -> usize {
        self.at.len()
    }

    /// The size the picture at `art` is currently held at, if any.
    pub fn size_of(&self, art: &std::rc::Rc<crate::pixels::Bitmap>) -> Option<(usize, usize)> {
        self.at.get(&(std::rc::Rc::as_ptr(art) as usize)).map(|e| (e.w, e.h))
    }
}

/// Draw a picture into the buffer, scaled to the box the layout gave it.
///
/// Scaled at draw time rather than at load time because the box depends on the
/// window's width, which changes; the decoded picture does not, and is shared.
fn draw_picture(
    buf: &mut [u32],
    width: usize,
    height: usize,
    p: &Picture,
    scroll: f32,
    scaled: &mut Scaled,
) {
    let (w, h) = (p.w.round().max(1.0) as usize, p.h.round().max(1.0) as usize);
    let small = scaled.get(&p.art, w, h);
    let x0 = p.x.round() as i64;
    let y0 = (p.y - scroll).round() as i64;
    for row in 0..h {
        let py = y0 + row as i64;
        if py < 0 || py >= height as i64 {
            continue;
        }
        let base = py as usize * width;
        for col in 0..w {
            let px = x0 + col as i64;
            if px < 0 || px >= width as i64 {
                continue;
            }
            let p = small.px[row * w + col];
            let a = ((p >> 24) & 0xff) as u8;
            if a == 0 {
                continue;
            }
            let i = base + px as usize;
            buf[i] = blend(buf[i], p & 0x00ff_ffff, a);
        }
    }
}

/// Draw `laid` into `buf`, scrolled down by `scroll` pixels.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    laid: &Laid,
    text: &mut Text,
    scaled: &mut Scaled,
    buf: &mut [u32],
    width: usize,
    height: usize,
    scroll: f32,
    theme: &Theme,
) {
    buf.fill(theme.bg);

    // Shapes first: they are grounds and rules, and text sits on them.
    for s in &laid.shapes {
        let y0 = (s.y - scroll).round() as i64;
        let y1 = y0 + s.h.round().max(1.0) as i64;
        let x0 = s.x.round() as i64;
        let x1 = x0 + s.w.round().max(1.0) as i64;
        let colour = match s.ink {
            Ink::Code => theme.code_bg,
            other => theme.of(other),
        };
        for y in y0.max(0)..y1.min(height as i64) {
            let row = y as usize * width;
            for x in x0.max(0)..x1.min(width as i64) {
                buf[row + x as usize] = colour;
            }
        }
    }

    // Pictures next: they sit on the grounds and under nothing.
    for p in &laid.pictures {
        let top = p.y - scroll;
        // Skipped before the picture is scaled, which is the expensive half. A
        // document of screenshots scrolls as fast as one of prose.
        if top + p.h < 0.0 || top > height as f32 {
            continue;
        }
        draw_picture(buf, width, height, p, scroll, scaled);
    }

    // Before the text, so a caret sitting on a glyph does not cover it.
    if let Some(c) = laid.caret {
        fill(buf, width, height, c.x, c.top - scroll, 2.0, c.height, theme.caret);
    }

    for run in &laid.runs {
        let baseline = run.baseline - scroll;
        // Skipped before a single glyph is rasterised. A generous margin either
        // side covers ascenders and descenders without measuring them.
        if baseline < -run.px * 2.0 || baseline > height as f32 + run.px * 2.0 {
            continue;
        }
        let colour = theme.of(run.ink);
        let mut pen = run.x;
        for ch in run.text.chars() {
            if ch == ' ' {
                pen += text.advance(run.face, ch, run.px);
                continue;
            }
            let g = text.glyph(run.face, ch, run.px);
            let advance = g.advance;
            // `top` is the offset of the bitmap's TOP edge from the baseline,
            // positive downward -- so it is negative for anything that rises
            // above the baseline. Adding it is correct; subtracting would put
            // every glyph a line away from where it belongs.
            let gx = pen + g.left;
            let gy = baseline + g.top;
            let shear = if run.italic { SHEAR } else { 0.0 };
            blit(buf, width, height, g, gx, gy, colour, shear);
            pen += advance;
        }
    }
}

/// Fill a rectangle, clipped to the buffer.
#[allow(clippy::too_many_arguments)]
fn fill(buf: &mut [u32], w: usize, h: usize, x: f32, y: f32, rw: f32, rh: f32, colour: u32) {
    let x0 = x.round().max(0.0) as usize;
    let y0 = y.round().max(0.0) as usize;
    let x1 = ((x + rw).round().max(0.0) as usize).min(w);
    let y1 = ((y + rh).round().max(0.0) as usize).min(h);
    for row in y0..y1 {
        let base = row * w;
        for col in x0..x1 {
            buf[base + col] = colour;
        }
    }
}

/// Draw one run of text at a baseline. Returns where the pen ended.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    text: &mut Text,
    buf: &mut [u32],
    w: usize,
    h: usize,
    x: f32,
    baseline: f32,
    s: &str,
    face: Face,
    px: f32,
    colour: u32,
) -> f32 {
    let mut pen = x;
    for ch in s.chars() {
        if ch == ' ' {
            pen += text.advance(face, ch, px);
            continue;
        }
        let g = text.glyph(face, ch, px);
        let (gx, gy) = (pen + g.left, baseline + g.top);
        blit(buf, w, h, g, gx, gy, colour, 0.0);
        pen += g.advance;
    }
    pen
}

/// Draw the path prompt where the status line normally is.
///
/// It replaces the status line rather than sitting above it, because the status
/// line's job -- what file, whether it is modified -- is not what you need while
/// you are naming a file, and a second bar would move the document.
#[allow(clippy::too_many_arguments)]
pub fn draw_prompt(
    text: &mut Text,
    buf: &mut [u32],
    width: usize,
    height: usize,
    base: f32,
    theme: &Theme,
    label: &str,
    typed: &str,
    caret_chars: usize,
    note: Option<&str>,
    entries: &[crate::prompt::Entry],
    selected: usize,
    first: usize,
) {
    let bar = status_height(base);
    let row = base * 1.5;
    let shown = entries.len().saturating_sub(first).min(crate::prompt::VISIBLE);
    let list_h = shown as f32 * row;
    let list_top = height as f32 - bar - list_h;

    // The listing sits on its own ground so it reads as something over the
    // document rather than as part of it.
    if shown > 0 {
        fill(buf, width, height, 0.0, list_top, width as f32, list_h, theme.bar);
        let px = base * 0.82;
        for (i, e) in entries.iter().skip(first).take(shown).enumerate() {
            let y = list_top + i as f32 * row;
            let here = first + i == selected;
            if here {
                fill(buf, width, height, 0.0, y, width as f32, row, theme.select);
            }
            let ink = if here {
                theme.strong
            } else if e.is_dir {
                theme.link
            } else {
                theme.body
            };
            // A trailing separator on directories, which is how every listing
            // in the world says "you can go in here".
            let name =
                if e.is_dir && e.name != ".." { format!("{}/", e.name) } else { e.name.clone() };
            let face = if e.is_dir { Face::SansBold } else { Face::Sans };
            draw_text(text, buf, width, height, PAD * 1.5, y + row * 0.72, &name, face, px, ink);
        }
    }

    let top = height as f32 - bar;
    fill(buf, width, height, 0.0, top, width as f32, bar, theme.bar);
    let px = base * 0.78;
    let baseline = top + bar * 0.5 + px * 0.36;

    let mut x = draw_text(
        text, buf, width, height, PAD, baseline, label, Face::SansBold, px, theme.strong,
    );
    x += base * 0.5;

    // The caret is drawn from the width of what is BEFORE it rather than from a
    // character count times an average, because the face is proportional and the
    // second answer is wrong by a growing amount as the path gets longer.
    let before: String = typed.chars().take(caret_chars).collect();
    let caret_x = x + text.width(Face::Mono, &before, px);
    let end = draw_text(text, buf, width, height, x, baseline, typed, Face::Mono, px, theme.body);
    fill(buf, width, height, caret_x, baseline - px * 0.85, 2.0, px * 1.15, theme.caret);

    if let Some(n) = note {
        let nx = (end + base).max(width as f32 * 0.62);
        draw_text(text, buf, width, height, nx, baseline, n, Face::Sans, px, theme.dim);
    } else {
        let hint = "Tab completes  ·  Enter opens  ·  Esc cancels";
        let hw = text.width(Face::Sans, hint, px);
        let hx = (width as f32 - PAD - hw).max(end + base);
        draw_text(text, buf, width, height, hx, baseline, hint, Face::Sans, px, theme.dim);
    }
}

/// How tall the status line is at a given base size.
pub fn status_height(base: f32) -> f32 {
    base * 1.9
}

/// One line at the bottom: what file, whether it is modified, and what to press.
#[allow(clippy::too_many_arguments)]
pub fn draw_status(
    text: &mut Text,
    buf: &mut [u32],
    width: usize,
    height: usize,
    base: f32,
    theme: &Theme,
    name: &str,
    dirty: bool,
    note: Option<&str>,
    // Set while closing would discard unsaved work. The whole bar changes
    // colour, because a line of grey text at the bottom of a window is not a
    // warning -- it is a place warnings go to be missed.
    alarm: bool,
) {
    let bh = status_height(base);
    let top = height as f32 - bh;
    let ground = if alarm { theme.alarm_bg } else { theme.bar };
    fill(buf, width, height, 0.0, top, width as f32, bh, ground);
    if alarm {
        // A rule along the top edge, so the bar reads as a band rather than as
        // the window having changed colour for no reason.
        fill(buf, width, height, 0.0, top, width as f32, 2.0, theme.caret);
    }

    let px = base * 0.78;
    let baseline = top + (bh + text.ascent(Face::Sans, px)) / 2.0 - base * 0.22;
    let mut x = draw_text(
        text, buf, width, height, PAD, baseline, name, Face::SansBold, px, theme.strong,
    );
    let hint_ink = if alarm { theme.strong } else { theme.dim };
    if dirty {
        // A word, not a symbol. "modified" needs no key and cannot be mistaken
        // for decoration the way a lone dot can.
        // On the alarm ground the accent nearly disappears into it, so the word
        // that says WHY the bar is red has to stop being the same colour as the
        // bar. It is the one word that must stay legible there.
        let ink = if alarm { theme.strong } else { theme.caret };
        x = draw_text(text, buf, width, height, x + base * 0.5, baseline, "modified", Face::Sans, px, ink);
    }
    let hint = note.unwrap_or("Ctrl+S save  ·  Ctrl+Z undo  ·  Esc close");
    let hw = text.width(if alarm { Face::SansBold } else { Face::Sans }, hint, px);
    let hx = (width as f32 - PAD - hw).max(x + base);
    let hint_face = if alarm { Face::SansBold } else { Face::Sans };
    draw_text(text, buf, width, height, hx, baseline, hint, hint_face, px, hint_ink);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;
    use crate::layout::lay_out;

    fn frame(src: &str, w: usize, h: usize, scroll: f32) -> Vec<u32> {
        let mut text = Text::new();
        let laid = lay_out(&parse(src), w as f32, 16.0, &text, None);
        let mut buf = vec![0u32; w * h];
        draw(&laid, &mut text, &mut Scaled::default(), &mut buf, w, h, scroll, &Theme::DARK);
        buf
    }

    fn ink_pixels(buf: &[u32], theme: &Theme) -> usize {
        buf.iter().filter(|p| **p != theme.bg).count()
    }

    #[test]
    fn text_actually_reaches_the_buffer() {
        // The whole pipeline in one assertion: parse, lay out, rasterise, blit.
        let buf = frame("# Hello\n\nSome body text here.\n", 800, 600, 0.0);
        assert!(ink_pixels(&buf, &Theme::DARK) > 500, "almost nothing was drawn");
    }

    #[test]
    fn an_empty_document_is_the_background_and_nothing_else() {
        let buf = frame("", 400, 300, 0.0);
        assert_eq!(ink_pixels(&buf, &Theme::DARK), 0);
    }

    #[test]
    fn scrolling_past_the_end_leaves_an_empty_frame() {
        // Proves the viewport cull is real rather than the text happening to fit.
        let buf = frame("# Hello\n\nbody\n", 400, 300, 5000.0);
        assert_eq!(ink_pixels(&buf, &Theme::DARK), 0);
    }

    #[test]
    fn scrolling_moves_what_is_on_screen() {
        let src = "# Title\n\n".to_string() + &"paragraph text ".repeat(200);
        let a = frame(&src, 500, 200, 0.0);
        let b = frame(&src, 500, 200, 120.0);
        assert_ne!(a, b, "scrolling changed nothing");
    }

    #[test]
    fn nothing_is_written_outside_the_buffer() {
        // A glyph partly off the left edge must clip rather than wrap onto the
        // previous row, which is what an unchecked index does and it looks like
        // corruption rather than a bounds bug.
        let buf = frame(&"wide ".repeat(200), 120, 80, 0.0);
        assert_eq!(buf.len(), 120 * 80);
    }

    #[test]
    fn the_caret_is_drawn_when_there_is_one() {
        // `Laid::caret` was added and nothing painted it, so live editing had an
        // insertion point that existed only in the layout. This is the assertion
        // that would have caught it.
        let src = "# Title\n\nbody\n";
        let mut text = Text::new();
        let (w, h) = (600usize, 400usize);
        let d = parse(src);
        let laid = lay_out(
            &d, w as f32, 16.0, &text,
            Some(crate::layout::Editing { source: src, cursor: 3 }),
        );
        assert!(laid.caret.is_some(), "the layout produced no caret to draw");
        let mut buf = vec![0u32; w * h];
        draw(&laid, &mut text, &mut Scaled::default(), &mut buf, w, h, 0.0, &Theme::DARK);
        assert!(
            buf.contains(&Theme::DARK.caret),
            "the caret was never painted"
        );
    }

    #[test]
    fn no_caret_is_drawn_when_not_editing() {
        let buf = frame("# Title\n\nbody\n", 600, 400, 0.0);
        assert!(!buf.contains(&Theme::DARK.caret));
    }

    #[test]
    fn a_code_block_paints_its_own_ground() {
        let buf = frame("```\ncode here\n```\n", 500, 300, 0.0);
        assert!(
            buf.contains(&Theme::DARK.code_bg),
            "no code ground was drawn"
        );
    }

    #[test]
    fn the_status_line_says_when_a_file_is_modified() {
        // Compares the two renders rather than counting pixels of an exact
        // colour: text is anti-aliased, so whether ANY pixel reaches full
        // coverage depends on the face, and the first version of this test broke
        // when the font changed without the behaviour changing at all.
        let (w, h) = (700usize, 120usize);
        let render = |dirty: bool| {
            let mut text = Text::new();
            let mut buf = vec![0u32; w * h];
            draw_status(&mut text, &mut buf, w, h, 16.0, &Theme::DARK, "notes.md", dirty, None, false);
            buf
        };
        assert_ne!(render(true), render(false), "a modified file said nothing");
    }

    #[test]
    fn the_whole_bar_changes_colour_while_work_is_one_keypress_from_being_lost() {
        // A line of grey text at the bottom of a window is not a warning -- it is
        // where warnings go to be missed. The alarm has to be hard to look past.
        let (w, h) = (700usize, 120usize);
        let render = |alarm: bool| {
            let mut text = Text::new();
            let mut buf = vec![0u32; w * h];
            draw_status(&mut text, &mut buf, w, h, 16.0, &Theme::DARK, "n.md", true, None, alarm);
            buf
        };
        let calm = render(false);
        let loud = render(true);
        assert_ne!(calm, loud, "the alarm looked identical to the ordinary bar");
        let alarmed = loud.iter().filter(|p| **p == Theme::DARK.alarm_bg).count();
        assert!(alarmed > w * 10, "only {alarmed} pixels changed; that is not a warning");
        assert_eq!(
            calm.iter().filter(|p| **p == Theme::DARK.alarm_bg).count(),
            0,
            "the ordinary bar is already alarm-coloured, so the alarm says nothing"
        );
    }

    #[test]
    fn a_status_note_replaces_the_hint() {
        let (w, h) = (700usize, 120usize);
        let render = |note: Option<&str>| {
            let mut text = Text::new();
            let mut buf = vec![0u32; w * h];
            draw_status(&mut text, &mut buf, w, h, 16.0, &Theme::DARK, "n.md", false, note, false);
            buf
        };
        assert_ne!(render(None), render(Some("saved")), "the note changed nothing");
    }

    #[test]
    fn blending_is_bounded_at_both_ends() {
        assert_eq!(blend(0x00000000, 0x00ffffff, 0), 0x00000000);
        assert_eq!(blend(0x00000000, 0x00ffffff, 255), 0x00ffffff);
        let mid = blend(0x00000000, 0x00ffffff, 128);
        assert!(mid > 0x00000000 && mid < 0x00ffffff, "midpoint {mid:#010x}");
    }

    #[test]
    fn italic_text_is_drawn_with_the_italic_face() {
        // Was: "a sheared glyph reaches further right". A real italic face is
        // compiled in now, so the shear is zero and that test asserted the old
        // mechanism rather than the behaviour. What matters is that emphasis
        // reaches a different face at all -- otherwise it silently disappears.
        let src = "plain *emphasised* plain";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 19.0, &t, None);
        let em = l
            .runs
            .iter()
            .find(|r| r.text.trim() == "emphasised")
            .expect("the emphasised run");
        assert_eq!(em.face, crate::text::Face::SansItalic);
        let plain = l.runs.iter().find(|r| r.text.trim() == "plain").expect("plain");
        assert_eq!(plain.face, crate::text::Face::Sans);
    }

    // ---- pictures and colour glyphs ------------------------------------

    fn doc_with_picture(w: usize, h: usize, colour: u32) -> crate::doc::Doc {
        let mut doc = parse("![](p.png)\n");
        let art = std::rc::Rc::new(crate::pixels::Bitmap { w, h, px: vec![colour; w * h] });
        for b in &mut doc.blocks {
            if let crate::doc::Kind::Image { art: a, .. } = &mut b.kind {
                *a = crate::media::Art::Ready(art.clone());
            }
        }
        doc
    }

    #[test]
    fn a_picture_puts_its_own_colours_on_the_page() {
        let mut text = Text::new();
        let doc = doc_with_picture(200, 100, 0xff_ff8800);
        let laid = lay_out(&doc, 400.0, 16.0, &text, None);
        let (w, h) = (400usize, 400usize);
        let mut buf = vec![0u32; w * h];
        draw(&laid, &mut text, &mut Scaled::default(), &mut buf, w, h, 0.0, &Theme::DARK);
        let orange = buf.iter().filter(|p| **p == 0x00ff_8800).count();
        assert!(orange > 10_000, "the picture is not on the page: {orange} pixels");
    }

    #[test]
    fn a_transparent_picture_lets_the_page_through() {
        // Otherwise every icon with a transparent border arrives in a black box.
        let mut text = Text::new();
        let doc = doc_with_picture(100, 100, 0x00_ff0000);
        let laid = lay_out(&doc, 400.0, 16.0, &text, None);
        let (w, h) = (400usize, 400usize);
        let mut buf = vec![0u32; w * h];
        draw(&laid, &mut text, &mut Scaled::default(), &mut buf, w, h, 0.0, &Theme::DARK);
        assert!(buf.iter().all(|p| *p == Theme::DARK.bg), "a fully transparent picture painted something");
    }

    #[test]
    fn a_picture_is_scaled_once_and_reused_across_frames() {
        // The measurement that made this exist: 15.5ms a frame without it,
        // 0.5ms with. Scrolling and typing both redraw, so an uncached scale is
        // paid per keystroke.
        let mut text = Text::new();
        let doc = doc_with_picture(900, 1100, 0xff_ff8800);
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let (w, h) = (900usize, 600usize);
        let mut buf = vec![0u32; w * h];
        let mut scaled = Scaled::default();
        for _ in 0..30 {
            draw(&laid, &mut text, &mut scaled, &mut buf, w, h, 0.0, &Theme::DARK);
        }
        assert_eq!(scaled.held(), 1, "thirty frames scaled the picture more than once");
    }

    #[test]
    fn resizing_the_window_scales_the_picture_again() {
        // The cached copy is only right for the size it was made at. If a resize
        // reused it the picture would be drawn at the old size into the new box.
        let mut text = Text::new();
        let doc = doc_with_picture(400, 400, 0xff_ff8800);
        let (w, h) = (900usize, 600usize);
        let mut buf = vec![0u32; w * h];
        let mut scaled = Scaled::default();
        let mut sizes = Vec::new();
        for width in [300.0, 500.0] {
            let laid = lay_out(&doc, width, 19.0, &text, None);
            draw(&laid, &mut text, &mut scaled, &mut buf, w, h, 0.0, &Theme::DARK);
            sizes.push(laid.pictures[0].w.round() as usize);
        }
        assert_ne!(sizes[0], sizes[1], "the test did not actually change the size");
        let orange = buf.iter().filter(|p| **p == 0x00ff_8800).count();
        assert!(orange > 1000, "nothing was drawn at the new size");
    }

    #[test]
    fn dragging_a_window_edge_does_not_keep_every_size_it_passed_through() {
        // The leak this replaced: winit delivers a resize per frame, so one drag
        // asks for hundreds of widths. Keeping them all measured 217MB of
        // resident memory after a single drag, none of it reachable, and it
        // never came back down -- in a program whose whole claim is that closing
        // it costs you nothing.
        let mut text = Text::new();
        // A small picture and a short drag: the assertion is about how many
        // copies are kept, and a big one would only make the test slow.
        let doc = doc_with_picture(240, 180, 0xff_ff8800);
        let (w, h) = (700usize, 400usize);
        let mut buf = vec![0u32; w * h];
        let mut scaled = Scaled::default();
        for width in 400..520 {
            let laid = lay_out(&doc, width as f32, 19.0, &text, None);
            draw(&laid, &mut text, &mut scaled, &mut buf, w, h, 0.0, &Theme::DARK);
        }
        assert_eq!(scaled.held(), 1, "one picture on screen, one copy kept");
    }

    #[test]
    fn every_picture_in_a_document_is_kept_at_its_own_size() {
        // The bound is the DOCUMENT, not one entry total: several pictures on a
        // page must each keep their own scaled copy or they would evict each
        // other and rescale on every frame.
        let mut text = Text::new();
        let mut doc = parse("![](a.png)\n\n![](b.png)\n\n![](c.png)\n");
        let arts: Vec<_> = [(40, 30), (80, 60), (120, 90)]
            .iter()
            .map(|(w, h)| {
                std::rc::Rc::new(crate::pixels::Bitmap { w: *w, h: *h, px: vec![0xff_ff8800; w * h] })
            })
            .collect();
        let mut i = 0;
        for b in &mut doc.blocks {
            if let crate::doc::Kind::Image { art, .. } = &mut b.kind {
                *art = crate::media::Art::Ready(arts[i].clone());
                i += 1;
            }
        }
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let (w, h) = (900usize, 900usize);
        let mut buf = vec![0u32; w * h];
        let mut scaled = Scaled::default();
        draw(&laid, &mut text, &mut scaled, &mut buf, w, h, 0.0, &Theme::DARK);
        assert_eq!(scaled.held(), 3);
        for a in &arts {
            assert_eq!(scaled.size_of(a), Some((a.w, a.h)), "a picture was not kept at its own size");
        }
    }

    #[test]
    fn a_picture_scrolled_off_the_top_is_not_on_the_page() {
        let mut text = Text::new();
        let doc = doc_with_picture(200, 100, 0xff_ff8800);
        let laid = lay_out(&doc, 400.0, 16.0, &text, None);
        let (w, h) = (400usize, 400usize);
        let mut buf = vec![0u32; w * h];
        draw(&laid, &mut text, &mut Scaled::default(), &mut buf, w, h, 5_000.0, &Theme::DARK);
        assert!(!buf.contains(&0x00ff_8800));
    }

    #[test]
    fn an_emoji_is_drawn_in_its_own_colours_not_the_text_colour() {
        // The whole point. If it came out in `theme.body` the colour bitmap is
        // being treated as a coverage mask and the picture is lost.
        if crate::emoji::Emoji::open().is_none() {
            eprintln!("no colour emoji font on this machine; skipping");
            return;
        }
        let buf = frame("Ship it \u{1F680}\n", 500, 200, 0.0);
        let theme = Theme::DARK;
        let known = [theme.bg, theme.body, theme.strong, theme.dim, theme.link, theme.code];
        let coloured = buf.iter().filter(|p| {
            let (r, g, _b) = ((*p >> 16) & 0xff, (*p >> 8) & 0xff, *p & 0xff);
            // Something saturated: no greyscale, and not one of the theme's own.
            r.abs_diff(g) > 40 && !known.contains(p)
        });
        assert!(coloured.count() > 20, "the rocket came out in the text colour");
    }
}
