// Compiled on every platform so the string handling below stays testable on any
// machine, but only CALLED on Linux -- so off Linux the private half is dead by
// construction. Allowed there and nowhere else: on Linux, dead code here is
// still an error, which is where it would actually mean something.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

//! The freedesktop half: a `.desktop` entry and `mimeapps.list`.
//!
//! On Linux, being the opener for a file type is two facts in two files. A
//! `.desktop` entry in `~/.local/share/applications` declares that this program
//! exists and which MIME types it can handle; a line in `~/.config/mimeapps.list`
//! says it is the one to use. Neither is enough alone -- an entry with no
//! default is an application nobody picks, and a default naming an entry that is
//! not there opens nothing.
//!
//! Everything here is written in terms of strings, and the string handling is
//! what the tests exercise. See [`super`] for why that matters more on the other
//! two platforms.

use std::io::Write;
use std::path::{Path, PathBuf};

/// The MIME types a markdown file is reported as.
///
/// Two, because `text/markdown` is what current shared-mime-info gives and
/// `text/x-markdown` is what older installs give. Claiming only one means the
/// association silently does nothing on half the machines it is installed on.
pub const MIMES: &[&str] = &["text/markdown", "text/x-markdown"];

/// The basename of the entry, which is also its id in `mimeapps.list`.
pub const DESKTOP_ID: &str = "mdblaze.desktop";

