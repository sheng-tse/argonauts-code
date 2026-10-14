mod common;

use argonauts_core::keywords::{Keyword, KeywordLook};
use argonauts_tui::HistoryCell;
use argonauts_tui::bottom::HistorySearch;
use argonauts_tui::commands::command_items;
use argonauts_tui::model::{BottomState, MenuItem, UserMessage, VimMode};
use argonauts_tui::prompt_history::PromptEntry;
use argonauts_tui::stability::{self, samples};
use argonauts_tui::theme::{ColorLevel, Glyphs, Theme};
use common::{assert_fits, ctx, still, styled, text, theme};
use crossterm::event::{KeyCode, KeyEvent};

const WIDTHS: [u16; 4] = [80, 100, 120, 200];

fn typed(text: &str) -> BottomState {
    let mut bottom = BottomState::new();
    bottom.composer.label = Some("council".into());
    bottom.menus.commands = command_items(None);
    bottom.menus.mentions = vec![
        MenuItem::new("claude-code", "opus · worker on task-002").with_kind("agent"),
        MenuItem::new(
            "codex",
            "2 sessions: worker on task-003, reviewer on task-002",
        )
        .with_kind("agent"),
        MenuItem::new("task-002", "Add the JSON writer").with_kind("task"),
        MenuItem::new("task-003", samples::CJK).with_kind("task"),
        MenuItem::new("worker", "claude-code/opus on task-002, codex on task-003")
            .with_kind("role"),
        MenuItem::new("reviewer", "codex on task-002").with_kind("role"),
        MenuItem::new("1", "claude-code/opus on task-002").with_kind("follow"),
        MenuItem::new("2", "codex on task-003").with_kind("follow"),
        MenuItem::new("all", "Every agent in this run").with_kind("run"),
    ];
    bottom.composer.set_text(text);
    bottom
}

fn snapshot_widths(name: &str, bottom: &BottomState) {
    let theme = theme();
    for width in stability::WIDTHS {
        let view = bottom.render(&ctx(&theme, width, 0.0));
        stability::assert_lines(&view.lines, width);
        assert_fits(&view.lines, width);
        if WIDTHS.contains(&width) {
            insta::assert_snapshot!(format!("{name}_{width}"), text(&view.lines));
        }
    }
}

#[test]
fn slash_menu_under_input() {
    let bottom = typed("/");
    snapshot_widths("slash_menu_under_input", &bottom);

    let theme = theme();
    let lines = bottom.render(&ctx(&theme, 80, 0.0)).lines;
    let shown = text(&lines);
    let rows: Vec<&str> = shown.lines().collect();
    let rule = rows
        .iter()
        .rposition(|row| row.starts_with('─'))
        .expect("the input's bottom rule");
    // Category names sit over their commands, which are indented under them, until you type.
    // The commands sit where they do once you type, under what you typed.
    assert_eq!(rows[rule + 1], "Session", "{shown}");
    assert!(rows[rule + 2].starts_with("  /clear"), "{shown}");
    // The rows left show the menu's first commands in its order, here all of them Session's.
    let entries: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.strip_prefix("  /"))
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    let items = command_items(None);
    let first: Vec<&str> = items
        .iter()
        .take(12)
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(entries, first, "{shown}");
    let groups: Vec<&str> = items
        .iter()
        .filter_map(|item| item.group.as_deref())
        .collect();
    assert!(
        groups[..12].iter().all(|group| *group == "Session"),
        "{groups:?}"
    );
    assert!(groups.contains(&"Work"), "{groups:?}");
    for line in &lines {
        assert!(line.width() <= 80, "one row each: {line:?}");
    }
    // Like the bottom line it covers, the menu leaves the last column empty.
    for row in rows[rule + 1..].iter() {
        assert!(row.trim_end().chars().count() < 80, "{row:?}");
    }

    let narrowed = text(&typed("/wo").render(&ctx(&theme, 80, 0.0)).lines);
    let names: Vec<&str> = narrowed
        .lines()
        .filter_map(|row| row.strip_prefix("  /"))
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    assert_eq!(names, ["workflows", "workflow"], "{narrowed}");
}

