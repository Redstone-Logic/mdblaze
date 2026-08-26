//! Markdown to a flat list of laid-out-able blocks.
//!
//! This does NOT produce HTML. The org-server console does, because a browser is
//! what consumes it there; here the consumer is a text rasteriser, and going
//! through HTML would mean building a string only to parse it again.
//!
//! The consequence, stated plainly because it is a real cost: the product now
//! renders markdown two ways -- HTML for the console, this for the editor -- and
//! they can drift. That is accepted because this opens arbitrary files off a
//! disk rather than organisation content, so the two never render the same
//! document. If that ever stops being true, one of them has to go.
//!
//! # Source ranges, and why every block carries one
//!
//! Live editing renders the document and reveals the markdown only in the block
//! the caret is in. That needs a map from a cursor position -- which lives in the
//! SOURCE -- to the block it lands in, so `into_offset_iter` is used rather than
//! the plain parser and every block records the byte range it came from.
//!
//! Without it the two coordinate systems never meet and the caret can only be
//! placed in a separate source view, which is the thing live editing exists to
//! avoid.
//!
//! # Why a flat list
//!
//! A tree would be the obvious shape and it is the wrong one. Laying out text
//! means walking blocks in order, measuring each against a width, and stopping
//! when the viewport is full. A flat list with a depth number does that in one
//! pass and makes "which block is at pixel Y" arithmetic rather than a search.
//! Nesting only ever affects indentation, which is what `depth` carries.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// How a run of text is drawn. Not a general style system: these are the only
/// distinctions markdown makes that change the glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    /// Monospace, and drawn on a tinted ground.
    pub code: bool,
    /// Part of a link. Coloured, and the destination is on the block.
    pub link: bool,
}

impl Style {
    pub const PLAIN: Style = Style { bold: false, italic: false, code: false, link: false };
}

/// A run of characters sharing one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// The source bytes this text came from.
    ///
    /// What makes a click land in the right place. The rendered text is not the
    /// source -- `**bold**` renders as `bold` -- so mapping a position on screen
    /// back to a position in the file needs the parser's answer, not arithmetic
    /// on the rendered characters.
    pub source: std::ops::Range<usize>,
}

/// What kind of block this is. Decides size, weight and spacing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Heading(u8),
    Paragraph,
    /// A list item. The marker is drawn by the layout, not stored as text.
    Item { ordered: Option<u64> },
    /// A fenced or indented code block. Never wrapped: code that wraps is code
    /// that lies about its shape.
    Code { lang: Option<String> },
    Quote,
    Rule,
    /// A picture.
    ///
    /// A block rather than something inline, even when the source wrote it in
    /// the middle of a sentence -- a picture in a rendered document occupies a
    /// band of the page, and inlining one would mean a line whose height varies
    /// with what is on it. The rare inline image comes out on its own line,
    /// which is what every renderer does with it in practice.
    ///
    /// `art` is what the picture turned out to be, filled in by
    /// [`crate::media::Media::attach`]. Parsing does no IO: it runs on every
    /// keystroke and it is a pure function of the source.
    Image { url: String, alt: String, art: crate::media::Art },
    /// One row of a table.
    ///
    /// Rows are separate blocks rather than a table being one block with a
    /// nested shape, because everything else here is a flat list walked in
    /// order and a table is the only thing that would have needed a tree. The
    /// layout gathers consecutive rows back together to measure the columns,
    /// which it has to do anyway -- column widths are a property of the whole
    /// table, not of any one row.
    ///
    /// `cells` holds the index into `spans` where each cell starts, so the flat
    /// span list keeps working and no cell text is copied.
    TableRow { header: bool, cells: Vec<usize> },
}

/// One block, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    pub spans: Vec<Span>,
    /// Nesting level, for indentation only.
    pub depth: u8,
    /// The bytes of the source this block was produced from.
    ///
    /// What lets a cursor -- which is a position in the source -- be resolved to
    /// the block it is inside, so that block can show its markdown while the rest
    /// of the document stays rendered.
    pub source: std::ops::Range<usize>,
}

/// A parsed document: blocks in order, and the links found in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doc {
    pub blocks: Vec<Block>,
}

