//! Registering as the handler for markdown files, and unregistering again.
//!
//! Double-clicking a `.md` file is the whole point of a fast editor: the speed
//! only matters if opening a file is one gesture.
//!
//! # Three platforms, three completely different mechanisms
//!
//! There is no portable way to say "I open this kind of file". Linux writes a
//! `.desktop` entry and a line in `mimeapps.list`; macOS declares document types
//! inside an application bundle and tells LaunchServices about it; Windows
//! writes a ProgId and an extension association into the registry. They share
//! nothing -- not the format, not the location, not even the idea of what a file
//! type IS. So each gets its own module rather than an abstraction that would
//! have to be wrong about two of them to be right about one.
//!
//! # Why the logic is in pure functions
//!
//! This is written on Linux. The macOS and Windows paths compile -- checked
//! against both targets -- but they have never been RUN, and code that has never
//! been run is a guess.
//!
//! So the guessing is confined to as little as possible. What a plist SAYS and
//! which registry values get written are pure functions returning strings and
//! lists, and those are tested here, on this machine, on every platform. What is
//! left untested is the thin layer that writes them down, which is `fs::write`
//! and one registry call.
//!
//! That is not the same as tested, and the README does not claim it is.
//!
//! # Why uninstall records what it replaced
//!
//! Installing this takes over a file type the person already had an opinion
//! about -- on the machine this was written on, GNOME Text Editor. A tool that
//! seizes a file association and cannot give it back is a tool people are right
//! to be wary of, so the previous default is written down before it is replaced
//! and put back on uninstall.

pub mod linux;
pub mod macos;
pub mod windows;

/// What this program is called, wherever a platform wants a name.
pub const NAME: &str = "mdblaze";

/// How the file type is described to a person, in a menu or a properties panel.
pub const DESCRIPTION: &str = "Markdown Document";

/// The extensions claimed, without their dots.
///
/// `markdown` as well as `md` because both are in common use and claiming one
/// silently does nothing for half the files people have.
pub const EXTENSIONS: &[&str] = &["md", "markdown"];

/// Names this program has shipped under before.
///
/// A rename is not just a string change once the program has installed itself on
/// somebody's machine. The old name left files behind and -- worst of all -- a
/// line saying that markdown belongs to it. Install the new name without
/// touching any of that and the desktop has two handlers for one file type, one
/// of them pointing at a binary that may not exist any more, and which one wins
/// is up to the desktop.
///
/// So installing sweeps the former names out. The list only ever grows.
pub const FORMER_NAMES: &[&str] = &["mdedit"];

/// Install the handler and make it the default, as far as the platform allows.
///
/// The report is returned rather than printed so the caller decides where it
/// goes, and so a test can read it.
pub fn install() -> std::io::Result<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        linux::install()
    }
    #[cfg(target_os = "macos")]
    {
        macos::install()
    }
    #[cfg(target_os = "windows")]
    {
        windows::install()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(unsupported())
    }
}

/// Remove the handler and restore whatever it displaced.
pub fn uninstall() -> std::io::Result<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        linux::uninstall()
    }
    #[cfg(target_os = "macos")]
    {
        macos::uninstall()
    }
    #[cfg(target_os = "windows")]
    {
        windows::uninstall()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        Err(unsupported())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn unsupported() -> std::io::Error {
    std::io::Error::other(
        "no idea how to register a file handler on this platform, and doing \
         nothing quietly would be worse than saying so",
    )
}

/// The user's home directory.
///
/// `HOME` everywhere, and `USERPROFILE` as well on Windows, which is where it
/// actually lives there. Falling back to `.` rather than `/` because a relative
/// path that is wrong is obvious immediately, whereas writing into the root of
/// the filesystem is wrong quietly.
pub(crate) fn home() -> std::path::PathBuf {
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("USERPROFILE") {
        return std::path::PathBuf::from(p);
    }
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| ".".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_name_is_not_in_the_list_of_former_ones() {
        // It would sweep its own files away in the middle of installing.
        assert!(!FORMER_NAMES.contains(&NAME));
    }

    #[test]
    fn both_spellings_of_the_extension_are_claimed() {
        // Claiming only `md` silently does nothing for half the files people
        // have, and the failure looks like the program being broken.
        assert!(EXTENSIONS.contains(&"md"));
        assert!(EXTENSIONS.contains(&"markdown"));
        assert!(EXTENSIONS.iter().all(|e| !e.starts_with('.')), "the dot is added per platform");
    }
}