use super::FORMER_NAMES;

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
         Name=mdblaze\n\
         GenericName=Markdown Editor\n\
         Comment=A markdown editor that renders, and opens instantly\n\
         Exec={} %f\n\
         Icon=mdblaze\n\
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
/// The icon, at every size a desktop asks for.
///
/// PNGs rather than one scalable SVG, because the mark is a rendered blaze with
/// type over it -- there is no vector source to scale from, and tracing one
/// would lose the thing that makes it look like anything.
///
/// The sizes are not the same picture resampled. Below 32 pixels the mark drops
/// to a single `m`: two letters and a flame will not fit, the letters close up,
/// and the blaze becomes a smudge behind them. See `assets/make-icons.py`.
pub const ICONS: &[(u32, &[u8])] = &[
    (16, include_bytes!("../../assets/icons/16.png")),
    (22, include_bytes!("../../assets/icons/22.png")),
    (24, include_bytes!("../../assets/icons/24.png")),
    (32, include_bytes!("../../assets/icons/32.png")),
    (48, include_bytes!("../../assets/icons/48.png")),
    (64, include_bytes!("../../assets/icons/64.png")),
    (128, include_bytes!("../../assets/icons/128.png")),
    (256, include_bytes!("../../assets/icons/256.png")),
    (512, include_bytes!("../../assets/icons/512.png")),
];

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
                    // `None` removes the line, which is what restoring a MIME
                    // type that had no default before means.
                    if let Some(v) = value {
                        out.push(format!("{key}={v}"));
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
    } else if !seen_section {
        if let Some(v) = value {
            if !out.is_empty() && !out.last().is_some_and(|l| l.trim().is_empty()) {
                out.push(String::new());
            }
            out.push(section.to_string());
            out.push(format!("{key}={v}"));
        }
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

/// Where a name -- this one or a former one -- keeps its three files.
fn entry_path_for(name: &str) -> PathBuf {
    applications_dir().join(format!("{name}.desktop"))
}
fn icon_path_for(name: &str, size: u32) -> PathBuf {
    home().join(format!(".local/share/icons/hicolor/{size}x{size}/apps/{name}.png"))
}
fn record_path_for(name: &str) -> PathBuf {
    home().join(format!(".local/share/{name}/replaced-defaults"))
}

/// Take a former name's files off the disk, and answer what IT displaced.
///
/// The record is the part that matters. When the old name installed itself it
/// wrote down what markdown used to open with -- a browser, an editor, nothing
/// at all -- so that uninstalling could give it back. That fact belongs to the
/// user, not to the name, so it is carried across rather than deleted with the
/// rest.
fn sweep_former(said: &mut Vec<String>) -> Vec<(String, Option<String>)> {
    let mut inherited = Vec::new();
    for name in FORMER_NAMES {
        let record = record_path_for(name);
        if let Ok(text) = std::fs::read_to_string(&record) {
            inherited = parse_record(&text);
        }
        let _ = std::fs::remove_file(&record);
        let _ = record.parent().map(std::fs::remove_dir);
        for (size, _) in ICONS {
            let _ = std::fs::remove_file(icon_path_for(name, *size));
        }
        // The former name may also have installed a scalable SVG, which this
        // one no longer does.
        let _ = std::fs::remove_file(
            home().join(format!(".local/share/icons/hicolor/scalable/apps/{name}.svg")),
        );
        let entry = entry_path_for(name);
        if entry.exists() {
            let _ = std::fs::remove_file(&entry);
            said.push(format!("removed the former {name} entry at {}", entry.display()));
        }
    }
    inherited
}

/// Rewrite a captured "what was here before" list so nothing in it names a
/// former identity.
///
/// Without this, installing over an older name records that markdown used to
/// belong to `mdedit.desktop` -- and uninstalling would then dutifully hand the
/// association back to a `.desktop` file that was just deleted, leaving markdown
/// opening with nothing. What the user actually wants back is whatever came
/// before the FIRST of our names, which is what the old record holds.
fn forget_former(
    previous: Vec<(String, Option<String>)>,
    inherited: &[(String, Option<String>)],
) -> Vec<(String, Option<String>)> {
    let ours: Vec<String> =
        FORMER_NAMES.iter().map(|n| format!("{n}.desktop")).chain([DESKTOP_ID.to_string()]).collect();
    previous
        .into_iter()
        .map(|(mime, was)| {
            let stale = was.as_deref().is_some_and(|w| ours.iter().any(|o| o == w));
            if !stale {
                return (mime, was);
            }
            let older = inherited.iter().find(|(m, _)| *m == mime).and_then(|(_, v)| v.clone());
            (mime, older)
        })
        .collect()
}

use super::home;

fn applications_dir() -> PathBuf {
    home().join(".local/share/applications")
}
fn icon_path(size: u32) -> PathBuf {
    icon_path_for(super::NAME, size)
}
fn mimeapps() -> PathBuf {
    home().join(".config/mimeapps.list")
}
/// Where the replaced defaults are remembered, so uninstall can restore them.
fn record_path() -> PathBuf {
    home().join(".local/share/mdblaze/replaced-defaults")
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
pub(super) fn install() -> std::io::Result<Vec<String>> {
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

    // First, so the report reads in the order things happened -- and so a
    // former name's files are gone before ours are written, rather than both
    // existing for the width of a few syscalls.
    let former = sweep_former(&mut said);
    // Our OWN record from a previous install, and it takes precedence.
    //
    // Installing twice is ordinary -- after an upgrade, or after moving the
    // binary. The second install captures the current default, which is us, and
    // `forget_former` has to turn that back into whatever was there originally.
    // Reading only a FORMER name's record was not enough: on the second install
    // of the same name there is no former record, the lookup found nothing, and
    // the memory of GNOME Text Editor was quietly replaced with "no default".
    // Uninstalling would then have left markdown opening with nothing at all.
    //
    // Caught by running it twice and reading the report, which said "had no
    // default" about a file type that certainly had one.
    let mine = std::fs::read_to_string(record_path()).map(|s| parse_record(&s)).unwrap_or_default();
    let inherited = if mine.is_empty() { former } else { mine };

    let apps = applications_dir();
    std::fs::create_dir_all(&apps)?;
    let entry_path = apps.join(DESKTOP_ID);
    std::fs::write(&entry_path, entry(&exe.to_string_lossy()))?;
    said.push(format!("wrote {}", entry_path.display()));

    for (size, bytes) in ICONS {
        let ico = icon_path(*size);
        if let Some(p) = ico.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&ico, bytes)?;
    }
    said.push(format!("wrote {} icon sizes under hicolor", ICONS.len()));

    let mimeapps_path = mimeapps();
    let before = std::fs::read_to_string(&mimeapps_path).unwrap_or_default();
    let (after, previous) = take_defaults(&before, DESKTOP_ID);
    let previous = forget_former(previous, &inherited);
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
            Some(w) if w != DESKTOP_ID => said.push(format!("{m} was {w}, now mdblaze")),
            Some(_) => said.push(format!("{m} already mdblaze")),
            None => said.push(format!("{m} had no default, now mdblaze")),
        }
    }

    refresh(&apps, &mut said);
    Ok(said)
}

