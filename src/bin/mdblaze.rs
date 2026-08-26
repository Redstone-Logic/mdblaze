//! Open a markdown file and edit it, rendered.
//!
//! The document is always rendered. The block the caret is in shows its markdown
//! so it can be changed, and the moment the caret leaves, that block renders
//! again. There is no mode to switch and no second pane: what you edit is what
//! you are looking at.
//!
//! # Why that is affordable
//!
//! Every keystroke re-parses and re-lays out the WHOLE document. On the machine
//! this was written on that is about 4ms for a 36KB file -- far inside a frame,
//! and about a quarter of the budget at 60Hz. Incremental re-layout is where
//! editors of this kind get complicated and subtly wrong, and it buys nothing at
//! this speed.
//!
//! # Where the time goes
//!
//! ```text
//!   read the file        0.08 ms
//!   reference the fonts  0.09 ms
//!   parse                0.8  ms
//!   lay out              3.5  ms
//!   window on screen    ~30   ms   <- winit
//! ```
//!
//! Our half is free; the toolkit is the budget. Raw X11 puts a window up in
//! 0.2ms, so most of winit's cost is work a first frame does not need. Claiming
//! it back means a backend per platform, which is more work than this program, so
//! it stays a number reported by `--timing` rather than a plan.

use std::sync::Arc;
use std::time::Instant;

use mdblaze::clip::Clip;
use mdblaze::desktop;
use mdblaze::doc;
use mdblaze::edit::Buffer;
use mdblaze::file;
use mdblaze::layout::{self, Editing, Laid};
use mdblaze::media::Media;

use mdblaze::render::{self, Scaled, Theme};
use winit::event_loop::EventLoopProxy;

/// Which command asked for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Open,
    SaveAs,
}

/// What the chooser thread sends back: the intent it was asked for, and the
/// path, or `None` if the person cancelled or no chooser could be reached.
#[derive(Debug)]
struct Chosen(Intent, Option<std::path::PathBuf>);
use mdblaze::text::Text;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

/// Pixels per wheel notch, when the platform reports notches rather than pixels.
const LINE_SCROLL: f32 = 48.0;

/// How long a status message stays up before the hint returns.
const NOTE_MS: u128 = 2_500;

/// How long the discard bypass stays armed.
///
/// Short on purpose. Without a deadline the second Escape could come a minute
/// later -- long after the warning has been forgotten -- and unsaved work would
/// go with it. A swift second press is someone confirming; a slow one is someone
/// who has moved on and pressed Escape for an unrelated reason.
const DISCARD_MS: u128 = 1_500;

const BASE: f32 = mdblaze::text::BODY_PX;

/// "s", unless there was exactly one of them.
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// How the person asked to close, so the warning can name that gesture back to
/// them rather than describing a different one.
#[derive(Clone, Copy)]
enum Route {
    Escape,
    CloseButton,
}

struct App {
    t0: Instant,
    started: Instant,
    path: Option<std::path::PathBuf>,
    buffer: Buffer,
    text: Text,
    /// The decoded pictures of this document, kept across reflows. The document
    /// is reparsed on every keystroke and decoding a screenshot is milliseconds;
    /// without this, typing beside a picture would be visibly slow.
    media: Media,
    /// A file chooser is open, and the answer will arrive on the event loop.
    ///
    /// Tracked so a second Ctrl+O does not stack dialogs, and so the status line
    /// can say what the window is waiting for.
    choosing: Option<Intent>,
    /// How the chooser thread hands its answer back.
    proxy: EventLoopProxy<Chosen>,
    /// Pictures at the size they are on screen. Rebuilding them every frame cost
    /// fifteen milliseconds a keystroke; see [`Scaled`].
    scaled: Scaled,
    laid: Laid,
    laid_for: f32,
    scroll: f32,
    mods: Modifiers,
    /// Where the pointer is, in window coordinates. Kept because a click event
    /// does not carry a position -- only the move before it does.
    pointer: (f32, f32),
    /// The system clipboard, not opened until the first copy or paste.
    clip: Clip,
    /// A left button is down: the selection is being dragged out. Set on press,
    /// cleared on release, and the reason a pointer move is otherwise still just
    /// a pointer move.
    dragging: bool,
    note: Option<(String, Instant)>,
    /// When the discard bypass was armed, if it is. It expires after
    /// [`DISCARD_MS`], so closing unsaved work needs a SWIFT second press.
    armed_at: Option<Instant>,
    timing: bool,
    reported: bool,
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
}

