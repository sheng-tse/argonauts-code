//! The orchestrator: the orchestrator runs `Orchestrate` (plan, tasks, workers, reviewers) in a
//! run-lifetime session, told first who may own and who may review. Code checks its tasks
//! against the rules (owned paths, limit, the task graph); a broken rule is rejected with the
//! reason and retried once, then the plan approval decides. When the tasks are sound, a worker
//! or reviewer the rules don't allow is moved by code to one they do, by the router's ranking,
//! and one note says what moved. The orchestrator can't raise a limit or approve anything.
//! Dividing the work takes its tasks and assignments as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use argonauts_core::TaskId;
use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::base::text::clause_case;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{AnswerAction, ApprovalKind};
use argonauts_core::runs::events::plan::PlanEvent;
use argonauts_core::runs::events::run::{ForkFrom, StartsFrom};
use argonauts_core::runs::events::session::{InputPart, InputPartKind};
use argonauts_core::runs::events::task::{TaskKind, TaskSpec};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{RoutingAlgorithm, TestAuthor};
use argonauts_core::workflows::steps::plan::{
    Assignment, Orchestrate, OrchestrateInput, OrchestrateOutput, TaskGraphProposal, TaskProposal,
};
use argonauts_core::{Note, NoteLevel, write_private_file};

use crate::approvals::autopilot;
use crate::divide::assign::{self as divide_assign, Pick};
use crate::divide::router::{self, Choice};
use crate::divide::{self, ring};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Ask, Host};
use crate::plan::context::{self, providers};
use crate::plan::drafts;
use crate::prompts::plan::ORCHESTRATE;
use crate::roles;
use crate::roles::tasks::{TaskRoles, named_in};
use crate::state::RunState;
use crate::state::plan::{self, OrchestratedRecord, SealedDraft};
use crate::task::at_start;
use crate::workflow::{StepCall, StepDone};

/// A rejected answer is retried once; the second rejection goes to you.
const TRIES: usize = 2;

/// What the orchestrator reads first when it is a copy of the main session. The conversation
/// it carries was with you, so it is told that part is over and what it does now; the brief
/// and the crew's tools come after.
pub const FORKED: &str = "You are now the orchestrator of the run you just started.\n\
     You no longer talk to the user: nothing you write reaches them, and the conversation's \
     own tools are gone. What you and the user settled in the conversation still holds. The \
     brief below is the run's request as you handed it over. Plan the run from both, with the \
     crew's tools, and answer in the form the brief asks for.";

/// The main session `orchestrator` starts as a copy of: the run's, when your words in a
/// conversation on the orchestrator's own CLI started it.
pub fn fork_for(host: &Host, orchestrator: &ModelRef) -> Option<ForkFrom> {
    host.read(
        |state| match state.run.started.as_ref()?.starts_from.as_ref()? {
            StartsFrom::Fork(from) if from.worker.agent == orchestrator.agent => Some(from.clone()),
            _ => None,
        },
    )
}

/// [`FORKED`] as the first part of a forked orchestrator's prompt, before the brief.
pub fn forked_opening() -> InputPart {
    context::part(
        InputPartKind::Prompt,
        "you are the orchestrator now",
        FORKED.to_owned(),
    )
}

/// The orchestrator's tasks and assignments, sealed beside its plan for dividing the work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestratedTasks {
    pub tasks: Vec<TaskProposal>,
    pub assignments: Vec<Assignment>,
}

