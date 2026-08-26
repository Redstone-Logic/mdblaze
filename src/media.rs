//! Finding the pictures a document points at, and refusing the ones it should
//! not have.
//!
//! # The network is not a source
//!
//! A markdown file arrives from anywhere -- an email attachment, a repository, a
//! colleague, a directory you were handed. `![](https://example.com/x.png)` in
//! such a file is a request that this program tell `example.com` that you opened
//! it, when you opened it, and from what address. Every mail client learned this
//! the hard way and every one of them now blocks remote images by default.
//!
//! So remote images are not fetched, and there is no setting to fetch them. The
//! alt text is shown with the reason instead, which is honest about what is
//! there and what is not.
//!
//! This is the same rule [`crate::doc`] applies to link schemes and the same one
//! [`crate::desktop`] checks its icon against. It is stated in three places
//! because it is a property of the product rather than of any one module.
//!
//! # Why decoding is cached
//!
//! The document is reparsed on every keystroke -- that is what makes live
//! rendering live -- and decoding a screenshot is milliseconds. Without a cache,
//! typing in a document with a picture in it would be visibly slow, and the
//! picture would be decoded again for a change three pages away from it.
//!
//! The cache is keyed on the URL as written, so it survives reparsing and is
//! emptied when a different file is opened.

use crate::doc::{Doc, Kind};
use crate::pixels::{decode, Bitmap};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Why a picture is not on the page.
///
/// Carried rather than collapsed to "missing" because the four cases want
/// different things from the reader: a typo'd path, a file they can fix, a
/// format this program does not read, and a deliberate refusal are not the same
/// news.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// A URL. Not fetched, by design.
    Remote,
    /// The path resolves nowhere.
    NotFound,
    /// It is there and could not be read -- permissions, most often.
    Unreadable,
    /// Read, and not a format this program decodes.
    Unsupported,
}

impl Missing {
    /// What to tell the reader, in the space where the picture would be.
    pub fn reason(self) -> &'static str {
        match self {
            Missing::Remote => "not fetched: this program does not go to the network",
            Missing::NotFound => "no such file",
            Missing::Unreadable => "could not be read",
            Missing::Unsupported => "not a PNG or JPEG",
        }
    }
}

/// A picture's state in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Art {
    /// Parsed, not yet looked for. [`Media::attach`] resolves these.
    ///
    /// Parsing does no IO on purpose: it is called on every keystroke and it is
    /// a pure function of the source, which is what makes it testable without a
    /// filesystem.
    Unresolved,
    Ready(Rc<Bitmap>),
    Missing(Missing),
}

/// The decoded pictures of one document.
#[derive(Debug, Default)]
pub struct Media {
    /// The directory the document is in. Relative paths are resolved against
    /// it -- not against the working directory, which for a file opened by
    /// double-click is wherever the desktop happened to be.
    base: Option<PathBuf>,
    cache: HashMap<String, Art>,
}

/// The most bytes read from one file before giving up.
///
/// The decoders refuse absurd dimensions, but that check happens after the file
/// is in memory. A 400MB file is not a picture in a document.
const MAX_BYTES: u64 = 64 << 20;

impl Media {
    /// A cache for a document at `path`.
    pub fn for_document(path: Option<&Path>) -> Media {
        Media {
            base: path.and_then(|p| p.parent()).map(Path::to_path_buf),
            cache: HashMap::new(),
        }
    }

    /// Resolve and decode every picture in `doc`, in place.
    ///
    /// Cheap to call repeatedly: a URL already looked up is an `Rc` clone.
    pub fn attach(&mut self, doc: &mut Doc) {
        for block in &mut doc.blocks {
            if let Kind::Image { url, art, .. } = &mut block.kind {
                if !self.cache.contains_key(url.as_str()) {
                    let loaded = self.load(url);
                    self.cache.insert(url.clone(), loaded);
                }
                *art = self.cache[url.as_str()].clone();
            }
        }
    }

