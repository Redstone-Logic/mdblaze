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
    macro_rules! push_text {
        ($t:expr) => {{
            let t: String = $t;
            if t.is_empty() {
            } else if let Some(b) = current.as_mut() {
                // Merged with the previous run when the style matches, so a
                // sentence broken by the parser into several events is measured
                // and drawn as one.
                match b.spans.last_mut() {
                    Some(last) if last.style == style => last.text.push_str(&t),
                    _ => b.spans.push(Span { text: t, style }),
                }
            } else {
                current = Some(Block {
                    kind: Kind::Paragraph,
                    spans: vec![Span { text: t, style }],
                    depth,
                    source: 0..0,
                });
            }
        }};
    }

    macro_rules! close {
        () => {
            if let Some(b) = current.take() {
                // A block with no text is not a blank line, it is nothing --
                // except a rule, which is all shape and no text.
                if b.kind == Kind::Rule || b.spans.iter().any(|s| !s.text.trim().is_empty()) {
                    doc.blocks.push(b);
                }
            }
        };
    }

    for (ev, range) in Parser::new_ext(src, opts).into_offset_iter() {
        // Every event inside a block pushes its end out, so a block that the
        // parser reports in pieces still ends up with the range of all of them.
        span_end = span_end.max(range.end);
        if let Some(b) = current.as_mut() {
            if b.source.start == 0 && b.source.end == 0 {
                b.source = range.start..range.end;
            } else {
                b.source.end = b.source.end.max(range.end);
            }
        }
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

            // Images are not fetched. A document viewer that reaches the network
            // because a file it opened said to is a document viewer that leaks
            // which files you open, and to whom.
            Event::Start(Tag::Image { .. }) => style.italic = true,
            Event::End(TagEnd::Image) => style.italic = false,

            Event::Text(t) => push_text!(t.to_string()),
            Event::Code(t) => {
                let was = style.code;
                style.code = true;
                push_text!(t.to_string());
                style.code = was;
            }
            // Raw HTML is TEXT, never markup. Nothing here can execute it, but
            // showing `<b>` as bold would mean this viewer disagrees with the
            // console about what the same file says.
            Event::Html(t) | Event::InlineHtml(t) => push_text!(t.to_string()),

            Event::SoftBreak => push_text!(" ".to_string()),
            Event::HardBreak => push_text!("\n".to_string()),
            Event::Rule => {
                close!();
                doc.blocks.push(Block { kind: Kind::Rule, spans: Vec::new(), depth, source: range.clone() });
            }
            Event::TaskListMarker(done) => {
                push_text!(if done { "[x] ".to_string() } else { "[ ] ".to_string() })
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
}
