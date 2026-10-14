//! One question about a CLI that went down while `fallback.same_cli` keeps its tasks on it.
//! The tasks already wait, as the setting says, and the CLI is tried again by itself, so the
//! question holds nothing up. It lets you move the tasks instead, and it goes away once the
//! CLI is back.
//!
//! A task here is whatever an agent of the CLI was doing: a build task, or another job such
//! as a review, a judge's ruling, or a step of the main agent's own. A build task is handed
//! to its new worker. Another job is let off its CLI, and its role's fallbacks take it when
//! the workflow reaches it again.
//!
//! A run that only waits has no engine, so the engine that hears your answer is rarely the
//! one that asked. What was asked and answered is read from the run's log each time, and an
//! answer is applied to the tasks that still wait.

use std::collections::BTreeSet;

use argonauts_core::TaskId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::agent_status::{FallbackEvent, FallbackPlan, JobRef, JobWait};
use argonauts_core::events::approval::{
    AgentDownPacket, AnswerAction, ApprovalDecided, ApprovalKind, ApprovalOpened, ApprovalPacket,
    FreeText, PausedTask,
};
use argonauts_core::events::shared::Recipient;
use argonauts_core::events::task::TaskState;
use argonauts_core::limits::HealthState;
use argonauts_core::names::{AgentId, ApprovalId, ModelRef, RoleName};
use argonauts_core::policy::EvidenceFacts;
use argonauts_core::settings::fallback::{AppliedFallback, FallbackMode};
use argonauts_core::{Note, NoteLevel};
use time::OffsetDateTime;

use crate::approvals::{self, ApprovalRequest, Opening, autopilot};
use crate::blackboard::commands;
use crate::engine::host::Host;
use crate::engine::job_fallback::Followed;
use crate::engine::shared::lock;
use crate::error::{EngineError, EngineResult};
use crate::fallback::{ask, assign, broken, choose, rules};
use crate::state::RunState;
use crate::state::approvals::ApprovalRecord;
use crate::state::tasks::TaskEntry;
use crate::task::support::task_events;

/// The setting the question names as what keeps the tasks where they are.
const SAME_CLI: &str = "same_cli";

/// What waits for a CLI that a setting keeps it on.
enum Paused {
    Task(Box<TaskEntry>),
    /// Another job than a build task, and the id the question lists it under.
    Job(Box<JobWait>, TaskId),
}

impl Paused {
    /// As the question lists it.
    fn listed(&self) -> PausedTask {
        match self {
            Self::Task(record) => PausedTask {
                task: record.spec.id.clone(),
                title: record.spec.title.clone(),
                role: RoleName::Worker,
            },
            Self::Job(wait, listed_as) => PausedTask {
                task: listed_as.clone(),
                title: wait.title.clone(),
                role: wait.job.role,
            },
        }
    }

    fn key(&self) -> Key {
        let listed = self.listed();
        (listed.task, listed.role)
    }

    /// What the question's evidence says of it: the tasks are what you decide about, so a
    /// question that lists others is another one.
    fn evidence(&self) -> String {
        let (id, role) = self.key();
        match self {
            Self::Task(_) => format!("paused {id}"),
            Self::Job(..) => format!("paused {id} as {}", role.as_str()),
        }
    }

    /// Who could take it on another CLI without asking you first, best first.
    fn elsewhere(&self, host: &Host) -> Vec<ModelRef> {
        match self {
            Self::Task(record) => elsewhere(host, record),
            Self::Job(wait, _) => {
                let takes_work = |model: &&ModelRef| rules::takes_work(host, model);
                wait.elsewhere.iter().filter(takes_work).cloned().collect()
            }
        }
    }
}

/// What tells one listed task from another: a task's review waits under the task's id too.
type Key = (TaskId, RoleName);

