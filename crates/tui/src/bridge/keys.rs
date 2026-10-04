//! Screen actions that become commands on their own: Esc's interrupt, Shift+Tab, focus, quit,
//! and the keys that open views.

use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::app::{AppMode, UiCommand, ViewRequest};
use argonauts_core::events::run::RunMode;
use argonauts_core::names::{RoleName, SessionKey, WorkflowId};
use argonauts_core::settings::approvals::ApprovalMode;

use crate::app::UiAction;
use crate::bottom::{AgentRef, ApprovalMode as ShownMode, ModeNotice};
use crate::cell::Tone;
use crate::format;
use crate::keymap::Keymap;
use crate::state::{AppState, RunState, SessionState, SessionStatus};
use crate::text::sanitize_line;
use crate::views::{crew::CrewView, help::HelpView, session::SessionView};

use super::Cx;

/// How long a notice from a key stays on the mode line.
const NOTICE_FOR: Duration = Duration::from_secs(4);

#[derive(Debug, Default)]
pub struct KeyActions {}

impl KeyActions {
    /// Takes `Interrupt` (CancelTurn, then Pause, then Pause now, by the app mode), `ModeChanged`,
    /// `Focus`, `Quit`, `OpenCrewView`, `OpenAgent`, `OpenTree`, `OpenWorkflow`, `Follow`,
    /// `OpenHelp`, `EditDraft`, `Command`, `Request`, `Notice`, `Copy`, `Picker`, `StopAgent`,
    /// and `StopRun`. The panel already asked for the stop with its first `x`.
    pub fn on_action(&mut self, action: &UiAction, cx: &mut Cx<'_>) -> bool {
        match action {
            UiAction::Interrupt => interrupt(cx),
            UiAction::ModeChanged(mode) => cx.out.send(UiCommand::ApprovalMode {
                mode: approval_mode(*mode),
            }),
            UiAction::Focus(focused) => cx.out.send(UiCommand::Focus { focused: *focused }),
            UiAction::Quit => {
                cx.out.send(UiCommand::Exit);
                cx.out.exit = true;
                cx.out.exit_lines = exit_lines(cx.state);
            }
            UiAction::OpenTree => open_tree(cx),
            UiAction::OpenWorkflow(key) => open_row(key, cx),
            UiAction::OpenCrewView => {
                cx.out.open(CrewView::opened_left(cx.state));
                // Conversations, the CLIs' own sessions, and other repos' runs come from the
                // controller; the panel shows the last list it sent until this one arrives.
                cx.out.send(UiCommand::Request {
                    view: ViewRequest::Crew,
                });
            }
            UiAction::OpenAgent(agent) => match find_agent(cx.state, agent) {
                Some((run, session)) => cx.out.open(SessionView::new(run, session.key.clone())),
                None => notice(&format!("No agent named {}.", agent.session), cx),
            },
            UiAction::StopAgent(agent) => match find_agent(cx.state, agent) {
                Some((run, session)) => cx.out.send(UiCommand::StopAgent {
                    run,
                    session: session.key.clone(),
                }),
                None => notice(&format!("No agent named {}.", agent.session), cx),
            },
            UiAction::StopRun(run) => stop_run(run, cx),
            UiAction::Follow(n) => match cx.state.follow(*n) {
                Some((run, session)) => {
                    let view = SessionView::new(run.clone(), session.key.clone());
                    cx.out.open(view);
                }
                None => notice(&format!("No agent on {n}."), cx),
            },
            UiAction::OpenHelp => {
                let (keymap, _) = Keymap::with_overrides(&cx.settings.keymap);
                cx.out
                    .open(HelpView::new(keymap).with_commands(cx.state.vendor_commands.clone()));
            }
            UiAction::EditDraft => cx.out.edit_draft = true,
            UiAction::Command(command) => cx.out.send(command.clone()),
            UiAction::Request(view) => cx.out.send(UiCommand::Request { view: view.clone() }),
            UiAction::Notice(text) => notice(text, cx),
            UiAction::Copy(text) => {
                cx.out.clipboard = Some(text.clone());
                notice("Copied", cx);
            }
            UiAction::Picker(event) => crate::views::settings::on_picker(event, cx),
            _ => return false,
        }
        true
    }
}

/// Esc while work runs: a chat turn stops at once, a run pauses at its next safe point, and a
/// run already pausing stops now.
fn interrupt(cx: &mut Cx<'_>) {
    let command = match cx.state.mode {
        Some(AppMode::Answering { .. }) => UiCommand::CancelTurn,
        Some(AppMode::Working { .. }) => UiCommand::Pause { now: false },
        Some(AppMode::Pausing { .. }) => UiCommand::Pause { now: true },
        Some(AppMode::Idle | AppMode::Parked { .. }) | None => return,
    };
    cx.out.send(command);
}