/// Remove the handler and restore whatever it displaced.
pub(super) fn uninstall() -> std::io::Result<Vec<String>> {
    let mut said = Vec::new();
    let apps = applications_dir();
    // A machine that still carries a former name's leftovers should come out of
    // this clean, not half-uninstalled under a name nobody typed.
    sweep_former(&mut said);
    let entry_path = apps.join(DESKTOP_ID);
    if entry_path.exists() {
        std::fs::remove_file(&entry_path)?;
        said.push(format!("removed {}", entry_path.display()));
    }
    for (size, _) in ICONS {
        let _ = std::fs::remove_file(icon_path(*size));
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
        let e = entry("/usr/local/bin/mdblaze");
        assert!(e.contains("MimeType=text/markdown;text/x-markdown;"), "{e}");
    }

    #[test]
    fn the_entry_passes_the_file_it_was_opened_with() {
        let e = entry("/usr/local/bin/mdblaze");
        assert!(e.contains("Exec=/usr/local/bin/mdblaze %f"), "{e}");
    }

    #[test]
    fn a_path_with_spaces_is_quoted() {
        // Unquoted, the desktop splits the path and launches something that does
        // not exist -- and the failure is a silent no-op on double-click.
        let e = entry("/home/a b/bin/mdblaze");
        assert!(e.contains("Exec=\"/home/a b/bin/mdblaze\" %f"), "{e}");
    }

    #[test]
    fn setting_a_key_leaves_every_other_line_alone() {
        // This file belongs to the desktop and other programs write to it.
        // Rewriting it from a parsed model would drop whatever this code does not
        // model, which is how a tool breaks an unrelated association.
        let before = "[Default Applications]\ntext/html=firefox.desktop\nx-scheme-handler/http=firefox.desktop\n";
        let after = ini_set(before, SECTION, "text/markdown", Some("mdblaze.desktop"));
        assert!(after.contains("text/html=firefox.desktop"));
        assert!(after.contains("x-scheme-handler/http=firefox.desktop"));
        assert!(after.contains("text/markdown=mdblaze.desktop"));
    }

    #[test]
    fn setting_an_existing_key_replaces_it_rather_than_adding_a_second() {
        let before = "[Default Applications]\ntext/markdown=gedit.desktop\n";
        let after = ini_set(before, SECTION, "text/markdown", Some("mdblaze.desktop"));
        assert_eq!(after.matches("text/markdown=").count(), 1, "{after}");
        assert!(after.contains("text/markdown=mdblaze.desktop"));
    }

    #[test]
    fn a_key_is_written_into_its_own_section_not_the_next_one() {
        let before = "[Default Applications]\ntext/html=a.desktop\n\n[Added Associations]\ntext/html=a.desktop;\n";
        let after = ini_set(before, SECTION, "text/markdown", Some("mdblaze.desktop"));
        let defaults = after.split("[Added Associations]").next().expect("section");
        assert!(defaults.contains("text/markdown=mdblaze.desktop"), "landed in the wrong section: {after}");
    }

    #[test]
    fn a_missing_section_is_created() {
        let after = ini_set("", SECTION, "text/markdown", Some("mdblaze.desktop"));
        assert!(after.contains(SECTION));
        assert!(after.contains("text/markdown=mdblaze.desktop"));
    }

    #[test]
    fn taking_the_default_records_what_it_replaced() {
        let before = "[Default Applications]\ntext/markdown=org.gnome.TextEditor.desktop\n";
        let (after, previous) = take_defaults(before, DESKTOP_ID);
        assert!(after.contains("text/markdown=mdblaze.desktop"));
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
        assert!(taken.contains("text/markdown=mdblaze.desktop"));
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
        assert!(taken.contains("text/markdown=mdblaze.desktop"));
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
    fn every_icon_size_is_a_real_png_of_that_size() {
        // A mis-built or truncated icon is one that silently does not appear,
        // with nothing said about it. The header carries the dimensions, so
        // this also catches a size wired to the wrong file.
        for (size, bytes) in ICONS {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "{size} is not a PNG");
            let w = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
            let h = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
            assert_eq!((w, h), (*size, *size), "{size}.png is actually {w}x{h}");
        }
    }

    #[test]
    fn the_icon_covers_the_sizes_a_desktop_asks_for() {
        // 16 and 24 are what a file list and a taskbar use, and they are the
        // ones a "just ship 512 and let it scale" icon gets wrong.
        for want in [16, 24, 32, 48, 256] {
            assert!(ICONS.iter().any(|(s, _)| *s == want), "no {want}px icon");
        }
    }

    // ---- the rename ----------------------------------------------------

    #[test]
    fn a_former_name_is_never_handed_the_association_back() {
        // The failure this prevents: install over `mdedit`, which captures
        // "markdown used to be mdedit.desktop", then uninstall -- which gives
        // markdown back to a .desktop file that was deleted during the install.
        // Double-clicking a file then opens nothing at all.
        let captured = vec![("text/markdown".to_string(), Some("mdedit.desktop".to_string()))];
        let inherited = vec![("text/markdown".to_string(), Some("firefox.desktop".to_string()))];
        let out = forget_former(captured, &inherited);
        assert_eq!(out, vec![("text/markdown".to_string(), Some("firefox.desktop".to_string()))]);
    }

    #[test]
    fn a_real_previous_default_is_left_exactly_as_it_was() {
        // The common case, and the one the laundering must not touch.
        let captured = vec![("text/markdown".to_string(), Some("okular.desktop".to_string()))];
        let out = forget_former(captured, &[]);
        assert_eq!(out[0].1, Some("okular.desktop".to_string()));
    }

    #[test]
    fn installing_over_a_former_name_that_displaced_nothing_still_displaces_nothing() {
        // `mdedit` took markdown when it had no default. After the rename,
        // uninstalling must leave it with no default -- not with `mdedit`.
        let captured = vec![("text/markdown".to_string(), Some("mdedit.desktop".to_string()))];
        let out = forget_former(captured, &[]);
        assert_eq!(out[0].1, None, "resurrected a name that no longer exists");
    }

    #[test]
    fn our_own_id_is_laundered_too_so_reinstalling_is_not_a_trap() {
        // Installing twice captures "markdown was mdblaze.desktop". Recording
        // that would make uninstall a no-op that leaves us as the default
        // forever.
        let captured = vec![("text/markdown".to_string(), Some(DESKTOP_ID.to_string()))];
        let inherited = vec![("text/markdown".to_string(), Some("nvim.desktop".to_string()))];
        assert_eq!(forget_former(captured, &inherited)[0].1, Some("nvim.desktop".to_string()));
    }

    #[test]
    fn installing_twice_does_not_forget_what_the_first_install_displaced() {
        // The second install sees ITSELF as the current default. If that is what
        // gets recorded, uninstalling hands markdown back to us -- or to
        // nothing -- instead of to the program that had it originally.
        let original = vec![("text/markdown".to_string(), Some("org.gnome.TextEditor.desktop".to_string()))];
        // First install recorded the truth; second install captures us.
        let captured = vec![("text/markdown".to_string(), Some(DESKTOP_ID.to_string()))];
        let after = forget_former(captured, &original);
        assert_eq!(
            after[0].1,
            Some("org.gnome.TextEditor.desktop".to_string()),
            "a second install forgot what the first one replaced"
        );
    }

    #[test]
    fn every_former_name_has_the_three_files_it_needs_swept() {
        // If a name were added to the list without its paths deriving from it,
        // the sweep would silently miss two of its three files.
        for n in FORMER_NAMES {
            assert!(entry_path_for(n).to_string_lossy().ends_with(&format!("{n}.desktop")));
            assert!(icon_path_for(n, 48).to_string_lossy().ends_with(&format!("{n}.png")));
            assert!(record_path_for(n).to_string_lossy().contains(*n));
        }
    }

    #[test]
    fn the_current_name_is_not_in_the_list_of_former_ones() {
        // It would sweep its own files away mid-install.
        assert!(!FORMER_NAMES.contains(&"mdblaze"));
    }

    #[test]
    fn the_entry_and_the_icon_agree_on_the_name() {
        // `Icon=` names an icon by stem, and the file written must have that
        // stem or the desktop shows a generic page instead.
        let e = entry("/usr/local/bin/mdblaze");
        assert!(e.contains("Icon=mdblaze"), "{e}");
        assert!(icon_path(48).to_string_lossy().ends_with("48x48/apps/mdblaze.png"));
        assert!(DESKTOP_ID.starts_with("mdblaze"));
    }

}
