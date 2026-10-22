use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::Line;

use super::*;
use crate::render::{Clock, RenderCtx};
use crate::text::plain_text;
use crate::theme::{Glyphs, Role, Theme};

/// Ages count back from here.
const NOW: u64 = 1_800_000_000;

fn entry(text: &str, minutes_ago: u64) -> PromptEntry {
    PromptEntry::new(text, Some(NOW - minutes_ago * 60))
}

/// This repo's three prompts, and two more from other repos, oldest first.
fn search() -> HistorySearch {
    let project = vec![
        entry("fix the export test", 40),
        entry("Follow-ups:\n1. rename the flag\n2. add a test", 20),
        entry("/usage", 1),
    ];
    let everywhere = vec![
        entry("deploy the web app", 90),
        entry("fix the export test", 40),
        entry("migrate the api schema", 30),
        entry("Follow-ups:\n1. rename the flag\n2. add a test", 20),
        entry("/usage", 1),
    ];
    HistorySearch::new(project, everywhere, NOW)
}

fn press(search: &mut HistorySearch, code: KeyCode) -> SearchEvent {
    search.handle_key(KeyEvent::from(code))
}

fn ctrl(search: &mut HistorySearch, c: char) -> SearchEvent {
    search.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn type_text(search: &mut HistorySearch, text: &str) {
    for c in text.chars() {
        press(search, KeyCode::Char(c));
    }
}

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn render(search: &HistorySearch, width: u16, max_rows: usize) -> (Vec<Line<'static>>, (u16, u16)) {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::still(Instant::now()));
    search.lines(&ctx, max_rows)
}

fn rows(search: &HistorySearch, width: u16) -> Vec<String> {
    let (lines, _) = render(search, width, usize::MAX);
    plain_text(&lines).lines().map(str::to_owned).collect()
}

#[test]
fn it_opens_on_everywhere_with_the_pointer_on_the_newest() {
    let search = search();
    assert_eq!(search.scope(), SearchScope::Everywhere);
    assert_eq!(search.shown().len(), 5);
    assert_eq!(search.shown()[0], "deploy the web app");
    assert_eq!(search.selected(), Some("/usage"));
}

#[test]
fn ctrl_s_switches_between_this_project_and_everywhere() {
    let mut search = search();
    assert_eq!(ctrl(&mut search, 's'), SearchEvent::Moved);
    assert_eq!(search.scope(), SearchScope::Project);
    assert_eq!(
        search.shown(),
        [
            "fix the export test",
            "Follow-ups:\n1. rename the flag\n2. add a test",
            "/usage"
        ]
    );
    assert!(rows(&search, 100)[1].starts_with("  Search prompts · this project"));

    // The filter stays and applies to the other scope.
    type_text(&mut search, "mig");
    assert!(search.shown().is_empty());
    ctrl(&mut search, 's');
    assert_eq!(search.scope(), SearchScope::Everywhere);
    assert_eq!(search.shown(), ["migrate the api schema"]);
}

#[test]
fn typing_filters_fuzzily_with_the_best_match_last() {
    let mut search = search();
    type_text(&mut search, "ft");
    // `fix the export test` holds f and t close, at word starts; `Follow-ups … the flag` holds
    // them far apart, so it ranks lower though it's newer.
    assert_eq!(search.filter(), "ft");
    assert_eq!(
        search.shown(),
        [
            "Follow-ups:\n1. rename the flag\n2. add a test",
            "fix the export test"
        ]
    );
    assert_eq!(search.selected(), Some("fix the export test"));

    // A filter matches across the lines of a prompt, as it lists them on one.
    ctrl(&mut search, 'u');
    type_text(&mut search, "ups: 1.");
    assert_eq!(
        search.shown(),
        ["Follow-ups:\n1. rename the flag\n2. add a test"]
    );

    press(&mut search, KeyCode::Backspace);
    press(&mut search, KeyCode::Backspace);
    assert_eq!(search.filter(), "ups: ");
    ctrl(&mut search, 'w');
    assert_eq!(search.filter(), "");
    assert_eq!(
        search.shown().len(),
        5,
        "an empty filter shows them all again"
    );
    assert_eq!(search.selected(), Some("/usage"));
}

#[test]
fn ctrl_w_drops_the_last_word() {
    let mut search = search();
    type_text(&mut search, "fix the exp");
    ctrl(&mut search, 'w');
    assert_eq!(search.filter(), "fix the ");
    ctrl(&mut search, 'w');
    assert_eq!(search.filter(), "fix ");
}

#[test]
fn ctrl_w_cuts_after_a_space_of_more_than_one_byte() {
    // Option+Space on macOS types U+00A0, and a CJK input method types U+3000.
    for (typed, kept) in [
        ("fix\u{a0}tests", "fix\u{a0}"),
        ("修复\u{3000}测试", "修复\u{3000}"),
        ("修复\u{3000}测试\u{3000}", "修复\u{3000}"),
    ] {
        let mut search = search();
        search.handle_paste(typed);
        assert_eq!(search.filter(), typed.trim_end());
        assert_eq!(ctrl(&mut search, 'w'), SearchEvent::Moved);
        assert_eq!(search.filter(), kept, "{typed:?}");
        ctrl(&mut search, 'w');
        assert_eq!(search.filter(), "");
    }
}

#[test]
fn arrows_move_and_stop_at_the_ends() {
    let mut search = search();
    press(&mut search, KeyCode::Down);
    assert_eq!(search.selected(), Some("/usage"));
    press(&mut search, KeyCode::Up);
    assert_eq!(
        search.selected(),
        Some("Follow-ups:\n1. rename the flag\n2. add a test")
    );
    ctrl(&mut search, 'p');
    ctrl(&mut search, 'r');
    assert_eq!(search.selected(), Some("fix the export test"));
    press(&mut search, KeyCode::PageUp);
    assert_eq!(search.selected(), Some("deploy the web app"));
    press(&mut search, KeyCode::Up);
    assert_eq!(search.selected(), Some("deploy the web app"));
    ctrl(&mut search, 'n');
    assert_eq!(search.selected(), Some("fix the export test"));
    press(&mut search, KeyCode::PageDown);
    assert_eq!(search.selected(), Some("/usage"));
}

#[test]
fn enter_uses_the_whole_prompt_and_esc_cancels() {
    let mut search = search();
    press(&mut search, KeyCode::Up);
    assert_eq!(
        press(&mut search, KeyCode::Enter),
        SearchEvent::Use("Follow-ups:\n1. rename the flag\n2. add a test".into())
    );
    assert_eq!(press(&mut search, KeyCode::Esc), SearchEvent::Cancel);
    assert_eq!(ctrl(&mut search, 'c'), SearchEvent::Cancel);
    assert_eq!(ctrl(&mut search, 'g'), SearchEvent::Cancel);
    assert_eq!(ctrl(&mut search, 'd'), SearchEvent::Unhandled);

    // With nothing matching, Enter has nothing to use.
    type_text(&mut search, "zzz");
    assert_eq!(press(&mut search, KeyCode::Enter), SearchEvent::Moved);
}

#[test]
fn a_paste_filters_on_one_line() {
    let mut search = search();
    search.handle_paste("web\napp\n");
    assert_eq!(search.filter(), "web app");
    assert_eq!(search.shown(), ["deploy the web app"]);

    // Terminals and tmux send a pasted line break as CR.
    for pasted in ["the\rexport", "the\r\nexport\r"] {
        let mut fresh = self::search();
        fresh.handle_paste(pasted);
        assert_eq!(fresh.filter(), "the export");
        assert_eq!(fresh.shown(), ["fix the export test"]);
    }
}

#[test]
fn alt_and_a_letter_is_esc_then_the_letter() {
    // A quick Esc i comes from the terminal as Alt+i.
    let mut search = search();
    let alt_i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT);
    assert_eq!(
        search.handle_key(alt_i),
        SearchEvent::CancelThen(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
    );
    let alt_shift_a = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(
        search.handle_key(alt_shift_a),
        SearchEvent::CancelThen(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT))
    );
    assert_eq!(search.filter(), "", "nothing went in the filter");
    // AltGr still types.
    let altgr = KeyEvent::new(
        KeyCode::Char('@'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(search.handle_key(altgr), SearchEvent::Moved);
    assert_eq!(search.filter(), "@");
}

#[test]
fn enter_inside_a_paste_typed_out_as_keys_never_picks_a_prompt() {
    let at = |ms: u64| Instant::now() + Duration::from_millis(ms);
    let t0 = at(0);
    let key = |c: char| KeyEvent::from(KeyCode::Char(c));
    let enter = KeyEvent::from(KeyCode::Enter);
    let mut search = search();
    for (n, c) in "exp".chars().enumerate() {
        search.handle_key_at(key(c), t0 + Duration::from_millis(n as u64));
    }
    // "exp\r\rweb" in one write: both Enters are line breaks in the paste.
    assert_eq!(
        search.handle_key_at(enter, t0 + Duration::from_millis(3)),
        SearchEvent::Moved
    );
    assert_eq!(
        search.handle_key_at(enter, t0 + Duration::from_millis(4)),
        SearchEvent::Moved
    );
    for (n, c) in "test".chars().enumerate() {
        search.handle_key_at(key(c), t0 + Duration::from_millis(5 + n as u64));
    }
    assert_eq!(search.filter(), "exp test");
    assert_eq!(search.shown(), ["fix the export test"]);
    // A chunk of the same paste a little later still belongs to it.
    assert_eq!(
        search.handle_key_at(enter, t0 + Duration::from_millis(60)),
        SearchEvent::Moved
    );

    // Enter a moment after typing picks the prompt.
    assert_eq!(
        search.handle_key_at(enter, t0 + Duration::from_millis(400)),
        SearchEvent::Use("fix the export test".into())
    );

    // Two keys close together are typing, not a paste.
    let mut search = self::search();
    search.handle_key_at(key('/'), t0);
    search.handle_key_at(key('u'), t0 + Duration::from_millis(100));
    search.handle_key_at(key('s'), t0 + Duration::from_millis(105));
    assert_eq!(
        search.handle_key_at(enter, t0 + Duration::from_millis(200)),
        SearchEvent::Use("/usage".into())
    );
}

#[test]
fn the_list_scrolls_to_keep_the_pointer_in_view() {
    let everywhere: Vec<PromptEntry> = (0..20)
        .map(|n| entry(&format!("prompt {n:02}"), 50 - n))
        .collect();
    let mut search = HistorySearch::new(Vec::new(), everywhere, NOW);
    let list =
        |search: &HistorySearch| -> Vec<String> { rows(search, 100)[2..2 + LIST_ROWS].to_vec() };
    let shown = list(&search);
    assert!(shown[0].starts_with("  ↑ 38m ago  prompt 12"), "{shown:#?}");
    assert!(shown[7].starts_with("  ❯ 31m ago  prompt 19"), "{shown:#?}");
    for _ in 0..8 {
        press(&mut search, KeyCode::Up);
    }
    let shown = list(&search);
    assert!(shown[0].starts_with("  ❯ 39m ago  prompt 11"), "{shown:#?}");
    assert!(shown[7].starts_with("  ↓ 32m ago  prompt 18"), "{shown:#?}");
}

#[test]
fn it_reads_like_claude_codes() {
    let search = search().with_vim(true);
    let rows = rows(&search, 120);
    let expected = ["─".repeat(120), "  Search prompts · everywhere".to_owned()];
    assert_eq!(rows[..2], expected);
    // Two columns between the age and the prompt, and two before the box.
    assert!(
        rows[2].starts_with("    1h ago   deploy the web app "),
        "{rows:#?}"
    );
    // A multi-line prompt lists its first line, as Claude Code's does, and shows whole in
    // the box beside the list.
    assert!(
        rows[5].starts_with("    20m ago  Follow-ups: "),
        "{rows:#?}"
    );
    assert!(!rows[5].contains("rename"), "{rows:#?}");
    assert!(rows[6].starts_with("  ❯ 1m ago   /usage "), "{rows:#?}");
    assert!(
        rows[6].contains(&format!("/usage{}╰", " ".repeat(43))),
        "{rows:#?}"
    );
    let boxed = 56;
    assert!(
        rows[2].ends_with(&format!("╭{}╮", "─".repeat(boxed - 2))),
        "{rows:#?}"
    );
    assert!(
        rows[3].ends_with(&format!("│ {:<53}│", "/usage")),
        "{rows:#?}"
    );
    // A list of five is that tall, so the panel doesn't jump as you filter.
    assert!(
        rows[6].ends_with(&format!("╰{}╯", "─".repeat(boxed - 2))),
        "{rows:#?}"
    );
    assert_eq!(rows[7], format!("  ╭{}╮", "─".repeat(114)));
    assert_eq!(rows[8], format!("  │ ⌕ Filter history…{}│", " ".repeat(96)));
    assert_eq!(rows[9], format!("  ╰{}╯", "─".repeat(114)));
    assert_eq!(
        rows[10],
        "  ↑/↓  to navigate · Enter to use · Esc to cancel · ctrl+s to scope · Esc i / for slash commands"
    );
    assert_eq!(rows.len(), 11);
}

#[test]
fn the_preview_shows_a_multi_line_prompt_whole() {
    let mut search = search();
    press(&mut search, KeyCode::Up);
    let rows = rows(&search, 120);
    let preview: Vec<&str> = rows[3..6]
        .iter()
        .map(|row| row[row.find('│').unwrap()..].trim_end())
        .collect();
    assert_eq!(
        preview,
        [
            format!("│ {:<53}│", "Follow-ups:"),
            format!("│ {:<53}│", "1. rename the flag"),
            format!("│ {:<53}│", "2. add a test"),
        ]
    );
}

#[test]
fn the_preview_drops_blank_lines_at_either_end() {
    // Five prompts make the box five rows tall, three of them text.
    let everywhere = vec![
        entry("deploy", 5),
        entry("migrate", 4),
        entry("lint", 3),
        entry("\n\nfirst\n\nsecond\n\n\n", 2),
        entry("one\ntwo\nthree\nfour", 1),
    ];
    let mut search = HistorySearch::new(Vec::new(), everywhere, NOW);
    let box_text = |search: &HistorySearch| -> Vec<String> {
        rows(search, 120)[3..6]
            .iter()
            .map(|row| row[row.find('│').unwrap()..].trim_end().to_owned())
            .collect()
    };
    // Three rows of text fit; the fourth line shows as the third row's ellipsis.
    assert_eq!(
        box_text(&search),
        [
            format!("│ {:<53}│", "one"),
            format!("│ {:<53}│", "two"),
            format!("│ {:<53}│", "three …"),
        ]
    );
    press(&mut search, KeyCode::Up);
    assert_eq!(
        box_text(&search),
        [
            format!("│ {:<53}│", "first"),
            format!("│ {:<53}│", ""),
            format!("│ {:<53}│", "second"),
        ]
    );
}

#[test]
fn matched_letters_are_highlighted() {
    let mut search = search();
    type_text(&mut search, "dwa");
    let (lines, cursor) = render(&search, 100, usize::MAX);
    let row = lines
        .iter()
        .find(|line| plain_text(std::slice::from_ref(*line)).contains("deploy"))
        .unwrap();
    let lit: String = row
        .spans
        .iter()
        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
        .map(|span| span.content.as_ref())
        .collect();
    assert_eq!(lit, "dwa");
    let theme = theme();
    let pointed = row.spans.iter().find(|span| span.content == "d").unwrap();
    assert_eq!(
        pointed.style.fg,
        theme.style(Role::Question).fg,
        "the pointed row is in the task color"
    );

    // The cursor waits after the filter, inside its box.
    let filter_row = &lines[usize::from(cursor.1)];
    assert!(plain_text(std::slice::from_ref(filter_row)).starts_with("  │ ⌕ dwa "));
    assert_eq!(cursor.0, 9);
}

#[test]
fn the_preview_is_dim_and_the_magnifier_is_in_the_text_color() {
    let search = search();
    let (lines, _) = render(&search, 120, usize::MAX);
    let theme = theme();
    let boxed = lines[3].spans.iter().find(|span| span.content == "/usage");
    assert_eq!(boxed.unwrap().style, theme.style(Role::Dim));
    let edge = lines[3].spans.last().unwrap();
    assert_eq!(edge.content, "│");
    assert_eq!(
        edge.style,
        theme.style(Role::Text).add_modifier(Modifier::DIM),
        "the box's border is the text color, dim"
    );

    let filter = &lines[8];
    let magnifier = filter.spans.iter().find(|span| span.content.contains('⌕'));
    assert_eq!(magnifier.unwrap().style, theme.style(Role::Text));
    let border = filter.spans.iter().find(|span| span.content == "│");
    assert_eq!(border.unwrap().style, theme.style(Role::Question));
}

#[test]
fn a_filter_finds_a_prompt_by_a_later_line() {
    let mut search = search();
    type_text(&mut search, "rename");
    assert_eq!(
        press(&mut search, KeyCode::Enter),
        SearchEvent::Use("Follow-ups:\n1. rename the flag\n2. add a test".to_owned())
    );
}

#[test]
fn nothing_to_show_says_so() {
    let mut search = search();
    type_text(&mut search, "qqq");
    assert!(rows(&search, 80)[2].starts_with("  No matching prompts"));
    let empty = HistorySearch::new(Vec::new(), Vec::new(), NOW);
    let rows = rows(&empty, 80);
    assert!(rows[2].starts_with("  No prompts yet"), "{rows:#?}");
    assert_eq!(rows.len(), 3 + 6, "three list rows at least");
}

#[test]
fn narrow_or_short_screens_keep_the_filter_and_hints() {
    let search = search();
    let narrow = rows(&search, 50);
    assert!(
        !narrow
            .iter()
            .any(|row| row.contains('╮') && row.contains("ago")),
        "no preview"
    );
    assert!(
        narrow[2].starts_with("    1h ago   deploy the web app"),
        "{narrow:#?}"
    );
    assert!(
        narrow
            .last()
            .unwrap()
            .starts_with("  ↑/↓  to navigate · Enter to use · Esc to cancel")
    );
    assert!(
        !narrow.last().unwrap().contains("Esc i /"),
        "no vim hint without vim keys"
    );

    let (short, _) = render(&search, 100, 8);
    assert_eq!(short.len(), 8, "two list rows in eight");
    let short = plain_text(&short);
    assert!(short.contains("❯ 1m ago"), "{short}");
}

#[test]
fn a_huge_prompt_draws_only_what_fits() {
    // A pasted log of about 2 MB, in long lines and one line with no break at all.
    let log = "error: the build failed at step 42, see the trace below\n".repeat(36_000);
    let one_line = "x".repeat(2_000_000);
    for text in [log, one_line] {
        let mut search = HistorySearch::new(
            Vec::new(),
            vec![entry("fix the export test", 9), entry(&text, 1)],
            NOW,
        );
        let start = Instant::now();
        for _ in 0..10 {
            let rows = rows(&search, 120);
            // The list row and the box's one row of text both end in an ellipsis.
            assert!(rows[3].starts_with("  ❯ 1m ago   "), "{rows:#?}");
            assert_eq!(rows[3].matches('…').count(), 2, "{rows:#?}");
            assert!(
                rows.iter().all(|row| row.chars().count() <= 120),
                "{rows:#?}"
            );
        }
        let ten_frames = start.elapsed();
        assert!(ten_frames < Duration::from_secs(2), "{ten_frames:?}");
        // The preview is kept until the pointer moves.
        assert!(search.preview.borrow().is_some());
        press(&mut search, KeyCode::Up);
        let rows = rows(&search, 120);
        assert!(rows[3].contains("│ fix the export test"), "{rows:#?}");
    }
}

#[test]
fn typing_on_narrows_what_is_shown() {
    let filtered = |filter: &str| {
        let mut fresh = search();
        fresh.handle_paste(filter);
        fresh
            .shown()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let mut search = search();
    type_text(&mut search, "t");
    assert_eq!(search.shown(), filtered("t"));
    type_text(&mut search, "es");
    assert_eq!(search.shown(), filtered("tes"));
    // Taking letters back, or a new start, widens it again.
    press(&mut search, KeyCode::Backspace);
    assert_eq!(search.shown(), filtered("te"));
    ctrl(&mut search, 'u');
    type_text(&mut search, "dep");
    assert_eq!(search.shown(), filtered("dep"));
}

#[test]
fn ages_read_like_claude_codes() {
    assert_eq!(age(NOW, NOW), "now");
    assert_eq!(age(NOW - 30, NOW), "30s ago");
    assert_eq!(age(NOW - 37 * 60, NOW), "37m ago");
    assert_eq!(age(NOW - 2 * 3_600, NOW), "2h ago");
    assert_eq!(age(NOW - 3 * 86_400, NOW), "3d ago");
    assert_eq!(age(NOW - 14 * 86_400, NOW), "2w ago");
    assert_eq!(age(NOW - 60 * 86_400, NOW), "2mo ago");
    assert_eq!(age(0, 3 * 365 * 86_400), "3y ago");
    // A clock that went back reads as now.
    assert_eq!(age(NOW + 5, NOW), "now");
}
