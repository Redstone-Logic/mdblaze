//! Writing a document back, without a window in which to lose it.
//!
//! # Why not just write the file
//!
//! `fs::write` truncates first and then writes. If anything goes wrong in
//! between -- the disk fills, the process is killed, the machine loses power --
//! the file on disk is the truncated one. The document the person had is gone,
//! and it is gone at the exact moment they asked for it to be kept.
//!
//! So: write a sibling temp file, flush it to the platter, and `rename` over the
//! original. Rename is atomic within a filesystem, so a reader sees either the
//! whole old file or the whole new one and never a half-written one.
//!
//! This is the same commit protocol the org server's file store uses. It is
//! restated here rather than shared because they have no code in common, and
//! because it is short enough that a copy is cheaper than a dependency between
//! two products that otherwise never touch.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A file's permission bits, on systems that have them.
///
/// Split out and gated rather than written inline, because `PermissionsExt` does
/// not exist on Windows and its absence was the ONLY thing stopping this crate
/// compiling there -- five errors, all in this file, in a program that is
/// otherwise portable.
///
/// Windows has no mode to carry across. Its access control lives in an ACL,
/// which a rename does not copy from the file being replaced: the new file keeps
/// the ACL it inherited from the directory it was created in, which is the same
/// place the original got its own. So doing nothing here is not a gap, it is the
/// correct behaviour written down.
#[cfg(unix)]
fn mode_of(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn mode_of(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

/// Where the temp sibling goes: beside the target, so the rename stays inside
/// one filesystem. A temp file in `/tmp` would be a copy across devices, which
/// rename cannot do and which stops being atomic.
fn temp_beside(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = format!(".{name}.mdblaze-{}", std::process::id());
    path.with_file_name(tmp)
}

/// Write `contents` to `path`, atomically.
///
/// The file's existing permissions are preserved. A document is not a secret and
/// forcing a restrictive mode on someone's notes -- which is what the org
/// server's store does, correctly, for its own files -- would silently change
/// what a shared file is readable by.
pub fn save_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = temp_beside(path);
    let existing_mode = mode_of(path);

    // Scoped so the handle is closed before the rename: on some platforms
    // renaming over an open file is refused, and a flush is not a close.
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        // Without this the rename can land before the contents reach the disk,
        // which on a power loss leaves an intact filename over empty blocks --
        // the failure this whole function exists to prevent, one level down.
        f.sync_all()?;
    }

    if let Some(mode) = existing_mode {
        set_mode(&tmp, mode);
    }

    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Leaving a stray dotfile beside someone's document every time a
            // save fails is its own small mess.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!("mdblaze-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    #[test]
    fn it_writes_what_it_was_given() {
        let d = tmpdir();
        let f = d.join("a.md");
        save_atomic(&f, "# hello\n\nbody\n").expect("save");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "# hello\n\nbody\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn it_overwrites_an_existing_file_completely() {
        // A shorter document must not leave the tail of the longer one behind,
        // which is what writing in place without truncating does.
        let d = tmpdir();
        let f = d.join("b.md");
        save_atomic(&f, "a very long original document indeed\n").expect("first");
        save_atomic(&f, "short\n").expect("second");
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "short\n");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn it_leaves_no_temp_file_behind() {
        let d = tmpdir();
        let f = d.join("c.md");
        save_atomic(&f, "x\n").expect("save");
        let left: Vec<String> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n != "c.md")
            .collect();
        assert!(left.is_empty(), "stray files: {left:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn the_temp_file_is_a_sibling_so_the_rename_stays_on_one_filesystem() {
        // A temp file in /tmp would be a cross-device copy, which rename cannot
        // do -- the save would fail on any machine whose /tmp is its own mount.
        let p = Path::new("/some/dir/notes.md");
        assert_eq!(temp_beside(p).parent(), p.parent());
    }

    #[test]
    #[cfg(unix)]
    fn existing_permissions_survive_a_save() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir();
        let f = d.join("shared.md");
        std::fs::write(&f, "one\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        save_atomic(&f, "two\n").expect("save");
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644, "a save changed who can read the file");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_new_file_is_created_rather_than_refused() {
        let d = tmpdir();
        let f = d.join("brand-new.md");
        assert!(!f.exists());
        save_atomic(&f, "new\n").expect("save");
        assert!(f.exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn an_unwritable_directory_reports_rather_than_pretending() {
        let f = Path::new("/proc/definitely/not/writable/x.md");
        assert!(save_atomic(f, "x\n").is_err());
    }
}
