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
use mdedit::layout::{self, Laid};
use mdedit::render::{self, Theme};
use mdedit::text::Text;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Pixels per wheel notch, when the platform reports notches rather than pixels.
const LINE_SCROLL: f32 = 48.0;

struct App {
    t0: Instant,
    path: Option<std::path::PathBuf>,
    source: String,
    text: Text,
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
            None => "markdown".to_string(),
        }
    }

    /// Re-flow for a new width. Cheap enough to do on every resize event.
    fn relayout(&mut self, width: f32) {
        if (width - self.laid_for).abs() < 0.5 {
            return;
        }
        let parsed = doc::parse(&self.source);
        self.laid = layout::lay_out(&parsed, width, 16.0, &self.text);
        self.laid_for = width;
    }

    fn max_scroll(&self, height: f32) -> f32 {
        (self.laid.height - height).max(0.0)
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

            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let size = window.inner_size();
                let page = size.height as f32 * 0.9;
                let max = self.max_scroll(size.height as f32);
                let before = self.scroll;
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => el.exit(),
                    Key::Named(NamedKey::ArrowDown) => self.scroll += LINE_SCROLL,
                    Key::Named(NamedKey::ArrowUp) => self.scroll -= LINE_SCROLL,
                    Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) => {
                        self.scroll += page
                    }
                    Key::Named(NamedKey::PageUp) => self.scroll -= page,
                    Key::Named(NamedKey::Home) => self.scroll = 0.0,
                    Key::Named(NamedKey::End) => self.scroll = max,
                    _ => {}
                }
                self.scroll = self.scroll.clamp(0.0, max);
                if self.scroll != before {
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
                self.relayout(size.width as f32);
                // Clamped after a re-flow: a window made wider is shorter in
                // total, and a scroll position from before it can be past the end.
                self.scroll = self.scroll.clamp(0.0, self.max_scroll(size.height as f32));

                let Some(surface) = self.surface.as_mut() else {
                    return;
                };
                surface.resize(w, h).expect("resize");
                let mut buf = surface.buffer_mut().expect("buffer");
                render::draw(
                    &self.laid,
                    &mut self.text,
                    &mut buf,
                    size.width as usize,
                    size.height as usize,
                    self.scroll,
                    &Theme::DARK,
                );
                buf.present().expect("present");

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
            "-h" | "--help" => {
                println!("mdedit [--timing] [--once] [--shot out.ppm] <file.md>");
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
        let laid = layout::lay_out(&doc::parse(&source), w as f32, 16.0, &text2);
        let mut buf = vec![0u32; w * h];
        render::draw(&laid, &mut text2, &mut buf, w, h, 0.0, &Theme::DARK);
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
        source,
        text,
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