/// Schemes a link may carry into the rendered view.
///
/// The same rule the console's renderer applies, for the same reason: a markdown
/// file can arrive from anywhere -- an email attachment, a repository, a
/// colleague -- and a document viewer that will follow `javascript:` because the
/// document said so is a document viewer with a hole in it.
///
/// Relative and fragment links are kept: they are meaningful inside a file.
fn scheme_ok(url: &str) -> bool {
    let t = url.trim();
    if t.is_empty() {
        return false;
    }
    if t.starts_with('#') || t.starts_with('/') || t.starts_with("./") || t.starts_with("../") {
        return true;
    }
    match t.split_once(':') {
        None => true, // no scheme at all: a bare relative path
        Some((scheme, _)) => {
            let s = scheme.trim().to_ascii_lowercase();
            s == "http" || s == "https" || s == "mailto"
        }
    }
}

/// Parse `src` into blocks.
pub fn parse(src: &str) -> Doc {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);

    let mut doc = Doc::default();
    let mut style = Style::PLAIN;
    let mut depth: u8 = 0;
    // How many block quotes deep. A paragraph inside one is a QUOTE, not a
    // paragraph that happens to be indented -- without this `Kind::Quote` is
    // never constructed and a quote renders as ordinary prose with a margin.
    let mut quoting: u32 = 0;
    // Ordered-list counters, one per nesting level, so a nested list restarts
    // rather than continuing its parent's numbering.
    let mut counters: Vec<Option<u64>> = Vec::new();
    let mut current: Option<Block> = None;
    // Extended as the block's events go by: a block's range is the span from its
    // opening tag to its closing one, and the events between are inside it.
    let mut span_end: usize = 0;

    // Push a span onto the open block. Text outside any block (which the parser
    // does emit, e.g. between a list marker and its paragraph) opens a paragraph
    // rather than being dropped, because dropping it loses the document.
    // The picture being read, if the parser is inside one: its URL, the alt
    // text gathered so far, and where in the source it came from.
    let mut picture: Option<(String, String, std::ops::Range<usize>)> = None;
    // How many tables deep. A picture inside a table CELL stays inline as its
    // alt text; see the image arm below.
    let mut tabling: u32 = 0;
    // The emphasis to restore after an inline image's alt text.
    let mut alt_italic: Option<bool> = None;

    macro_rules! push_text {
        ($t:expr, $r:expr) => {{
            let t: String = $t;
            let r: std::ops::Range<usize> = $r;
            if let Some((_, alt, _)) = picture.as_mut() {
                // Text inside an image tag is its ALT text, which describes the
                // picture rather than being part of the prose around it.
                alt.push_str(&t);
            } else if t.is_empty() {
            } else if let Some(b) = current.as_mut() {
                // Merged with the previous run when the style matches AND the
                // bytes follow on, so a sentence the parser split into several
                // events is measured and drawn as one. Merging across a GAP would
                // make the span's range cover source it does not contain, and a
                // click inside it would land in the wrong place.
                match b.spans.last_mut() {
                    Some(last) if last.style == style && last.source.end == r.start => {
                        last.text.push_str(&t);
                        last.source.end = r.end;
                    }
                    _ => b.spans.push(Span { text: t, style, source: r }),
                }
            } else {
                current = Some(Block {
                    kind: Kind::Paragraph,
                    spans: vec![Span { text: t, style, source: r.clone() }],
                    depth,
                    source: r,
                });
            }
            // Grow the block to cover what was just put in it. Only text does
            // this, so a block can never reach past its own content.
            if let Some(b) = current.as_mut().filter(|_| picture.is_none()) {
                if let Some(last) = b.spans.last() {
                    b.source.end = b.source.end.max(last.source.end);
                }
            }
        }};
    }

    macro_rules! close {
        () => {
            if let Some(b) = current.take() {
                // A block with no text is not a blank line, it is nothing --
                // except a rule, which is all shape and no text.
                let keeps = b.kind == Kind::Rule
                    || matches!(b.kind, Kind::TableRow { .. })
                    || b.spans.iter().any(|s| !s.text.trim().is_empty());
                if keeps {
                    doc.blocks.push(b);
                }
            }
        };
    }

    // A block's range is NOT extended by every event that passes while it is open.
    // A `Start` tag carries the range of its whole element, so extending on one
    // made the open block swallow the entire next paragraph before it closed --
    // and the paragraph then got no block of its own. Block-level starts set the
    // range exactly; only blocks opened implicitly by text (a table cell, say)
    // grow, and only by the text pushed into them.
    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        span_end = span_end.max(range.end);
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                close!();
                let n = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                current = Some(Block { kind: Kind::Heading(n), spans: Vec::new(), depth, source: range.clone() });
            }
            Event::End(TagEnd::Heading(_)) => close!(),

            Event::Start(Tag::Paragraph) => {
                close!();
                let kind = if quoting > 0 { Kind::Quote } else { Kind::Paragraph };
                current = Some(Block { kind, spans: Vec::new(), depth, source: range.clone() });
            }
            Event::End(TagEnd::Paragraph) => close!(),

            Event::Start(Tag::List(first)) => {
                close!();
                counters.push(first);
                depth = depth.saturating_add(1);
            }
            Event::End(TagEnd::List(_)) => {
                close!();
                counters.pop();
                depth = depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                close!();
                let n = counters.last_mut().and_then(|c| {
                    c.as_mut().map(|v| {
                        let cur = *v;
                        *v += 1;
                        cur
                    })
                });
                current = Some(Block { kind: Kind::Item { ordered: n }, spans: Vec::new(), depth, source: range.clone() });
            }
            Event::End(TagEnd::Item) => close!(),

            Event::Start(Tag::CodeBlock(kind)) => {
                close!();
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.to_string()),
                    _ => None,
                };
                style.code = true;
                current = Some(Block { kind: Kind::Code { lang }, spans: Vec::new(), depth, source: range.clone() });
            }
            Event::End(TagEnd::CodeBlock) => {
                style.code = false;
                // Code keeps its blank lines and its trailing newline is noise.
                if let Some(b) = current.as_mut() {
                    if let Some(last) = b.spans.last_mut() {
                        while last.text.ends_with('\n') {
                            last.text.pop();
                        }
                    }
                }
                close!();
            }

            // ---- tables ----------------------------------------------------
            Event::Start(Tag::Table(_)) => {
                close!();
                tabling += 1;
            }
            Event::End(TagEnd::Table) => {
                close!();
                tabling = tabling.saturating_sub(1);
            }
            Event::Start(Tag::TableHead) => {
                close!();
                current = Some(Block {
                    kind: Kind::TableRow { header: true, cells: Vec::new() },
                    spans: Vec::new(),
                    depth,
                    source: range.clone(),
                });
            }
            Event::Start(Tag::TableRow) => {
                close!();
                current = Some(Block {
                    kind: Kind::TableRow { header: false, cells: Vec::new() },
                    spans: Vec::new(),
                    depth,
                    source: range.clone(),
                });
            }
            Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => close!(),
            Event::Start(Tag::TableCell) => {
                // Where this cell's spans begin. Recorded even for an empty cell,
                // so a blank column still occupies its place in the row.
                if let Some(b) = current.as_mut() {
                    let at = b.spans.len();
                    if let Kind::TableRow { cells, .. } = &mut b.kind {
                        cells.push(at);
                    }
                }
            }

            Event::Start(Tag::BlockQuote(_)) => {
                close!();
                quoting += 1;
                depth = depth.saturating_add(1);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                close!();
                quoting = quoting.saturating_sub(1);
                depth = depth.saturating_sub(1);
            }

            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,

            Event::Start(Tag::Link { dest_url, .. }) => {
                // A refused scheme still shows its text -- the words are part of
                // the document -- it simply is not a link.
                if scheme_ok(&dest_url) {
                    style.link = true;
                }
            }
            Event::End(TagEnd::Link) => style.link = false,

            // A picture ends the paragraph it was written in and starts again
            // after it. See `Kind::Image`.
            //
            // The URL is kept as written and resolved later, by `media`, which
            // is where the rule about what may be loaded lives. Nothing is
            // fetched: a viewer that reaches the network because a file it
            // opened said to is a viewer that leaks which files you open.
            Event::Start(Tag::Image { dest_url, .. }) => {
                // Not inside a table. A picture in a CELL would need that row to
                // be as tall as the picture, and the table layout sizes rows
                // from their type -- it is measured as a grid of text, which is
                // what makes the columns line up. Ending the row to put a
                // picture between two of them is worse than not showing it: it
                // takes the table apart.
                //
                // So in a table an image is its alt text, in italic, in the
                // cell. Badge tables read as a list of what the badges say,
                // which is what a badge is for.
                if tabling > 0 {
                    alt_italic = Some(style.italic);
                    style.italic = true;
                } else {
                    close!();
                    picture = Some((dest_url.to_string(), String::new(), range.clone()));
                }
            }
            Event::End(TagEnd::Image) => {
                if let Some(was) = alt_italic.take() {
                    style.italic = was;
                } else if let Some((url, alt, at)) = picture.take() {
                    doc.blocks.push(Block {
                        kind: Kind::Image { url, alt, art: crate::media::Art::Unresolved },
                        spans: Vec::new(),
                        depth,
                        source: at,
                    });
                }
            }

            Event::Text(t) => push_text!(t.to_string(), range.clone()),
            Event::Code(t) => {
                let was = style.code;
                style.code = true;
                push_text!(t.to_string(), range.clone());
                style.code = was;
            }
            // Raw HTML is TEXT, never markup. Nothing here can execute it, but
            // showing `<b>` as bold would mean this viewer disagrees with the
            // console about what the same file says.
            Event::Html(t) | Event::InlineHtml(t) => push_text!(t.to_string(), range.clone()),

            Event::SoftBreak => push_text!(" ".to_string(), range.clone()),
            Event::HardBreak => push_text!("\n".to_string(), range.clone()),
            Event::Rule => {
                close!();
                doc.blocks.push(Block { kind: Kind::Rule, spans: Vec::new(), depth, source: range.clone() });
            }
            Event::TaskListMarker(done) => {
                push_text!(if done { "[x] ".to_string() } else { "[ ] ".to_string() }, range.clone())
            }
            _ => {}
        }
    }
    close!();
    let _ = span_end;
    doc
}

