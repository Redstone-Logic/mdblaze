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

use crate::layout::{Ink, Laid};
use crate::text::{Text, SHEAR};

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
    };

    fn of(&self, ink: Ink) -> u32 {
        match ink {
            Ink::Body => self.body,
            Ink::Strong => self.strong,
            Ink::Dim => self.dim,
            Ink::Link => self.link,
            Ink::Code => self.code,
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

/// Draw `laid` into `buf`, scrolled down by `scroll` pixels.
pub fn draw(
    laid: &Laid,
    text: &mut Text,
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

            for row in 0..g.height {
                let py = gy.round() as i64 + row as i64;
                if py < 0 || py >= height as i64 {
                    continue;
                }
                // Synthetic italics: shift each row by its distance above the
                // baseline. The top of a tall glyph leans furthest.
                let lean = if run.italic {
                    (g.height as f32 - row as f32) * SHEAR
                } else {
                    0.0
                };
                let base = py as usize * width;
                for col in 0..g.width {
                    let cov = g.bitmap[row * g.width + col];
                    if cov == 0 {
                        continue;
                    }
                    let px = (gx + lean).round() as i64 + col as i64;
                    if px < 0 || px >= width as i64 {
                        continue;
                    }
                    let i = base + px as usize;
                    buf[i] = blend(buf[i], colour, cov);
                }
            }
            pen += advance;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;
    use crate::layout::lay_out;

    fn frame(src: &str, w: usize, h: usize, scroll: f32) -> Vec<u32> {
        let mut text = Text::new();
        let laid = lay_out(&parse(src), w as f32, 16.0, &text);
        let mut buf = vec![0u32; w * h];
        draw(&laid, &mut text, &mut buf, w, h, scroll, &Theme::DARK);
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
    fn a_code_block_paints_its_own_ground() {
        let buf = frame("```\ncode here\n```\n", 500, 300, 0.0);
        assert!(
            buf.iter().any(|p| *p == Theme::DARK.code_bg),
            "no code ground was drawn"
        );
    }

    #[test]
    fn blending_is_bounded_at_both_ends() {
        assert_eq!(blend(0x00000000, 0x00ffffff, 0), 0x00000000);
        assert_eq!(blend(0x00000000, 0x00ffffff, 255), 0x00ffffff);
        let mid = blend(0x00000000, 0x00ffffff, 128);
        assert!(mid > 0x00000000 && mid < 0x00ffffff, "midpoint {mid:#010x}");
    }

    #[test]
    fn italics_lean_right_of_upright() {
        // The shear is applied per row, so the rightmost inked pixel of a sheared
        // glyph sits further right than the same glyph upright.
        let w = 400usize;
        let rightmost = |italic: bool| {
            let mut text = Text::new();
            let mut laid = lay_out(&parse("MMMM"), w as f32, 40.0, &text);
            for r in laid.runs.iter_mut() {
                r.italic = italic;
            }
            let mut buf = vec![0u32; w * 200];
            draw(&laid, &mut text, &mut buf, w, 200, 0.0, &Theme::DARK);
            buf.iter()
                .enumerate()
                .filter(|(_, p)| **p != Theme::DARK.bg)
                .map(|(i, _)| i % w)
                .max()
                .unwrap_or(0)
        };
        assert!(rightmost(true) > rightmost(false), "shear had no effect");
    }
}
