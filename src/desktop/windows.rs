//! The Windows half: a ProgId and an extension association, in the registry.
//!
//! # The shape of it
//!
//! Windows separates "what kind of thing is this" from "what opens it", and both
//! live under `HKEY_CURRENT_USER\Software\Classes`:
//!
//!   - a **ProgId** -- here `mdblaze.md` -- describing the file type and holding
//!     the command that opens it;
//!   - an **extension key** -- `.md` -- naming the ProgId that owns it.
//!
//! `HKEY_CURRENT_USER`, never `HKEY_LOCAL_MACHINE`. The machine-wide hive needs
//! Administrator and changes the file type for everybody who uses the computer.
//! A markdown editor one person installed has no business doing either.
//!
//! # Why it does not force itself to be the default
//!
//! Since Windows 8 the default handler for an extension is protected: the value
//! that decides it carries a hash Microsoft computes, and writing it by hand is
//! detected and reverted. This is a deliberate defence against exactly the
//! browser-and-media-player hijacking that made it necessary.
//!
//! So this registers the association properly and leaves the choice to the
//! person, who gets asked by Windows the first time. Writing the raw value would
//! either be undone or, worse, work for a while and then be undone.
//!
//! # Why the operations are data
//!
//! This was written on Linux and has never been run on Windows. Which keys and
//! values get written is a pure function returning a list, which is tested on
//! every platform including this one; the part that cannot be tested here is the
//! loop that hands them to the registry.

use super::{DESCRIPTION, EXTENSIONS, FORMER_NAMES, NAME};

/// One registry value to write: the key path under `HKCU`, the value name --
/// empty for the key's default value -- and the data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub key: String,
    pub name: String,
    pub data: String,
}

/// The ProgId a name owns. Namespaced by the program so it cannot collide with
/// another editor's idea of what a markdown file is.
pub fn prog_id(name: &str) -> String {
    format!("{name}.md")
}

/// Every value that installing writes, in order.
///
/// A pure function of the executable's path, which is what makes the decisions
/// here checkable on a machine that is not a Windows machine.
pub fn install_values(exe: &str) -> Vec<Value> {
    let id = prog_id(NAME);
    let mut out = vec![
        Value { key: format!("Software\\Classes\\{id}"), name: String::new(), data: DESCRIPTION.into() },
        Value {
            key: format!("Software\\Classes\\{id}\\DefaultIcon"),
            name: String::new(),
            data: format!("\"{exe}\",0"),
        },
        Value {
            key: format!("Software\\Classes\\{id}\\shell\\open\\command"),
            name: String::new(),
            // `%1` in its own quotes: a path with a space -- `C:\Users\Someone\
            // My Documents\notes.md` -- arrives as several arguments otherwise,
            // and the editor opens nothing.
            data: format!("\"{exe}\" \"%1\""),
        },
    ];
    for ext in EXTENSIONS {
        // NOT the extension key's default value. Setting that seizes the type
        // outright; `OpenWithProgids` adds this program to the list Windows
        // offers and lets the person choose, which is the same restraint the
        // Linux side shows by recording what it replaced.
        out.push(Value {
            key: format!("Software\\Classes\\.{ext}\\OpenWithProgids"),
            name: id.clone(),
            data: String::new(),
        });
    }
    out
}

/// Every key that uninstalling removes, deepest first.
///
/// Deepest first because a registry key with subkeys underneath it cannot be
/// deleted, and a half-removed ProgId leaves Windows offering a program that is
/// no longer there.
pub fn uninstall_keys() -> Vec<String> {
    let mut out = Vec::new();
    for name in FORMER_NAMES.iter().copied().chain([NAME]) {
        let id = prog_id(name);
        out.push(format!("Software\\Classes\\{id}\\shell\\open\\command"));
        out.push(format!("Software\\Classes\\{id}\\shell\\open"));
        out.push(format!("Software\\Classes\\{id}\\shell"));
        out.push(format!("Software\\Classes\\{id}\\DefaultIcon"));
        out.push(format!("Software\\Classes\\{id}"));
    }
    out
}

/// The `OpenWithProgids` entries uninstalling clears, as (key, value name).
pub fn uninstall_values() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in FORMER_NAMES.iter().copied().chain([NAME]) {
        for ext in EXTENSIONS {
            out.push((format!("Software\\Classes\\.{ext}\\OpenWithProgids"), prog_id(name)));
        }
    }
    out
}

#[cfg(target_os = "windows")]
pub(super) fn install() -> std::io::Result<Vec<String>> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
    use winreg::RegKey;

    let exe = std::env::current_exe()?;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut said = Vec::new();

    // A former name's keys first, so two ProgIds do not both claim markdown.
    for (key, value) in uninstall_values() {
        if key.contains(&format!("{}.md", NAME)) {
            continue;
        }
        if let Ok(k) = hkcu.open_subkey_with_flags(&key, KEY_WRITE) {
            let _ = k.delete_value(&value);
        }
    }
    for key in uninstall_keys() {
        if key.starts_with(&format!("Software\\Classes\\{}", prog_id(NAME))) {
            continue;
        }
        if hkcu.delete_subkey(&key).is_ok() {
            said.push(format!("removed the former key {key}"));
        }
    }

    for v in install_values(&exe.to_string_lossy()) {
        let (k, _) = hkcu.create_subkey(&v.key)?;
        k.set_value(&v.name, &v.data)?;
    }
    said.push(format!("registered {} as an opener for .md and .markdown", exe.display()));
    said.push(
        "Windows will not let a program make itself the default opener. \
         Right-click a .md file, choose Open with > Choose another app, pick \
         mdblaze and tick \"Always use this app\"."
            .into(),
    );
    Ok(said)
}