impl App {
    fn title(&self) -> String {
        match &self.path {
            Some(p) => p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            None => "untitled".to_string(),
        }
    }

    /// Re-parse and re-lay out, revealing the block the caret is in.
    ///
    /// Called after every edit AND every caret movement, because moving the caret
    /// out of a block is what puts that block back to rendered -- the movement
    /// changes what is on screen even though it changes no text.
    fn reflow(&mut self, width: f32) {
        let source = self.buffer.text();
        let cursor = self.buffer.byte_offset();
        let mut parsed = doc::parse(&source);
        // Resolved here rather than in the parser, which does no IO on purpose:
        // it runs on every keystroke and is a pure function of the source.
        self.media.attach(&mut parsed);
        // The block that shows its markdown follows the SELECTION'S ANCHOR when
        // there is one, and the cursor otherwise. Revealing a block changes its
        // height, so letting the cursor decide would re-reveal a different block
        // on every pixel of a drag and shuffle the document under the pointer
        // doing the selecting.
        let selection = self.buffer.selection();
        let reveal = self.buffer.anchor_byte().unwrap_or(cursor);
        self.laid = layout::lay_out(
            &parsed,
            width,
            BASE,
            &self.text,
            Some(Editing { source: &source, cursor, reveal, selection }),
        );
        self.laid_for = width;
    }

    // ---- the clipboard ----------------------------------------------------
    //
    // What lands on the clipboard is the MARKDOWN, not what is on the screen.
    // Selecting a rendered heading and pasting it elsewhere gives `# Heading`,
    // because the file is the document and the rendering is a view of it -- and
    // because anything else would silently drop the formatting on the way out.

    fn copy(&mut self) {
        match self.buffer.selected_text() {
            Some(t) => {
                let n = t.chars().count();
                if self.clip.set(&t) {
                    self.say(&format!("copied {n} character{}", plural(n)));
                } else {
                    self.say("no clipboard available");
                }
            }
            None => self.say("nothing selected"),
        }
    }

    fn cut(&mut self, now: u128) {
        let Some(t) = self.buffer.selected_text() else {
            self.say("nothing selected");
            return;
        };
        let n = t.chars().count();
        // The text only leaves the buffer once it is safely on the clipboard.
        // Cutting into a clipboard that refused it would destroy the selection
        // and leave nowhere to paste it back from.
        if self.clip.set(&t) {
            self.buffer.delete_selection(now);
            self.say(&format!("cut {n} character{}", plural(n)));
        } else {
            self.say("no clipboard available \u{2014} nothing cut");
        }
    }

    fn paste(&mut self, now: u128) {
        match self.clip.get() {
            Some(t) => {
                let n = t.chars().count();
                self.buffer.insert_str(&t, now);
                self.say(&format!("pasted {n} character{}", plural(n)));
            }
            None => self.say("clipboard is empty"),
        }
    }

    /// Ask the system for a path, without blocking the window.
    ///
    /// On its own thread, always. `rfd`'s call is synchronous, and on Linux it
    /// is a D-Bus round trip to `xdg-desktop-portal` -- a service that may be
    /// slow to activate, or absent. Called on the event loop that would freeze
    /// the editor: no repaint, no Escape, nothing, until it answered. Measured
    /// on this machine, it never answered at all.
    ///
    /// So the loop keeps running, the status line says what it is waiting for,
    /// and the answer arrives later as an event.
    fn ask(&mut self, intent: Intent) {
        if self.choosing.is_some() {
            return;
        }
        self.choosing = Some(intent);
        self.say(match intent {
            Intent::Open => "choosing a file…",
            Intent::SaveAs => "choosing where to save…",
        });

        let start = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "untitled.md".into());
        let proxy = self.proxy.clone();

