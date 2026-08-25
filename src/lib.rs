//! A markdown editor that renders, and opens instantly.
//!
//! # What this is for
//!
//! Nothing occupies the space between "renders markdown" and "opens now". A
//! browser or a text editor shows you the source; Obsidian shows you the
//! document but pays an Electron start and a vault index before it can show you
//! anything at all. Opening one file to read it should not cost either.
//!
//! # The shape that follows from that
//!
//! Speed is the feature, so it decides the architecture rather than being tuned
//! for afterwards:
//!
//!   - **No document tree.** [`doc::parse`] produces a flat list of blocks with
//!     a depth number. Laying out is then one ordered walk.
//!   - **No HTML.** The console renders markdown to HTML because a browser
//!     consumes it there. Here the consumer is a rasteriser.
//!   - **No font system.** The face is compiled in. Asking the OS what fonts
//!     exist is the single most reliable way to lose the entire budget, and it
//!     is a question this program never needs answered.
//!   - **No GPU.** Creating a graphics context costs more than everything else
//!     here put together, to draw static text.
//!   - **No index, no vault, no workspace.** One file, opened.
//!
//! Measured on the machine this was written on: parsing 36KB of markdown takes
//! 0.35ms and putting a window on screen with raw X11 takes 0.2ms. The budget
//! is not spent on the work; it is spent on the toolkit, which is why the
//! windowing lives behind its own edge and can be replaced without touching
//! anything here.

pub mod doc;
pub mod edit;
pub mod file;
pub mod layout;
pub mod render;
pub mod text;
