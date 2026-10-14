use std::time::{Duration, Instant};

use super::*;
use crate::render::Clock;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

/// The region as text, with the cursor cell marked by `▏` before it.
fn screen(view: &BottomView) -> String {
    let mut rows: Vec<String> = view
        .lines
        .iter()
        .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    if let Some((column, row)) = view.cursor
        && let Some(text) = rows.get_mut(usize::from(row))
    {
        let at = text
            .char_indices()
            .scan(0usize, |width, (index, c)| {
                let start = *width;
                *width += crate::text::str_width(c.encode_utf8(&mut [0; 4]));
                Some((index, start))
            })
            .find(|(_, start)| *start >= usize::from(column))
            .map_or(text.len(), |(index, _)| index);
        text.insert(at, '▏');
    }
    rows.iter()
        .map(|row| row.trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn crew(t0: Instant) -> BottomState {
    let mut state = BottomState::new();
    let mut working = WorkingLine::new("Building", t0);
    working.tokens_down = 48_200;
    working.details = vec!["3 agents".into(), "$0.21".into()];
    // A run alone is at work, and Esc leaves its agents going.
    working.esc_stops = false;
    state.working = Some(working);
    state.composer.label = Some("council".into());
    state.mode.background_agents = 3;
    let mut claude = AgentRow::new("claude-code", "task-002", AgentState::Working);
    claude.steps = Some((2, 5));
    claude.started_at = Some(t0);
    claude.tokens_down = 18_100;
    let mut codex = AgentRow::new("codex", "task-003", AgentState::Working);
    codex.steps = Some((1, 5));
    codex.started_at = Some(t0 + Duration::from_secs(32));
    codex.tokens_down = 12_900;
    let mut agy = AgentRow::new("agy", "review", AgentState::Waiting);
    agy.note = Some("waiting".into());
    state.agents.rows = vec![claude, codex, agy];
    state
}

fn merge_card() -> CardPrompt {
    let mut card = CardPrompt::new(
        2,
        "Merge task-002 into main?",
        "Merge task-002?",
        vec![
            CardOption::irreversible("Merge"),
            CardOption::new("Open a pull request"),
            CardOption::new("Keep the branch"),
            CardOption::irreversible("Abandon the run"),
            CardOption::view("Show the diff"),
        ],
    );
    card.detail = vec!["3 files · +48 -6 · tests passed · agy reviewed it and approved".into()];
    card
}

fn render_at(state: &BottomState, width: u16, t0: Instant, elapsed: Duration) -> BottomView {
    let theme = theme();
    let ctx = RenderCtx::new(width, &theme, Clock::new(t0, t0 + elapsed));
    state.render(&ctx)
}

#[test]
fn idle_chat() {
    let t0 = Instant::now();
    let mut state = BottomState::new();
    state.composer.label = Some("council".into());
    for width in [80, 120] {
        let view = render_at(&state, width, t0, Duration::ZERO);
        insta::assert_snapshot!(format!("idle_chat_{width}"), screen(&view));
    }
}

#[test]
fn crew_run_in_progress() {
    let t0 = Instant::now();
    let state = crew(t0);
    for width in [80, 120] {
        let view = render_at(&state, width, t0, Duration::from_secs(252));
        insta::assert_snapshot!(format!("crew_run_{width}"), screen(&view));
    }
}

#[test]
fn checkpoint_card_over_the_prompt() {
    let t0 = Instant::now();
    let mut state = crew(t0);
    state.working = None;
    state.card = Some(merge_card());
    state.queued = vec!["also add a --csv flag\nand docs".into()];
    let mut claude = CliUse::new("Claude", Role::CliClaude);
    claude.main = true;
    claude.windows = vec![WindowUse::new("5h", 0.58)];
    state.line.right = vec![
        LinePart::PlanUsage(vec![claude]),
        LinePart::Added(AddedPart::words(
            "spend",
            "$0.46/$3.00",
            crate::cell::Tone::Dim,
        )),
    ];
    for width in [80, 120] {
        let view = render_at(&state, width, t0, Duration::from_secs(60));
        insta::assert_snapshot!(format!("checkpoint_{width}"), screen(&view));
    }
}

#[test]
fn no_row_is_wider_than_the_screen() {
    let t0 = Instant::now();
    let mut state = crew(t0);
    state.queued = vec!["a queued message that runs on for a while".into()];
    let mut claude = CliUse::new("Claude", Role::CliClaude);
    claude.windows = vec![WindowUse::new("5h", 0.58)];
    state.line.right = vec![LinePart::PlanUsage(vec![claude])];
    state.agents.max_rows = 2;
    state.composer.set_text("a draft with a few words in it");
    let mut card = merge_card();
    card.question = Some("Merge it now?".into());
    card.allow_note = true;
    card.handle_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Tab,
    ));
    state.card = Some(card);
    let theme = theme();
    for width in (1..=40).chain([80, 120]) {
        let ctx = RenderCtx::new(width, &theme, Clock::new(t0, t0 + Duration::from_secs(61)));
        for dismissed in [false, true] {
            state.card.as_mut().unwrap().dismissed = dismissed;
            for line in state.render(&ctx).lines {
                assert!(line.width() <= usize::from(width), "{width}: {line:?}");
            }
        }
    }
}

