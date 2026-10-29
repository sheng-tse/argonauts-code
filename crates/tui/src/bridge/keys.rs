//! Screen actions that become commands on their own: Esc's interrupt, Shift+Tab, focus, quit,
//! and the keys that open views.

use std::time::Duration;

use argonauts_core::RunId;
use argonauts_core::api::UsagePeriod;
use argonauts_core::app::{AppMode, CommandWork, UiCommand, ViewRequest};
use argonauts_core::events::session::SubagentStatus;
use argonauts_core::input::{ModsAction, SlashCommand, ViewAction};
use argonauts_core::names::{SessionKey, WorkflowId};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::ui::ModName;
use argonauts_core::views::{cli_workflow_run_id, conversation_run};

use crate::app::UiAction;
use crate::bottom::{AgentRef, ApprovalMode as ShownMode, ModeNotice};
use crate::cell::Tone;
use crate::format;
use crate::keymap::{Action, Keymap};
use crate::overview::{OverviewView, Tab};
use crate::state::{AppState, RunState, SessionState, SessionStatus};
use crate::text::sanitize_line;
use crate::views::waiting::{self, RunEntry, WaitingView};
use crate::views::{crew::AgentPanelView, help::HelpView, session::SessionView};

use super::Cx;

/// How long a notice from a key stays on the bottom line.
const NOTICE_FOR: Duration = Duration::from_secs(4);

#[derive(Debug, Default)]
pub struct KeyActions {}

impl KeyActions {
    /// Takes `Interrupt` (CancelTurn, or Pause now for a run that's pausing), `ModeChanged`,
    /// `Focus`, `Quit`, `OpenAgentPanel`, `OpenAgent`, `OpenTree`, `OpenWorkflow`, `Follow`,
    /// `OpenHelp`, `EditDraft`, `Command`, `Request`, `Notice`, `Copy`, `Picker`, `StopAgent`,
    /// `StopRun`, and `PauseRun`. The panel already asked for the stop with its first `x`.
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
            // The tab that shows the mod at full size opens at once, and what it draws follows
            // from the controller.
            // The stats card opens its tab for the period its file sets, as `/stats` does.
            UiAction::OpenTab(ModName::Stats) => open_stats(cx, false),
            UiAction::OpenTab(name) => {
                let watch = cx.model.bottom.band.watch.clone();
                let tab = Tab::showing(*name);
                let period = match tab {
                    Tab::Usage => cx.settings.views.usage.period.usage(),
                    _ => UsagePeriod::OPENS_ON,
                };
                cx.out
                    .open(OverviewView::new(tab, watch).with_period(period));
                // What the panel asks when its own tab opens, so the tab draws whole and
                // nothing of it reaches the conversation.
                let ask = |view| UiCommand::Request { view };
                match tab {
                    Tab::Status => cx.out.send(ask(ViewRequest::StatusTab)),
                    Tab::Context => cx.out.send(ask(ViewRequest::Context)),
                    _ => {
                        cx.out.send(ask(ViewRequest::Usage { period }));
                        cx.out.send(ask(ViewRequest::ReadPlanUsage));
                    }
                }
            }
            UiAction::OpenAgentPanel => open_agent_panel(cx),
            // A subagent of the main agent's own CLI has no view of its own: what it does
            // shows in the conversation, in full with the transcript key.
            UiAction::OpenAgent(agent) if of_conversation(agent) => {
                notice(
                    "Its steps are in the conversation · ctrl+o shows them all",
                    cx,
                );
            }
            UiAction::OpenAgent(agent) => match find_agent(cx.state, agent) {
                Some((run, session)) => cx.out.open(SessionView::new(run, session.key.clone())),
                None => notice(&format!("No agent named {}.", agent.session), cx),
            },
            UiAction::StopAgent(agent) if of_conversation(agent) => {
                cx.out.send(UiCommand::StopSubagent {
                    id: agent.session.clone(),
                });
            }
            UiAction::StopAgent(agent) => match find_agent(cx.state, agent) {
                Some((run, session)) => cx.out.send(UiCommand::StopAgent {
                    run,
                    session: session.key.clone(),
                }),
                None => notice(&format!("No agent named {}.", agent.session), cx),
            },
            UiAction::OpenArtifact => match cx.state.artifacts.latest() {
                Some(latest) => {
                    let (url, name) = (latest.artifact.url.clone(), latest.artifact.name.clone());
                    open_link(&url, &name, cx);
                }
                None => notice(crate::views::artifacts::NONE_YET, cx),
            },
            UiAction::OpenLink { url, name } => open_link(url, name, cx),
            UiAction::MessageAgent { agent, name, text } => message_agent(agent, name, text, cx),
            UiAction::StopAllAgents => stop_all_agents(cx),
            UiAction::StopRun(run) => stop_run(run, cx),
            UiAction::PauseRun(run) => pause_run(run, cx),
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
                    .open(HelpView::new(keymap).with_commands(cx.state.cli_commands.clone()));
            }
            UiAction::EditDraft => cx.out.edit_draft = true,
            // `,` on the pinned stats card: its page is the Stats tab's own.
            UiAction::Command(UiCommand::Slash {
                command:
                    SlashCommand::Mods {
                        action:
                            ModsAction::Page {
                                the_mod: ModName::Stats,
                            },
                        ..
                    },
            }) => open_stats(cx, true),
            UiAction::Command(command) => cx.out.send(command.clone()),
            UiAction::Replied(reply) => cx.out.replies.push(reply.clone()),
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

