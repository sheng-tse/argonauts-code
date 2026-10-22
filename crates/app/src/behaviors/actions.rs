//! Every action a run's agents took, labelled, with who took it and how its task ended.

use std::collections::BTreeMap;

use argonauts_core::app::ConversationEvent;
use argonauts_core::behaviors::Behavior;
use argonauts_core::crew_stats::StatsRole;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::{ReviewEvent, TaskEvent, TaskState};
use argonauts_core::events::{RunEvent, RunEventKind, SessionEvent};
use argonauts_core::model_catalog::ModelCatalog;
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{AgentEvent, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use super::labeller::{self, ToolCall};
use super::turn_times::TurnTimes;
use crate::views::sessions::RunFacts;
use crate::work_ledger::{Reported, filed_as, role_of};

/// The most of an action's command or file an action keeps, in characters.
const WHAT_CAP: usize = 300;

/// One action an agent took.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Action {
    /// The state folder of the project it worked in, such as `council-0123456789ab`.
    #[serde(default)]
    pub repo: String,
    /// The run, or the conversation for the main agent's own turns.
    pub run: String,
    pub session: String,
    /// The tool call's id within its session.
    pub call: String,
    /// The name the run gave its agent, such as `build:per-api-key`.
    pub agent: String,
    /// The CLI or endpoint that ran it, such as `codex`.
    pub cli: String,
    /// As the model catalog spells it. `None` when only the CLI is known.
    pub model: Option<String>,
    pub role: StatsRole,
    /// The workflow its run ran, such as `council`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// How its task ended. `None` outside a task, and while the run goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// As the CLI names it.
    pub tool: String,
    /// The command it ran or the file it touched, cut short.
    pub what: String,
    /// `None` when its end was never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    pub behavior: Behavior,
    /// How its session ended, once the log says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<SessionEnded>,
    /// The call's input as the CLI gave it, for an export. Never kept with the labels.
    #[serde(skip)]
    pub input: Value,
    /// The main agent's own, in a conversation, rather than a run's. Only runs are kept.
    #[serde(skip)]
    pub conversation: bool,
    /// What the first review of its task after it said, which its example's words show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How the work an action was part of ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Outcome {
    /// Its task merged with this work in it.
    Merged,
    /// A review sent this work back.
    SentBack,
    /// Its task ended without merging.
    Dropped,
}

impl Outcome {
    /// As the Behaviors map says it.
    pub fn words(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::SentBack => "sent back",
            Self::Dropped => "dropped",
        }
    }

    /// As reports and exports name it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::SentBack => "sent_back",
            Self::Dropped => "dropped",
        }
    }
}

/// How a session ended, as an export says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionEnded {
    Completed,
    /// You or a limit stopped it, or its context filled and the next session carried on.
    Stopped,
    /// Its CLI failed, crashed, got stuck or couldn't go on.
    Failed,
}

impl SessionEnded {
    fn of(reason: &EndReason) -> SessionEnded {
        match reason {
            EndReason::Completed => SessionEnded::Completed,
            EndReason::Paused
            | EndReason::Stopped
            | EndReason::ContextFull
            | EndReason::LimitHit { .. }
            | EndReason::UsageLimit { .. } => SessionEnded::Stopped,
            _ => SessionEnded::Failed,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            SessionEnded::Completed => "completed",
            SessionEnded::Stopped => "stopped",
            SessionEnded::Failed => "failed",
        }
    }
}

impl Action {
    /// The folder it worked in, by name.
    pub fn project(&self) -> &str {
        super::records::project_name(&self.repo)
    }
}

/// What an older log says of a session whose start no longer reads as today's event, from
/// the line's own fields.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OlderStart {
    pub worker: ModelRef,
    pub role: String,
    pub label: Option<String>,
    pub task: Option<String>,
}

/// The session starts among `lines`, the log lines that didn't read as events, by session.
pub(crate) fn older_starts<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> BTreeMap<String, OlderStart> {
    let mut starts = BTreeMap::new();
    for line in lines {
        let Ok(line) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = &line["kind"]["data"];
        let started = line["kind"]["type"] == "session" && data["event"] == "started";
        let (Some(session), Some(worker)) = (line["session"].as_str(), data["worker"].as_str())
        else {
            continue;
        };
        let Ok(worker) = worker.parse::<ModelRef>() else {
            continue;
        };
        if started {
            starts.insert(
                session.to_owned(),
                OlderStart {
                    worker,
                    role: data["role"].as_str().unwrap_or_default().to_owned(),
                    label: data["label"].as_str().map(str::to_owned),
                    task: line["task_id"].as_str().map(str::to_owned),
                },
            );
        }
    }
    starts
}

