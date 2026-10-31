//! A task's box while its seat works on it, drawn from the run as it stands: the way and the
//! task, what it does now (a workflow's steps, the agents on their parts, or the latest tool
//! call of its own session), and its progress.

use std::collections::HashMap;
use std::time::Duration;

use argonauts_core::TaskId;
use argonauts_core::base::names::{ModelRef, SeatId, SessionKey};
use argonauts_core::runs::event::TodoStatus;
use argonauts_core::runs::events::task::Way;

use crate::base::format::{duration_slot, tokens};
use crate::bridge::tools::start_call;
use crate::cells::cell::ToolTitle;
use crate::council::work::plan::{Now, Progress, Step, TaskWork, way_word};
use crate::council::work::task_header::TaskHeader;
use crate::council::{Item, Piece};
use crate::frame::state::{RunState, SessionState, SessionStatus, WorkflowState};

/// The centred line that says a seat set out on its task's way, as "▐▌ 3.8 Flash started 3
/// agents" reads. None for a task in its own session, whose box says so.
pub fn started(seat: &SeatId, name: &str, way: &Way) -> Option<Item> {
    let what = match way {
        Way::OwnSession => return None,
        Way::Workflow { .. } | Way::CliWorkflow => {
            format!(" {name} started a workflow: {}", way_word(way))
        }
        Way::Agents { count } => format!(" {name} started {count} agents"),
    };
    Some(Item::Line(vec![
        Piece::Badge(seat.clone()),
        Piece::dim(what),
    ]))
}

/// Who sits where: each seat's own sessions, and the model each seat runs.
pub struct Seats<'a> {
    pub sessions: &'a HashMap<SessionKey, SeatId>,
    pub models: &'a [(SeatId, ModelRef)],
}

impl Seats<'_> {
    /// The seat a session belongs to, and whether the session is one of its own rather than a
    /// fresh one of its model.
    fn of(&self, session: &SessionState) -> Option<(SeatId, bool)> {
        if let Some(seat) = self.sessions.get(&session.key) {
            return Some((seat.clone(), true));
        }
        self.models
            .iter()
            .find(|(_, model)| session.is_worker(model))
            .map(|(seat, _)| (seat.clone(), false))
    }
}

/// The box of `task`, called `title` and done `way` by `seat`, as the run stands now.
pub fn drawn(
    run: &RunState,
    (task, title): (&TaskId, &str),
    way: &Way,
    seat: &SeatId,
    seats: &Seats<'_>,
) -> TaskWork {
    let sessions: Vec<&SessionState> = run
        .sessions_in_order()
        .filter(|session| session.task.as_ref() == Some(task))
        .collect();
    let (now, (done, total, unit)) = match way {
        Way::OwnSession | Way::CliWorkflow => (latest_call(&sessions), to_dos(&sessions)),
        Way::Agents { count } => agents(&sessions, seat, seats, *count),
        Way::Workflow { .. } => steps(run, &sessions, seats),
    };
    TaskWork {
        way: way_word(way),
        task: title.to_owned(),
        now,
        progress: Progress {
            done,
            total,
            unit: unit.to_owned(),
            time: duration_slot(elapsed(&sessions)),
            tokens: tokens(sessions.iter().map(|session| session.tokens_out).sum()),
        },
    }
}

/// The workflow a task's sessions work in.
fn workflow_of<'r>(run: &'r RunState, sessions: &[&SessionState]) -> Option<&'r WorkflowState> {
    sessions
        .iter()
        .find_map(|session| session.workflow_step.as_ref())
        .and_then(|step| run.workflows.iter().find(|w| w.id == step.workflow))
}

/// The top of the workflow view for `task`, called `title` and done `way` by `seat`: its
/// workflow, whose task it is in which council, and the way's own line. None until a workflow
/// does the task.
pub fn header(
    run: &RunState,
    (task, title): (&TaskId, &str),
    way: &Way,
    seat: &SeatId,
) -> Option<TaskHeader> {
    if !matches!(way, Way::Workflow { .. }) {
        return None;
    }
    let sessions: Vec<&SessionState> = run
        .sessions_in_order()
        .filter(|session| session.task.as_ref() == Some(task))
        .collect();
    let workflow = workflow_of(run, &sessions)?;
    Some(TaskHeader {
        workflow: workflow.id.clone(),
        way: way_word(way),
        task: title.to_owned(),
        seat: seat.clone(),
        council: run.title.clone(),
        about: workflow.description.clone(),
    })
}