impl Doc {
    /// Which block contains `byte`, if any.
    ///
    /// Ranges do not tile the document -- blank lines between blocks belong to
    /// none of them -- so a cursor sitting in the gap answers `None`, and the
    /// caller renders everything and puts the caret nowhere. That is the honest
    /// answer: there is no block there to reveal.
    pub fn block_at(&self, byte: usize) -> Option<usize> {
        self.blocks.iter().position(|b| b.source.contains(&byte))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(d: &Doc) -> Vec<&Kind> {
        d.blocks.iter().map(|b| &b.kind).collect()
    }
    fn text(b: &Block) -> String {
        b.spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn the_shapes_a_document_is_actually_made_of() {
        let d = parse("# Title\n\nA para.\n\n- one\n- two\n\n> quoted\n\n---\n\n```rs\nfn x() {}\n```\n");
        assert_eq!(
            kinds(&d),
            vec![
                &Kind::Heading(1),
                &Kind::Paragraph,
                &Kind::Item { ordered: None },
                &Kind::Item { ordered: None },
                // A quote, not a paragraph. This line asserted the bug until
                // `Kind::Quote` started actually being constructed.
                &Kind::Quote,
                &Kind::Rule,
                &Kind::Code { lang: Some("rs".into()) },
            ]
        );
    }

    #[test]
    fn runs_sharing_a_style_are_merged_into_one_span() {
        // The parser splits a sentence at entity and soft-break boundaries. Left
        // as separate spans they would be measured and drawn separately, which
        // costs a shaping pass per fragment for no difference on screen.
        let d = parse("plain text with **bold** and more plain");
        let b = &d.blocks[0];
        assert_eq!(b.spans.len(), 3, "plain | bold | plain: {:?}", b.spans);
        assert!(b.spans[1].style.bold);
        assert_eq!(text(b), "plain text with bold and more plain");
    }

    #[test]
    fn a_nested_list_restarts_its_numbering() {
        // One shared counter would number the inner list 3, 4 -- continuing its
        // parent -- which is wrong and looks like a rendering bug rather than a
        // counting one.
        let d = parse("1. one\n2. two\n   1. inner\n   2. also inner\n");
        let nums: Vec<Option<u64>> = d
            .blocks
            .iter()
            .filter_map(|b| match b.kind {
                Kind::Item { ordered } => Some(ordered),
                _ => None,
            })
            .collect();
        assert_eq!(nums, vec![Some(1), Some(2), Some(1), Some(2)]);
    }

    #[test]
    fn depth_tracks_nesting_so_indentation_can_be_arithmetic() {
        let d = parse("- outer\n  - inner\n");
        let depths: Vec<u8> = d
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, Kind::Item { .. }))
            .map(|b| b.depth)
            .collect();
        assert_eq!(depths, vec![1, 2]);
    }