/// `Stop` acts on the run on screen, so for any other run you're told how to get there.
fn stop_run(run: &str, cx: &mut Cx<'_>) {
    if run.contains('#') {
        notice(
            "This workflow is an agent's own; stop it from that agent",
            cx,
        );
        return;
    }
    match RunId::new(run) {
        Ok(run) if cx.state.attached.as_ref() == Some(&run) => cx.out.send(UiCommand::Stop),
        Ok(run) => notice(
            &format!(
                "Only the run on screen can be stopped. Resume {} first, then stop it",
                sanitize_line(run.as_str())
            ),
            cx,
        ),
        Err(_) => notice(&format!("No run named {}.", sanitize_line(run)), cx),
    }
}

fn approval_mode(mode: ShownMode) -> ApprovalMode {
    match mode {
        ShownMode::Supervised => ApprovalMode::Supervised,
        ShownMode::Shadow => ApprovalMode::Shadow,
        ShownMode::Auto => ApprovalMode::Auto,
        ShownMode::FullAuto => ApprovalMode::FullAuto,
    }
}

/// Ctrl+T and `/workflows`.
/// Ctrl+T and `/workflows`: the run's workflow view, or its Library tab alone when there's no
/// run to show.
pub(super) fn open_tree(cx: &mut Cx<'_>) {
    match shown_run(cx.state) {
        Some(run) => open_workflow(run.id.clone(), cx),
        None => cx.out.send(UiCommand::Request {
            view: ViewRequest::WorkflowLibrary,
        }),
    }
}

/// Enter on a panel row: a run's workflow, or a workflow an agent's own CLI runs, whose key
/// names its run and its id after a `#`.
fn open_row(key: &str, cx: &mut Cx<'_>) {
    let (run, vendor) = match key.split_once('#') {
        Some((run, workflow)) => (run, WorkflowId::parse(workflow)),
        None => (key, None),
    };
    let Ok(run) = RunId::new(run) else {
        notice(&format!("No run named {}.", sanitize_line(key)), cx);
        return;
    };
    match vendor {
        Some(workflow) => cx.out.send(UiCommand::Request {
            view: ViewRequest::Workflow {
                run,
                workflow: Some(workflow),
            },
        }),
        None => open_workflow(run, cx),
    }
}

/// The run's workflow view, or for a run whose work is one agent's, that agent's session:
/// a workflow view with one row says less than the agent does. A run from an
/// earlier session comes back on screen first, as `/resume <run>` brings it, with its card if
/// it waits on you.
fn open_workflow(run: RunId, cx: &mut Cx<'_>) {
    let elsewhere = !cx.state.runs.contains_key(&run)
        && cx.state.background.iter().any(|summary| summary.run == run);
    if elsewhere {
        // Focus goes back to the prompt, so the run's card can take the keys once it opens.
        cx.model.bottom.workflows.blur();
        cx.model.bottom.agents.blur();
        cx.out.send(UiCommand::Resume { run: Some(run) });
        return;
    }
    if let Some(session) = single_agent(cx.state, &run) {
        cx.out.open(SessionView::new(run, session));
        return;
    }
    cx.out.send(UiCommand::Request {
        view: ViewRequest::Workflow {
            run,
            workflow: None,
        },
    });
}

/// The one agent a run's work is: a solo run's owner, or the only agent a run started.
fn single_agent(state: &AppState, run: &RunId) -> Option<SessionKey> {
    let run = state.runs.get(run)?;
    if run.mode == Some(RunMode::Solo)
        && let Some(owner) = run
            .sessions_in_order()
            .find(|session| session.role == RoleName::Owner)
    {
        return Some(owner.key.clone());
    }
    let mut sessions = run.sessions_in_order();
    let only = sessions.next()?;
    sessions.next().is_none().then(|| only.key.clone())
}

pub(crate) fn notice(text: &str, cx: &mut Cx<'_>) {
    cx.model.bottom.mode.notice = Some(ModeNotice {
        text: text.to_owned(),
        tone: Tone::Dim,
        until: cx.now + NOTICE_FOR,
    });
}

/// The attached run, else the newest one still going.
pub(crate) fn shown_run(state: &AppState) -> Option<&RunState> {
    state
        .attached_run()
        .or_else(|| state.runs.values().rev().find(|run| run.finished.is_none()))
}

