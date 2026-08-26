//! The text being edited: lines, a cursor, and an undo history.
//!
//! Nothing here draws or knows about a window, so all of it is testable without
//! a display -- which matters more for an editor than for a viewer, because the
//! failures are silent. A viewer that lays out wrongly looks wrong. An editor
//! that puts a character in the wrong place, or loses a line at a boundary, does
//! not announce itself until someone's work is gone.
//!
//! # Lines, not one string
//!
//! A single `String` makes insertion at the cursor an O(n) memmove of everything
//! after it, and makes "move up one line" a backwards scan for a newline. A
//! `Vec<String>` makes both local: typing touches one line, and vertical movement
//! is an index. The cost is that reading the whole document back joins them, which
//! happens once per save rather than once per keystroke.
//!
//! # Columns are character counts, not byte offsets
//!
//! The obvious bug in a Rust editor is to store the cursor as a byte index and
//! then split a string in the middle of a multi-byte character -- which panics,
//! at the moment someone types an accent into a word. Columns here count
//! characters, and the conversion to bytes happens at the point of use.

/// How long typing can pause before the next keystroke starts a new undo step.
///
/// Without coalescing, undo steps back one character at a time, which nobody
/// wants; with only coalescing, a whole paragraph is one step, which is worse.
/// A pause is the natural boundary -- it is where the typist stopped to think.
pub const UNDO_PAUSE_MS: u128 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    lines: Vec<String>,
    line: usize,
    col: usize,
}

/// What kind of edit was last applied, so like follows like into one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Last {
    None,
    Typing,
    Deleting,
}

pub struct Buffer {
    lines: Vec<String>,
    /// Cursor line, and column in CHARACTERS.
    pub line: usize,
    pub col: usize,
    /// The column to return to when moving vertically through short lines.
    ///
    /// Without it, moving down through a short line and back up leaves the
    /// cursor at the short line's end -- the column is forgotten, which every
    /// editor people already use remembers.
    goal: Option<usize>,
    dirty: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last: Last,
    /// Milliseconds since some fixed origin, supplied by the caller so this
    /// module stays testable and clock-free.
    last_edit_at: u128,
    /// The fixed end of a selection, in (line, character column). The cursor is
    /// the moving end, so a selection needs no second cursor and every existing
    /// movement extends one for free -- see [`Buffer::extend`].
    ///
    /// `Some` with the cursor sitting on top of it is an EMPTY selection, which
    /// is what holding Shift without moving yet produces. [`Buffer::selection`]
    /// answers `None` for that, so nothing downstream has to special-case it.
    anchor: Option<(usize, usize)>,
}

