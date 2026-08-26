//! The macOS half: an application bundle, and LaunchServices.
//!
//! # Why a bundle at all
//!
//! macOS has no notion of a bare executable that opens a file type. Everything
//! LaunchServices knows about is a bundle -- a directory with a particular shape
//! and an `Info.plist` inside it declaring what the thing is and what it can
//! open. A binary in `~/.cargo/bin` is invisible to the Finder no matter what it
//! can do.
//!
//! So `--install-handler` builds one: `~/Applications/mdblaze.app`, with a plist
//! that claims markdown, and a two-line shell script that hands off to wherever
//! the real binary lives.
//!
//! # Why a trampoline rather than a copy
//!
//! The obvious move is to copy the executable into `Contents/MacOS`. It is also
//! wrong for how this program is installed: `cargo install mdblaze` replaces the
//! binary in place, and a copy made at install time would go stale on the first
//! upgrade and keep opening last month's version for ever, silently.
//!
//! A script that `exec`s the real path stays correct across upgrades. It costs a
//! `/bin/sh` exec on launch, which is well under a millisecond and does not move
//! a budget whose limit is elsewhere entirely.
//!
//! # What this does NOT do
//!
//! It does not make itself the DEFAULT. Setting the default handler for a
//! content type on macOS means `LSSetDefaultRoleHandlerForContentType`, which is
//! a CoreServices call requiring an Objective-C runtime dependency, and which
//! recent macOS may prompt about or refuse outright.
//!
//! Registering the bundle puts this program in the Finder's "Open With" list,
//! which is the honest half that works. Making it the default is one gesture in
//! Get Info, and the report says so rather than leaving somebody to wonder why
//! double-click did not change.

// `FORMER_NAMES` is referenced through `super::` at its two use sites rather
// than imported, because both of them are inside `#[cfg(target_os = "macos")]`.
// Imported, it reads as unused on every other platform -- and `cargo clippy
// --fix` duly deleted it, which compiled cleanly on Linux and broke the macOS
// build. The cross-target check caught it; nothing else would have.
use super::{DESCRIPTION, EXTENSIONS, NAME};
use std::path::{Path, PathBuf};

/// The Uniform Type Identifier for markdown.
///
/// Not an invention: this is the identifier the wider ecosystem settled on, and
/// declaring a different one would mean this program and every other markdown
/// application disagree about what a `.md` file is.
pub const UTI: &str = "net.daringfireball.markdown";

/// `Info.plist` for a bundle whose executable is called `exec_name`.
///
/// A pure function returning the exact bytes that get written, which is what
/// makes the interesting half of this testable on a machine that is not a Mac.
///
/// `LSMinimumSystemVersion` is deliberately low: nothing here uses a recent API,
/// and a version floor invented for the sake of it locks people out of a text
/// editor for no reason.
pub fn plist(exec_name: &str) -> String {
    let extensions: String = EXTENSIONS
        .iter()
        .map(|e| format!("                    <string>{e}</string>\n"))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20   <key>CFBundleIdentifier</key>\n\
         \x20   <string>com.redstonelogic.{NAME}</string>\n\
         \x20   <key>CFBundleName</key>\n\
         \x20   <string>{NAME}</string>\n\
         \x20   <key>CFBundleDisplayName</key>\n\
         \x20   <string>{NAME}</string>\n\
         \x20   <key>CFBundleExecutable</key>\n\
         \x20   <string>{exec_name}</string>\n\
         \x20   <key>CFBundlePackageType</key>\n\
         \x20   <string>APPL</string>\n\
         \x20   <key>CFBundleInfoDictionaryVersion</key>\n\
         \x20   <string>6.0</string>\n\
         \x20   <key>CFBundleShortVersionString</key>\n\
         \x20   <string>{version}</string>\n\
         \x20   <key>CFBundleVersion</key>\n\
         \x20   <string>{version}</string>\n\
         \x20   <key>LSMinimumSystemVersion</key>\n\
         \x20   <string>10.13</string>\n\
         \x20   <key>NSHighResolutionCapable</key>\n\
         \x20   <true/>\n\
         \x20   <key>CFBundleDocumentTypes</key>\n\
         \x20   <array>\n\
         \x20       <dict>\n\
         \x20           <key>CFBundleTypeName</key>\n\
         \x20           <string>{DESCRIPTION}</string>\n\
         \x20           <key>CFBundleTypeRole</key>\n\
         \x20           <string>Editor</string>\n\
         \x20           <key>LSHandlerRank</key>\n\
         \x20           <string>Alternate</string>\n\
         \x20           <key>LSItemContentTypes</key>\n\
         \x20           <array>\n\
         \x20               <string>{UTI}</string>\n\
         \x20           </array>\n\
         \x20           <key>CFBundleTypeExtensions</key>\n\
         \x20           <array>\n\
         {extensions}\
         \x20           </array>\n\
         \x20       </dict>\n\
         \x20   </array>\n\
         </dict>\n\
         </plist>\n",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// The script placed at `Contents/MacOS/<name>`, handing off to the real binary.
///
/// `exec` rather than a call, so no shell process is left hanging around for the
/// life of the editor. `"$@"` quoted, so a path with a space in it arrives as one
/// argument -- which is most paths on a Mac, where `~/Documents/My Notes` is
/// ordinary.
pub fn trampoline(real: &Path) -> String {
    format!("#!/bin/sh\nexec {} \"$@\"\n", shell_quote(&real.to_string_lossy()))
}

/// Single-quote a path for `/bin/sh`, the only quoting that needs no exceptions.
///
/// Inside single quotes every character is literal, so the one thing to handle
/// is a single quote itself: close, emit an escaped quote, reopen.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Where the bundle for a given name lives.
pub fn bundle_path(name: &str) -> PathBuf {
    super::home().join("Applications").join(format!("{name}.app"))
}

#[cfg(target_os = "macos")]
pub(super) fn install() -> std::io::Result<Vec<String>> {
    use std::os::unix::fs::PermissionsExt;

    let exe = std::env::current_exe()?;
    let mut said = Vec::new();

    // A former name's bundle first: two bundles claiming markdown is exactly the
    // ambiguity the rename was supposed to remove.
    for former in super::FORMER_NAMES {
        let old = bundle_path(former);
        if old.exists() {
            std::fs::remove_dir_all(&old)?;
            said.push(format!("removed the former {former} bundle at {}", old.display()));
        }
    }

    let app = bundle_path(NAME);
    let macos_dir = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos_dir)?;
    std::fs::write(app.join("Contents/Info.plist"), plist(NAME))?;

    let launcher = macos_dir.join(NAME);
    std::fs::write(&launcher, trampoline(&exe))?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    said.push(format!("wrote {}", app.display()));
    said.push(format!("it runs {}", exe.display()));

    // Tell LaunchServices the bundle exists. Without this it is found whenever
    // the system next rescans, which is not a promise anybody should wait on.
    const LSREGISTER: &str =
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    match std::process::Command::new(LSREGISTER).arg("-f").arg(&app).status() {
        Ok(s) if s.success() => said.push("registered it with LaunchServices".into()),
        _ => said.push("could not run lsregister; it will be found on the next rescan".into()),
    }

    said.push(
        "macOS will not let a program make itself the default opener. \
         mdblaze is now in the Finder's \"Open With\" list; to make it the \
         default, Get Info on any .md file, choose it, and click Change All."
            .into(),
    );
    Ok(said)
}