/// The orchestrator's plan broke the rules on every try since you last sent it back.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub orchestrator: ModelRef,
    /// Why each try was rejected, oldest first.
    pub reasons: Vec<String>,
}

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    context::phase_started(host)?;
    let orchestrator = context::workers_for(call, host.settings(), RoleName::Orchestrator, Some(1))
        .into_iter()
        .next()
        .unwrap_or_else(|| host.settings().crew.main.clone());
    let provider = providers(host);
    let task_roles = roles::tasks::for_run(host);
    task_roles.announce(host)?;
    let settings = task_roles.apply(host.settings());
    // A copy that failed before its session was recorded is never tried again either.
    let mut fork_gave_way = false;
    loop {
        // A plan you sent back gets two new tries, in the orchestrator's own session, with your
        // note. A settled plan sent back starts a new planning round.
        let (records, sent_back, replanning, asked, session, failed_forks) = host.read(|state| {
            (
                tries(state),
                sent_back(state),
                plan::sent_back(state).is_some(),
                state.plan.orchestrated.len(),
                state
                    .plan
                    .orchestrated
                    .iter()
                    .rev()
                    .find_map(|r| r.session.clone()),
                failed_forks(state, call),
            )
        });
        if records.iter().any(|record| record.rejection.is_none()) {
            return Ok(StepDone::done("The orchestrator's plan follows the rules"));
        }
        if records.len() >= TRIES {
            let summary = if autopilot::applies_now(host, ApprovalKind::Plan) {
                "The orchestrator's plan broke the rules twice; autopilot settles the plan \
                 approval"
            } else {
                "The orchestrator's plan broke the rules twice; the plan approval asks you"
            };
            context::phase_finished(host, summary)?;
            return Ok(StepDone::done(summary));
        }
        let rejection = match records.last() {
            Some(record) => record.rejection.clone(),
            None => sent_back.map(|sent| match sent.note {
                Some(note) => format!("You sent the plan back with this note: {note}"),
                None => "You sent the plan back.".to_owned(),
            }),
        };
        let input = host.read(|state| OrchestrateInput {
            goal: context::goal_brief(state),
            crew: task_roles.lines(&provider),
            rules: rules(&settings),
            rejection,
        });
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Orchestrator,
            orchestrator.clone(),
            ORCHESTRATE,
            input,
        );
        ask.access = Access::ReadOnly;
        // Counted over the whole run, so an answer after a send-back never reuses a slot. A copy
        // of the main session that ended without a plan took a slot of its own.
        ask.slot = (asked + failed_forks) as u32;
        ask.resume = session;
        ask.parts
            .push(context::status_part(host, RoleName::Orchestrator, None));
        // A session that carries on already heard it. A new one may start as a copy of the
        // main session, until one such copy ended without a plan.
        if ask.resume.is_none() {
            if failed_forks == 0 && !fork_gave_way {
                ask.fork_of = fork_for(host, &orchestrator);
            }
            ask.parts.extend(at_start::part(host).await?);
        }
        // A copy gets one try and no fallback: the same orchestrator plans from the brief next.
        let forked = ask.fork_of.is_some();
        ask.optional = forked;
        let answered = match host.ask::<Orchestrate>(ask).await {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(_) if forked => {
                fork_gave_way = true;
                host.record(
                    None,
                    None,
                    RunEventKind::Note(Note {
                        level: NoteLevel::Info,
                        text: "The copy of your main session ended without a plan, so the \
                               orchestrator plans from the brief"
                            .to_owned(),
                    }),
                )?;
                continue;
            }
            Err(error) => {
                reject(
                    host,
                    &orchestrator,
                    None,
                    format!("the answer failed: {error}"),
                )?;
                continue;
            }
        };
        let mut output = answered.output;
        let mut broken = task_problems(&output.tasks, &settings);
        if broken.is_empty() {
            broken = graph_problems(host, &output.tasks);
        }
        if broken.is_empty() {
            let moved = repair(
                host.names(),
                &mut output,
                &task_roles,
                &ranking(host, &task_roles),
                &provider,
            );
            moved_note(host, &moved)?;
            broken = assignment_problems(host.names(), &output, &task_roles, &provider);
        }
        if !broken.is_empty() {
            reject(
                host,
                &orchestrator,
                Some(&answered.session),
                broken.join("; "),
            )?;
            continue;
        }
        let round = host.read(|state| state.plan.blind_round(replanning));
        let draft = drafts::seal(
            host,
            1,
            &orchestrator,
            round,
            &output.plan,
            Some(&answered.session),
        )?;
        let sealed = host
            .read(|state| state.plan.drafts.iter().find(|d| d.draft == draft).cloned())
            .ok_or_else(|| EngineError::Refused(format!("draft {draft} was not recorded")))?;
        let tasks = OrchestratedTasks {
            tasks: output.tasks,
            assignments: output.assignments,
        };
        write_private_file(
            &tasks_path(&sealed.path),
            &serde_json::to_vec_pretty(&tasks)?,
        )?;
        host.record(
            None,
            Some(&answered.session),
            RunEventKind::Plan(PlanEvent::Orchestrated {
                orchestrator: orchestrator.clone(),
                rejection: None,
            }),
        )?;
        let summary = format!(
            "The orchestrator planned {} task{}",
            tasks.tasks.len(),
            if tasks.tasks.len() == 1 { "" } else { "s" }
        );
        context::phase_finished(host, &summary)?;
        return Ok(StepDone::done(summary));
    }
}

/// What the task graph validator and contracts first say about the tasks, which dividing the
/// work would otherwise find only after the orchestrator is done.
fn graph_problems(host: &Host, tasks: &[TaskProposal]) -> Vec<String> {
    let proposal = TaskGraphProposal {
        tasks: tasks.to_vec(),
    };
    divide::build(host.run_id(), &proposal, host.settings())
        .err()
        .unwrap_or_default()
}

/// The copies of the main session this step started for its orchestrator that ended without
/// a plan any try was recorded with. Once one has, the orchestrator plans from the brief.
fn failed_forks(state: &RunState, call: &StepCall) -> usize {
    let answered: BTreeSet<&SessionKey> = state
        .plan
        .orchestrated
        .iter()
        .filter_map(|record| record.session.as_ref())
        .collect();
    state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| {
            session.started.role == RoleName::Orchestrator
                && session.started.forked
                && session.started.workflow_step.as_ref() == Some(&call.at.step)
                && !session.resumable()
                && !answered.contains(&session.key)
        })
        .count()
}

struct SentBack {
    /// The seq of the plan approval that was answered.
    since: u64,
    note: Option<String>,
}

/// The last time you sent the plan back at the plan approval.
fn sent_back(state: &RunState) -> Option<SentBack> {
    state
        .approvals
        .by_id
        .values()
        .filter(|record| record.opened.kind == ApprovalKind::Plan && record.withdrawn.is_none())
        .filter_map(|record| {
            let decided = record.decided.as_ref()?;
            (decided.action == AnswerAction::SendBack).then(|| SentBack {
                since: record.since,
                note: decided
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|note| !note.is_empty())
                    .map(str::to_owned),
            })
        })
        .max_by_key(|sent| sent.since)
}