/// Hands a claude.ai page to the browser and says so, in Claude Code's words. Anything else
/// is refused: the address came out of a tool's result, and only an artifact's is opened.
fn open_link(url: &str, name: &str, cx: &mut Cx<'_>) {
    if !argonauts_core::artifacts::is_link(url) {
        notice("That isn't a claude.ai link, so it wasn't opened", cx);
        return;
    }
    cx.out.open_link = Some(url.to_owned());
    notice(
        &format!("Opened {} in your browser", sanitize_line(name).trim()),
        cx,
    );
}

/// A row of the main agent's own CLI: one of its subagents.
fn of_conversation(agent: &AgentRef) -> bool {
    agent.run.as_deref() == Some(conversation_run().as_str())
}

/// Enter on a draft with an agent's row selected. An agent a run started is steered directly,
/// and reads the message at its next step. A subagent of the main agent's own CLI has no line
/// of its own when Argonauts Code drives that CLI, so the message goes to the main agent to pass
/// on, and the conversation says so.
fn message_agent(agent: &AgentRef, name: &str, text: &str, cx: &mut Cx<'_>) {
    let name = sanitize_line(name).trim().to_owned();
    if of_conversation(agent) {
        cx.out.send(UiCommand::MessageSubagent {
            name: name.clone(),
            text: text.trim().to_owned(),
        });
        notice(
            &format!("Sent through the main agent, which passes it on to {name}"),
            cx,
        );
        return;
    }
    match find_agent(cx.state, agent) {
        Some((run, session)) => {
            cx.out.send(UiCommand::Steer {
                run,
                session: session.key.clone(),
                text: text.to_owned(),
                now: false,
            });
            notice(
                &format!("Sent to {name}, which reads it at its next step"),
                cx,
            );
        }
        None => notice(&format!("No agent named {name}."), cx),
    }
}

/// The stop-all chord, confirmed: every subagent of the main agent's own CLI, and every agent
/// still going in the runs this screen holds. Each keeps what it did.
fn stop_all_agents(cx: &mut Cx<'_>) {
    let mut stopped = 0;
    for subagent in &cx.state.conversation.subagents {
        let ended = matches!(
            subagent.status,
            SubagentStatus::Done | SubagentStatus::Failed
        );
        if !ended {
            cx.out.send(UiCommand::StopSubagent {
                id: subagent.id.clone(),
            });
            stopped += 1;
        }
    }
    for run in cx.state.runs.values() {
        for session in run
            .sessions_in_order()
            .filter(|session| session.is_running())
        {
            cx.out.send(UiCommand::StopAgent {
                run: run.id.clone(),
                session: session.key.clone(),
            });
            stopped += 1;
        }
    }
    let said = match stopped {
        0 => "No background agents running".to_owned(),
        1 => "Stopping 1 agent".to_owned(),
        n => format!("Stopping {n} agents"),
    };
    notice(&said, cx);
}

/// Esc while work runs stops the main agent's turn and nothing else, as on Claude Code's
/// screen: the agents a run started keep going, and stop from the list, one by
/// one or all at once. A run that's already pausing stops now, and so does `/update`.
fn interrupt(cx: &mut Cx<'_>) {
    let command = match cx.state.mode {
        Some(AppMode::Answering { .. }) => UiCommand::CancelTurn,
        Some(AppMode::Working { .. }) if cx.state.conversation.turn.is_some() => {
            UiCommand::CancelTurn
        }
        Some(AppMode::Working { .. }) => {
            let said = run_keeps_going(cx);
            notice(&said, cx);
            return;
        }
        Some(AppMode::Pausing { .. }) => UiCommand::Pause { now: true },
        Some(AppMode::Idle | AppMode::Waiting { .. }) | None
            if cx.state.command_work == Some(CommandWork::Updating) =>
        {
            UiCommand::StopCommand
        }
        Some(AppMode::Idle | AppMode::Waiting { .. }) | None => return,
    };
    cx.out.send(command);
}