/// The newest tool call of the task's sessions, as Claude Code writes its row.
fn latest_call(sessions: &[&SessionState]) -> Now {
    let call = sessions
        .iter()
        .rev()
        .find_map(|session| session.tools.last())
        .map(|tool| start_call("", &tool.name, &tool.input).title);
    match call {
        Some(ToolTitle::Call { name, args }) => Now::Call {
            tool: name,
            argument: args,
        },
        Some(ToolTitle::Text(text)) => Now::Words(vec![Piece::dim(text)]),
        _ => Now::Words(vec![Piece::dim("Starting…")]),
    }
}

/// The to-dos done of the newest list the task's sessions keep.
fn to_dos(sessions: &[&SessionState]) -> (u32, u32, &'static str) {
    let todos = sessions
        .iter()
        .rev()
        .map(|session| &session.todos)
        .find(|todos| !todos.is_empty());
    let Some(todos) = todos else {
        return (0, 0, "to-dos");
    };
    let done = todos
        .iter()
        .filter(|todo| todo.status == TodoStatus::Completed)
        .count();
    (count(done), count(todos.len()), "to-dos")
}

/// The agents of the seat's model, each on its part, with the words every part ends with
/// kept apart so a narrow screen can drop them.
fn agents(
    sessions: &[&SessionState],
    seat: &SeatId,
    seats: &Seats<'_>,
    planned: u8,
) -> (Now, (u32, u32, &'static str)) {
    let agents: Vec<&&SessionState> = sessions
        .iter()
        .filter(|session| seats.sessions.get(&session.key) != Some(seat))
        .collect();
    let labels: Vec<String> = agents
        .iter()
        .map(|session| {
            session
                .label
                .clone()
                .unwrap_or_else(|| session.role.as_str().to_owned())
        })
        .collect();
    let suffix = shared_last_word(&labels);
    let parts = agents
        .iter()
        .zip(&labels)
        .map(|(session, label)| {
            let part = label.strip_suffix(suffix.as_str()).unwrap_or(label);
            (finished(session), part.to_owned())
        })
        .collect::<Vec<_>>();
    let done = parts.iter().filter(|(done, _)| *done).count();
    let total = u32::from(planned).max(count(parts.len()));
    let now = Now::Agents {
        seat: seat.clone(),
        parts,
        suffix,
    };
    (now, (count(done), total, "agents"))
}

/// " routes" when every label ends with that word, and there are more words before it.
fn shared_last_word(labels: &[String]) -> String {
    let last = |label: &str| label.rsplit_once(' ').map(|(_, last)| last.to_owned());
    let Some(first) = labels.first().and_then(|label| last(label)) else {
        return String::new();
    };
    if labels
        .iter()
        .all(|label| last(label).as_deref() == Some(&first))
    {
        format!(" {first}")
    } else {
        String::new()
    }
}

/// The steps of the workflow the task's sessions work in, each by the seat whose model takes
/// it, done once every session on it finished.
fn steps(
    run: &RunState,
    sessions: &[&SessionState],
    seats: &Seats<'_>,
) -> (Now, (u32, u32, &'static str)) {
    let Some(workflow) = workflow_of(run, sessions) else {
        return (Now::Words(vec![Piece::dim("Starting…")]), (0, 0, "steps"));
    };
    let steps: Vec<Step> = workflow
        .phases
        .iter()
        .enumerate()
        .filter_map(|(index, name)| {
            let on_it: Vec<&&SessionState> = sessions
                .iter()
                .filter(|session| {
                    session.workflow_step.as_ref().is_some_and(|step| {
                        step.workflow == workflow.id && phase_index(step.phase) == Some(index)
                    })
                })
                .collect();
            let (by, own) = on_it.first().and_then(|session| seats.of(session))?;
            Some(Step {
                done: on_it.iter().all(|session| finished(session)),
                name: name.clone(),
                by,
                note: if own {
                    String::new()
                } else {
                    ", new session".to_owned()
                },
            })
        })
        .collect();
    let done = steps.iter().filter(|step| step.done).count();
    let total = count(workflow.phases.len());
    let now = Now::Steps {
        steps,
        ending: None,
    };
    (now, (count(done), total, "steps"))
}

fn phase_index(phase: u32) -> Option<usize> {
    usize::try_from(phase).ok()
}

fn finished(session: &SessionState) -> bool {
    session.status == SessionStatus::Done
}

/// From the first of the sessions starting to the last one's end, or its latest event while it
/// works.
fn elapsed(sessions: &[&SessionState]) -> Duration {
    let start = sessions.iter().map(|session| session.started).min();
    let end = sessions
        .iter()
        .map(|session| session.ended.unwrap_or(session.last_event))
        .max();
    match (start, end) {
        (Some(start), Some(end)) => positive(end - start),
        _ => Duration::ZERO,
    }
}

fn positive(span: time::Duration) -> Duration {
    Duration::try_from(span.max(time::Duration::ZERO)).unwrap_or_default()
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use argonauts_core::RunId;
    use argonauts_core::base::names::{RoleName, WorkflowId};
    use argonauts_core::workflows::workflow::{StepRef, WorkflowRef};
    use time::macros::datetime;

    use super::*;

    fn opus() -> ModelRef {
        ModelRef::new("claude-code", Some("claude-opus-5-5".to_owned()))
    }

    fn luna() -> ModelRef {
        ModelRef::new("codex", Some("gpt-5.6-luna".to_owned()))
    }

    /// A session of `worker` on `task`, in `phase` of workflow 2, done or still at work.
    fn on_phase(n: u32, worker: ModelRef, task: &TaskId, phase: u32, done: bool) -> SessionState {
        let at = datetime!(2026-10-25 10:47 UTC);
        let mut session = SessionState::new(
            SessionKey::numbered(n),
            worker,
            "worker",
            RoleName::Worker,
            at,
        );
        session.task = Some(task.clone());
        session.workflow_step = Some(StepRef {
            workflow: WorkflowId::numbered(2),
            phase,
            path: vec![0],
        });
        session.status = if done {
            SessionStatus::Done
        } else {
            SessionStatus::Working
        };
        session.last_event = at + time::Duration::seconds(216);
        session.tokens_out = 201_000;
        session
    }

    /// A workflow's steps each name the seat whose model takes it, a fresh session of a seat's
    /// model saying so, and the count is of the steps done.
    #[test]
    fn a_workflows_box_names_who_takes_each_step() {
        let task = TaskId::new("task-001").expect("a task id");
        let mut run = RunState::new(RunId::new("run-rate-limit").expect("a run id"));
        run.title = "Rate limit the API".to_owned();
        run.workflows.push(WorkflowState {
            id: WorkflowId::numbered(2),
            name: "maker-checker".to_owned(),
            from: None,
            description: "A worker builds, a reviewer sends it back until it holds".to_owned(),
            phases: vec!["Build".to_owned(), "Review".to_owned()],
            parent: None,
            steps: Vec::new(),
        });
        for session in [
            on_phase(1, opus(), &task, 0, true),
            on_phase(2, luna(), &task, 1, false),
        ] {
            run.session_order.push(session.key.clone());
            run.sessions.insert(session.key.clone(), session);
        }
        let own = HashMap::from([(SessionKey::numbered(1), SeatId::numbered(1))]);
        let models = [(SeatId::numbered(1), opus()), (SeatId::numbered(2), luna())];
        let seats = Seats {
            sessions: &own,
            models: &models,
        };
        let way = Way::Workflow {
            name: WorkflowRef::new("maker-checker"),
        };
        let drawn = drawn(
            &run,
            (&task, "Redis bucket"),
            &way,
            &SeatId::numbered(1),
            &seats,
        );
        assert_eq!(drawn.way, "Maker-checker");
        assert_eq!(drawn.task, "Redis bucket");
        let Now::Steps { steps, .. } = &drawn.now else {
            panic!("steps: {:?}", drawn.now);
        };
        let named: Vec<(bool, &str, &SeatId, &str)> = steps
            .iter()
            .map(|step| (step.done, step.name.as_str(), &step.by, step.note.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                (true, "Build", &SeatId::numbered(1), ""),
                (false, "Review", &SeatId::numbered(2), ", new session"),
            ]
        );
        let progress = &drawn.progress;
        assert_eq!(
            (progress.done, progress.total, progress.unit.as_str()),
            (1, 2, "steps")
        );
        assert_eq!(
            (progress.time.as_str(), progress.tokens.as_str()),
            ("3m36s", "402k")
        );

        let header = header(&run, (&task, "Redis bucket"), &way, &SeatId::numbered(1))
            .expect("a workflow does the task");
        assert_eq!(header.workflow, WorkflowId::numbered(2));
        assert_eq!(
            (header.way.as_str(), header.council.as_str()),
            ("Maker-checker", "Rate limit the API")
        );
        assert_eq!(
            header.about,
            "A worker builds, a reviewer sends it back until it holds"
        );
        let own = Way::OwnSession;
        assert_eq!(
            super::header(&run, (&task, "Redis bucket"), &own, &SeatId::numbered(1)),
            None
        );
    }

    #[test]
    fn parts_keep_the_word_they_share_apart() {
        let labels = ["Orders routes", "Users routes"].map(str::to_owned);
        assert_eq!(shared_last_word(&labels), " routes");
        let labels = ["Orders routes", "Users"].map(str::to_owned);
        assert_eq!(shared_last_word(&labels), "");
    }
}
