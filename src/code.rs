//! Syntax highlighting, in about as little code as the job admits.
//!
//! # Why not a real highlighting library
//!
//! The usual answer is `syntect`, which is excellent and wrong here: it loads a
//! syntax definition set at startup, and this program's entire budget is smaller
//! than that load. `tree-sitter` means a grammar per language, compiled in, for
//! the same reason. Both are the right choice for an editor you leave open all
//! day and the wrong one for a program whose pitch is that opening it costs
//! nothing.
//!
//! So: a table-driven lexer. Each language is a handful of strings -- what starts
//! a comment, what quotes a string, which words are keywords -- and one scanner
//! walks the text once. It is not a parser and does not pretend to be: it cannot
//! tell a type from a variable or know that a word is a function call.
//!
//! What that buys, and it is the whole trade: a code block is coloured in
//! microseconds with no tables to load, and the languages that matter for the
//! files this opens -- skills, agent instructions, configuration, READMEs -- are
//! a short list.
//!
//! What it costs, stated plainly: nested block comments end at the first close in
//! languages that allow nesting, and a keyword inside an identifier-like context
//! is still a keyword. Both are visible only if looked for, and neither can
//! misrepresent what the code says, because the TEXT is never altered -- only
//! its colour.

/// What a stretch of code is, as far as colour is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Plain,
    Keyword,
    Str,
    Number,
    Comment,
}

/// The few facts about a language that a lexer needs.
pub struct Lang {
    pub names: &'static [&'static str],
    pub line_comment: &'static [&'static str],
    pub block_comment: Option<(&'static str, &'static str)>,
    /// Characters that open and close a string. Escaping is `\` where the
    /// language has it.
    pub quotes: &'static [char],
    pub escapes: bool,
    pub keywords: &'static [&'static str],
}

const RUST_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "bool", "char", "str", "u8", "u16", "u32", "u64",
    "usize", "i8", "i16", "i32", "i64", "isize", "f32", "f64", "String", "Vec", "Option", "Result",
    "Some", "None", "Ok", "Err",
];

const JS_KW: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
    "delete", "do", "else", "export", "extends", "finally", "for", "from", "function", "if",
    "import", "in", "instanceof", "let", "new", "of", "return", "static", "super", "switch",
    "this", "throw", "try", "typeof", "var", "void", "while", "yield", "true", "false", "null",
    "undefined", "interface", "type", "enum", "implements", "readonly",
];

const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "True",
    "False", "None", "self",
];

const SH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "case", "esac", "for", "while", "until", "do", "done",
    "function", "in", "return", "export", "local", "readonly", "set", "unset", "shift", "echo",
    "cd", "source", "exit", "trap",
];

const GO_KW: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
    "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
    "return", "select", "struct", "switch", "type", "var", "nil", "true", "false", "string",
    "int", "error", "bool", "byte", "rune",
];

const C_KW: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "int", "long", "return", "short", "signed",
    "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned", "void", "volatile",
    "while", "class", "namespace", "template", "public", "private", "protected", "virtual",
    "new", "delete", "nullptr", "true", "false", "bool",
];

const SQL_KW: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create",
    "table", "drop", "alter", "add", "index", "join", "left", "right", "inner", "outer", "on",
    "group", "order", "by", "having", "limit", "offset", "as", "and", "or", "not", "null",
    "primary", "key", "foreign", "references", "distinct", "union", "all", "case", "when",
    "then", "else", "end",
];

const CONF_KW: &[&str] = &["true", "false", "null", "yes", "no", "on", "off"];

/// Every language this knows, in the order it looks them up.
pub const LANGS: &[Lang] = &[
    Lang {
        names: &["rust", "rs"],
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &['"'],
        escapes: true,
        keywords: RUST_KW,
    },
    Lang {
        names: &["javascript", "js", "typescript", "ts", "jsx", "tsx", "json5"],
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &['"', '\'', '`'],
        escapes: true,
        keywords: JS_KW,
    },
    Lang {
        names: &["python", "py"],
        line_comment: &["#"],
        block_comment: None,
        quotes: &['"', '\''],
        escapes: true,
        keywords: PY_KW,
    },
    Lang {
        names: &["bash", "sh", "shell", "zsh", "console"],
        line_comment: &["#"],
        block_comment: None,
        quotes: &['"', '\''],
        escapes: true,
        keywords: SH_KW,
    },
    Lang {
        names: &["go", "golang"],
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &['"', '`'],
        escapes: true,
        keywords: GO_KW,
    },
    Lang {
        names: &["c", "cpp", "c++", "h", "hpp", "java", "cs", "csharp"],
        line_comment: &["//"],
        block_comment: Some(("/*", "*/")),
        quotes: &['"', '\''],
        escapes: true,
        keywords: C_KW,
    },
    Lang {
        names: &["sql", "postgres", "postgresql", "mysql", "sqlite"],
        line_comment: &["--"],
        block_comment: Some(("/*", "*/")),
        quotes: &['\'', '"'],
        escapes: false,
        keywords: SQL_KW,
    },
    Lang {
        names: &["json"],
        line_comment: &[],
        block_comment: None,
        quotes: &['"'],
        escapes: true,
        keywords: &["true", "false", "null"],
    },
    Lang {
        names: &["yaml", "yml", "toml", "ini", "conf", "cfg", "properties"],
        line_comment: &["#"],
        block_comment: None,
        quotes: &['"', '\''],
        escapes: true,
        keywords: CONF_KW,
    },
];

