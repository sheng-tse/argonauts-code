#[cfg(test)]
pub(super) mod drawing_tests;
#[cfg(test)]
pub(super) mod golden_tests;
use super::example;
use super::*;
use crate::base::text::plain_text;
use crate::base::theme::Theme;

fn shown(example: &example::Example, pane: &ContextPane, width: usize) -> Vec<String> {
    let theme = Theme::dark();
    let kit = Kit::new(&theme);
    let facts = example.facts();
    let title = |_: &RunId| example::RUN_TITLE.to_owned();
    let tab = ContextTab {
        view: &example.view,
        names: &example.names,
        count: example.count.as_ref(),
        main: None,
        places: &example.places,
        now: example.now,
        run_title: &title,
        facts: &facts,
        pane,
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    for section in tab.sections(&kit, width) {
        if !lines.is_empty() && !section.joined {
            lines.push(Line::default());
        }
        lines.extend(section.drawn.lines);
    }
    plain_text(&lines)
        .lines()
        .map(|row| row.trim_end().to_owned())
        .collect()
}

/// The row under the tabs counts the live sessions, and the crew's agents that ended with
/// their tries and tie-breakers counted as one.
#[test]
fn the_summary_counts_live_sessions_and_ended_agents() {
    let (after, straight) = (example::after_the_run(), example::straight_to_a_run());
    assert_eq!(
        summary(&after.view, after.count.as_ref()),
        "1 session \u{b7} 182.5k in context \u{b7} 7 ended"
    );
    assert_eq!(
        summary(&straight.view, straight.count.as_ref()),
        "none open \u{b7} 7 ended"
    );
    assert_eq!(summary(&ContextData::default(), None), "none open");
}

/// Folded, the tab says how to open its lists; open, it doesn't.
#[test]
fn the_fold_hint_shows_while_a_list_is_folded() {
    let example = example::after_the_run();
    let folded = shown(&example, &ContextPane::default(), 118);
    let hint = folded
        .iter()
        .position(|row| row == "     /context all to expand")
        .expect("the hint while a list is folded");
    // The context card follows it, after a blank row.
    assert_eq!(folded[hint + 1], "");
    assert!(folded[hint + 2].starts_with(" ╭"), "{folded:#?}");
    let mut pane = ContextPane::default();
    pane.open_every_list();
    let open = shown(&example, &pane, 118);
    assert!(
        !open.iter().any(|row| row.contains("to expand")),
        "{open:#?}"
    );
    assert!(
        open.iter().any(|row| row.starts_with("     └ main:answer")),
        "{open:#?}"
    );
}

/// A narrow panel keeps every row inside it, the column beside the grid under it.
#[test]
fn a_narrow_tab_keeps_its_rows_inside_it() {
    let example = example::after_the_run();
    let mut pane = ContextPane::default();
    pane.open_every_list();
    for width in [40, 60, 80] {
        let rows = shown(&example, &pane, width);
        assert!(
            rows.iter()
                .all(|row| crate::base::text::str_width(row) <= width),
            "at {width}: {rows:#?}"
        );
        assert!(rows.iter().any(|row| row.contains("Opus 5.5")), "{rows:#?}");
    }
}

mod before_the_count {
    use argonauts_core::base::names::ModelRef;
    use argonauts_core::runs::events::context::{
        FirstCount, NamedTokens, StartHolds, StartPart, StartedFrom, StartingCount, StartingTools,
    };

    use super::*;

    fn part(name: &str, tokens: u64, color: &str) -> StartPart {
        StartPart::named(name, tokens, color)
    }

    fn own(tokens: u64) -> StartPart {
        StartPart::holding(StartHolds::OwnPromptAndTools, tokens, "promptBorder")
    }

    fn instruction_files(tokens: u64) -> StartPart {
        StartPart::holding(StartHolds::InstructionFiles, tokens, "claude")
    }

    fn claude_code(from: StartedFrom) -> StartingCount {
        StartingCount {
            agent: "claude-code".to_owned(),
            model: Some("claude-opus-5-5".to_owned()),
            window: Some(1_000_000),
            buffer: 33_000,
            parts: vec![
                part("System prompt", 6_888, "promptBorder"),
                part("System tools", 15_135, "inactive"),
                part("Memory files", 503, "claude"),
            ],
            files: Vec::new(),
            tools: None,
            from,
            first_count: Some(FirstCount::Answer),
        }
    }

    fn codex(from: StartedFrom) -> StartingCount {
        StartingCount {
            agent: "codex".to_owned(),
            model: Some("gpt-6-luna".to_owned()),
            window: Some(372_000),
            buffer: 0,
            parts: vec![own(15_236), instruction_files(1_200)],
            files: vec![NamedTokens {
                name: "AGENTS.md".to_owned(),
                tokens: 1_200,
            }],
            tools: None,
            from,
            first_count: Some(FirstCount::Turn),
        }
    }

    fn agy() -> StartingCount {
        StartingCount {
            agent: "antigravity-cli".to_owned(),
            model: Some("gemini-3.8-flash-low".to_owned()),
            window: Some(1_000_000),
            buffer: 0,
            parts: vec![own(4_226), instruction_files(1_200)],
            files: vec![NamedTokens {
                name: "AGENTS.md".to_owned(),
                tokens: 1_200,
            }],
            tools: Some(StartingTools {
                named: 60,
                read_only: Some(5),
            }),
            from: StartedFrom::LastSession,
            first_count: Some(FirstCount::Step),
        }
    }

    /// No session yet: the main session's start and no row of it.
    fn before(start: StartingCount) -> example::Example {
        let mut example = example::claude_code_alone();
        example.count = None;
        example.view = ContextData::default();
        example.view.main_start = Some(start);
        example
    }

    /// The session started on `start`'s CLI and holds `used`, which its CLI said.
    fn with_row(start: StartingCount, used: u64, estimated: bool) -> example::Example {
        let mut example = example::claude_code_alone();
        example.count = None;
        let row = &mut example.view.rows[0];
        row.worker = ModelRef::new(start.agent.as_str(), start.model.clone());
        row.used = used;
        row.window = start.window;
        row.counts = start.agent == "claude-code";
        row.estimated = estimated;
        example.view.main_start = Some(start);
        example
    }

    fn has(rows: &[String], text: &str) {
        assert!(
            rows.iter().any(|row| row.contains(text)),
            "{text:?} in {rows:#?}"
        );
    }

    #[test]
    fn before_the_first_message_the_tab_shows_the_last_sessions_start() {
        let rows = shown(
            &before(claude_code(StartedFrom::LastSession)),
            &ContextPane::default(),
            118,
        );
        has(&rows, "~22.5k/1m tokens (2%) \u{b7} from your last session");
        has(&rows, "System prompt: ~6.9k tokens (0.7%)");
        has(&rows, "Free space: ~944.5k (94.4%)");
        has(&rows, "Autocompact buffer: ~33k tokens (3.3%)");
        has(
            &rows,
            "Claude Code's own count replaces it when its first answer ends.",
        );
        assert!(
            !rows.iter().any(|row| row.contains("No session")),
            "{rows:#?}"
        );
        // The card ends the tab, so it never shows nothing.
        assert!(rows.iter().any(|row| row.starts_with(" ╭")), "{rows:#?}");
    }

    /// No list gives the window: every figure still shows, with `–` for each one that needs
    /// the window, and the card above the input says the same.
    #[test]
    fn without_a_window_every_figure_shows_with_blanks() {
        let start = StartingCount {
            model: Some("gemini-3.6-pro".to_owned()),
            window: None,
            from: StartedFrom::Unrecorded,
            ..agy()
        };
        let rows = shown(&before(start), &ContextPane::default(), 118);
        has(&rows, "~5.4k/\u{2013} tokens (\u{2013})");
        has(&rows, "agy's own prompt and tools: ~4.2k tokens (\u{2013})");
        has(&rows, "Free space: \u{2013} (\u{2013})");
        has(
            &rows,
            "No list gives Gemini 3.6 Pro's window yet, so it shows \u{2013}.",
        );
        has(&rows, "~5.4k of \u{2013} \u{b7} estimated");
        assert!(
            !rows.iter().any(|row| row.contains("No session")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_very_first_run_says_where_its_estimate_comes_from() {
        let from = StartedFrom::Recording {
            version: "0.163.0".to_owned(),
        };
        let rows = shown(&before(codex(from)), &ContextPane::default(), 118);
        has(&rows, "~16.4k/372k tokens (4%)");
        has(&rows, "Codex's own prompt and tools: ~15.2k tokens (4.1%)");
        has(
            &rows,
            "~ from our recording of Codex 0.163.0 and from this machine",
        );
        has(
            &rows,
            "Codex's own count replaces it when its first turn ends.",
        );
        assert!(
            !rows.iter().any(|row| row.contains("last session")),
            "{rows:#?}"
        );
    }

    /// A resumed conversation, before its next turn, as its record left it.
    fn recorded(start: StartingCount, used: u64, estimated: bool) -> example::Example {
        let mut example = with_row(start, used, estimated);
        example.view.rows[0].recorded = Some(argonauts_core::app::views::RecordedContext {
            at: example.now - time::Duration::days(3),
            turns: 4,
        });
        example
    }

    #[test]
    fn a_resumed_conversation_on_a_cli_that_counts_keeps_its_buffer_as_recorded() {
        let example = recorded(claude_code(StartedFrom::LastSession), 40_000, false);
        let rows = shown(&example, &ContextPane::default(), 118);
        has(&rows, "40k/1m tokens (4%)");
        has(&rows, "Messages: ~17.5k tokens (1.7%)");
        has(&rows, "Autocompact buffer: 33k tokens (3.3%)");
        has(
            &rows,
            "~ is our count. Used is as this conversation's record left it 3 days ago.",
        );
        for wrong in ["is loading", "the window is from our list", "last session"] {
            assert!(
                !rows.iter().any(|row| row.contains(wrong)),
                "{wrong:?} in {rows:#?}"
            );
        }
    }

    #[test]
    fn a_resumed_conversation_on_a_cli_that_gives_totals_marks_used_as_ours() {
        let example = recorded(codex(StartedFrom::LastSession), 61_400, true);
        let rows = shown(&example, &ContextPane::default(), 118);
        has(&rows, "~61.4k/372k tokens (17%)");
        assert!(
            !rows.iter().any(|row| row.contains("Autocompact buffer")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_cli_no_recording_ships_for_says_its_numbers_are_this_machines_alone() {
        let mut start = codex(StartedFrom::Unrecorded);
        start.first_count = None;
        let rows = shown(&before(start), &ContextPane::default(), 118);
        has(
            &rows,
            "~ from this machine alone: no recording of Codex ships with Argonauts Code.",
        );
        has(
            &rows,
            "Codex's own count replaces it once Codex says how full it is.",
        );
    }

    #[test]
    fn while_the_cli_loads_the_tab_says_so() {
        let example = with_row(claude_code(StartedFrom::LastSession), 0, false);
        let rows = shown(&example, &ContextPane::default(), 118);
        has(&rows, "Claude Code is loading.");
        has(&rows, "\u{b7} from your last session");
        assert_eq!(summary(&example.view, None), "1 session \u{b7} loading");
    }

    /// Between turns a CLI that counts by category isn't asked, so after its first answer
    /// its row holds a fill and no count: the tab draws that fill, never `loading`.
    #[test]
    fn a_counting_cli_between_turns_draws_its_fill() {
        let example = with_row(claude_code(StartedFrom::LastSession), 41_200, false);
        let rows = shown(&example, &ContextPane::default(), 118);
        has(&rows, "41.2k/1m tokens (4%)");
        has(&rows, "Messages: ~18.7k tokens");
        has(&rows, "Autocompact buffer: 33k tokens (3.3%)");
        has(&rows, "~ is our count.");
        assert!(!rows.iter().any(|row| row.contains("loading")), "{rows:#?}");
        assert!(
            !rows.iter().any(|row| row.contains("last session")),
            "{rows:#?}"
        );
    }

    #[test]
    fn a_cli_that_says_only_how_full_shows_its_start_and_the_rest_as_messages() {
        let example = with_row(codex(StartedFrom::LastSession), 61_400, true);
        let rows = shown(&example, &ContextPane::default(), 118);
        has(&rows, "~61.4k/372k tokens (17%)");
        has(&rows, "Codex's own prompt and tools: ~15.2k tokens (4.1%)");
        has(&rows, "Instruction files: ~1.2k tokens (0.3%)");
        has(&rows, "Messages: ~45k tokens (12.1%)");
        has(&rows, "Free space: ~310.6k (83.5%)");
        has(
            &rows,
            "~ is our count: Codex gives its turns' totals, not what fills it.",
        );
        has(&rows, "Instruction files \u{b7} loaded by Codex itself");
        has(&rows, "\u{2514} AGENTS.md \u{b7} ~1.2k tokens");
        has(&rows, "/context all to expand");
        assert!(
            !rows.iter().any(|row| row.contains("last session")),
            "{rows:#?}"
        );
    }

    /// One session's summary reads as its block's line: marked when the count is ours, and in
    /// 1,024s beside a window counted in them.
    #[test]
    fn one_sessions_summary_reads_as_its_block() {
        let ours = with_row(codex(StartedFrom::LastSession), 61_400, true);
        assert_eq!(
            summary(&ours.view, None),
            "1 session \u{b7} ~61.4k in context"
        );
        let mut local = codex(StartedFrom::LastSession);
        local.window = Some(65_536);
        let local = with_row(local, 23_700, false);
        assert_eq!(
            summary(&local.view, None),
            "1 session \u{b7} 23.1k in context"
        );
    }

    /// agy never says its window, so its row has none and its block counts in our list's
    /// 1,048,576: the summary takes that window too, never the row's missing one.
    #[test]
    fn one_sessions_summary_counts_in_the_blocks_window_when_the_cli_gives_none() {
        let mut start = agy();
        start.window = Some(1_048_576);
        let mut example = with_row(start, 38_900, false);
        example.view.rows[0].window = None;
        has(
            &shown(&example, &ContextPane::default(), 118),
            "38k/1M tokens (4%)",
        );
        assert_eq!(
            summary(&example.view, None),
            "1 session \u{b7} 38k in context"
        );
    }

    /// Whichever CLI answers and wherever its window comes from (its row, our list, or its
    /// count by category), one session's summary says the figure its block's line does.
    #[test]
    fn one_sessions_summary_says_its_blocks_figure_for_every_cli() {
        let window = 1_048_576;
        let mut examples = Vec::new();
        for start in [
            claude_code(StartedFrom::LastSession),
            codex(StartedFrom::LastSession),
            agy(),
        ] {
            for (row_window, estimated) in [(None, false), (Some(window), false), (None, true)] {
                let start = StartingCount {
                    window: Some(window),
                    ..start.clone()
                };
                let mut example = with_row(start, 38_900, estimated);
                example.view.rows[0].window = row_window;
                examples.push(example);
            }
        }
        let mut counted = example::claude_code_alone();
        if let Some(count) = counted.count.as_mut() {
            count.usage.window = window;
        }
        counted.view.rows[0].window = None;
        examples.push(counted);
        for example in examples {
            let rows = shown(&example, &ContextPane::default(), 118);
            let figure = rows
                .iter()
                .flat_map(|row| row.split_whitespace())
                .find_map(|word| word.strip_suffix("/1M"))
                .unwrap_or_else(|| panic!("a used/1M line in {rows:#?}"));
            assert_eq!(
                summary(&example.view, example.count.as_ref()),
                format!("1 session \u{b7} {figure} in context"),
                "{rows:#?}"
            );
        }
    }

    #[test]
    fn a_window_the_cli_gives_is_its_own() {
        let mut example = with_row(codex(StartedFrom::LastSession), 30_000, false);
        let rows = shown(&example, &ContextPane::default(), 118);
        has(
            &rows,
            "~ is our count. Used is Codex's own; the window is from our list.",
        );
        has(&rows, "30k/372k tokens (8%)");
        example.view.main_window = Some(argonauts_core::app::views::WindowFigures {
            cli: Some(372_000),
            ..Default::default()
        });
        let rows = shown(&example, &ContextPane::default(), 118);
        has(
            &rows,
            "~ is our count. Used and the window are Codex's own.",
        );
    }

    #[test]
    fn a_cli_that_names_its_tools_lists_them_once_it_has_said() {
        let rows = shown(
            &with_row(agy(), 38_900, false),
            &ContextPane::default(),
            118,
        );
        has(&rows, "agy's own prompt and tools: ~4.2k tokens (0.4%)");
        has(&rows, "Tools \u{b7} as agy names them at start");
        has(
            &rows,
            "\u{2514} 60 tools \u{b7} 5 offered in a read-only session, in agy's own prompt and tools",
        );
        let before = shown(&before(agy()), &ContextPane::default(), 118);
        assert!(
            !before.iter().any(|row| row.contains("Tools \u{b7}")),
            "{before:#?}"
        );
    }
}
