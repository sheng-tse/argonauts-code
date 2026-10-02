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
    state.working = Some(working);
    state.composer.label = Some("peer-crew".into());
    state.mode.background_agents = 3;
    let mut claude = AgentRow::new("claude-code", "card-002", AgentState::Working);
    claude.steps = Some((2, 5));
    claude.started_at = Some(t0);
    claude.tokens_down = 18_100;
    let mut codex = AgentRow::new("codex", "card-003", AgentState::Working);
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
        "Merge card-002 into main?",
        "Merge card-002?",
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
    state.composer.label = Some("peer-crew".into());
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
    state.status_line = vec![
        StatusPart::quota("Claude 5h 58% used", 58, Tone::Dim),
        StatusPart::spend("$0.46 of $3.00"),
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
    state.status_line = vec![StatusPart::quota("Claude 5h 58% used", 58, Tone::Dim)];
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
        for pinned in [false, true] {
            state.card.as_mut().unwrap().pinned = pinned;
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

    state.card.as_mut().unwrap().pinned = true;
    state.card.as_mut().unwrap().focused = false;
    let view = render_at(&state, 80, t0, Duration::ZERO);
    assert_eq!(view.cursor, Some((2, 5)));
    assert!(plain_text(&view.lines).contains("◆ Merge card-002? · needs you · ↑ to answer"));

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
fn tight_space_drops_parts_in_order() {
    let t0 = Instant::now();
    let mut state = crew(t0);
    state.queued = vec!["queued".into()];
    state.status_line = vec![StatusPart::new("quota 42%", Tone::Dim)];
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
    assert!(mode.ends_with("  quota 42%"), "{mode:?}");
    assert!(text.ends_with("waiting"), "{text}");

    let text_within = |rows| plain_text(&state.render_within(&ctx, rows).lines);
    let without_agents = state.render_within(&ctx, 22);
    assert_eq!(without_agents.lines.len(), 19);
    let without_agents = plain_text(&without_agents.lines);
    assert!(!without_agents.contains("claude-code"));
    assert!(without_agents.ends_with("quota 42%"), "{without_agents}");
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
        text.starts_with("◆ Merge card-002 into main?\n    1. Merge"),
        "{text}"
    );
    assert!(text.contains("5. Show the diff"), "{text}");
    assert!(!text.contains("3 files"), "{text}");
    assert!(text.contains("ǁ supervised mode on"), "{text}");

    let clipped = state.render_within(&ctx, 3);
    assert_eq!(clipped.lines.len(), 3);
    // The quota never gives way, so the mode's hints make room for it.
    let clipped = plain_text(&clipped.lines);
    assert!(
        clipped.contains("mode on (shift+tab to cycle) · ← 3 agents"),
        "{clipped}"
    );
    assert!(clipped.ends_with("  quota 42%"), "{clipped}");
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
    let mut codex = AgentRow::new("codex", "owner", AgentState::Working).with_model("gpt-5.6");
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
        Some("x again to stop run-01 · esc to keep")
    );
    assert!(mode_row(&state).is_some());
    assert_eq!(
        state.panel_key(key(KeyCode::Char('x'))),
        PanelEvent::StopRun("run-01".into())
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
        Some("x again to stop run-01 · esc to keep")
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