        std::thread::spawn(move || {
            let d = rfd::FileDialog::new()
                .set_directory(&start)
                .add_filter("Markdown", &["md", "markdown"])
                .add_filter("All files", &["*"]);
            let picked = match intent {
                Intent::Open => d.pick_file(),
                Intent::SaveAs => d.set_file_name(name).save_file(),
            };
            // If the loop is already gone the send fails, which is fine: there
            // is nothing left to tell.
            let _ = proxy.send_event(Chosen(intent, picked));
        });
    }

    /// Open `path`, replacing what is on screen.
    ///
    /// The media cache is rebuilt rather than kept: it is keyed by URL and
    /// resolved against the OLD document's directory, so carrying it over would
    /// answer a relative path with the previous file's picture.
    fn open(&mut self, path: std::path::PathBuf, width: f32) {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.buffer = Buffer::from_str(&text);
                self.media = Media::for_document(Some(&path));
                self.scaled = Scaled::default();
                self.path = Some(path);
                self.scroll = 0.0;
                self.armed_at = None;
                self.note = None;
                self.reflow(width);
                if let Some(w) = &self.window {
                    w.set_title(&self.title());
                }
            }
            Err(e) => self.say(&format!("could not open: {e}")),
        }
    }

    fn ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    fn say(&mut self, what: &str) {
        self.note = Some((what.to_string(), Instant::now()));
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            // Not an error and not a refusal. A buffer with no filename is one
            // that has never been given one, so ask for it -- the previous
            // answer, "open a file to save it", was circular and left typed
            // work with nowhere to go.
            self.ask(Intent::SaveAs);
            return;
        };
        match file::save_atomic(&path, &self.buffer.text()) {
            Ok(()) => {
                self.buffer.mark_saved();
                self.armed_at = None;
                self.say("saved");
            }
            // Never silent. A save that failed and said nothing is how work is
            // lost while someone believes it is safe.
            Err(e) => self.say(&format!("could not save: {e}")),
        }
    }

    /// Whether it is safe to close, and say so if it is not.
    ///
    /// The guard is on the ACTION, not on one key, because there are several ways
    /// out of a window -- Escape, the title bar's close button, the window
    /// manager -- and a guard that only covers the one you thought of is not a
    /// guard. It is a promise that fails on the path nobody tested.
    fn may_close(&mut self, via: Route) -> bool {
        if !self.buffer.is_dirty() {
            return true;
        }
        if self.armed().is_some() {
            return true;
        }
        self.armed_at = Some(Instant::now());
        // Names the gesture the person just made, and says it has to be QUICK.
        // "Press again to close" is true and useless: it does not say that the
        // offer expires, so a second press a minute later feels like it should
        // work -- and the first version of this message let it.
        self.say(&format!(
            "UNSAVED CHANGES — Ctrl+S to save · {} to discard",
            match via {
                Route::Escape => "double-tap Esc quickly",
                Route::CloseButton => "click close twice quickly",
            }
        ));
        false
    }

    /// The arming instant, if the bypass is still live.
    fn armed(&self) -> Option<Instant> {
        self.armed_at.filter(|t| t.elapsed().as_millis() <= DISCARD_MS)
    }

    fn viewport(&self, height: f32) -> f32 {
        (height - render::status_height(BASE)).max(1.0)
    }

    fn max_scroll(&self, height: f32) -> f32 {
        (self.laid.height - self.viewport(height)).max(0.0)
    }

    /// Keep the caret on screen. Called after the layout that placed it.
    fn follow_caret(&mut self, height: f32) {
        // What the view has to keep looking at. Normally the caret -- but a
        // selection that reaches outside the revealed block leaves no caret to
        // follow, and then it is the selection's MOVING edge instead. Following
        // the whole selection would be wrong: growing one downwards would scroll
        // to its top and lose the end being dragged out.
        let (top, bottom) = match self.laid.caret {
            Some(c) => (c.top, c.top + c.height),
            None => {
                let Some((t, b)) = self.laid.selection_bounds() else { return };
                let cursor = self.buffer.byte_offset();
                match self.buffer.selection() {
                    Some((_, hi)) if cursor == hi => (b - 1.0, b),
                    Some(_) => (t, t + 1.0),
                    None => return,
                }
            }
        };
        let view = self.viewport(height);
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + view {
            self.scroll = bottom - view;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(height));
    }
}

