//! Registering as the handler for markdown files, and unregistering again.
//!
//! Double-clicking a `.md` file is the whole point of a fast editor: the speed
//! only matters if opening a file is one gesture. On Linux that means a
//! `.desktop` entry declaring the MIME types it handles, plus an entry in
//! `mimeapps.list` saying it is the default.
//!
//! # Why uninstall records what it replaced
//!
//! Installing this takes over a file type the person already had an opinion
//! about -- on the machine this was written on, GNOME Text Editor. A tool that
//! seizes a file association and cannot give it back is a tool people are right
//! to be wary of, so the previous default is written down before it is replaced
//! and put back on uninstall.
//!
//! # Linux only, and honest about it
//!
//! `.desktop` files are a freedesktop convention. macOS declares document types
//! in an app bundle's `Info.plist` and Windows writes registry keys; neither is
//! this, and pretending otherwise by silently doing nothing would be worse than
//! saying so.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The MIME types a markdown file is reported as.
///
/// Two, because `text/markdown` is what current shared-mime-info gives and
/// `text/x-markdown` is what older installs give. Claiming only one means the
/// association silently does nothing on half the machines it is installed on.
pub const MIMES: &[&str] = &["text/markdown", "text/x-markdown"];

/// The basename of the entry, which is also its id in `mimeapps.list`.
pub const DESKTOP_ID: &str = "mdedit.desktop";

/// Quote a path for a `.desktop` `Exec` line if it needs it.
fn exec_quote(p: &str) -> String {
    if p.contains(' ') || p.contains('"') {
        format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        p.to_string()
    }
}

/// The `.desktop` entry's contents, for a binary at `exec`.
pub fn entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=mdedit\n\
         GenericName=Markdown Editor\n\
         Comment=A markdown editor that renders, and opens instantly\n\
         Exec={} %f\n\
         Icon=mdedit\n\
         Terminal=false\n\
         Categories=Utility;TextEditor;\n\
         MimeType={};\n\
         StartupNotify=true\n",
        exec_quote(exec),
        MIMES.join(";")
    )
}

/// A document with a corner turned, in the accent. Scalable, so it is one file
/// for every size a desktop asks for.
pub fn icon() -> String {
    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\n\
     <rect width=\"64\" height=\"64\" rx=\"10\" fill=\"#121212\"/>\n\
     <path d=\"M20 14h16l10 10v26a2 2 0 0 1-2 2H20a2 2 0 0 1-2-2V16a2 2 0 0 1 2-2z\" fill=\"#1b1b1b\" stroke=\"#b63c35\" stroke-width=\"2\"/>\n\
     <path d=\"M36 14v10h10\" fill=\"none\" stroke=\"#b63c35\" stroke-width=\"2\"/>\n\
     <rect x=\"24\" y=\"32\" width=\"16\" height=\"2.5\" fill=\"#e8e8e8\"/>\n\
     <rect x=\"24\" y=\"38\" width=\"12\" height=\"2.5\" fill=\"#a8a8a8\"/>\n\
     <rect x=\"24\" y=\"44\" width=\"14\" height=\"2.5\" fill=\"#a8a8a8\"/>\n\
     </svg>\n"
        .to_string()
}