/// Whose a call is: a session whose start the log reads, by where it stands among them, or
/// one known only from an older start line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Whose<'a> {
    Started(usize),
    Older(&'a SessionKey),
}

/// A call seen in the log, until its end and patch are known.
struct Seen<'a> {
    at_event: usize,
    session: Whose<'a>,
    id: &'a str,
    name: &'a str,
    input: &'a Value,
    ok: Option<bool>,
    patch: Option<String>,
}

/// One file's diff as Argonauts Code took it after a change, against the worktree's last
/// commit, with only what changed since the diff before it of the same file.
struct Diffed<'a> {
    at_event: usize,
    session: Whose<'a>,
    path: &'a str,
    whole: &'a str,
    since_before: String,
}

/// The lines of `now` that `before`, an earlier diff of the same file, didn't have: both are
/// against the same commit, so what one change did is what the later diff adds to the earlier.
fn since(before: Option<&str>, now: &str) -> String {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for line in before.unwrap_or_default().lines() {
        *seen.entry(line).or_default() += 1;
    }
    let mut out = String::new();
    for line in now.lines() {
        let changed = line.starts_with('+') || line.starts_with('-');
        match seen.get_mut(line) {
            Some(count) if *count > 0 && changed => *count -= 1,
            _ if changed || line.starts_with("@@") => {
                out.push_str(line);
                out.push('\n');
            }
            _ => {}
        }
    }
    out
}

/// The file a call changed: the first of Codex's `file_change` list, else the one it names.
fn changed_file(input: &Value) -> Option<&str> {
    let listed = input
        .get("changes")
        .and_then(Value::as_array)
        .and_then(|changes| changes.first())
        .and_then(|change| change.get("path"))
        .and_then(Value::as_str);
    listed.or_else(|| labeller::path_of(input))
}

/// Paths to one file, as a CLI and a worktree's diff each give it: one may be relative.
fn same_file(one: &str, other: &str) -> bool {
    one == other || one.ends_with(&format!("/{other}")) || other.ends_with(&format!("/{one}"))
}

