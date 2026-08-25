//! Blocks to positioned runs.
//!
//! One ordered walk over [`crate::doc::Doc`], producing text runs with baselines
//! and the few shapes markdown needs. Nothing here draws; nothing here knows what
//! colour anything is. Layout answers *where*, the renderer answers *what it
//! looks like*, and keeping them apart is what lets the layout be tested without
//! a window.
//!
//! # The measure
//!
//! Prose is capped at [`MEASURE`] regardless of how wide the window is. A line of
//! text much beyond that is measurably harder to read -- the eye loses the return
//! sweep -- so widening the window past it should give margins, not longer lines.
//! Code and rules take the full width, because a code block that wraps is lying
//! about its shape.
//!
//! This is the same rule the console applies at `46rem`; it is repeated here
//! rather than shared because the two have no code in common, and a constant is
//! a cheaper thing to keep in step than a dependency.
//!
//! # Live editing
//!
//! When [`Editing`] is supplied, the block the cursor is inside is laid out as
//! its own MARKDOWN and every other block stays rendered. So a heading looks like
//! a heading until the caret enters it, at which point the `##` appears and can
//! be edited, and leaving it puts the heading back.
//!
//! That is the whole trick, and it is only affordable because parsing and laying
//! out this document takes about four milliseconds: the entire thing is redone on
//! every keystroke rather than patched incrementally. Incremental re-layout is
//! where editors of this kind become complicated and wrong, and it buys nothing
//! at this speed.
//!
//! The caret comes back in [`Laid::caret`] because only the layout knows where a
//! byte offset ended up on screen -- it is the thing that put it there.

use crate::doc::{Doc, Kind, Span, Style};
use crate::text::{Face, Text, CODE_LEADING};

/// Longest a line of prose may get, in pixels at the base size.
pub const MEASURE: f32 = 736.0;

/// Space either side of the text column.
pub const PAD: f32 = 28.0;

/// Indentation per nesting level.
pub const INDENT: f32 = 22.0;

/// What a run is for. The renderer turns these into colours; layout does not
/// know or care what they look like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    Body,
    /// Headings above the body, and list markers below it.
    Strong,
    Dim,
    Link,
    Code,
}

/// A run of text on one line, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub x: f32,
    /// The baseline, not the top: glyphs sit on it.
    pub baseline: f32,
    pub text: String,
    pub face: Face,
    pub px: f32,
    pub ink: Ink,
    /// Sheared at draw time, since no oblique face is compiled in.
    pub italic: bool,
}

/// A filled or stroked rectangle: a rule, a code ground, a quote bar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub ink: Ink,
}

/// What the caller needs to know to draw a caret: where, and how tall.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caret {
    pub x: f32,
    pub top: f32,
    pub height: f32,
}

/// A laid-out document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Laid {
    pub runs: Vec<Run>,
    pub shapes: Vec<Shape>,
    /// Total height, so the caller knows how far it can scroll.
    pub height: f32,
    /// Where the insertion point landed, when editing.
    pub caret: Option<Caret>,
}

/// The state of an edit in progress.
#[derive(Debug, Clone, Copy)]
pub struct Editing<'a> {
    /// The document's source, which the revealed block is sliced out of.
    pub source: &'a str,
    /// The cursor, as a byte offset into `source`.
    pub cursor: usize,
}

/// Point size for each block kind at a base size.
fn size_of(kind: &Kind, base: f32) -> f32 {
    match kind {
        Kind::Heading(1) => base * 1.9,
        Kind::Heading(2) => base * 1.45,
        Kind::Heading(3) => base * 1.2,
        Kind::Heading(_) => base * 1.05,
        Kind::Code { .. } => base * 0.92,
        _ => base,
    }
}

/// Which compiled-in face a span wants.
fn face_of(kind: &Kind, style: &Style) -> Face {
    if style.code || matches!(kind, Kind::Code { .. }) {
        Face::Mono
    } else if style.bold || matches!(kind, Kind::Heading(_)) {
        Face::SansBold
    } else {
        Face::Sans
    }
}

fn ink_of(kind: &Kind, style: &Style) -> Ink {
    if style.link {
        Ink::Link
    } else if style.code || matches!(kind, Kind::Code { .. }) {
        Ink::Code
    } else if matches!(kind, Kind::Heading(_)) {
        Ink::Strong
    } else if matches!(kind, Kind::Quote) {
        Ink::Dim
    } else {
        Ink::Body
    }
}

