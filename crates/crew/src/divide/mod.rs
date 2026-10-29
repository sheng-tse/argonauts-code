//! Dividing the work: the chosen plan becomes tasks (`PlanToTasks`), which code validates; then
//! contracts first, workers by the router, the review ring, predictions, limit lines, and new
//! dependencies. An orchestrator's plan comes with its tasks, workers, and reviewers, so only what
//! it left open is routed. The plan approval shows all of it together.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use futures::future::join_all;
use rand::SeedableRng;
use rand::rngs::StdRng;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::events::plan::{DivideEvent, LimitPlan, TaskGraph};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::task::{TaskKind, TaskSpec, TaskWay};
use argonauts_core::limits::{HealthState, PlanUsageStatus};
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::TestAuthor;
use argonauts_core::steps::plan::{
    GoalBrief, PlanDraft, PlanToTasks, PlanToTasksInput, TaskGraphProposal,
};
use argonauts_core::{Note, NoteLevel, RunId, TaskId};

use argonauts_learning::Store;
use argonauts_learning::records::Book;

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::orchestrated::{self, OrchestratedTasks, Rejected};
use crate::run_review::outcomes;
use crate::workflow::{StepCall, StepDone};
use crate::{prompts, roles, tie_breaker};

pub mod assign;
pub mod contracts;
pub mod dependencies;
pub mod edits;
pub mod graph;
pub mod limit_lines;
pub mod paths;
pub mod pick;
pub mod plan_approval;
pub mod plan_facts;
pub mod predictions;
pub mod ring;
pub mod router;
pub mod workflow_plan;

/// The size of a typical task session, for comparing registry cost estimates.
const TASK_INPUT_TOKENS: u64 = 400_000;
const TASK_OUTPUT_TOKENS: u64 = 40_000;

/// `approval: plan`: divide, then open the plan approval and act on your answer.
pub async fn plan_approval_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if !host.read(|state| workflow_plan::drafts_plan(state, &call.at.step.workflow)) {
        return workflow_plan::run(host, call).await;
    }
    plan_approval::run(host, call).await
}

/// The validated task graph with workers, reviewers, predictions, and limit lines, recorded as
/// `Divide` events. Retried once with the reasons when the proposal breaks a rule.
pub async fn divide(host: &Host, call: &StepCall) -> EngineResult<TaskGraph> {
    let division = division(host, call).await?;
    if division.broken.is_empty() {
        Ok(division.graph)
    } else {
        Err(EngineError::Refused(division.broken.join(" ")))
    }
}

/// Everything the plan approval shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Division {
    pub plan: PlanDraft,
    /// The sealed plan as the judges saw it, for the evidence packet.
    pub plan_text: String,
    pub plan_author: ModelRef,
    /// Who turned the plan into tasks.
    pub divided_by: ModelRef,
    pub graph: TaskGraph,
    pub limit: LimitPlan,
    /// Rules the graph still breaks after the retry. Any one blocks approval, and nothing was
    /// recorded.
    pub broken: Vec<String>,
}