impl ApplicationHandler<Chosen> for App {
    /// The chooser answered. This is the only place a path arrives from it.
    fn user_event(&mut self, _el: &ActiveEventLoop, Chosen(intent, path): Chosen) {
        self.choosing = None;
        let width = self
            .window
            .as_ref()
            .map(|w| w.inner_size().width as f32)
            .unwrap_or(self.laid_for);
        let Some(path) = path else {
            self.say("nothing chosen");
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        };
        match intent {
            Intent::Open => self.open(path, width),
            Intent::SaveAs => {
                self.path = Some(path);
                self.media = Media::for_document(self.path.as_deref());
                self.save();
                if let Some(w) = &self.window {
                    w.set_title(&self.title());
                }
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(self.title())
            // Wide enough that a 79-column code block -- PEP 8's limit, and what
            // most code is written to -- fits without being clipped. That needs
            // about 683px; the rest is margin so the window is not painted into
            // a corner. Prose still stops at its own, narrower measure.
            .with_inner_size(winit::dpi::LogicalSize::new(940.0, 820.0));
        let t_win = Instant::now();
        let w = Arc::new(el.create_window(attrs).expect("could not open a window"));
        if self.timing {
            eprintln!("window:     {:.2} ms", t_win.elapsed().as_secs_f64() * 1000.0);
        }
        // The window is a document from edge to edge, so the pointer says so
        // everywhere rather than changing shape over text.
        w.set_cursor(CursorIcon::Text);
        let t_surf = Instant::now();
        let ctx = softbuffer::Context::new(w.clone()).expect("no drawing context");
        let surface = softbuffer::Surface::new(&ctx, w.clone()).expect("no drawing surface");
        if self.timing {
            eprintln!("surface:    {:.2} ms", t_surf.elapsed().as_secs_f64() * 1000.0);
        }
        self.window = Some(w);
        self.surface = Some(surface);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        let Some(window) = self.window.clone() else {
            return;
        };
        match ev {
            WindowEvent::CloseRequested => {
                // The title bar's close button went straight out, so a document
                // with unsaved changes could be lost by clicking the one control
                // every window has. Escape was guarded and this was not, which is
                // the worse half to miss.
                if self.may_close(Route::CloseButton) {
                    el.exit();
                } else {
                    window.request_redraw();
                }
            }
            // Dropping a file on the window opens it. winit already delivers
            // this; not handling it was the difference between "you cannot open
            // a file from the UI" and one match arm.
            WindowEvent::DroppedFile(path) => {
                let w = window.inner_size().width as f32;
                if self.buffer.is_dirty() {
                    self.say("unsaved changes — Ctrl+S first");
                } else {
                    self.open(path, w);
                }
                window.request_redraw();
            }

            WindowEvent::ModifiersChanged(m) => self.mods = m,

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let size = window.inner_size();
                // Command on macOS, Control everywhere else -- and Control on
                // macOS too, because a terminal-shaped person there presses it
                // out of habit and nothing else in this window wants the chord.
                let ctrl = self.mods.state().control_key()
                    || (cfg!(target_os = "macos") && self.mods.state().super_key());
                let shift = self.mods.state().shift_key();
                let now = self.ms();
                let page = self.viewport(size.height as f32) * 0.9;
                let escape = matches!(event.logical_key, Key::Named(NamedKey::Escape));
                // Whether the caret moved or the text changed: either way the
                // rendering differs, because a block reveals and hides with the
                // caret.
                let mut touched = true;

                // Shift plus a movement grows a selection, and the same movement
                // without it drops one. Done here rather than in each arm below,
                // so every movement key extends a selection without knowing that
                // selections exist -- and so does the next one added.
                //
                // Paging is deliberately not in the list: it scrolls without
                // moving the caret, so there is nothing for it to extend.
                if !ctrl
                    && matches!(
                        &event.logical_key,
                        Key::Named(
                            NamedKey::ArrowLeft
                                | NamedKey::ArrowRight
                                | NamedKey::ArrowUp
                                | NamedKey::ArrowDown
                                | NamedKey::Home
                                | NamedKey::End
                        )
                    )
                {
                    self.buffer.extend(shift);
                }

                match &event.logical_key {
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("o") => {
                        self.ask(Intent::Open);
                        touched = false;
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("s") => {
                        self.save();
                        touched = false;
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                        self.copy();
                        touched = false;
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("x") => {
                        self.cut(now);
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                        self.paste(now);
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                        self.buffer.select_all();
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("z") => {
                        let moved = if shift { self.buffer.redo() } else { self.buffer.undo() };
                        if !moved {
                            self.say(if shift { "nothing to redo" } else { "nothing to undo" });
                        }
                    }
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("y") => {
                        if !self.buffer.redo() {
                            self.say("nothing to redo");
                        }
                    }
                    // Any other Ctrl chord is somebody reaching for a feature this
                    // does not have. Swallowed rather than typed into the file.
                    Key::Character(_) if ctrl => touched = false,

                    Key::Named(NamedKey::Escape) => {
                        if self.may_close(Route::Escape) {
                            el.exit();
                        }
                        touched = false;
                    }

                    Key::Named(NamedKey::Enter) => self.buffer.insert_newline(now),
                    Key::Named(NamedKey::Backspace) => self.buffer.backspace(now),
                    Key::Named(NamedKey::Delete) => self.buffer.delete(now),
                    Key::Named(NamedKey::Space) => self.buffer.insert_char(' ', now),
                    Key::Named(NamedKey::Tab) => {
                        // Two spaces, not a tab: markdown's nesting is defined in
                        // spaces, and a literal tab renders differently in every
                        // tool that reads the file next.
                        self.buffer.insert_char(' ', now);
                        self.buffer.insert_char(' ', now);
                    }

                    Key::Named(NamedKey::ArrowLeft) => self.buffer.left(),
                    Key::Named(NamedKey::ArrowRight) => self.buffer.right(),
                    Key::Named(NamedKey::ArrowUp) => self.buffer.up(),
                    Key::Named(NamedKey::ArrowDown) => self.buffer.down(),
                    Key::Named(NamedKey::Home) => self.buffer.home(),
                    Key::Named(NamedKey::End) => self.buffer.end(),

                    // Paging scrolls without moving the caret: it is for reading,
                    // and dragging the insertion point along would lose the place
                    // someone was editing.
                    Key::Named(NamedKey::PageDown) => {
                        self.scroll =
                            (self.scroll + page).clamp(0.0, self.max_scroll(size.height as f32));
                        touched = false;
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.scroll =
                            (self.scroll - page).clamp(0.0, self.max_scroll(size.height as f32));
                        touched = false;
                    }

                    Key::Character(c) => {
                        // What the keyboard layout produced, so a composed or
                        // accented character arrives as itself rather than as the
                        // key that happened to be under the finger.
                        for ch in c.chars() {
                            self.buffer.insert_char(ch, now);
                        }
                    }
                    _ => touched = false,
                }

                if touched {
                    self.reflow(size.width as f32);
                    self.follow_caret(size.height as f32);
                }
                // Any other key means they did not mean to discard after all.
                if !escape {
                    self.armed_at = None;
                }
                window.request_redraw();
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x as f32, position.y as f32);
                // Dragging with the button down sweeps out a selection. The
                // anchor was left where the press landed, so `extend(true)` finds
                // it already there and this only has to move the cursor end.
                if self.dragging {
                    let size = window.inner_size();
                    let (x, y) = (self.pointer.0, self.pointer.1 + self.scroll);
                    if let Some(byte) = self.laid.hit(x, y, &self.text) {
                        if byte != self.buffer.byte_offset() {
                            self.buffer.extend(true);
                            self.buffer.set_byte_offset(byte);
                            self.reflow(size.width as f32);
                            window.request_redraw();
                        }
                    }
                }
            }

            WindowEvent::MouseInput { state, button, .. }
                if state == ElementState::Pressed && button == MouseButton::Left =>
            {
                let size = window.inner_size();
                // Window coordinates to document coordinates: only the scroll
                // separates them, because the layout is in document space.
                let (x, y) = (self.pointer.0, self.pointer.1 + self.scroll);
                // A click below the last line is a click at the end, which is
                // what dragging past the bottom of a document means.
                if let Some(byte) = self.laid.hit(x, y, &self.text) {
                    // Shift+click reaches from where the caret already is, the
                    // way it does in every other editor; a plain press drops any
                    // selection and starts a new one here.
                    self.buffer.extend(self.mods.state().shift_key());
                    self.buffer.set_byte_offset(byte);
                    self.dragging = true;
                    // The block under the caret changed, so what is revealed did
                    // too -- the click changes the picture even though it changed
                    // no text.
                    self.reflow(size.width as f32);
                    window.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. }
                if state == ElementState::Released && button == MouseButton::Left =>
            {
                self.dragging = false;
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let by = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * LINE_SCROLL,
                    MouseScrollDelta::PixelDelta(p) => -p.y as f32,
                };
                let max = self.max_scroll(window.inner_size().height as f32);
                let before = self.scroll;
                self.scroll = (self.scroll + by).clamp(0.0, max);
                if self.scroll != before {
                    window.request_redraw();
                }
            }

            WindowEvent::Resized(_) => window.request_redraw(),

            WindowEvent::RedrawRequested => {
                let size = window.inner_size();
                let (Some(w), Some(h)) = (
                    std::num::NonZeroU32::new(size.width.max(1)),
                    std::num::NonZeroU32::new(size.height.max(1)),
                ) else {
                    return;
                };
                if (size.width as f32 - self.laid_for).abs() > 0.5 {
                    self.reflow(size.width as f32);
                }
                self.scroll = self.scroll.clamp(0.0, self.max_scroll(size.height as f32));

                // While armed, the message lives exactly as long as the bypass
                // does -- so the warning being on screen and the bypass being
                // available are the same fact, rather than two that drift apart.
                let armed = self.armed().is_some();
                let life = if self.armed_at.is_some() { DISCARD_MS } else { NOTE_MS };
                if self.note.as_ref().is_some_and(|(_, at)| at.elapsed().as_millis() > life) {
                    self.note = None;
                    self.armed_at = None;
                }

                let (width, height) = (size.width as usize, size.height as usize);
                let name = self.title();
                let dirty = self.buffer.is_dirty();
                let note = self.note.as_ref().map(|(t, _)| t.clone());
                let scroll = self.scroll;

                let surface = self.surface.as_mut().expect("surface");
                surface.resize(w, h).expect("resize");
                let mut buf = surface.buffer_mut().expect("buffer");
                render::draw(
                    &self.laid, &mut self.text, &mut self.scaled, &mut buf, width, height,
                    scroll, &Theme::DARK,
                );
                render::draw_status(
                    &mut self.text, &mut buf, width, height, BASE, &Theme::DARK, &name, dirty,
                    note.as_deref(), armed,
                );
                buf.present().expect("present");

                if self.timing && !self.reported {
                    self.reported = true;
                    eprintln!("first frame: {:.1} ms", self.t0.elapsed().as_secs_f64() * 1000.0);
                }
            }
            _ => {}
        }
    }
}

/// Print what an install or uninstall did, or why it did not.
fn report(r: std::io::Result<Vec<String>>) {
    match r {
        Ok(lines) => {
            for l in lines {
                println!("  {l}");
            }
        }
        Err(e) => {
            eprintln!("mdblaze: {e}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let t0 = Instant::now();
    let mut path: Option<std::path::PathBuf> = None;
    let mut timing = false;
    let mut once = false;
    let mut shot_to: Option<std::path::PathBuf> = None;
    let mut want_shot = false;
    let mut shot_cursor: Option<usize> = None;
    let mut shot_selection: Option<(usize, usize)> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--timing" => timing = true,
            // Draw one frame and exit. What a measurement harness uses.
            "--once" => {
                timing = true;
                once = true;
            }
            // Render one frame with no window at all and write it out, so a
            // change to layout can be LOOKED at -- in review, or on a machine
            // with no display -- without flashing windows on someone's desktop.
            "--shot" => want_shot = true,
            // Where to put the caret for a shot, as a byte offset. What makes the
            // live-reveal visible in a still image.
            "--at" => shot_cursor = args.next().and_then(|v| v.parse().ok()),
            // Which source bytes to show as selected, as `START:END`. The other
            // half of `--at`: a selection is drawn by the layout, so this is the
            // only way to LOOK at one without a person at the keyboard.
            "--select" => {
                shot_selection = args.next().and_then(|v| {
                    let (a, b) = v.split_once(':')?;
                    Some((a.parse().ok()?, b.parse().ok()?))
                })
            }
            // Register as the handler for markdown, so double-clicking a .md
            // opens this. Separated from opening a file because it changes the
            // machine rather than the document, and because taking over a file
            // type should be something a person asked for by name.
            "--install-handler" => {
                report(desktop::install());
                return;
            }
            "--uninstall-handler" => {
                report(desktop::uninstall());
                return;
            }
            "-h" | "--help" => {
                println!(
                    "mdblaze [--timing] [--once] [--shot out.ppm [--at B] [--select A:B]] <file.md>\n\
                     mdblaze --install-handler     open .md files by double-click\n\
                     mdblaze --uninstall-handler   and give the association back"
                );
                return;
            }
            other if want_shot && shot_to.is_none() => {
                shot_to = Some(std::path::PathBuf::from(other))
            }
            other => path = Some(std::path::PathBuf::from(other)),
        }
    }

    // Read BEFORE the window exists. A file that does not open should say so on
    // the terminal it was launched from rather than flashing a window first.
    let source = match &path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("mdblaze: {}: {e}", p.display());
                std::process::exit(1);
            }
        },
        None => concat!(
            "# mdblaze\n\n",
            "**Ctrl+O** to open a file, or drop one on this window.\n\n",
            "Start typing and **Ctrl+S** will ask where to put it.\n\n",
            "From a terminal: `mdblaze notes.md`\n",
        )
        .to_string(),
    };

    let text = Text::new();
    let buffer = Buffer::from_str(&source);
    // Parsed and laid out before the window opens, so the first frame has
    // something to draw the instant the surface exists rather than a frame later.
    let mut parsed = doc::parse(&source);
    // Pictures are resolved against the DOCUMENT's directory, not the working
    // directory -- a file opened by double-click inherits whatever directory the
    // desktop happened to be in.
    let mut media = Media::for_document(path.as_deref());
    media.attach(&mut parsed);
    let laid = layout::lay_out(&parsed, 900.0, BASE, &text, None);
    if timing {
        eprintln!(
            "read+parse+layout: {:.2} ms ({} blocks, {} runs)",
            t0.elapsed().as_secs_f64() * 1000.0,
            parsed.blocks.len(),
            laid.runs.len()
        );
    }

    if let Some(out) = shot_to {
        let (w, h) = (900usize, 1100usize);
        let mut text2 = Text::new();
        let mut buf = vec![0u32; w * h];
        let editing = shot_cursor.map(|cursor| Editing {
            source: &source,
            cursor,
            // The anchor decides the reveal, exactly as it does when a person is
            // dragging, so a shot shows what they would be looking at.
            reveal: shot_selection.map(|(a, _)| a).unwrap_or(cursor),
            selection: shot_selection,
        });
        let laid = layout::lay_out(&parsed, w as f32, BASE, &text2, editing);
        render::draw(&laid, &mut text2, &mut Scaled::default(), &mut buf, w, h, 0.0, &Theme::DARK);
        render::draw_status(
            &mut text2,
            &mut buf,
            w,
            h,
            BASE,
            &Theme::DARK,
            &path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "untitled".into()),
            false,
            None,
            false,
        );
        // Binary PPM: three bytes a pixel and a one-line header. No encoder, no
        // dependency, and every image tool reads it.
        let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in &buf {
            bytes.push((p >> 16) as u8);
            bytes.push((p >> 8) as u8);
            bytes.push(*p as u8);
        }
        std::fs::write(&out, bytes).expect("write the frame");
        eprintln!("wrote {} ({w}x{h})", out.display());
        return;
    }