#[test]
fn a_blank_row_sets_the_agent_panel_apart() {
    let t0 = Instant::now();
    let view = render_at(&crew(t0), 80, t0, Duration::ZERO);
    let rows: Vec<String> = view
        .lines
        .iter()
        .map(|line| plain_text(std::slice::from_ref(line)))
        .collect();
    let mode = rows.iter().position(|row| row.contains("mode on")).unwrap();
    assert_eq!(rows[mode + 1], "");
    assert!(rows[mode + 2].contains("claude-code"), "{rows:?}");

    let idle = render_at(&BottomState::new(), 80, t0, Duration::ZERO);
    assert!(
        plain_text(&idle.lines)
            .ends_with("ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts")
    );
}

#[test]
fn the_cursor_follows_focus() {
    let t0 = Instant::now();
    let mut state = crew(t0);
    let view = render_at(&state, 80, t0, Duration::ZERO);
    assert_eq!(view.cursor, Some((2, 3)));

    state.card = Some(merge_card());
    let view = render_at(&state, 80, t0, Duration::ZERO);
    assert_eq!(view.cursor, Some((2, 15)));

    state.card.as_mut().unwrap().focus();
    let view = render_at(&state, 80, t0, Duration::ZERO);
    assert_eq!(view.cursor, None);

    state.card.as_mut().unwrap().dismissed = true;
    state.card.as_mut().unwrap().focused = false;
    let view = render_at(&state, 80, t0, Duration::ZERO);
    assert_eq!(view.cursor, Some((2, 3)), "the input is back");
    assert!(!plain_text(&view.lines).contains("Merge task-002"));

    state.agents.focus();
    let view = render_at(&state, 80, t0, Duration::ZERO);
    assert_eq!(view.cursor, None);
}

#[test]
fn a_card_note_takes_the_cursor() {
    let t0 = Instant::now();
    let mut state = BottomState::new();
    let mut card = merge_card();
    card.allow_note = true;
    card.focus();
    card.handle_key(crossterm::event::KeyEvent::from(
        crossterm::event::KeyCode::Tab,
    ));
    state.card = Some(card);
    let view = render_at(&state, 80, t0, Duration::ZERO);
    let (column, row) = view.cursor.unwrap();
    // On the placeholder's first letter, right after the option's own label.
    assert_eq!(column, 14);
    let text = plain_text(&view.lines[usize::from(row)..=usize::from(row)]);
    assert_eq!(text, "  ❯ 1. Merge, and add a note");
}

#[test]
fn a_hint_shows_until_it_expires() {
    let t0 = Instant::now();
    let mut state = BottomState::new();
    state.hint = Some(Hint {
        text: "Press Ctrl-C again to exit".into(),
        until: t0 + Duration::from_millis(1500),
    });
    let mode_row = |elapsed| {
        let view = render_at(&state, 80, t0, elapsed);
        plain_text(&view.lines[view.lines.len() - 1..])
    };
    assert_eq!(
        mode_row(Duration::from_secs(1)),
        "  Press Ctrl-C again to exit"
    );
    assert_eq!(
        mode_row(Duration::from_secs(2)),
        "  ǁ supervised mode on (shift+tab to cycle) · ? for shortcuts"
    );
}

#[test]
fn a_command_typed_while_work_runs_is_never_said_to_wait() {
    let t0 = Instant::now();
    let mut state = crew(t0);
    state.enter_sends = Some("Enter sends it after this answer".into());
    state.composer.set_text("what's left?");
    let message = screen(&render_at(&state, 100, t0, Duration::ZERO));
    assert!(message.contains("Enter sends it after this answer"));
    // A command opens or answers at once, so the line doesn't say it waits.
    state.composer.set_text("/usage");
    let command = screen(&render_at(&state, 100, t0, Duration::ZERO));
    assert!(!command.contains("Enter sends it after this answer"));
}

