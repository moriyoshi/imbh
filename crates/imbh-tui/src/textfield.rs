//! The one-line text field shared by the query box and the absolute-range form.
//!
//! Both editors are the same thing — a `String` plus a caret into it, driven by the same keys — so
//! the caret arithmetic and the key map live here once. The caret is a byte offset held *beside* its
//! buffer rather than inside it (the buffers are plain `String`s the rest of the app reads and
//! replaces at will), which is why every read clamps: see [`caret_in`].

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// `caret` clamped into `text` and floored onto a character boundary.
///
/// A caret routinely outlives the buffer it was set against — the query box swaps buffers with the
/// screen, the range form swaps fields, and Back/Forward restores whole query sets — and clamping on
/// read is what keeps that unobservable instead of a panicking slice.
pub(crate) fn caret_in(text: &str, caret: usize) -> usize {
    let mut caret = caret.min(text.len());
    while !text.is_char_boundary(caret) {
        caret -= 1;
    }
    caret
}

/// A borrowed one-line text field: a buffer and the caret into it.
pub(crate) struct TextField<'a> {
    pub(crate) text: &'a mut String,
    pub(crate) caret: &'a mut usize,
}

impl TextField<'_> {
    /// The caret, clamped into the buffer (see [`caret_in`]).
    pub(crate) fn position(&self) -> usize {
        caret_in(self.text, *self.caret)
    }

    /// Move one character left (`←` / `Ctrl-B`). A no-op at the start of the buffer.
    pub(crate) fn left(&mut self) {
        let at = self.position();
        let step = self.text[..at]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
        *self.caret = at - step;
    }

    /// Move one character right (`→` / `Ctrl-F`). A no-op at the end of the buffer.
    pub(crate) fn right(&mut self) {
        let at = self.position();
        let step = self.text[at..].chars().next().map_or(0, char::len_utf8);
        *self.caret = at + step;
    }

    pub(crate) fn home(&mut self) {
        *self.caret = 0;
    }

    pub(crate) fn end(&mut self) {
        *self.caret = self.text.len();
    }

    /// Insert a character at the caret and step over it.
    pub(crate) fn insert(&mut self, character: char) {
        let at = self.position();
        self.text.insert(at, character);
        *self.caret = at + character.len_utf8();
    }

    /// Delete the character before the caret (Backspace). A no-op at the start of the buffer.
    pub(crate) fn delete_before(&mut self) {
        let at = self.position();
        let Some(previous) = self.text[..at].chars().next_back() else {
            return;
        };
        let start = at - previous.len_utf8();
        self.text.remove(start);
        *self.caret = start;
    }

    /// Delete the character under the caret (`Delete` / `Ctrl-D`). A no-op at the end of the buffer.
    pub(crate) fn delete_after(&mut self) {
        let at = self.position();
        if at == self.text.len() {
            return;
        }
        self.text.remove(at);
        *self.caret = at;
    }

    /// Delete from the caret to the end of the line (`Ctrl-K`): up to the next newline, or to the end
    /// of the buffer when there is none. A caret sitting *on* a newline kills only that break, joining
    /// the two lines — Emacs `kill-line`, which is worth honouring because the query box does hold
    /// newline-joined queries (the catalog's multi-metric "visualize"). Nothing is stashed: there is no
    /// yank to pair a kill ring with.
    pub(crate) fn kill_to_end(&mut self) {
        let at = self.position();
        let end = match self.text[at..].find('\n') {
            Some(0) => at + 1,
            Some(offset) => at + offset,
            None => self.text.len(),
        };
        self.text.replace_range(at..end, "");
        *self.caret = at;
    }
}

/// Apply one editing key to `field`: the cursor keys and their Emacs aliases (`Ctrl-B`/`Ctrl-F`,
/// `Ctrl-A`/`Ctrl-E`), `Home`/`End`, `Backspace`, forward delete (`Delete`/`Ctrl-D`), `Ctrl-K`, and
/// ordinary characters.
///
/// Returns whether the key belonged to the field, so a caller can bind the rest itself. The modifier
/// guard on the character arm is load-bearing: without it every Emacs binding would also type its own
/// letter.
pub(crate) fn handle_edit_key(mut field: TextField<'_>, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Left => field.left(),
        KeyCode::Right => field.right(),
        KeyCode::Char('b') if ctrl => field.left(),
        KeyCode::Char('f') if ctrl => field.right(),
        KeyCode::Home => field.home(),
        KeyCode::End => field.end(),
        KeyCode::Char('a') if ctrl => field.home(),
        KeyCode::Char('e') if ctrl => field.end(),
        KeyCode::Backspace => field.delete_before(),
        KeyCode::Delete => field.delete_after(),
        KeyCode::Char('d') if ctrl => field.delete_after(),
        KeyCode::Char('k') if ctrl => field.kill_to_end(),
        KeyCode::Char(character) if !ctrl && !alt => field.insert(character),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a field through a key sequence, as `handle_key` does.
    fn press(text: &mut String, caret: &mut usize, keys: &[(KeyCode, KeyModifiers)]) {
        for (code, modifiers) in keys {
            handle_edit_key(TextField { text, caret }, KeyEvent::new(*code, *modifiers));
        }
    }

    #[test]
    fn the_caret_clamps_to_the_buffer_and_to_character_boundaries() {
        assert_eq!(caret_in("café", 5), 5); // the end (`é` is two bytes)
        assert_eq!(caret_in("café", 4), 3); // inside `é` -> back to its start
        assert_eq!(caret_in("café", 99), 5); // past the end -> the end
        assert_eq!(caret_in("", 3), 0);
    }

    #[test]
    fn editing_keys_move_and_change_the_text_around_the_caret() {
        let (mut text, mut caret) = ("abc".to_owned(), 3);
        press(
            &mut text,
            &mut caret,
            &[
                (KeyCode::Left, KeyModifiers::NONE),
                (KeyCode::Char('X'), KeyModifiers::NONE),
            ],
        );
        assert_eq!((text.as_str(), caret), ("abXc", 3));

        // Ctrl-K from the caret to the end, then Ctrl-A/Ctrl-E to the ends.
        press(
            &mut text,
            &mut caret,
            &[(KeyCode::Char('k'), KeyModifiers::CONTROL)],
        );
        assert_eq!((text.as_str(), caret), ("abX", 3));
        press(
            &mut text,
            &mut caret,
            &[(KeyCode::Char('a'), KeyModifiers::CONTROL)],
        );
        assert_eq!(caret, 0);
        press(
            &mut text,
            &mut caret,
            &[(KeyCode::Char('d'), KeyModifiers::CONTROL)],
        );
        assert_eq!((text.as_str(), caret), ("bX", 0));
        press(
            &mut text,
            &mut caret,
            &[(KeyCode::Char('e'), KeyModifiers::CONTROL)],
        );
        assert_eq!(caret, 2);
    }

    #[test]
    fn an_unbound_key_is_left_to_the_caller() {
        let (mut text, mut caret) = ("x".to_owned(), 1);
        let field = TextField {
            text: &mut text,
            caret: &mut caret,
        };
        // Enter, Tab, Esc and the modified letters the field does not claim belong to the caller.
        assert!(!handle_edit_key(
            field,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ));
        let field = TextField {
            text: &mut text,
            caret: &mut caret,
        };
        assert!(!handle_edit_key(
            field,
            KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)
        ));
        assert_eq!(
            text, "x",
            "a key the field declines never reaches the buffer"
        );
    }
}
