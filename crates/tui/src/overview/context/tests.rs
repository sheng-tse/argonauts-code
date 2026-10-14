use argonauts_core::events::context::{ContextKind, ContextSkill, McpTool};

use super::example::{self, Example};
use super::*;
use crate::stability;
use crate::state::ContextCount;
use crate::text::{plain_text, spans_width};
use crate::theme::{ColorLevel, Glyphs, Theme};

fn plain() -> Theme {
    let mut theme = Theme::dark().with_level(ColorLevel::None);
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The tab's rows from `example` at `width`, a blank row between blocks, with `count` in
/// place of the example's own.
fn rows_with(example: &Example, count: Option<&ContextCount>, width: usize) -> Vec<String> {
    rows_titled(example, count, width, example::RUN_TITLE)
}

/// [`rows_with`], the example's run named `title`.
fn rows_titled(
    example: &Example,
    count: Option<&ContextCount>,
    width: usize,
    title: &str,
) -> Vec<String> {
    let theme = plain();
    let kit = Kit::new(&theme);
    let run_title = |_: &RunId| title.to_owned();
    let tab = ContextTab {
        view: &example.view,
        names: &example.names,
        count,
        main: None,
        per_turn: Some(example::PER_TURN),
        places: &example.places,
        now: example.now,
        run_title: &run_title,
        ranked: &[],
    };
    let mut rows = Vec::new();
    for section in tab.sections(&kit, width) {
        if !rows.is_empty() {
            rows.push(String::new());
        }
        for line in &section.drawn.lines {
            assert!(
                spans_width(&line.spans) <= width,
                "at {width}: {}",
                plain_text(std::slice::from_ref(line))
            );
        }
        rows.extend(
            plain_text(&section.drawn.lines)
                .lines()
                .map(|row| row.trim_end().to_owned()),
        );
    }
    rows
}

fn rows(example: &Example, width: usize) -> Vec<String> {
    rows_with(example, Some(&example.count), width)
}

fn row<'a>(rows: &'a [String], part: &str) -> &'a str {
    rows.iter()
        .find(|row| row.contains(part))
        .unwrap_or_else(|| panic!("{part}: {rows:#?}"))
}

#[test]
fn every_list_shows_in_full_under_a_header_with_its_count_and_total() {
    let mut example = example::at_three_quarters();
    let skills = example.count.usage.skills.as_mut().unwrap();
    skills.items = (0..40)
        .map(|n| ContextSkill {
            name: format!("skill-{n:02}"),
            source: "userSettings".to_owned(),
            plugin: None,
            tokens: 100 + n,
        })
        .collect();
    example.count.usage.mcp_tools = (0..30)
        .map(|n| McpTool {
            name: format!("mcp__docs__tool_{n:02}"),
            server: "docs".to_owned(),
            tokens: 200,
            loaded: n % 2 == 0,
        })
        .collect();
    let shown = rows(&example, 118);
    assert!(row(&shown, "Skills · /skills").ends_with("40 skills  710"));
    assert!(
        row(&shown, "MCP tools · /mcp (loaded on-demand)")
            .ends_with("30 tools · 3k loaded · 3k on demand")
    );
    for n in 0..40 {
        row(&shown, &format!("skill-{n:02}"));
    }
    for n in 0..30 {
        row(&shown, &format!("tool_{n:02}"));
    }
    assert!(!shown.iter().any(|row| row.contains(" more")), "{shown:#?}");
    // The largest first inside a group, and the last closes the tree.
    assert!(row(&shown, "skill-39").contains("User"), "{shown:#?}");
    assert!(row(&shown, "skill-00").contains("└ skill-00"), "{shown:#?}");
}

#[test]
fn without_tools_on_demand_the_tools_are_one_list() {
    let mut example = example::at_three_quarters();
    let usage = &mut example.count.usage;
    usage
        .categories
        .retain(|row| row.kind != ContextKind::Deferred);
    for tool in &mut usage.mcp_tools {
        tool.loaded = true;
    }
    let shown = rows(&example, 118);
    assert!(
        row(&shown, "MCP tools · /mcp").ends_with("5 tools · 4.5k"),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|row| row.contains("on-demand")));
    assert!(!shown.iter().any(|row| row.trim() == "Loaded"));
    assert!(!shown.iter().any(|row| row.contains("Available")));
    assert!(row(&shown, "search_issues").contains("└ search_issues"));
    // Nothing loads on demand, so the line that says what does is gone.
    assert!(!shown.iter().any(|row| row.contains("Loaded on demand")));
}