/// The id a job is listed under: its task's, or one made from where its step sits, since a
/// step of the main agent's own has no task.
fn listed_as(job: &JobRef) -> Option<TaskId> {
    if let Some(task) = &job.task {
        return Some(task.clone());
    }
    let place = match &job.step {
        Some(step) => {
            let path: Vec<String> = step
                .path
                .iter()
                .map(|index| (index + 1).to_string())
                .collect();
            format!("phase-{}-step-{}", step.phase + 1, path.join("."))
        }
        None => job.role.as_str().to_owned(),
    };
    TaskId::new(place).ok()
}

/// What comes next for the question about one CLI.
enum Next {
    /// Nothing waits for it, it isn't down, or you already said to wait this time.
    Nothing,
    Asked(Opening),
    Answered(Box<ApprovalDecided>, Box<AgentDownPacket>),
}

/// Asks about `agent_id` when tasks wait for it because they must stay on it, or acts on the
/// answer the log already holds, without holding up the caller. The question is in the log
/// before this returns, so a run that goes on to wait shows it.
pub fn ask_in_background(host: &Host, agent_id: &AgentId) -> EngineResult<()> {
    let followed = Followed::Cli(agent_id.clone());
    if !lock(&host.shared().job_fallbacks).follow(followed.clone()) {
        // Whoever follows the question finds the one this leaves open: the same one, or a new
        // one about every task that waits now, with the old one withdrawn under them.
        return next(host, agent_id).map(drop);
    }
    let next = match next(host, agent_id) {
        Ok(Next::Nothing) => {
            lock(&host.shared().job_fallbacks).unfollow(&followed);
            return Ok(());
        }
        Err(err) => {
            lock(&host.shared().job_fallbacks).unfollow(&followed);
            return Err(err);
        }
        Ok(next) => next,
    };
    let newly_asked = matches!(next, Next::Asked(_));
    let (host, agent_id) = (host.clone(), agent_id.clone());
    tokio::spawn(async move {
        let ended = tokio::select! {
            () = host.interrupted().cancelled() => None,
            result = follow(&host, &agent_id, next) => Some(result),
        };
        lock(&host.shared().job_fallbacks).unfollow(&followed);
        let Some(result) = ended else {
            return;
        };
        let settled = newly_asked && result.is_ok();
        let cli = DisplayNames::default().cli(&agent_id.0);
        let what = format!("The question about {cli} being down");
        report(&host, None, result, &what);
        // Another CLI may have waited its turn while this question was open. Only a question
        // that was asked here hands the turn on, so two answered ones never pass it back
        // and forth.
        if settled {
            report(
                &host,
                None,
                ask_again(&host),
                "Asking about a CLI that is down",
            );
        }
    });
    Ok(())
}

/// A run that woke asks again about every CLI its tasks still wait for.
pub fn ask_again(host: &Host) -> EngineResult<()> {
    let waited_for: BTreeSet<AgentId> = host.read(|state| {
        let tasks = state
            .tasks
            .by_id
            .values()
            .filter(|record| record.state == TaskState::NeedsDecision)
            .filter_map(|record| state.tasks.progress(&record.spec.id).waiting_for.clone());
        let jobs = state.run.waiting_jobs.iter();
        tasks
            .chain(jobs.map(|wait| wait.stopped.agent_id()))
            .collect()
    });
    for agent_id in waited_for {
        ask_in_background(host, &agent_id)?;
    }
    Ok(())
}

/// A job that isn't a build task stopped the run, and only `same_cli` keeps it from an agent
/// of another CLI. The log says it waits, so the question about its CLI lists it too.
pub fn job_waits(host: &Host, wait: &JobWait) -> EngineResult<()> {
    if !host.read(|state| state.run.waiting_jobs.contains(wait)) {
        let waiting = FallbackEvent::JobWaiting(wait.clone());
        host.record(None, None, RunEventKind::Fallback(waiting))?;
    }
    ask_in_background(host, &wait.stopped.agent_id())
}

