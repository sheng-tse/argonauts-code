//! The picker's agent rows: each run's agents fold under it until → shows them, a session's
//! helpers are counted on its row, `/` narrows the list to what you type, an older run's agents
//! are asked for when you open it, and Enter carries a session on only once its run ended.

use std::time::Instant;

use argonauts_core::agent_sessions::{AgentSession, AgentSessionRef, Helper, Resume};
use argonauts_core::api::{RunState as RunPhase, RunSummary};
use argonauts_core::app::{UiCommand, ViewData, ViewRequest};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::session::{SubagentKind, SubagentStatus};
use argonauts_core::input::{ResumeTarget, SlashCommand};
use argonauts_core::names::{RoleName, SessionKey};
use argonauts_core::{RunId, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use super::ResumePicker;
use crate::state::AppState;
use crate::theme::Theme;
use crate::views::{View, ViewCtx, ViewOutcome};

const WALL: OffsetDateTime = datetime!(2026-09-30 12:00 UTC);

fn run(id: &str, hours_ago: i64, state: RunPhase) -> RunSummary {
    RunSummary {
        run: RunId::new(id).unwrap(),
        title: format!("{id} title"),
        mode: RunMode::Crew,
        workflow: "crew".into(),
        state,
        stopped_at: "merged".into(),
        updated: WALL - Duration::hours(hours_ago),
        spent: Spend::default(),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: false,
        remind: false,
        name: None,
    }
}

fn agent(run: &str, n: u32, role: RoleName, worker: &str, resume: Resume) -> AgentSession {
    AgentSession {
        run: RunId::new(run).unwrap(),
        session: SessionKey::numbered(n),
        role,
        label: Some("task-001".into()),
        worker: worker.parse().unwrap(),
        last_message: Some("Fixed both functions.".into()),
        updated: WALL - Duration::hours(5),
        resume,
        helpers: Vec::new(),
    }
}

fn helper(id: &str, kind: SubagentKind) -> Helper {
    Helper {
        id: id.into(),
        kind,
        label: Some("explorer".into()),
        status: SubagentStatus::Done,
        last_message: Some("The parser reads the sign last.".into()),
    }
}

/// run-02, still waiting, with its finished worker, then run-01, merged, with its worker (who
/// started two explorers and a workflow) and its reviewer, whose CLI can't resume.
fn picker() -> ResumePicker {
    let mut worker = agent("run-01", 1, RoleName::Worker, "claude-code", Resume::Ready);
    worker.helpers = vec![
        helper("toolu_01", SubagentKind::Subagent),
        helper("toolu_02", SubagentKind::Subagent),
        helper("wf_01", SubagentKind::Workflow),
    ];
    let reviewer = agent(
        "run-01",
        2,
        RoleName::Reviewer,
        "opencode",
        Resume::Cannot {
            why: "OpenCode can't resume a session".into(),
        },
    );
    let waiting = agent("run-02", 1, RoleName::Worker, "codex", Resume::RunOwns);
    let runs = vec![
        run("run-02", 1, RunPhase::Waiting),
        run("run-01", 5, RunPhase::Merged),
    ];
    let read = runs.iter().map(|run| run.run.clone()).collect();
    let mut view = ResumePicker::new(runs).with_agents(vec![worker, reviewer, waiting], read);
    view.clock = crate::format::WallClock::pinned(WALL);
    view
}

fn key(view: &mut ResumePicker, code: KeyCode, modifiers: KeyModifiers) -> ViewOutcome {
    let theme = Theme::dark();
    let state = AppState::default();
    let ctx = ViewCtx {
        theme: &theme,
        state: &state,
        now: Instant::now(),
    };
    view.handle_key(KeyEvent::new(code, modifiers), &ctx)
}

fn press(view: &mut ResumePicker, code: KeyCode) -> ViewOutcome {
    key(view, code, KeyModifiers::NONE)
}

fn typed(view: &mut ResumePicker, text: &str) {
    for c in text.chars() {
        press(view, KeyCode::Char(c));
    }
}

fn rows(view: &ResumePicker) -> Vec<String> {
    view.lines(140, &Default::default(), &Theme::dark())
        .iter()
        .skip(1)
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

/// Each row's kind and id, such as `agent s-001`.
fn listed(view: &ResumePicker) -> Vec<String> {
    view.rows.iter().map(|row| view.named(*row)).collect()
}

fn hints(view: &ResumePicker) -> Vec<String> {
    view.keys().into_iter().map(|(key, _)| key).collect()
}

/// Moves the selection to the row `at`, counting from the first row under the heading.
fn select(view: &mut ResumePicker, at: usize) {
    press(view, KeyCode::Home);
    for _ in 0..at {
        press(view, KeyCode::Down);
    }
}

fn resumed(outcome: &ViewOutcome) -> Option<AgentSessionRef> {
    match outcome {
        ViewOutcome::Send(UiCommand::Slash {
            command:
                SlashCommand::Resume {
                    target: Some(ResumeTarget::Agent(agent)),
                },
        }) => Some(agent.clone()),
        _ => None,
    }
}

#[test]
fn a_runs_agents_fold_under_it_until_you_open_it() {
    let mut view = picker();
    assert_eq!(listed(&view), ["run run-02", "run run-01"]);

    select(&mut view, 1);
    assert!(
        hints(&view).contains(&"enter shows its agents".to_owned()),
        "{:?}",
        hints(&view)
    );
    assert!(matches!(
        press(&mut view, KeyCode::Right),
        ViewOutcome::Consumed
    ));
    assert_eq!(
        listed(&view),
        ["run run-02", "run run-01", "agent s-001", "agent s-002"]
    );
    let text = rows(&view);
    for cell in [
        // Indented under its run, with two subagents and a workflow counted on its row.
        "  worker · task-001 · 3 helpers · Fixed both functions.",
        "Claude Code",
        "5h ago",
    ] {
        assert!(text[2].contains(cell), "{cell}: {}", text[2]);
    }
    for cell in ["can't resume", "OpenCode"] {
        assert!(text[3].contains(cell), "{cell}: {}", text[3]);
    }
    // No row of its own for a helper: it comes back with its session.
    assert!(!text.iter().any(|row| row.contains("toolu_01")), "{text:?}");

    // ← from one of its agents folds it again, with the run selected.
    select(&mut view, 3);
    press(&mut view, KeyCode::Left);
    assert_eq!(listed(&view), ["run run-02", "run run-01"]);
    assert!(rows(&view)[1].starts_with("❯"), "{:?}", rows(&view));
}

#[test]
fn enter_carries_on_a_session_whose_run_ended() {
    let mut view = picker();
    select(&mut view, 1);
    press(&mut view, KeyCode::Right);
    select(&mut view, 2);
    assert!(hints(&view).contains(&"enter continues it".to_owned()));
    let outcome = press(&mut view, KeyCode::Enter);
    assert_eq!(
        resumed(&outcome),
        Some(AgentSessionRef::parse("run-01/s-001").unwrap()),
        "{outcome:?}"
    );

    // The app says why one can't, and shows its transcript.
    select(&mut view, 3);
    assert!(hints(&view).contains(&"enter shows its transcript".to_owned()));
    let outcome = press(&mut view, KeyCode::Enter);
    assert_eq!(
        resumed(&outcome),
        Some(AgentSessionRef::parse("run-01/s-002").unwrap()),
        "{outcome:?}"
    );
}

/// The waiting run still owns its finished worker's session: its next round resumes it.
#[test]
fn a_session_whose_run_hasnt_ended_waits_for_the_run() {
    let mut view = picker();
    select(&mut view, 0);
    assert!(hints(&view).contains(&"enter resumes it".to_owned()));
    assert!(hints(&view).contains(&"→ agents".to_owned()));
    press(&mut view, KeyCode::Right);
    select(&mut view, 1);
    let text = rows(&view);
    assert!(
        text[1].contains("run owns it") && text[1].contains("Codex"),
        "{}",
        text[1]
    );
    let offered = hints(&view);
    assert!(
        offered.iter().all(|hint| !hint.starts_with("enter")),
        "{offered:?}"
    );
    match press(&mut view, KeyCode::Enter) {
        ViewOutcome::Notice(text) => assert_eq!(
            text,
            "run-02 hasn't ended, so it still owns run-02/s-001. t in its agent view takes it \
             over, or it carries on once the run ends"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn slash_narrows_the_list_to_the_rows_that_match() {
    let mut view = picker();
    assert!(hints(&view).contains(&"/ search".to_owned()));
    press(&mut view, KeyCode::Char('/'));
    // Agents show under their run when they match, folded or not.
    typed(&mut view, "opencode");
    assert_eq!(listed(&view), ["run run-01", "agent s-002"]);
    assert_eq!(
        view.title_line("~", 2, &Theme::dark()).to_string(),
        "Resume a conversation · in ~ · search: opencode"
    );
    // Letters are the search's now, so x stops nothing.
    assert!(hints(&view).contains(&"esc clears the search".to_owned()));
    assert!(!hints(&view).contains(&"x stops it".to_owned()));

    // Every word you type is found, in any case.
    for _ in 0.."opencode".len() {
        press(&mut view, KeyCode::Backspace);
    }
    typed(&mut view, "WORKER run-02");
    assert_eq!(listed(&view), ["run run-02", "agent s-001"]);
    typed(&mut view, "x");
    let text = rows(&view);
    assert_eq!(text.len(), 1, "{text:?}");
    assert!(text[0].contains("Nothing matches"), "{text:?}");

    // Esc clears the search, and the letters are keys again.
    assert!(matches!(
        press(&mut view, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert_eq!(listed(&view), ["run run-02", "run run-01"]);
    assert!(matches!(
        press(&mut view, KeyCode::Char('x')),
        ViewOutcome::Consumed
    ));
    assert!(hints(&view)[0].starts_with("x again"), "{:?}", hints(&view));
}

#[test]
fn an_older_runs_agents_are_asked_for_when_you_open_it() {
    let runs = vec![
        run("run-02", 1, RunPhase::Merged),
        run("run-01", 30, RunPhase::Merged),
    ];
    let worker = agent("run-02", 1, RoleName::Worker, "claude-code", Resume::Ready);
    let mut view =
        ResumePicker::new(runs).with_agents(vec![worker], vec![RunId::new("run-02").unwrap()]);
    select(&mut view, 1);
    match press(&mut view, KeyCode::Right) {
        ViewOutcome::Request(ViewRequest::AgentSessions { run }) => {
            assert_eq!(run.as_str(), "run-01");
        }
        other => panic!("{other:?}"),
    }
    let older = agent("run-01", 1, RoleName::Worker, "codex", Resume::Ready);
    assert!(view.accept(&ViewData::AgentSessions {
        run: RunId::new("run-01").unwrap(),
        agents: vec![older],
    }));
    assert_eq!(listed(&view), ["run run-02", "run run-01", "agent s-001"]);
    assert!(rows(&view)[1].starts_with("❯"), "{:?}", rows(&view));

    // A fresh list from the app keeps what was read since.
    let data = ViewData::Runs {
        runs: view.runs.clone(),
        conversations: Vec::new(),
        sessions: Vec::new(),
        agents: vec![agent(
            "run-02",
            1,
            RoleName::Worker,
            "claude-code",
            Resume::Ready,
        )],
        agents_read: vec![RunId::new("run-02").unwrap()],
    };
    assert!(view.accept(&data));
    assert_eq!(listed(&view), ["run run-02", "run run-01", "agent s-001"]);
    // Opened again, it asks for nothing more.
    press(&mut view, KeyCode::Left);
    assert!(matches!(
        press(&mut view, KeyCode::Right),
        ViewOutcome::Consumed
    ));
}

#[test]
fn r_shows_an_agents_raw_log() {
    let mut view = picker();
    select(&mut view, 1);
    press(&mut view, KeyCode::Right);
    select(&mut view, 2);
    match press(&mut view, KeyCode::Char('r')) {
        ViewOutcome::Request(ViewRequest::Raw { run, session }) => {
            assert_eq!(run.as_str(), "run-01");
            assert_eq!(session.as_str(), "s-001");
        }
        other => panic!("{other:?}"),
    }
}

/// A merged run has nothing left to stop, so x isn't offered for it.
#[test]
fn a_run_that_ended_offers_no_stop() {
    let mut view = picker();
    select(&mut view, 1);
    assert!(
        !hints(&view).contains(&"x stops it".to_owned()),
        "{:?}",
        hints(&view)
    );
    match press(&mut view, KeyCode::Char('x')) {
        ViewOutcome::Notice(text) => {
            assert_eq!(text, "run-01 has ended, so it has nothing to stop")
        }
        other => panic!("{other:?}"),
    }
    select(&mut view, 0);
    assert!(
        hints(&view).contains(&"x stops it".to_owned()),
        "{:?}",
        hints(&view)
    );
}

/// The title says a step named like its role once, as the run review's is, and leaves out the
/// fenced report a build session's last message ends with.
#[test]
fn a_title_says_its_role_once_and_leaves_the_report_block_out() {
    let mut run_review = agent(
        "run-01",
        3,
        RoleName::RunReview,
        "claude-code",
        Resume::Ready,
    );
    run_review.label = Some("run-review".into());
    run_review.last_message = None;
    assert_eq!(super::agent_title(&run_review), "run-review");
    let mut worker = agent("run-01", 1, RoleName::Worker, "claude-code", Resume::Ready);
    worker.label = Some("build:money".into());
    worker.last_message = Some("Done\n```json\n{\"status\": \"done\"}\n```".into());
    assert_eq!(super::agent_title(&worker), "worker · build:money · Done");
}