    fn load(&self, url: &str) -> Art {
        let Some(path) = self.resolve(url) else {
            return Art::Missing(Missing::Remote);
        };
        match std::fs::metadata(&path) {
            Err(_) => return Art::Missing(Missing::NotFound),
            Ok(m) if m.len() > MAX_BYTES => return Art::Missing(Missing::Unreadable),
            Ok(m) if !m.is_file() => return Art::Missing(Missing::NotFound),
            Ok(_) => {}
        }
        let Ok(bytes) = std::fs::read(&path) else {
            return Art::Missing(Missing::Unreadable);
        };
        match decode(&bytes) {
            Some(b) => Art::Ready(Rc::new(b)),
            None => Art::Missing(Missing::Unsupported),
        }
    }

    /// Where on disk a URL points, or `None` if it does not point at disk.
    ///
    /// A scheme means somewhere else. `file:` is refused with the rest, not
    /// because it is dangerous but because a document that says `file:///home/
    /// someone-else/...` is a document written on another machine, and quietly
    /// resolving it against this one is a guess dressed as a result.
    fn resolve(&self, url: &str) -> Option<PathBuf> {
        let url = url.trim();
        if url.is_empty() || has_scheme(url) {
            return None;
        }
        // A fragment or query on a local path is a web habit, not a filename.
        let url = url.split(['#', '?']).next().unwrap_or(url);
        let decoded = percent_decode(url);
        let p = Path::new(&decoded);
        if p.is_absolute() {
            return Some(p.to_path_buf());
        }
        // No document on disk -- an unsaved buffer -- means there is no "beside
        // the document" to resolve against, so a relative path has no meaning
        // and answering with the working directory would be a guess.
        self.base.as_ref().map(|dir| dir.join(p))
    }

    /// How many URLs have been looked up. For a test to prove the cache is one.
    pub fn looked_up(&self) -> usize {
        self.cache.len()
    }
}

/// Does this look like `scheme:` rather than a path?
///
/// Deliberately strict about what a scheme is -- a letter followed by letters,
/// digits, `+`, `-` or `.` and then a colon -- so that a file called
/// `notes:2026.png` is a file and not a protocol.
fn has_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    for c in chars {
        match c {
            ':' => return true,
            c if c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.' => {}
            _ => return false,
        }
    }
    false
}