/// Every action in a run's log, oldest first.
pub(crate) fn of_run(
    run: &str,
    events: &[RunEvent],
    older: &BTreeMap<String, OlderStart>,
    catalog: &ModelCatalog,
) -> Vec<Action> {
    let facts = RunFacts::of(events);
    let reported = Reported::of(events);
    let workflow = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) if !started.workflow.is_empty() => {
            Some(started.workflow.clone())
        }
        _ => None,
    });
    let root = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(started)) => {
            Some(started.repo_root.to_string_lossy().into_owned())
        }
        _ => None,
    });
    let ended = events
        .iter()
        .any(|event| matches!(event.kind, RunEventKind::Run(RunLifecycle::Finished { .. })));

    let mut latest = BTreeMap::new();
    let mut started_sessions = 0usize;
    let mut calls: Vec<Seen<'_>> = Vec::new();
    let mut by_id: BTreeMap<(Whose<'_>, &str), usize> = BTreeMap::new();
    // The model each older session's CLI said it runs.
    let mut older_models: BTreeMap<&SessionKey, &str> = BTreeMap::new();
    let mut verdicts: BTreeMap<&TaskId, Vec<(usize, Outcome)>> = BTreeMap::new();
    // Each task's checkpoints, by number, and the times its branch went back to one.
    let mut checkpoints: BTreeMap<&TaskId, Vec<(usize, u32)>> = BTreeMap::new();
    let mut rewinds: BTreeMap<&TaskId, Vec<(usize, u32)>> = BTreeMap::new();
    let mut diffs: Vec<Diffed<'_>> = Vec::new();
    let mut ends: BTreeMap<Whose<'_>, SessionEnded> = BTreeMap::new();
    for (at_event, event) in events.iter().enumerate() {
        let session = event
            .session
            .as_ref()
            .and_then(|key| match latest.get(key) {
                Some(at) => Some(Whose::Started(*at)),
                None => older
                    .contains_key(key.as_str())
                    .then_some(Whose::Older(key)),
            });
        match &event.kind {
            RunEventKind::Session(SessionEvent::Started(_)) => {
                if let Some(key) = &event.session {
                    latest.insert(key.clone(), started_sessions);
                    started_sessions += 1;
                }
            }
            RunEventKind::Agent(AgentEvent::Init {
                model: Some(model), ..
            }) => {
                if let Some(Whose::Older(key)) = session {
                    older_models.insert(key, model);
                }
            }
            RunEventKind::Agent(AgentEvent::ToolStarted {
                id, name, input, ..
            }) => {
                if let Some(session) = session {
                    by_id.insert((session, id.as_str()), calls.len());
                    calls.push(Seen {
                        at_event,
                        session,
                        id,
                        name,
                        input,
                        ok: None,
                        patch: None,
                    });
                }
            }
            RunEventKind::Agent(AgentEvent::ToolFinished { id, ok, .. }) => {
                if let Some(seen) = session
                    .and_then(|session| by_id.get(&(session, id.as_str())))
                    .and_then(|at| calls.get_mut(*at))
                {
                    seen.ok = Some(*ok);
                }
            }
            RunEventKind::Agent(AgentEvent::FilePatch { id, hunks, .. }) => {
                if let Some(seen) = session
                    .and_then(|session| by_id.get(&(session, id.as_str())))
                    .and_then(|at| calls.get_mut(*at))
                {
                    seen.patch = Some(hunks.clone());
                }
            }
            RunEventKind::Session(SessionEvent::FileDiff(diff)) => {
                if let Some(session) = session {
                    let before = diffs
                        .iter()
                        .rev()
                        .find(|earlier| earlier.session == session && earlier.path == diff.path)
                        .map(|earlier| earlier.whole);
                    diffs.push(Diffed {
                        at_event,
                        session,
                        path: &diff.path,
                        whole: &diff.hunk,
                        since_before: since(before, &diff.hunk),
                    });
                }
            }
            RunEventKind::Session(SessionEvent::Ended(end)) => {
                if let Some(session) = session {
                    ends.insert(session, SessionEnded::of(&end.reason));
                }
            }
            RunEventKind::Task(TaskEvent::CheckpointTagged { n, .. }) => {
                if let Some(task) = &event.task_id {
                    checkpoints.entry(task).or_default().push((at_event, *n));
                }
            }
            RunEventKind::Task(TaskEvent::Rewound { to, .. }) => {
                if let Some(task) = &event.task_id {
                    rewinds.entry(task).or_default().push((at_event, *to));
                }
            }
            RunEventKind::Review(ReviewEvent::Reviewed { review, .. })
                if review.verdict != ReviewVerdict::Approve =>
            {
                if let Some(task) = &event.task_id {
                    verdicts
                        .entry(task)
                        .or_default()
                        .push((at_event, Outcome::SentBack));
                }
            }
            RunEventKind::Review(ReviewEvent::Settled { ruling, .. })
                if ruling.verdict != ReviewVerdict::Approve =>
            {
                if let Some(task) = &event.task_id {
                    verdicts
                        .entry(task)
                        .or_default()
                        .push((at_event, Outcome::SentBack));
                }
            }
            RunEventKind::Task(TaskEvent::State {
                to: TaskState::Merged,
                ..
            }) => {
                if let Some(task) = &event.task_id {
                    verdicts
                        .entry(task)
                        .or_default()
                        .push((at_event, Outcome::Merged));
                }
            }
            _ => {}
        }
    }

    // The work an action is part of ends at the first verdict on its task after it: a review
    // that sends it back, or the merge. With none, the task ended without merging, once the
    // run has ended. A session that failed leaves its work in the task's worktree, where the
    // next try carries it on, so its actions share that end; only a branch sent back to a
    // checkpoint from before them drops their work.
    let outcome = |task: Option<&TaskId>, at_event: usize| -> Option<Outcome> {
        let task = task?;
        let next = verdicts
            .get(task)
            .and_then(|verdicts| verdicts.iter().find(|(at, _)| *at > at_event));
        let tagged_before = |n: u32| {
            n == 0
                || checkpoints.get(task).is_some_and(|tags| {
                    tags.iter()
                        .any(|(tagged, number)| *number == n && *tagged < at_event)
                })
        };
        let set_aside = rewinds.get(task).is_some_and(|rewinds| {
            rewinds.iter().any(|(at, to)| {
                *at > at_event && next.is_none_or(|(verdict, _)| at < verdict) && tagged_before(*to)
            })
        });
        match next {
            _ if set_aside => Some(Outcome::Dropped),
            Some((_, outcome)) => Some(*outcome),
            None => ended.then_some(Outcome::Dropped),
        }
    };

    // A Codex `file_change` names its files without their lines, which the diff Argonauts
    // Code takes after it gives.
    for seen in &mut calls {
        if seen.patch.is_some() || !seen.name.eq_ignore_ascii_case("file_change") {
            continue;
        }
        let Some(file) = changed_file(seen.input) else {
            continue;
        };
        seen.patch = diffs
            .iter()
            .find(|diff| {
                diff.at_event > seen.at_event
                    && diff.session == seen.session
                    && same_file(diff.path, file)
            })
            .map(|diff| diff.since_before.clone());
    }

    let mut reviews: BTreeMap<&TaskId, Vec<(usize, &str)>> = BTreeMap::new();
    for (at_event, event) in events.iter().enumerate() {
        if let (RunEventKind::Review(ReviewEvent::Reviewed { review, .. }), Some(task)) =
            (&event.kind, &event.task_id)
        {
            reviews
                .entry(task)
                .or_default()
                .push((at_event, review.summary.trim()));
        }
    }
    // The review that judged the work an action was part of is the first of its task after it.
    let note = |task: Option<&TaskId>, at_event: usize| -> Option<String> {
        let (_, summary) = reviews.get(task?)?.iter().find(|(at, _)| *at > at_event)?;
        (!summary.is_empty()).then(|| (*summary).to_owned())
    };

    let older_action = |seen: &Seen<'_>, key: &SessionKey| -> Option<Action> {
        let start = older.get(key.as_str())?;
        let mut worker = start.worker.clone();
        if let Some(model) = older_models.get(key) {
            worker.model = Some((*model).to_owned());
        }
        let model = filed_as(catalog, &worker);
        let call = ToolCall {
            name: seen.name,
            input: seen.input,
            ok: seen.ok,
            patch: seen.patch.as_deref(),
            root: root.as_deref(),
        };
        Some(Action {
            repo: String::new(),
            run: run.to_owned(),
            session: key.to_string(),
            call: seen.id.to_owned(),
            agent: start.label.clone().unwrap_or_else(|| start.role.clone()),
            cli: model.agent,
            model: model.model,
            role: StatsRole::counted_as(&start.role),
            workflow: workflow.clone(),
            task: start.task.clone(),
            // How its task ended may be in lines that no longer read either.
            outcome: None,
            at: events.get(seen.at_event)?.ts,
            tool: seen.name.to_owned(),
            what: what_of(seen.input),
            ok: seen.ok,
            behavior: labeller::label(&call),
            ended: ends.get(&seen.session).copied(),
            input: seen.input.clone(),
            conversation: false,
            note: None,
        })
    };

    calls
        .into_iter()
        .filter_map(|seen| {
            let at = match seen.session {
                Whose::Started(at) => at,
                Whose::Older(key) => return older_action(&seen, key),
            };
            let session = facts.sessions.get(at)?;
            let model = filed_as(catalog, &reported.ran(at, session));
            let call = ToolCall {
                name: seen.name,
                input: seen.input,
                ok: seen.ok,
                patch: seen.patch.as_deref(),
                root: root.as_deref(),
            };
            Some(Action {
                repo: String::new(),
                run: run.to_owned(),
                session: session.key.to_string(),
                call: seen.id.to_owned(),
                agent: session.label(),
                cli: model.agent.clone(),
                model: model.model.clone(),
                role: StatsRole::counted_as(role_of(&session.started)),
                workflow: workflow.clone(),
                task: session.task.as_ref().map(ToString::to_string),
                outcome: outcome(session.task.as_ref(), seen.at_event),
                at: events
                    .get(seen.at_event)
                    .map_or(session.started_at, |event| event.ts),
                tool: seen.name.to_owned(),
                what: what_of(seen.input),
                ok: seen.ok,
                behavior: labeller::label(&call),
                ended: ends.get(&seen.session).copied(),
                input: seen.input.clone(),
                conversation: false,
                note: note(session.task.as_ref(), seen.at_event),
            })
        })
        .collect()
}