/// Divides the chosen plan, or finds the division already recorded for it. An orchestrator's
/// plan that broke the rules twice has nothing to divide; its reasons block approval.
pub async fn division(host: &Host, call: &StepCall) -> EngineResult<Division> {
    if let Some(rejected) = host.read(orchestrated::rejected) {
        return Ok(rejected_division(host.settings(), rejected));
    }
    let chosen = chosen_plan(host)?;
    let divided_by = if chosen.delegated.is_some() {
        chosen.author.clone()
    } else {
        divider(host)
    };
    if let Some((graph, limit)) = current(host) {
        return Ok(Division {
            plan: chosen.plan,
            plan_text: chosen.text,
            plan_author: chosen.author,
            divided_by,
            graph,
            limit,
            broken: Vec::new(),
        });
    }
    // Who may own and review as the rules settled, which routing and the review ring go by.
    let task_roles = roles::tasks::for_run(host);
    task_roles.announce(host)?;
    let settled = task_roles.apply(host.settings());
    let settings = &*settled;
    let (tasks, mut broken) = match &chosen.delegated {
        Some(delegated) => {
            let proposal = TaskGraphProposal {
                tasks: delegated.tasks.clone(),
            };
            match build(host.run_id(), &proposal, settings) {
                Ok(tasks) => (tasks, Vec::new()),
                Err(reasons) => (graph::convert(&proposal).0, reasons),
            }
        }
        None => {
            let input = PlanToTasksInput {
                goal: goal_brief(host)?,
                plan: chosen.plan.clone(),
                decisions: decisions(host),
                forbidden_paths: forbidden_paths(settings),
                max_tasks: settings.divide.max_tasks,
            };
            let first = ask_tasks(host, call, &divided_by, input.clone(), &[]).await?;
            match build(host.run_id(), &first, settings) {
                Ok(tasks) => (tasks, Vec::new()),
                Err(reasons) => {
                    let second = ask_tasks(host, call, &divided_by, input, &reasons).await?;
                    match build(host.run_id(), &second, settings) {
                        Ok(tasks) => (tasks, Vec::new()),
                        Err(reasons) => (graph::convert(&second).0, reasons),
                    }
                }
            }
        }
    };
    let limit = match limit_lines::plan(&settings.limits.spend) {
        Ok(limit) => limit.unwrap_or_else(empty_limit),
        Err(reason) => {
            broken.push(reason);
            empty_limit()
        }
    };
    if !broken.is_empty() {
        return Ok(Division {
            plan: chosen.plan,
            plan_text: chosen.text,
            plan_author: chosen.author,
            divided_by,
            graph: TaskGraph {
                critical_path: tie_breaker::critical_path(&tasks),
                tasks,
            },
            limit,
            broken,
        });
    }

    let run_task_type = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .and_then(|started| started.estimate.as_ref())
            .map(|estimate| estimate.task_type.clone())
    });
    let labels = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .and_then(|started| started.estimate.as_ref())
            .map(|estimate| estimate.risk_labels.clone())
            .unwrap_or_default()
    });
    let tasks = with_limits(host, tasks, run_task_type.as_deref())?;
    let book = track_records(host);
    let protected = forbidden_paths(settings);
    let assignment = {
        let workers = |task_type: &str| {
            worker_pool(host)
                .into_iter()
                .map(|model| candidate(host, model, task_type, book.as_ref()))
                .collect()
        };
        let provider = |model: &ModelRef| roles::provider(host, model);
        let crew = assign::Crew {
            settings,
            labels: &labels,
            run_task_type: run_task_type.as_deref(),
            workers: &workers,
            provider: &provider,
            protected: &protected,
        };
        let mut rng = StdRng::seed_from_u64(seed(host.run_id()));
        match &chosen.delegated {
            Some(delegated) => delegated_assignment(tasks, delegated, &crew, &mut rng),
            None => routed(tasks, &crew, &mut rng),
        }
    };
    let mut tasks = assignment.tasks;
    log_draws(host, &assignment.draws)?;
    if settings.divide.predictions {
        predict(host, call, &mut tasks).await?;
        predictions::log(host, &tasks, run_task_type.as_deref())?;
    }

    for pick in &assignment.picks {
        let event = DivideEvent::Worker {
            task: pick.task.clone(),
            worker: pick.choice.worker.clone(),
            algorithm: pick.algorithm,
            explored: pick.choice.explored,
            reason: pick.choice.reason.clone(),
        };
        host.record(Some(&pick.task), None, RunEventKind::Divide(event))?;
    }
    for task in tasks.iter().filter(|task| task.worker.is_some()) {
        let event = DivideEvent::Reviewers {
            task: task.id.clone(),
            reviewers: task.reviewers.clone(),
            blind: settings.review.blind,
        };
        host.record(Some(&task.id), None, RunEventKind::Divide(event))?;
        if let Some(predictions) = task.predictions {
            let event = DivideEvent::Predicted {
                task: task.id.clone(),
                predictions,
            };
            host.record(Some(&task.id), None, RunEventKind::Divide(event))?;
        }
    }
    host.record(
        None,
        None,
        RunEventKind::Divide(DivideEvent::Limit(limit.clone())),
    )?;
    // A task the plan gave no way gets the rule of thumb's, from the ways the setup offers, so
    // the run's record says from the graph on how each task runs. The plan approval then puts
    // the ways your request names in their place.
    let offer = crate::council::ways::offer_for(host);
    for task in &mut tasks {
        if task.way.is_none() {
            let how = crate::council::ways::propose(task, &offer).way;
            task.way = Some(TaskWay {
                how,
                asked_by_you: false,
                steps: None,
            });
        }
    }
    let graph = TaskGraph {
        critical_path: tie_breaker::critical_path(&tasks),
        tasks,
    };
    host.record(
        None,
        None,
        RunEventKind::Divide(DivideEvent::Graph(graph.clone())),
    )?;
    Ok(Division {
        plan: chosen.plan,
        plan_text: chosen.text,
        plan_author: chosen.author,
        divided_by,
        graph,
        limit,
        broken: Vec::new(),
    })
}