/// The session a panel row names. Session keys repeat across runs, so a row that knows its
/// run is looked up in that run alone.
pub(crate) fn find_agent<'a>(
    state: &'a AppState,
    agent: &AgentRef,
) -> Option<(RunId, &'a SessionState)> {
    let Some(run) = &agent.run else {
        return find_session(state, &agent.session);
    };
    let run = RunId::new(run.as_str()).ok()?;
    let key = SessionKey::parse(&agent.session)?;
    let session = state.session(&run, &key)?;
    Some((run, session))
}

/// A row without a run names its agent by session key, or by harness when that's all it
/// shows. The shown run is searched first, and a working session wins over a finished one.
fn find_session<'a>(state: &'a AppState, name: &str) -> Option<(RunId, &'a SessionState)> {
    let runs = shown_run(state)
        .into_iter()
        .chain(state.runs.values())
        .collect::<Vec<_>>();
    let by_key = runs.iter().find_map(|run| {
        run.sessions
            .values()
            .find(|session| session.key.as_str() == name)
            .map(|session| (run.id.clone(), session))
    });
    by_key.or_else(|| {
        runs.iter().find_map(|run| {
            let named =
                |session: &&SessionState| session.vendor == name || session.role.as_str() == name;
            let sessions = run.sessions_in_order().filter(named).collect::<Vec<_>>();
            sessions
                .iter()
                .find(|session| is_live(session))
                .or(sessions.first())
                .map(|session| (run.id.clone(), *session))
        })
    })
}

fn is_live(session: &SessionState) -> bool {
    matches!(
        session.status,
        SessionStatus::Starting
            | SessionStatus::Working
            | SessionStatus::Waiting
            | SessionStatus::Sleeping
    )
}

/// The one line a fullscreen exit leaves on the main screen, and only while runs keep going:
/// which, and how to watch them.
pub fn watch_line(state: &AppState) -> Option<String> {
    let going: Vec<&RunState> = state
        .runs
        .values()
        .filter(|run| run.finished.is_none())
        .collect();
    match going.as_slice() {
        [] => None,
        [run] => {
            let doing = if run.parked.is_some() {
                "is paused"
            } else {
                "keeps going"
            };
            Some(format!(
                "{id} {doing} · watch it with: argonauts resume {id}",
                id = run.id
            ))
        }
        runs => Some(format!(
            "{} runs keep going · watch them with: argonauts resume",
            runs.len()
        )),
    }
}