#[test]
fn tight_space_drops_parts_in_order() {
    let t0 = Instant::now();
    let mut state = crew(t0);
    state.queued = vec!["queued".into()];
    let words = AddedPart::words("stats", "plan usage 42%", crate::cell::Tone::Dim);
    state.line.right = vec![LinePart::Added(words)];
    state.card = Some(merge_card());
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0));
    let full = state.render(&ctx);
    // Working 2; the card's rule, title, detail, blank, 5 options, blank, and key hints, then a
    // blank row; queued 1, composer 3, the mode line with the quota at its right end, then a
    // blank row and agents 3. No row goes under them for the status.
    assert_eq!(full.lines.len(), 23);
    let text = plain_text(&full.lines);
    let mode = text.lines().find(|row| row.contains("mode on")).unwrap();
    assert!(mode.ends_with("  plan usage 42%"), "{mode:?}");
    assert!(text.ends_with("waiting"), "{text}");

    let text_within = |rows| plain_text(&state.render_within(&ctx, rows).lines);
    let without_agents = state.render_within(&ctx, 22);
    assert_eq!(without_agents.lines.len(), 19);
    let without_agents = plain_text(&without_agents.lines);
    assert!(!without_agents.contains("claude-code"));
    assert!(
        without_agents.ends_with("plan usage 42%"),
        "{without_agents}"
    );
    assert!(without_agents.contains("queued"));
    let without_queue = text_within(18);
    assert!(!without_queue.contains("queued"));
    assert!(without_queue.contains("Building"));
    let without_working = text_within(16);
    assert!(!without_working.contains("Building"));
    assert!(without_working.contains("3 files"));
    assert!(without_working.contains("\n  ↑ to answer\n"));
    let without_hints = text_within(13);
    assert!(
        !without_hints.contains("\n  ↑ to answer\n"),
        "{without_hints}"
    );
    assert!(without_hints.contains("3 files"), "{without_hints}");

    let squeezed = state.render_within(&ctx, 10);
    assert_eq!(squeezed.lines.len(), 10);
    let text = plain_text(&squeezed.lines);
    assert!(
        text.starts_with("◆ Merge task-002 into main?\n    1. Merge"),
        "{text}"
    );
    assert!(text.contains("5. Show the diff"), "{text}");
    assert!(!text.contains("3 files"), "{text}");
    assert!(text.contains("ǁ supervised mode on"), "{text}");

    let clipped = state.render_within(&ctx, 3);
    assert_eq!(clipped.lines.len(), 3);
    // The mode's hints make room for the line's parts, the footer's and the cycle hint first.
    let clipped = plain_text(&clipped.lines);
    assert!(
        clipped.contains("mode on · ← 3 agents · ↓ to manage"),
        "{clipped}"
    );
    assert!(clipped.ends_with("  plan usage 42%"), "{clipped}");
}

#[test]
fn next_change_is_none_when_nothing_moves() {
    let t0 = Instant::now();
    let theme = theme();
    let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0 + Duration::from_millis(50)));
    let mut state = BottomState::new();
    assert_eq!(state.next_change(&ctx), None);

    state.hint = Some(Hint {
        text: "hint".into(),
        until: t0 + Duration::from_secs(1),
    });
    assert_eq!(state.next_change(&ctx), Some(t0 + Duration::from_secs(1)));

    state.working = Some(WorkingLine::new("Rowing", t0));
    assert_eq!(state.next_change(&ctx), Some(t0 + SPINNER_PERIOD));
}