/// Your answer to the question about a CLI lets the jobs it listed leave at once. The
/// workflow that stopped on one may be on its way back to it already, and must find it free.
/// A note typed with the answer goes to the paused tasks' workers.
pub fn on_decided(host: &Host, opened: &ApprovalOpened, decided: &ApprovalDecided) {
    let ApprovalPacket::AgentDown(packet) = &opened.packet else {
        return;
    };
    if packet.kept_by.is_some() {
        let freed = free_jobs(host, packet, decided);
        report(host, None, freed, "Letting a job leave its CLI");
    }
    let passed = pass_note_on(host, packet, decided);
    report(host, None, passed, "Passing your note to the paused tasks");
}

/// Your note is for whoever works each paused task next, whatever you answered: the agent it
/// waits for, or the one it goes to. Each gets it once, with the task's next prompt. This
/// runs as the answer is recorded, so an answer read back from the log passes nothing on
/// again.
fn pass_note_on(
    host: &Host,
    packet: &AgentDownPacket,
    decided: &ApprovalDecided,
) -> EngineResult<()> {
    let note = decided.text.as_deref().map(str::trim).unwrap_or_default();
    if note.is_empty() {
        return Ok(());
    }
    // A review or a step that waits is listed too, and has no worker of a task to tell.
    let builds = packet
        .paused
        .iter()
        .filter(|paused| paused.role == RoleName::Worker)
        .filter(|paused| host.read(|state| state.tasks.get(&paused.task).is_some()));
    for paused in builds {
        let task = paused.task.clone();
        commands::mention_to(host, Recipient::Task { task }, note.to_owned())?;
    }
    Ok(())
}

/// Lets each job the question listed that still waits leave its CLI, as you answered: to the
/// agent that fits every paused task where it may take the job, else to whoever its role's
/// fallbacks come to. Waiting leaves them where they are.
fn free_jobs(host: &Host, packet: &AgentDownPacket, decided: &ApprovalDecided) -> EngineResult<()> {
    let one = match (&decided.action, &packet.fits_all) {
        (AnswerAction::OneForAll, one) => one.as_ref(),
        (AnswerAction::PerTask, _) => None,
        _ => return Ok(()),
    };
    let listed = listed(packet);
    for paused in paused(host, &packet.agent_id) {
        let Paused::Job(wait, _) = &paused else {
            continue;
        };
        if !listed.contains(&paused.key()) {
            continue;
        }
        let freed = FallbackEvent::JobFreed {
            job: wait.job.clone(),
            to: one.filter(|one| wait.elsewhere.contains(one)).cloned(),
            by: decided.by.clone(),
        };
        host.record(None, None, RunEventKind::Fallback(freed))?;
    }
    Ok(())
}

/// `agent_id` takes work again, so its open question has nothing left to ask.
pub fn withdraw(host: &Host, agent_id: &AgentId) -> EngineResult<()> {
    let open = host.read(|state| {
        latest(state, agent_id)
            .filter(|record| record.is_open())
            .map(|record| record.opened.id.clone())
    });
    match open {
        Some(id) => {
            let cli = DisplayNames::default().cli(&agent_id.0);
            approvals::withdraw(host, &id, &format!("{cli} is tried again"))
        }
        None => Ok(()),
    }
}

/// Waits on the question's answer and applies it. A question withdrawn under it is asked
/// again when tasks still wait: another one joined them.
async fn follow(host: &Host, agent_id: &AgentId, mut next: Next) -> EngineResult<()> {
    loop {
        let opening = match next {
            Next::Nothing => return Ok(()),
            Next::Answered(decided, packet) => {
                return apply(host, agent_id, &packet, &decided).await;
            }
            Next::Asked(opening) => opening,
        };
        let id = opening.id().clone();
        next = match opening.decided_or_withdrawn(host).await? {
            Ok(decided) => match host.read(|state| packet_of(state, &id)) {
                Some(packet) => Next::Answered(Box::new(decided), Box::new(packet)),
                None => Next::Nothing,
            },
            Err(_withdrawn) => self::next(host, agent_id)?,
        };
    }
}