    #[test]
    fn a_link_that_runs_keeps_its_words_and_loses_its_linkness() {
        let d = parse("[click me](javascript:alert(1)) and [ok](https://example.com)");
        let b = &d.blocks[0];
        assert!(text(b).contains("click me"), "the words stay: {:?}", b.spans);
        let linked: Vec<&str> = b
            .spans
            .iter()
            .filter(|s| s.style.link)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(linked, vec!["ok"], "only the safe one is a link");
    }

    #[test]
    fn a_quote_is_a_quote_and_not_an_indented_paragraph() {
        // `Kind::Quote` existed and was never constructed, so quotes rendered as
        // ordinary prose with a margin -- no bar, no dimming. The kind is what
        // the layout keys the bar off, so this is the whole feature.
        let d = parse("before\n\n> quoted words\n\nafter\n");
        assert_eq!(
            kinds(&d),
            vec![&Kind::Paragraph, &Kind::Quote, &Kind::Paragraph]
        );
    }

    #[test]
    fn a_paragraph_after_a_quote_is_not_still_quoted() {
        let d = parse("> inside\n\noutside\n");
        let last = d.blocks.last().expect("a block");
        assert_eq!(last.kind, Kind::Paragraph);
        assert_eq!(text(last), "outside");
    }

    #[test]
    fn a_block_knows_which_bytes_it_came_from() {
        // The map that makes live editing possible: a cursor is a position in
        // the source, and this is what turns it into "which block am I in".
        let src = "# Title\n\nA paragraph.\n\n- an item\n";
        let d = parse(src);
        for b in &d.blocks {
            let slice = &src[b.source.clone()];
            assert!(!slice.trim().is_empty(), "{:?} maps to nothing", b.kind);
        }
        assert!(src[d.blocks[0].source.clone()].contains("Title"));
        assert!(src[d.blocks[1].source.clone()].contains("paragraph"));
        assert!(src[d.blocks[2].source.clone()].contains("an item"));
    }

