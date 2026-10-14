//! The panel's rows: this screen's own agents, live, then what the controller listed that the
//! screen doesn't hold, such as conversations, the CLIs' own sessions, and other repos' runs.

use std::collections::BTreeSet;

use argonauts_core::Spend;
use argonauts_core::crew_panel::{
    CrewDetails, CrewPanel, CrewRow, CrewRun, CrewState, CrewTarget, RunStands, SeatTry,
    fold_tries, seat_of,
};
use argonauts_core::events::checkpoint::{CheckpointKind, CheckpointPacket};
use argonauts_core::events::run::{ParkReason, RunOutcome};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::steps::summary_line;
use time::OffsetDateTime;

use crate::bridge::tools;
use crate::state::{AppState, OpenCheckpoint, RunState, SessionState, SessionStatus, task_label};
use crate::text::sanitize_line;
use crate::theme::Glyphs;
use crate::views::panes;
use crate::views::settings::words::capitalized;

pub(super) fn clean(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

/// The last line of `text` with anything in it.
fn last_line(text: &str) -> String {
    let text = crate::text::sanitize(text);
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// This repo as the status line or the welcome names it, else as the controller did.
fn this_repo(state: &AppState) -> String {
    let named = state
        .status
        .as_ref()
        .map(|status| status.repo.clone())
        .or_else(|| state.welcome.as_ref().map(|welcome| welcome.repo.clone()))
        .or_else(|| state.crew_panel.as_ref().map(|panel| panel.repo.clone()))
        .unwrap_or_default();
    match clean(&named) {
        repo if repo.is_empty() => "This repo".to_owned(),
        repo => repo,
    }
}

/// Every row the panel shows. A run the screen holds is shown by its agents, so the
/// controller's row for the whole run gives way to them.
pub(super) fn panel(state: &AppState, now: OffsetDateTime) -> CrewPanel {
    let repo = this_repo(state);
    let mut rows = own_rows(state, &repo, now);
    if let Some(fetched) = &state.crew_panel {
        let shown: BTreeSet<_> = rows
            .iter()
            .filter_map(|row| row.run.as_ref().map(|run| run.id.clone()))
            .collect();
        rows.extend(
            fetched
                .rows
                .iter()
                .filter(|row| match &row.target {
                    CrewTarget::Run { run }
                    | CrewTarget::Agent { run, .. }
                    | CrewTarget::Checkpoint { run, .. } => !shown.contains(run),
                    CrewTarget::Conversation { .. } | CrewTarget::VendorSession { .. } => true,
                })
                .map(|row| CrewRow {
                    // The controller writes this repo the way the screen does; the rest are
                    // other repos and keep their names.
                    repo: if row.repo == fetched.repo {
                        repo.clone()
                    } else {
                        row.repo.clone()
                    },
                    ..row.clone()
                }),
        );
    }
    CrewPanel { repo, rows }
}

/// The run on screen, or every run the screen holds when none is.
fn runs(state: &AppState) -> Vec<&RunState> {
    match state.attached_run() {
        Some(run) => vec![run],
        None => state.runs.values().collect(),
    }
}

fn own_rows(state: &AppState, repo: &str, now: OffsetDateTime) -> Vec<CrewRow> {
    let mut rows = Vec::new();
    for run in runs(state) {
        let crew_run = CrewRun {
            id: run.id.clone(),
            title: clean(&run.title),
            name: run
                .named
                .as_ref()
                .map(|named| clean(&named.name))
                .filter(|name| !name.is_empty()),
            stands: Some(run_stands(run, now)),
        };
        let sessions: Vec<&SessionState> = run.sessions_in_order().collect();
        let mut tries = Vec::new();
        for session in &sessions {
            let asks = asked_by(run, &session.key);
            let label = label_of(session);
            let row = CrewRow {
                target: CrewTarget::Agent {
                    run: run.id.clone(),
                    session: session.key.clone(),
                },
                state: match asks {
                    Some(_) => CrewState::NeedsYou,
                    None => state_of(session.status),
                },
                title: session_title(run, session),
                label: Some(label.clone()),
                tries: 0,
                repo: repo.to_owned(),
                run: Some(crew_run.clone()),
                worker: Some(session.worker.clone()),
                latest: match asks {
                    Some(open) => format!("asks: {}", question(open)),
                    None => {
                        let page = state.artifacts.by_agent(&run.id, &session.key);
                        let page = page.map(|page| page.name.as_str());
                        with_artifact(page, activity(session), page_mark(state))
                    }
                },
                updated: session.ended.unwrap_or(session.last_event),
                details: details(run, session),
            };
            tries.push(SeatTry {
                seat: seat(session, &label),
                started: session.started,
                ended: session.ended,
                row,
            });
        }
        rows.extend(fold_tries(tries));
        // Cards that wait on you with no agent of their own, such as a merge.
        for open in run.checkpoints.iter().filter(|open| open.opened.needs_you) {
            if asker(&sessions, open).is_some() {
                continue;
            }
            let card = open.opened.card.as_ref();
            rows.push(CrewRow {
                target: CrewTarget::Checkpoint {
                    run: run.id.clone(),
                    checkpoint: open.opened.id.clone(),
                },
                state: CrewState::NeedsYou,
                // The question goes in the message column, so the title names the work.
                title: card_title(run, card),
                label: Some(match card {
                    Some(card) => clean(&run.card_label(card)),
                    None => approval_name(open.opened.kind).to_owned(),
                }),
                tries: 0,
                repo: repo.to_owned(),
                run: Some(crew_run.clone()),
                worker: None,
                latest: format!("asks: {}", question(open)),
                updated: run.updated.unwrap_or(now),
                details: CrewDetails {
                    last_message: Some(question(open)),
                    card: card.cloned(),
                    ..CrewDetails::default()
                },
            });
        }
    }
    rows
}

/// An agent's last line, led by the page it published, marked as Claude Code marks one: the
/// page is what it left to open, and Enter on the row shows the call with its link.
fn with_artifact(page: Option<&str>, said: String, mark: &str) -> String {
    let Some(name) = page.map(clean).filter(|name| !name.is_empty()) else {
        return said;
    };
    if said.is_empty() {
        format!("{mark} {name}")
    } else {
        format!("{mark} {name} · {said}")
    }
}

/// The mark a published page's name follows, from the settings the theme takes its own from.
fn page_mark(state: &AppState) -> &'static str {
    let plain = state
        .ui
        .as_ref()
        .is_some_and(|ui| Glyphs::for_settings(ui.glyphs, ui.ambiguous_width).is_ascii());
    panes::artifact_mark(plain)
}

/// Where the run stands, for its heading: how it ended and when, else what it's doing now.
fn run_stands(run: &RunState, now: OffsetDateTime) -> RunStands {
    let (state, word) = match (&run.finished, &run.parked) {
        (Some(RunOutcome::Merged), _) => (CrewState::Done, "merged"),
        (Some(RunOutcome::PullRequest), _) => (CrewState::Done, "pull request"),
        (Some(RunOutcome::BranchLeft), _) => (CrewState::Done, "branch left"),
        (Some(RunOutcome::Stopped), _) => (CrewState::Stopped, "stopped"),
        (Some(RunOutcome::Abandoned), _) => (CrewState::Stopped, "abandoned"),
        (Some(RunOutcome::Failed), _) => (CrewState::Failed, "failed"),
        (None, _) if run.checkpoints.iter().any(|open| open.opened.needs_you) => {
            (CrewState::NeedsYou, "needs you")
        }
        (None, Some(ParkReason::Checkpoint)) => (CrewState::NeedsYou, "needs you"),
        (None, Some(_)) => (CrewState::Waiting, "waiting"),
        (None, None) if run.pausing => (CrewState::Waiting, "pausing"),
        (None, None) => (CrewState::Working, "running"),
    };
    let spent = run.spend();
    RunStands {
        state,
        word: word.to_owned(),
        since: run.ended.or(run.updated).unwrap_or(now),
        spent: (spent.total_tokens() > 0 || spent.cost_usd > 0.0).then_some(spent),
    }
}

/// What the list calls an approval that waits on you with no task of its own. The run's
/// heading already says what the work is.
fn approval_name(kind: CheckpointKind) -> &'static str {
    match kind {
        CheckpointKind::Goal | CheckpointKind::GoalChange => "goal approval",
        CheckpointKind::Plan => "plan approval",
        CheckpointKind::Merge => "merge approval",
        CheckpointKind::Question => "question",
        _ => "approval",
    }
}