/// Undo `%20` and friends. Markdown written by a tool escapes spaces in paths,
/// and a file called `my shot.png` is otherwise never found.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::parse;

    fn dir() -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("mdedit-media-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    fn write_png(path: &Path, w: u32, h: u32) {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("header");
            writer
                .write_image_data(&vec![200u8; (w * h * 4) as usize])
                .expect("data");
        }
        std::fs::write(path, out).expect("write png");
    }

    fn art_of(doc: &Doc) -> Vec<&Art> {
        doc.blocks
            .iter()
            .filter_map(|b| match &b.kind {
                Kind::Image { art, .. } => Some(art),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_picture_beside_the_document_is_found_and_decoded() {
        let d = dir();
        write_png(&d.join("shot.png"), 8, 4);
        let mut doc = parse("![a screenshot](shot.png)\n");
        let mut m = Media::for_document(Some(&d.join("notes.md")));
        m.attach(&mut doc);
        match art_of(&doc)[..] {
            [Art::Ready(b)] => assert_eq!((b.w, b.h), (8, 4)),
            ref other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn relative_paths_resolve_beside_the_document_not_beside_the_shell() {
        // A file opened by double-click inherits whatever directory the desktop
        // was in. Resolving against that would find pictures at random.
        let d = dir();
        std::fs::create_dir_all(d.join("img")).unwrap();
        write_png(&d.join("img/one.png"), 2, 2);
        let mut doc = parse("![](img/one.png)\n");
        let mut m = Media::for_document(Some(&d.join("notes.md")));
        m.attach(&mut doc);
        assert!(matches!(art_of(&doc)[0], Art::Ready(_)), "not found beside the document");
        // And the working directory is not where it looked: the same relative
        // path from a document somewhere else finds nothing.
        let elsewhere = Media::for_document(Some(Path::new("/tmp/other/notes.md")));
        assert_eq!(elsewhere.resolve("img/one.png"), Some(PathBuf::from("/tmp/other/img/one.png")));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_remote_picture_is_refused_rather_than_fetched() {
        for url in [
            "https://example.com/tracker.png",
            "http://example.com/x.png",
            "//example.com/x.png",
            "data:image/png;base64,AAAA",
            "file:///etc/passwd",
        ] {
            let mut doc = parse(&format!("![alt]({url})\n"));
            let mut m = Media::for_document(Some(Path::new("/tmp/notes.md")));
            m.attach(&mut doc);
            assert!(
                matches!(art_of(&doc)[0], Art::Missing(Missing::Remote) | Art::Missing(Missing::NotFound)),
                "{url} was not refused"
            );
        }
    }

    #[test]
    fn a_url_with_a_scheme_never_becomes_a_path() {
        let m = Media::for_document(Some(Path::new("/tmp/notes.md")));
        assert_eq!(m.resolve("https://example.com/a.png"), None);
        assert_eq!(m.resolve("data:image/png,x"), None);
        assert_eq!(m.resolve("javascript:alert(1)"), None);
    }

    #[test]
    fn a_filename_containing_a_colon_is_still_a_filename() {
        // `2026-08-21: notes.png` is a file somebody made, not a protocol.
        let m = Media::for_document(Some(Path::new("/tmp/notes.md")));
        assert_eq!(m.resolve("a file: with a colon.png"), Some(PathBuf::from("/tmp/a file: with a colon.png")));
    }

    #[test]
    fn escaped_spaces_in_a_path_are_unescaped() {
        // Every tool that writes markdown escapes spaces; the file on disk has
        // a real space in it.
        let m = Media::for_document(Some(Path::new("/tmp/notes.md")));
        assert_eq!(m.resolve("my%20shot.png"), Some(PathBuf::from("/tmp/my shot.png")));
    }

    #[test]
    fn a_missing_file_says_so_rather_than_showing_nothing() {
        let mut doc = parse("![diagram](nowhere-at-all.png)\n");
        let mut m = Media::for_document(Some(Path::new("/tmp/notes.md")));
        m.attach(&mut doc);
        assert_eq!(art_of(&doc)[0], &Art::Missing(Missing::NotFound));
    }

    #[test]
    fn a_file_that_is_not_a_picture_is_reported_as_such() {
        let d = dir();
        std::fs::write(d.join("notes.txt"), "this is not a picture").unwrap();
        let mut doc = parse("![](notes.txt)\n");
        let mut m = Media::for_document(Some(&d.join("a.md")));
        m.attach(&mut doc);
        assert_eq!(art_of(&doc)[0], &Art::Missing(Missing::Unsupported));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_unsaved_buffer_has_nowhere_to_resolve_relative_paths_against() {
        // Guessing the working directory here would find a picture belonging to
        // some other document, which is worse than finding none.
        let m = Media::for_document(None);
        assert_eq!(m.resolve("shot.png"), None);
        assert_eq!(m.resolve("/absolute/shot.png"), Some(PathBuf::from("/absolute/shot.png")));
    }

    #[test]
    fn one_picture_used_five_times_is_decoded_once() {
        // The document is reparsed on every keystroke. Without this, typing in
        // a document with a screenshot in it would decode the screenshot on
        // every character.
        let d = dir();
        write_png(&d.join("s.png"), 4, 4);
        let src = "![](s.png)\n\n![](s.png)\n\n![](s.png)\n\n![](s.png)\n\n![](s.png)\n";
        let mut m = Media::for_document(Some(&d.join("a.md")));
        for _ in 0..3 {
            let mut doc = parse(src);
            m.attach(&mut doc);
            assert_eq!(art_of(&doc).len(), 5);
        }
        assert_eq!(m.looked_up(), 1, "the same URL was looked up more than once");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn every_reason_says_something_a_reader_can_act_on() {
        for m in [Missing::Remote, Missing::NotFound, Missing::Unreadable, Missing::Unsupported] {
            assert!(m.reason().len() > 8, "{m:?}");
        }
    }
}