    #[test]
    fn a_cursor_resolves_to_the_block_it_is_inside() {
        let src = "# Title\n\nA paragraph.\n";
        let d = parse(src);
        let at = |needle: &str| src.find(needle).expect("present");
        assert_eq!(d.block_at(at("Title")), Some(0));
        assert_eq!(d.block_at(at("paragraph")), Some(1));
    }

    #[test]
    fn a_cursor_in_the_gap_between_blocks_belongs_to_neither() {
        // Blank lines are not part of any block, so the honest answer is None --
        // there is no markdown there to reveal.
        let src = "# Title\n\n\n\nA paragraph.\n";
        let d = parse(src);
        let gap = src.find("\n\n\n").expect("gap") + 2;
        assert_eq!(d.block_at(gap), None);
    }

    #[test]
    fn ranges_are_in_document_order_and_do_not_overlap() {
        // Overlapping ranges would make `block_at` answer whichever came first
        // rather than the right one, and the caret would land in the wrong place.
        let d = parse("# A\n\nb\n\n- c\n- d\n\n```\ne\n```\n");
        let mut last = 0;
        for b in &d.blocks {
            assert!(b.source.start >= last, "out of order or overlapping: {:?}", b.kind);
            last = b.source.start;
        }
    }

    #[test]
    fn raw_html_is_text_not_markup() {
        let d = parse("<b>not bold</b> and <script>x</script>");
        let b = &d.blocks[0];
        assert!(text(b).contains("<b>"), "shown literally: {:?}", text(b));
        assert!(!b.spans.iter().any(|s| s.style.bold));
    }

    #[test]
    fn code_keeps_its_line_breaks() {
        // Code that wraps is code that lies about its shape, so the newlines have
        // to survive parsing to be honoured at layout.
        let d = parse("```\nline one\nline two\n```\n");
        assert_eq!(text(&d.blocks[0]), "line one\nline two");
    }

    #[test]
    fn an_empty_document_is_empty_rather_than_one_blank_block() {
        assert!(parse("").blocks.is_empty());
        assert!(parse("\n\n   \n\n").blocks.is_empty());
    }

    fn image(b: &Block) -> (&str, &str) {
        match &b.kind {
            Kind::Image { url, alt, .. } => (url.as_str(), alt.as_str()),
            other => panic!("not an image: {other:?}"),
        }
    }