/// What the list calls an agent: the run's label for it, such as `build:add-due`. A log from
/// before agents had labels gives its role and task, as the panel names it.
fn label_of(session: &SessionState) -> String {
    let label = session
        .label
        .clone()
        .unwrap_or_else(|| task_label(session.role, session.step, session.card.as_ref()));
    clean(&label)
}

/// What an agent's tries share. A task's sessions keep its label through every retry, so the
/// label is the seat. An agent with no task gets `-2` on a retry to keep its label unique, so
/// its seat is the label without that, told apart by its role.
fn seat(session: &SessionState, label: &str) -> String {
    match &session.card {
        Some(card) => format!("{card}/{label}"),
        None => format!("{}/{}", session.role.as_str(), seat_of(label)),
    }
}

pub(super) fn state_of(status: SessionStatus) -> CrewState {
    match status {
        SessionStatus::Starting | SessionStatus::Working => CrewState::Working,
        SessionStatus::Waiting | SessionStatus::Sleeping => CrewState::Waiting,
        SessionStatus::Done => CrewState::Done,
        SessionStatus::Failed => CrewState::Failed,
        SessionStatus::Stopped => CrewState::Stopped,
    }
}

/// What an agent works on: its card, else the run's goal, after the role it plays when that
/// isn't the default agent's, so a run's restaters, drafters, and judges read apart.
fn session_title(run: &RunState, session: &SessionState) -> String {
    let title = card_title(run, session.card.as_ref());
    if session.card.is_some() || session.role == RoleName::Main {
        return title;
    }
    let role = capitalized(&session.role.as_str().replace('_', " "));
    if title.is_empty() {
        role
    } else {
        format!("{role} · {title}")
    }
}