/// The language a fence names, if it is one this knows.
///
/// Matched case-insensitively and on the first word, so ```` ```rust,no_run ````
/// and ```` ```Rust ```` both find Rust. An unknown fence is not an error: the
/// block is shown plain, which is what a code block did before any of this.
pub fn lang_for(fence: &str) -> Option<&'static Lang> {
    let first = fence
        .trim()
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if first.is_empty() {
        return None;
    }
    LANGS.iter().find(|l| l.names.contains(&first.as_str()))
}

/// Colour `src` as `lang`. Returns byte ranges in order, covering the whole
/// input with no gaps and no overlaps.
///
/// Covering everything is what lets the caller emit runs by walking the list
/// once. A sparse list of "interesting" ranges would make the caller responsible
/// for the gaps, which is where an off-by-one loses a character.
pub fn highlight(lang: &Lang, src: &str) -> Vec<(usize, usize, Tok)> {
    let b = src.as_bytes();
    let mut out: Vec<(usize, usize, Tok)> = Vec::new();
    let mut i = 0usize;
    let mut plain_from = 0usize;

    // Close the run of ordinary text before a coloured one starts.
    macro_rules! flush {
        ($to:expr) => {
            if $to > plain_from {
                out.push((plain_from, $to, Tok::Plain));
            }
        };
    }

    while i < b.len() {
        // Block comment
        if let Some((open, close)) = lang.block_comment {
            if src[i..].starts_with(open) {
                flush!(i);
                let end = src[i + open.len()..]
                    .find(close)
                    .map(|p| i + open.len() + p + close.len())
                    .unwrap_or(b.len());
                out.push((i, end, Tok::Comment));
                i = end;
                plain_from = i;
                continue;
            }
        }
        // Line comment
        if let Some(marker) = lang.line_comment.iter().find(|m| src[i..].starts_with(**m)) {
            let _ = marker;
            flush!(i);
            let end = src[i..].find('\n').map(|p| i + p).unwrap_or(b.len());
            out.push((i, end, Tok::Comment));
            i = end;
            plain_from = i;
            continue;
        }
        let c = b[i] as char;
        // String
        if lang.quotes.contains(&c) {
            flush!(i);
            let mut j = i + 1;
            while j < b.len() {
                let d = b[j] as char;
                if lang.escapes && d == '\\' {
                    j += 2;
                    continue;
                }
                if d == c {
                    j += 1;
                    break;
                }
                // An unterminated string ends at the line, not at the end of the
                // file: one stray quote would otherwise colour everything after
                // it, which looks like the highlighter broke.
                if d == '\n' {
                    break;
                }
                j += 1;
            }
            let end = j.min(b.len());
            out.push((i, end, Tok::Str));
            i = end;
            plain_from = i;
            continue;
        }
        // Number
        if c.is_ascii_digit() && !prev_is_word(b, i) {
            flush!(i);
            let mut j = i;
            while j < b.len() {
                let d = b[j] as char;
                if d.is_ascii_alphanumeric() || d == '.' || d == '_' {
                    j += 1;
                } else {
                    break;
                }
            }
            out.push((i, j, Tok::Number));
            i = j;
            plain_from = i;
            continue;
        }
        // Word, which may be a keyword
        if c.is_ascii_alphabetic() || c == '_' {
            let mut j = i;
            while j < b.len() {
                let d = b[j] as char;
                if d.is_ascii_alphanumeric() || d == '_' {
                    j += 1;
                } else {
                    break;
                }
            }
            let word = &src[i..j];
            if lang.keywords.contains(&word) {
                flush!(i);
                out.push((i, j, Tok::Keyword));
                plain_from = j;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    flush!(b.len());
    out
}

/// Whether the byte before `i` is part of a word, so `x1` is not read as a
/// number stuck to an identifier.
fn prev_is_word(b: &[u8], i: usize) -> bool {
    i > 0 && {
        let p = b[i - 1] as char;
        p.is_ascii_alphanumeric() || p == '_'
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(lang: &str, src: &str) -> Vec<(String, Tok)> {
        let l = lang_for(lang).expect("a known language");
        highlight(l, src)
            .into_iter()
            .map(|(a, b, t)| (src[a..b].to_string(), t))
            .collect()
    }

    /// The property everything else depends on: the ranges tile the input.
    fn assert_covers(lang: &str, src: &str) {
        let l = lang_for(lang).expect("known");
        let spans = highlight(l, src);
        let mut at = 0;
        for (a, b, _) in &spans {
            assert_eq!(*a, at, "gap or overlap at {at} in {src:?}");
            assert!(b > a, "empty span at {a}");
            at = *b;
        }
        assert_eq!(at, src.len(), "did not reach the end of {src:?}");
        let rebuilt: String = spans.iter().map(|(a, b, _)| &src[*a..*b]).collect();
        assert_eq!(rebuilt, src, "the text was altered");
    }

    #[test]
    fn the_spans_tile_the_input_exactly() {
        // Covering everything with no gaps is what lets the caller walk the list
        // once. A gap silently drops characters from the screen.
        for (lang, src) in [
            ("rust", "fn main() { let x = 1; }\n"),
            ("python", "def f(a):\n    return 'hi'  # note\n"),
            ("bash", "if [ -f x ]; then echo \"hi\"; fi\n"),
            ("json", "{\"a\": 1, \"b\": [true, null]}\n"),
            ("sql", "select * from t where a = 'x' -- c\n"),
            ("go", "func main() { /* b */ }\n"),
        ] {
            assert_covers(lang, src);
        }
    }

    #[test]
    fn the_text_is_never_altered_only_coloured() {
        // A highlighter that can change what the code SAYS is worse than none.
        let src = "let s = \"a\\\"b\"; // trailing\n";
        assert_covers("rust", src);
    }

    #[test]
    fn keywords_strings_numbers_and_comments_are_told_apart() {
        let t = toks("rust", "let x = 42; // why\n");
        assert!(t.contains(&("let".into(), Tok::Keyword)), "{t:?}");
        assert!(t.iter().any(|(s, k)| s == "42" && *k == Tok::Number), "{t:?}");
        assert!(t.iter().any(|(s, k)| s.contains("why") && *k == Tok::Comment), "{t:?}");
    }

    #[test]
    fn a_hash_is_a_comment_in_python_and_not_in_rust() {
        assert!(toks("python", "# hi\n").iter().any(|(_, k)| *k == Tok::Comment));
        assert!(!toks("rust", "# hi\n").iter().any(|(_, k)| *k == Tok::Comment));
    }

    #[test]
    fn an_unterminated_string_stops_at_the_line() {
        // One stray quote would otherwise colour the rest of the file, which
        // looks like the highlighter fell over rather than like a typo.
        let t = toks("rust", "let a = \"oops\nlet b = 1;\n");
        let coloured: usize = t
            .iter()
            .filter(|(_, k)| *k == Tok::Str)
            .map(|(s, _)| s.len())
            .sum();
        assert!(coloured < 10, "the string ran away: {t:?}");
        assert!(t.iter().any(|(s, k)| s == "let" && *k == Tok::Keyword), "{t:?}");
    }

    #[test]
    fn an_unterminated_block_comment_stops_at_the_end_rather_than_panicking() {
        assert_covers("rust", "/* never closed\nfn x() {}\n");
    }

    #[test]
    fn a_number_stuck_to_an_identifier_is_not_a_number() {
        let t = toks("rust", "let x1 = 2;\n");
        assert!(!t.iter().any(|(s, k)| s == "1" && *k == Tok::Number), "{t:?}");
    }

    #[test]
    fn a_fence_with_extra_words_still_finds_its_language() {
        // ```rust,no_run and ```Rust both mean Rust.
        assert!(lang_for("rust,no_run").is_some());
        assert!(lang_for("Rust").is_some());
        assert!(lang_for("  js ").is_some());
    }

    #[test]
    fn an_unknown_or_absent_fence_is_not_an_error() {
        // The block is shown plain, which is what it did before highlighting
        // existed. Refusing would make an unfamiliar language worse than none.
        assert!(lang_for("brainfuck").is_none());
        assert!(lang_for("").is_none());
        assert!(lang_for("   ").is_none());
    }

    #[test]
    fn every_language_is_reachable_by_every_name_it_claims() {
        for l in LANGS {
            for n in l.names {
                assert!(lang_for(n).is_some(), "{n} is unreachable");
            }
        }
    }

    #[test]
    fn no_two_languages_claim_the_same_name() {
        // Two claims on one name means the second is dead code and whichever
        // came first silently wins.
        let mut seen = std::collections::BTreeSet::new();
        for l in LANGS {
            for n in l.names {
                assert!(seen.insert(*n), "{n} is claimed twice");
            }
        }
    }

    #[test]
    fn highlighting_a_large_block_is_fast_enough_to_do_on_every_keystroke() {
        // The budget this whole program is built around. A highlighter that costs
        // milliseconds per block would undo it.
        let src = "fn main() {\n    let x = 42; // a comment\n    println!(\"hi\");\n}\n".repeat(200);
        let l = lang_for("rust").expect("rust");
        let t = std::time::Instant::now();
        let spans = highlight(l, &src);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(!spans.is_empty());
        assert!(ms < 20.0, "highlighting {} bytes took {ms:.2}ms", src.len());
    }
}