    #[test]
    fn an_image_becomes_its_own_block_carrying_the_url_and_the_alt_text() {
        let d = parse("![a screenshot](shot.png)\n");
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(image(&d.blocks[0]), ("shot.png", "a screenshot"));
    }

    #[test]
    fn parsing_an_image_touches_no_filesystem() {
        // Parsing runs on every keystroke and is a pure function of the source.
        // The picture is looked for later, by `media`.
        let d = parse("![](/definitely/not/here.png)\n");
        assert!(matches!(d.blocks[0].kind, Kind::Image { ref art, .. } if *art == crate::media::Art::Unresolved));
    }

    #[test]
    fn the_alt_text_is_not_left_in_the_prose() {
        // It describes the picture. Rendered as ordinary text it would read as
        // a stray sentence with no punctuation in the middle of the document.
        let d = parse("Before.\n\n![a cat on a bench](cat.png)\n\nAfter.\n");
        let prose: String = d
            .blocks
            .iter()
            .filter(|b| !matches!(b.kind, Kind::Image { .. }))
            .map(text)
            .collect();
        assert!(!prose.contains("cat on a bench"), "alt text leaked into the prose: {prose:?}");
        assert!(prose.contains("Before.") && prose.contains("After."));
    }

    #[test]
    fn an_image_in_the_middle_of_a_paragraph_takes_its_own_line() {
        // A picture occupies a band of the page. Inlining one would mean a line
        // whose height depends on what is in it -- see `Kind::Image`.
        let d = parse("one ![pic](a.png) two\n");
        assert_eq!(d.blocks.len(), 3, "{:?}", kinds(&d));
        assert_eq!(d.blocks[0].kind, Kind::Paragraph);
        assert_eq!(d.blocks[2].kind, Kind::Paragraph);
        assert_eq!(text(&d.blocks[0]).trim(), "one");
        assert_eq!(image(&d.blocks[1]), ("a.png", "pic"));
        assert_eq!(text(&d.blocks[2]).trim(), "two");
    }

    #[test]
    fn an_image_wrapped_in_a_link_is_still_the_image() {
        // The commonest badge in a README. The link is dropped -- nothing here
        // follows one -- and the picture is what the reader came for.
        let d = parse("[![build](badge.png)](https://example.com/ci)\n");
        assert_eq!(image(&d.blocks[0]), ("badge.png", "build"));
    }

    #[test]
    fn several_images_in_a_row_are_several_blocks() {
        let d = parse("![one](1.png)\n![two](2.png)\n![three](3.png)\n");
        let urls: Vec<&str> = d.blocks.iter().map(|b| image(b).0).collect();
        assert_eq!(urls, vec!["1.png", "2.png", "3.png"]);
    }

    #[test]
    fn an_image_in_a_table_cell_stays_in_the_cell() {
        // Badge tables are the commonest place an image appears in a README, and
        // ending the row to make room for a picture takes the table apart --
        // every cell becomes a paragraph of its own.
        let d = parse(
            "| Build | Icon |\n| --- | --- |\n| passing | ![the badge](b.png) |\n",
        );
        assert!(
            d.blocks.iter().all(|b| matches!(b.kind, Kind::TableRow { .. })),
            "the table came apart: {:?}",
            kinds(&d)
        );
        let row = d.blocks.last().expect("a body row");
        assert!(text(row).contains("the badge"), "the alt text is not in the cell: {:?}", text(row));
        assert!(row.spans.iter().any(|s| s.style.italic), "not marked as standing in for a picture");
    }

    #[test]
    fn emphasis_around_an_image_in_a_cell_is_put_back_afterwards() {
        // The alt text borrows italic to mark itself. If it did not restore
        // what was there, everything after an image in a table would be italic.
        let d = parse("| a |\n| --- |\n| ![x](p.png) plain |\n");
        let row = d.blocks.last().expect("row");
        let plain = row.spans.iter().find(|s| s.text.contains("plain")).expect("the text after");
        assert!(!plain.style.italic);
    }

    #[test]
    fn an_image_records_where_in_the_source_it_came_from() {
        // What lets the caret be put inside it, so a wrong path can be corrected
        // in place rather than in another program.
        let src = "text\n\n![alt](p.png)\n";
        let d = parse(src);
        let img = d.blocks.iter().find(|b| matches!(b.kind, Kind::Image { .. })).expect("image");
        assert_eq!(&src[img.source.clone()].trim(), &"![alt](p.png)");
    }
}