/// The orchestrator's answers since you last sent its plan back.
fn tries(state: &RunState) -> Vec<OrchestratedRecord> {
    let since = sent_back(state).map(|sent| sent.since);
    state
        .plan
        .orchestrated
        .iter()
        .filter(|record| since.is_none_or(|since| record.seq > since))
        .cloned()
        .collect()
}

/// Why the orchestrator's plan was rejected, once it has used every try since you last sent it
/// back without an answer that stands.
pub fn rejected(state: &RunState) -> Option<Rejected> {
    let tries = tries(state);
    if tries.len() < TRIES || tries.iter().any(|record| record.rejection.is_none()) {
        return None;
    }
    Some(Rejected {
        orchestrator: tries.last()?.orchestrator.clone(),
        reasons: tries
            .into_iter()
            .filter_map(|record| record.rejection)
            .collect(),
    })
}

fn reject(
    host: &Host,
    orchestrator: &ModelRef,
    session: Option<&SessionKey>,
    reason: String,
) -> EngineResult<()> {
    host.record(
        None,
        session,
        RunEventKind::Plan(PlanEvent::Orchestrated {
            orchestrator: orchestrator.clone(),
            rejection: Some(reason),
        }),
    )?;
    Ok(())
}

fn tasks_path(plan: &Path) -> PathBuf {
    plan.with_extension("tasks.json")
}

/// The accepted orchestrator's tasks and assignments, for dividing the work.
pub fn load_tasks(sealed: &SealedDraft) -> EngineResult<OrchestratedTasks> {
    let bytes = std::fs::read(tasks_path(&sealed.path))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Puts the orchestrator's workers and reviewers on the tasks built from its proposal, where the
/// proposal's n-th task became `task-00n`. Each acceptance-test task goes to its build task's
/// first reviewer, with a review ring of its own. A task left without a worker is the router's.
pub fn delegate(
    tasks: &mut [TaskSpec],
    delegated: &OrchestratedTasks,
    settings: &Settings,
    provider: &dyn Fn(&ModelRef) -> String,
) -> Vec<Pick> {
    let ids: BTreeMap<&str, TaskId> = delegated
        .tasks
        .iter()
        .enumerate()
        .map(|(n, task)| (task.key.trim(), TaskId::numbered(n as u32 + 1)))
        .collect();
    let mut picks = Vec::new();
    for assignment in &delegated.assignments {
        let Some(id) = ids.get(assignment.task.trim()) else {
            continue;
        };
        let (Some(task), Ok(worker)) = (
            tasks.iter_mut().find(|task| &task.id == id),
            assignment.worker.parse::<ModelRef>(),
        ) else {
            continue;
        };
        task.worker = Some(worker.clone());
        task.reviewers = assignment
            .reviewers
            .iter()
            .filter_map(|reviewer| reviewer.parse().ok())
            .collect();
        picks.push(Pick {
            task: task.id.clone(),
            algorithm: RoutingAlgorithm::Orchestrator,
            choice: Choice {
                worker,
                explored: false,
                reason: "the orchestrator's assignment".to_owned(),
            },
        });
    }

    let built: BTreeMap<TaskId, (ModelRef, Vec<ModelRef>)> = tasks
        .iter()
        .filter_map(|task| {
            Some((
                task.id.clone(),
                (task.worker.clone()?, task.reviewers.clone()),
            ))
        })
        .collect();
    let pool = roles::candidates(settings, RoleName::Reviewer);
    for (offset, task) in tasks.iter_mut().enumerate() {
        let TaskKind::AcceptanceTests { for_task } = &task.kind else {
            continue;
        };
        let Some((worker, reviewers)) = built.get(for_task) else {
            continue;
        };
        let Some(writer) = reviewers.first() else {
            continue;
        };
        task.worker = Some(writer.clone());
        let mut reviewers = test_reviewers(writer, worker, &pool, settings, provider, offset);
        let per_task = settings.review.per_task as usize;
        if reviewers.len() < per_task {
            // The writer is the build task's reviewer, often the only one allowed, so the agents
            // that may own tasks review its tests.
            let builders: Vec<ModelRef> = roles::candidates(settings, RoleName::Worker)
                .into_iter()
                .filter(|builder| !reviewers.contains(builder))
                .collect();
            let more = test_reviewers(writer, worker, &builders, settings, provider, offset);
            let short = per_task - reviewers.len();
            reviewers.extend(more.into_iter().take(short));
        }
        task.reviewers = reviewers;
        picks.push(Pick {
            task: task.id.clone(),
            algorithm: RoutingAlgorithm::Orchestrator,
            choice: Choice {
                worker: writer.clone(),
                explored: false,
                reason: format!("{for_task}'s reviewer writes its acceptance tests"),
            },
        });
    }
    picks
}

/// The ring around a test task's writer, never the writer or the tested task's worker.
fn test_reviewers(
    writer: &ModelRef,
    tested_worker: &ModelRef,
    pool: &[ModelRef],
    settings: &Settings,
    provider: &dyn Fn(&ModelRef) -> String,
    offset: usize,
) -> Vec<ModelRef> {
    let clear = |worker: &ModelRef, of: &ModelRef| {
        roles::conflict(RoleName::Reviewer, worker, Some(of), provider).is_none()
    };
    let candidates: Vec<(ModelRef, String)> = pool
        .iter()
        .filter(|worker| clear(worker, writer) && clear(worker, tested_worker))
        .map(|worker| (worker.clone(), provider(worker)))
        .collect();
    ring::reviewers(
        &provider(writer),
        &candidates,
        settings.review.per_task as usize,
        settings.review.other_provider,
        offset,
    )
}

/// Everyone who may own or review, best first by the router's ranking for the run's task type.
fn ranking(host: &Host, task_roles: &TaskRoles) -> Vec<ModelRef> {
    let run_task_type = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .and_then(|started| started.estimate.as_ref())
            .map(|estimate| estimate.task_type.clone())
    });
    let task_type = divide_assign::task_type(&TaskKind::Build, run_task_type.as_deref());
    let book = divide::track_records(host);
    let candidates: Vec<router::Candidate> = task_roles
        .listed()
        .into_iter()
        .map(|worker| divide::candidate(host, worker, &task_type, book.as_ref()))
        .collect();
    router::rank(&candidates)
}