#[test]
fn stops_from_the_panel_ask_first_like_ctrl_c() {
    use crossterm::event::{KeyCode, KeyEvent};

    let t0 = Instant::now();
    let key = |code| KeyEvent::from(code);
    let mut state = BottomState::new();
    state.workflows.rows.push(WorkflowRow::new(
        "run-01",
        "export",
        WorkflowRowState::Running,
    ));
    state.workflows.show_agents = true;
    let mut codex = AgentRow::new("codex", "worker", AgentState::Working).with_model("gpt-5.6");
    codex.key = Some("s-002".into());
    codex.run = Some("run-01".into());
    state.agents.rows.push(codex);
    let mode_row = |state: &BottomState| {
        let text = plain_text(&render_at(state, 100, t0, Duration::ZERO).lines);
        text.lines()
            .find(|line| line.contains("again to stop"))
            .map(str::to_owned)
    };

    state.focus_panel();
    // x x on a run's row stops the whole run.
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    assert_eq!(
        state.active_hint(t0),
        Some("x again to stop run-01 · p to pause it · esc to keep")
    );
    assert!(mode_row(&state).is_some());
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopRun("run-01".into())
    );
    assert_eq!(state.active_hint(t0), None);
    // Asked on a run's row, p pauses it instead. Without the ask p is no key of the list's,
    // so a message typed with the row selected never pauses the run.
    assert_eq!(
        state.panel_key(key(KeyCode::Char('p'))),
        PanelEvent::Unhandled
    );
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    assert_eq!(
        state.panel_key(key(KeyCode::Char('p'))),
        PanelEvent::PauseRun("run-01".into())
    );
    assert_eq!(state.active_hint(t0), None);

    // Another key keeps the agent, and the next x asks again.
    assert_eq!(state.panel_key(key(KeyCode::Down)), PanelEvent::Moved);
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    assert_eq!(
        mode_row(&state).as_deref().map(str::trim),
        Some("x again to stop codex/gpt-5.6 · esc to keep")
    );
    // An agent is stopped or kept, so its ask takes no p.
    assert_eq!(
        state.panel_key(key(KeyCode::Char('p'))),
        PanelEvent::Unhandled
    );
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    assert_eq!(state.panel_key(key(KeyCode::Up)), PanelEvent::Moved);
    assert_eq!(state.active_hint(t0), None);
    assert_eq!(state.panel_key(key(KeyCode::Down)), PanelEvent::Moved);
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    let codex = AgentRef {
        run: Some("run-01".into()),
        session: "s-002".into(),
    };
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::Stop {
            agent: codex,
            name: "codex/gpt-5.6".into(),
        }
    );

    // Leaving the panel drops the ask, so coming back needs both presses again.
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    assert_eq!(state.panel_key(key(KeyCode::Esc)), PanelEvent::Leave);
    assert_eq!(state.active_hint(t0), None);
    state.focus_panel();
    assert_eq!(state.panel_key(key(KeyCode::Down)), PanelEvent::Moved);
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );

    // An ask whose row went away doesn't show, and x on the row now selected asks first.
    state.agents.rows.clear();
    state.workflows.reselect(&state.agents, None);
    assert_eq!(state.active_hint(t0), None);
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopAsked
    );
    assert_eq!(
        state.active_hint(t0),
        Some("x again to stop run-01 · p to pause it · esc to keep")
    );
}

#[test]
fn a_panel_of_agents_alone_asks_before_a_stop_too() {
    use crossterm::event::{KeyCode, KeyEvent};

    let t0 = Instant::now();
    let mut state = BottomState::new();
    state
        .agents
        .rows
        .push(AgentRow::new("agy", "review", AgentState::Waiting));
    state.focus_panel();
    let x = KeyEvent::from(KeyCode::Char('x'));
    assert_eq!(state.panel_key(x), PanelEvent::StopAsked);
    assert_eq!(
        state.active_hint(t0),
        Some("x again to stop agy · esc to keep")
    );
    assert!(matches!(state.panel_key(x), PanelEvent::Stop { .. }));
}

