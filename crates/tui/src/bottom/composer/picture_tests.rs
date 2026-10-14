use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::{Composer, ComposerEvent};
use super::{Clipboard, picture_path, picture_paths};

const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn shot(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, PNG).unwrap();
    path
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// How a terminal hands over a dropped file: its path, with spaces escaped.
fn dropped(path: &Path) -> String {
    path.display().to_string().replace(' ', "\\ ")
}

#[test]
fn a_dropped_picture_becomes_a_chip_numbered_across_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path(), "Screenshot 2026-10-08 at 10.07.05 PM.png");
    let mut c = Composer::new();
    for ch in "what's off on this screen?".chars() {
        c.handle_key(key(KeyCode::Char(ch)));
    }
    c.handle_paste(&dropped(&path));
    assert_eq!(c.text(), "what's off on this screen? [Image #1] ");
    let ComposerEvent::Submitted { text, shown } = c.handle_key(key(KeyCode::Enter)) else {
        panic!("not sent");
    };
    assert_eq!(text, "what's off on this screen? [Image #1] ");
    assert_eq!(shown, text);
    let sent = c.take_sent_pictures();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].number, 1);
    assert_eq!(sent[0].path, path);
    assert!(c.take_sent_pictures().is_empty(), "taken once");

    // The next one counts on.
    c.handle_paste(&dropped(&path));
    assert_eq!(c.text(), "[Image #2] ");
}

#[test]
fn backspace_takes_a_chip_whole_and_a_chip_taken_out_isnt_sent() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path(), "one.png");
    let mut c = Composer::new();
    c.handle_paste(&dropped(&path));
    c.handle_key(key(KeyCode::Backspace));
    assert_eq!(c.text(), "[Image #1]", "the space after it goes first");
    c.handle_key(key(KeyCode::Backspace));
    assert_eq!(c.text(), "", "then the chip, whole");
    for ch in "hi".chars() {
        c.handle_key(key(KeyCode::Char(ch)));
    }
    c.handle_key(key(KeyCode::Enter));
    assert!(c.take_sent_pictures().is_empty());
}

#[test]
fn a_path_is_read_as_claude_code_reads_one() {
    let dir = tempfile::tempdir().unwrap();
    let spaced = shot(dir.path(), "my shot.PNG");
    let shown = spaced.display().to_string();
    assert_eq!(
        picture_path(&shown.replace(' ', "\\ ")),
        Some(spaced.clone())
    );
    assert_eq!(picture_path(&format!("'{shown}'")), Some(spaced.clone()));
    assert_eq!(picture_path(&format!("\"{shown}\"")), Some(spaced.clone()));
    let address = format!("file://{}", shown.replace(' ', "%20"));
    assert_eq!(picture_path(&address), Some(spaced.clone()));
    // Not a picture, or not there: it stays text.
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "hi").unwrap();
    assert_eq!(picture_path(&notes.display().to_string()), None);
    assert_eq!(
        picture_path(&dir.path().join("gone.png").display().to_string()),
        None
    );
    let renamed = dir.path().join("text.png");
    std::fs::write(&renamed, "not a picture").unwrap();
    assert_eq!(picture_path(&renamed.display().to_string()), None);

    // Two dropped at once are two chips; a path among words is only text.
    let other = shot(dir.path(), "b.jpg");
    std::fs::write(&other, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
    let both = format!("{} {}", dropped(&spaced), dropped(&other));
    assert_eq!(picture_paths(&both), Some(vec![spaced.clone(), other]));
    assert_eq!(
        picture_paths(&format!("look at {}", dropped(&spaced))),
        None
    );
    let mut c = Composer::new();
    c.handle_paste(&both);
    assert_eq!(c.text(), "[Image #1] [Image #2] ");
}

#[test]
fn ctrl_v_pastes_a_copied_picture_as_a_chip() {
    fn copied(_: &Path) -> Option<PathBuf> {
        Some(PathBuf::from("/tmp/pasted.png"))
    }
    fn nothing(_: &Path) -> Option<PathBuf> {
        None
    }
    let mut c = Composer::new();
    c.clipboard = Clipboard {
        holds: || true,
        save: copied,
    };
    let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
    c.handle_key(ctrl_v);
    // The picture is saved off the key thread, and its chip goes in once it lands.
    for _ in 0..200 {
        if !c.pictures_arriving() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(c.take_arrived_pictures());
    assert_eq!(c.text(), "[Image #1] ");
    c.clipboard = Clipboard {
        holds: || false,
        save: nothing,
    };
    assert_eq!(c.handle_key(ctrl_v), ComposerEvent::Unhandled);
    assert_eq!(c.text(), "[Image #1] ");
    assert!(!c.take_arrived_pictures());
}

#[test]
fn a_shell_command_keeps_a_path_as_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path(), "a.png");
    let mut c = Composer::new();
    c.handle_key(key(KeyCode::Char('!')));
    c.handle_paste(&path.display().to_string());
    assert_eq!(c.text(), path.display().to_string());
}

fn send(c: &mut Composer) {
    let ComposerEvent::Submitted { .. } = c.handle_key(key(KeyCode::Enter)) else {
        panic!("not sent");
    };
}

#[test]
fn a_new_conversation_counts_chips_from_one_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path(), "one.png");
    let mut c = Composer::new();
    c.handle_paste(&dropped(&path));
    c.handle_paste(&dropped(&path));
    send(&mut c);
    c.start_conversation();
    c.handle_paste(&dropped(&path));
    assert_eq!(c.text(), "[Image #1] ");
    assert!(
        c.picture_numbers_in("[Image #2]").is_empty(),
        "the last conversation's pictures went with it"
    );

    // A chip still in the draft keeps its number, and the next counts on from it.
    c.start_conversation();
    c.handle_paste(&dropped(&path));
    assert_eq!(c.text(), "[Image #1] [Image #2] ");

    // A resumed conversation counts on after the highest chip its messages hold.
    c.clear();
    c.resume_conversation(["see [Image #4]", "and [Image #7]"]);
    c.handle_paste(&dropped(&path));
    assert_eq!(c.text(), "[Image #8] ");
}

#[test]
fn a_message_brought_back_keeps_its_pictures() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path(), "one.png");
    let mut c = Composer::new();
    for ch in "the header".chars() {
        c.handle_key(key(KeyCode::Char(ch)));
    }
    c.handle_paste(&dropped(&path));
    send(&mut c);
    let sent = c.take_sent_pictures();
    assert_eq!(c.picture_numbers_in("the header [Image #1] "), [1]);

    // Taken back from the queue, it goes again with the same picture.
    c.restore(&["the header [Image #1] ".to_owned()]);
    send(&mut c);
    assert_eq!(c.take_sent_pictures(), sent);
}