/// Every worker predicts its task at once. Predictions only feed the run review, so one that fails
/// is left out with a note.
async fn predict(host: &Host, call: &StepCall, tasks: &mut [TaskSpec]) -> EngineResult<()> {
    let asks = tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.worker.is_some())
        .map(|(index, task)| async move {
            let at = Some(call.at.clone());
            (index, predictions::predict(host, at, task).await)
        });
    for (index, answer) in join_all(asks.collect::<Vec<_>>()).await {
        match answer {
            Ok(predicted) => tasks[index].predictions = Some(predicted),
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                host.record(
                    Some(&tasks[index].id),
                    None,
                    RunEventKind::Note(Note {
                        level: NoteLevel::Warn,
                        text: format!("{}'s prediction is left out: {error}", tasks[index].id),
                    }),
                )?;
            }
        }
    }
    Ok(())
}

/// Workers and reviewers by the router and the review ring, each pick named by the algorithm
/// that made it.
fn routed(tasks: Vec<TaskSpec>, crew: &assign::Crew, rng: &mut StdRng) -> assign::Assignment {
    let mut assignment = assign::assign(tasks, crew, rng);
    for pick in &mut assignment.picks {
        pick.algorithm = router::effective(pick.algorithm);
    }
    assignment
}

/// The orchestrator's workers and reviewers, and the router's for any task it left without one.
fn delegated_assignment(
    mut tasks: Vec<TaskSpec>,
    delegated: &OrchestratedTasks,
    crew: &assign::Crew,
    rng: &mut StdRng,
) -> assign::Assignment {
    let mut picks = orchestrated::delegate(&mut tasks, delegated, crew.settings, crew.provider);
    let unowned: Vec<TaskSpec> = tasks
        .iter()
        .filter(|task| task.worker.is_none())
        .cloned()
        .collect();
    if unowned.is_empty() {
        return assign::Assignment {
            tasks,
            picks,
            draws: Vec::new(),
        };
    }
    let routed = routed(unowned, crew, rng);
    picks.extend(routed.picks);
    let draws = routed.draws;
    let mut by_id: BTreeMap<TaskId, TaskSpec> = routed
        .tasks
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect();
    let tasks = tasks
        .into_iter()
        .map(|task| by_id.remove(&task.id).unwrap_or(task))
        .collect();
    assign::Assignment {
        tasks,
        picks,
        draws,
    }
}