/// The largest char boundary at or before `byte`.
///
/// Every offset that reaches a `&str` slice has to pass through this, because
/// not all of them are trustworthy. A span's rendered text and its source are
/// the same length for plain text and DIFFER wherever the parser resolved
/// something -- `&hellip;` is eight source bytes and three rendered ones -- so a
/// position derived by walking rendered characters can land a byte or two off.
/// Clicks near those constructs are approximate, and so is the edge of a
/// selection dragged across one; see `layout::lay_selection`.
///
/// Approximate is survivable. Slicing a string inside a character is not: it
/// panics, and this binary is built with `panic = "abort"`, so it is an instant
/// process kill that takes unsaved edits with it. Found by clicking beside the
/// emoji in `a &hellip; \u{1f389} b`.
///
/// Rounding DOWN rather than up, so the answer is never past the end of the
/// string and never past what the caller asked for.
pub fn boundary(s: &str, byte: usize) -> usize {
    let mut i = byte.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

impl Buffer {
    // Not `std::str::FromStr`: that returns a Result and this cannot fail.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(text: &str) -> Self {
        // A trailing newline is a line terminator, not an empty last line: split
        // would otherwise invent a blank line at the end of every well-formed
        // file, and saving would add another each time.
        let body = text.strip_suffix('\n').unwrap_or(text);
        let lines: Vec<String> = body.split('\n').map(str::to_string).collect();
        Buffer {
            lines: if lines.is_empty() { vec![String::new()] } else { lines },
            line: 0,
            col: 0,
            goal: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            last: Last::None,
            last_edit_at: 0,
            anchor: None,
        }
    }

    /// The whole document, with the trailing newline a text file should have.
    pub fn text(&self) -> String {
        let mut s = self.lines.join("\n");
        s.push('\n');
        s
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    fn chars(&self, line: usize) -> usize {
        self.lines[line].chars().count()
    }

    /// Byte offset of character `col` on `line`.
    fn byte_of(&self, line: usize, col: usize) -> usize {
        self.lines[line]
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[line].len())
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { lines: self.lines.clone(), line: self.line, col: self.col }
    }

    /// Record a point to come back to, if this edit is not a continuation.
    fn checkpoint(&mut self, kind: Last, now_ms: u128) {
        let paused = now_ms.saturating_sub(self.last_edit_at) > UNDO_PAUSE_MS;
        if self.last != kind || paused || self.undo.is_empty() {
            self.undo.push(self.snapshot());
        }
        self.last = kind;
        self.last_edit_at = now_ms;
        // Anything redone is unreachable once a new edit happens: keeping it
        // would let undo/redo/type/redo restore text the typist never wrote.
        self.redo.clear();
        self.dirty = true;
    }

    pub fn insert_char(&mut self, ch: char, now_ms: u128) {
        // Typing over a selection replaces it, and the removal and the character
        // are ONE undo step -- `take_selection` has already opened it.
        if !self.take_selection(Last::Typing, now_ms) {
            self.checkpoint(Last::Typing, now_ms);
        }
        let b = self.byte_of(self.line, self.col);
        self.lines[self.line].insert(b, ch);
        self.col += 1;
        self.goal = None;
    }

    pub fn insert_newline(&mut self, now_ms: u128) {
        // Always its own step. A line break is where a thought ended, and undoing
        // back through one is what a typist expects.
        if !self.take_selection(Last::None, now_ms) {
            self.undo.push(self.snapshot());
            self.last = Last::None;
            self.last_edit_at = now_ms;
            self.redo.clear();
            self.dirty = true;
        }

        let b = self.byte_of(self.line, self.col);
        let rest = self.lines[self.line].split_off(b);
        self.lines.insert(self.line + 1, rest);
        self.line += 1;
        self.col = 0;
        self.goal = None;
    }

    /// Delete backwards. At the start of a line, joins it to the one above.
    pub fn backspace(&mut self, now_ms: u128) {
        // With a selection, Backspace deletes THAT and nothing more -- it does
        // not also eat the character before it.
        if self.take_selection(Last::Deleting, now_ms) {
            return;
        }
        if self.col == 0 && self.line == 0 {
            return;
        }
        self.checkpoint(Last::Deleting, now_ms);
        if self.col > 0 {
            let b = self.byte_of(self.line, self.col - 1);
            self.lines[self.line].remove(b);
            self.col -= 1;
        } else {
            let cur = self.lines.remove(self.line);
            self.line -= 1;
            self.col = self.chars(self.line);
            self.lines[self.line].push_str(&cur);
        }
        self.goal = None;
    }

    /// Delete forwards. At the end of a line, pulls the next one up.
    pub fn delete(&mut self, now_ms: u128) {
        if self.take_selection(Last::Deleting, now_ms) {
            return;
        }
        let at_end = self.col == self.chars(self.line);
        if at_end && self.line + 1 >= self.lines.len() {
            return;
        }
        self.checkpoint(Last::Deleting, now_ms);
        if at_end {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
        } else {
            let b = self.byte_of(self.line, self.col);
            self.lines[self.line].remove(b);
        }
        self.goal = None;
    }

    /// Step back one edit. Returns whether there was one.
    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(s) => {
                self.redo.push(self.snapshot());
                self.lines = s.lines;
                self.line = s.line;
                self.col = s.col;
                self.last = Last::None;
                self.dirty = true;
                // Whatever was selected is not what is there now.
                self.anchor = None;
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(s) => {
                self.undo.push(self.snapshot());
                self.lines = s.lines;
                self.line = s.line;
                self.col = s.col;
                self.last = Last::None;
                self.dirty = true;
                // Whatever was selected is not what is there now.
                self.anchor = None;
                true
            }
            None => false,
        }
    }

    // ---- movement ---------------------------------------------------------

    pub fn left(&mut self) {
        self.goal = None;
        if self.col > 0 {
            self.col -= 1;
        } else if self.line > 0 {
            self.line -= 1;
            self.col = self.chars(self.line);
        }
    }

    pub fn right(&mut self) {
        self.goal = None;
        if self.col < self.chars(self.line) {
            self.col += 1;
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.col = 0;
        }
    }

    pub fn up(&mut self) {
        if self.line == 0 {
            self.col = 0;
            return;
        }
        let goal = *self.goal.get_or_insert(self.col);
        self.line -= 1;
        self.col = goal.min(self.chars(self.line));
    }

    pub fn down(&mut self) {
        if self.line + 1 >= self.lines.len() {
            self.col = self.chars(self.line);
            return;
        }
        let goal = *self.goal.get_or_insert(self.col);
        self.line += 1;
        self.col = goal.min(self.chars(self.line));
    }

    /// The cursor as a byte offset into [`Buffer::text`].
    ///
    /// The bridge to the parser's world: blocks record the source bytes they came
    /// from, so this is what turns "line 4, column 7" into "inside that heading".
    pub fn byte_offset(&self) -> usize {
        self.byte_at(self.line, self.col)
    }

    /// Any position as a byte offset into [`Buffer::text`].
    fn byte_at(&self, line: usize, col: usize) -> usize {
        let mut n = 0;
        for l in &self.lines[..line] {
            n += l.len() + 1; // the newline that joins it to the next
        }
        n + self.byte_of(line, col)
    }

    /// Put the cursor at a byte offset, clamped into the document.
    ///
    /// Used after an edit re-parses: the caret must land where the text moved to,
    /// not where it was on screen a moment ago.
    pub fn set_byte_offset(&mut self, mut byte: usize) {
        self.goal = None;
        for (i, l) in self.lines.iter().enumerate() {
            let len = l.len();
            if byte <= len {
                self.line = i;
                self.col = l[..boundary(l, byte)].chars().count();
                return;
            }
            byte -= len + 1;
        }
        self.line = self.lines.len() - 1;
        self.col = self.chars(self.line);
    }

    pub fn home(&mut self) {
        self.goal = None;
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.goal = None;
        self.col = self.chars(self.line);
    }

    // ---- selection --------------------------------------------------------
    //
    // A selection is an anchor plus the cursor, and the cursor is the one that
    // already exists. That is the whole trick: every movement above -- arrows,
    // Home, End, a click -- extends a selection without knowing that selections
    // exist, because extending one means moving the cursor and leaving the
    // anchor where it was.
    //
    // So the caller's job is only to say, before each movement, whether Shift
    // was down. Nothing else in this module changed to gain selection.

    /// Start or keep a selection before a movement, or drop one.
    ///
    /// Call it with whether Shift is held. Turning it on where there is already
    /// an anchor KEEPS that anchor, which is what makes several Shift+Arrows in
    /// a row grow one selection rather than restarting it each time.
    pub fn extend(&mut self, on: bool) {
        if on {
            self.anchor.get_or_insert((self.line, self.col));
        } else {
            self.anchor = None;
        }
    }

    /// The anchor as a byte offset, if there is one.
    ///
    /// What decides which block shows its markdown while a selection is live.
    /// Following the CURSOR there, as the caret normally does, would re-reveal a
    /// different block on every pixel of a drag -- and revealing a block changes
    /// its height, so the document would shuffle under the pointer trying to
    /// select it. The anchor does not move, so nothing moves.
    pub fn anchor_byte(&self) -> Option<usize> {
        self.anchor.map(|(l, c)| self.byte_at(l, c))
    }

    pub fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.line = self.lines.len() - 1;
        self.col = self.chars(self.line);
        self.goal = None;
    }

    /// The selected range as byte offsets into [`Buffer::text`], low end first.
    ///
    /// `None` when nothing is selected, INCLUDING when the anchor and the cursor
    /// are the same place -- holding Shift and not moving selects nothing, and
    /// callers should not have to tell that apart from not holding it.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let (al, ac) = self.anchor?;
        let (a, b) = ordered((al, ac), (self.line, self.col));
        let (lo, hi) = (self.byte_at(a.0, a.1), self.byte_at(b.0, b.1));
        (lo != hi).then_some((lo, hi))
    }

    pub fn has_selection(&self) -> bool {
        self.selection().is_some()
    }

    /// The selected text, or `None` if nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        let (al, ac) = self.anchor?;
        let ((sl, sc), (el, ec)) = ordered((al, ac), (self.line, self.col));
        if (sl, sc) == (el, ec) {
            return None;
        }
        if sl == el {
            let (a, b) = (self.byte_of(sl, sc), self.byte_of(sl, ec));
            return Some(self.lines[sl][a..b].to_string());
        }
        let mut out = self.lines[sl][self.byte_of(sl, sc)..].to_string();
        for l in &self.lines[sl + 1..el] {
            out.push('\n');
            out.push_str(l);
        }
        out.push('\n');
        out.push_str(&self.lines[el][..self.byte_of(el, ec)]);
        Some(out)
    }

    /// Remove the selected text. No undo step and no dirty flag: for callers
    /// that are opening one of their own around it.
    fn cut(&mut self) {
        let Some((al, ac)) = self.anchor else { return };
        let ((sl, sc), (el, ec)) = ordered((al, ac), (self.line, self.col));
        if (sl, sc) != (el, ec) {
            if sl == el {
                let (a, b) = (self.byte_of(sl, sc), self.byte_of(sl, ec));
                self.lines[sl].replace_range(a..b, "");
            } else {
                let tail = self.lines[el][self.byte_of(el, ec)..].to_string();
                let head = self.byte_of(sl, sc);
                self.lines[sl].truncate(head);
                self.lines[sl].push_str(&tail);
                self.lines.drain(sl + 1..=el);
            }
            self.line = sl;
            self.col = sc;
        }
        self.anchor = None;
        self.goal = None;
    }

    /// Delete the selection as one undo step. Answers whether there was one.
    ///
    /// The gate at the top of every edit above. It returns `true` once it has
    /// opened an undo step, so the caller does not open a second one and split
    /// "replace this word" into two things to undo.
    ///
    /// `kind` is what the CALLER is about to do, so that what follows coalesces
    /// into the step opened here. Selecting a word and typing a new one over it
    /// is one action to the person doing it, and one Undo should put the old
    /// word back -- not strip the replacement a letter at a time.
    fn take_selection(&mut self, kind: Last, now_ms: u128) -> bool {
        if self.selection().is_none() {
            // An empty selection is still an anchor, and leaving it behind would
            // make the next Shift+Arrow extend from somewhere stale.
            self.anchor = None;
            return false;
        }
        self.undo.push(self.snapshot());
        self.last = kind;
        self.last_edit_at = now_ms;
        self.redo.clear();
        self.dirty = true;
        self.cut();
        true
    }

    /// Delete the selection, if there is one. Answers whether there was.
    pub fn delete_selection(&mut self, now_ms: u128) -> bool {
        self.take_selection(Last::Deleting, now_ms)
    }

    /// Insert text at the cursor, replacing the selection. This is paste.
    ///
    /// Always its own undo step, never coalesced with typing around it: a paste
    /// is one action to the person who did it, and undoing it should put back
    /// exactly what was there.
    ///
    /// Carriage returns are dropped rather than stored. Text copied from a
    /// Windows application arrives as CRLF, and a `\r` kept in the buffer is
    /// invisible, survives the save, and turns up later as a stray character in
    /// whatever reads the file next.
    pub fn insert_str(&mut self, s: &str, now_ms: u128) {
        if !self.take_selection(Last::None, now_ms) {
            self.undo.push(self.snapshot());
            self.last = Last::None;
            self.last_edit_at = now_ms;
            self.redo.clear();
            self.dirty = true;
        }
        self.goal = None;
        let mut first = true;
        for part in s.replace('\r', "").split('\n') {
            if !first {
                let b = self.byte_of(self.line, self.col);
                let rest = self.lines[self.line].split_off(b);
                self.lines.insert(self.line + 1, rest);
                self.line += 1;
                self.col = 0;
            }
            first = false;
            if part.is_empty() {
                continue;
            }
            let b = self.byte_of(self.line, self.col);
            self.lines[self.line].insert_str(b, part);
            self.col += part.chars().count();
        }
    }
}