/// Dim lines left in scrollback on exit: what keeps going, and how to come back.
pub fn exit_lines(state: &AppState) -> Vec<String> {
    let going: Vec<&RunState> = state
        .runs
        .values()
        .filter(|run| run.finished.is_none())
        .collect();
    if going.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = going
        .iter()
        .map(|run| {
            let mut parts = vec![if run.parked.is_some() {
                format!("{} is paused", run.id)
            } else {
                format!("{} keeps going", run.id)
            }];
            let working = run.sessions.values().filter(|s| is_live(s)).count();
            match working {
                0 => {}
                1 => parts.push("1 agent working".to_owned()),
                n => parts.push(format!("{n} agents working")),
            }
            let spent = run.spend();
            if spent.cost_usd > 0.0 {
                parts.push(format!("{} spent", format::spend(&spent)));
            }
            parts.join(" · ")
        })
        .collect();
    // A plain `argonauts` opens a new conversation, so the way back names the run.
    lines.push(match going.as_slice() {
        [run] => format!("Reattach with: argonauts resume {}", run.id),
        _ => "Reattach with: argonauts resume".to_owned(),
    });
    lines
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::names::TurnId;
    use argonauts_core::settings::ui::UiSettings;

    use super::*;
    use crate::bridge::Effects;
    use crate::model::ViewModel;

    fn act(state: &AppState, action: &UiAction) -> (Effects, ViewModel) {
        let mut model = ViewModel::default();
        let mut out = Effects::default();
        let settings = UiSettings::default();
        let mut cx = Cx {
            model: &mut model,
            state,
            settings: &settings,
            now: Instant::now(),
            out: &mut out,
        };
        assert!(KeyActions::default().on_action(action, &mut cx));
        (out, model)
    }

    fn run_id() -> RunId {
        RunId::new("run-01").unwrap()
    }

    #[test]
    fn a_selection_goes_to_the_clipboard_with_a_short_notice() {
        let (out, model) = act(&AppState::default(), &UiAction::Copy("a word".into()));
        assert_eq!(out.clipboard.as_deref(), Some("a word"));
        let notice = model.bottom.mode.notice.expect("a notice");
        assert_eq!(notice.text, "Copied");
    }

    #[test]
    fn interrupt_maps_by_app_mode() {
        let run = run_id();
        let cases = [
            (
                Some(AppMode::Answering {
                    turn: TurnId::numbered(1),
                }),
                vec![UiCommand::CancelTurn],
            ),
            (
                Some(AppMode::Working { run: run.clone() }),
                vec![UiCommand::Pause { now: false }],
            ),
            (
                Some(AppMode::Pausing { run: run.clone() }),
                vec![UiCommand::Pause { now: true }],
            ),
            (Some(AppMode::Idle), Vec::new()),
            (None, Vec::new()),
        ];
        for (mode, sent) in cases {
            let mut state = AppState::default();
            state.mode = mode.clone();
            let (out, _) = act(&state, &UiAction::Interrupt);
            assert_eq!(out.commands, sent, "{mode:?}");
        }
    }

    #[test]
    fn keys_become_commands_views_and_notices() {
        let state = AppState::default();
        let (out, _) = act(&state, &UiAction::ModeChanged(ShownMode::Auto));
        assert_eq!(
            out.commands,
            [UiCommand::ApprovalMode {
                mode: ApprovalMode::Auto
            }]
        );
        let (out, _) = act(&state, &UiAction::Focus(false));
        assert_eq!(out.commands, [UiCommand::Focus { focused: false }]);

        let (out, _) = act(&state, &UiAction::OpenWorkflow("run-01".into()));
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: ViewRequest::Workflow {
                    run: run_id(),
                    workflow: None
                }
            }]
        );
        // A run from an earlier session that this screen doesn't hold comes back on screen,
        // with its card.
        let mut earlier = AppState::default();
        earlier.apply(&argonauts_core::app::AppEvent::BackgroundRuns {
            runs: vec![argonauts_core::api::RunSummary {
                run: run_id(),
                title: "hi".into(),
                mode: argonauts_core::events::run::RunMode::Solo,
                preset: "peer-crew".into(),
                state: argonauts_core::api::RunState::Parked,
                stopped_at: String::new(),
                updated: time::OffsetDateTime::UNIX_EPOCH,
                spent: argonauts_core::Spend::default(),
                sessions_to_resume: 0,
                waiting: vec![argonauts_core::names::CheckpointId::numbered(1)],
                live: false,
                remind: false,
                name: None,
            }],
        });
        let (out, _) = act(&earlier, &UiAction::OpenWorkflow("run-01".into()));
        assert_eq!(
            out.commands,
            [UiCommand::Resume {
                run: Some(run_id())
            }]
        );

        // With no run to show, Ctrl+T opens the workflow library on its own.
        let (out, model) = act(&state, &UiAction::OpenTree);
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: ViewRequest::WorkflowLibrary
            }]
        );
        assert!(model.bottom.mode.notice.is_none());

        let (out, _) = act(&state, &UiAction::OpenCrewView);
        assert_eq!(out.views.len(), 1);
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: ViewRequest::Crew
            }]
        );
        let (out, _) = act(&state, &UiAction::OpenHelp);
        assert_eq!(out.views.len(), 1);
        let (out, _) = act(&state, &UiAction::EditDraft);
        assert!(out.edit_draft);
        let (out, _) = act(&state, &UiAction::Request(ViewRequest::Context));
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: ViewRequest::Context
            }]
        );
    }

    #[test]
    fn quit_leaves_what_keeps_going_in_scrollback() {
        let (out, _) = act(&AppState::default(), &UiAction::Quit);
        assert!(out.exit);
        assert_eq!(out.commands, [UiCommand::Exit]);
        assert!(out.exit_lines.is_empty());

        let mut run = RunState::new(run_id());
        run.spent.cost_usd = 7.4;
        let mut state = AppState::default();
        state.runs.insert(run_id(), run);
        assert_eq!(
            exit_lines(&state),
            [
                "run-01 keeps going · $7.40 spent",
                "Reattach with: argonauts resume run-01"
            ]
        );
    }

    #[test]
    fn a_fullscreen_exit_names_how_to_watch_what_keeps_going() {
        let mut state = AppState::default();
        assert_eq!(watch_line(&state), None);

        state.runs.insert(run_id(), RunState::new(run_id()));
        assert_eq!(
            watch_line(&state).as_deref(),
            Some("run-01 keeps going · watch it with: argonauts resume run-01")
        );

        let second = RunId::new("run-02").unwrap();
        state.runs.insert(second.clone(), RunState::new(second));
        assert_eq!(
            watch_line(&state).as_deref(),
            Some("2 runs keep going · watch them with: argonauts resume")
        );
    }
}
