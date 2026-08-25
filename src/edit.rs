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
}

impl Buffer {
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
        self.checkpoint(Last::Typing, now_ms);
        let b = self.byte_of(self.line, self.col);
        self.lines[self.line].insert(b, ch);
        self.col += 1;
        self.goal = None;
    }

    pub fn insert_newline(&mut self, now_ms: u128) {
        // Always its own step. A line break is where a thought ended, and undoing
        // back through one is what a typist expects.
        self.undo.push(self.snapshot());
        self.last = Last::None;
        self.last_edit_at = now_ms;
        self.redo.clear();
        self.dirty = true;

        let b = self.byte_of(self.line, self.col);
        let rest = self.lines[self.line].split_off(b);
        self.lines.insert(self.line + 1, rest);
        self.line += 1;
        self.col = 0;
        self.goal = None;
    }

    /// Delete backwards. At the start of a line, joins it to the one above.
    pub fn backspace(&mut self, now_ms: u128) {
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

    pub fn home(&mut self) {
        self.goal = None;
        self.col = 0;
    }

    pub fn end(&mut self) {
        self.goal = None;
        self.col = self.chars(self.line);
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
}