/// Reads where the question about `agent_id` stands and opens it when it should be open.
fn next(host: &Host, agent_id: &AgentId) -> EngineResult<Next> {
    let paused = paused(host, agent_id);
    let health = host.services().health.state(agent_id);
    let asks = !paused.is_empty() && health.state == HealthState::Down;
    let latest = host.read(|state| {
        latest(state, agent_id).map(|record| (record.opened.id.clone(), record.decided.clone()))
    });
    match latest {
        Some((id, None)) if !asks => {
            approvals::withdraw(host, &id, "no task waits for it any more")?;
            return Ok(Next::Nothing);
        }
        // Still open: it is found again when the same tasks wait, and replaced otherwise.
        Some((_, None)) | None => {}
        Some((id, Some(decided))) => {
            let Some(packet) = host.read(|state| packet_of(state, &id)) else {
                return Ok(Next::Nothing);
            };
            let waiting = keys(&paused);
            let listed = listed(&packet);
            if decided.action == AnswerAction::Wait {
                // You said to wait for this try of the CLI. A later try, or a task that
                // joined since, is asked about anew.
                let this_try = seconds(packet.test_at) == seconds(health.next_test_at);
                if waiting.is_subset(&listed) && this_try {
                    return Ok(Next::Nothing);
                }
            } else if !waiting.is_disjoint(&listed) {
                // The jobs go free before anyone enters the workflow that stopped on them.
                free_jobs(host, &packet, &decided)?;
                return Ok(Next::Answered(Box::new(decided), Box::new(packet)));
            }
        }
    }
    if !asks || autopilot::applies_now(host, ApprovalKind::AgentDown) {
        return Ok(Next::Nothing);
    }
    // One question about a CLI is open at a time, and a second would replace it. This CLI's
    // tasks wait and are tried again all the same, and are asked about when the run next wakes.
    if host.read(|state| another_is_asked_about(state, agent_id)) {
        return Ok(Next::Nothing);
    }
    approvals::open(host, request(host, agent_id, &paused)).map(Next::Asked)
}

/// Whether the open question of this kind is about another CLI than `agent_id`.
fn another_is_asked_about(state: &RunState, agent_id: &AgentId) -> bool {
    state
        .approvals
        .open_for(ApprovalKind::AgentDown, None)
        .is_some_and(|record| {
            !matches!(
                &record.opened.packet,
                ApprovalPacket::AgentDown(down) if down.agent_id == *agent_id
            )
        })
}

/// Does what you answered for each task the question listed that still waits.
async fn apply(
    host: &Host,
    agent_id: &AgentId,
    packet: &AgentDownPacket,
    decided: &ApprovalDecided,
) -> EngineResult<()> {
    free_jobs(host, packet, decided)?;
    let listed = listed(packet);
    let waiting = paused(host, agent_id);
    let still = waiting.iter().filter_map(|paused| match paused {
        Paused::Task(record) if listed.contains(&paused.key()) => Some(&record.spec.id),
        _ => None,
    });
    match (&decided.action, &packet.fits_all) {
        (AnswerAction::Wait, _) => Ok(()),
        (AnswerAction::OneForAll, Some(to)) => {
            for task in still {
                let record = rules::record(host, task)?;
                let Some(stopped) = rules::worker(&record) else {
                    continue;
                };
                let note = host
                    .services()
                    .fallback
                    .note(&record.spec, &task_events(host, task)?);
                let tag = rules::applied_for(host, &record.spec, &stopped).checkpoint;
                assign::fallback(host, task, to, &note, tag, decided.by.clone()).await?;
            }
            Ok(())
        }
        // Nobody fits them all, so each is asked about by itself.
        (AnswerAction::OneForAll | AnswerAction::PerTask, _) => {
            for task in still {
                ask_task_in_background(host, task);
            }
            Ok(())
        }
        (other, _) => Err(EngineError::Refused(format!(
            "the agent-down question doesn't offer {other:?}"
        ))),
    }
}