#[test]
fn slash_menu_with_the_main_agents_commands() {
    use argonauts_core::cli_commands::{
        CliCommand, CliCommandSet, CommandKind, CommandScope, CommandSource,
    };
    use argonauts_core::names::AgentId;
    let command = |name: &str, args: &str, help: &str, source: CommandSource| CliCommand {
        name: name.into(),
        aliases: Vec::new(),
        args: args.into(),
        help: help.into(),
        kind: CommandKind::Cli,
        source,
    };
    let set = CliCommandSet::new(
        AgentId("claude-code".into()),
        "Claude Code",
        &[],
        vec![
            command(
                "ship",
                "[branch]",
                "Ship the branch once the checks pass",
                CommandSource::Custom {
                    scope: CommandScope::Project,
                    path: "/repo/.claude/commands/ship.md".into(),
                },
            ),
            command(
                "shared-notes:share",
                "",
                "Share the notes with the team",
                CommandSource::Plugin {
                    plugin: "shared-notes".into(),
                },
            ),
        ],
    );
    let mut bottom = typed("/sh");
    bottom.menus.commands = command_items(Some(&set));
    snapshot_widths("slash_menu_with_the_main_agents_commands", &bottom);
}

#[test]
fn mention_list_under_input() {
    let bottom = typed("tell @");
    snapshot_widths("mention_list_under_input", &bottom);

    let theme = theme();
    let shown = text(&bottom.render(&ctx(&theme, 120, 0.0)).lines);
    let names: Vec<&str> = shown
        .lines()
        .filter_map(|row| row.strip_prefix("  @"))
        .filter_map(|row| row.split_whitespace().next())
        .collect();
    assert_eq!(
        names,
        [
            "claude-code",
            "codex",
            "task-002",
            "task-003",
            "worker",
            "reviewer",
            "1",
            "2"
        ],
        "{shown}"
    );
}

fn in_shell_mode(command: &str) -> BottomState {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut bottom = typed("");
    bottom
        .composer
        .handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
    bottom.composer.set_text(command);
    bottom
}

/// Claude Code's bash mode: a pink `!` and rules, and a bottom line that says only that.
#[test]
fn shell_mode_input_and_bottom_row() {
    let empty = in_shell_mode("");
    assert!(empty.composer.is_shell());
    snapshot_widths("shell_mode_empty", &empty);
    let typed = in_shell_mode("git status && ls /tmp");
    snapshot_widths("shell_mode_typed", &typed);

    let theme = theme();
    let lines = typed.render(&ctx(&theme, 80, 0.0)).lines;
    let pink = common::fg(argonauts_tui::theme::Role::Shell);
    let marker = common::style_of(&lines, "! ").expect("the input row");
    assert_eq!(marker.fg, pink, "the ! is pink");
    let mode = common::style_of(&lines, "! for shell mode").expect("the bottom line");
    assert_eq!(mode.fg, pink);
    let rules: Vec<_> = lines
        .iter()
        .filter(|line| common::row_text(line).starts_with('─'))
        .collect();
    assert_eq!(rules.len(), 2, "{}", text(&lines));
    for rule in rules {
        assert_eq!(rule.spans[0].style.fg, pink, "both rules are pink");
    }
}

fn with_files(text: &str, run: bool) -> BottomState {
    let mut bottom = typed("");
    if !run {
        bottom.menus.mentions.clear();
    }
    let files: Vec<String> = [
        ".gitignore",
        "Cargo.toml",
        "README.md",
        "crates/core/src/lib.rs",
        "crates/tui/src/bottom/menu.rs",
        "crates/tui/src/bottom/files.rs",
        "crates/tui/src/mentions.rs",
        "docs/manual.md",
        "tests/main_test.rs",
    ]
    .map(str::to_owned)
    .to_vec();
    bottom.menus.set_files(&files);
    bottom.composer.set_text(text);
    bottom
}

#[test]
fn file_mention_list_under_input() {
    // No run: the repo's top level, files first, as Claude Code lists it.
    let bottom = with_files("@", false);
    snapshot_widths("file_mention_list_top_level", &bottom);
    let theme = theme();
    let shown = text(&bottom.render(&ctx(&theme, 80, 0.0)).lines);
    let files: Vec<&str> = shown
        .lines()
        .filter_map(|row| row.strip_prefix("  + "))
        .map(str::trim_end)
        .collect();
    assert_eq!(
        files,
        [
            ".gitignore",
            "Cargo.toml",
            "README.md",
            "crates/",
            "docs/",
            "tests/"
        ],
        "{shown}"
    );

    // Typing narrows it anywhere in the path.
    let bottom = with_files("explain @menu", false);
    snapshot_widths("file_mention_list_filtered", &bottom);

    // In a run, the agents follow under their own heading.
    let bottom = with_files("@c", true);
    snapshot_widths("file_mention_list_with_agents", &bottom);
    let shown = text(&bottom.render(&ctx(&theme, 120, 0.0)).lines);
    let rows: Vec<&str> = shown.lines().collect();
    let heading = rows
        .iter()
        .position(|row| *row == "Agents")
        .unwrap_or_else(|| panic!("an Agents heading:\n{shown}"));
    assert!(rows[heading - 1].starts_with("  + "), "{shown}");
    assert!(rows[heading + 1].starts_with("  @claude-code"), "{shown}");
}