/// The rules code checks the orchestrator's output against, as the orchestrator reads them.
pub fn rules(settings: &Settings) -> Vec<String> {
    let mut rules = vec![
        format!(
            "At most {} tasks, each with a unique key.",
            settings.divide.max_tasks
        ),
        "A task depends only on other tasks' keys, with no cycles.".to_owned(),
        "No two tasks own the same path, and every task owns at least one path.".to_owned(),
        format!(
            "No task owns a protected path: .argonauts/**, {}.",
            settings.limits.protected_paths.join(", ")
        ),
        "Every task has exactly one assignment, with a worker the crew list says may own tasks."
            .to_owned(),
        format!(
            "Every task has at least {} reviewers the crew list says may review, never its \
             worker{}.",
            settings.review.per_task,
            if settings.review.other_provider {
                ", each from another provider than the worker"
            } else {
                ""
            }
        ),
        "You can't raise a limit or approve anything.".to_owned(),
    ];
    if settings.divide.contracts_first {
        rules.push(
            "Exactly one task has kind interface: the stubs every build task codes against."
                .to_owned(),
        );
        if settings.divide.acceptance_tests_by == TestAuthor::Reviewer {
            rules.push(
                "Code adds an acceptance-test task for each build task, written by its first \
                 reviewer."
                    .to_owned(),
            );
        }
    }
    if let Some(limit) = task_limit(settings) {
        rules.push(format!(
            "Each task gets ${:.2} of the run's ${:.2}, so at most {limit} tasks fit.",
            settings.limits.spend.per_task_usd.unwrap_or_default(),
            settings.limits.spend.spend_per_run.unwrap_or_default(),
        ));
    }
    rules
}

/// How many tasks the run limit holds at the per-task limit, when both are in dollars.
fn task_limit(settings: &Settings) -> Option<usize> {
    let limit = &settings.limits.spend;
    match (limit.spend_per_run, limit.per_task_usd) {
        (Some(run), Some(task)) if task > 0.0 => Some((run / task).floor() as usize),
        _ => None,
    }
}

/// Every rule the output breaks, one line each. Empty means it stands. `providers` gives a
/// worker's model provider from the registry.
pub fn check(
    names: &DisplayNames,
    output: &OrchestrateOutput,
    settings: &Settings,
    providers: &dyn Fn(&ModelRef) -> String,
) -> Vec<String> {
    let task_roles = roles::tasks::settle(names, settings, Default::default(), providers);
    let mut broken = task_problems(&output.tasks, settings);
    broken.extend(assignment_problems(names, output, &task_roles, providers));
    broken
}

/// Every rule the tasks break, whoever they go to.
fn task_problems(tasks: &[TaskProposal], settings: &Settings) -> Vec<String> {
    let mut broken = Vec::new();
    check_tasks(tasks, settings, &mut broken);
    check_paths(tasks, settings, &mut broken);
    broken
}

fn check_tasks(tasks: &[TaskProposal], settings: &Settings, broken: &mut Vec<String>) {
    if tasks.is_empty() {
        broken.push("The plan has no tasks.".to_owned());
    }
    if tasks.len() > settings.divide.max_tasks as usize {
        broken.push(format!(
            "{} tasks is more than the {} allowed.",
            tasks.len(),
            settings.divide.max_tasks
        ));
    }
    if let Some(limit) = task_limit(settings)
        && tasks.len() > limit
    {
        broken.push(format!(
            "{} tasks don't fit the run limit, which holds {limit}.",
            tasks.len()
        ));
    }
    let mut keys = BTreeSet::new();
    for task in tasks {
        if task.key.trim().is_empty() {
            broken.push(format!("Task \"{}\" has an empty key.", task.title));
        } else if !keys.insert(task.key.as_str()) {
            broken.push(format!("The key {} is used twice.", task.key));
        }
    }
    for task in tasks {
        for dep in &task.depends_on {
            if dep == &task.key {
                broken.push(format!("Task {} depends on itself.", task.key));
            } else if !keys.contains(dep.as_str()) {
                broken.push(format!("Task {} depends on unknown task {dep}.", task.key));
            }
        }
    }
    if let Some(key) = first_cycle(tasks) {
        broken.push(format!("Task {key} is part of a dependency cycle."));
    }
}