/// Read the value of `key` in `section` of an INI-ish desktop config file.
fn ini_get(content: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_section = t == section;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Set `key` to `value` in `section`, creating the section if it is absent.
///
/// A hand-rolled edit rather than a config library, because this file belongs to
/// the desktop and is edited by other programs: rewriting it wholesale from a
/// parsed model would drop comments and any key this code does not model, which
/// is how a tool quietly breaks somebody's unrelated file association.
fn ini_set(content: &str, section: &str, key: &str, value: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut wrote = false;
    let mut seen_section = false;

    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            // Leaving the target section without having written the key: write it
            // at the end of that section rather than in the next one.
            if in_section && !wrote {
                if let Some(v) = value {
                    out.push(format!("{key}={v}"));
                }
                wrote = true;
            }
            in_section = t == section;
            seen_section |= in_section;
            out.push(line.to_string());
            continue;
        }
        if in_section {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim() == key {
                    match value {
                        Some(v) => out.push(format!("{key}={v}")),
                        // None removes it, which is what restoring a MIME type
                        // that had no default before means.
                        None => {}
                    }
                    wrote = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }

    if in_section && !wrote {
        if let Some(v) = value {
            out.push(format!("{key}={v}"));
        }
        wrote = true;
    }
    if !seen_section && value.is_some() {
        if !out.is_empty() && !out.last().is_some_and(|l| l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(section.to_string());
        out.push(format!("{key}={}", value.expect("checked")));
    }

    let mut s = out.join("\n");
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

const SECTION: &str = "[Default Applications]";

/// Point every markdown MIME type at `desktop_id`, and report what each was.
pub fn take_defaults(content: &str, desktop_id: &str) -> (String, Vec<(String, Option<String>)>) {
    let mut out = content.to_string();
    let mut previous = Vec::new();
    for m in MIMES {
        previous.push(((*m).to_string(), ini_get(&out, SECTION, m)));
        out = ini_set(&out, SECTION, m, Some(desktop_id));
    }
    (out, previous)
}

/// Put back what `take_defaults` replaced.
pub fn give_back_defaults(content: &str, previous: &[(String, Option<String>)]) -> String {
    let mut out = content.to_string();
    for (mime, was) in previous {
        out = ini_set(&out, SECTION, mime, was.as_deref());
    }
    out
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn applications_dir() -> PathBuf {
    home().join(".local/share/applications")
}
fn icon_path() -> PathBuf {
    home().join(".local/share/icons/hicolor/scalable/apps/mdedit.svg")
}
fn mimeapps() -> PathBuf {
    home().join(".config/mimeapps.list")
}
/// Where the replaced defaults are remembered, so uninstall can restore them.
fn record_path() -> PathBuf {
    home().join(".local/share/mdedit/replaced-defaults")
}

fn serialise_record(previous: &[(String, Option<String>)]) -> String {
    previous
        .iter()
        .map(|(m, v)| format!("{m}={}", v.as_deref().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn parse_record(s: &str) -> Vec<(String, Option<String>)> {
    s.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(m, v)| {
            let v = v.trim();
            (m.trim().to_string(), (!v.is_empty()).then(|| v.to_string()))
        })
        .collect()
}

/// Install the handler and make it the default for markdown.
pub fn install() -> std::io::Result<Vec<String>> {
    if !cfg!(target_os = "linux") {
        return Err(std::io::Error::other(
            "file associations here are a freedesktop convention; macOS uses an \
             app bundle's Info.plist and Windows uses the registry, and neither is this",
        ));
    }
    let exe = std::env::current_exe()?;
    let mut said = Vec::new();

    // The entry records an absolute path, so installing from a build directory
    // works until the next `cargo clean` and then double-click silently does
    // nothing. Worth saying at the moment it is being wired up rather than
    // leaving to be discovered later.
    if exe.components().any(|c| c.as_os_str() == "target") {
        said.push(format!(
            "note: pointing at a build artifact ({}). Copy it somewhere stable \
             (~/.local/bin) and re-run this, or a rebuild will break the association.",
            exe.display()
        ));
    }

    let apps = applications_dir();
    std::fs::create_dir_all(&apps)?;
    let entry_path = apps.join(DESKTOP_ID);
    std::fs::write(&entry_path, entry(&exe.to_string_lossy()))?;
    said.push(format!("wrote {}", entry_path.display()));

    let ico = icon_path();
    if let Some(p) = ico.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&ico, icon())?;
    said.push(format!("wrote {}", ico.display()));

    let mimeapps_path = mimeapps();
    let before = std::fs::read_to_string(&mimeapps_path).unwrap_or_default();
    let (after, previous) = take_defaults(&before, DESKTOP_ID);
    if let Some(p) = mimeapps_path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&mimeapps_path, after)?;

    // Written BEFORE anything else can fail, so uninstall can always give back
    // what was taken.
    if let Some(p) = record_path().parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(record_path(), serialise_record(&previous))?;

    for (m, was) in &previous {
        match was {
            Some(w) if w != DESKTOP_ID => said.push(format!("{m} was {w}, now mdedit")),
            Some(_) => said.push(format!("{m} already mdedit")),
            None => said.push(format!("{m} had no default, now mdedit")),
        }
    }

    refresh(&apps, &mut said);
    Ok(said)
}

/// Remove the handler and restore whatever it displaced.
pub fn uninstall() -> std::io::Result<Vec<String>> {
    let mut said = Vec::new();
    let apps = applications_dir();
    let entry_path = apps.join(DESKTOP_ID);
    if entry_path.exists() {
        std::fs::remove_file(&entry_path)?;
        said.push(format!("removed {}", entry_path.display()));
    }
    let ico = icon_path();
    if ico.exists() {
        std::fs::remove_file(&ico)?;
    }

    let record = record_path();
    let previous = std::fs::read_to_string(&record).map(|s| parse_record(&s)).unwrap_or_default();
    let mimeapps_path = mimeapps();
    if let Ok(before) = std::fs::read_to_string(&mimeapps_path) {
        let after = give_back_defaults(&before, &previous);
        std::fs::write(&mimeapps_path, after)?;
        for (m, was) in &previous {
            match was {
                Some(w) => said.push(format!("{m} restored to {w}")),
                None => said.push(format!("{m} left with no default, as it was")),
            }
        }
    }
    let _ = std::fs::remove_file(&record);

    refresh(&apps, &mut said);
    Ok(said)
}

/// Tell the desktop the association changed. Best effort: the entry is written
/// either way, and a stale cache resolves itself on the next login.
fn refresh(apps: &Path, said: &mut Vec<String>) {
    match std::process::Command::new("update-desktop-database").arg(apps).status() {
        Ok(s) if s.success() => said.push("refreshed the desktop database".into()),
        _ => said.push(
            "could not run update-desktop-database; the association may need a re-login".into(),
        ),
    }
    let _ = std::io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_declares_both_markdown_types() {
        // Only `text/markdown` means the association silently does nothing on any
        // machine whose shared-mime-info still says `text/x-markdown`.
        let e = entry("/usr/local/bin/mdedit");
        assert!(e.contains("MimeType=text/markdown;text/x-markdown;"), "{e}");
    }

    #[test]
    fn the_entry_passes_the_file_it_was_opened_with() {
        let e = entry("/usr/local/bin/mdedit");
        assert!(e.contains("Exec=/usr/local/bin/mdedit %f"), "{e}");
    }

    #[test]
    fn a_path_with_spaces_is_quoted() {
        // Unquoted, the desktop splits the path and launches something that does
        // not exist -- and the failure is a silent no-op on double-click.
        let e = entry("/home/a b/bin/mdedit");
        assert!(e.contains("Exec=\"/home/a b/bin/mdedit\" %f"), "{e}");
    }

    #[test]
    fn setting_a_key_leaves_every_other_line_alone() {
        // This file belongs to the desktop and other programs write to it.
        // Rewriting it from a parsed model would drop whatever this code does not
        // model, which is how a tool breaks an unrelated association.
        let before = "[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/http=firefox.desktop\n";
        let after = ini_set(before, SECTION, "text/markdown", Some("mdedit.desktop"));
        assert!(after.contains("text/html=firefox.desktop"));
        assert!(after.contains("x-scheme-handler/http=firefox.desktop"));
        assert!(after.contains("text/markdown=mdedit.desktop"));
    }

    #[test]
    fn setting_an_existing_key_replaces_it_rather_than_adding_a_second() {
        let before = "[Default Applications]\ntext/markdown=gedit.desktop\n";
        let after = ini_set(before, SECTION, "text/markdown", Some("mdedit.desktop"));
        assert_eq!(after.matches("text/markdown=").count(), 1, "{after}");
        assert!(after.contains("text/markdown=mdedit.desktop"));
    }

    #[test]
    fn a_key_is_written_into_its_own_section_not_the_next_one() {
        let before = "[Default Applications]\ntext/html=a.desktop\n\n[Added Associations]\ntext/html=a.desktop;\n";
        let after = ini_set(before, SECTION, "text/markdown", Some("mdedit.desktop"));
        let defaults = after.split("[Added Associations]").next().expect("section");
        assert!(defaults.contains("text/markdown=mdedit.desktop"), "landed in the wrong section: {after}");
    }

    #[test]
    fn a_missing_section_is_created() {
        let after = ini_set("", SECTION, "text/markdown", Some("mdedit.desktop"));
        assert!(after.contains(SECTION));
        assert!(after.contains("text/markdown=mdedit.desktop"));
    }

    #[test]
    fn taking_the_default_records_what_it_replaced() {
        let before = "[Default Applications]\ntext/markdown=org.gnome.TextEditor.desktop\n";
        let (after, previous) = take_defaults(before, DESKTOP_ID);
        assert!(after.contains("text/markdown=mdedit.desktop"));
        assert_eq!(
            previous[0],
            ("text/markdown".to_string(), Some("org.gnome.TextEditor.desktop".to_string()))
        );
    }

    #[test]
    fn giving_it_back_restores_the_previous_handler_exactly() {
        // The property that makes this safe to try: what was taken is given back.
        let before = "[Default Applications]\ntext/html=firefox.desktop\ntext/markdown=org.gnome.TextEditor.desktop\n";
        let (taken, previous) = take_defaults(before, DESKTOP_ID);
        assert!(taken.contains("text/markdown=mdedit.desktop"));
        let restored = give_back_defaults(&taken, &previous);
        assert!(restored.contains("text/markdown=org.gnome.TextEditor.desktop"), "{restored}");
        assert!(restored.contains("text/html=firefox.desktop"), "an unrelated key was lost");
    }

    #[test]
    fn a_type_that_had_no_default_is_left_with_none() {
        // Not "restored" to some invention. It had no default before and must
        // have none after, or uninstalling leaves a handler nobody chose.
        let before = "[Default Applications]\ntext/html=firefox.desktop\n";
        let (taken, previous) = take_defaults(before, DESKTOP_ID);
        assert!(taken.contains("text/markdown=mdedit.desktop"));
        let restored = give_back_defaults(&taken, &previous);
        assert!(!restored.contains("text/markdown="), "left behind: {restored}");
        assert!(!restored.contains("text/x-markdown="), "left behind: {restored}");
    }

    #[test]
    fn the_record_survives_a_round_trip_through_a_file() {
        let previous = vec![
            ("text/markdown".to_string(), Some("gedit.desktop".to_string())),
            ("text/x-markdown".to_string(), None),
        ];
        assert_eq!(parse_record(&serialise_record(&previous)), previous);
    }

    #[test]
    fn the_icon_is_a_single_self_contained_svg() {
        let i = icon();
        assert!(i.starts_with("<svg"));
        assert!(i.contains("</svg>"));
        // Nothing FETCHED. The `xmlns` is a namespace identifier and is never
        // dereferenced, so testing for the string "http" flags it and fails for
        // a reason that has nothing to do with the icon. What would actually
        // reach the network is a reference to another resource.
        for fetches in ["xlink:href", "<image", "url(http", "src="] {
            assert!(!i.contains(fetches), "the icon references {fetches}: {i}");
        }
    }
}