/// The track records as of now, rebuilt from the outcome log at divide time so this run routes
/// on every run before it. `None` when the log can't be read, and the router falls back to the
/// plain counts fallback questions read.
pub(crate) fn track_records(host: &Host) -> Option<Book> {
    let store = Store::new(repo_state_dir(host), host.repo().root.clone());
    match Book::rebuild(&store, &host.settings().routing, host.now()) {
        Ok(book) => Some(book),
        Err(err) => {
            tracing::warn!("track records unavailable, routing on plain counts: {err}");
            None
        }
    }
}

/// Each draw goes to the outcome log before any task starts, a draw already there once.
fn log_draws(host: &Host, draws: &[argonauts_learning::routing::Draw]) -> EngineResult<()> {
    let state_dir = repo_state_dir(host);
    let logged: Vec<TaskId> = outcomes::run_lines(&state_dir, host.run_id())?
        .into_iter()
        .filter_map(|line| match line {
            outcomes::LogLine::Draw(draw) => Some(draw.task),
            _ => None,
        })
        .collect();
    let lines: Vec<outcomes::LogLine> = draws
        .iter()
        .filter(|draw| !logged.contains(&draw.task))
        .cloned()
        .map(outcomes::LogLine::Draw)
        .collect();
    outcomes::append_lines(&state_dir, host.run_id(), &lines)
}

/// Nothing to divide: the plan approval shows why the orchestrator's plan was rejected, and
/// you send it back, run it as Captain, or abandon the run.
fn rejected_division(settings: &Settings, rejected: Rejected) -> Division {
    let summary = "The orchestrator's plan broke the rules twice.".to_owned();
    let mut broken = vec![format!(
        "{summary} Send it back with a note, run it as Captain, or abandon the run."
    )];
    let tries: Vec<String> = rejected
        .reasons
        .iter()
        .enumerate()
        .map(|(n, reason)| format!("Try {}: {reason}", n + 1))
        .collect();
    broken.extend(tries.iter().cloned());
    Division {
        plan: PlanDraft {
            summary: summary.clone(),
            approach: String::new(),
            pieces: Vec::new(),
            choices: Vec::new(),
            risks: Vec::new(),
            test_strategy: String::new(),
            new_dependencies: Vec::new(),
        },
        plan_text: format!("{summary}\n\n- {}\n", tries.join("\n- ")),
        plan_author: rejected.orchestrator.clone(),
        divided_by: rejected.orchestrator,
        graph: TaskGraph {
            tasks: Vec::new(),
            critical_path: Vec::new(),
        },
        limit: limit_plan(settings),
        broken,
    }
}

/// Validated tasks with contracts first applied, or every rule they break.
pub fn build(
    run: &RunId,
    proposal: &TaskGraphProposal,
    settings: &Settings,
) -> Result<Vec<TaskSpec>, Vec<String>> {
    let tasks = graph::validate(run, proposal, settings)?;
    if !settings.divide.contracts_first {
        return Ok(tasks);
    }
    let interfaces = tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Interface)
        .count();
    let builds = tasks.iter().any(|task| task.kind == TaskKind::Build);
    if builds && interfaces != 1 {
        return Err(vec![format!(
            "Contracts first needs exactly one interface task (kind `interface`) with the \
             stubs every build task codes against. The plan has {interfaces}."
        )]);
    }
    let tests = settings.divide.acceptance_tests_by == TestAuthor::Reviewer;
    let tasks = contracts::contracts_first(tasks, tests);
    let broken = graph::rules(&tasks, settings);
    if broken.is_empty() {
        Ok(tasks)
    } else {
        Err(broken)
    }
}

/// `.argonauts/**` and `limits.protected_paths`.
pub fn forbidden_paths(settings: &Settings) -> Vec<String> {
    std::iter::once(graph::CONDUCTOR_AREA.to_owned())
        .chain(settings.limits.protected_paths.iter().cloned())
        .collect()
}

/// `~/.argonauts/state/<repo>`, which holds the outcome log.
pub fn repo_state_dir(host: &Host) -> PathBuf {
    let run_dir = &host.repo().run_dir;
    run_dir
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| run_dir.clone(), Path::to_path_buf)
}

