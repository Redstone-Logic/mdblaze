//! Open a markdown file and read it.
//!
//! The whole program: read a file, parse it, put it on screen. No vault, no
//! index, no workspace, no project. Those are what make an editor slow to open,
//! and opening is the thing this is for.
//!
//! # Where the time goes
//!
//! Measured on the machine this was written on, for a 36KB document:
//!
//! ```text
//!   read the file        0.08 ms
//!   parse it             0.35 ms
//!   lay it out            ~1 ms
//!   window on screen    ~35 ms   <- winit
//! ```
//!
//! Our half is free; the toolkit is the budget. Raw X11 puts a window up in
//! 0.2ms, so most of winit's cost is work a first frame does not need -- keyboard
//! layout compilation, cursor themes, monitor enumeration. Claiming it back means
//! a backend per platform, which is more work than this program, so it stays
//! behind `--timing` as a number rather than a plan.

use std::sync::Arc;
use std::time::Instant;

use mdedit::doc;
use mdedit::edit::Buffer;
use mdedit::file;
use mdedit::layout::{self, Laid};
use mdedit::render::{self, Theme};
use mdedit::text::Text;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, Modifiers, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Pixels per wheel notch, when the platform reports notches rather than pixels.
const LINE_SCROLL: f32 = 48.0;

/// How long a status message stays up before the hint returns.
const NOTE_MS: u128 = 2_500;

/// Reading, or changing.
///
/// Two modes rather than editing the rendered view directly. Mapping a cursor
/// between rendered text and the markdown behind it is the hard half of a
/// WYSIWYG editor, and getting it subtly wrong puts someone's characters
/// somewhere they did not ask for. A key to switch is cheap and never lies.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Read,
    Edit,
}

struct App {
    t0: Instant,
    path: Option<std::path::PathBuf>,
    buffer: Buffer,
    text: Text,
    mode: Mode,
    mods: Modifiers,
    note: Option<(String, Instant)>,
    /// Set when Escape was pressed with unsaved changes. A second press closes.
    confirm_discard: bool,
    started: Instant,
    laid: Laid,
    /// The width the layout was computed for, so a resize that does not change
    /// the width does not redo it.
    laid_for: f32,
    scroll: f32,
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

    /// Re-flow for a new width. Cheap enough to do on every resize event.
    fn relayout(&mut self, width: f32) {
        if (width - self.laid_for).abs() < 0.5 {
            return;
        }
        self.reflow(width);
    }

