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

use mdedit::desktop;
use mdedit::doc;
use mdedit::edit::Buffer;
use mdedit::file;
use mdedit::layout::{self, Editing, Laid};
use mdedit::media::Media;
use mdedit::render::{self, Scaled, Theme};
use mdedit::text::Text;

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

const BASE: f32 = mdedit::text::BODY_PX;

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
        self.laid = layout::lay_out(
            &parsed,
            width,
            BASE,
            &self.text,
            Some(Editing { source: &source, cursor }),
        );
        self.laid_for = width;
    }

    fn ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    fn say(&mut self, what: &str) {
        self.note = Some((what.to_string(), Instant::now()));
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.say("no filename — open a file to save it");
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
        let Some(c) = self.laid.caret else { return };
        let view = self.viewport(height);
        if c.top < self.scroll {
            self.scroll = c.top;
        } else if c.top + c.height > self.scroll + view {
            self.scroll = c.top + c.height - view;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(height));
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(self.title())
            // Wide enough that a 79-column code block -- PEP 8's limit, and what
            // most code is written to -- fits without being clipped. That needs
            // about 683px; the rest is margin so the window is not painted into
            // a corner. Prose still stops at its own, narrower measure.
            .with_inner_size(winit::dpi::LogicalSize::new(940.0, 820.0));
        let w = Arc::new(el.create_window(attrs).expect("could not open a window"));
        // The window is a document from edge to edge, so the pointer says so
        // everywhere rather than changing shape over text.
        w.set_cursor(CursorIcon::Text);
        let ctx = softbuffer::Context::new(w.clone()).expect("no drawing context");
        let surface = softbuffer::Surface::new(&ctx, w.clone()).expect("no drawing surface");
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
            WindowEvent::ModifiersChanged(m) => self.mods = m,

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let size = window.inner_size();
                let ctrl = self.mods.state().control_key();
                let shift = self.mods.state().shift_key();
                let now = self.ms();
                let page = self.viewport(size.height as f32) * 0.9;
                let escape = matches!(event.logical_key, Key::Named(NamedKey::Escape));
                // Whether the caret moved or the text changed: either way the
                // rendering differs, because a block reveals and hides with the
                // caret.
                let mut touched = true;

                match &event.logical_key {
                    Key::Character(c) if ctrl && c.eq_ignore_ascii_case("s") => {
                        self.save();
                        touched = false;
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
                    self.buffer.set_byte_offset(byte);
                    // The block under the caret changed, so what is revealed did
                    // too -- the click changes the picture even though it changed
                    // no text.
                    self.reflow(size.width as f32);
                    window.request_redraw();
                }
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
            eprintln!("mdedit: {e}");
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
                    "mdedit [--timing] [--once] [--shot out.ppm [--at BYTE]] <file.md>\n\
                     mdedit --install-handler     open .md files by double-click\n\
                     mdedit --uninstall-handler   and give the association back"
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
                eprintln!("mdedit: {}: {e}", p.display());
                std::process::exit(1);
            }
        },
        None => "# mdedit\n\nPass a markdown file to open it.\n".to_string(),
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
        let editing = shot_cursor.map(|cursor| Editing { source: &source, cursor });
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

    let el = EventLoop::new().expect("no event loop");
    el.set_control_flow(ControlFlow::Wait);
    let mut app = App {
        t0,
        started: Instant::now(),
        path,
        buffer,
        text,
        media,
        scaled: Scaled::default(),
        laid,
        laid_for: 900.0,
        scroll: 0.0,
        mods: Modifiers::default(),
        pointer: (0.0, 0.0),
        note: None,
        armed_at: None,
        timing,
        reported: false,
        window: None,
        surface: None,
    };

    if once {
        struct Once<'a>(&'a mut App);
        impl ApplicationHandler for Once<'_> {
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