/// A card's title, else the run's goal, which is what an agent with no card works on.
fn card_title(run: &RunState, card: Option<&argonauts_core::CardId>) -> String {
    let title = card
        .and_then(|card| run.cards.get(card))
        .map(|row| clean(&row.title))
        .filter(|title| !title.is_empty());
    match (title, card) {
        (Some(title), _) => title,
        (None, Some(card)) if run.title.trim().is_empty() => clean(card.as_str()),
        _ => clean(&run.title),
    }
}

/// The question a card asks, word for word, or its title.
pub(super) fn question(open: &OpenCheckpoint) -> String {
    match &open.opened.packet {
        CheckpointPacket::Question(question) => clean(&question.text),
        _ => clean(&open.opened.title),
    }
}

/// The card waiting on you that `session` asked, if any.
pub(super) fn asked_by<'a>(run: &'a RunState, session: &SessionKey) -> Option<&'a OpenCheckpoint> {
    let sessions: Vec<&SessionState> = run.sessions_in_order().collect();
    run.checkpoints
        .iter()
        .filter(|open| open.opened.needs_you)
        .find(|open| asker(&sessions, open).is_some_and(|asker| asker.key == *session))
}

/// The session a card that waits on you belongs to: the one that asked, else a running one on
/// the card, else its newest.
fn asker<'a>(sessions: &[&'a SessionState], open: &OpenCheckpoint) -> Option<&'a SessionState> {
    let card = open.opened.card.as_ref()?;
    let on_card: Vec<&SessionState> = sessions
        .iter()
        .copied()
        .filter(|session| session.card.as_ref() == Some(card))
        .collect();
    if let CheckpointPacket::Question(question) = &open.opened.packet
        && let Some(session) = on_card
            .iter()
            .copied()
            .filter(|session| session.is_worker(&question.from))
            .max_by_key(|session| session.started)
    {
        return Some(session);
    }
    on_card
        .iter()
        .copied()
        .max_by_key(|session| (session.is_running(), session.started))
}