/// What Esc says with only a run at work: it keeps going, and the chord that stops its
/// agents, in the list's own words. Short enough to show whole beside the mode at 100 columns.
fn run_keeps_going(cx: &Cx<'_>) -> String {
    let (keymap, _) = Keymap::with_overrides(&cx.settings.keymap);
    let chord = keymap.keys(Action::StopAgents);
    let chord = chord.first().map_or("ctrl+x ctrl+k", String::as_str);
    format!("The run keeps going · {chord} to stop all agents")
}

/// `Stop` stops the run on its row, the one on screen or any other by name.
fn stop_run(run: &str, cx: &mut Cx<'_>) {
    if run.contains('#') {
        notice(
            "This workflow is an agent's own; stop it from that agent",
            cx,
        );
        return;
    }
    match RunId::new(run) {
        Ok(run) => cx.out.send(crate::views::stop(&run, cx.state)),
        Err(_) => notice(&format!("No run named {}.", sanitize_line(run)), cx),
    }
}

/// `Pause` acts on the run on screen too: it stops at its next safe point, and `/resume`
/// carries it on.
fn pause_run(run: &str, cx: &mut Cx<'_>) {
    if run.contains('#') {
        notice(
            "This workflow is an agent's own; stop it from that agent",
            cx,
        );
        return;
    }
    match RunId::new(run) {
        Ok(run) if cx.state.attached.as_ref() == Some(&run) => {
            cx.out.send(UiCommand::Pause { now: false });
        }
        Ok(run) => notice(
            &format!(
                "Only the run on screen can be paused. Resume {} first, then pause it",
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
        ShownMode::AutoPreview => ApprovalMode::AutoPreview,
        ShownMode::Auto => ApprovalMode::Auto,
        ShownMode::Autopilot => ApprovalMode::Autopilot,
    }
}

/// The Stats tab for the period `mods/stats.toml` sets, with the stats mod's page over it
/// when `page`. The numbers, which carry the file's settings, follow from the controller.
pub(super) fn open_stats(cx: &mut Cx<'_>, page: bool) {
    let watch = cx.model.bottom.band.watch.clone();
    let view = OverviewView::new(Tab::Stats, watch).with_stats_period_from_file();
    cx.out
        .open(if page { view.with_stats_page() } else { view });
    cx.out.send(UiCommand::Slash {
        command: SlashCommand::Stats {
            period: None,
            action: ViewAction::Open,
        },
    });
}

/// Ctrl+T and `/workflows`: the run's workflow view, or its Library tab alone when there's no
/// run to show.
pub(super) fn open_tree(cx: &mut Cx<'_>) {
    // A workflow the conversation's own CLI runs now comes first, as Claude Code's does.
    let conversation = &cx.state.conversation.workflows;
    let running = conversation.iter().rev().find(|workflow| !workflow.ended());
    if let Some(workflow) = running {
        let workflow = cli_workflow_run_id(&workflow.id);
        open_conversation_workflow(workflow, cx);
        return;
    }
    match shown_run(cx.state) {
        Some(run) => {
            let entry = waiting::workflow_entry(cx.state, &run.id);
            open_workflow(run.id.clone(), entry, cx);
        }
        None => match conversation.last() {
            Some(workflow) => {
                let workflow = cli_workflow_run_id(&workflow.id);
                open_conversation_workflow(workflow, cx);
            }
            None => cx.out.send(UiCommand::Request {
                view: ViewRequest::WorkflowLibrary,
            }),
        },
    }
}

/// The workflow view of one the conversation's CLI runs itself.
fn open_conversation_workflow(workflow: WorkflowId, cx: &mut Cx<'_>) {
    cx.out.send(UiCommand::Request {
        view: ViewRequest::Workflow {
            run: conversation_run(),
            workflow: Some(workflow),
        },
    });
}

/// Enter on a panel row: a run's workflow, or a workflow an agent's own CLI runs, whose key
/// names its run and its id after a `#`.
pub(crate) fn open_row(key: &str, cx: &mut Cx<'_>) {
    let (run, agent_id) = match key.split_once('#') {
        Some((run, workflow)) => (run, WorkflowId::parse(workflow)),
        None => (key, None),
    };
    let Ok(run) = RunId::new(run) else {
        notice(&format!("No run named {}.", sanitize_line(key)), cx);
        return;
    };
    // A workflow inside a run opens in the run's view, open and with the cursor on it: one a
    // row is, or one a task's row names as where its work runs.
    let inside = cx
        .model
        .bottom
        .workflows
        .rows
        .iter()
        .any(|row| (row.key == key || row.opens.as_deref() == Some(key)) && row.depth > 0);
    if inside {
        cx.model.bottom.workflows.opening = Some(key.to_owned());
        cx.out.send(UiCommand::Request {
            view: ViewRequest::Workflow {
                run,
                workflow: None,
            },
        });
        return;
    }
    match agent_id {
        Some(workflow) => cx.out.send(UiCommand::Request {
            view: ViewRequest::Workflow {
                run,
                workflow: Some(workflow),
            },
        }),
        None => {
            let entry = waiting::entry(cx.state, &run);
            open_workflow(run, entry, cx);
        }
    }
}

/// Opens `entry` of a run: the session of a run that is one worker, the workflow view once its
/// shape is known, or a short view of what the run waits for. A workflow still being written
/// opens nothing, and a line says so. A run from an earlier session comes back on screen first,
/// as `/resume <run>` brings it, with its question if it waits on you.
fn open_workflow(run: RunId, entry: RunEntry, cx: &mut Cx<'_>) {
    let listed = cx
        .state
        .background
        .iter()
        .find(|summary| summary.run == run)
        .filter(|_| !cx.state.runs.contains_key(&run));
    // A run you paused opens paused, its workflow read from its journal: `p` there resumes it.
    if listed.is_some_and(super::panel::paused_by_you) {
        cx.out.send(UiCommand::Request {
            view: ViewRequest::Workflow {
                run,
                workflow: None,
            },
        });
        return;
    }
    if listed.is_some() {
        // Focus goes back to the prompt, so the run's question can take the keys once it opens.
        cx.model.bottom.workflows.blur();
        cx.model.bottom.agents.blur();
        cx.out.send(UiCommand::Resume { run: Some(run) });
        return;
    }
    match entry {
        RunEntry::Session(session) => cx.out.open(SessionView::for_run(run, session)),
        RunEntry::Waiting => cx.out.open(WaitingView::new(run)),
        RunEntry::Writing => notice(waiting::BEING_WRITTEN, cx),
        RunEntry::Workflow => cx.out.send(UiCommand::Request {
            view: ViewRequest::Workflow {
                run,
                workflow: None,
            },
        }),
    }
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

/// A row without a run names its agent by session key, or by CLI when that's all it
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
                |session: &&SessionState| session.agent_id == name || session.role.as_str() == name;
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

/// `/overview`: the usage panel on its Overview tab, which asks for its numbers as it opens, as
/// moving to the tab does.
pub(super) fn open_overview(cx: &mut Cx<'_>) {
    let watch = cx.model.bottom.band.watch.clone();
    cx.out.open(OverviewView::new(Tab::Overview, watch));
    cx.out.send(UiCommand::Request {
        view: ViewRequest::Overview {
            period: argonauts_core::overview::OverviewPeriod::default(),
            import: None,
        },
    });
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
            let doing = if run.waiting.is_some() {
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

/// Dim lines left in scrollback on exit: what keeps going, and how to come back, to the runs
/// and to the conversation, here or in its CLI's own screen.
pub fn exit_lines(state: &AppState) -> Vec<String> {
    let mut lines = run_lines(state);
    let line = state.status.as_ref().map(|status| &status.line);
    if let Some(conversation) = line.and_then(|line| line.conversation.as_ref()) {
        lines.push(format!(
            "Resume this conversation with: argonauts resume {conversation}"
        ));
        if let Some(cli) = line.and_then(|line| line.in_cli.as_ref()) {
            lines.push(format!("Or in {}: {}", cli.cli, cli.command));
        }
    }
    lines
}

/// What keeps going after you quit, and how to reattach.
fn run_lines(state: &AppState) -> Vec<String> {
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
            let mut parts = vec![if run.waiting.is_some() {
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

/// The agent panel, as ← on an empty prompt opens it. It asks the controller for its rows as
/// it opens: conversations, the CLIs' own sessions and other repos' runs, and shows the last
/// list it was sent until they arrive.
pub(super) fn open_agent_panel(cx: &mut Cx<'_>) {
    cx.out.open(AgentPanelView::opened_left(cx.state));
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

    /// Enter on a workflow inside a run asks for the run's own view, which opens on it.
    #[test]
    fn enter_on_a_workflow_inside_a_run_asks_for_the_runs_view() {
        use crate::bottom::{WorkflowRow, WorkflowRowState};
        let key = "run-01#cli-wf_due";
        let state = AppState::default();
        let mut model = ViewModel::default();
        let mut row = WorkflowRow::new(key, "add-due-flag", WorkflowRowState::Running);
        row.depth = 1;
        model.bottom.workflows.rows.push(row);
        let mut out = Effects::default();
        let settings = UiSettings::default();
        let mut cx = Cx {
            model: &mut model,
            state: &state,
            settings: &settings,
            now: Instant::now(),
            out: &mut out,
        };
        assert!(KeyActions::default().on_action(&UiAction::OpenWorkflow(key.into()), &mut cx));
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: ViewRequest::Workflow {
                    run: run_id(),
                    workflow: None,
                },
            }]
        );
        assert_eq!(model.bottom.workflows.opening.as_deref(), Some(key));
    }

    #[test]
    fn comma_on_the_pinned_stats_card_opens_its_page_over_the_stats_tab() {
        use argonauts_core::settings::ui::ModName;

        let page = |the_mod| {
            UiAction::Command(UiCommand::Slash {
                command: SlashCommand::Mods {
                    action: ModsAction::Page { the_mod },
                    scope: None,
                },
            })
        };
        let state = AppState::default();
        let (out, _) = act(&state, &page(ModName::Stats));
        assert_eq!(out.views.len(), 1);
        assert_eq!(
            out.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stats {
                    period: None,
                    action: ViewAction::Open,
                },
            }]
        );
        let (out, _) = act(&state, &page(ModName::Spend));
        assert!(out.views.is_empty());
        assert_eq!(out.commands.len(), 1);
    }

    #[test]
    fn enter_on_a_pinned_mod_opens_the_tab_that_shows_it_whole() {
        use argonauts_core::settings::ui::ModName;

        let state = AppState::default();
        let request = |view| UiCommand::Request { view };
        let (out, _) = act(&state, &UiAction::OpenTab(ModName::Run));
        assert_eq!(out.views.len(), 1);
        assert_eq!(out.commands, [request(ViewRequest::StatusTab)]);
        // Never the old read, which printed "No runs in this repo yet." into the conversation.
        for pinned in [ModName::Run, ModName::Waiting, ModName::Recent] {
            let (out, _) = act(&state, &UiAction::OpenTab(pinned));
            assert!(
                !out.commands.contains(&request(ViewRequest::Status)),
                "{pinned:?}"
            );
        }
        let (out, _) = act(&state, &UiAction::OpenTab(ModName::Context));
        assert_eq!(out.commands, [request(ViewRequest::Context)]);
        // Every tab opens on All, so its first numbers are every day's.
        let (out, _) = act(&state, &UiAction::OpenTab(ModName::Spend));
        assert_eq!(out.views.len(), 1);
        assert!(
            matches!(
                out.commands[..],
                [
                    UiCommand::Request {
                        view: ViewRequest::Usage {
                            period: UsagePeriod::All
                        }
                    },
                    UiCommand::Request {
                        view: ViewRequest::ReadPlanUsage
                    }
                ]
            ),
            "{:?}",
            out.commands
        );
        // Stats asks as `/stats` does, for the period its file sets.
        let (out, _) = act(&state, &UiAction::OpenTab(ModName::Stats));
        assert_eq!(out.views.len(), 1);
        assert_eq!(
            out.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stats {
                    period: None,
                    action: ViewAction::Open,
                },
            }]
        );
    }

    #[test]
    fn a_selection_goes_to_the_clipboard_with_a_short_notice() {
        let (out, model) = act(&AppState::default(), &UiAction::Copy("a word".into()));
        assert_eq!(out.clipboard.as_deref(), Some("a word"));
        let notice = model.bottom.mode.notice.expect("a notice");
        assert_eq!(notice.text, "Copied");
    }

    fn subagent(id: &str, label: &str, status: SubagentStatus) -> argonauts_core::AgentEvent {
        argonauts_core::AgentEvent::Subagent {
            update: argonauts_core::events::session::SubagentUpdate {
                id: id.into(),
                parent: None,
                kind: argonauts_core::events::session::SubagentKind::Subagent,
                label: Some(label.into()),
                prompt: None,
                status,
                last_message: None,
                log_uri: None,
                steps_visible: true,
                tokens: None,
            },
        }
    }

    /// The conversation's own turn with two subagents, one of them finished.
    fn chatting() -> AppState {
        use argonauts_core::app::{AppEvent, ConversationEvent};
        let turn = TurnId::numbered(1);
        let mut state = AppState::default();
        state.apply(&AppEvent::Conversation {
            event: ConversationEvent::TurnStarted {
                turn: turn.clone(),
                text: "split the checks".into(),
                worker: "claude-code/opus".parse().unwrap(),
                read_only: true,
                commit: String::new(),
                uncommitted: Vec::new(),
                settings_sha256: None,
                continues: false,
            },
        });
        for event in [
            subagent("toolu_a", "Count slowly", SubagentStatus::Running),
            subagent("toolu_b", "Read the readme", SubagentStatus::Done),
        ] {
            state.apply(&AppEvent::Conversation {
                event: ConversationEvent::Agent {
                    turn: turn.clone(),
                    event,
                },
            });
        }
        state
    }

    fn of_main(id: &str) -> AgentRef {
        AgentRef {
            run: Some(conversation_run().to_string()),
            session: id.into(),
        }
    }

    /// A message for a subagent of the main agent's own CLI goes to the main agent to pass
    /// on, as a command of its own that no mention parsing reads, and the screen says it went
    /// that way.
    #[test]
    fn a_message_for_a_subagent_goes_through_the_main_agent_and_says_so() {
        let state = chatting();
        let message = UiAction::MessageAgent {
            agent: of_main("toolu_a"),
            name: "Count slowly".into(),
            text: " also check the tests ".into(),
        };
        let (out, model) = act(&state, &message);
        assert_eq!(
            out.commands,
            [UiCommand::MessageSubagent {
                name: "Count slowly".into(),
                text: "also check the tests".into(),
            }]
        );
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "Sent through the main agent, which passes it on to Count slowly"
        );
    }

    /// `x` twice on a subagent's row stops that one, and the stop-all chord stops every one
    /// still running, in the conversation and in the runs on screen.
    #[test]
    fn one_subagent_stops_alone_and_the_chord_stops_every_one_still_running() {
        let mut state = chatting();
        let (out, _) = act(&state, &UiAction::StopAgent(of_main("toolu_a")));
        assert_eq!(
            out.commands,
            [UiCommand::StopSubagent {
                id: "toolu_a".into()
            }]
        );
        state.runs = crate::mentions::fixture::working_state().runs;
        let running: Vec<_> = state
            .runs
            .values()
            .flat_map(|run| run.sessions_in_order().filter(|s| s.is_running()))
            .map(|session| session.key.clone())
            .collect();
        assert!(!running.is_empty());
        let (out, model) = act(&state, &UiAction::StopAllAgents);
        assert_eq!(
            out.commands[0],
            UiCommand::StopSubagent {
                id: "toolu_a".into()
            },
            "the finished one is left alone"
        );
        let stopped: Vec<_> = out.commands[1..]
            .iter()
            .map(|command| match command {
                UiCommand::StopAgent { session, .. } => session.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(stopped, running);
        let said = model.bottom.mode.notice.expect("a notice").text;
        assert_eq!(said, format!("Stopping {} agents", running.len() + 1));

        let (out, model) = act(&AppState::default(), &UiAction::StopAllAgents);
        assert!(out.commands.is_empty());
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "No background agents running"
        );
    }

    /// Ctrl+] opens the most recent artifact in the browser and says so, and only a
    /// claude.ai page is ever handed over.
    #[test]
    fn the_open_artifact_key_opens_the_most_recent_one() {
        use argonauts_core::app::{AppEvent, ConversationEvent};
        let url = "https://claude.ai/code/artifact/0b1e7c9a-4f1d-4c57-9a55-2f6f0d3a8e11";
        let (out, model) = act(&AppState::default(), &UiAction::OpenArtifact);
        assert_eq!(out.open_link, None);
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "No artifacts yet. Publish one with the Artifact tool."
        );

        let mut state = chatting();
        let turn = TurnId::numbered(1);
        for event in [
            argonauts_core::AgentEvent::ToolStarted {
                id: "toolu_p".into(),
                name: "Artifact".into(),
                input: serde_json::json!({"file_path": "out/weekly-usage-chart.html"}),
                parent: None,
            },
            argonauts_core::AgentEvent::ToolFinished {
                id: "toolu_p".into(),
                ok: true,
                output: Some(format!("Published out/weekly-usage-chart.html at {url}")),
                parent: None,
            },
        ] {
            state.apply(&AppEvent::Conversation {
                event: ConversationEvent::Agent {
                    turn: turn.clone(),
                    event,
                },
            });
        }
        let (out, model) = act(&state, &UiAction::OpenArtifact);
        assert_eq!(out.open_link.as_deref(), Some(url));
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "Opened weekly-usage-chart in your browser"
        );

        let elsewhere = UiAction::OpenLink {
            url: "https://example.com/artifact/1".into(),
            name: "x".into(),
        };
        let (out, model) = act(&state, &elsewhere);
        assert_eq!(out.open_link, None);
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "That isn't a claude.ai link, so it wasn't opened"
        );
    }

    /// A subagent of the main agent's own CLI has no view of its own.
    #[test]
    fn enter_on_a_subagent_says_where_its_steps_are() {
        let (out, model) = act(&chatting(), &UiAction::OpenAgent(of_main("toolu_a")));
        assert!(out.commands.is_empty() && out.views.is_empty());
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "Its steps are in the conversation · ctrl+o shows them all"
        );
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
            // A run's agents keep going past Esc.
            (Some(AppMode::Working { run: run.clone() }), Vec::new()),
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
        // `/update` at work stops; the Doctor check after it runs to its end.
        for (work, sent) in [
            (CommandWork::Updating, vec![UiCommand::StopCommand]),
            (CommandWork::Checking, Vec::new()),
        ] {
            let mut state = AppState::default();
            state.mode = Some(AppMode::Idle);
            state.command_work = Some(work);
            let (out, _) = act(&state, &UiAction::Interrupt);
            assert_eq!(out.commands, sent, "{work:?}");
        }
        // With only a run at work Esc says it keeps going, and how to stop its agents.
        let mut state = AppState::default();
        state.mode = Some(AppMode::Working { run: run.clone() });
        let (_, model) = act(&state, &UiAction::Interrupt);
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "The run keeps going · ctrl+x ctrl+k to stop all agents"
        );
        // A turn the main agent answers meanwhile is what Esc stops.
        state.conversation.turn = Some(TurnId::numbered(1));
        let (out, model) = act(&state, &UiAction::Interrupt);
        assert_eq!(out.commands, [UiCommand::CancelTurn]);
        assert!(model.bottom.mode.notice.is_none());
        // The list pauses the run on screen, and says so of any other.
        state.attached = Some(run.clone());
        let (out, _) = act(&state, &UiAction::PauseRun(run.as_str().to_owned()));
        assert_eq!(out.commands, [UiCommand::Pause { now: false }]);
        let (out, model) = act(&state, &UiAction::PauseRun("run-02".into()));
        assert!(out.commands.is_empty());
        assert_eq!(
            model.bottom.mode.notice.expect("a notice").text,
            "Only the run on screen can be paused. Resume run-02 first, then pause it"
        );
        // Stop acts on any run's row: the one on screen as Stop, another by name.
        let (out, _) = act(&state, &UiAction::StopRun(run.as_str().to_owned()));
        assert_eq!(out.commands, [UiCommand::Stop]);
        let (out, _) = act(&state, &UiAction::StopRun("run-02".into()));
        assert_eq!(
            out.commands,
            [UiCommand::Slash {
                command: SlashCommand::Stop {
                    run: Some(RunId::new("run-02").unwrap()),
                },
            }]
        );
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
        // with its question.
        let mut earlier = AppState::default();
        earlier.apply(&argonauts_core::app::AppEvent::BackgroundRuns {
            runs: vec![argonauts_core::api::RunSummary {
                run: run_id(),
                title: "hi".into(),
                mode: argonauts_core::events::run::RunMode::Captain,
                workflow: "council".into(),
                state: argonauts_core::api::RunState::Waiting,
                stopped_at: String::new(),
                updated: time::OffsetDateTime::UNIX_EPOCH,
                spent: argonauts_core::Spend::default(),
                sessions_to_resume: 0,
                waiting: vec![argonauts_core::names::ApprovalId::numbered(1)],
                live: false,
                remind: false,
                name: None,
                shape_known: true,
            }],
        });
        let (out, _) = act(&earlier, &UiAction::OpenWorkflow("run-01".into()));
        assert_eq!(
            out.commands,
            [UiCommand::Resume {
                run: Some(run_id())
            }]
        );
        // One you paused opens paused: its workflow, which `p` resumes.
        let mut paused = earlier.clone();
        if let Some(listed) = paused.background.first_mut() {
            listed.waiting.clear();
            listed.stopped_at = "paused".into();
        }
        let (out, _) = act(&paused, &UiAction::OpenWorkflow("run-01".into()));
        assert_eq!(
            out.commands,
            [UiCommand::Request {
                view: ViewRequest::Workflow {
                    run: run_id(),
                    workflow: None
                }
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

        let (out, _) = act(&state, &UiAction::OpenAgentPanel);
        assert_eq!(out.views.len(), 1);
        // The panel asks for its rows as it opens, as it does however it opens.
        assert!(out.commands.is_empty());
        assert_eq!(out.views[0].asks_on_open(), Some(ViewRequest::Crew));
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

    /// Enter on a run's row always opens something: the main agent at work while it
    /// designs, the workflow view once declared, and before any agent, what the run waits for.
    #[test]
    fn enter_on_a_run_row_always_opens_something() {
        use crate::views::waiting::fixture::{RUN, RunLog, run_id};

        let opened = |log: &RunLog, action: &UiAction| {
            let (out, model) = act(&log.state, action);
            assert!(model.bottom.mode.notice.is_none(), "no hint: {action:?}");
            let views: Vec<String> = out.views.iter().map(|view| format!("{view:?}")).collect();
            (views, out.commands)
        };
        let enter = UiAction::OpenWorkflow(RUN.into());

        let mut log = RunLog::started();
        let (views, commands) = opened(&log, &enter);
        assert!(commands.is_empty(), "{commands:?}");
        assert!(views[0].starts_with("WaitingView"), "{views:?}");

        // While the main agent designs it, the workflow is being written: nothing opens, and
        // a line says so, for Enter and for Ctrl+T on the run on screen alike.
        let writing = |log: &RunLog, action: &UiAction| {
            let (out, model) = act(&log.state, action);
            assert!(out.views.is_empty(), "{action:?}");
            assert!(out.commands.is_empty(), "{action:?}");
            let notice = model.bottom.mode.notice.map(|notice| notice.text);
            assert_eq!(
                notice.as_deref(),
                Some(crate::views::waiting::BEING_WRITTEN)
            );
        };
        log.design("s-001", "main:design");
        writing(&log, &enter);
        log.state.attached = Some(run_id());
        writing(&log, &UiAction::OpenTree);
        log.ended("s-001");
        log.design("s-002", "main:design-2");
        writing(&log, &enter);

        log.ended("s-002");
        log.declare();
        let (views, commands) = opened(&log, &enter);
        assert!(views.is_empty(), "{views:?}");
        assert_eq!(
            commands,
            [UiCommand::Request {
                view: ViewRequest::Workflow {
                    run: run_id(),
                    workflow: None
                }
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
    fn quit_says_how_to_resume_the_conversation_here_or_in_its_cli() {
        use argonauts_core::app::{HeaderAndBottomLine, LineFacts};
        use argonauts_core::cli_sessions::{CliSessionRef, InItsCli};
        use argonauts_core::names::{AgentId, ConversationId};

        let id = "00000000-0000-4000-8000-000000000001";
        let line = LineFacts {
            conversation: Some(ConversationId::numbered(4)),
            in_cli: Some(InItsCli {
                session: CliSessionRef {
                    agent_id: AgentId("claude-code".into()),
                    id: id.into(),
                },
                conversation: Some(ConversationId::numbered(4)),
                cli: "Claude Code".to_owned(),
                command: format!("claude --resume {id}"),
                local_model: false,
                also: None,
                made_there: 0,
            }),
            ..LineFacts::default()
        };
        let status: HeaderAndBottomLine = serde_json::from_value(serde_json::json!({
            "title": "", "repo": "~/github/tasks-cli", "workflow": "captain",
            "ultracode": false, "ultra": null, "run": null, "main_agent": "claude-code",
            "plan_chips": [], "spent": null, "limit": null, "narrator": "off",
            "approval_mode": "supervised",
        }))
        .unwrap();
        let mut state = AppState::default();
        state.status = Some(HeaderAndBottomLine {
            line: Box::new(line),
            ..status
        });
        assert_eq!(
            exit_lines(&state),
            [
                "Resume this conversation with: argonauts resume conversation-004".to_owned(),
                format!("Or in Claude Code: claude --resume {id}"),
            ]
        );
        state.runs.insert(run_id(), RunState::new(run_id()));
        assert_eq!(
            exit_lines(&state)[1],
            "Reattach with: argonauts resume run-01"
        );
        assert_eq!(exit_lines(&state).len(), 4);
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
