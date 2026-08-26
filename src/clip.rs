//! The system clipboard, opened the first time it is needed.
//!
//! # Why this is lazy
//!
//! Constructing an `arboard::Clipboard` is not free: on X11 it opens a second
//! connection to the display server and spawns a thread to own selections on,
//! and on Wayland it binds a `wlr-data-control` global. That is startup cost, in
//! a program whose entire reason to exist is that it has almost none -- and it
//! is cost paid by every launch to serve the launches where somebody presses
//! Ctrl+C. So nothing happens here until the first copy or paste.
//!
//! # Why a failure is not fatal
//!
//! There is no clipboard at all in a headless session, in a bare compositor with
//! no data-control protocol, or over a forwarded display that refuses the
//! selection. None of that is a reason for a text editor to stop working, so
//! every call answers with whether it worked and the caller says so in the
//! status line. The alternative -- unwrapping -- is an abort, because this
//! binary is built with `panic = "abort"`.
//!
//! # The thing that will still surprise people on Linux
//!
//! X11 and Wayland have no clipboard *server*. The application that copied owns
//! the selection and hands it over on request, so when it exits the clipboard
//! empties unless something else took a copy first. Every desktop environment
//! runs a clipboard manager that does exactly that, which is why nobody
//! normally notices; a bare `sway` or `i3` with nothing else running is where
//! they will. That is a property of the platform, not of this program, and the
//! fix is `wl-clip-persist` or `clipmenud` rather than anything here.

/// A handle to the system clipboard that has not necessarily been opened.
#[derive(Default)]
pub struct Clip {
    /// `None` before the first use, and after a failed open -- see [`Clip::open`].
    inner: Option<arboard::Clipboard>,
    /// Whether opening has been tried and failed, so it is not retried on every
    /// keystroke. A display server that refused once will refuse again, and the
    /// retry would cost a D-Bus or X11 round trip per attempt.
    failed: bool,
}

impl Clip {
    pub const fn new() -> Self {
        Clip { inner: None, failed: false }
    }

    fn open(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.inner.is_none() && !self.failed {
            match arboard::Clipboard::new() {
                Ok(c) => self.inner = Some(c),
                Err(_) => self.failed = true,
            }
        }
        self.inner.as_mut()
    }

    /// Put text on the clipboard. Answers whether it got there.
    pub fn set(&mut self, s: &str) -> bool {
        self.open().map(|c| c.set_text(s.to_string()).is_ok()).unwrap_or(false)
    }

    /// Take text off the clipboard.
    ///
    /// `None` covers both "there is no clipboard" and "there is nothing on it",
    /// because the caller does the same thing either way: nothing.
    pub fn get(&mut self) -> Option<String> {
        let text = self.open()?.get_text().ok()?;
        if text.is_empty() {
            return None;
        }
        Some(text)
    }
}
