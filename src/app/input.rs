//! Text-input editing helpers (cursor movement, insertion, deletion).

use super::App;

impl App {
    /// Insert an ASCII character at the current cursor position.
    pub(crate) fn insert_char(&mut self, ch: char) {
        if !ch.is_ascii() {
            return;
        }
        self.input.insert(self.cursor, ch);
        self.cursor = (self.cursor + 1).min(self.input.len());
    }

    /// Delete the character before the cursor.
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        self.input.remove(self.cursor);
    }

    /// Delete the character at the cursor.
    pub(crate) fn delete(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        self.input.remove(self.cursor);
    }

    /// Move the cursor one position to the left.
    pub(crate) fn move_cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    /// Move the cursor one position to the right.
    pub(crate) fn move_cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor += 1;
        }
    }

    /// Move the cursor to the beginning of the input.
    pub(crate) fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end of the input.
    pub(crate) fn move_cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    /// Browse to the previous entry in input history (Up arrow).
    pub(crate) fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }

        let new_idx = match self.history_index {
            None => {
                // Stash whatever the user is currently typing.
                self.history_stash = self.input.clone();
                self.input_history.len() - 1
            }
            Some(0) => return, // already at oldest entry
            Some(i) => i - 1,
        };

        self.history_index = Some(new_idx);
        self.input = self.input_history[new_idx].clone();
        self.cursor = self.input.len();
    }

    /// Browse to the next entry in input history (Down arrow), or
    /// return to the in-progress input when past the newest entry.
    pub(crate) fn history_next(&mut self) {
        let idx = match self.history_index {
            Some(i) => i,
            None => return, // not browsing history
        };

        if idx + 1 < self.input_history.len() {
            let new_idx = idx + 1;
            self.history_index = Some(new_idx);
            self.input = self.input_history[new_idx].clone();
            self.cursor = self.input.len();
        } else {
            // Moved past newest entry — restore the stashed input.
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_stash);
            self.cursor = self.input.len();
        }
    }
}
