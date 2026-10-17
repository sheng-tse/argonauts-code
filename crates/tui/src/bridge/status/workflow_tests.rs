//! The working line while an agent writes a workflow's script: nothing of the workflow shows
//! until the script exists, so this line is the only thing that moves.

use std::time::{Duration, Instant};

use argonauts_core::AgentEvent;
use argonauts_core::app::{AppEvent, AppMode, ConversationEvent};
use argonauts_core::names::{ModelRef, TurnId};

use super::sync_working;
use crate::bottom::{BottomState, WorkingLine};
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::views::workflow_pictures::{buffer_of, dark, picture};

/// A turn the CLI `cli` answers.
fn turn_on(cli: &str) -> (AppState, TurnId) {
    let turn = TurnId::numbered(1);
    let mut state = AppState::default();
    state.mode = Some(AppMode::Answering { turn: turn.clone() });
    state.conversation.turn = Some(turn.clone());
    state.conversation.worker = Some(ModelRef::new(cli, None));
    (state, turn)
}

fn agent(state: &mut AppState, turn: &TurnId, event: AgentEvent) {
    state.apply(&AppEvent::Conversation {
        event: ConversationEvent::Agent {
            turn: turn.clone(),
            event,
        },
    });
}

fn pending(id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolPending {
        id: id.to_owned(),
        name: name.to_owned(),
        parent: None,
    }
}

fn started(id: &str, name: &str) -> AgentEvent {
    AgentEvent::ToolStarted {
        id: id.to_owned(),
        name: name.to_owned(),
        input: serde_json::json!({ "script": "export const meta = {}" }),
        parent: None,
    }
}

fn synced(state: &AppState, bottom: &mut BottomState, now: Instant) -> WorkingLine {
    sync_working(state, bottom, || "Rowing", now);
    bottom.working.clone().expect("a working line")
}

#[test]
fn the_line_says_a_workflow_is_being_written_until_its_script_exists() {
    let (mut state, turn) = turn_on("claude-code");
    let mut bottom = BottomState::new();
    let t0 = Instant::now();
    let at = |secs: u64| t0 + Duration::from_secs(secs);

    assert_eq!(synced(&state, &mut bottom, at(0)).verb, "Rowing");

    agent(&mut state, &turn, pending("toolu_1", "Workflow"));
    let line = synced(&state, &mut bottom, at(3));
    assert_eq!(line.verb, "Writing a workflow");
    assert_eq!(line.started_at, t0, "the turn's own clock keeps counting");
    // A sync that brings nothing new changes nothing.
    assert_eq!(synced(&state, &mut bottom, at(4)), line);

    // The call is whole, so the script exists, and the turn's own word is back.
    agent(&mut state, &turn, started("toolu_1", "Workflow"));
    let line = synced(&state, &mut bottom, at(12));
    assert_eq!(line.verb, "Rowing");
    assert_eq!(line.started_at, t0);
}

#[test]
fn a_call_the_model_gave_up_gives_the_word_back() {
    let (mut state, turn) = turn_on("claude-code");
    let mut bottom = BottomState::new();
    let now = Instant::now();
    synced(&state, &mut bottom, now);

    agent(&mut state, &turn, pending("toolu_1", "Workflow"));
    assert_eq!(synced(&state, &mut bottom, now).verb, "Writing a workflow");
    agent(
        &mut state,
        &turn,
        AgentEvent::ToolDropped {
            id: "toolu_1".to_owned(),
            parent: None,
        },
    );
    assert_eq!(synced(&state, &mut bottom, now).verb, "Rowing");
}

#[test]
fn only_a_call_that_starts_a_workflow_changes_the_word() {
    let mut bottom = BottomState::new();
    let now = Instant::now();

    let (mut state, turn) = turn_on("claude-code");
    agent(&mut state, &turn, pending("toolu_1", "Write"));
    assert_eq!(synced(&state, &mut bottom, now).verb, "Rowing");

    // Another CLI's tool of the same name starts no workflow of its own.
    let (mut state, turn) = turn_on("codex");
    agent(&mut state, &turn, pending("call_1", "Workflow"));
    assert_eq!(synced(&state, &mut bottom, now).verb, "Rowing");
}

