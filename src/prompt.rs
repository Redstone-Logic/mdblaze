//! Naming a file: the one line that both opening and saving go through.
//!
//! # Why there is no file browser
//!
//! Until this existed the program could only ever show a path somebody else had
//! supplied -- on the command line, or by double-click. Launched from an
//! application menu it opened an untitled buffer, refused to save it because
//! there was no filename, and offered no way to get one. It said "open a file to
//! save it", which is circular.
//!
//! Opening and saving-as are the same problem: the program needs to be told a
//! path. A browser answers only half of it, because a file you are about to
//! create is not in the listing yet -- you would have to type a name anyway. So
//! there is one prompt, it takes a path, and both commands use it.
//!
//! It is also faster than a picker for anyone who types. `~/CLAUDE.md` and Enter
//! is two seconds; finding the same file by clicking through folders is not.
//!
//! # Completion
//!
//! Tab completes against the directory. One match fills it in; several fill in
//! as far as they agree, which is the behaviour every shell has and nobody has
//! to be taught.

use std::path::{Path, PathBuf};

/// What the prompt will do with the path when it is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Open,
    SaveAs,
}

impl Intent {
    /// The word shown at the start of the line.
    pub fn label(self) -> &'static str {
        match self {
            Intent::Open => "Open:",
            Intent::SaveAs => "Save as:",
        }
    }
}

/// One entry in the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// How many entries are shown at once. The list scrolls; the window does not
/// grow, and a hundred-line dropdown over the document would hide the document.
pub const VISIBLE: usize = 10;

/// A path being typed, and the directory it is being typed in.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub intent: Intent,
    text: String,
    /// Caret position, as a byte offset into `text`.
    cursor: usize,
    /// What the last Tab said, if anything -- "3 matches", or a complaint.
    pub note: Option<String>,
    /// Nothing has been typed yet, so the seeded directory is still just a
    /// suggestion. See [`Prompt::insert`].
    fresh: bool,
    /// The directory last read, and everything in it.
    ///
    /// Cached per DIRECTORY, not per keystroke. Re-reading on every character
    /// would mean a `stat` per entry per keypress, which is unnoticeable in a
    /// project folder and awful in a home directory with ten thousand files.
    /// Filtering what is already in memory costs nothing.
    listing: Option<(PathBuf, Vec<Entry>)>,
    /// Which visible entry is highlighted.
    pub selected: usize,
    /// First visible row, so a long list scrolls instead of overflowing.
    pub top: usize,
}

