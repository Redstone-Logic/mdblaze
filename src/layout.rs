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
    // The measure is a number of CHARACTERS, converted here. A pixel constant
    // silently becomes the wrong measure the moment the face or size changes.
    let measure = text.measure_width(base).max(MEASURE_MIN);
    let column = (width - PAD * 2.0).min(measure);
    let left = ((width - column) / 2.0).max(PAD);
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
            // A table is not prose, so the MEASURE does not apply to it. The
            // measure exists so the eye can find the next line of a paragraph;
            // a table is read cell by cell and constraining it to the prose
            // column just wraps every heading onto two lines.
            let table_width = (width - PAD * 2.0 - indent).max(avail);
            let table_x = if table_width > avail { PAD + indent } else { x0 };
            y = lay_table(&mut out, text, editing, rows, reveal_rel, table_x, y, table_width, base);
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
                        source: block.source.start,
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
                            let w = text.width(face, word, px);
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
                            let w = text.width(face, trimmed, px);
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
            widths[n] = widths[n].max(w + cell_pad * 2.0);
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
}