/// Opens one task's own fallback question, with agents from every installed CLI, without
/// holding up the caller.
fn ask_task_in_background(host: &Host, task: &TaskId) {
    let followed = Followed::Task(task.clone());
    if !lock(&host.shared().job_fallbacks).follow(followed.clone()) {
        return;
    }
    let (host, task) = (host.clone(), task.clone());
    tokio::spawn(async move {
        let result = tokio::select! {
            () = host.interrupted().cancelled() => Ok(()),
            result = ask_task(&host, &task) => result,
        };
        lock(&host.shared().job_fallbacks).unfollow(&followed);
        report(
            &host,
            Some(&task),
            result,
            &format!("The fallback question for {task}"),
        );
    });
}

async fn ask_task(host: &Host, task: &TaskId) -> EngineResult<()> {
    let record = rules::record(host, task)?;
    let Some(stopped) = rules::worker(&record) else {
        return Ok(());
    };
    let rules = rules::applied_for(host, &record.spec, &stopped);
    let note = host
        .services()
        .fallback
        .note(&record.spec, &task_events(host, task)?);
    let query = choose::query(host, &record, &stopped, true);
    let plan = FallbackPlan {
        task: task.clone(),
        title: record.spec.title.clone(),
        reason: note.why_stopped.clone(),
        resets_at: host
            .services()
            .health
            .state(&stopped.agent_id())
            .next_test_at,
        work: note.work.stat,
        checks_ran: broken::verdict(host, task)?.is_some(),
        candidates: host.services().fallback.candidates(&query),
        mode: rules.mode,
        strategy: rules.strategy,
        race_count: rules.together,
        also_moving: Vec::new(),
        stopped,
    };
    ask::per_task(host, task, plan).await
}

/// A question that failed says so in the run's log. One cut short by a pause or a stop is
/// asked again by the engine that goes on.
fn report(host: &Host, task: Option<&TaskId>, result: EngineResult<()>, what: &str) {
    let err = match result {
        Ok(()) | Err(EngineError::Interrupted) => return,
        Err(err) => err,
    };
    let note = RunEventKind::Note(Note {
        level: NoteLevel::Error,
        text: format!("{what} failed: {err}"),
    });
    if let Err(err) = host.record(task, None, note) {
        tracing::error!(%err, what, "could not record a failed question");
    }
}

