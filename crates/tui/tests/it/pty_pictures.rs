//! Pictures through a pty that reports its size in pixels, as Ghostty does: the header's mascot
//! goes out drawn for the terminal's cells, and a font size change, which can keep the rows and
//! columns, takes it off and sends it again drawn for the new cells.

use std::thread;
use std::time::Duration;

use argonauts_testkit::pty::deadline;
use base64::Engine;

use crate::pty_support::*;

const POLL: Duration = Duration::from_millis(20);

/// A picture the program sent with the kitty protocol: its id, and its size in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    id: u32,
    size: (u32, u32),
}

#[test]
fn a_font_size_change_sends_the_mascot_again_drawn_for_the_new_cells() {
    let mut driver = TestDriver::launch(Script::idle())
        .env("TERM_PROGRAM", "ghostty")
        .start();
    let before = wait_sent(&driver, 0, "the mascot", |sent| !sent.is_empty());
    let mascot = before[0].clone();

    // A bigger font: the same rows and columns, each cell twice as many pixels.
    let from = driver.raw().len();
    driver.resize_with_cell(ROWS, COLS, (20, 40));
    let after = wait_sent(
        &driver,
        from,
        "the mascot drawn for the new cells",
        |sent| sent.iter().any(|picture| picture.id != mascot.id),
    );
    let again = after
        .iter()
        .find(|picture| picture.id != mascot.id)
        .expect("a new picture");
    assert_eq!(again.size, (mascot.size.0 * 2, mascot.size.1 * 2));

    let raw = String::from_utf8_lossy(&driver.raw()[from..]).into_owned();
    let freed = format!("a=d,d=I,q=2,i={}\x1b\\", mascot.id);
    let placed = format!("i={}", again.id);
    let freed_at = raw.find(&freed).unwrap_or_else(|| panic!("freed: {raw:?}"));
    let placed_at = raw
        .find(&placed)
        .unwrap_or_else(|| panic!("placed: {raw:?}"));
    assert!(freed_at < placed_at, "{raw:?}");
}

/// Waits until the pictures sent after byte `from` of the program's output pass `done`.
fn wait_sent(
    driver: &TestDriver,
    from: usize,
    what: &str,
    done: impl Fn(&[Sent]) -> bool,
) -> Vec<Sent> {
    let deadline = deadline::after(WAIT);
    loop {
        let raw = driver.raw();
        let sent = pictures(&raw[from.min(raw.len())..]);
        if done(&sent) {
            return sent;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; sent {sent:?}; the screen was:\n{}",
            driver.text()
        );
        thread::sleep(POLL);
    }
}

/// Every picture the kitty protocol carries in `raw`, in the order sent: a transmission starts
/// with `a=T` or `a=t` and goes on through its chunks while they say `m=1`.
fn pictures(raw: &[u8]) -> Vec<Sent> {
    let text = String::from_utf8_lossy(raw);
    let mut sent = Vec::new();
    let mut open: Option<(u32, String)> = None;
    for escape in text.split("\x1b_G").skip(1) {
        let Some((body, _)) = escape.split_once("\x1b\\") else {
            break;
        };
        let (keys, payload) = body.split_once(';').unwrap_or((body, ""));
        let key = |name: &str| {
            keys.split(',')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        };
        if matches!(key("a"), Some("T" | "t")) {
            let id = key("i").and_then(|id| id.parse().ok()).unwrap_or_default();
            open = Some((id, String::new()));
        }
        let Some((_, data)) = open.as_mut() else {
            continue;
        };
        data.push_str(payload);
        if key("m") != Some("1")
            && let Some((id, data)) = open.take()
            && let Some(size) = png_size(&data)
        {
            sent.push(Sent { id, size });
        }
    }
    sent
}

/// A PNG's width and height, from its header, in a base64 payload.
fn png_size(data: &str) -> Option<(u32, u32)> {
    let png = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let word = |at: usize| Some(u32::from_be_bytes(png.get(at..at + 4)?.try_into().ok()?));
    Some((word(16)?, word(20)?))
}