/// Which of two (line, column) positions comes first in the document.
fn ordered(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(s: &str) -> Buffer {
        Buffer::from_str(s)
    }

    #[test]
    fn a_file_round_trips_unchanged() {
        for src in ["one\ntwo\nthree\n", "single\n", "\n", "no trailing newline"] {
            let b = buf(src);
            let want = if src.ends_with('\n') { src.to_string() } else { format!("{src}\n") };
            assert_eq!(b.text(), want, "round trip of {src:?}");
        }
    }

    #[test]
    fn a_trailing_newline_does_not_grow_a_blank_line_each_save() {
        // The classic off-by-one in a line-based buffer: split('\n') on a file
        // ending in a newline yields a phantom empty last line, and joining adds
        // another every time it is written.
        let mut b = buf("hello\n");
        assert_eq!(b.lines().len(), 1);
        for _ in 0..5 {
            b = Buffer::from_str(&b.text());
        }
        assert_eq!(b.text(), "hello\n");
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut b = buf("ac\n");
        b.right();
        b.insert_char('b', 0);
        assert_eq!(b.text(), "abc\n");
        assert_eq!((b.line, b.col), (0, 2));
    }

    #[test]
    fn typing_past_a_multibyte_character_does_not_split_it() {
        // A byte-indexed cursor panics here. This is the whole reason columns
        // count characters.
        let mut b = buf("café\n");
        b.end();
        b.insert_char('!', 0);
        assert_eq!(b.text(), "café!\n");
        b.left();
        b.left();
        b.insert_char('-', 0);
        assert_eq!(b.text(), "caf-é!\n");
    }

    #[test]
    fn enter_splits_a_line_and_backspace_rejoins_it() {
        let mut b = buf("abcd\n");
        b.right();
        b.right();
        b.insert_newline(0);
        assert_eq!(b.text(), "ab\ncd\n");
        assert_eq!((b.line, b.col), (1, 0));
        b.backspace(0);
        assert_eq!(b.text(), "abcd\n");
        assert_eq!((b.line, b.col), (0, 2));
    }

    #[test]
    fn backspace_at_the_very_start_does_nothing() {
        let mut b = buf("abc\n");
        b.backspace(0);
        assert_eq!(b.text(), "abc\n");
        assert!(!b.is_dirty(), "a no-op must not mark the file modified");
    }

    #[test]
    fn delete_at_the_very_end_does_nothing() {
        let mut b = buf("abc\n");
        b.end();
        b.delete(0);
        assert_eq!(b.text(), "abc\n");
        assert!(!b.is_dirty());
    }

    #[test]
    fn delete_forwards_pulls_the_next_line_up() {
        let mut b = buf("ab\ncd\n");
        b.end();
        b.delete(0);
        assert_eq!(b.text(), "abcd\n");
    }

    #[test]
    fn the_column_survives_a_trip_through_a_short_line() {
        // Down through a short line and back up should return to where the
        // cursor visually was, not to the short line's end.
        let mut b = buf("aaaaaaaa\nbb\ncccccccc\n");
        b.end();
        assert_eq!(b.col, 8);
        b.down();
        assert_eq!(b.col, 2, "clamped to the short line");
        b.down();
        assert_eq!(b.col, 8, "and back out to the remembered column");
    }

    #[test]
    fn moving_sideways_forgets_the_goal_column() {
        let mut b = buf("aaaaaaaa\nbb\ncccccccc\n");
        b.end();
        b.down();
        b.left();
        b.down();
        assert_eq!(b.col, 1, "after moving sideways the goal is the new column");
    }

    #[test]
    fn a_pause_in_typing_starts_a_new_undo_step() {
        let mut b = buf("\n");
        for (i, c) in "abc".chars().enumerate() {
            b.insert_char(c, i as u128 * 10);
        }
        // Far enough after to be a new thought.
        b.insert_char('d', 5_000);
        assert_eq!(b.text(), "abcd\n");
        b.undo();
        assert_eq!(b.text(), "abc\n", "the pause is the boundary");
        b.undo();
        assert_eq!(b.text(), "\n", "and the burst before it is one step");
    }

    #[test]
    fn typing_and_deleting_are_separate_steps() {
        let mut b = buf("\n");
        b.insert_char('a', 0);
        b.insert_char('b', 1);
        b.backspace(2);
        assert_eq!(b.text(), "a\n");
        b.undo();
        assert_eq!(b.text(), "ab\n", "the deletion undoes on its own");
    }

    #[test]
    fn a_line_break_is_always_its_own_undo_step() {
        let mut b = buf("\n");
        b.insert_char('a', 0);
        b.insert_newline(1);
        b.insert_char('b', 2);
        b.undo();
        assert_eq!(b.text(), "a\n\n");
        b.undo();
        assert_eq!(b.text(), "a\n");
    }

    #[test]
    fn redo_puts_back_what_undo_took() {
        let mut b = buf("\n");
        b.insert_char('x', 0);
        b.undo();
        assert_eq!(b.text(), "\n");
        assert!(b.redo());
        assert_eq!(b.text(), "x\n");
    }

    #[test]
    fn typing_after_an_undo_makes_the_redo_unreachable() {
        // Otherwise undo, type, redo restores text that was never written in
        // that order -- a history that never happened.
        let mut b = buf("\n");
        b.insert_char('a', 0);
        b.undo();
        b.insert_char('z', 5_000);
        assert!(!b.redo(), "redo should have been discarded");
        assert_eq!(b.text(), "z\n");
    }

    #[test]
    fn undo_with_no_history_is_a_no_op_rather_than_a_panic() {
        let mut b = buf("hello\n");
        assert!(!b.undo());
        assert!(!b.redo());
        assert_eq!(b.text(), "hello\n");
    }

    #[test]
    fn editing_marks_the_file_modified_and_saving_clears_it() {
        let mut b = buf("hello\n");
        assert!(!b.is_dirty());
        b.insert_char('!', 0);
        assert!(b.is_dirty());
        b.mark_saved();
        assert!(!b.is_dirty());
    }

    #[test]
    fn a_byte_offset_round_trips_through_the_cursor() {
        let mut b = buf("alpha\nbravo\ncharlie\n");
        for (line, col) in [(0, 0), (0, 5), (1, 3), (2, 7)] {
            b.line = line;
            b.col = col;
            let off = b.byte_offset();
            b.set_byte_offset(off);
            assert_eq!((b.line, b.col), (line, col), "offset {off} did not round trip");
        }
    }

    #[test]
    fn a_byte_offset_lands_correctly_in_multibyte_text() {
        // The offset is BYTES, the column is CHARACTERS. Conflating them puts the
        // caret several characters away from where the parser thinks it is.
        let mut b = buf("café note\n");
        b.line = 0;
        b.col = 5; // after the space
        assert_eq!(b.byte_offset(), 6, "é is two bytes");
        b.set_byte_offset(6);
        assert_eq!(b.col, 5);
    }

    #[test]
    fn the_offset_agrees_with_the_text_it_indexes() {
        let b = {
            let mut b = buf("one\ntwo\nthree\n");
            b.line = 2;
            b.col = 2;
            b
        };
        let t = b.text();
        assert_eq!(&t[b.byte_offset()..b.byte_offset() + 3], "ree");
    }

    #[test]
    fn an_offset_past_the_end_clamps_rather_than_panicking() {
        let mut b = buf("short\n");
        b.set_byte_offset(9_999);
        assert_eq!((b.line, b.col), (0, 5));
    }

    #[test]
    fn movement_at_the_edges_stays_in_bounds() {
        let mut b = buf("ab\ncd\n");
        for _ in 0..10 {
            b.left();
            b.up();
        }
        assert_eq!((b.line, b.col), (0, 0));
        for _ in 0..10 {
            b.right();
            b.down();
        }
        assert_eq!((b.line, b.col), (1, 2));
    }
    // ---- char boundaries -----------------------------------------------

    #[test]
    fn a_position_inside_a_character_is_rounded_back_to_its_start() {
        let s = "a \u{1f389} b";
        // The emoji occupies bytes 2..6. Every offset inside it answers 2.
        for i in 2..6 {
            assert_eq!(boundary(s, i), 2, "offset {i}");
        }
        assert_eq!(boundary(s, 6), 6);
    }

    #[test]
    fn a_position_past_the_end_is_the_end() {
        let s = "abc";
        assert_eq!(boundary(s, 99), 3);
        assert_eq!(boundary("", 5), 0);
    }

    #[test]
    fn positions_on_a_boundary_are_left_alone() {
        let s = "a \u{1f389} b";
        for i in [0, 1, 2, 6, 7, 8] {
            assert_eq!(boundary(s, i), i);
        }
    }

    #[test]
    fn a_click_that_lands_inside_a_character_does_not_kill_the_program() {
        // The real one. `&hellip;` is eight source bytes and three rendered
        // ones, so a position walked through the rendered text lands two bytes
        // into the emoji after it. Slicing there panics, and this binary is
        // built with `panic = "abort"` -- an instant process kill, with the
        // unsaved buffer in it.
        let src = "a &hellip; \u{1f389} b\n";
        let mut b = Buffer::from_str(src);
        for at in 0..src.len() {
            b.set_byte_offset(at);
            // And the offset it settles on is always somewhere real.
            assert!(src.is_char_boundary(b.byte_offset()), "left the caret inside a character");
        }
    }


    // ---- selection --------------------------------------------------------

    /// Put the cursor somewhere, then select to somewhere else.
    fn selecting(src: &str, from: usize, to: usize) -> Buffer {
        let mut b = buf(src);
        b.set_byte_offset(from);
        b.extend(true);
        b.set_byte_offset(to);
        b
    }

    #[test]
    fn holding_shift_without_moving_selects_nothing() {
        // An anchor exists but sits on the cursor. Reporting that as a selection
        // would make Shift alone arm a delete.
        let mut b = buf("hello\n");
        b.set_byte_offset(2);
        b.extend(true);
        assert_eq!(b.selection(), None);
        assert_eq!(b.selected_text(), None);
        assert!(!b.has_selection());
    }

    #[test]
    fn a_selection_reads_the_same_from_either_end() {
        let src = "hello world\n";
        assert_eq!(selecting(src, 0, 5).selected_text().as_deref(), Some("hello"));
        assert_eq!(selecting(src, 5, 0).selected_text().as_deref(), Some("hello"));
        assert_eq!(selecting(src, 0, 5).selection(), Some((0, 5)));
        assert_eq!(selecting(src, 5, 0).selection(), Some((0, 5)));
    }

    #[test]
    fn a_selection_across_lines_carries_the_newlines() {
        let mut b = selecting("one\ntwo\nthree\n", 1, 9);
        assert_eq!(b.selected_text().as_deref(), Some("ne\ntwo\nt"));
        b.delete_selection(0);
        assert_eq!(b.text(), "ohree\n");
        assert_eq!(b.byte_offset(), 1);
    }

    #[test]
    fn selecting_whole_lines_removes_them() {
        let mut b = selecting("a\nb\nc\nd\n", 2, 6);
        assert_eq!(b.selected_text().as_deref(), Some("b\nc\n"));
        b.delete_selection(0);
        assert_eq!(b.text(), "a\nd\n");
    }

    #[test]
    fn select_all_takes_the_document_and_leaves_it_empty() {
        let mut b = buf("one\ntwo\n");
        b.select_all();
        assert_eq!(b.selected_text().as_deref(), Some("one\ntwo"));
        b.delete_selection(0);
        assert_eq!(b.text(), "\n");
        assert_eq!(b.lines().len(), 1);
    }

    #[test]
    fn a_selection_survives_multibyte_text_intact() {
        // The bug this guards is a byte offset landing inside a character, which
        // panics on the slice -- and this binary aborts on panic.
        let src = "h\u{e9}llo w\u{f6}rld \u{fc}n\u{ef}code\n";
        for from in 0..src.len() {
            if !src.is_char_boundary(from) {
                continue;
            }
            for to in 0..src.len() {
                if !src.is_char_boundary(to) {
                    continue;
                }
                let mut b = selecting(src, from, to);
                let taken = b.selected_text().unwrap_or_default();
                let (lo, hi) = (from.min(to), from.max(to));
                assert_eq!(taken, src[lo..hi], "{from}..{to}");
                b.delete_selection(0);
                assert_eq!(b.text(), format!("{}{}", &src[..lo], &src[hi..]), "{from}..{to}");
            }
        }
    }

    #[test]
    fn typing_over_a_selection_replaces_it_in_one_undo_step() {
        let mut b = selecting("hello world\n", 0, 5);
        b.insert_char('b', 0);
        b.insert_char('y', 1);
        b.insert_char('e', 2);
        assert_eq!(b.text(), "bye world\n");
        // One step back to the whole original, not three.
        b.undo();
        assert_eq!(b.text(), "hello world\n");
    }

    #[test]
    fn backspace_over_a_selection_takes_only_the_selection() {
        // The bug: deleting the selection AND the character before it, which
        // silently eats a character every time somebody replaces a word.
        let mut b = selecting("hello world\n", 6, 11);
        b.backspace(0);
        assert_eq!(b.text(), "hello \n");
        let mut b = selecting("hello world\n", 6, 11);
        b.delete(0);
        assert_eq!(b.text(), "hello \n");
    }

    #[test]
    fn enter_over_a_selection_replaces_it_with_the_break() {
        let mut b = selecting("hello world\n", 5, 6);
        b.insert_newline(0);
        assert_eq!(b.text(), "hello\nworld\n");
        b.undo();
        assert_eq!(b.text(), "hello world\n");
    }

    #[test]
    fn pasting_puts_text_in_and_undoes_as_one_step() {
        let mut b = buf("ac\n");
        b.set_byte_offset(1);
        b.insert_str("b", 0);
        assert_eq!(b.text(), "abc\n");
        assert_eq!(b.byte_offset(), 2);
        b.undo();
        assert_eq!(b.text(), "ac\n");
    }

    #[test]
    fn pasting_several_lines_makes_several_lines() {
        let mut b = buf("start end\n");
        b.set_byte_offset(6);
        b.insert_str("one\ntwo\nthree", 0);
        assert_eq!(b.text(), "start one\ntwo\nthreeend\n");
        assert_eq!(b.lines().len(), 3);
        // The cursor lands after what was pasted, ready to keep typing.
        assert_eq!(b.byte_offset(), 19);
    }

    #[test]
    fn pasting_from_windows_does_not_leave_carriage_returns() {
        // Text copied out of a Windows application arrives as CRLF. A `\r` kept
        // in the buffer is invisible, survives the save, and turns up as a stray
        // character in whatever reads the file next.
        let mut b = buf("\n");
        b.insert_str("one\r\ntwo\r\n", 0);
        assert!(!b.text().contains('\r'), "kept a carriage return: {:?}", b.text());
        assert_eq!(b.text(), "one\ntwo\n\n");
    }

    #[test]
    fn pasting_over_a_selection_replaces_it_in_one_step() {
        let mut b = selecting("hello world\n", 0, 5);
        b.insert_str("goodbye", 0);
        assert_eq!(b.text(), "goodbye world\n");
        b.undo();
        assert_eq!(b.text(), "hello world\n");
    }

    #[test]
    fn several_shifted_movements_grow_one_selection() {
        // `extend(true)` must KEEP an anchor it already has. Resetting it each
        // time would make every Shift+Arrow select exactly one character.
        let mut b = buf("hello\n");
        for _ in 0..3 {
            b.extend(true);
            b.right();
        }
        assert_eq!(b.selected_text().as_deref(), Some("hel"));
    }

    #[test]
    fn a_movement_without_shift_drops_the_selection() {
        let mut b = selecting("hello\n", 0, 3);
        b.extend(false);
        b.right();
        assert_eq!(b.selection(), None);
        assert!(b.anchor_byte().is_none());
    }

    #[test]
    fn undoing_drops_a_selection_rather_than_keeping_a_stale_one() {
        // The text under the anchor is not the text that was selected any more,
        // so keeping it would make the next keystroke delete the wrong range.
        let mut b = buf("hello world\n");
        b.set_byte_offset(5);
        b.insert_char('!', 0);
        b.set_byte_offset(0);
        b.extend(true);
        b.set_byte_offset(5);
        b.undo();
        assert_eq!(b.selection(), None);
    }

    #[test]
    fn deleting_a_selection_marks_the_file_modified() {
        let mut b = selecting("hello\n", 0, 2);
        assert!(!b.is_dirty());
        b.delete_selection(0);
        assert!(b.is_dirty());
    }

    #[test]
    fn deleting_with_nothing_selected_changes_nothing() {
        let mut b = buf("hello\n");
        assert!(!b.delete_selection(0));
        assert_eq!(b.text(), "hello\n");
        assert!(!b.is_dirty(), "a no-op must not mark the file modified");
    }

    #[test]
    fn the_anchor_is_where_the_selection_started_not_where_it_ends() {
        // What the revealed block follows during a drag. If it moved with the
        // cursor the document would re-lay itself under the pointer.
        let mut b = buf("one\ntwo\nthree\n");
        b.set_byte_offset(2);
        b.extend(true);
        b.set_byte_offset(10);
        assert_eq!(b.anchor_byte(), Some(2));
        b.set_byte_offset(12);
        assert_eq!(b.anchor_byte(), Some(2));
    }
}