/// The question for `paused`, every task that waits for `agent_id`.
fn request(host: &Host, agent_id: &AgentId, paused: &[Paused]) -> ApprovalRequest {
    let services = host.services();
    let health = services.health.state(agent_id);
    let fits_all = fits_all(host, paused);
    let count = match paused.len() {
        1 => "1 task".to_owned(),
        many => format!("{many} tasks"),
    };
    let packet = AgentDownPacket {
        agent_id: agent_id.clone(),
        evidence: ask::models_out(&services.health.models(agent_id))
            .or(health.reason)
            .unwrap_or_else(|| "it stopped answering".to_owned()),
        test_at: health.next_test_at,
        paused: paused.iter().map(Paused::listed).collect(),
        fits_all: fits_all.clone(),
        kept_by: Some(SAME_CLI.to_owned()),
    };
    ApprovalRequest {
        at: host.read(rules::current_step),
        kind: ApprovalKind::AgentDown,
        title: format!(
            "{} is down · {count} paused",
            DisplayNames::default().cli(&agent_id.0)
        ),
        task: None,
        packet: ApprovalPacket::AgentDown(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: paused.iter().map(Paused::evidence).collect(),
        documents: Vec::new(),
        options: Some(ask::down_options(agent_id, fits_all.is_some())),
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    }
}

/// The build tasks and the other jobs that wait for `agent_id` because their fallbacks must
/// stay on it, the build tasks first. A role told to wait or to ask has its own answer already.
fn paused(host: &Host, agent_id: &AgentId) -> Vec<Paused> {
    let (tasks, jobs): (Vec<TaskEntry>, Vec<JobWait>) = host.read(|state| {
        let tasks = state
            .tasks
            .by_id
            .values()
            .filter(|record| record.state == TaskState::NeedsDecision)
            .filter(|record| {
                state.tasks.progress(&record.spec.id).waiting_for.as_ref() == Some(agent_id)
            });
        let jobs = state.run.waiting_jobs.iter();
        let jobs = jobs.filter(|wait| wait.stopped.agent_id() == *agent_id);
        (tasks.cloned().collect(), jobs.cloned().collect())
    });
    let kept = |rules: AppliedFallback| rules.same_cli && rules.mode == FallbackMode::Auto;
    let tasks = tasks.into_iter().filter(|record| {
        rules::worker(record)
            .is_some_and(|worker| kept(rules::applied_for(host, &record.spec, &worker)))
    });
    let jobs = jobs
        .into_iter()
        .filter(|wait| kept(rules::applied(host, wait.job.role, agent_id)))
        .filter_map(|wait| listed_as(&wait.job).map(|id| Paused::Job(Box::new(wait), id)));
    tasks
        .map(|record| Paused::Task(Box::new(record)))
        .chain(jobs)
        .collect()
}

/// The first agent that may take every one of `paused` without asking you first, in the
/// order the first task's fallbacks come.
fn fits_all(host: &Host, paused: &[Paused]) -> Option<ModelRef> {
    let mut lists = paused.iter().map(|paused| paused.elsewhere(host));
    let first = lists.next()?;
    let rest: Vec<Vec<ModelRef>> = lists.collect();
    first
        .into_iter()
        .find(|worker| rest.iter().all(|list| list.contains(worker)))
}

/// Who could take the task on another CLI, best first.
pub(super) fn elsewhere(host: &Host, record: &TaskEntry) -> Vec<ModelRef> {
    let Some(stopped) = rules::worker(record) else {
        return Vec::new();
    };
    let query = choose::query(host, record, &stopped, true);
    host.services()
        .fallback
        .candidates(&query)
        .into_iter()
        .filter(|candidate| {
            !candidate.needs_confirm && candidate.worker.agent_id() != stopped.agent_id()
        })
        .map(|candidate| candidate.worker)
        .collect()
}

/// The newest question about `agent_id` that a setting keeps waiting. A withdrawn one was
/// never answered and is left out.
fn latest<'s>(state: &'s RunState, agent_id: &AgentId) -> Option<&'s ApprovalRecord> {
    state
        .approvals
        .by_id
        .values()
        .filter(|record| record.withdrawn.is_none())
        .filter(|record| {
            matches!(
                &record.opened.packet,
                ApprovalPacket::AgentDown(down)
                    if down.agent_id == *agent_id && down.kept_by.is_some()
            )
        })
        .max_by_key(|record| record.since)
}

fn packet_of(state: &RunState, id: &ApprovalId) -> Option<AgentDownPacket> {
    match &state.approvals.by_id.get(id)?.opened.packet {
        ApprovalPacket::AgentDown(down) => Some(down.clone()),
        _ => None,
    }
}

fn keys(paused: &[Paused]) -> BTreeSet<Key> {
    paused.iter().map(Paused::key).collect()
}

fn listed(packet: &AgentDownPacket) -> BTreeSet<Key> {
    packet
        .paused
        .iter()
        .map(|paused| (paused.task.clone(), paused.role))
        .collect()
}

/// A time as the log keeps it: to the second.
fn seconds(at: Option<OffsetDateTime>) -> Option<i64> {
    at.map(OffsetDateTime::unix_timestamp)
}