/// The picture's middle row, twelve seconds into the turn with 3.1k tokens down. The line
/// ends its parentheses after the tokens; the picture goes on to offer Esc there.
#[test]
fn the_line_reads_as_its_picture_while_the_script_is_written() {
    let picture = picture!("workflow_too_early");
    let wanted = "✶ Writing a workflow… (12s · ↓ 3.1k tokens";
    assert!(picture.text(5..6)[0].starts_with(wanted));

    let (mut state, turn) = turn_on("claude-code");
    let mut bottom = BottomState::new();
    let t0 = Instant::now();
    synced(&state, &mut bottom, t0);
    agent(&mut state, &turn, pending("toolu_1", "Workflow"));
    state.conversation.tokens_out = 3_100;
    let now = t0 + Duration::from_secs(12);
    let line = synced(&state, &mut bottom, now);

    // The spinner is on its fourth frame and the shimmer rests off the verb 2640 ms into
    // their cycles.
    let theme = dark();
    let clock = Clock::new(now - Duration::from_millis(2640), now);
    let ctx = RenderCtx::new(118, &theme, clock);
    let buf = buffer_of(&[line.line(&ctx)], 118);
    let drawn: String = (0..118).map(|x| buf[(x, 0)].symbol()).collect();
    let after = drawn
        .trim_end()
        .strip_prefix(wanted)
        .unwrap_or_else(|| panic!("{drawn}"));
    // The picture's line goes on to offer Esc. The bottom line makes that offer, so the line
    // here closes after the tokens.
    assert_eq!(after, ")", "{drawn}");
    picture.assert_cells(5, 0..wanted.chars().count(), &buf, 0);
}

/// A Captain task whose agent's turn is over while the workflows its CLI started go on: the
/// line under the conversation says it waits for them, dim and still, in the words of the
/// picture's row under the two Workflow calls. Claude Code's own turn end says "dynamic
/// workflow" and counts the session's background agents as well; the line keeps the picture's
/// words and counts the workflows. The agent's next turn brings the working line back.
#[test]
fn a_task_whose_workflows_go_on_after_its_turn_waits_for_them() {
    use argonauts_core::app::AppMode;
    use serde_json::json;

    use crate::views::waiting::fixture::{RunLog, run_id};

    let mut picture = picture!("nested_captain_second");
    // The input's rules keep their own grey, a step brighter than the picture's.
    let rule = dark().style(crate::theme::Role::Rule).fg;
    for row in [9, 11] {
        for column in 0..118 {
            picture.recolour(row, column, rule);
        }
    }
    let mut log = RunLog::started_as("captain");
    log.state.mode = Some(AppMode::Working { run: run_id() });
    log.workflow("wf-001");
    log.worker("s-001");
    let workflow = |status: &str| {
        json!({"type": "agent", "data": {"type": "workflow", "workflow": {
            "id": "wf_2", "name": "final-signoff-money", "description": "",
            "phases": [{"index": 1, "title": "Final"}], "agents": [], "status": status,
            "tokens": 0, "tool_calls": 0, "duration_ms": 0}}})
    };
    log.send(Some("s-001"), workflow("running"));
    log.send(
        Some("s-001"),
        json!({"type": "agent", "data": {"type": "result", "text": "Waiting for it.",
            "structured": null, "is_error": false}}),
    );

    let mut bottom = BottomState::new();
    let now = Instant::now();
    sync_working(&log.state, &mut bottom, || "Rowing", now);
    assert!(bottom.working.is_none(), "{:?}", bottom.working);
    assert_eq!(
        bottom.waits_for.as_deref(),
        Some("Waiting for 1 workflow to finish")
    );
    // The run's row under the input, so the hint line names its key, and the picture's mode.
    bottom.workflows.rows =
        super::super::panel::run_rows(&log.state, time::OffsetDateTime::now_utc(), now);
    bottom.mode.approval = crate::bottom::ApprovalMode::Auto;
    let theme = dark();
    let ctx = RenderCtx::new(118, &theme, Clock::still(now));
    let lines = bottom.render(&ctx).lines;
    // The agent's work goes on, so the input offers no example and the hint line no shortcuts.
    picture.assert_drawn(7..13, &buffer_of(&lines[..6], 118), 0);

    // The workflow ends and the agent goes on with its turn: the run's own line is back.
    log.send(Some("s-001"), workflow("done"));
    log.send(
        Some("s-001"),
        json!({"type": "agent", "data": {"type": "text_delta", "text": "It passed.",
            "parent": null}}),
    );
    sync_working(&log.state, &mut bottom, || "Rowing", now);
    assert_eq!(bottom.waits_for, None);
    assert_eq!(
        bottom.working.as_ref().map(|line| line.verb.as_str()),
        Some("Rowing")
    );
}