impl Prompt {
    /// A prompt seeded with `start`, which should be the directory the person is
    /// most likely to mean: the open file's own, or the working directory.
    pub fn new(intent: Intent, start: &str) -> Prompt {
        Prompt {
            intent,
            text: start.to_string(),
            cursor: start.len(),
            note: None,
            fresh: true,
            listing: None,
            selected: 0,
            top: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the caret is, in characters, for drawing it.
    pub fn caret_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    pub fn insert(&mut self, ch: char) {
        // The seeded directory behaves as though it were selected: the first
        // thing typed, if it starts a path of its own, replaces it.
        //
        // Without this, typing an absolute path into a prompt that was helpfully
        // prefilled produces `/home/me/project//tmp/notes.md` -- the two glued
        // together -- and Enter then reports that no such file exists. Found by
        // driving the real window and doing exactly that.
        if self.fresh && (ch == '/' || ch == '~') {
            self.text.clear();
            self.cursor = 0;
        }
        self.fresh = false;
        self.selected = 0;
        self.top = 0;
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.note = None;
    }

    pub fn backspace(&mut self) {
        self.fresh = false;
        self.selected = 0;
        self.top = 0;
        if self.cursor == 0 {
            return;
        }
        let prev = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.note = None;
    }

    pub fn left(&mut self) {
        if let Some((i, _)) = self.text[..self.cursor].char_indices().next_back() {
            self.cursor = i;
        }
    }

    pub fn right(&mut self) {
        if let Some(ch) = self.text[self.cursor..].chars().next() {
            self.cursor += ch.len_utf8();
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// The path this names, with `~` expanded.
    ///
    /// Expanded here rather than left to the shell, because there is no shell --
    /// the program is reading this out of its own window, and `~/notes.md` is
    /// what a person types.
    pub fn path(&self, home: &Path) -> PathBuf {
        expand(&self.text, home)
    }

    /// Complete against the filesystem. Called on Tab.
    ///
    /// Fills in as far as the candidates agree, which is one match completely
    /// and several up to their common prefix. A completed directory gets its
    /// separator, so the next Tab lists inside it without a keystroke in
    /// between.
    pub fn complete(&mut self, home: &Path) {
        self.fresh = false;
        let full = expand(&self.text, home);
        let (dir, stem) = split(&full);
        let mut hits: Vec<(String, bool)> = match std::fs::read_dir(&dir) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    // Hidden entries only when asked for by name, the way a
                    // shell does it -- otherwise every completion in a home
                    // directory is a wall of dotfiles.
                    if !name.starts_with(&stem) || (name.starts_with('.') && !stem.starts_with('.'))
                    {
                        return None;
                    }
                    Some((name, is_dir))
                })
                .collect(),
            Err(_) => {
                self.note = Some("no such directory".into());
                return;
            }
        };
        if hits.is_empty() {
            self.note = Some("no match".into());
            return;
        }
        hits.sort();
        let common = shared_prefix(hits.iter().map(|(n, _)| n.as_str()));
        let only_one = hits.len() == 1;
        let mut done = dir.join(&common).to_string_lossy().to_string();
        if only_one && hits[0].1 {
            done.push('/');
        }
        self.note = if only_one { None } else { Some(format!("{} matches", hits.len())) };
        self.text = done;
        self.cursor = self.text.len();
    }
}

impl Prompt {
    /// Everything in the current directory that matches what has been typed.
    ///
    /// Reads the directory only when the directory itself changes; the filter is
    /// applied to what is already in memory.
    pub fn entries(&mut self, home: &Path) -> Vec<Entry> {
        let full = expand(&self.text, home);
        let (dir, stem) = split(&full);
        let stale = self.listing.as_ref().map(|(d, _)| d != &dir).unwrap_or(true);
        if stale {
            let mut found: Vec<Entry> = std::fs::read_dir(&dir)
                .map(|it| {
                    it.filter_map(|e| e.ok())
                        .map(|e| Entry {
                            name: e.file_name().to_string_lossy().to_string(),
                            is_dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Directories first, then by name. A listing sorted purely
            // alphabetically buries the folders among the files and makes
            // getting anywhere a hunt.
            found.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
            self.listing = Some((dir.clone(), found));
        }
        let all = self.listing.as_ref().map(|(_, v)| v.as_slice()).unwrap_or(&[]);
        let mut out: Vec<Entry> = all
            .iter()
            .filter(|e| {
                if e.name.starts_with('.') && !stem.starts_with('.') {
                    return false;
                }
                // Case-insensitive, because nobody holding shift is trying to
                // tell you something about the filesystem.
                e.name.to_lowercase().starts_with(&stem.to_lowercase())
            })
            .cloned()
            .collect();
        // The parent, unless we are already at the root. Listed first so going
        // up is always in the same place.
        if stem.is_empty() && dir.parent().is_some() {
            out.insert(0, Entry { name: "..".into(), is_dir: true });
        }
        out
    }

    /// Move the highlight, keeping it inside the window that is drawn.
    pub fn move_by(&mut self, delta: isize, count: usize) {
        if count == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        self.selected = next;
        if next < self.top {
            self.top = next;
        } else if next >= self.top + VISIBLE {
            self.top = next + 1 - VISIBLE;
        }
    }

    /// Put the highlighted entry into the line.
    ///
    /// A directory gets its separator and the listing follows it, so the whole
    /// thing is navigable with two arrow keys and Enter -- no path typed at all.
    pub fn take(&mut self, entry: &Entry, home: &Path) {
        let full = expand(&self.text, home);
        let (dir, _) = split(&full);
        let target = if entry.name == ".." {
            dir.parent().map(Path::to_path_buf).unwrap_or(dir)
        } else {
            dir.join(&entry.name)
        };
        let mut t = target.to_string_lossy().to_string();
        if entry.is_dir && !t.ends_with('/') {
            t.push('/');
        }
        self.text = t;
        self.cursor = self.text.len();
        self.fresh = false;
        self.selected = 0;
        self.top = 0;
        self.note = None;
    }
}

/// Expand a leading `~`, and nothing else. `$VARS` are deliberately not
/// expanded: this is a filename, and a file called `$HOME` should be openable.
fn expand(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(s)
}

/// Split a partly-typed path into the directory to search and the prefix to
/// match. A path ending in a separator is all directory and no prefix.
///
/// Done on the STRING rather than through `Path`, because this is text somebody
/// is in the middle of typing and `Path` normalises it. A path ending in `.` has
/// no `file_name` -- the dot is a "this directory" component -- so going through
/// `Path` meant typing a dot to reach the dotfiles silently filtered nothing.
fn split(p: &Path) -> (PathBuf, String) {
    let s = p.to_string_lossy().to_string();
    match s.rfind('/') {
        Some(i) => (PathBuf::from(&s[..=i]), s[i + 1..].to_string()),
        None if s.is_empty() => (PathBuf::from("."), String::new()),
        None => (PathBuf::from("."), s),
    }
}

/// The longest prefix every candidate shares.
fn shared_prefix<'a>(mut names: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = names.next() else { return String::new() };
    let mut end = first.len();
    for n in names {
        end = end.min(
            first
                .char_indices()
                .zip(n.char_indices())
                .take_while(|((_, a), (_, b))| a == b)
                .last()
                .map(|((i, c), _)| i + c.len_utf8())
                .unwrap_or(0),
        );
    }
    first[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "mdblaze-prompt-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    #[test]
    fn typing_and_erasing_moves_the_caret_with_the_text() {
        let mut p = Prompt::new(Intent::Open, "");
        for c in "ab".chars() {
            p.insert(c);
        }
        assert_eq!((p.text(), p.caret_chars()), ("ab", 2));
        p.backspace();
        assert_eq!((p.text(), p.caret_chars()), ("a", 1));
        p.backspace();
        p.backspace(); // past the start is not an error
        assert_eq!(p.text(), "");
    }

    #[test]
    fn a_multibyte_character_is_erased_whole() {
        // A path with an emoji or an accent in it is an ordinary path, and
        // erasing one byte of it would leave invalid UTF-8 -- which in Rust is
        // a panic, not a mistake you can see.
        let mut p = Prompt::new(Intent::Open, "");
        p.insert('n');
        p.insert('\u{e9}');
        p.insert('\u{1f389}');
        p.backspace();
        assert_eq!(p.text(), "n\u{e9}");
        p.backspace();
        assert_eq!(p.text(), "n");
    }

    #[test]
    fn the_caret_moves_by_characters_not_bytes() {
        let mut p = Prompt::new(Intent::Open, "a\u{1f389}b");
        p.home();
        assert_eq!(p.caret_chars(), 0);
        p.right();
        p.right();
        assert_eq!(p.caret_chars(), 2, "the emoji counted as more than one step");
        p.end();
        assert_eq!(p.caret_chars(), 3);
    }

    #[test]
    fn a_leading_tilde_becomes_the_home_directory() {
        // There is no shell here to do it, and `~/CLAUDE.md` is what a person
        // types.
        let home = Path::new("/home/someone");
        assert_eq!(Prompt::new(Intent::Open, "~/notes.md").path(home), home.join("notes.md"));
        assert_eq!(Prompt::new(Intent::Open, "~").path(home), home);
        // Not in the middle, and not a file that merely starts with one.
        assert_eq!(Prompt::new(Intent::Open, "~notes.md").path(home), PathBuf::from("~notes.md"));
    }

    #[test]
    fn typing_an_absolute_path_replaces_the_suggested_directory() {
        // The seeded directory is a suggestion, not a prefix. Gluing the two
        // together gives `/home/me/project//tmp/notes.md`, and Enter then says
        // no such file -- which is what the real window did.
        let mut p = Prompt::new(Intent::Open, "/home/me/project/");
        for c in "/tmp/notes.md".chars() {
            p.insert(c);
        }
        assert_eq!(p.text(), "/tmp/notes.md");
    }

    #[test]
    fn a_tilde_also_replaces_the_suggestion() {
        let mut p = Prompt::new(Intent::Open, "/home/me/project/");
        for c in "~/notes.md".chars() {
            p.insert(c);
        }
        assert_eq!(p.text(), "~/notes.md");
    }

    #[test]
    fn typing_a_plain_name_keeps_the_suggested_directory() {
        // The common case, and the reason the directory is seeded at all.
        let mut p = Prompt::new(Intent::Open, "/home/me/project/");
        for c in "notes.md".chars() {
            p.insert(c);
        }
        assert_eq!(p.text(), "/home/me/project/notes.md");
    }

    #[test]
    fn the_replacement_only_happens_on_the_very_first_keystroke() {
        // Otherwise every `/` typed while navigating would wipe the line.
        let mut p = Prompt::new(Intent::Open, "/home/me/");
        for c in "sub/dir/file.md".chars() {
            p.insert(c);
        }
        assert_eq!(p.text(), "/home/me/sub/dir/file.md");
    }

    #[test]
    fn erasing_first_also_commits_to_the_suggestion() {
        // Backspacing means the person is editing the seeded path, not starting
        // a new one, so a `/` after that is just a separator.
        let mut p = Prompt::new(Intent::Open, "/home/me/x");
        p.backspace();
        p.insert('/');
        assert_eq!(p.text(), "/home/me//");
    }

    #[test]
    fn a_dollar_sign_is_part_of_the_filename() {
        // This is a path, not a shell command. A file called `$HOME` opens.
        let home = Path::new("/home/someone");
        assert_eq!(Prompt::new(Intent::Open, "$HOME").path(home), PathBuf::from("$HOME"));
    }

    #[test]
    fn one_match_completes_the_whole_name() {
        let d = dir();
        std::fs::write(d.join("uniquely-named.md"), "x").unwrap();
        let mut p = Prompt::new(Intent::Open, &d.join("uniq").to_string_lossy());
        p.complete(Path::new("/nonexistent"));
        assert!(p.text().ends_with("uniquely-named.md"), "{}", p.text());
        assert_eq!(p.note, None, "one match needs no commentary");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn several_matches_complete_as_far_as_they_agree() {
        // What every shell does, so nobody has to be taught it.
        let d = dir();
        for n in ["report-jan.md", "report-feb.md", "report-mar.md"] {
            std::fs::write(d.join(n), "x").unwrap();
        }
        let mut p = Prompt::new(Intent::Open, &d.join("rep").to_string_lossy());
        p.complete(Path::new("/nonexistent"));
        assert!(p.text().ends_with("report-"), "stopped at {}", p.text());
        assert_eq!(p.note.as_deref(), Some("3 matches"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn completing_a_directory_adds_its_separator() {
        // So the next Tab lists inside it without a keystroke in between.
        let d = dir();
        std::fs::create_dir_all(d.join("drafts")).unwrap();
        let mut p = Prompt::new(Intent::Open, &d.join("dra").to_string_lossy());
        p.complete(Path::new("/nonexistent"));
        assert!(p.text().ends_with("drafts/"), "{}", p.text());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn nothing_matching_says_so_and_changes_nothing() {
        let d = dir();
        let typed = d.join("zzz").to_string_lossy().to_string();
        let mut p = Prompt::new(Intent::Open, &typed);
        p.complete(Path::new("/nonexistent"));
        assert_eq!(p.text(), typed, "completion ate what was typed");
        assert_eq!(p.note.as_deref(), Some("no match"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_directory_that_does_not_exist_is_reported_rather_than_ignored() {
        let mut p = Prompt::new(Intent::Open, "/definitely/not/here/x");
        p.complete(Path::new("/nonexistent"));
        assert_eq!(p.note.as_deref(), Some("no such directory"));
    }

    #[test]
    fn hidden_files_stay_hidden_until_asked_for_by_name() {
        // Otherwise every completion in a home directory is a wall of dotfiles.
        let d = dir();
        std::fs::write(d.join(".hidden.md"), "x").unwrap();
        std::fs::write(d.join("visible.md"), "x").unwrap();
        let mut p = Prompt::new(Intent::Open, &format!("{}/", d.to_string_lossy()));
        p.complete(Path::new("/nonexistent"));
        assert!(p.text().ends_with("visible.md"), "a dotfile was offered: {}", p.text());

        let mut q = Prompt::new(Intent::Open, &d.join(".hid").to_string_lossy());
        q.complete(Path::new("/nonexistent"));
        assert!(q.text().ends_with(".hidden.md"), "asked for by name and still hidden");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn typing_clears_a_stale_completion_note() {
        // "3 matches" must not sit there describing something that is no longer
        // what is in the line.
        let mut p = Prompt::new(Intent::Open, "/nope/x");
        p.complete(Path::new("/nonexistent"));
        assert!(p.note.is_some());
        p.insert('a');
        assert_eq!(p.note, None);
    }

    // ---- the listing ---------------------------------------------------

    fn populated() -> PathBuf {
        let d = dir();
        std::fs::create_dir_all(d.join("drafts")).unwrap();
        std::fs::create_dir_all(d.join("images")).unwrap();
        for n in ["alpha.md", "beta.md", "Gamma.md", ".hidden.md"] {
            std::fs::write(d.join(n), "x").unwrap();
        }
        d
    }

    #[test]
    fn the_listing_puts_directories_first() {
        // Sorted purely alphabetically, folders are buried among the files and
        // getting anywhere becomes a hunt.
        let d = populated();
        let mut p = Prompt::new(Intent::Open, &format!("{}/", d.to_string_lossy()));
        let e = p.entries(Path::new("/nonexistent"));
        let names: Vec<&str> = e.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names[0], "..", "the parent is always first");
        let first_file = e.iter().position(|x| !x.is_dir).expect("a file");
        assert!(e[..first_file].iter().all(|x| x.is_dir), "a file came before a directory");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn typing_filters_the_listing_without_rereading_the_directory() {
        let d = populated();
        let mut p = Prompt::new(Intent::Open, &format!("{}/", d.to_string_lossy()));
        assert!(p.entries(Path::new("/nonexistent")).len() > 3);
        for c in "al".chars() {
            p.insert(c);
        }
        let e = p.entries(Path::new("/nonexistent"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "alpha.md");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn filtering_ignores_case() {
        // Nobody holding shift is trying to tell you something about the
        // filesystem.
        let d = populated();
        let mut p = Prompt::new(Intent::Open, &d.join("g").to_string_lossy());
        let e = p.entries(Path::new("/nonexistent"));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "Gamma.md");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_listing_hides_dotfiles_until_asked_for() {
        let d = populated();
        let mut p = Prompt::new(Intent::Open, &format!("{}/", d.to_string_lossy()));
        assert!(p.entries(Path::new("/nonexistent")).iter().all(|e| e.name != ".hidden.md"));
        let mut q = Prompt::new(Intent::Open, &d.join(".").to_string_lossy());
        assert!(q.entries(Path::new("/nonexistent")).iter().any(|e| e.name == ".hidden.md"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn choosing_a_directory_descends_into_it() {
        // Two arrow keys and Enter, with no path typed at all -- which is the
        // whole reason the listing exists.
        let d = populated();
        let mut p = Prompt::new(Intent::Open, &format!("{}/", d.to_string_lossy()));
        let e = p.entries(Path::new("/nonexistent"));
        let drafts = e.iter().find(|x| x.name == "drafts").expect("drafts").clone();
        p.take(&drafts, Path::new("/nonexistent"));
        assert!(p.text().ends_with("drafts/"), "{}", p.text());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn choosing_the_parent_goes_up() {
        let d = populated();
        let mut p = Prompt::new(Intent::Open, &format!("{}/", d.to_string_lossy()));
        let up = Entry { name: "..".into(), is_dir: true };
        p.take(&up, Path::new("/nonexistent"));
        assert_eq!(
            PathBuf::from(p.text().trim_end_matches('/')),
            d.parent().unwrap(),
            "did not land in the parent"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_highlight_stays_inside_the_window_that_is_drawn() {
        // Otherwise the selection walks off the bottom of the list and the
        // person is moving something they cannot see.
        let mut p = Prompt::new(Intent::Open, "/tmp/");
        p.move_by(1, 40);
        assert_eq!((p.selected, p.top), (1, 0));
        for _ in 0..40 {
            p.move_by(1, 40);
        }
        assert_eq!(p.selected, 39, "ran past the end");
        assert!(p.selected < p.top + VISIBLE && p.selected >= p.top, "highlight is off screen");
        for _ in 0..80 {
            p.move_by(-1, 40);
        }
        assert_eq!((p.selected, p.top), (0, 0), "ran past the start");
    }

    #[test]
    fn moving_in_an_empty_listing_does_nothing() {
        let mut p = Prompt::new(Intent::Open, "/tmp/");
        p.move_by(1, 0);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn typing_resets_the_highlight_to_the_top() {
        // The list under it just changed; keeping the old index would highlight
        // something unrelated.
        let mut p = Prompt::new(Intent::Open, "/tmp/");
        p.move_by(1, 20);
        assert_eq!(p.selected, 1);
        p.insert('a');
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn both_intents_have_a_label_that_says_what_enter_will_do() {
        assert_eq!(Intent::Open.label(), "Open:");
        assert_eq!(Intent::SaveAs.label(), "Save as:");
    }
}