#[test]
fn a_section_with_nothing_in_it_is_left_out() {
    let mut example = example::at_three_quarters();
    let usage = &mut example.count.usage;
    usage.mcp_tools.clear();
    usage.agents.clear();
    usage.memory_files.clear();
    usage.skills = None;
    usage.messages = None;
    usage.api_usage = None;
    example.view.rows.truncate(1);
    let shown = rows(&example, 118);
    for gone in [
        "MCP tools ·",
        "Custom agents ·",
        "Memory files ·",
        "Skills ·",
        "Messages ·",
        "Tool results",
        "Suggestions",
        "Last request",
        "Crew agents",
    ] {
        assert!(
            !shown.iter().any(|row| row.contains(gone)),
            "{gone}: {shown:#?}"
        );
    }
    // The grid, the legend and the window's line stay.
    row(&shown, "Estimated usage by category");
    row(&shown, "Auto-compact window:");
}

#[test]
fn with_auto_compact_off_the_reserve_is_an_ordinary_row_and_the_window_is_the_limit() {
    let mut example = example::at_three_quarters();
    let usage = &mut example.count.usage;
    usage.auto_compact = false;
    usage.percentage = 60;
    for row in &mut usage.categories {
        if row.kind == ContextKind::Buffer {
            row.name = "Compact buffer".to_owned();
            row.tokens = 3_000;
        }
    }
    let shown = rows(&example, 118);
    let reserve = row(&shown, "Compact buffer");
    assert!(reserve.contains("⛁ Compact buffer"), "{shown:#?}");
    assert!(
        row(&shown, "Auto-compact window:").contains("never compacts by itself · 250k left"),
        "{shown:#?}"
    );
    // Claude Code's note for a half-full context that won't compact.
    row(&shown, "ℹ Autocompact is disabled");
    row(&shown, "Without autocompact, you will hit context limits");
}

#[test]
fn a_context_past_its_window_says_so_under_the_title() {
    let mut example = example::near_full();
    let usage = &mut example.count.usage;
    usage.total_tokens = 1_012_000;
    usage.percentage = 101;
    let shown = rows(&example, 118);
    assert!(shown[0].contains("1M/1M tokens (101%)"), "{shown:#?}");
    assert_eq!(
        shown[1].trim(),
        "Context is 12k tokens past the 1M-token compaction window — run /compact to reduce usage."
    );
    assert!(
        !shown.iter().any(|row| row.contains("% full")),
        "{shown:#?}"
    );
}

#[test]
fn a_session_its_cli_has_not_counted_draws_as_one_bar() {
    let example = example::at_three_quarters();
    let shown = rows_with(&example, None, 118);
    assert!(shown[0].contains("Context Usage"), "{shown:#?}");
    assert!(shown[0].contains("Opus 5.5 on Claude Code"), "{shown:#?}");
    assert!(shown[0].ends_with("750k/1M tokens (75%)"), "{shown:#?}");
    assert!(shown[2].contains("not counted yet"), "{shown:#?}");
    assert!(shown[2].contains('━'), "{shown:#?}");
    // A CLI that never says what fills its sessions is named for it.
    let mut silent = example::at_three_quarters();
    silent.view.rows[0].counts = false;
    let shown = rows_with(&silent, None, 118);
    assert!(
        shown[2].contains("Claude Code says how full, not with what"),
        "{shown:#?}"
    );
    let shown = rows_with(&example, None, 118);
    // The crew follows as ever.
    row(&shown, "Crew agents · Add due dates");

    let mut alone = example::at_three_quarters();
    alone.view.rows.clear();
    let shown = rows_with(&alone, None, 118);
    assert_eq!(shown.len(), 2, "{shown:#?}");
    assert!(shown[1].contains("Not counted yet"), "{shown:#?}");
}

#[test]
fn each_run_has_its_own_crew_section() {
    let mut example = example::at_three_quarters();
    let other = RunId::new("run-08").unwrap();
    let mut row_of_other = example.view.rows[2].clone();
    row_of_other.run = Some(other);
    row_of_other.label = "review:due-flag".to_owned();
    example.view.rows.push(row_of_other);
    let shown = rows(&example, 118);
    let headers: Vec<&String> = shown
        .iter()
        .filter(|row| row.contains("Crew agents ·"))
        .collect();
    assert_eq!(headers.len(), 2, "{shown:#?}");
    assert!(headers[0].ends_with("2 agents  49.6k"), "{shown:#?}");
    assert!(headers[1].ends_with("1 agent  13.4k"), "{shown:#?}");
}