/// The workflow word's notice sits right above the rule, at its right end, while the draft
/// holds the word, and the cursor stays on the input row.
#[test]
fn the_workflow_word_says_so_above_the_rule_until_it_is_ignored() {
    let t0 = Instant::now();
    let theme = theme();
    let ctx = RenderCtx::new(64, &theme, Clock::still(t0));
    let mut state = BottomState::new();
    state.composer.label = Some("Opus 5.5".into());
    state.workflow_key = "opt+w".into();
    for c in "ultracode fix the flaky export test".chars() {
        state.composer.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    let view = state.render(&ctx);
    let rows = screen(&view);
    let rows: Vec<&str> = rows.lines().collect();
    assert_eq!(
        rows[0],
        "     Dynamic workflow requested for this turn · opt+w to ignore"
    );
    assert_eq!(
        rows[1],
        "───────────────────────────────────────────────────── Opus 5.5 ─"
    );
    assert_eq!(rows[2], "❯ ultracode fix the flaky export test▏");
    assert_eq!(view.lines[0].spans[1].style, theme.style(Role::Dim));

    // Claude Code's notice goes after half a minute, and the word keeps its color.
    state.settle_workflow_word(t0);
    assert_eq!(state.next_change(&ctx), Some(t0 + WORD_REQUESTED_NOTICE));
    let later = RenderCtx::new(64, &theme, Clock::still(t0 + WORD_REQUESTED_NOTICE));
    let view = state.render(&later);
    assert!(screen(&view).starts_with("────"), "{}", screen(&view));
    let word = view.lines[1]
        .spans
        .iter()
        .find(|span| span.content == "ultracode")
        .expect("the word is one span");
    assert_eq!(word.style, theme.style(Role::Ultracode));

    // Opt+W: the notice says the word was ignored, for a moment, then the row goes.
    assert_eq!(state.composer.toggle_workflow_word(), Some(true));
    state.word_notice = Some(Hint {
        text: ignored_notice(&state.workflow_key),
        until: t0 + WORD_IGNORED_NOTICE,
    });
    let rows = screen(&state.render(&ctx));
    assert_eq!(
        rows.lines().next().unwrap(),
        "      Ultracode keyword ignored for this prompt · opt+w to undo"
    );
    assert_eq!(state.next_change(&ctx), Some(t0 + WORD_IGNORED_NOTICE));
    let later = RenderCtx::new(64, &theme, Clock::still(t0 + WORD_IGNORED_NOTICE));
    let rows = screen(&state.render(&later));
    assert!(rows.starts_with("────"), "{rows}");

    // A draft without the word has no such row.
    state.composer.clear();
    assert!(screen(&state.render(&ctx)).starts_with("────"));

    // Ignored, cleared and typed again, the word asks again and the screen says so, not
    // that it's ignored.
    state.word_notice = Some(Hint {
        text: ignored_notice(&state.workflow_key),
        until: t0 + WORD_IGNORED_NOTICE,
    });
    for c in "what is new? ultracode it".chars() {
        state.composer.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::NONE,
        ));
    }
    state.settle_workflow_word(t0);
    assert!(state.word_notice.is_none());
    let rows = screen(&state.render(&ctx));
    assert!(
        rows.lines()
            .next()
            .unwrap()
            .contains("Dynamic workflow requested for this turn"),
        "{rows}"
    );
}

/// One of your mods from `mods/`, as the `checks` starter is, with its numbers.
fn checks_mod(
    places: &[argonauts_core::settings::mods::ModPlace],
) -> argonauts_core::settings::mods::ModsView {
    use argonauts_core::settings::mods::view::{ModRow, ModRowKind, ModsView};
    use argonauts_core::settings::mods::{ModBody, ModOrigin, ModReading, builtin, check};

    let (name, text) = builtin::STARTERS
        .iter()
        .find(|(starter, _)| *starter == "checks")
        .unwrap();
    let mut file = check::read(name, text).unwrap();
    file.places = places.to_vec();
    ModsView {
        rows: vec![ModRow {
            name: (*name).to_owned(),
            origin: ModOrigin::Yours,
            path: format!("~/.argonauts/mods/{name}.toml"),
            kind: ModRowKind::Card {
                file: Box::new(file),
                built_in: None,
                reading: Some(ModReading::sample(ModBody::Stack)),
                note: None,
                needs_trust: false,
                asks_for_line: false,
                hidden_by_line: false,
            },
        }],
        ..ModsView::default()
    }
}

/// Your mods placed above the input draw under the band's own cards, and go with the band
/// when you hide it.
#[test]
fn your_mods_above_the_input_draw_in_the_band() {
    use argonauts_core::settings::mods::ModPlace;

    let theme = theme();
    let t0 = Instant::now();
    let ctx = RenderCtx::new(80, &theme, Clock::new(t0, t0));
    let mut state = BottomState::default();
    state.band.shown = true;
    let without = state.render(&ctx).lines.len();
    state.file_mods = Some(checks_mod(&[ModPlace::Above]));
    let with = state.render(&ctx).lines.len();
    assert!(with > without, "{with} rows, {without} without the mod");
    state.band.hidden = true;
    assert_eq!(state.render(&ctx).lines.len(), without);
    state.band.hidden = false;
    state.file_mods = Some(checks_mod(&[ModPlace::ModeLine]));
    assert_eq!(
        state.render(&ctx).lines.len(),
        without,
        "it sits on the line instead"
    );
}
