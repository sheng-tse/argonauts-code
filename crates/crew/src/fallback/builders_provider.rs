//! The question a task's review or ruling asks when every agent left to take it is of its
//! builder's own provider, which a fallback never comes from by itself. You choose: wait for
//! the agent it was asked of, let the builder's provider take it with the merge then left to
//! you, or pick an agent yourself.
//!
//! As with the question about a CLI that is down, the engine that hears your answer is rarely
//! the one that asked, so what was asked and answered is read from the run's log each time.

use argonauts_core::TaskId;
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::agent_status::{FallbackEvent, JobKeeper, JobWait};
use argonauts_core::runs::events::approval::{
    AgentDownPacket, AnswerAction, AnswerOption, ApprovalDecided, ApprovalKind, ApprovalOpened,
    ApprovalPacket, FreeText, PausedTask,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::safety::policy::EvidenceFacts;
use time::OffsetDateTime;

use crate::approvals::{self, ApprovalRequest, Opening};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::engine::job_fallback::{verb_of, work_of};
use crate::fallback::down::{listed_as, report};
use crate::fallback::rules;
use crate::roles::display_name;
use crate::state::RunState;

/// What an option that gives the job to the builder's provider says of it.
const MERGE_IS_YOURS: &str = "the merge is left to you";

/// Where the question lists the job: under its task's id, as the reviewer or judge it is.
#[derive(Debug, Clone, PartialEq)]
struct Listed {
    task: TaskId,
    role: RoleName,
}

impl Listed {
    fn of(wait: &JobWait) -> Option<Self> {
        Some(Self {
            task: listed_as(&wait.job)?,
            role: wait.job.role,
        })
    }

    fn lists(&self, packet: &AgentDownPacket) -> bool {
        packet.kept_by.is_none()
            && matches!(packet.paused.as_slice(), [paused] if paused.task == self.task && paused.role == self.role)
    }
}

/// Asks about `wait`, or acts on what the log says you answered. A wait you chose holds while
/// the same agent is offered and its CLI is on the same try.
pub fn ask(host: &Host, wait: &JobWait) -> EngineResult<()> {
    let Some(listed) = Listed::of(wait) else {
        return Ok(());
    };
    let test_at = host
        .services()
        .health
        .state(&wait.stopped.agent_id())
        .next_test_at;
    let latest = host.read(|state| latest(state, &listed));
    let mut picking = false;
    if let Some((opened, decided)) = latest {
        match decided.map(|decided| decided.action) {
            // Still open, or answered with an agent: the pick you asked for stays one. Asking
            // the first question again would find it answered with a pick, and come back here
            // without end.
            None | Some(AnswerAction::Candidate { .. }) => picking = !offers_a_pick(&opened),
            Some(AnswerAction::Wait) if still_waits(&opened, wait, test_at) => return Ok(()),
            Some(AnswerAction::OtherCandidates) => picking = true,
            _ => {}
        }
    }
    let request = request(host, wait, &listed, picking, test_at);
    match approvals::open(host, request)? {
        // The same question was answered earlier in this round.
        Opening::Decided(decided) => apply(host, wait, &decided),
        Opening::Waiting { .. } => Ok(()),
    }
}

/// Your answer to the question, as it is recorded: the agent you chose may take the job at
/// once, and a pick lists every agent next. The answer's lock is held here, so that list
/// opens once it is let go.
pub fn on_decided(host: &Host, packet: &AgentDownPacket, decided: &ApprovalDecided) {
    let Some(wait) = host.read(|state| waiting(state, packet)) else {
        return;
    };
    let task = wait.job.task.clone();
    match &decided.action {
        AnswerAction::Candidate { worker } => {
            let freed = free(host, &wait, worker, decided.by.clone());
            report(
                host,
                task.as_ref(),
                freed,
                "Letting the agent you chose take a job",
            );
        }
        AnswerAction::OtherCandidates => {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let host = host.clone();
            runtime.spawn(async move {
                let asked = ask(&host, &wait);
                report(&host, task.as_ref(), asked, "Listing every agent for a job");
            });
        }
        _ => {}
    }
}

/// What an answer the log already held means for the job.
fn apply(host: &Host, wait: &JobWait, decided: &ApprovalDecided) -> EngineResult<()> {
    match &decided.action {
        AnswerAction::Candidate { worker } => free(host, wait, worker, decided.by.clone()),
        AnswerAction::OtherCandidates => ask(host, wait),
        _ => Ok(()),
    }
}

/// Lets `worker` take the job, whoever built the work, where it is still one of those who
/// could.
fn free(host: &Host, wait: &JobWait, worker: &ModelRef, by: Party) -> EngineResult<()> {
    if !wait.elsewhere.contains(worker) {
        return Ok(());
    }
    let freed = FallbackEvent::JobFreed {
        job: wait.job.clone(),
        to: Some(worker.clone()),
        by,
    };
    host.record(wait.job.task.as_ref(), None, RunEventKind::Fallback(freed))?;
    Ok(())
}

/// The job the question lists, while it still waits.
fn waiting(state: &RunState, packet: &AgentDownPacket) -> Option<JobWait> {
    state
        .run
        .waiting_jobs
        .iter()
        .filter(|wait| matches!(wait.kept_by, JobKeeper::BuildersProvider { .. }))
        .find(|wait| Listed::of(wait).is_some_and(|listed| listed.lists(packet)))
        .cloned()
}

/// The newest question about the job that wasn't withdrawn, and your answer to it.
fn latest(state: &RunState, listed: &Listed) -> Option<(ApprovalOpened, Option<ApprovalDecided>)> {
    state
        .approvals
        .by_id
        .values()
        .filter(|record| record.withdrawn.is_none())
        .filter(|record| match &record.opened.packet {
            ApprovalPacket::AgentDown(down) => listed.lists(down),
            _ => false,
        })
        .max_by_key(|record| record.since)
        .map(|record| (record.opened.clone(), record.decided.clone()))
}

fn offers_a_pick(opened: &ApprovalOpened) -> bool {
    opened
        .options
        .iter()
        .any(|option| option.action == AnswerAction::OtherCandidates)
}

/// Whether the wait you chose still holds: the agent it would go to is the same, and so is
/// the try of its CLI you waited for.
fn still_waits(opened: &ApprovalOpened, wait: &JobWait, test_at: Option<OffsetDateTime>) -> bool {
    let ApprovalPacket::AgentDown(down) = &opened.packet else {
        return false;
    };
    let seconds = |at: Option<OffsetDateTime>| at.map(OffsetDateTime::unix_timestamp);
    down.fits_all.as_ref() == wait.elsewhere.first() && seconds(down.test_at) == seconds(test_at)
}

/// The question: who stopped, that only the builder's provider is left, and the choices. With
/// `picking`, every agent that may take the job instead, beside waiting.
fn request(
    host: &Host,
    wait: &JobWait,
    listed: &Listed,
    picking: bool,
    test_at: Option<OffsetDateTime>,
) -> ApprovalRequest {
    let cli = DisplayNames::default().cli(&wait.stopped.agent);
    let verb = verb_of(listed.role);
    let (builders, why) = match &wait.kept_by {
        JobKeeper::BuildersProvider { builders, why } => (builders.as_slice(), why.as_str()),
        JobKeeper::SameCli => (&[][..], ""),
    };
    let evidence = format!(
        "{why}, and nobody outside the builder's provider is left to {verb} {}",
        work_of(builders)
    );
    let providers: Vec<String> = builders
        .iter()
        .map(|builder| host.services().registry.provider(builder))
        .collect();
    let theirs = |model: &ModelRef| providers.contains(&host.services().registry.provider(model));
    let packet = AgentDownPacket {
        agent_id: wait.stopped.agent_id(),
        evidence: argonauts_core::base::text::sentence_case(&evidence),
        test_at,
        paused: vec![PausedTask {
            task: listed.task.clone(),
            title: wait.title.clone(),
            role: listed.role,
        }],
        fits_all: wait.elsewhere.first().cloned(),
        kept_by: None,
    };
    let mut options = vec![offer('w', format!("Wait for {cli}"), AnswerAction::Wait)];
    options[0].recommended = true;
    let detail = |model: &ModelRef| theirs(model).then(|| MERGE_IS_YOURS.to_owned());
    if picking {
        for (index, model) in wait.elsewhere.iter().enumerate() {
            let mut pick = offer(key(index), display_name(model), take(model));
            pick.detail = detail(model);
            options.push(pick);
        }
    } else if let Some(first) = wait.elsewhere.first() {
        let mut first_one = offer(
            'l',
            format!("Let {} {verb}", display_name(first)),
            take(first),
        );
        first_one.detail = detail(first);
        options.push(first_one);
        if wait.elsewhere.len() > 1 {
            let mut pick = offer(
                'p',
                "Pick an agent".to_owned(),
                AnswerAction::OtherCandidates,
            );
            pick.detail = Some("from every agent that may take it".to_owned());
            options.push(pick);
        }
    }
    let seconds = test_at.map_or_else(|| "none".to_owned(), |at| at.unix_timestamp().to_string());
    let offered: Vec<String> = wait.elsewhere.iter().map(ModelRef::to_string).collect();
    let mut agent_outputs = vec![
        format!("paused {} as {}", listed.task, listed.role.as_str()),
        format!("may take it: {}", offered.join(", ")),
        format!("next try of {cli}: {seconds}"),
    ];
    let title = if picking {
        agent_outputs.push("you pick an agent".to_owned());
        format!("Pick an agent for {}", wait.title.trim())
    } else {
        format!(
            "{} · {} waits for it",
            argonauts_core::base::text::sentence_case(why),
            wait.title.trim()
        )
    };
    ApprovalRequest {
        at: host.read(rules::current_step),
        kind: ApprovalKind::AgentDown,
        title,
        task: wait.job.task.clone(),
        packet: ApprovalPacket::AgentDown(packet),
        facts: EvidenceFacts::default(),
        agent_outputs,
        documents: Vec::new(),
        options: Some(options),
        free_text: FreeText::None,
        authors: Vec::new(),
    }
}

fn offer(key: char, label: String, action: AnswerAction) -> AnswerOption {
    AnswerOption {
        key,
        label,
        action,
        recommended: false,
        detail: None,
    }
}

fn take(model: &ModelRef) -> AnswerAction {
    AnswerAction::Candidate {
        worker: model.clone(),
    }
}

/// The key of the `index`th agent in the list: a, b, c, and so on, past the `w` of waiting.
fn key(index: usize) -> char {
    ('a'..='z')
        .filter(|letter| *letter != 'w')
        .nth(index)
        .unwrap_or('z')
}
