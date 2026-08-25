//! Edit, save, reopen. The cycle that has to be right or someone loses work.
//!
//! The unit tests cover the buffer and the writer separately; this covers the
//! seam between them, which is where an editor actually loses a document -- the
//! buffer is correct, the writer is correct, and what reaches the disk is
//! neither.

use mdedit::{doc, edit::Buffer, file};

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mdedit-rt-{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("tempdir");
    d.join(name)
}

#[test]
fn typing_into_a_document_and_saving_it_survives_a_reopen() {
    let f = tmp("a.md");
    std::fs::write(&f, "# Title\n\nbody\n").expect("seed");

    let mut b = Buffer::from_str(&std::fs::read_to_string(&f).unwrap());
    // To the end of the last line, and add a sentence.
    while b.line + 1 < b.lines().len() {
        b.down();
    }
    b.end();
    for (i, c) in " and more.".chars().enumerate() {
        b.insert_char(c, i as u128);
    }
    assert!(b.is_dirty());
    file::save_atomic(&f, &b.text()).expect("save");
    b.mark_saved();

    let reread = std::fs::read_to_string(&f).unwrap();
    assert_eq!(reread, "# Title\n\nbody and more.\n");
    // And it is still the document it was, not just the right bytes.
    let parsed = doc::parse(&reread);
    assert_eq!(parsed.blocks.len(), 2);
    std::fs::remove_file(&f).ok();
}

#[test]
fn a_save_does_not_grow_a_blank_line_each_time() {
    // The line-based buffer's classic failure: split on newline, join on save,
    // and the file gains a line every single save until someone notices.
    let f = tmp("b.md");
    std::fs::write(&f, "one\ntwo\n").expect("seed");
    for _ in 0..6 {
        let b = Buffer::from_str(&std::fs::read_to_string(&f).unwrap());
        file::save_atomic(&f, &b.text()).expect("save");
    }
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "one\ntwo\n");
    std::fs::remove_file(&f).ok();
}

#[test]
fn undoing_everything_then_saving_restores_the_original_bytes() {
    let f = tmp("c.md");
    let original = "# Keep\n\nexactly this\n";
    std::fs::write(&f, original).expect("seed");

    let mut b = Buffer::from_str(original);
    b.end();
    // Two separate bursts, so there are two undo steps to walk back.
    b.insert_char('X', 0);
    b.insert_char('Y', 5_000);
    while b.undo() {}
    file::save_atomic(&f, &b.text()).expect("save");

    assert_eq!(std::fs::read_to_string(&f).unwrap(), original);
    std::fs::remove_file(&f).ok();
}

#[test]
fn a_document_with_multibyte_text_round_trips_byte_for_byte() {
    let f = tmp("d.md");
    let original = "# Café\n\nnaïve — résumé, £20, 日本\n";
    std::fs::write(&f, original).expect("seed");
    let b = Buffer::from_str(&std::fs::read_to_string(&f).unwrap());
    file::save_atomic(&f, &b.text()).expect("save");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), original);
    std::fs::remove_file(&f).ok();
}

#[test]
fn saving_a_file_that_did_not_exist_creates_it() {
    let f = tmp("e-new.md");
    std::fs::remove_file(&f).ok();
    let b = Buffer::from_str("# New\n");
    file::save_atomic(&f, &b.text()).expect("save");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "# New\n");
    std::fs::remove_file(&f).ok();
}