    // Where the budget actually goes. Everything above this line is this
    // program's own work and is measured in fractions of a millisecond; almost
    // all of what a person waits for is below it, in the toolkit.
    let t_el = Instant::now();
    // A loop that can carry our own events, so the file chooser -- which runs
    // on its own thread and may take a long time or never answer -- can hand its
    // result back without the window having waited for it.
    let el = EventLoop::<Chosen>::with_user_event().build().expect("no event loop");
    let proxy = el.create_proxy();
    if timing {
        eprintln!("event loop: {:.2} ms", t_el.elapsed().as_secs_f64() * 1000.0);
    }
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        t0,
        started: Instant::now(),
        path,
        buffer,
        text,
        media,
        choosing: None,
        proxy,
        scaled: Scaled::default(),
        laid,
        laid_for: 900.0,
        scroll: 0.0,
        mods: Modifiers::default(),
        pointer: (0.0, 0.0),
        clip: Clip::new(),
        dragging: false,
        note: None,
        armed_at: None,
        timing,
        reported: false,
        window: None,
        surface: None,
    };

    if once {
        struct Once<'a>(&'a mut App);
        impl ApplicationHandler<Chosen> for Once<'_> {
            fn resumed(&mut self, el: &ActiveEventLoop) {
                self.0.resumed(el);
                if let Some(w) = self.0.window.clone() {
                    w.request_redraw();
                }
            }
            fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
                let redraw = matches!(ev, WindowEvent::RedrawRequested);
                self.0.window_event(el, id, ev);
                if redraw {
                    el.exit();
                }
            }
        }
        el.run_app(&mut Once(&mut app)).expect("run");
        return;
    }
    el.run_app(&mut app).expect("run");
}