/// What a session is doing in one line: its running tool call, the text streaming now, or
/// its latest message.
pub(crate) fn activity(session: &SessionState) -> String {
    let running =
        |tool: &&crate::state::ToolRecord| tool.ok.is_none() && !tools::is_tool_lookup(&tool.name);
    if let Some(tool) = session.tools.iter().rev().find(running) {
        let call = tools::start_call(&session.vendor, &tool.name, &tool.input);
        return clean(&call.title.plain());
    }
    if !session.streaming.trim().is_empty() {
        return last_line(&session.streaming);
    }
    if let Some(message) = last_message(session) {
        return last_line(&message);
    }
    match session.status {
        SessionStatus::Starting => "starting",
        SessionStatus::Working => "thinking",
        SessionStatus::Waiting => "waiting on other work",
        SessionStatus::Sleeping => "waiting for a usage reset",
        SessionStatus::Done => "done",
        SessionStatus::Failed => "failed",
        SessionStatus::Stopped => "stopped",
    }
    .to_owned()
}

/// Its report's summary, else its final result, else its latest whole message. A structured
/// result never shows as JSON.
fn last_message(session: &SessionState) -> Option<String> {
    let ended = session.end.as_ref().and_then(|end| {
        let summary = end.structured.as_ref().and_then(summary_line);
        let result = end.result_text.clone().filter(|text| !is_json_object(text));
        summary.or(result).or_else(|| end.last_message.clone())
    });
    ended
        .or_else(|| session.messages.last().cloned())
        .filter(|text| !text.trim().is_empty())
}

fn is_json_object(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text.trim()).is_ok_and(|value| value.is_object())
}

fn details(run: &RunState, session: &SessionState) -> CrewDetails {
    let tokens = session.tokens_in + session.tokens_out;
    let spent = (tokens > 0 || session.cost_usd.is_some()).then(|| Spend {
        input_tokens: session.tokens_in,
        output_tokens: session.tokens_out,
        cost_usd: session.cost_usd.unwrap_or(0.0),
        ..Spend::default()
    });
    CrewDetails {
        last_message: last_message(session),
        card: session.card.clone(),
        spent,
        context: session.context,
        // The conductor's name for a card's branch (`argonauts_crew::integrate::branch`).
        branch: session
            .card
            .as_ref()
            .map(|card| format!("argonauts/{}/{card}", run.id)),
        prompt: prompt(session),
    }
}