/// The main agent's own actions in a conversation's journal, taken by the worker each turn
/// started with, each dated by its turn, with files judged below `root`, the project's folder,
/// where it's known.
pub(crate) fn of_conversation(
    conversation: &str,
    events: &[ConversationEvent],
    times: &TurnTimes,
    root: Option<&str>,
    catalog: &ModelCatalog,
) -> Vec<Action> {
    let mut worker: Option<ModelRef> = None;
    let mut actions: Vec<Action> = Vec::new();
    // Codex numbers its calls afresh each turn, so a call is known by its turn too.
    let mut by_id: BTreeMap<(String, &str), usize> = BTreeMap::new();
    let mut patches: BTreeMap<usize, &str> = BTreeMap::new();
    for event in events {
        match event {
            ConversationEvent::TurnStarted {
                worker: started, ..
            } => worker = Some(started.clone()),
            // The model the CLI says it runs, which an alias in the settings may not name.
            ConversationEvent::Agent {
                event:
                    AgentEvent::Init {
                        model: Some(model), ..
                    },
                ..
            } => {
                if let Some(worker) = &mut worker {
                    worker.model = Some(model.clone());
                }
            }
            ConversationEvent::Agent {
                turn,
                event:
                    AgentEvent::ToolStarted {
                        id, name, input, ..
                    },
            } => {
                let Some(worker) = &worker else { continue };
                let model = filed_as(catalog, worker);
                by_id.insert((turn.to_string(), id), actions.len());
                actions.push(Action {
                    repo: String::new(),
                    run: conversation.to_owned(),
                    session: turn.to_string(),
                    call: id.clone(),
                    agent: "main agent".to_owned(),
                    cli: model.agent,
                    model: model.model,
                    role: StatsRole::Main,
                    workflow: None,
                    task: None,
                    outcome: None,
                    at: times.at(turn.as_str()),
                    tool: name.clone(),
                    what: what_of(input),
                    ok: None,
                    behavior: Behavior::Unlabelled,
                    ended: None,
                    input: input.clone(),
                    conversation: true,
                    note: None,
                });
            }
            ConversationEvent::Agent {
                turn,
                event: AgentEvent::ToolFinished { id, ok, .. },
            } => {
                let call = (turn.to_string(), id.as_str());
                if let Some(action) = by_id.get(&call).and_then(|at| actions.get_mut(*at)) {
                    action.ok = Some(*ok);
                }
            }
            ConversationEvent::Agent {
                turn,
                event: AgentEvent::FilePatch { id, hunks, .. },
            } => {
                if let Some(at) = by_id.get(&(turn.to_string(), id.as_str())) {
                    patches.insert(*at, hunks);
                }
            }
            _ => {}
        }
    }
    for (at, action) in actions.iter_mut().enumerate() {
        action.behavior = labeller::label(&ToolCall {
            name: &action.tool,
            input: &action.input,
            ok: action.ok,
            patch: patches.get(&at).copied(),
            root,
        });
    }
    actions
}

