use std::collections::HashMap;

use super::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    move |name| vars.get(name).cloned()
}

#[test]
fn ghostty_and_kitty_draw_placeholders_and_wezterm_does_not() {
    assert!(terminal_draws_them(env(&[("TERM_PROGRAM", "ghostty")])));
    assert!(terminal_draws_them(env(&[("TERM", "xterm-kitty")])));
    assert!(terminal_draws_them(env(&[("KITTY_WINDOW_ID", "1")])));
    assert!(terminal_draws_them(env(&[(
        "GHOSTTY_RESOURCES_DIR",
        "/Applications/Ghostty.app/Contents/Resources/ghostty"
    )])));
    assert!(!terminal_draws_them(env(&[("TERM_PROGRAM", "WezTerm")])));
    assert!(!terminal_draws_them(env(&[
        ("WEZTERM_PANE", "0"),
        ("GHOSTTY_RESOURCES_DIR", "/Applications/Ghostty.app")
    ])));
    assert!(!terminal_draws_them(env(&[("TERM_PROGRAM", "iTerm.app")])));
    assert!(!terminal_draws_them(env(&[("TERM", "xterm-256color")])));
}

#[test]
fn a_terminal_started_from_ghostty_is_not_taken_for_it() {
    let vscode = env(&[
        ("TERM_PROGRAM", "vscode"),
        ("GHOSTTY_RESOURCES_DIR", "/Applications/Ghostty.app"),
    ]);
    assert!(!terminal_draws_them(vscode));
}

#[test]
fn nothing_goes_through_a_multiplexer() {
    let tmux = env(&[
        ("TERM_PROGRAM", "ghostty"),
        ("TMUX", "/tmp/tmux-501/default"),
    ]);
    assert!(!terminal_draws_them(tmux));
    let screen = env(&[("TERM", "screen-256color"), ("KITTY_WINDOW_ID", "1")]);
    assert!(!terminal_draws_them(screen));
}

#[test]
fn the_query_asks_for_one_pixel_then_the_attributes() {
    assert_eq!(
        query(),
        b"\x1b_Gi=19537,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c".to_vec()
    );
}

#[test]
fn only_an_ok_before_the_attributes_counts_as_an_answer() {
    assert!(query_answered(b"\x1b_Gi=19537;OK\x1b\\\x1b[?62;22;52c"));
    assert!(!query_answered(b"\x1b[?62;22c"));
    assert!(!query_answered(
        b"\x1b_Gi=19537;EINVAL:unsupported format\x1b\\\x1b[?62;22c"
    ));
    assert!(!query_answered(b"\x1b_Gi=7;OK\x1b\\\x1b[?62;22c"));
    // An answer cut off at the deadline, with no attributes after it, is no answer.
    assert!(!query_answered(b"\x1b_Gi=19537;OK\x1b\\"));
    assert!(!query_answered(b""));
}

#[test]
fn a_picture_goes_out_with_a_virtual_placement_and_no_reply() {
    let png = [0x89, b'P', b'N', b'G'];
    let sent = String::from_utf8(send(&png, 0x4c4f02, (2, 1))).unwrap();
    assert_eq!(
        sent,
        "\x1b_Ga=t,f=100,q=2,i=5000962,m=0;iVBORw==\x1b\\\x1b_Ga=p,U=1,i=5000962,c=2,r=1,q=2\x1b\\"
    );
}

#[test]
fn each_cell_names_its_row_and_column() {
    assert_eq!(cell(0, 0).unwrap(), "\u{10EEEE}\u{0305}\u{0305}");
    assert_eq!(cell(0, 1).unwrap(), "\u{10EEEE}\u{0305}\u{030D}");
    assert_eq!(cell(2, 9).unwrap(), "\u{10EEEE}\u{030E}\u{034A}");
    assert_eq!(cell(0, MOST_CELLS), None);
}

#[test]
fn a_placeholder_cell_takes_one_column() {
    use unicode_width::UnicodeWidthStr;
    for column in 0..MOST_CELLS {
        assert_eq!(cell(0, column).unwrap().width(), 1, "column {column}");
    }
}

#[test]
fn the_foreground_colour_carries_the_id() {
    assert_eq!(id_colour(0x4c4f02), Color::Rgb(0x4c, 0x4f, 0x02));
    assert_eq!(id_colour(7), Color::Rgb(0, 0, 7));
}
