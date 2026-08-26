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

use crate::code::{self, Tok};
use crate::doc::{Doc, Kind, Span, Style};
use crate::media::Art;
use crate::text::{Face, Text, CODE_LEADING};

/// Longest a line of prose may get, in pixels.
///
/// Derived from a character count at layout time rather than being this
/// constant: see [`crate::text::MEASURE_CHARS`]. Kept as a floor so a very small
/// window still has a sane column.
pub const MEASURE_MIN: f32 = 320.0;

/// Space either side of the text column.
pub const PAD: f32 = 28.0;

/// Indentation per nesting level.
pub const INDENT: f32 = 22.0;

/// How many monospace columns a code block may use.
///
/// 79, which is PEP 8's limit and the width most code is written to -- it comes
/// from an 80-column terminal leaving room for the newline. Prose and code want
/// DIFFERENT measures and it is a mistake to give them one: prose is swept line
/// by line and wants about 66 characters, code is read structurally and wants to
/// show the line the author actually wrote. Holding code to the prose measure
/// clipped it at 66 columns.
pub const CODE_COLUMNS: f32 = 79.0;

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
    /// Inside a fenced block whose language is known. Anything else in a code
    /// block stays `Code`, so an unknown language looks exactly as it did before
    /// highlighting existed.
    Keyword,
    Str,
    Number,
    Comment,
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
    /// Byte offset in the source of this run's first character.
    ///
    /// What a click resolves through: find the run under the pointer, walk its
    /// characters to the one clicked, and the answer is a position in the FILE
    /// rather than on the screen.
    pub source: usize,
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

