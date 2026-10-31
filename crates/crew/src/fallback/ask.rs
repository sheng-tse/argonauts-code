//! The fallback question: a task's agent stopped and you pick who continues, take it over, or wait.
//! When an outage has the agent marked down, the agent-down question asks first.

use time::OffsetDateTime;

use argonauts_core::TaskId;
use argonauts_core::base::names::{AgentId, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::models::limits::{HealthState, ModelHealth, ModelState, Trouble};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::agent_status::{FallbackEvent, FallbackPlan};
use argonauts_core::runs::events::approval::{
    AgentDownPacket, AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText, PausedTask,
};
use argonauts_core::runs::events::session::EndReason;
use argonauts_core::runs::events::task::TaskState;
use argonauts_core::safety::policy::EvidenceFacts;

use crate::approvals::options::{self, FallbackCause};
use crate::approvals::{self, ApprovalRequest, autopilot};
use crate::engine::error::{EngineError, EngineResult, Stall};
use crate::engine::host::Host;
use crate::engine::job_fallback;
use crate::fallback::{assign, choose, down, rules};
use crate::roles::display_name;
use crate::state::RunState;
use crate::task::machine::Trigger;
use crate::task::support::newest_worker_session;
use crate::task::{pipeline, take_over};

/// Opens the fallback question and acts on your answer. Autopilot never asks: with nobody to take
/// the task, the run stops until you carry it on.
pub async fn ask(host: &Host, task: &TaskId, plan: FallbackPlan) -> EngineResult<()> {
    if agent_down(host, task, &plan).await? == Asked::Settled {
        return Ok(());
    }
    if plan.candidates.is_empty() && autopilot::applies_now(host, ApprovalKind::Fallback) {
        return stop_the_run(host, task, &plan).await;
    }
    per_task(host, task, plan).await
}

/// The fallback question for one task, asked until an answer settles it.
pub(super) async fn per_task(host: &Host, task: &TaskId, plan: FallbackPlan) -> EngineResult<()> {
    let mut plan = plan;
    let mut seen = Vec::new();
    loop {
        let mut cause = host.read(|state| cause(state, task));
        // "I logged in, retry" only where the CLI's own check says it's logged out.
        if cause == FallbackCause::LoggedOut && !says_logged_out(host, task, &plan).await {
            cause = FallbackCause::Outage;
        }
        let request = ApprovalRequest {
            at: host.read(rules::current_step),
            kind: ApprovalKind::Fallback,
            // Titles reach the transcript's notices, so they name the agent and the task as
            // people read them, not by their ids.
            title: format!(
                "{} stopped · {}",
                DisplayNames::default().worker(&plan.stopped),
                plan.title.trim()
            ),
            task: Some(task.clone()),
            packet: ApprovalPacket::Fallback(plan.clone()),
            facts: EvidenceFacts::default(),
            // The candidates are part of what you decide on, so they're in the evidence.
            agent_outputs: plan
                .candidates
                .iter()
                .map(|candidate| format!("candidate {}", candidate.worker))
                .collect(),
            documents: Vec::new(),
            options: Some(options::fallback(&plan, cause)),
            free_text: FreeText::None,
            authors: Vec::new(),
        };
        let decided = approvals::open_and_wait(host, request).await?;
        if seen.contains(&decided.id) {
            return Err(EngineError::Refused(format!(
                "the fallback question for {task} could not open again"
            )));
        }
        seen.push(decided.id.clone());
        let record = rules::record(host, task)?;
        match decided.action {
            AnswerAction::Candidate { worker } => {
                let note = host.services().fallback.note(&record.spec, &[]);
                let tag = rules::applied_for(host, &record.spec, &plan.stopped).checkpoint;
                assign::fallback(host, task, &worker, &note, tag, decided.by).await?;
                return Ok(());
            }
            AnswerAction::OtherCandidates => {
                let query = choose::query(host, &record, &plan.stopped, true);
                plan.candidates = host.services().fallback.candidates(&query);
            }
            AnswerAction::TakeOver => {
                take_over::take_over(host, task)
                    .await
                    .map_err(EngineError::Refused)?;
                return Ok(());
            }
            AnswerAction::Wait => {
                let agent_id = plan.stopped.agent_id();
                // Asked while it already waited for its agent: it goes on as it was.
                if host.read(|state| waits_for(state, task, &agent_id)) {
                    return Ok(());
                }
                // Without a reset to wait for, it tries again after a while.
                let until = plan.resets_at.or_else(|| retry_at(host, task));
                return wait(host, task, agent_id, until, None);
            }
            AnswerAction::Retry => {
                return rules::back_in_line(host, task, "retry after you fixed the account");
            }
            AnswerAction::AskYou | AnswerAction::Stop | AnswerAction::Cancel => {
                pipeline::transition(
                    host,
                    task,
                    Trigger::AskYou,
                    "sent to you from the fallback question",
                )?;
                return Ok(());
            }
            other => {
                return Err(EngineError::Refused(format!(
                    "the fallback question doesn't offer {other:?}"
                )));
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Asked {
    Settled,
    /// The fallback question asks next: the agent isn't down, or you chose per task.
    PerTask,
}

/// An outage that has the agent marked down asks about the agent: one agent takes every
/// paused task (the first valid candidate), you choose per task, or the task waits for it.
/// On autopilot nothing is asked, and the task is settled on its own.
async fn agent_down(host: &Host, task: &TaskId, plan: &FallbackPlan) -> EngineResult<Asked> {
    let agent_id = plan.stopped.agent_id();
    let health = host.services().health.state(&agent_id);
    let down = health.state == HealthState::Down || health.disabled.is_some();
    if !down || host.read(|state| cause(state, task)) != FallbackCause::Outage {
        return Ok(Asked::PerTask);
    }
    // The question is how you'd like to choose, which autopilot never asks: each task gets the
    // agent it would recommend, or waits for its own and tries again.
    if autopilot::applies_now(host, ApprovalKind::AgentDown) {
        return Ok(Asked::PerTask);
    }
    let packet = AgentDownPacket {
        agent_id: agent_id.clone(),
        evidence: models_out(&host.services().health.models(&agent_id))
            .or(health.reason)
            .unwrap_or_else(|| plan.reason.clone()),
        test_at: health.next_test_at,
        paused: vec![PausedTask {
            task: task.clone(),
            title: plan.title.clone(),
            role: RoleName::Worker,
        }],
        fits_all: plan
            .candidates
            .first()
            .map(|candidate| candidate.worker.clone()),
        kept_by: None,
    };
    let request = ApprovalRequest {
        at: host.read(rules::current_step),
        kind: ApprovalKind::AgentDown,
        title: format!(
            "{} is down · {} waits for it",
            DisplayNames::default().cli(&agent_id.0),
            plan.title.trim()
        ),
        task: Some(task.clone()),
        packet: ApprovalPacket::AgentDown(packet),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: Some(down_options(&agent_id, !plan.candidates.is_empty())),
        free_text: FreeText::Guidance,
        authors: Vec::new(),
    };
    let decided = approvals::open_and_wait(host, request).await?;
    match decided.action {
        AnswerAction::OneForAll => {
            let Some(first) = plan.candidates.first() else {
                return Ok(Asked::PerTask);
            };
            let record = rules::record(host, task)?;
            let note = host.services().fallback.note(&record.spec, &[]);
            let tag = rules::applied_for(host, &record.spec, &plan.stopped).checkpoint;
            assign::fallback(host, task, &first.worker, &note, tag, decided.by).await?;
            Ok(Asked::Settled)
        }
        AnswerAction::Wait => {
            wait(host, task, agent_id, health.next_test_at, None)?;
            Ok(Asked::Settled)
        }
        AnswerAction::PerTask => Ok(Asked::PerTask),
        other => Err(EngineError::Refused(format!(
            "the agent-down question doesn't offer {other:?}"
        ))),
    }
}

/// What the agent-down question offers, in the order it shows them. Waiting comes first,
/// since a task that waits loses nothing. One agent for every paused task is offered only
/// when `one_fits`: an agent can take them.
pub(super) fn down_options(agent_id: &AgentId, one_fits: bool) -> Vec<AnswerOption> {
    let offer = |key: char, label: String, action: AnswerAction| AnswerOption {
        key,
        label,
        action,
        recommended: false,
        detail: None,
    };
    let cli = DisplayNames::default().cli(&agent_id.0);
    let mut options = vec![offer('w', format!("Wait for {cli}"), AnswerAction::Wait)];
    if one_fits {
        let label = "One agent for every paused task".to_owned();
        options.push(offer('a', label, AnswerAction::OneForAll));
    }
    options.push(offer(
        'e',
        "Choose per task".to_owned(),
        AnswerAction::PerTask,
    ));
    options
}

/// `Opus 5.5 overloaded, Sonnet 5.5 failing with server errors`: what each of a CLI's `models`
/// that is out has, which says more of a CLI that went down than that two of its models are
/// out. `None` when none is, as for a CLI the network took down.
pub(super) fn models_out(models: &[ModelHealth]) -> Option<String> {
    let names = DisplayNames::default();
    let out: Vec<String> = models
        .iter()
        .filter(|health| health.state == ModelState::Out)
        .map(|health| {
            let model = &health.model;
            let name = names
                .model(&model.agent, model.model.as_deref())
                .unwrap_or_else(|| names.cli(&model.agent));
            let has = match health.trouble {
                Some(Trouble::Overload) => "overloaded",
                Some(Trouble::ServerErrors) => "failing with server errors",
                None => "out",
            };
            format!("{name} {has}")
        })
        .collect();
    (!out.is_empty()).then(|| out.join(", "))
}

/// Autopilot, and nobody may take the task: the run stops fully, its work kept, as a step's
/// job nobody can take stops it. Waiting can't bring a login or a model back, and only
/// `/resume` carries the run on.
async fn stop_the_run(host: &Host, task: &TaskId, plan: &FallbackPlan) -> EngineResult<()> {
    let ended = ended(host, task);
    let stopped = display_name(&plan.stopped);
    let logged_out = says_logged_out(host, task, plan).await;
    let why = ended.as_ref().map_or_else(
        || "it stopped".to_owned(),
        |reason| {
            format!(
                "its session ended early ({})",
                job_fallback::in_words(reason)
            )
        },
    );
    let (job, tried) =
        job_fallback::stalled_task(host, &plan.title, &plan.stopped, ended.as_ref(), logged_out);
    Err(EngineError::Stalled(Box::new(Stall {
        detail: format!("nobody to fall back to for {}", plan.title.trim()),
        line: format!(
            "Nobody to fall back to from {stopped} for \"{}\": {why}, and no other agent can \
             take the task.",
            plan.title.trim()
        ),
        logged_out: if logged_out {
            vec![stopped]
        } else {
            Vec::new()
        },
        resets_at: plan.resets_at,
        kept: None,
        job,
        tried,
        in_step: Vec::new(),
        stop: None,
        beside: None,
    })))
}

/// When a task with no reset to wait for tries again: after the breaker's cool-down, doubled for
/// each wait it had. `None` once it stopped trying on its own.
fn retry_at(host: &Host, task: &TaskId) -> Option<OffsetDateTime> {
    let waits = host.read(|state| state.tasks.progress(task).agent_waits);
    job_fallback::retry_after(host.settings(), waits).map(|retry| host.now() + retry)
}

/// Whether the stopped agent's CLI says it's logged out, after the sign-in error that stopped
/// the task.
async fn says_logged_out(host: &Host, task: &TaskId, plan: &FallbackPlan) -> bool {
    match ended(host, task) {
        Some(reason) => {
            job_fallback::login_check(host, &plan.stopped, &reason).await == Some(false)
        }
        None => false,
    }
}

/// How the task's newest worker session ended.
fn ended(host: &Host, task: &TaskId) -> Option<EndReason> {
    host.read(|state| {
        newest_worker_session(state, task)
            .and_then(|session| session.end.as_ref())
            .map(|end| end.reason.clone())
    })
}

/// The task waits for its agent, in `NeedsDecision`. `kept_off` is the CLI that would have
/// taken it, where staying on its own CLI is why it waits.
pub fn wait(
    host: &Host,
    task: &TaskId,
    agent_id: AgentId,
    until: Option<OffsetDateTime>,
    kept_off: Option<AgentId>,
) -> EngineResult<()> {
    host.record(
        Some(task),
        None,
        RunEventKind::Fallback(FallbackEvent::Waiting {
            task: task.clone(),
            agent_id,
            until,
            kept_off,
        }),
    )?;
    Ok(())
}

/// Whether `task` already waits for `agent_id`, with the run's own wait behind it.
fn waits_for(state: &RunState, task: &TaskId, agent_id: &AgentId) -> bool {
    state.tasks.progress(task).waiting_for.as_ref() == Some(agent_id)
        && state.run.agent_waits.contains_key(task)
}

/// `agent` is back: each task that waits for it goes back in line with its worker, which
/// carries on in its own session. The task's own progress says what it waits for, since the
/// run's waits are cleared as the run wakes. A question about its being down has nothing
/// left to ask.
pub fn agent_back(host: &Host, agent_id: &AgentId) -> EngineResult<()> {
    down::withdraw(host, agent_id)?;
    let waiting: Vec<TaskId> = host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter(|record| record.state == TaskState::NeedsDecision)
            .map(|record| record.spec.id.clone())
            .filter(|task| state.tasks.progress(task).waiting_for.as_ref() == Some(agent_id))
            .collect()
    });
    for task in waiting {
        pipeline::transition(
            host,
            &task,
            Trigger::Decided,
            &format!("{agent_id} is back"),
        )?;
    }
    Ok(())
}

/// Why the task's last session stopped, which decides the question's first option.
pub fn cause(state: &RunState, task: &TaskId) -> FallbackCause {
    let last = state
        .sessions
        .by_key
        .values()
        .filter(|session| session.task.as_ref() == Some(task))
        .filter_map(|session| Some((session.since, &session.end.as_ref()?.reason)))
        .max_by_key(|(since, _)| *since);
    match last.map(|(_, reason)| reason) {
        Some(EndReason::Auth { .. }) => FallbackCause::LoggedOut,
        Some(EndReason::Billing { .. }) => FallbackCause::BillingCap,
        Some(EndReason::CliError { .. }) => FallbackCause::Outage,
        _ => FallbackCause::UsageLimit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(one_fits: bool) -> Vec<(char, String, AnswerAction)> {
        down_options(&AgentId("claude-code".into()), one_fits)
            .into_iter()
            .map(|option| (option.key, option.label, option.action))
            .collect()
    }

    #[test]
    fn the_agent_down_question_offers_waiting_first_and_one_agent_only_when_one_fits() {
        assert_eq!(
            offered(true),
            [
                ('w', "Wait for Claude Code".to_owned(), AnswerAction::Wait),
                (
                    'a',
                    "One agent for every paused task".to_owned(),
                    AnswerAction::OneForAll
                ),
                ('e', "Choose per task".to_owned(), AnswerAction::PerTask),
            ]
        );
        assert_eq!(
            offered(false),
            [
                ('w', "Wait for Claude Code".to_owned(), AnswerAction::Wait),
                ('e', "Choose per task".to_owned(), AnswerAction::PerTask),
            ]
        );
    }

    #[test]
    fn a_cli_that_is_down_is_explained_by_what_each_model_has() {
        let model = |name: &str, state: ModelState, trouble: Option<Trouble>| ModelHealth {
            state,
            trouble,
            ..ModelHealth::healthy(&format!("claude-code/{name}").parse().unwrap())
        };
        let models = [
            model("claude-opus-5-5", ModelState::Out, Some(Trouble::Overload)),
            model("claude-haiku-5-5", ModelState::Healthy, None),
            model(
                "claude-sonnet-5-5",
                ModelState::Out,
                Some(Trouble::ServerErrors),
            ),
        ];
        assert_eq!(
            models_out(&models).as_deref(),
            Some("Opus 5.5 overloaded, Sonnet 5.5 failing with server errors")
        );
        assert_eq!(models_out(&models[1..2]), None);
        assert_eq!(models_out(&[]), None);
    }
}