/// The recorded division, unless a plan approval after it sent the plan back. The pick question
/// of the next round is a plan approval too, so any send back since counts, not only the last.
fn current(host: &Host) -> Option<(TaskGraph, LimitPlan)> {
    host.read(|state| {
        let graph = state.divide.graph.clone()?;
        let seq = state.divide.graph_seq?;
        let replanned = state.approvals.by_id.values().any(|record| {
            record.opened.kind == ApprovalKind::Plan
                && record.since > seq
                && record.withdrawn.is_none()
                && record
                    .decided
                    .as_ref()
                    .is_some_and(|decided| decided.action == AnswerAction::SendBack)
        });
        let limit = state.divide.limit.clone().unwrap_or_else(empty_limit);
        (!replanned).then_some((graph, limit))
    })
}

struct ChosenPlan {
    plan: PlanDraft,
    text: String,
    author: ModelRef,
    /// The orchestrator's tasks and assignments, when the plan is the orchestrator's.
    delegated: Option<OrchestratedTasks>,
}

/// The chosen draft. Its structured form is read from `<draft>.json` beside the sealed text
/// when it's there, or from the text itself when that is JSON; otherwise the text is the
/// approach and the new dependencies come from its "New dependencies" section.
fn chosen_plan(host: &Host) -> EngineResult<ChosenPlan> {
    let (chosen, orchestrated) = host.read(|state| {
        (
            state.plan.chosen().cloned(),
            state.plan.orchestrated_plan_chosen(),
        )
    });
    let chosen = chosen
        .ok_or_else(|| EngineError::Refused("No plan has been chosen to divide.".to_owned()))?;
    let delegated = if orchestrated {
        Some(orchestrated::load_tasks(&chosen)?)
    } else {
        None
    };
    let text = std::fs::read_to_string(&chosen.path)?;
    let sidecar = chosen.path.with_extension("json");
    let plan = if sidecar.is_file() {
        serde_json::from_str(&std::fs::read_to_string(&sidecar)?)?
    } else if let Ok(plan) = serde_json::from_str::<PlanDraft>(&text) {
        plan
    } else {
        plan_from_text(&text)
    };
    Ok(ChosenPlan {
        plan,
        text,
        author: chosen.worker,
        delegated,
    })
}

pub fn plan_from_text(text: &str) -> PlanDraft {
    let summary = text
        .split("\n\n")
        .map(str::trim)
        .find(|paragraph| !paragraph.is_empty() && !paragraph.starts_with('#'))
        .unwrap_or_default()
        .to_owned();
    PlanDraft {
        summary,
        approach: text.to_owned(),
        pieces: Vec::new(),
        choices: Vec::new(),
        risks: Vec::new(),
        test_strategy: String::new(),
        new_dependencies: section_items(text, "new dependencies"),
    }
}

/// The bullet items under the first heading that contains `heading`.
fn section_items(text: &str, heading: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut inside = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') {
            if inside {
                break;
            }
            inside = line.to_ascii_lowercase().contains(heading);
            continue;
        }
        if !inside {
            continue;
        }
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let item = item.trim().trim_matches('`').trim();
            if !item.is_empty() && !item.eq_ignore_ascii_case("none") {
                items.push(item.to_owned());
            }
        }
    }
    items
}

fn goal_brief(host: &Host) -> EngineResult<GoalBrief> {
    let goal = host
        .read(|state| state.goal.approved.clone())
        .ok_or_else(|| EngineError::Refused("The goal isn't approved yet.".to_owned()))?;
    Ok(GoalBrief {
        summary: goal.summary,
        criteria: goal
            .criteria
            .iter()
            .map(|criterion| format!("{}: {}", criterion.id, criterion.text))
            .collect(),
        assumptions: goal.assumptions.iter().map(|a| a.text.clone()).collect(),
        sha256: goal.sha256,
    })
}