#[cfg(target_os = "macos")]
pub(super) fn uninstall() -> std::io::Result<Vec<String>> {
    let mut said = Vec::new();
    for name in super::FORMER_NAMES.iter().copied().chain([NAME]) {
        let app = bundle_path(name);
        if app.exists() {
            std::fs::remove_dir_all(&app)?;
            said.push(format!("removed {}", app.display()));
        }
    }
    said.push("whatever opened markdown before is the default again".into());
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_declares_both_extensions_and_the_shared_type_identifier() {
        let p = plist("mdblaze");
        for e in EXTENSIONS {
            assert!(p.contains(&format!("<string>{e}</string>")), "{e} missing from {p}");
        }
        // Not an invented identifier: disagreeing with every other markdown
        // application about what a .md file IS would be worse than not claiming
        // it at all.
        assert!(p.contains("net.daringfireball.markdown"));
    }

    #[test]
    fn the_plist_names_the_executable_the_bundle_actually_contains() {
        // `CFBundleExecutable` names a file inside `Contents/MacOS`. If the two
        // disagree the bundle is inert and the Finder gives no reason.
        assert!(plist("mdblaze").contains("<key>CFBundleExecutable</key>\n    <string>mdblaze</string>"));
    }

    #[test]
    fn the_plist_carries_the_crate_version_rather_than_a_hardcoded_one() {
        // A version frozen in a string literal is a version that is wrong from
        // the next release onwards.
        assert!(plist("x").contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_plist_is_a_complete_document() {
        let p = plist("x");
        assert!(p.starts_with("<?xml"));
        assert!(p.trim_end().ends_with("</plist>"));
        // Every key has a value after it: an odd count means one is dangling,
        // which makes the whole plist unreadable and the bundle invisible.
        assert_eq!(p.matches("<dict>").count(), p.matches("</dict>").count());
        assert_eq!(p.matches("<array>").count(), p.matches("</array>").count());
    }

    #[test]
    fn the_trampoline_execs_rather_than_calls() {
        // A call would leave a shell sitting around for the life of the editor,
        // doing nothing, for every window opened.
        let t = trampoline(Path::new("/usr/local/bin/mdblaze"));
        assert!(t.starts_with("#!/bin/sh\n"));
        assert!(t.contains("exec "), "{t}");
    }

    #[test]
    fn a_path_with_a_space_survives_the_trampoline() {
        // `~/Documents/My Notes` is an ordinary path on a Mac, and an unquoted
        // one arrives as two arguments and opens nothing.
        let t = trampoline(Path::new("/Users/someone/Applications/my tools/mdblaze"));
        assert!(t.contains("'/Users/someone/Applications/my tools/mdblaze'"), "{t}");
        assert!(t.contains("\"$@\""), "arguments must be passed as one each");
    }

    #[test]
    fn a_path_with_a_quote_in_it_cannot_break_out_of_the_script() {
        // Not a realistic filename, but the escaping either holds or it does
        // not, and "unlikely" is not a security property.
        let t = trampoline(Path::new("/tmp/it's here/mdblaze"));
        assert!(t.contains("'/tmp/it'\\''s here/mdblaze'"), "{t}");
    }

    #[test]
    fn the_bundle_goes_in_the_users_applications_folder() {
        // Compared as PATH COMPONENTS rather than as a string: `join` uses a
        // backslash on Windows, and this module's tests run there too.
        let p = bundle_path("mdblaze");
        let tail: Vec<_> = p.components().rev().take(2).collect();
        assert_eq!(tail[0].as_os_str(), "mdblaze.app");
        assert_eq!(tail[1].as_os_str(), "Applications");
    }
}