/// A picture, placed. The pixels are shared with the document rather than copied
/// into the layout, which is laid out again on every keystroke.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub art: std::rc::Rc<crate::pixels::Bitmap>,
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
    pub pictures: Vec<Picture>,
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
    } else if style.bold
        || matches!(kind, Kind::Heading(_))
        || matches!(kind, Kind::TableRow { header: true, .. })
    {
        Face::SansBold
    } else if style.italic {
        // A real face, so `Run::italic` no longer has to ask for a shear.
        Face::SansItalic
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
        // Rows of one table are not separate paragraphs.
        Kind::TableRow { .. } if matches!(prev, Some(Kind::TableRow { .. })) => 0.0,
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
    // ONE content column, and everything starts at its left edge.
    //
    // Prose, code, tables and diagrams all want different WIDTHS -- 66 characters
    // of prose, 79 columns of code, whatever a table's cells need. What they must
    // share is where they BEGIN. Sizing each one independently and centring it
    // gives a page whose left edge moves from block to block, which reads as a
    // fault however defensible each individual width is.
    //
    // So the column is as wide as the widest thing in it (code), the page centres
    // THAT, and every block is laid out from its left edge.
    let measure = text.measure_width(base).max(MEASURE_MIN);
    let code_measure = text.advance(Face::Mono, 'M', base * 0.92) * CODE_COLUMNS + base;
    let content = (width - PAD * 2.0).min(measure.max(code_measure));
    let left = ((width - content) / 2.0).max(PAD);
    // Prose still wraps at its own, narrower measure -- it just starts in the
    // same place as everything else.
    let column = content.min(measure);
    let mut y = PAD;
    let mut prev: Option<Kind> = None;

    // Which block, if any, shows its source instead of its rendering.
    let reveal = editing.and_then(|e| doc.block_at(e.cursor));
    // The end of the last block passed, so a cursor sitting in the blank line
    // between two blocks can still be given somewhere to be.
    let mut prev_source_end = 0usize;

    // Rows already laid out as part of a table, so the loop does not draw them
    // again one at a time.
    let mut done_to = 0usize;

    for (i, block) in doc.blocks.iter().enumerate() {
        if i < done_to {
            continue;
        }
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

        // A table is measured whole: a column is as wide as its widest cell in
        // ANY row, so the rows cannot be laid out one at a time as the flat list
        // would otherwise have them.
        if matches!(block.kind, Kind::TableRow { .. }) {
            let mut j = i;
            while j < doc.blocks.len() && matches!(doc.blocks[j].kind, Kind::TableRow { .. }) {
                j += 1;
            }
            let rows = &doc.blocks[i..j];
            let reveal_rel = reveal.and_then(|r| (r >= i && r < j).then_some(r - i));
            // A table is not prose, so the MEASURE does not apply -- it is read
            // cell by cell rather than swept line by line, and holding it to the
            // prose column wraps every heading onto two lines.
            //
            // But it is still part of the document, so it is CENTRED on the same
            // axis as the text rather than pinned left. Pinned, a narrow table
            // sits alone at the window's edge with the prose it belongs to
            // several inches away.
            // Sized to its cells -- stretching columns to fill puts a gulf
            // between a label and its value -- but starting at the shared left
            // edge like everything else.
            let room = (content - indent).max(avail);
            y = lay_table(&mut out, text, editing, rows, reveal_rel, x0, y, room, base);
            done_to = j;
            prev = Some(block.kind.clone());
            prev_source_end = doc.blocks[j - 1].source.end;
            continue;
        }

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
            Kind::Image { url, alt, art } => {
                y = lay_picture(&mut out, text, block, url, alt, art, x0, y, content - indent, base);
            }
            // A mermaid fence is a DIAGRAM, not code. Rendered to box-drawing
            // text, which this program already knows how to draw -- the SVG
            // renderers would need a rasteriser, and the obvious one drags in the
            // font scanning this program exists without.
            Kind::Code { lang } if lang.as_deref() == Some("mermaid") => {
                let face = Face::Mono;
                let lh = text.line_height_with(face, px, CODE_LEADING);
                let asc = text.ascent(face, px);
                let body: String = block.spans.iter().map(|s| s.text.as_str()).collect();
                // A diagram that will not parse falls back to its source. Showing
                // nothing, or an error where a diagram should be, loses the
                // content -- the source at least still says what was meant.
                let (drawn, ink) = match mermaid_text::render(&body) {
                    Ok(d) if !d.trim().is_empty() => (d, Ink::Code),
                    _ => (body.clone(), Ink::Dim),
                };
                let lines: Vec<&str> = drawn.trim_end().split('\n').collect();
                let pad = base * 0.5;
                // A diagram is not prose either. Its ground is sized to the
                // WIDEST LINE and centred on the text, so it neither overflows
                // its own background nor sits pinned at the window's edge.
                let ground_w = (content - indent).max(120.0);
                let gx = left + indent;
                out.shapes.push(Shape {
                    x: gx,
                    y,
                    w: ground_w,
                    h: lh * lines.len() as f32 + pad * 2.0,
                    ink: Ink::Code,
                });
                y += pad;
                for line in lines {
                    out.runs.push(Run {
                        x: gx + pad,
                        baseline: y + asc,
                        text: line.to_string(),
                        face,
                        px,
                        ink,
                        italic: false,
                        source: block.source.start,
                    });
                    y += lh;
                }
                y += pad;
            }
            Kind::Code { lang } => {
                // Never wrapped. Lines that overflow are clipped by the viewport
                // rather than folded, because folded code reads as different code.
                let face = Face::Mono;
                let lh = text.line_height_with(face, px, CODE_LEADING);
                let asc = text.ascent(face, px);
                let body: String = block.spans.iter().map(|s| s.text.as_str()).collect();
                let lines: Vec<&str> = body.split('\n').collect();
                let pad = base * 0.5;
                // Code gets its OWN measure -- CODE_COLUMNS of monospace -- and is
                // centred on the same axis as the prose. Held to the prose measure
                // it was clipped at 66 columns, well inside what code is written
                // to. Sized to the widest line when that is narrower, so a short
                // snippet does not sit in a wide empty box.
                // Every code block is the same width and starts in the same
                // place, so a page of them has one straight edge rather than a
                // ragged one. A short snippet in a full-width box is what every
                // renderer does and reads as deliberate; boxes of six different
                // widths do not.
                let ground_w = (content - indent).max(120.0);
                let cx = left + indent;
                out.shapes.push(Shape {
                    x: cx,
                    y,
                    w: ground_w,
                    h: lh * lines.len() as f32 + pad * 2.0,
                    ink: Ink::Code,
                });
                y += pad;

                // Highlighted per LINE rather than over the whole block, because
                // a line is the unit that gets a baseline. An unknown or absent
                // fence produces one plain run, exactly as before.
                let known = lang.as_deref().and_then(code::lang_for);
                for line in lines {
                    let mut pen = cx + pad;
                    let pieces: Vec<(usize, usize, Tok)> = match known {
                        Some(l) => code::highlight(l, line),
                        None => vec![(0, line.len(), Tok::Plain)],
                    };
                    for (a, b, tok) in pieces {
                        let piece = &line[a..b];
                        if piece.is_empty() {
                            continue;
                        }
                        out.runs.push(Run {
                            x: pen,
                            baseline: y + asc,
                            text: piece.to_string(),
                            face,
                            px,
                            ink: match tok {
                                Tok::Plain => Ink::Code,
                                Tok::Keyword => Ink::Keyword,
                                Tok::Str => Ink::Str,
                                Tok::Number => Ink::Number,
                                Tok::Comment => Ink::Comment,
                            },
                            italic: false,
                            source: block.source.start,
                        });
                        pen += text.width(face, piece, px);
                    }
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
                        source: block.source.start,
                    });
                }

                let mut cursor = text_x;
                let mut line_start = true;
                for span in &block.spans {
                    let Span { text: t, style, source } = span;
                    let face = face_of(kind, style);
                    let ink = ink_of(kind, style);
                    // How far into this span's text we have got, so each word can
                    // be given the source byte it starts at. The span's rendered
                    // text and its source are the same length for plain text and
                    // differ where markers were stripped, so this is a good
                    // approximation inside emphasis and exact outside it.
                    let mut used = 0usize;
                    for chunk in t.split('\n') {
                        if !line_start && chunk.is_empty() {
                            y += lh;
                            cursor = text_x;
                            used += 1;
                            continue;
                        }
                        for word in words(chunk) {
                            let at = source.start + used;
                            used += word.len();
                            let mut w = text.width(face, word, px);
                            if !line_start && cursor + w > text_x + text_avail {
                                y += lh;
                                cursor = text_x;
                                line_start = true;
                            }
                            let trimmed = if line_start { word.trim_start() } else { word };
                            if trimmed.is_empty() {
                                continue;
                            }
                            let at = at + (word.len() - trimmed.len());
                            // Measured again ONLY if trimming took something
                            // off. Every word was being measured twice --
                            // once to test the wrap and once to place it -- and
                            // measuring is most of what laying out costs. Off a
                            // line start the two are the same string.
                            if trimmed.len() != word.len() {
                                w = text.width(face, trimmed, px);
                            }
                            out.runs.push(Run {
                                x: cursor,
                                baseline: y + asc,
                                text: trimmed.to_string(),
                                face,
                                px,
                                ink,
                                italic: style.italic,
                                source: at.min(source.end),
                            });
                            cursor += w;
                            line_start = false;
                        }
                        used += 1; // the newline split consumed
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

impl Laid {
    /// Which source byte is under a point, in document coordinates.
    ///
    /// The inverse of laying out, and the reason every run records where it came
    /// from. Without that this could only answer in screen terms, and a click
    /// could not move a cursor that lives in the file.
    ///
    /// Picks the nearest LINE first and the nearest character within it second,
    /// so clicking in the margin to the right of a line puts the caret at that
    /// line's end -- which is what clicking past the end of a line means
    /// everywhere else -- rather than finding nothing.
    pub fn hit(&self, x: f32, y: f32, text: &Text) -> Option<usize> {
        if self.runs.is_empty() {
            return None;
        }
        // The closest baseline to the click. Runs on one visual line share it,
        // so this picks a line rather than a word.
        let mut best_line = f32::MAX;
        let mut best_delta = f32::MAX;
        for r in &self.runs {
            let d = (r.baseline - y).abs();
            if d < best_delta {
                best_delta = d;
                best_line = r.baseline;
            }
        }

        let mut on_line: Vec<&Run> = self
            .runs
            .iter()
            .filter(|r| (r.baseline - best_line).abs() < 0.5)
            .collect();
        on_line.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

        let first = on_line.first()?;
        if x <= first.x {
            return Some(first.source);
        }

        for r in &on_line {
            let w = text.width(r.face, &r.text, r.px);
            if x > r.x + w {
                continue;
            }
            // Inside this run: walk characters until the pointer is passed, and
            // land on whichever side of the glyph is nearer -- clicking the right
            // half of a character puts the caret after it, as it does everywhere.
            let mut pen = r.x;
            let mut at = r.source;
            for ch in r.text.chars() {
                let adv = text.advance(r.face, ch, r.px);
                if x < pen + adv / 2.0 {
                    return Some(at);
                }
                pen += adv;
                at += ch.len_utf8();
            }
            return Some(at);
        }

        // Past the end of the line: its end.
        let last = on_line.last()?;
        Some(last.source + last.text.len())
    }
}

/// Lay one picture out, and answer the y it ends at.
///
/// A picture is placed at the shared left edge like every other block, sized
/// down to fit the column and never up -- see [`crate::pixels::Bitmap::fit`].
///
/// When there is no picture the space is not left blank. A gap where a figure
/// should be tells the reader nothing; the alt text and the reason tell them
/// what was meant and why it is not there, which is the difference between a
/// document that is missing something and a program that is broken.
#[allow(clippy::too_many_arguments)]
fn lay_picture(
    out: &mut Laid,
    text: &Text,
    block: &crate::doc::Block,
    url: &str,
    alt: &str,
    art: &Art,
    x0: f32,
    y: f32,
    avail: f32,
    base: f32,
) -> f32 {
    match art {
        Art::Ready(bitmap) => {
            let (w, h) = bitmap.fit(avail.max(1.0), base * MAX_PICTURE_EMS);
            out.pictures.push(Picture { x: x0, y, w, h, art: bitmap.clone() });
            // An anchor with nothing in it, at the picture's middle.
            //
            // `hit` answers in RUNS, so without one of these a click anywhere on
            // a picture lands on the paragraph above or below it and there is no
            // way to reach the image's own source to correct a path. It draws
            // nothing: a run with no characters has no glyphs.
            out.runs.push(Run {
                x: x0,
                baseline: y + h / 2.0,
                text: String::new(),
                face: Face::Sans,
                px: base,
                ink: Ink::Dim,
                italic: false,
                source: block.source.start,
            });
            y + h
        }
        Art::Missing(why) => {
            let px = base * 0.92;
            let face = Face::SansItalic;
            let lh = text.line_height(face, px);
            let name = if alt.trim().is_empty() { url } else { alt };
            out.runs.push(Run {
                x: x0,
                baseline: y + text.ascent(face, px),
                text: format!("[{}] \u{2014} {}", name.trim(), why.reason()),
                face,
                px,
                ink: Ink::Dim,
                italic: true,
                source: block.source.start,
            });
            y + lh
        }
        // Laid out before `media` was given the chance to look. Not a state the
        // program reaches -- the binary attaches media to every document it
        // parses -- but a test that lays out a document without a filesystem
        // does, and it should see the alt text rather than nothing.
        Art::Unresolved => {
            let px = base * 0.92;
            let face = Face::SansItalic;
            let lh = text.line_height(face, px);
            let name = if alt.trim().is_empty() { url } else { alt };
            out.runs.push(Run {
                x: x0,
                baseline: y + text.ascent(face, px),
                text: format!("[{}]", name.trim()),
                face,
                px,
                ink: Ink::Dim,
                italic: true,
                source: block.source.start,
            });
            y + lh
        }
    }
}

/// The tallest a picture is drawn, in multiples of the type size.
///
/// A screenshot of a whole screen is taller than it is wide; sized only by the
/// column it would push everything after it a page and a half down, and the
/// reader would have to scroll past a picture to find out whether the document
/// continues. Twenty-six ems is about two thirds of a window.
const MAX_PICTURE_EMS: f32 = 26.0;

/// Lay a run of table rows out as a table.
///
/// Column widths come from the widest cell in any row, then are scaled down
/// together if the table is wider than the space -- so the columns stay in
/// proportion rather than the last one being squeezed to nothing.
#[allow(clippy::too_many_arguments)]
fn lay_table(
    out: &mut Laid,
    text: &Text,
    editing: Option<Editing>,
    rows: &[crate::doc::Block],
    reveal_rel: Option<usize>,
    x0: f32,
    mut y: f32,
    avail: f32,
    base: f32,
) -> f32 {
    let px = base * 0.94;
    let lh = text.line_height(Face::Sans, px);
    let asc = text.ascent(Face::Sans, px);
    let cell_pad = base * 0.55;

    // Cells, as slices into each row's flat span list.
    let cells_of = |b: &crate::doc::Block| -> Vec<(usize, usize)> {
        let Kind::TableRow { cells, .. } = &b.kind else { return Vec::new() };
        let mut out = Vec::new();
        for (n, start) in cells.iter().enumerate() {
            let end = cells.get(n + 1).copied().unwrap_or(b.spans.len());
            out.push((*start, end));
        }
        out
    };

    let columns = rows.iter().map(|r| cells_of(r).len()).max().unwrap_or(0);
    if columns == 0 {
        return y;
    }

    // Natural width of every column: the widest cell in it, plus padding.
    let mut widths = vec![0.0f32; columns];
    for row in rows {
        let header = matches!(row.kind, Kind::TableRow { header: true, .. });
        let face = if header { Face::SansBold } else { Face::Sans };
        for (n, (a, b)) in cells_of(row).into_iter().enumerate() {
            let w: f32 = row.spans[a..b]
                .iter()
                .map(|s| text.width(if s.style.code { Face::Mono } else { face }, &s.text, px))
                .sum();
            // Plus a pixel of slack. Without it the natural width equals the
            // content width EXACTLY, and accumulated float error in the placement
            // trips the wrap test -- so a heading that fits by construction wraps
            // onto two lines anyway.
            widths[n] = widths[n].max(w + cell_pad * 2.0 + 1.0);
        }
    }

    // Scaled together if too wide. A minimum stops a column vanishing entirely,
    // at the cost of the table overflowing when there are very many columns --
    // which is the honest failure, since a column of no width shows nothing.
    let total: f32 = widths.iter().sum();
    if total > avail {
        let min = base * 3.0;
        let scale = avail / total;
        for w in widths.iter_mut() {
            *w = (*w * scale).max(min);
        }
    }


    for (n, row) in rows.iter().enumerate() {
        if Some(n) == reveal_rel {
            let e = editing.expect("reveal implies editing");
            y = lay_revealed(out, text, e, row, x0, y, avail, base);
            continue;
        }
        let header = matches!(row.kind, Kind::TableRow { header: true, .. });
        let face = if header { Face::SansBold } else { Face::Sans };
        let ink = if header { Ink::Strong } else { Ink::Body };

        let mut x = x0;
        let mut tallest = lh;
        for (n_cell, (a, b)) in cells_of(row).into_iter().enumerate() {
            let w = widths.get(n_cell).copied().unwrap_or(base * 4.0);
            let mut pen = x + cell_pad;
            let mut line = 0.0f32;
            for span in &row.spans[a..b] {
                // The span's own emphasis still counts inside a cell: a bold word
                // in a body row was coming out plain because the face was chosen
                // from the header flag alone.
                let f = if span.style.code {
                    Face::Mono
                } else if span.style.bold {
                    Face::SansBold
                } else if span.style.italic {
                    Face::SansItalic
                } else {
                    face
                };
                // Wrapped inside the column rather than spilling into the next
                // one, which is what makes a table read as a grid at all.
                for word in words(&span.text) {
                    let ww = text.width(f, word, px);
                    if pen > x + cell_pad && pen + ww > x + w - cell_pad {
                        line += lh;
                        pen = x + cell_pad;
                    }
                    let trimmed = if pen == x + cell_pad { word.trim_start() } else { word };
                    if trimmed.is_empty() {
                        continue;
                    }
                    out.runs.push(Run {
                        x: pen,
                        baseline: y + line + asc,
                        text: trimmed.to_string(),
                        face: f,
                        px,
                        ink: if span.style.code { Ink::Code } else { ink },
                        italic: span.style.italic,
                        source: span.source.start,
                    });
                    pen += text.width(f, trimmed, px);
                }
            }
            tallest = tallest.max(line + lh);
            x += w;
        }

        y += tallest;
        // A rule under the header, and nothing between body rows: the alignment
        // of the columns is what separates them, and a line per row turns a small
        // table into a cage.
        if header {
            out.shapes.push(Shape {
                x: x0,
                y,
                w: (x - x0).min(avail),
                h: 1.0,
                ink: Ink::Dim,
            });
            y += base * 0.25;
        }
    }
    y
}

/// Lay one block out as its own markdown, and place the caret inside it.
///
/// The revealed block is drawn in mono on a tinted ground: it is source, and
/// making it look like source is the point -- the reader can see exactly which
/// characters they are changing, which is what a rendered-only editor cannot say.
#[allow(clippy::too_many_arguments)]
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
                // Through `boundary`, because the cursor is caller-supplied
                // and a slice inside a character aborts the process.
                let upto = &word[..crate::edit::boundary(word, e.cursor - start)];
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
                // The revealed block is literal source, so this is exact.
                source: start,
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
        let cap = Text::new().measure_width(16.0);
        assert!(extent(&huge) <= cap, "extent {} exceeds the measure {cap}", extent(&huge));
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
    fn a_laid_out_paragraph_holds_a_readable_number_of_characters() {
        // The property, measured on text rather than asserted about a constant.
        // Sized by the advance of a single glyph the column came out at 89
        // characters -- past the band where the eye reliably finds the next line.
        let prose = "the quick brown fox jumps over the lazy dog and keeps on running \
                     across the field toward the river where it stops to drink ";
        let src = prose.repeat(6);
        let t = Text::new();
        let l = lay_out(&parse(&src), 1600.0, crate::text::BODY_PX, &t, None);

        use std::collections::BTreeMap;
        let mut lines: BTreeMap<i64, usize> = BTreeMap::new();
        for r in &l.runs {
            *lines.entry((r.baseline * 10.0) as i64).or_default() += r.text.chars().count();
        }
        let mut lens: Vec<usize> = lines.values().copied().collect();
        assert!(lens.len() > 4, "expected several lines, got {}", lens.len());
        lens.sort_unstable();
        // The last line of a paragraph is short by definition, so the check is on
        // the full ones.
        let longest = *lens.last().expect("a line");
        assert!(
            (45..=80).contains(&longest),
            "longest line is {longest} characters, outside the readable band"
        );
    }

    #[test]
    fn a_very_wide_window_does_not_lengthen_the_line() {
        let src = "the quick brown fox jumps over the lazy dog ".repeat(20);
        let t = Text::new();
        let chars_at = |w: f32| {
            let l = lay_out(&parse(&src), w, crate::text::BODY_PX, &t, None);
            use std::collections::BTreeMap;
            let mut lines: BTreeMap<i64, usize> = BTreeMap::new();
            for r in &l.runs {
                *lines.entry((r.baseline * 10.0) as i64).or_default() += r.text.chars().count();
            }
            lines.values().copied().max().unwrap_or(0)
        };
        assert_eq!(chars_at(1000.0), chars_at(3000.0), "the line grew with the window");
    }

    // ---- code ----------------------------------------------------------------

    #[test]
    fn a_fenced_block_with_a_known_language_is_coloured() {
        let l = lay("```rust\nlet x = 42; // note\n```\n", 800.0);
        let inks: Vec<Ink> = l.runs.iter().map(|r| r.ink).collect();
        assert!(inks.contains(&Ink::Keyword), "no keyword: {inks:?}");
        assert!(inks.contains(&Ink::Number), "no number: {inks:?}");
        assert!(inks.contains(&Ink::Comment), "no comment: {inks:?}");
    }

    #[test]
    fn an_unknown_language_looks_exactly_as_it_did_before() {
        // Refusing to show a block, or colouring it wrongly by guessing, would
        // make an unfamiliar language worse than no highlighting at all.
        let l = lay("```brainfuck\n+++[->+<]\n```\n", 800.0);
        assert!(l.runs.iter().all(|r| r.ink == Ink::Code), "guessed at an unknown language");
    }

    #[test]
    fn a_fence_with_no_language_is_left_plain() {
        let l = lay("```\nlet x = 42;\n```\n", 800.0);
        assert!(l.runs.iter().all(|r| r.ink == Ink::Code));
    }

    #[test]
    fn highlighting_does_not_lose_or_reorder_a_single_character() {
        // The one thing a highlighter must never do is change what the code says.
        let src = "```rust\nfn main() { let s = \"hi\"; } // done\n```\n";
        let l = lay(src, 800.0);
        let rebuilt: String = l.runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(rebuilt, "fn main() { let s = \"hi\"; } // done");
    }

    #[test]
    fn coloured_pieces_are_laid_out_left_to_right_in_order() {
        let l = lay("```rust\nlet x = 1;\n```\n", 800.0);
        let xs: Vec<f32> = l.runs.iter().map(|r| r.x).collect();
        for w in xs.windows(2) {
            assert!(w[1] >= w[0], "pieces out of order: {xs:?}");
        }
    }

    #[test]
    fn a_mermaid_fence_becomes_a_diagram_not_its_source() {
        let src = "```mermaid\nflowchart TD\n    A[Start] --> B[End]\n```\n";
        let l = lay(src, 900.0);
        let all: String = l.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(all.contains("Start") && all.contains("End"), "labels missing: {all:?}");
        // Box drawing, which is how you can tell it was laid out rather than
        // echoed back.
        assert!(
            all.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c)),
            "no box-drawing characters, so nothing was drawn: {all:?}"
        );
        assert!(!all.contains("flowchart TD"), "the source was echoed instead");
    }

    #[test]
    fn a_broken_diagram_falls_back_to_its_source_rather_than_vanishing() {
        // Showing nothing, or an error where a diagram should be, loses what the
        // author wrote. The source at least still says what was meant.
        let src = "```mermaid\nthis is not a diagram at all @@@\n```\n";
        let l = lay(src, 900.0);
        let all: String = l.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(!all.trim().is_empty(), "the block disappeared");
    }

    #[test]
    fn a_mermaid_fence_is_not_run_through_the_syntax_highlighter() {
        let src = "```mermaid\nflowchart TD\n    A[if] --> B[let]\n```\n";
        let l = lay(src, 900.0);
        // `if` and `let` are keywords in several languages; in a diagram they are
        // node labels and must not be coloured as code.
        assert!(
            l.runs.iter().all(|r| r.ink != Ink::Keyword),
            "a diagram label was highlighted as a keyword"
        );
    }

    // ---- tables --------------------------------------------------------------

    // Deliberately distinct first words per cell: searching runs by text prefix
    // is how these tests find a cell, and "head one"/"head two" both start with
    // "head", which finds the wrong run and fails for the wrong reason.
    const TABLE: &str = "| alpha | bravo |\n|---|---|\n| a | b |\n| ccc | ddd |\n";

    /// Find a run by its EXACT text. Prefix matching finds "alpha" when asked
    /// for "a", which fails these tests for a reason that has nothing to do with
    /// tables.
    fn run_of<'a>(l: &'a Laid, want: &str) -> Option<&'a Run> {
        l.runs.iter().find(|r| r.text.trim() == want)
    }

    #[test]
    fn a_table_puts_its_columns_in_columns() {
        // The bug this fixes: every cell became one run of prose, so a table read
        // as "head onehead twoab" -- present, and unreadable.
        let l = lay(TABLE, 800.0);
        let c1 = run_of(&l, "alpha").expect("col 1").x;
        let c2 = run_of(&l, "bravo").expect("col 2 missing").x;
        assert!(c2 > c1, "the second column is not right of the first");
    }

    #[test]
    fn a_column_lines_up_down_the_table() {
        // What makes it a table rather than rows of text: the same column starts
        // at the same x in every row.
        let l = lay(TABLE, 800.0);
        let second_col_x: Vec<f32> =
            ["bravo", "b", "ddd"].iter().filter_map(|t| run_of(&l, t).map(|r| r.x)).collect();
        assert_eq!(second_col_x.len(), 3, "not all rows found");
        for w in second_col_x.windows(2) {
            assert!((w[0] - w[1]).abs() < 0.5, "column drifted: {second_col_x:?}");
        }
    }

    #[test]
    fn rows_go_down_the_page_in_order() {
        let l = lay(TABLE, 800.0);
        let y_of = |t: &str| run_of(&l, t).map(|r| r.baseline).expect(t);
        assert!(y_of("a") > y_of("alpha"), "body above header");
        assert!(y_of("ccc") > y_of("a"), "rows out of order");
    }

    #[test]
    fn the_header_is_ruled_off_and_the_body_is_not() {
        // One line under the header; a line per row turns a small table into a
        // cage, and the column alignment already separates them.
        let l = lay(TABLE, 800.0);
        let rules = l.shapes.iter().filter(|s| s.h <= 2.0 && s.ink == Ink::Dim).count();
        assert_eq!(rules, 1, "expected exactly one rule, got {rules}");
    }

    #[test]
    fn a_narrow_window_keeps_every_column_visible() {
        // Scaled together rather than squeezing the last column to nothing: a
        // column of no width shows nothing at all, which is worse than crowding.
        let l = lay(TABLE, 260.0);
        for t in ["alpha", "bravo", "ccc", "ddd"] {
            assert!(run_of(&l, t).is_some(), "{t:?} vanished at a narrow width");
        }
    }

    #[test]
    fn emphasis_inside_a_cell_survives() {
        // The face was chosen from the header flag alone, so a bold word in a
        // body row came out plain -- markup silently dropped rather than shown.
        let l = lay("| a | b |\n|---|---|\n| **loud** | quiet |\n", 800.0);
        let loud = run_of(&l, "loud").expect("the cell text");
        let quiet = run_of(&l, "quiet").expect("the plain cell");
        assert_eq!(loud.face, Face::SansBold, "bold in a cell was lost");
        assert_eq!(quiet.face, Face::Sans);
    }

    #[test]
    fn code_gets_more_columns_than_prose_gets_characters() {
        // Prose and code want different measures. Held to the prose measure, a
        // line of code was clipped at 66 columns -- well inside the 79 that code
        // is actually written to.
        let long = "x".repeat(79);
        let src = format!("```\n{long}\n```\n");
        let t = Text::new();
        let l = lay_out(&parse(&src), 1000.0, crate::text::BODY_PX, &t, None);
        let run = l.runs.first().expect("a code run");
        let w = t.width(run.face, &run.text, run.px);
        assert!(
            w > t.measure_width(crate::text::BODY_PX),
            "79 columns of code ({w:.0}px) fit inside the prose measure, so the \
             measure is not doing anything for code"
        );
        let ground = l.shapes.iter().find(|s| s.ink == Ink::Code).expect("a ground");
        assert!(
            run.x >= ground.x - 0.5 && run.x + w <= ground.x + ground.w + 0.5,
            "the code line runs outside its own ground"
        );
    }

    #[test]
    fn every_kind_of_block_starts_at_the_same_left_edge() {
        // The rule, and the thing that looked wrong when it was broken: prose,
        // code, tables and diagrams want different WIDTHS, but they must share
        // where they BEGIN. A left edge that moves from block to block reads as a
        // fault however defensible each individual width is.
        let src = "prose line\n\n\
                   ```\ncode line\n```\n\n\
                   | a | b |\n|---|---|\n| 1 | 2 |\n\n\
                   ```mermaid\nflowchart TD\n  A[x] --> B[y]\n```\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 1400.0, crate::text::BODY_PX, &t, None);

        let prose = l.runs.iter().find(|r| r.text.starts_with("prose")).expect("prose").x;
        let grounds: Vec<f32> = l.shapes.iter().filter(|s| s.ink == Ink::Code).map(|s| s.x).collect();
        assert!(grounds.len() >= 2, "expected a code block and a diagram");
        for g in &grounds {
            assert!((g - prose).abs() < 1.0, "a block starts at {g}, prose at {prose}");
        }
        let cell = l.runs.iter().find(|r| r.text.trim() == "a").expect("a table cell").x;
        // The first cell is inset by the cell padding, which is small and
        // deliberate; the table's own edge is the prose edge.
        assert!(
            cell - prose < crate::text::BODY_PX,
            "the table starts at {cell}, prose at {prose}"
        );
    }

    #[test]
    fn every_code_block_is_the_same_width() {
        // Boxes of six different widths read as ragged; one width reads as
        // deliberate, which is what every other renderer does.
        let l = lay("```\nx\n```\n\n```\na much much longer line of code here\n```\n", 1200.0);
        let ws: Vec<f32> = l.shapes.iter().filter(|s| s.ink == Ink::Code).map(|s| s.w).collect();
        assert_eq!(ws.len(), 2);
        assert!((ws[0] - ws[1]).abs() < 1.0, "code blocks differ in width: {ws:?}");
    }

    #[test]
    fn a_table_heading_that_fits_by_construction_does_not_wrap() {
        // The natural width equalled the content width exactly, so float error in
        // the placement wrapped a heading that fits. One pixel of slack.
        let l = lay("| stage | small file | 36 KB file |\n|---|---|---|\n| read | 1 | 2 |\n", 900.0);
        let baselines: std::collections::BTreeSet<i64> =
            l.runs.iter().map(|r| (r.baseline * 10.0) as i64).collect();
        assert_eq!(baselines.len(), 2, "a header cell wrapped: {} lines", baselines.len());
    }

    #[test]
    fn a_narrow_table_does_not_stretch_to_fill_the_window() {
        // Stretched columns put a gulf between a label and its value, which is
        // the one thing a table exists to keep close.
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let t = Text::new();
        let span = |w: f32| {
            let l = lay_out(&parse(src), w, crate::text::BODY_PX, &t, None);
            let xs: Vec<f32> = l.runs.iter().map(|r| r.x).collect();
            xs.iter().fold(0.0_f32, |m, x| m.max(*x)) - xs.iter().fold(f32::MAX, |m, x| m.min(*x))
        };
        assert!(
            (span(900.0) - span(1800.0)).abs() < 1.0,
            "the table stretched with the window: {} then {}",
            span(900.0),
            span(1800.0)
        );
    }

    #[test]
    fn a_diagram_fits_inside_its_own_background() {
        // It was drawn on a ground sized to the prose column and ran off the
        // right of it, which reads as a rendering fault rather than a wide
        // diagram.
        let src = "```mermaid\nflowchart LR\n    A[Double-click] --> B[Read it] --> C[Close] --> D[Nothing resident]\n```\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 1000.0, crate::text::BODY_PX, &t, None);
        let ground = l.shapes.iter().find(|s| s.ink == Ink::Code).expect("a ground");
        for r in l.runs.iter() {
            let right = r.x + t.width(r.face, &r.text, r.px);
            assert!(
                r.x >= ground.x - 0.5 && right <= ground.x + ground.w + 0.5,
                "a diagram line runs from {} to {right}, outside its ground {}..{}",
                r.x,
                ground.x,
                ground.x + ground.w
            );
        }
    }

    #[test]
    fn a_table_does_not_swallow_the_paragraph_after_it() {
        // The range bug: a still-open block absorbed the whole next paragraph
        // because a Start tag carries the range of its entire element. The
        // paragraph then got no block, and the revealed source showed both.
        let src = "before\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter\n";
        let d = parse(src);
        let last = d.blocks.last().expect("a block");
        assert_eq!(&src[last.source.clone()], "after\n");
        assert!(
            d.blocks.iter().any(|b| matches!(b.kind, Kind::TableRow { .. })),
            "the table did not become rows"
        );
    }

    #[test]
    fn an_empty_cell_still_holds_its_column_open() {
        let l = lay("| a |  | c |\n|---|---|---|\n| 1 | 2 | 3 |\n", 800.0);
        let x = |t: &str| run_of(&l, t).map(|r| r.x).expect(t);
        assert!(x("3") > x("2"), "an empty header cell collapsed its column");
    }

    // ---- clicking ----------------------------------------------------------

    #[test]
    fn a_click_on_a_word_lands_on_that_word_in_the_source() {
        let src = "# Title\n\nalpha bravo charlie\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 16.0, &t, None);
        let bravo = l.runs.iter().find(|r| r.text.starts_with("bravo")).expect("run");
        let hit = l.hit(bravo.x + 1.0, bravo.baseline, &t).expect("hit");
        let want = src.find("bravo").expect("in source");
        assert!(
            hit.abs_diff(want) <= 1,
            "clicked 'bravo' at {hit}, which is {:?} in the source, expected {want}",
            &src[hit.min(src.len())..]
        );
    }

    #[test]
    fn clicking_further_right_lands_further_into_the_text() {
        let src = "alpha bravo charlie delta\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 16.0, &t, None);
        let base = l.runs[0].baseline;
        let a = l.hit(l.runs[0].x + 2.0, base, &t).expect("a");
        let b = l.hit(l.runs[0].x + 160.0, base, &t).expect("b");
        assert!(b > a, "clicking right did not move further in: {a} then {b}");
    }

    #[test]
    fn clicking_left_of_a_line_lands_at_its_start() {
        let src = "# Title\n\nsome words here\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 16.0, &t, None);
        let run = l.runs.iter().find(|r| r.text.starts_with("some")).expect("run");
        let hit = l.hit(0.0, run.baseline, &t).expect("hit");
        assert_eq!(hit, run.source, "clicking the margin should mean the line start");
    }

    #[test]
    fn clicking_past_the_end_of_a_line_lands_at_its_end() {
        // What clicking in the empty space to the right of a line means
        // everywhere else, and the case a naive run-containment test misses.
        let src = "short line\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 16.0, &t, None);
        let base = l.runs[0].baseline;
        let hit = l.hit(5_000.0, base, &t).expect("hit");
        assert!(hit >= src.find("line").unwrap(), "landed at {hit}, expected the line end");
    }

    #[test]
    fn a_click_picks_the_nearest_line_not_the_first_one() {
        let src = "first line here\n\nsecond line here\n\nthird line here\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 16.0, &t, None);
        let third = l.runs.iter().find(|r| r.text.starts_with("third")).expect("run");
        let hit = l.hit(third.x + 1.0, third.baseline, &t).expect("hit");
        assert!(
            hit >= src.find("third").unwrap() - 2,
            "clicked the third line and landed at {hit}"
        );
    }

    #[test]
    fn clicking_inside_a_revealed_block_is_exact() {
        // The revealed block is literal source, so there is no approximation to
        // make: the byte under the pointer is the byte.
        let src = "# Heading\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 800.0, 16.0, &t, Some(Editing { source: src, cursor: 0 }));
        let run = l.runs.first().expect("a run");
        let hit = l.hit(run.x + 0.5, run.baseline, &t).expect("hit");
        assert_eq!(hit, 0, "the start of the revealed source is byte 0");
    }

    #[test]
    fn clicking_an_empty_document_finds_nothing_rather_than_panicking() {
        let t = Text::new();
        let l = lay_out(&parse(""), 800.0, 16.0, &t, None);
        assert_eq!(l.hit(10.0, 10.0, &t), None);
    }

    #[test]
    fn every_run_can_be_clicked_back_to_a_byte_inside_the_document() {
        // A guard against an offset that walks off the end: a click must never
        // produce a position the buffer cannot seek to.
        let src = "# Title\n\nsome **bold** and `code` and [a link](https://x.com)\n\n- item one\n- item two\n";
        let t = Text::new();
        let l = lay_out(&parse(src), 700.0, 16.0, &t, None);
        for r in &l.runs {
            let hit = l.hit(r.x + 1.0, r.baseline, &t).expect("hit");
            assert!(hit <= src.len(), "run {:?} resolved to {hit}, past the end", r.text);
        }
    }

    #[test]
    fn words_keeps_spaces_with_the_word_they_follow() {
        assert_eq!(words("a b  c"), vec!["a ", "b  ", "c"]);
        assert_eq!(words(""), Vec::<&str>::new());
    }
    // ---- pictures ------------------------------------------------------

    fn stub(w: usize, h: usize) -> std::rc::Rc<crate::pixels::Bitmap> {
        std::rc::Rc::new(crate::pixels::Bitmap { w, h, px: vec![0xff_ff8800; w * h] })
    }

    /// A document with one picture in it, already resolved -- so a layout test
    /// needs no filesystem and no decoder.
    fn with_picture(src: &str, art: Art) -> Doc {
        let mut doc = parse(src);
        for b in &mut doc.blocks {
            if let Kind::Image { art: a, .. } = &mut b.kind {
                *a = art.clone();
            }
        }
        doc
    }

    #[test]
    fn a_picture_is_sized_to_the_column_and_never_larger() {
        let text = Text::new();
        let doc = with_picture("![](big.png)\n", Art::Ready(stub(4000, 2000)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let p = laid.pictures.first().expect("a picture was laid out");
        assert!(p.w <= 900.0 - PAD * 2.0, "wider than the page: {}", p.w);
        assert!((p.w / p.h - 2.0).abs() < 0.01, "proportions lost: {}x{}", p.w, p.h);
    }

    #[test]
    fn a_small_picture_is_left_at_its_own_size() {
        // A 32-pixel icon blown up to the width of the column is a blurred
        // rectangle, and it is not what the author wrote.
        let text = Text::new();
        let doc = with_picture("![](icon.png)\n", Art::Ready(stub(32, 32)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let p = laid.pictures.first().expect("picture");
        assert_eq!((p.w, p.h), (32.0, 32.0));
    }

    #[test]
    fn a_very_tall_picture_is_capped_so_the_document_still_continues() {
        // Sized only by the column, a full-screen portrait screenshot pushes
        // everything after it a page and a half down and the reader has to
        // scroll past a picture to learn whether there is any more document.
        let text = Text::new();
        let doc = with_picture("![](tall.png)\n", Art::Ready(stub(400, 8000)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let p = laid.pictures.first().expect("picture");
        assert!(p.h <= 19.0 * 26.0 + 0.01, "not capped: {}", p.h);
    }

    #[test]
    fn a_picture_starts_at_the_same_left_edge_as_the_prose() {
        // One content column. A page whose left edge moves from block to block
        // reads as a fault however defensible each width is on its own.
        let text = Text::new();
        let doc = with_picture("Some prose.\n\n![](p.png)\n", Art::Ready(stub(200, 100)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let prose_x = laid.runs.iter().find(|r| r.text.contains("Some")).expect("prose").x;
        assert!((laid.pictures[0].x - prose_x).abs() < 0.01);
    }

    #[test]
    fn a_picture_takes_up_the_room_it_is_drawn_in() {
        // If the layout advanced by less than the picture's height, everything
        // after it would be drawn on top of it.
        let text = Text::new();
        let doc = with_picture("![](p.png)\n\nAfter.\n", Art::Ready(stub(200, 400)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let p = &laid.pictures[0];
        let after = laid.runs.iter().find(|r| r.text.contains("After")).expect("prose after");
        assert!(after.baseline > p.y + p.h, "the paragraph after landed on the picture");
    }

    #[test]
    fn a_missing_picture_says_what_was_meant_and_why_it_is_not_there() {
        // A gap where a figure should be tells the reader nothing.
        let text = Text::new();
        let doc = with_picture(
            "![the architecture](arch.png)\n",
            Art::Missing(crate::media::Missing::NotFound),
        );
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let line: String = laid.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(line.contains("the architecture"), "{line:?}");
        assert!(line.contains("no such file"), "{line:?}");
        assert!(laid.pictures.is_empty());
    }

    #[test]
    fn a_refused_remote_picture_says_it_was_refused_rather_than_broken() {
        let text = Text::new();
        let doc = with_picture("![](https://x/y.png)\n", Art::Missing(crate::media::Missing::Remote));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let line: String = laid.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(line.contains("network"), "{line:?}");
    }

    #[test]
    fn a_picture_with_no_alt_text_names_the_file_instead() {
        let text = Text::new();
        let doc = with_picture("![](diagrams/flow.png)\n", Art::Missing(crate::media::Missing::NotFound));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let line: String = laid.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(line.contains("diagrams/flow.png"), "{line:?}");
    }

    #[test]
    fn clicking_a_picture_reaches_its_source_so_a_wrong_path_can_be_corrected() {
        // Without an anchor run the click lands on the paragraph above or below
        // and there is no way to put the caret in the image's own markdown.
        let text = Text::new();
        let src = "Before.\n\n![alt](p.png)\n\nAfter.\n";
        let doc = with_picture(src, Art::Ready(stub(300, 200)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        let p = &laid.pictures[0];
        let at = laid.hit(p.x + p.w / 2.0, p.y + p.h / 2.0, &text).expect("a hit");
        assert!(src[at..].starts_with("!["), "landed at {at}: {:?}", &src[at..at + 6.min(src.len() - at)]);
    }

    #[test]
    fn an_anchor_draws_nothing() {
        // It exists to be hit, not to be seen. A run with characters in it would
        // put stray text where the picture is.
        let text = Text::new();
        let doc = with_picture("![alt](p.png)\n", Art::Ready(stub(300, 200)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, None);
        assert!(laid.runs.iter().all(|r| r.text.is_empty()));
    }

    #[test]
    fn the_caret_in_a_picture_shows_its_markdown() {
        // Live editing: the block under the caret reveals its source. An image
        // is where that matters most -- it is how a path gets corrected.
        let text = Text::new();
        let src = "![alt](p.png)\n";
        let doc = with_picture(src, Art::Ready(stub(300, 200)));
        let laid = lay_out(&doc, 900.0, 19.0, &text, Some(Editing { source: src, cursor: 3 }));
        let shown: String = laid.runs.iter().map(|r| r.text.as_str()).collect();
        assert!(shown.contains("![alt](p.png)"), "{shown:?}");
        assert!(laid.pictures.is_empty(), "the picture is still drawn under its own source");
    }

}