/// Space above a block, given what came before it.
fn gap_before(kind: &Kind, prev: Option<&Kind>, base: f32) -> f32 {
    if prev.is_none() {
        return 0.0;
    }
    match kind {
        Kind::Heading(1) => base * 1.5,
        Kind::Heading(_) => base * 1.15,
        // Consecutive list items are one list, not several: a paragraph gap
        // between them makes a tight list look like a loose one.
        Kind::Item { .. } if matches!(prev, Some(Kind::Item { .. })) => base * 0.28,
        Kind::Rule => base * 1.2,
        _ => base * 0.85,
    }
}

/// Split into pieces that may end a line, keeping the spaces attached to the
/// word they follow so a break does not swallow them.
fn words(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            // Take the run of spaces with the preceding word.
            let mut j = i;
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            out.push(&s[start..j]);
            start = j;
            i = j;
        } else {
            i += 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Lay `doc` out for a window `width` pixels across.
///
/// Pass `editing` to reveal the markdown of the block the cursor is in and get
/// the caret's position back.
pub fn lay_out(
    doc: &Doc,
    width: f32,
    base: f32,
    text: &Text,
    editing: Option<Editing>,
) -> Laid {
    let mut out = Laid::default();
    // Centred when the window is wider than the measure, so growing the window
    // gives margins rather than longer lines.
    let column = (width - PAD * 2.0).min(MEASURE);
    let left = ((width - column) / 2.0).max(PAD);
    let mut y = PAD;
    let mut prev: Option<Kind> = None;

    // Which block, if any, shows its source instead of its rendering.
    let reveal = editing.and_then(|e| doc.block_at(e.cursor));
    // The end of the last block passed, so a cursor sitting in the blank line
    // between two blocks can still be given somewhere to be.
    let mut prev_source_end = 0usize;

    for (i, block) in doc.blocks.iter().enumerate() {
        y += gap_before(&block.kind, prev.as_ref(), base);

        // A cursor in the gap before this block: no markdown to reveal, but it
        // still needs a caret, or typing into a blank line looks like nothing is
        // happening.
        if let Some(e) = editing {
            if out.caret.is_none()
                && reveal.is_none()
                && e.cursor >= prev_source_end
                && e.cursor < block.source.start
            {
                out.caret = Some(Caret { x: left, top: y, height: base * 1.3 });
            }
        }

        let px = size_of(&block.kind, base);
        let indent = INDENT * f32::from(block.depth);
        let x0 = left + indent;
        let avail = (column - indent).max(80.0);

        if Some(i) == reveal {
            let e = editing.expect("reveal implies editing");
            y = lay_revealed(&mut out, text, e, block, x0, y, avail, base);
            prev = Some(block.kind.clone());
            prev_source_end = block.source.end;
            continue;
        }

        match &block.kind {
            Kind::Rule => {
                out.shapes.push(Shape { x: x0, y, w: avail, h: 1.0, ink: Ink::Dim });
                y += base * 0.6;
            }
            Kind::Code { .. } => {
                // Never wrapped. Lines that overflow are clipped by the viewport
                // rather than folded, because folded code reads as different code.
                let face = Face::Mono;
                let lh = text.line_height_with(face, px, CODE_LEADING);
                let asc = text.ascent(face, px);
                let body: String = block.spans.iter().map(|s| s.text.as_str()).collect();
                let lines: Vec<&str> = body.split('\n').collect();
                let pad = base * 0.5;
                out.shapes.push(Shape {
                    x: x0,
                    y,
                    w: avail,
                    h: lh * lines.len() as f32 + pad * 2.0,
                    ink: Ink::Code,
                });
                y += pad;
                for line in lines {
                    out.runs.push(Run {
                        x: x0 + pad,
                        baseline: y + asc,
                        text: line.to_string(),
                        face,
                        px,
                        ink: Ink::Code,
                        italic: false,
                    });
                    y += lh;
                }
                y += pad;
            }
            kind => {
                let quote_at = matches!(kind, Kind::Quote).then_some(y);

                let marker = match kind {
                    Kind::Item { ordered: Some(n) } => Some(format!("{n}.")),
                    Kind::Item { ordered: None } => Some("\u{2022}".to_string()),
                    _ => None,
                };

                let lh = text.line_height(Face::Sans, px);
                let asc = text.ascent(Face::Sans, px);
                let text_x = if marker.is_some() { x0 + INDENT } else { x0 };
                let text_avail = (avail - (text_x - x0)).max(60.0);

                if let Some(m) = marker {
                    out.runs.push(Run {
                        x: x0,
                        baseline: y + asc,
                        text: m,
                        face: Face::Sans,
                        px,
                        ink: Ink::Dim,
                        italic: false,
                    });
                }

                let mut cursor = text_x;
                let mut line_start = true;
                for Span { text: t, style } in &block.spans {
                    let face = face_of(kind, style);
                    let ink = ink_of(kind, style);
                    for chunk in t.split('\n') {
                        if !line_start && chunk.is_empty() {
                            y += lh;
                            cursor = text_x;
                            continue;
                        }
                        for word in words(chunk) {
                            let w = text.width(face, word, px);
                            if !line_start && cursor + w > text_x + text_avail {
                                y += lh;
                                cursor = text_x;
                                line_start = true;
                            }
                            let word = if line_start { word.trim_start() } else { word };
                            if word.is_empty() {
                                continue;
                            }
                            let w = text.width(face, word, px);
                            out.runs.push(Run {
                                x: cursor,
                                baseline: y + asc,
                                text: word.to_string(),
                                face,
                                px,
                                ink,
                                italic: style.italic,
                            });
                            cursor += w;
                            line_start = false;
                        }
                    }
                }
                y += lh;

                if let Some(top) = quote_at {
                    out.shapes.push(Shape {
                        x: x0 - INDENT * 0.5,
                        y: top,
                        w: 2.0,
                        h: (y - top).max(lh),
                        ink: Ink::Dim,
                    });
                }
            }
        }
        prev = Some(block.kind.clone());
        prev_source_end = block.source.end;
    }

    // A cursor past the last block -- at the end of the document, or in trailing
    // blank lines -- still needs somewhere to be.
    if let Some(e) = editing {
        if out.caret.is_none() && e.cursor >= prev_source_end {
            out.caret = Some(Caret { x: left, top: y + base * 0.4, height: base * 1.3 });
        }
    }

    out.height = y + PAD;
    out
}

/// Lay one block out as its own markdown, and place the caret inside it.
///
/// The revealed block is drawn in mono on a tinted ground: it is source, and
/// making it look like source is the point -- the reader can see exactly which
/// characters they are changing, which is what a rendered-only editor cannot say.
fn lay_revealed(
    out: &mut Laid,
    text: &Text,
    e: Editing,
    block: &crate::doc::Block,
    x0: f32,
    mut y: f32,
    avail: f32,
    base: f32,
) -> f32 {
    let face = Face::Mono;
    let px = base * 0.95;
    let lh = text.line_height_with(face, px, CODE_LEADING);
    let asc = text.ascent(face, px);
    let pad = base * 0.4;
    let src = &e.source[block.source.clone()];

    let ground_top = y;
    y += pad;

    // Byte offset of the start of the current source line, so the caret can be
    // matched against the cursor's absolute offset in the document.
    let mut at = block.source.start;
    for line in src.split('\n') {
        let mut cursor_x = x0 + pad;
        let mut consumed = 0usize; // bytes of this line already placed

        // Wrapped by words like prose, because a long paragraph's source is one
        // very long line and clipping it would hide what is being edited.
        for word in words(line) {
            let w = text.width(face, word, px);
            if consumed > 0 && cursor_x + w > x0 + avail - pad {
                y += lh;
                cursor_x = x0 + pad;
            }
            // The caret, if it falls inside this word.
            let start = at + consumed;
            if e.cursor >= start && e.cursor <= start + word.len() && out.caret.is_none() {
                let upto = &word[..(e.cursor - start).min(word.len())];
                out.caret = Some(Caret {
                    x: cursor_x + text.width(face, upto, px),
                    top: y,
                    height: lh,
                });
            }
            out.runs.push(Run {
                x: cursor_x,
                baseline: y + asc,
                text: word.to_string(),
                face,
                px,
                ink: Ink::Code,
                italic: false,
            });
            cursor_x += w;
            consumed += word.len();
        }
        // An empty source line still occupies one, and the caret can sit on it.
        if line.is_empty() && e.cursor == at && out.caret.is_none() {
            out.caret = Some(Caret { x: x0 + pad, top: y, height: lh });
        }
        at += line.len() + 1; // the newline
        y += lh;
    }

    y += pad;
    // Behind the text, so it is inserted before the runs just pushed.
    out.shapes.insert(
        0,
        Shape { x: x0, y: ground_top, w: avail, h: y - ground_top, ink: Ink::Code },
    );
    y
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;

    fn lay(src: &str, width: f32) -> Laid {
        lay_out(&parse(src), width, 16.0, &Text::new(), None)
    }

    #[test]
    fn prose_stops_widening_at_the_measure() {
        // The whole point of the cap. Without it a maximised window gives a line
        // nobody can comfortably read, and "wider" becomes a downgrade.
        let narrow = lay("word ".repeat(400).trim(), 900.0);
        let huge = lay("word ".repeat(400).trim(), 3000.0);
        // The EXTENT, not the absolute x: the column centres, so a wider window
        // shifts every run right without making any line longer. Comparing x
        // directly measures the margin and calls it a regression.
        let extent = |l: &Laid| {
            let lo = l.runs.iter().map(|r| r.x).fold(f32::MAX, f32::min);
            let hi = l.runs.iter().map(|r| r.x).fold(0.0_f32, f32::max);
            hi - lo
        };
        assert!(
            (extent(&huge) - extent(&narrow)).abs() < 1.0,
            "column extent changed with window width: {} vs {}",
            extent(&narrow),
            extent(&huge)
        );
        assert!(extent(&huge) <= MEASURE, "extent {} exceeds MEASURE", extent(&huge));
    }

    #[test]
    fn a_wider_window_gives_margins_not_longer_lines() {
        // The other half of the same rule: the column centres instead of hugging
        // the left edge.
        let l = lay("hello world", 2000.0);
        assert!(l.runs[0].x > PAD * 2.0, "column did not centre: x={}", l.runs[0].x);
    }

    #[test]
    fn long_prose_wraps_onto_several_baselines() {
        let l = lay(&"alpha bravo charlie delta echo foxtrot ".repeat(20), 700.0);
        let lines: std::collections::BTreeSet<i64> =
            l.runs.iter().map(|r| (r.baseline * 10.0) as i64).collect();
        assert!(lines.len() > 5, "expected many lines, got {}", lines.len());
    }

    #[test]
    fn code_does_not_wrap_however_long_its_lines_are() {
        // Folded code reads as different code, so a long line is clipped by the
        // viewport rather than broken.
        let long = "x".repeat(400);
        let l = lay(&format!("```\n{long}\n```\n"), 600.0);
        let code: Vec<&Run> = l.runs.iter().filter(|r| r.ink == Ink::Code).collect();
        assert_eq!(code.len(), 1, "one line in, one run out: {}", code.len());
        assert_eq!(code[0].text.len(), 400);
    }

    #[test]
    fn code_keeps_one_run_per_source_line() {
        let l = lay("```\nalpha\nbravo\ncharlie\n```\n", 600.0);
        let code: Vec<&Run> = l.runs.iter().filter(|r| r.ink == Ink::Code).collect();
        assert_eq!(code.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn a_heading_is_bigger_than_the_prose_under_it() {
        let l = lay("# Title\n\nbody text\n", 800.0);
        let h = l.runs.iter().find(|r| r.text == "Title").expect("heading");
        let b = l.runs.iter().find(|r| r.text.starts_with("body")).expect("body");
        assert!(h.px > b.px);
        assert!(h.baseline < b.baseline, "the heading is above its body");
    }

    #[test]
    fn list_markers_hang_left_of_their_text() {
        // So the text edges line up down the list instead of stepping in and out
        // as the numbers get wider.
        let l = lay("1. one\n2. two\n", 800.0);
        let marker = l.runs.iter().find(|r| r.text == "1.").expect("marker");
        let word = l.runs.iter().find(|r| r.text == "one").expect("text");
        assert!(marker.x < word.x, "marker should hang in the margin");
    }

    #[test]
    fn nesting_indents() {
        let l = lay("- outer\n  - inner\n", 800.0);
        let outer = l.runs.iter().find(|r| r.text == "outer").expect("outer");
        let inner = l.runs.iter().find(|r| r.text == "inner").expect("inner");
        assert!(inner.x > outer.x);
    }

    #[test]
    fn a_rule_is_a_shape_with_no_text() {
        let l = lay("above\n\n---\n\nbelow\n", 800.0);
        assert!(l.shapes.iter().any(|s| s.h <= 2.0 && s.w > 100.0), "no rule shape: {:?}", l.shapes);
    }

    #[test]
    fn height_covers_the_last_line() {
        let l = lay("# Title\n\nsome body\n", 800.0);
        let lowest = l.runs.iter().map(|r| r.baseline).fold(0.0_f32, f32::max);
        assert!(l.height > lowest, "height {} must clear the last baseline {}", l.height, lowest);
    }

    #[test]
    fn an_empty_document_lays_out_to_nothing() {
        let l = lay("", 800.0);
        assert!(l.runs.is_empty() && l.shapes.is_empty());
    }

    // ---- live editing ----------------------------------------------------

    fn editing(src: &str, cursor: usize, width: f32) -> Laid {
        let d = parse(src);
        lay_out(&d, width, 16.0, &Text::new(), Some(Editing { source: src, cursor }))
    }

    #[test]
    fn the_block_under_the_caret_shows_its_markdown() {
        // The whole feature: a heading looks like a heading until the caret is
        // in it, and then the hashes appear so they can be edited.
        let src = "# Title\n\nA paragraph.\n";
        let inside = editing(src, src.find("Title").unwrap(), 800.0);
        assert!(
            inside.runs.iter().any(|r| r.text.contains('#')),
            "the markdown was not revealed: {:?}",
            inside.runs.iter().map(|r| &r.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn every_other_block_stays_rendered() {
        // If revealing one block revealed them all this would just be a source
        // view with extra steps.
        let src = "# Title\n\nA paragraph.\n";
        let l = editing(src, src.find("Title").unwrap(), 800.0);
        assert!(
            l.runs.iter().any(|r| r.text.starts_with("paragraph") || r.text.starts_with("A")),
            "the other block lost its rendering"
        );
        let hashes = l.runs.iter().filter(|r| r.text.contains('#')).count();
        assert_eq!(hashes, 1, "more than one block revealed its source");
    }

    #[test]
    fn moving_the_caret_moves_which_block_is_revealed() {
        let src = "# Title\n\nA paragraph.\n";
        let in_heading = editing(src, src.find("Title").unwrap(), 800.0);
        let in_para = editing(src, src.find("paragraph").unwrap(), 800.0);
        assert!(in_heading.runs.iter().any(|r| r.text.contains('#')));
        assert!(
            !in_para.runs.iter().any(|r| r.text.contains('#')),
            "the heading stayed revealed after the caret left it"
        );
    }

    #[test]
    fn a_caret_is_produced_and_sits_inside_the_revealed_block() {
        let src = "# Title\n\nbody\n";
        let l = editing(src, src.find("itle").unwrap(), 800.0);
        let c = l.caret.expect("no caret");
        assert!(c.height > 0.0);
        // Inside the ground drawn for the revealed block.
        let ground = l.shapes.iter().find(|s| s.ink == Ink::Code).expect("no ground");
        assert!(
            c.top >= ground.y - 1.0 && c.top <= ground.y + ground.h,
            "caret at {} is outside the revealed block {}..{}",
            c.top,
            ground.y,
            ground.y + ground.h
        );
    }

    #[test]
    fn the_caret_advances_along_the_line_as_the_cursor_does() {
        let src = "# Heading here\n";
        let a = editing(src, 2, 800.0).caret.expect("caret");
        let b = editing(src, 9, 800.0).caret.expect("caret");
        assert!(b.x > a.x, "caret did not move: {} then {}", a.x, b.x);
    }

    #[test]
    fn a_caret_in_the_gap_between_blocks_still_gets_a_position() {
        // Typing into a blank line must not look like nothing is happening.
        let src = "# Title\n\n\n\nbody\n";
        let gap = src.find("\n\n\n").expect("gap") + 2;
        let l = editing(src, gap, 800.0);
        assert!(l.caret.is_some(), "no caret in the gap");
        assert!(
            !l.runs.iter().any(|r| r.text.contains('#')),
            "a gap revealed a block it is not inside"
        );
    }

    #[test]
    fn a_caret_at_the_very_end_of_the_document_gets_a_position() {
        let src = "# Title\n\nbody\n";
        let l = editing(src, src.len(), 800.0);
        let c = l.caret.expect("no caret at end of document");
        assert!(c.top > 0.0);
    }

    #[test]
    fn laying_out_without_editing_produces_no_caret() {
        let l = lay("# Title\n\nbody\n", 800.0);
        assert!(l.caret.is_none());
    }

    #[test]
    fn revealing_a_block_does_not_lose_the_rest_of_the_document() {
        // A regression guard: an early version returned after the revealed block
        // and silently truncated everything below it.
        let src = "# One\n\ntwo\n\nthree\n\nfour\n";
        let l = editing(src, src.find("two").unwrap(), 800.0);
        let all: String = l.runs.iter().map(|r| r.text.as_str()).collect();
        for word in ["One", "three", "four"] {
            assert!(all.contains(word), "{word} went missing; got {all:?}");
        }
    }

    #[test]
    fn words_keeps_spaces_with_the_word_they_follow() {
        assert_eq!(words("a b  c"), vec!["a ", "b  ", "c"]);
        assert_eq!(words(""), Vec::<&str>::new());
    }
}