fn decisions(host: &Host) -> Vec<String> {
    host.read(|state| {
        state
            .plan
            .decisions
            .iter()
            .map(|decision| format!("{} {}: {}", decision.id, decision.title, decision.choice))
            .collect()
    })
}

/// The tie-breaker turns the plan into tasks; the main agent when there is none.
fn divider(host: &Host) -> ModelRef {
    let settings = host.settings();
    host.read(|state| tie_breaker::tie_breaker(state, settings))
        .unwrap_or_else(|| settings.crew.main.clone())
}

async fn ask_tasks(
    host: &Host,
    call: &StepCall,
    worker: &ModelRef,
    input: PlanToTasksInput,
    broken: &[String],
) -> EngineResult<TaskGraphProposal> {
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::TieBreaker,
        worker.clone(),
        prompts::divide::PLAN_TO_TASKS,
        input,
    );
    if !broken.is_empty() {
        let text = format!(
            "Your last proposal broke these rules. Propose the tasks again with each fixed.\n\n- {}",
            broken.join("\n- ")
        );
        ask.parts.push(InputPart {
            kind: InputPartKind::Message,
            label: "rules broken".to_owned(),
            approx_tokens: (text.len() / 4) as u32,
            text,
            source: None,
        });
        ask.slot = 1;
    }
    let note = host.read(crate::state::plan::replan_note);
    ask.parts.extend(crate::plan::drafting::guidance_part(
        note.as_deref(),
        "plan approval",
    ));
    // The fix for what already fails goes to one task, and no other is told to leave it be.
    ask.parts.extend(crate::task::at_start::part(host).await?);
    Ok(host.ask::<PlanToTasks>(ask).await?.output)
}

/// Each task's limit: the median spend of past tasks of its task type, else `per_task_*`.
fn with_limits(
    host: &Host,
    mut tasks: Vec<TaskSpec>,
    run_task_type: Option<&str>,
) -> EngineResult<Vec<TaskSpec>> {
    let state_dir = repo_state_dir(host);
    let mut history: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for task in &mut tasks {
        let task_type = assign::task_type(&task.kind, run_task_type);
        if !history.contains_key(&task_type) {
            let spent = outcomes::load(&state_dir, &task_type)?
                .into_iter()
                .map(|record| record.spent_usd)
                .collect();
            history.insert(task_type.clone(), spent);
        }
        let past = history
            .get(&task_type)
            .map(Vec::as_slice)
            .unwrap_or_default();
        task.limit = limit_lines::task_limit(past, &host.settings().limits.spend);
    }
    Ok(tasks)
}

/// Who may build a task: the workers' list, or with the orchestrator picking, every member of
/// the crew that's set up here, then anything else that is.
pub(crate) fn worker_pool(host: &Host) -> Vec<ModelRef> {
    let settings = host.settings();
    if !settings.crew.orchestrator_picks.worker {
        return roles::candidates(settings, RoleName::Worker);
    }
    let available = host.services().workers.available();
    let mut pool: Vec<ModelRef> = Vec::new();
    let members = settings
        .crew
        .members()
        .into_iter()
        .chain(std::iter::once(settings.crew.main.clone()))
        .filter(|model| available.contains(model));
    for model in members.chain(available.iter().cloned()) {
        if !pool.contains(&model) {
            pool.push(model);
        }
    }
    pool
}

/// `pool` in the orchestrator's order for a task of `task_type`, as [`router::pick_order`]
/// ranks it from each model's record, plan and cost now.
pub(crate) fn picked_order(host: &Host, pool: Vec<ModelRef>, task_type: &str) -> Vec<ModelRef> {
    let book = track_records(host);
    let candidates: Vec<router::Candidate> = pool
        .into_iter()
        .map(|model| candidate(host, model, task_type, book.as_ref()))
        .collect();
    router::pick_order(&candidates)
}