#[cfg(target_os = "windows")]
pub(super) fn uninstall() -> std::io::Result<Vec<String>> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut said = Vec::new();
    for (key, value) in uninstall_values() {
        if let Ok(k) = hkcu.open_subkey_with_flags(&key, KEY_WRITE) {
            let _ = k.delete_value(&value);
        }
    }
    for key in uninstall_keys() {
        if hkcu.delete_subkey(&key).is_ok() {
            said.push(format!("removed {key}"));
        }
    }
    said.push("whatever opened markdown before is the default again".into());
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Vec<Value> {
        install_values("C:\\Users\\Someone\\.cargo\\bin\\mdblaze.exe")
    }

    #[test]
    fn a_path_with_a_space_in_it_still_opens() {
        // `C:\Users\Someone\My Documents\notes.md` is an ordinary path. Without
        // quotes around `%1` it arrives as several arguments and the editor
        // opens nothing -- and this is THE classic Windows association bug.
        let cmd = values().into_iter().find(|v| v.key.ends_with("command")).expect("a command");
        assert!(cmd.data.ends_with("\"%1\""), "{}", cmd.data);
        assert!(cmd.data.starts_with('"'), "the exe path is unquoted: {}", cmd.data);
    }

    #[test]
    fn everything_is_written_under_the_current_user() {
        // HKLM would need Administrator and would change the file type for
        // everybody with an account on the computer.
        for v in values() {
            assert!(v.key.starts_with("Software\\Classes\\"), "{}", v.key);
            assert!(!v.key.contains("HKEY_LOCAL_MACHINE"));
        }
    }

    #[test]
    fn the_extension_is_offered_rather_than_seized() {
        // Setting the extension key's DEFAULT value takes the file type
        // outright. `OpenWithProgids` adds this program to the list and leaves
        // the choice with the person -- and since Windows 8 the real default is
        // hash-protected anyway, so seizing it would be undone.
        for ext in EXTENSIONS {
            let key = format!("Software\\Classes\\.{ext}\\OpenWithProgids");
            let v = values().into_iter().find(|v| v.key == key).unwrap_or_else(|| panic!("{key}"));
            assert_eq!(v.name, prog_id(NAME), "the ProgId is the VALUE NAME here, not the data");
            assert!(
                !values().iter().any(|v| v.key == format!("Software\\Classes\\.{ext}") && v.name.is_empty()),
                ".{ext} default value was seized"
            );
        }
    }

    #[test]
    fn both_extensions_are_claimed() {
        for ext in EXTENSIONS {
            assert!(values().iter().any(|v| v.key.contains(&format!("\\.{ext}\\"))), "{ext}");
        }
    }

    #[test]
    fn the_prog_id_is_namespaced_so_it_cannot_collide() {
        // A bare `md` ProgId is a name every markdown tool would want.
        assert_eq!(prog_id("mdblaze"), "mdblaze.md");
        assert!(prog_id(NAME).starts_with(NAME));
    }

    #[test]
    fn uninstall_deletes_children_before_their_parents() {
        // A registry key with subkeys under it cannot be deleted, so the wrong
        // order leaves a half-removed ProgId and Windows offering a program that
        // is not there.
        let keys = uninstall_keys();
        for (child, k) in keys.iter().enumerate() {
            for (parent, other) in keys.iter().enumerate() {
                if k.starts_with(&format!("{other}\\")) {
                    assert!(child < parent, "{k} is listed after its parent {other}");
                }
            }
        }
        // And the ordering is not vacuously satisfied: there really are nested
        // keys in the list.
        assert!(keys.iter().any(|k| k.ends_with("shell\\open\\command")));
    }

    #[test]
    fn uninstall_covers_every_name_this_program_has_had() {
        // A rename that leaves the old ProgId behind leaves Windows offering a
        // binary that may no longer exist.
        let keys = uninstall_keys();
        for name in FORMER_NAMES.iter().copied().chain([NAME]) {
            assert!(keys.iter().any(|k| k.contains(&prog_id(name))), "{name} not cleaned up");
        }
    }

    #[test]
    fn uninstall_removes_the_open_with_entries_too() {
        // Leaving these behind means the program keeps appearing in the
        // "Open with" list after it has been uninstalled.
        let vals = uninstall_values();
        assert_eq!(vals.len(), EXTENSIONS.len() * (FORMER_NAMES.len() + 1));
        assert!(vals.iter().all(|(k, _)| k.ends_with("OpenWithProgids")));
    }
}