    /// Re-parse and re-lay out unconditionally. Only on leaving edit mode: doing
    /// it per keystroke would parse the whole document on every character, which
    /// is affordable but pointless while the source is what is on screen.
    fn reflow(&mut self, width: f32) {
        let parsed = doc::parse(&self.buffer.text());
        self.laid = layout::lay_out(&parsed, width, 16.0, &self.text);
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
                self.say("saved");
            }
            // Never silent. A save that failed and said nothing is how work is
            // lost while someone believes it is safe.
            Err(e) => self.say(&format!("could not save: {e}")),
        }
    }

    fn max_scroll(&self, height: f32) -> f32 {
        let view = (height - render::status_height(16.0)).max(1.0);
        match self.mode {
            Mode::Read => (self.laid.height - view).max(0.0),
            Mode::Edit => {
                let lh = self.text.line_height_with(mdedit::text::Face::Mono, 16.0 * 0.95, mdedit::text::CODE_LEADING);
                (layout::PAD * 2.0 + self.buffer.lines().len() as f32 * lh - view).max(0.0)
            }
        }
    }

    /// Keep the caret on screen after a movement or an edit.
    fn follow_caret(&mut self, height: f32, caret_top: f32, lh: f32) {
        let view = (height - render::status_height(16.0)).max(1.0);
        if caret_top < self.scroll {
            self.scroll = caret_top;
        } else if caret_top + lh > self.scroll + view {
            self.scroll = caret_top + lh - view;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(self.title())
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 760.0));
        let w = Arc::new(el.create_window(attrs).expect("could not open a window"));
        let ctx = softbuffer::Context::new(w.clone()).expect("no drawing context");
        let surface = softbuffer::Surface::new(&ctx, w.clone()).expect("no drawing surface");
        self.window = Some(w);
        self.surface = Some(surface);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        // Only the window is taken here. The surface is borrowed inside the
        // redraw arm, because holding it across the whole match would borrow
        // `self` mutably for the duration and lock out relayout().
        let Some(window) = self.window.clone() else {
            return;
        };
        match ev {
            WindowEvent::CloseRequested => el.exit(),

            WindowEvent::ModifiersChanged(m) => self.mods = m,

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let size = window.inner_size();
                let page = size.height as f32 * 0.9;
                let ctrl = self.mods.state().control_key();
                let shift = self.mods.state().shift_key();
                let now = self.ms();

                match (&event.logical_key, self.mode) {
                    // ---- always ------------------------------------------
                    (Key::Character(c), _) if ctrl && c.eq_ignore_ascii_case("s") => {
                        self.save();
                    }

                    // ---- reading -----------------------------------------
                    (Key::Character(c), Mode::Read) if c.eq_ignore_ascii_case("e") && !ctrl => {
                        self.mode = Mode::Edit;
                        self.scroll = 0.0;
                        self.confirm_discard = false;
                    }
                    (Key::Named(NamedKey::Escape), Mode::Read) => {
                        // Unsaved work is not discarded on one keypress. The
                        // second press is the person saying they meant it.
                        if self.buffer.is_dirty() && !self.confirm_discard {
                            self.confirm_discard = true;
                            self.say("unsaved — Ctrl+S to save, Esc again to discard");
                        } else {
                            el.exit();
                        }
                    }
                    (Key::Named(NamedKey::ArrowDown), Mode::Read) => self.scroll += LINE_SCROLL,
                    (Key::Named(NamedKey::ArrowUp), Mode::Read) => self.scroll -= LINE_SCROLL,
                    (Key::Named(NamedKey::PageDown), Mode::Read)
                    | (Key::Named(NamedKey::Space), Mode::Read) => self.scroll += page,
                    (Key::Named(NamedKey::PageUp), Mode::Read) => self.scroll -= page,
                    (Key::Named(NamedKey::Home), Mode::Read) => self.scroll = 0.0,
                    (Key::Named(NamedKey::End), Mode::Read) => {
                        self.scroll = self.max_scroll(size.height as f32)
                    }

                    // ---- editing -----------------------------------------
                    (Key::Named(NamedKey::Escape), Mode::Edit) => {
                        self.mode = Mode::Read;
                        self.reflow(size.width as f32);
                        self.scroll = 0.0;
                    }
                    (Key::Character(c), Mode::Edit) if ctrl && c.eq_ignore_ascii_case("z") => {
                        // Ctrl+Shift+Z redoes, which is the other convention and
                        // costs nothing to also accept.
                        let moved = if shift { self.buffer.redo() } else { self.buffer.undo() };
                        if !moved {
                            self.say(if shift { "nothing to redo" } else { "nothing to undo" });
                        }
                    }
                    (Key::Character(c), Mode::Edit) if ctrl && c.eq_ignore_ascii_case("y") => {
                        if !self.buffer.redo() {
                            self.say("nothing to redo");
                        }
                    }
                    (Key::Named(NamedKey::Enter), Mode::Edit) => self.buffer.insert_newline(now),
                    (Key::Named(NamedKey::Backspace), Mode::Edit) => self.buffer.backspace(now),
                    (Key::Named(NamedKey::Delete), Mode::Edit) => self.buffer.delete(now),
                    (Key::Named(NamedKey::Tab), Mode::Edit) => {
                        // Two spaces, not a tab character: markdown's nesting is
                        // defined in spaces and a literal tab renders differently
                        // in every tool that reads it afterwards.
                        self.buffer.insert_char(' ', now);
                        self.buffer.insert_char(' ', now);
                    }
                    (Key::Named(NamedKey::ArrowLeft), Mode::Edit) => self.buffer.left(),
                    (Key::Named(NamedKey::ArrowRight), Mode::Edit) => self.buffer.right(),
                    (Key::Named(NamedKey::ArrowUp), Mode::Edit) => self.buffer.up(),
                    (Key::Named(NamedKey::ArrowDown), Mode::Edit) => self.buffer.down(),
                    (Key::Named(NamedKey::Home), Mode::Edit) => self.buffer.home(),
                    (Key::Named(NamedKey::End), Mode::Edit) => self.buffer.end(),
                    (Key::Named(NamedKey::Space), Mode::Edit) => self.buffer.insert_char(' ', now),
                    (Key::Character(c), Mode::Edit) if !ctrl => {
                        // What the layout produced, so an accented or composed
                        // character arrives as itself rather than as the key that
                        // happened to be under the finger.
                        for ch in c.chars() {
                            self.buffer.insert_char(ch, now);
                        }
                    }
                    _ => {}
                }

                if self.mode == Mode::Read {
                    self.scroll = self.scroll.clamp(0.0, self.max_scroll(size.height as f32));
                }
                // Any keypress that was not the second Escape means they did not
                // mean to discard after all.
                if !matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
                    self.confirm_discard = false;
                }
                window.request_redraw();
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
                self.relayout(size.width as f32);
                // Clamped after a re-flow: a window made wider is shorter in
                // total, and a scroll position from before it can be past the end.
                self.scroll = self.scroll.clamp(0.0, self.max_scroll(size.height as f32));

                // A note that has had its moment gives the hint back.
                if self.note.as_ref().is_some_and(|(_, at)| at.elapsed().as_millis() > NOTE_MS) {
                    self.note = None;
                }

                let (width, height) = (size.width as usize, size.height as usize);
                let mode = self.mode;
                let scroll = self.scroll;
                let name = self.title();
                let dirty = self.buffer.is_dirty();
                let note = self.note.as_ref().map(|(t, _)| t.clone());

                let mut caret: Option<(f32, f32)> = None;
                {
                    let surface = self.surface.as_mut().expect("surface");
                    surface.resize(w, h).expect("resize");
                    let mut buf = surface.buffer_mut().expect("buffer");
                    match mode {
                        Mode::Read => render::draw(
                            &self.laid, &mut self.text, &mut buf, width, height, scroll, &Theme::DARK,
                        ),
                        Mode::Edit => {
                            caret = Some(render::draw_source(
                                self.buffer.lines(),
                                (self.buffer.line, self.buffer.col),
                                &mut self.text,
                                &mut buf,
                                width,
                                height,
                                scroll,
                                16.0,
                                &Theme::DARK,
                            ));
                        }
                    }
                    render::draw_status(
                        &mut self.text, &mut buf, width, height, 16.0, &Theme::DARK,
                        &name, dirty, mode == Mode::Edit, note.as_deref(),
                    );
                    buf.present().expect("present");
                }

                // Scrolling to the caret needs the layout the frame just used, so
                // it happens after drawing and asks for one more frame only when
                // the view actually has to move.
                if let Some((top, lh)) = caret {
                    let before = self.scroll;
                    self.follow_caret(size.height as f32, top, lh);
                    self.scroll = self.scroll.clamp(0.0, self.max_scroll(size.height as f32));
                    if (self.scroll - before).abs() > 0.5 {
                        window.request_redraw();
                    }
                }

                if self.timing && !self.reported {
                    self.reported = true;
                    eprintln!(
                        "first frame: {:.1} ms",
                        self.t0.elapsed().as_secs_f64() * 1000.0
                    );
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let t0 = Instant::now();
    let mut args = std::env::args().skip(1);
    let mut path: Option<std::path::PathBuf> = None;
    let mut timing = false;
    let mut once = false;
    let mut shot = false;
    let mut shot_to: Option<std::path::PathBuf> = None;
    let mut start_editing = false;
    for a in args.by_ref() {
        match a.as_str() {
            "--timing" => timing = true,
            // Draw one frame and exit. What a measurement harness uses, and what
            // proves the pipeline works without a human to close the window.
            "--once" => {
                timing = true;
                once = true;
            }
            // Render one frame with no window at all and write it out. The
            // whole pipeline runs; only the compositor is absent. It exists so a
            // change to layout or rasterising can be LOOKED at -- by a person or
            // in review -- without a display, and so measuring does not require
            // flashing windows on someone's desktop.
            "--shot" => shot = true,
            // Start in edit mode. Also what `--shot --edit` renders.
            "--edit" => start_editing = true,
            "-h" | "--help" => {
                println!("mdedit [--timing] [--once] [--edit] [--shot out.ppm] <file.md>");
                return;
            }
            other if shot && shot_to.is_none() => {
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
        None => "# mdedit\n\nPass a markdown file to read it.\n".to_string(),
    };

    // Parsed and laid out before the window opens, so the first frame has
    // something to draw the instant the surface exists rather than a frame later.
    let t_read = t0.elapsed();
    let text = Text::new();
    let t_fonts = t0.elapsed();
    let parsed = doc::parse(&source);
    let t_parse = t0.elapsed();
    let laid = layout::lay_out(&parsed, 900.0, 16.0, &text);
    if timing {
        eprintln!(
            "read {:.2} | fonts {:.2} | parse {:.2} | layout {:.2} | total {:.2} ms ({} blocks, {} runs)",
            t_read.as_secs_f64() * 1000.0,
            (t_fonts - t_read).as_secs_f64() * 1000.0,
            (t_parse - t_fonts).as_secs_f64() * 1000.0,
            (t0.elapsed() - t_parse).as_secs_f64() * 1000.0,
            t0.elapsed().as_secs_f64() * 1000.0,
            parsed.blocks.len(),
            laid.runs.len()
        );
    }

    if let Some(out) = shot_to {
        let (w, h) = (900usize, 1100usize);
        let mut text2 = Text::new();
        let mut buf = vec![0u32; w * h];
        let buffer = Buffer::from_str(&source);
        if start_editing {
            render::draw_source(
                buffer.lines(), (2, 6), &mut text2, &mut buf, w, h, 0.0, 16.0, &Theme::DARK,
            );
        } else {
            let laid = layout::lay_out(&doc::parse(&source), w as f32, 16.0, &text2);
            render::draw(&laid, &mut text2, &mut buf, w, h, 0.0, &Theme::DARK);
        }
        render::draw_status(
            &mut text2, &mut buf, w, h, 16.0, &Theme::DARK,
            &path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "untitled".into()),
            start_editing, start_editing, None,
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
        path,
        buffer: Buffer::from_str(&source),
        text,
        mode: if start_editing { Mode::Edit } else { Mode::Read },
        mods: Modifiers::default(),
        note: None,
        confirm_discard: false,
        started: Instant::now(),
        laid,
        laid_for: 900.0,
        scroll: 0.0,
        timing,
        reported: false,
        window: None,
        surface: None,
    };
    if once {
        // One frame, then out. Runs the whole path including present().
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