/// Narrower than the columns it was designed on, the tab keeps one list a row, and narrower
/// still it sets the legend under the grid. No row ever passes the panel's edge.
#[test]
fn a_narrow_panel_stacks_the_lists_and_keeps_every_row_inside() {
    let example = example::near_full();
    for width in stability::WIDTHS
        .map(usize::from)
        .into_iter()
        .chain([118, 150])
    {
        let shown = rows(&example, width);
        if width >= 62 {
            for part in [
                "Context Usage",
                "Estimated usage by category",
                "Auto-compact window:",
                "MCP tools",
                "Custom agents",
                "Memory files",
                "Skills",
                "Messages",
                "Tool results",
                "Suggestions",
                "Crew agents",
                "build:due-tests",
            ] {
                row(&shown, part);
            }
        }
    }
    let medium = rows(&example, 100);
    assert!(
        row(&medium, "⛁ System prompt").contains("⛁ ⛁ ⛁ ⛁ ⛀"),
        "the legend stays beside the grid: {medium:#?}"
    );
    assert!(!row(&medium, "⛁ System prompt").contains("Claude Code's own"));
    let skills = medium
        .iter()
        .position(|row| row.contains("Skills · /skills"))
        .unwrap();
    let agents = medium
        .iter()
        .position(|row| row.contains("Custom agents ·"))
        .unwrap();
    assert!(
        skills > agents + 3,
        "skills go under the left lists: {medium:#?}"
    );

    let narrow = rows(&example, 70);
    assert!(
        !row(&narrow, "⛁ System prompt").contains("⛁ ⛁"),
        "the legend goes under the grid: {narrow:#?}"
    );
    row(&narrow, "build:due-flag  Sonnet 5.5 on Claude Code");
}

/// A panel too narrow for a crew's whole header keeps the count and the total at its end: the
/// words about the windows give way to them, and then the end of a long title. What fills an
/// agent's session is listed in whole parts, the smallest left out first, so no row of the
/// section ends in half a word.
#[test]
fn a_narrow_panel_keeps_the_crews_count_and_lists_whole_parts() {
    let example = example::at_three_quarters();
    let wide = rows(&example, 100);
    assert_eq!(
        row(&wide, "Crew agents"),
        "     Crew agents · Add due dates · each in its own window, outside this one  2 agents  49.6k"
    );
    assert!(
        row(&wide, "⛁ system").ends_with("⛁ system 15k   ⛁ memory files 3.7k   ⛁ messages 17.5k")
    );

    let narrow = rows(&example, 80);
    assert_eq!(
        row(&narrow, "Crew agents"),
        "  Crew agents · Add due dates              2 agents  49.6k"
    );
    let crew = narrow.iter().position(|row| row.contains("Crew agents"));
    let section = &narrow[crew.expect("the crew's header")..];
    assert!(
        row(section, "⛁ system")
            .ends_with("starts afresh at 180k   ⛁ system 15k   ⛁ messages 17.5k"),
        "{section:#?}"
    );
    assert!(!section.iter().any(|row| row.contains('…')), "{section:#?}");

    let title = "Add due dates to every task the tasks CLI lists and to its export";
    let titled = rows_titled(&example, Some(&example.count), 80, title);
    assert_eq!(
        row(&titled, "Crew agents"),
        "  Crew agents · Add due dates to every task the tasks CLI list…  2 agents  49.6k"
    );
}

/// Ghostty with Iosevka draws some of these glyphs wider than a cell, so each has a blank
/// after it and nothing ever touches the grid, a swatch or a bar.
#[test]
fn every_grid_glyph_and_bar_has_a_blank_after_it() {
    for example in [example::at_three_quarters(), example::near_full()] {
        for width in [70, 100, 118] {
            for row in rows(&example, width) {
                let cells: Vec<char> = row.chars().collect();
                for (at, cell) in cells.iter().enumerate() {
                    let next = cells.get(at + 1).copied().unwrap_or(' ');
                    if "⛁⛀⛶⛝".contains(*cell) {
                        assert_eq!(next, ' ', "{row}");
                    }
                    if "━╸─┃".contains(*cell) {
                        assert!(" ━╸─┃".contains(next), "{row}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_tab_says_what_its_sessions_hold_together() {
    let example = example::at_three_quarters();
    assert_eq!(summary(&example.view.rows), "3 sessions · 800k in context");
    assert_eq!(
        summary(&example.view.rows[..1]),
        "1 session · 750k in context"
    );
}