fn first_cycle(tasks: &[TaskProposal]) -> Option<String> {
    let deps: BTreeMap<&str, Vec<&str>> = tasks
        .iter()
        .map(|c| {
            (
                c.key.as_str(),
                c.depends_on.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    fn visit<'a>(
        key: &'a str,
        deps: &BTreeMap<&'a str, Vec<&'a str>>,
        path: &mut Vec<&'a str>,
        done: &mut BTreeSet<&'a str>,
    ) -> Option<String> {
        if path.contains(&key) {
            return Some(key.to_owned());
        }
        if !done.insert(key) {
            return None;
        }
        path.push(key);
        for dep in deps.get(key).into_iter().flatten() {
            if *dep != key
                && deps.contains_key(dep)
                && let Some(cycle) = visit(dep, deps, path, done)
            {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }
    let mut done = BTreeSet::new();
    deps.keys()
        .find_map(|key| visit(key, &deps, &mut Vec::new(), &mut done))
}

fn check_paths(tasks: &[TaskProposal], settings: &Settings, broken: &mut Vec<String>) {
    let protected = protected_set(settings);
    let mut workers: Vec<(&str, String)> = Vec::new();
    for task in tasks {
        if task.owned_paths.is_empty() && tasks.len() > 1 {
            broken.push(format!("Task {} owns no path.", task.key));
        }
        for path in &task.owned_paths {
            let base = path_base(path);
            let probe = format!("{base}/file");
            if base.is_empty() || base.starts_with('/') || base.split('/').any(|c| c == "..") {
                broken.push(format!("Task {} owns an invalid path {path}.", task.key));
                continue;
            }
            if protected.is_match(&base) || protected.is_match(&probe) {
                broken.push(format!("Task {} owns protected path {path}.", task.key));
            }
            for (other, other_base) in &workers {
                if *other != task.key.as_str() && overlaps(&base, other_base) {
                    broken.push(format!("Tasks {other} and {} both own {path}.", task.key));
                }
            }
        }
        for path in &task.owned_paths {
            workers.push((task.key.as_str(), path_base(path)));
        }
    }
}

/// The literal part of a path or glob, without `./` or a trailing slash.
fn path_base(path: &str) -> String {
    let path = path.trim().strip_prefix("./").unwrap_or(path.trim());
    let literal = match path.find(['*', '?', '[', '{']) {
        Some(at) => &path[..at],
        None => path,
    };
    literal.trim_end_matches('/').to_owned()
}

fn overlaps(a: &str, b: &str) -> bool {
    let inside =
        |x: &str, dir: &str| dir.is_empty() || x == dir || x.starts_with(&format!("{dir}/"));
    inside(a, b) || inside(b, a)
}

fn protected_set(settings: &Settings) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    let always = [".argonauts/**".to_owned(), ".argonauts".to_owned()];
    for pattern in always.iter().chain(&settings.limits.protected_paths) {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Every rule the assignments break, in plain words with names: "agy can't own task
/// `tests`: only Claude Code and Codex may own tasks."
fn assignment_problems(
    names: &DisplayNames,
    output: &OrchestrateOutput,
    task_roles: &TaskRoles,
    providers: &dyn Fn(&ModelRef) -> String,
) -> Vec<String> {
    let mut broken = Vec::new();
    let keys: BTreeSet<&str> = output.tasks.iter().map(|c| c.key.as_str()).collect();
    let mut assigned: BTreeMap<&str, u32> = BTreeMap::new();
    for assignment in &output.assignments {
        let key = assignment.task.as_str();
        if !keys.contains(key) {
            broken.push(format!(
                "An assignment names task `{key}`, which the plan doesn't have."
            ));
            continue;
        }
        *assigned.entry(key).or_default() += 1;
        let Ok(worker) = assignment.worker.parse::<ModelRef>() else {
            broken.push(format!(
                "Task `{key}` names `{}` as its worker, which is no agent.",
                assignment.worker
            ));
            continue;
        };
        if !task_roles.may_own(&worker) {
            broken.push(format!(
                "{} can't own task `{key}`: {}.",
                names.worker(&worker),
                clause_case(&task_roles.why_not_worker(&worker))
            ));
        }
        let mut seen = BTreeSet::new();
        for text in &assignment.reviewers {
            let Ok(reviewer) = text.parse::<ModelRef>() else {
                broken.push(format!(
                    "Task `{key}` names `{text}` as a reviewer, which is no agent."
                ));
                continue;
            };
            let name = names.worker(&reviewer);
            if !seen.insert(reviewer.clone()) {
                broken.push(format!("{name} reviews task `{key}` twice."));
            } else if reviewer == worker {
                broken.push(format!("{name} would review its own task `{key}`."));
            } else if !task_roles
                .reviewers_for(&worker, providers)
                .contains(&reviewer)
            {
                broken.push(format!(
                    "{name} can't review task `{key}`: {}.",
                    task_roles.why_not_reviewer(&reviewer, &worker, providers)
                ));
            }
        }
        if (seen.len() as u32) < task_roles.per_task {
            broken.push(format!(
                "Task `{key}` has {} reviewer{}; it needs {}.",
                seen.len(),
                if seen.len() == 1 { "" } else { "s" },
                task_roles.per_task
            ));
        }
    }
    for key in &keys {
        match assigned.get(key).copied().unwrap_or(0) {
            0 => broken.push(format!("Task `{key}` has no assignment.")),
            1 => {}
            n => broken.push(format!("Task `{key}` has {n} assignments.")),
        }
    }
    broken
}

/// Gives each task one assignment the rules allow: the orchestrator's worker and reviewers where
/// they may take it, else whoever may, the least busy first and then the best by `ranked`.
/// Returns a line for each task whose worker or reviewers moved, saying who and why. A name the
/// crew list spells another way, such as `codex/gpt` for `codex`, is the same agent, not a move.
pub fn repair(
    names: &DisplayNames,
    output: &mut OrchestrateOutput,
    task_roles: &TaskRoles,
    ranked: &[ModelRef],
    provider: &dyn Fn(&ModelRef) -> String,
) -> Vec<String> {
    let rank = |model: &ModelRef| {
        ranked
            .iter()
            .position(|ranked| ranked == model)
            .unwrap_or(usize::MAX)
    };
    let pick = |pool: &[ModelRef], load: &BTreeMap<ModelRef, usize>| {
        pool.iter()
            .min_by_key(|model| (load.get(*model).copied().unwrap_or(0), rank(model)))
            .cloned()
    };
    let mut owning: BTreeMap<ModelRef, usize> = BTreeMap::new();
    let mut reviewing: BTreeMap<ModelRef, usize> = BTreeMap::new();
    let mut assignments = Vec::new();
    let mut moved = Vec::new();
    for task in &output.tasks {
        let key = task.key.trim();
        let given = output
            .assignments
            .iter()
            .find(|assignment| assignment.task.trim() == key);
        let named = given.and_then(|given| named_in(&given.worker, &task_roles.workers));
        let Some(worker) = named.clone().or_else(|| pick(&task_roles.workers, &owning)) else {
            assignments.extend(given.cloned());
            continue;
        };
        *owning.entry(worker.clone()).or_default() += 1;

        let allowed = task_roles.reviewers_for(&worker, provider);
        let mut reviewers: Vec<ModelRef> = Vec::new();
        for text in given
            .map(|given| given.reviewers.as_slice())
            .unwrap_or_default()
        {
            if let Some(reviewer) = named_in(text, &allowed)
                && !reviewers.contains(&reviewer)
            {
                reviewers.push(reviewer);
            }
        }
        while reviewers.len() < task_roles.per_task as usize {
            let open: Vec<ModelRef> = allowed
                .iter()
                .filter(|model| !reviewers.contains(model))
                .cloned()
                .collect();
            let Some(next) = pick(&open, &reviewing) else {
                break;
            };
            reviewers.push(next);
        }
        for reviewer in &reviewers {
            *reviewing.entry(reviewer.clone()).or_default() += 1;
        }

        let mut changes = Vec::new();
        if named.is_none() {
            let was = given.and_then(|given| task_roles.named(&given.worker));
            changes.push(match was {
                Some(was) => format!(
                    "{} owns it, not {}, since {}",
                    names.worker(&worker),
                    names.worker(&was),
                    clause_case(&task_roles.why_not_worker(&was))
                ),
                None => format!("{} owns it", names.worker(&worker)),
            });
        }
        let dropped: Vec<ModelRef> = given
            .map(|given| given.reviewers.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|text| named_in(text, &allowed).is_none())
            .filter_map(|text| task_roles.named(text))
            .collect();
        let added: Vec<ModelRef> = reviewers
            .iter()
            .filter(|reviewer| {
                !given
                    .map(|given| given.reviewers.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .any(|text| named_in(text, &allowed).as_ref() == Some(*reviewer))
            })
            .cloned()
            .collect();
        let reviews = |models: &[ModelRef]| {
            let verb = if models.len() == 1 {
                "reviews"
            } else {
                "review"
            };
            format!("{} {verb} it", roles::tasks::who(names, models))
        };
        match (dropped.first(), added.is_empty()) {
            (Some(first), false) => changes.push(format!(
                "{}, not {}, since {}",
                reviews(&added),
                roles::tasks::who(names, &dropped),
                task_roles.why_not_reviewer(first, &worker, provider)
            )),
            (Some(first), true) => changes.push(format!(
                "{} doesn't review it, since {}",
                roles::tasks::who(names, &dropped),
                task_roles.why_not_reviewer(first, &worker, provider)
            )),
            (None, false) => changes.push(reviews(&added)),
            (None, true) => {}
        }
        if !changes.is_empty() {
            moved.push(format!("`{key}`: {}.", changes.join("; ")));
        }
        assignments.push(Assignment {
            task: task.key.clone(),
            worker: worker.to_string(),
            reviewers: reviewers.iter().map(ToString::to_string).collect(),
        });
    }
    output.assignments = assignments;
    moved
}

/// One note for what [`repair`] moved: the line says how many tasks, and Ctrl+O shows each.
fn moved_note(host: &Host, moved: &[String]) -> EngineResult<()> {
    if moved.is_empty() {
        return Ok(());
    }
    let tasks = if moved.len() == 1 {
        "1 of the orchestrator's tasks".to_owned()
    } else {
        format!("{} of the orchestrator's tasks", moved.len())
    };
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!(
                "Code moved who owns or reviews {tasks} to agents your rules allow.\n{}",
                moved.join("\n")
            ),
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::workflows::steps::plan::{PlanDraft, ProposedKind};

    use super::*;

    fn provider(worker: &ModelRef) -> String {
        match worker.agent.as_str() {
            "claude-code" => "claude",
            "codex" | "opencode" => "gpt",
            _ => "gemini",
        }
        .to_owned()
    }

    fn settings() -> Settings {
        let mut settings = Settings {
            crew: ["claude-code", "codex", "antigravity-cli", "opencode"]
                .iter()
                .map(|w| w.parse().unwrap())
                .collect(),
            ..Settings::default()
        };
        settings.review.per_task = 2;
        settings
    }

    fn task(key: &str, paths: &[&str], deps: &[&str]) -> TaskProposal {
        TaskProposal {
            key: key.to_owned(),
            title: key.to_owned(),
            task: format!("build {key}"),
            kind: ProposedKind::Build,
            owned_paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            read_only_paths: Vec::new(),
            depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
            criteria: vec!["G1".to_owned()],
        }
    }

    fn assign(key: &str, worker: &str, reviewers: &[&str]) -> Assignment {
        Assignment {
            task: key.to_owned(),
            worker: worker.to_owned(),
            reviewers: reviewers.iter().map(|r| (*r).to_owned()).collect(),
        }
    }

    fn plan() -> PlanDraft {
        PlanDraft {
            summary: "two tasks".to_owned(),
            approach: String::new(),
            pieces: Vec::new(),
            choices: Vec::new(),
            risks: Vec::new(),
            test_strategy: String::new(),
            new_dependencies: Vec::new(),
        }
    }

    fn good() -> OrchestrateOutput {
        OrchestrateOutput {
            plan: plan(),
            tasks: vec![
                task("store", &["src/store/"], &[]),
                task("api", &["src/api.rs"], &["store"]),
            ],
            assignments: vec![
                assign("store", "claude-code", &["codex", "antigravity-cli"]),
                assign("api", "codex", &["claude-code", "antigravity-cli"]),
            ],
        }
    }

    fn broken(output: &OrchestrateOutput) -> Vec<String> {
        check(&DisplayNames::default(), output, &settings(), &provider)
    }

    /// A copy of the main session opens on its new role, in the words the orchestrator's own
    /// view shows first, and never on the conversation's.
    #[test]
    fn a_forked_orchestrator_is_told_first_that_it_no_longer_talks_to_you() {
        const PICTURE: &str = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tui/tests/golden/fork_start.txt"
        ));
        let opening = forked_opening();
        assert_eq!(opening.kind, InputPartKind::Prompt);
        let first = opening.text.lines().next().unwrap();
        assert!(
            PICTURE
                .lines()
                .any(|row| row.contains(&format!("│   {first} "))),
            "{first}"
        );
        assert!(opening.text.contains("You no longer talk to the user"));
    }

    #[test]
    fn a_plan_that_keeps_the_rules_stands() {
        assert_eq!(broken(&good()), Vec::<String>::new());
    }

    #[test]
    fn reviewers_of_the_workers_provider_or_the_worker_are_rejected() {
        let mut output = good();
        output.assignments[1] = assign("api", "codex", &["opencode", "codex"]);
        let broken = broken(&output);
        assert!(
            broken.contains(
                &"OpenCode can't review task `api`: OpenCode is from the provider of the task's \
                  worker, Codex."
                    .to_owned()
            ),
            "{broken:?}"
        );
        assert!(
            broken.contains(&"Codex would review its own task `api`.".to_owned()),
            "{broken:?}"
        );
    }

    #[test]
    fn overlapping_and_protected_paths_are_rejected() {
        let mut output = good();
        output.tasks[1].owned_paths = vec!["src/store/redis.rs".to_owned(), ".github/".to_owned()];
        let broken = broken(&output);
        assert!(
            broken
                .iter()
                .any(|b| b.contains("both own src/store/redis.rs")),
            "{broken:?}"
        );
        assert!(
            broken.iter().any(|b| b.contains("protected path .github/")),
            "{broken:?}"
        );
    }

    #[test]
    fn graph_errors_are_rejected() {
        let mut output = good();
        output.tasks[0].depends_on = vec!["api".to_owned()];
        output.tasks.push(task("web", &["src/web.rs"], &["ghost"]));
        output.tasks.push(task("web", &["src/web2.rs"], &[]));
        let broken = broken(&output);
        assert!(broken.iter().any(|b| b.contains("key web is used twice")));
        assert!(broken.iter().any(|b| b.contains("unknown task ghost")));
        assert!(broken.iter().any(|b| b.contains("dependency cycle")));
    }

    #[test]
    fn workers_outside_the_crew_and_missing_assignments_are_rejected() {
        let mut output = good();
        output.assignments[0] = assign("store", "stranger", &["codex", "antigravity-cli"]);
        output.assignments.pop();
        let broken = broken(&output);
        assert!(
            broken.contains(
                &"stranger can't own task `store`: only Claude Code, Codex, agy and \
                  OpenCode may own tasks."
                    .to_owned()
            ),
            "{broken:?}"
        );
        assert!(
            broken.contains(&"Task `api` has no assignment.".to_owned()),
            "{broken:?}"
        );
    }

    #[test]
    fn too_many_tasks_for_the_limit_are_rejected() {
        let mut output = good();
        for n in 0..6 {
            let key = format!("extra{n}");
            output
                .tasks
                .push(task(&key, &[&format!("src/extra{n}.rs")], &[]));
            output
                .assignments
                .push(assign(&key, "claude-code", &["codex", "antigravity-cli"]));
        }
        // With no limits set, any number of tasks fits.
        assert_eq!(broken(&output), Vec::<String>::new());
        assert!(!rules(&settings()).iter().any(|r| r.contains("tasks fit")));

        let mut limited = settings();
        limited.limits.spend.spend_per_run = Some(20.0);
        limited.limits.spend.per_task_usd = Some(3.0);
        assert_eq!(
            check(&DisplayNames::default(), &output, &limited, &provider),
            ["8 tasks don't fit the run limit, which holds 6."]
        );
        assert!(
            rules(&limited)
                .iter()
                .any(|r| r.contains("at most 6 tasks fit"))
        );
    }

    #[test]
    fn too_few_reviewers_are_rejected() {
        let mut output = good();
        output.assignments[0].reviewers.pop();
        assert_eq!(
            broken(&output),
            ["Task `store` has 1 reviewer; it needs 2."]
        );
    }

    /// The sample repo's run: Claude Code and Codex may own, agy alone reviews, and
    /// the orchestrator wrote each agent with its provider where a model goes and rotated them
    /// through every role.
    fn trial_roles() -> TaskRoles {
        let mut settings = Settings {
            crew: ["claude-code", "codex", "antigravity-cli"]
                .iter()
                .map(|w| w.parse().unwrap())
                .collect(),
            ..Settings::default()
        };
        settings.review.per_task = 1;
        let worker = |text: &str| text.parse::<ModelRef>().unwrap();
        settings.roles.entry(RoleName::Worker).or_default().models =
            vec![worker("claude-code"), worker("codex")];
        settings.roles.entry(RoleName::Reviewer).or_default().models =
            vec![worker("antigravity-cli")];
        roles::tasks::settle(
            &DisplayNames::default(),
            &settings,
            Default::default(),
            &provider,
        )
    }

    fn trial_output() -> OrchestrateOutput {
        let mut tasks = vec![
            task("iface", &["src/types.js"], &[]),
            task("impl", &["src/store.js"], &["iface"]),
            task("tests", &["test/"], &["impl"]),
            task("review", &["docs/"], &["impl"]),
        ];
        tasks[0].kind = ProposedKind::Interface;
        OrchestrateOutput {
            plan: plan(),
            tasks,
            assignments: vec![
                assign("iface", "codex/gpt", &["claude-code/claude"]),
                assign("impl", "claude-code/claude", &["antigravity-cli/gemini"]),
                assign("tests", "antigravity-cli/gemini", &["codex/gpt"]),
                assign("review", "antigravity-cli/gemini", &["claude-code/claude"]),
            ],
        }
    }

    #[test]
    fn assignments_that_break_the_rules_move_to_agents_that_may_take_them() {
        let task_roles = trial_roles();
        let mut output = trial_output();
        let ranked: Vec<ModelRef> = ["codex", "claude-code", "antigravity-cli"]
            .iter()
            .map(|w| w.parse().unwrap())
            .collect();
        let moved = repair(
            &DisplayNames::default(),
            &mut output,
            &task_roles,
            &ranked,
            &provider,
        );
        assert_eq!(
            assignment_problems(&DisplayNames::default(), &output, &task_roles, &provider),
            Vec::<String>::new()
        );
        let given: Vec<(&str, &str, Vec<&str>)> = output
            .assignments
            .iter()
            .map(|a| {
                let reviewers = a.reviewers.iter().map(String::as_str).collect();
                (a.task.as_str(), a.worker.as_str(), reviewers)
            })
            .collect();
        assert_eq!(
            given,
            [
                ("iface", "codex", vec!["antigravity-cli"]),
                ("impl", "claude-code", vec!["antigravity-cli"]),
                ("tests", "codex", vec!["antigravity-cli"]),
                ("review", "claude-code", vec!["antigravity-cli"]),
            ]
        );
        assert_eq!(
            moved,
            [
                "`iface`: agy reviews it, not Claude Code, since only agy \
                 may review tasks.",
                "`tests`: Codex owns it, not agy, since only Claude Code and Codex may \
                 own tasks; agy reviews it, not Codex, since only agy may \
                 review tasks.",
                "`review`: Claude Code owns it, not agy, since only Claude Code and \
                 Codex may own tasks; agy reviews it, not Claude Code, since only \
                 agy may review tasks.",
            ]
        );
        // The same plan moves the same way every time.
        let mut again = trial_output();
        assert_eq!(
            repair(
                &DisplayNames::default(),
                &mut again,
                &task_roles,
                &ranked,
                &provider
            ),
            moved
        );
        assert_eq!(again, output);
    }

    #[test]
    fn a_plan_that_keeps_the_rules_moves_nothing() {
        let task_roles = roles::tasks::settle(
            &DisplayNames::default(),
            &settings(),
            Default::default(),
            &provider,
        );
        let mut output = good();
        let ranked = task_roles.workers.clone();
        assert!(
            repair(
                &DisplayNames::default(),
                &mut output,
                &task_roles,
                &ranked,
                &provider
            )
            .is_empty()
        );
        assert_eq!(output, good());
    }
}