pub(crate) fn candidate(
    host: &Host,
    worker: ModelRef,
    task_type: &str,
    book: Option<&Book>,
) -> router::Candidate {
    let services = host.services();
    let agent_id = worker.agent_id();
    let health = services.health.state(&agent_id);
    let plan_usage = services.plan_usage.get(&agent_id);
    router::Candidate {
        record: services.track_records.get(&worker, task_type),
        health: if health.disabled.is_some() {
            HealthState::Down
        } else {
            health.state
        },
        plan_usage_used: plan_usage.and_then(|reading| match reading.status {
            PlanUsageStatus::Rejected => Some(1.0),
            _ => reading.used,
        }),
        plan_limit: host.settings().limits.spend.plan_usage,
        cost: services
            .registry
            .estimate(&worker, TASK_INPUT_TOKENS, TASK_OUTPUT_TOKENS),
        running_tasks: 0,
        learned: book.map(|book| book.record(&worker, task_type)),
        worker,
    }
}

/// The run's limit by line, or the empty plan when none is set or its lines don't add up.
pub(crate) fn limit_plan(settings: &Settings) -> LimitPlan {
    limit_lines::plan(&settings.limits.spend)
        .ok()
        .flatten()
        .unwrap_or_else(empty_limit)
}

pub(crate) fn empty_limit() -> LimitPlan {
    LimitPlan::unlimited()
}

/// The run's own seed (FNV-1a of its id), so a re-divided plan routes the same way.
fn seed(run: &RunId) -> u64 {
    run.as_str()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::{ProposedKind, TaskProposal};

    use super::*;

    fn proposal(interfaces: usize) -> TaskGraphProposal {
        let task = |key: &str, kind, owned: &str| TaskProposal {
            key: key.to_owned(),
            title: key.to_owned(),
            task: key.to_owned(),
            kind,
            owned_paths: vec![owned.to_owned()],
            read_only_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: vec!["G1".to_owned()],
        };
        let mut tasks = vec![task("store", ProposedKind::Build, "src/store/**")];
        for n in 0..interfaces {
            tasks.push(task(
                &format!("api{n}"),
                ProposedKind::Interface,
                &format!("src/api{n}.rs"),
            ));
        }
        TaskGraphProposal { tasks }
    }

    #[test]
    fn contracts_first_needs_one_interface_task() {
        let run = RunId::new("run-01").unwrap();
        let settings = Settings::default();
        assert_eq!(build(&run, &proposal(1), &settings).unwrap().len(), 3);
        let reasons = build(&run, &proposal(0), &settings).unwrap_err();
        assert!(
            reasons[0].contains("exactly one interface task"),
            "{reasons:?}"
        );
        assert!(build(&run, &proposal(2), &settings).is_err());
        let mut off = Settings::default();
        off.divide.contracts_first = false;
        assert_eq!(build(&run, &proposal(0), &off).unwrap().len(), 1);
        let mut no_tests = Settings::default();
        no_tests.divide.acceptance_tests_by = TestAuthor::None;
        assert_eq!(build(&run, &proposal(1), &no_tests).unwrap().len(), 2);
    }

    #[test]
    fn a_plan_in_markdown_still_lists_its_dependencies() {
        let text = "# Plan\n\nKeep counters in Redis.\n\n## Pieces\n\n- store\n\n## New dependencies\n\n- `ioredis`\n- none\n\n## Risks\n\n- latency\n";
        let plan = plan_from_text(text);
        assert_eq!(plan.summary, "Keep counters in Redis.");
        assert_eq!(plan.new_dependencies, ["ioredis"]);
        assert_eq!(plan.approach, text);
    }

    #[test]
    fn the_seed_is_stable_per_run() {
        let a = RunId::new("run-20260930-101500-00aa").unwrap();
        let b = RunId::new("run-20260930-101500-00ab").unwrap();
        assert_eq!(seed(&a), seed(&a));
        assert_ne!(seed(&a), seed(&b));
    }
}