/// Ages count back from here.
const SEARCH_NOW: u64 = 1_800_000_000;

fn searching(vim: bool) -> BottomState {
    let at = |minutes: u64| Some(SEARCH_NOW - minutes * 60);
    let everywhere = vec![
        PromptEntry::new("Should this be a workflow?", at(37)),
        PromptEntry::new(
            "Can we add a --json option to the export command and keep CSV output as it is?",
            at(29),
        ),
        PromptEntry::new(samples::CJK, at(18)),
        PromptEntry::new(samples::EMOJI, at(12)),
        PromptEntry::new(samples::LONG_PATH, at(10)),
        PromptEntry::new(samples::long_word(), at(4)),
        PromptEntry::new(
            "Follow-ups:\n1. Make sure the flag works in other workflows\n2. Add a test for the CSV",
            at(2),
        ),
        PromptEntry::new("Show the status", at(2)),
        PromptEntry::new("/usage", at(1)),
    ];
    let project = everywhere[6..].to_vec();
    let mut bottom = BottomState::new();
    bottom.mode.vim = vim.then_some(VimMode::Normal);
    bottom.history_search = Some(HistorySearch::new(project, everywhere, SEARCH_NOW).with_vim(vim));
    bottom
}

fn snapshot_search(name: &str, bottom: &BottomState) {
    let theme = theme();
    for width in stability::WIDTHS {
        let view = bottom.render(&ctx(&theme, width, 0.0));
        stability::assert_lines(&view.lines, width);
        assert_fits(&view.lines, width);
        if [80, 100, 120].contains(&width) {
            insta::assert_snapshot!(format!("{name}_{width}"), text(&view.lines));
        }
    }
}

#[test]
fn history_search_like_claude_codes() {
    let bottom = searching(true);
    snapshot_search("history_search", &bottom);
    let theme = theme();
    let view = bottom.render(&ctx(&theme, 120, 0.0));
    let rows: Vec<String> = text(&view.lines).lines().map(str::to_owned).collect();
    assert_eq!(rows[1], "  Search prompts · everywhere");
    assert!(rows.last().unwrap().ends_with("Esc i / for slash commands"));
    // The cursor waits in the filter box, after its search mark.
    let filter = rows
        .iter()
        .position(|row| row.contains("Filter history"))
        .unwrap();
    assert_eq!(view.cursor, Some((6, filter as u16)));
}

#[test]
fn history_search_filtered() {
    let mut bottom = searching(false);
    let search = bottom.history_search.as_mut().unwrap();
    for c in "workflow".chars() {
        search.handle_key(KeyEvent::from(KeyCode::Char(c)));
    }
    search.handle_key(KeyEvent::from(KeyCode::Up));
    snapshot_search("history_search_filtered", &bottom);
}

#[test]
fn history_search_on_a_short_screen_keeps_its_filter() {
    let bottom = searching(true);
    let theme = theme();
    let view = bottom.render_within(&ctx(&theme, 100, 0.0), 9);
    let shown = text(&view.lines);
    assert_eq!(view.lines.len(), 9, "{shown}");
    assert!(shown.contains("Search prompts"), "{shown}");
    assert!(shown.contains("❯ 1m ago   /usage"), "{shown}");
    assert!(shown.contains("Filter history"), "{shown}");
}

/// Claude Code 2.1.289's keyword, as its adapter's table has it.
const ULTRATHINK: Keyword = Keyword::new("ultrathink", KeywordLook::Rainbow, true);
const KEYWORD_PROMPT: &str = "please ultrathink about this";

/// The input as the shimmer passes over the word, then the echo of the sent message, held still.
#[test]
fn keyword_rainbow_in_the_input_and_the_echo() {
    let themes = [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("ansi16", Theme::dark().with_level(ColorLevel::Ansi16)),
    ];
    for (name, mut theme) in themes {
        theme.glyphs = Glyphs::UNICODE;
        let mut bottom = BottomState::new();
        bottom.composer.keywords = vec![ULTRATHINK];
        bottom.composer.set_text(KEYWORD_PROMPT);
        // 640 ms in, the band's middle is on the word's third letter.
        let input = bottom.render(&ctx(&theme, 100, 0.64)).lines;
        let echo = UserMessage::new(KEYWORD_PROMPT)
            .with_keywords(&[ULTRATHINK])
            .lines(&still(&theme, 100));
        assert_fits(&input, 100);
        assert_fits(&echo, 100);
        assert_eq!(text(&echo), format!("❯ {KEYWORD_PROMPT}"));
        insta::assert_snapshot!(
            format!("keyword_rainbow_{name}_100"),
            format!("{}\n\n{}", styled(&input), styled(&echo))
        );
    }
}