/// What the session was asked to do: what the orchestrator wrote for it, then the task it was
/// handed, else the message that started it, else the run's goal. The role's standing
/// instructions aren't the ask.
fn prompt(session: &SessionState) -> Option<String> {
    let first = session.inputs.first()?;
    let texts = |wanted: &dyn Fn(&InputPart) -> bool| {
        first
            .parts
            .iter()
            .filter(|part| wanted(part))
            .map(|part| part.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let written = texts(&argonauts_core::prompt_parts::written_for_the_agent);
    let handed = [
        InputPartKind::Card,
        InputPartKind::Message,
        InputPartKind::Goal,
    ]
    .into_iter()
    .map(|kind| texts(&|part| part.kind == kind))
    .find(|text| !text.is_empty());
    let ask: Vec<String> = [Some(written), handed]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .collect();
    (!ask.is_empty()).then(|| ask.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::session::{EndReason, SessionEnd};
    use argonauts_core::names::RoleName;
    use serde_json::json;

    use super::*;

    fn ended(result_text: &str, structured: Option<serde_json::Value>) -> SessionState {
        let mut session = SessionState::new(
            SessionKey::numbered(1),
            "codex".parse().unwrap(),
            "codex",
            RoleName::Worker,
            OffsetDateTime::UNIX_EPOCH,
        );
        session.end = Some(Box::new(SessionEnd {
            vendor_session: None,
            exit_code: Some(0),
            signal: None,
            duration_ms: 1_000,
            reason: EndReason::Completed,
            spend: Spend::default(),
            tool_calls: 1,
            result_text: Some(result_text.to_owned()),
            structured,
            last_message: Some("Creating hello.txt now.".to_owned()),
            raw_log: None,
        }));
        session
    }

    /// The orchestrator's brief is the ask, ahead of the task's facts, and the frame and
    /// Argonauts' own template for the step are never part of it.
    #[test]
    fn the_ask_is_what_the_orchestrator_wrote_then_the_task() {
        use argonauts_core::events::session::{LaunchRecord, PromptVia, SessionInput};
        use argonauts_core::prompt_parts as parts;

        let task = InputPart {
            kind: InputPartKind::Card,
            label: "task-001".to_owned(),
            text: "task-001 · Add due dates\n\nKind: building\n".to_owned(),
            approx_tokens: 8,
            source: None,
        };
        let input = |parts: Vec<InputPart>| SessionInput {
            launch: LaunchRecord {
                program: "codex".to_owned(),
                args: Vec::new(),
                cwd: "/repo".into(),
                env_set: Vec::new(),
                env_removed: Vec::new(),
                prompt_via: PromptVia::Stdin,
                sandbox: None,
            },
            parts,
            queued: Vec::new(),
        };
        let mut session = ended("Done.", None);
        session.inputs = vec![input(vec![
            parts::frame("You are a worker.", None),
            parts::from_orchestrator("Add a due date to each todo.", None),
            task.clone(),
        ])];
        assert_eq!(
            prompt(&session).as_deref(),
            Some("Add a due date to each todo.\n\ntask-001 · Add due dates\n\nKind: building")
        );

        session.inputs = vec![input(vec![
            parts::frame("You are a worker.", None),
            parts::built_in("Review the change.", None),
            task,
        ])];
        assert_eq!(
            prompt(&session).as_deref(),
            Some("task-001 · Add due dates\n\nKind: building")
        );
    }

    #[test]
    fn a_structured_result_reads_as_its_summary_never_as_json() {
        let json = r#"{"status":"done","summary":"Created hello.txt"}"#;
        let report = serde_json::from_str(json).unwrap();
        let session = ended(json, Some(report));
        assert_eq!(last_message(&session).as_deref(), Some("Created hello.txt"));

        let session = ended(r#"{"status":"done"}"#, Some(json!({"status": "done"})));
        assert_eq!(
            last_message(&session).as_deref(),
            Some("Creating hello.txt now.")
        );

        let session = ended("All done.", None);
        assert_eq!(last_message(&session).as_deref(), Some("All done."));
    }

    #[test]
    fn a_page_an_agent_published_leads_its_last_line() {
        let said = || "Wrote the test report.".to_owned();
        assert_eq!(with_artifact(None, said(), "⧉"), "Wrote the test report.");
        assert_eq!(
            with_artifact(Some("test-report"), said(), "⧉"),
            "⧉ test-report · Wrote the test report."
        );
        assert_eq!(
            with_artifact(Some("test-report"), String::new(), "⧉"),
            "⧉ test-report"
        );
        assert_eq!(
            with_artifact(Some("  "), said(), "⧉"),
            "Wrote the test report."
        );
        // Where only plain characters draw, the mark is Claude Code's word for it.
        let mut state = AppState::default();
        assert_eq!(page_mark(&state), "⧉");
        state.ui = Some(argonauts_core::settings::ui::UiSettings {
            glyphs: argonauts_core::settings::ui::Glyphs::Ascii,
            ..Default::default()
        });
        assert_eq!(page_mark(&state), "artifact");
        assert_eq!(
            with_artifact(Some("test-report"), said(), page_mark(&state)),
            "artifact test-report · Wrote the test report."
        );
    }

    /// An agent at work in the list under the input says what it is doing as soon as its
    /// model starts writing a call, by the call's name alone.
    #[test]
    fn a_call_being_written_is_what_the_agent_is_doing() {
        let mut session = SessionState::new(
            SessionKey::numbered(1),
            "claude-code".parse().unwrap(),
            "claude-code",
            RoleName::Worker,
            OffsetDateTime::UNIX_EPOCH,
        );
        session.tools.push(crate::state::ToolRecord {
            id: "e1".to_owned(),
            name: "Edit".to_owned(),
            input: serde_json::Value::Null,
            parent: None,
            ok: None,
            output: None,
        });
        assert_eq!(activity(&session), "Update");
        session.tools[0].input = json!({"file_path": "src/app.rs"});
        assert_eq!(activity(&session), "Update(src/app.rs)");
    }
}