/// The command a call ran, out of the shell it was wrapped in, else the file or folder it
/// named, else its input, in one line.
pub(crate) fn what_of(input: &Value) -> String {
    let command = labeller::command_of(input);
    let text = (!command.is_empty())
        .then(|| labeller::script_of(&command))
        .or_else(|| labeller::path_of(input).map(str::to_owned))
        .or_else(|| {
            let first = input.get("changes")?.as_array()?.first()?;
            first.get("path")?.as_str().map(str::to_owned)
        })
        .unwrap_or_else(|| match input {
            Value::Null => String::new(),
            other => other.to_string(),
        });
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(WHAT_CAP) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line,
    }
}

#[cfg(test)]
impl Action {
    /// A builder's `git show` in a run of the council project, for a test to change.
    pub fn sample() -> Action {
        Action {
            repo: "council-0123456789ab".to_owned(),
            run: "run-20261020-123036-4e50".to_owned(),
            session: "s-004".to_owned(),
            call: "item_1".to_owned(),
            agent: "build:per-api-key".to_owned(),
            cli: "codex".to_owned(),
            model: Some("gpt-6-luna".to_owned()),
            role: StatsRole::Worker,
            workflow: Some("council".to_owned()),
            task: Some("task-per-api-key".to_owned()),
            outcome: None,
            at: time::macros::datetime!(2026-10-20 12:30 UTC),
            tool: "command_execution".to_owned(),
            what: "git show ef3f187".to_owned(),
            ok: Some(true),
            behavior: Behavior::One(argonauts_core::behaviors::Label::GitHistory),
            ended: None,
            input: Value::Null,
            conversation: false,
            note: None,
        }
    }
}

#[cfg(test)]
#[path = "actions_tests.rs"]
mod tests;
