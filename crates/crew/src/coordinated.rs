//! The coordinated crew: the coordinator runs `Coordinate` (plan, cards, owners, reviewers) in a
//! run-lifetime session, told first who may own and who may review. Code checks its cards
//! against the rules (owned paths, budget, the task graph); a broken rule is rejected with the
//! reason and retried once, then the plan checkpoint decides. When the cards are sound, an owner
//! or reviewer the rules don't allow is moved by code to one they do, by the router's ranking,
//! and one note says what moved. The coordinator can't raise a budget or approve anything.
//! Dividing the work takes its cards and assignments as they are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::plan::PlanEvent;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{RoutingAlgorithm, TestAuthor};
use argonauts_core::steps::plan::{
    Assignment, CardProposal, Coordinate, CoordinateInput, CoordinateOutput, TaskGraphProposal,
};
use argonauts_core::text::clause_case;
use argonauts_core::worker::Access;
use argonauts_core::{Note, NoteLevel, write_private_file};

use crate::checkpoints::autopilot;
use crate::divide::assign::{self as divide_assign, Pick};
use crate::divide::router::{self, Choice};
use crate::divide::{self, ring};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::plan::context::{self, families};
use crate::plan::drafts;
use crate::prompts::plan::COORDINATE;
use crate::roles::cards::{CardRoles, named_in};
use crate::roles::{self, display_name};
use crate::state::RunState;
use crate::state::plan::{self, CoordinatedRecord, SealedDraft};
use crate::workflow::{StepCall, StepDone};

/// A rejected answer is retried once; the second rejection goes to you.
const ATTEMPTS: usize = 2;

/// The coordinator's cards and assignments, sealed beside its plan for dividing the work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoordinatedCards {
    pub cards: Vec<CardProposal>,
    pub assignments: Vec<Assignment>,
}

/// The coordinator's plan broke the rules on every try since you last sent it back.
#[derive(Debug, Clone, PartialEq)]
pub struct Rejected {
    pub coordinator: WorkerRef,
    /// Why each try was rejected, oldest first.
    pub reasons: Vec<String>,
}

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    context::phase_started(host)?;
    let coordinator = context::workers_for(call, host.settings(), RoleName::Orchestrator, Some(1))
        .into_iter()
        .next()
        .unwrap_or_else(|| host.settings().main_agent.clone());
    let family = families(host);
    let card_roles = roles::cards::for_run(host);
    card_roles.announce(host)?;
    let settings = card_roles.apply(host.settings());
    loop {
        // A plan you sent back gets two new tries, in the coordinator's own session, with your
        // note. A settled plan sent back starts a new planning round.
        let (records, sent_back, replanning, asked, session) = host.read(|state| {
            (
                attempts(state),
                sent_back(state),
                plan::sent_back(state).is_some(),
                state.plan.coordinated.len(),
                state
                    .plan
                    .coordinated
                    .iter()
                    .rev()
                    .find_map(|r| r.session.clone()),
            )
        });
        if records.iter().any(|record| record.rejection.is_none()) {
            return Ok(StepDone::done("The orchestrator's plan follows the rules"));
        }
        if records.len() >= ATTEMPTS {
            let summary = if autopilot::applies_now(host, CheckpointKind::Plan) {
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
        let input = host.read(|state| CoordinateInput {
            goal: context::goal_brief(state),
            crew: card_roles.lines(&family),
            rules: rules(&settings),
            rejection,
        });
        let mut ask = Ask::new(
            Some(call.at.clone()),
            RoleName::Orchestrator,
            coordinator.clone(),
            COORDINATE,
            input,
        );
        ask.access = Access::ReadOnly;
        // Counted over the whole run, so an answer after a send-back never reuses a slot.
        ask.slot = asked as u32;
        ask.resume = session;
        ask.parts
            .push(context::status_part(host, RoleName::Orchestrator, None));
        let answered = match host.ask::<Coordinate>(ask).await {
            Ok(answered) => answered,
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                reject(
                    host,
                    &coordinator,
                    None,
                    format!("the answer failed: {error}"),
                )?;
                continue;
            }
        };
        let mut output = answered.output;
        let mut broken = card_problems(&output.cards, &settings);
        if broken.is_empty() {
            broken = graph_problems(host, &output.cards);
        }
        if broken.is_empty() {
            let moved = repair(
                &mut output,
                &card_roles,
                &ranking(host, &card_roles),
                &family,
            );
            moved_note(host, &moved)?;
            broken = assignment_problems(&output, &card_roles, &family);
        }
        if !broken.is_empty() {
            reject(
                host,
                &coordinator,
                Some(&answered.session),
                broken.join("; "),
            )?;
            continue;
        }
        let round = host.read(|state| state.plan.blind_round(replanning));
        let draft = drafts::seal(
            host,
            1,
            &coordinator,
            round,
            &output.plan,
            Some(&answered.session),
        )?;
        let sealed = host
            .read(|state| state.plan.drafts.iter().find(|d| d.draft == draft).cloned())
            .ok_or_else(|| EngineError::Refused(format!("draft {draft} was not recorded")))?;
        let cards = CoordinatedCards {
            cards: output.cards,
            assignments: output.assignments,
        };
        write_private_file(
            &cards_path(&sealed.path),
            &serde_json::to_vec_pretty(&cards)?,
        )?;
        host.record(
            None,
            Some(&answered.session),
            RunEventKind::Plan(PlanEvent::Coordinated {
                coordinator: coordinator.clone(),
                rejection: None,
            }),
        )?;
        let summary = format!(
            "The orchestrator planned {} task{}",
            cards.cards.len(),
            if cards.cards.len() == 1 { "" } else { "s" }
        );
        context::phase_finished(host, &summary)?;
        return Ok(StepDone::done(summary));
    }
}

/// What the task graph validator and contracts first say about the cards, which dividing the
/// work would otherwise find only after the coordinator is done.
fn graph_problems(host: &Host, cards: &[CardProposal]) -> Vec<String> {
    let proposal = TaskGraphProposal {
        cards: cards.to_vec(),
    };
    divide::build(host.run_id(), &proposal, host.settings())
        .err()
        .unwrap_or_default()
}

struct SentBack {
    /// The seq of the plan checkpoint that was answered.
    since: u64,
    note: Option<String>,
}

/// The last time you sent the plan back at the plan checkpoint.
fn sent_back(state: &RunState) -> Option<SentBack> {
    state
        .checkpoints
        .by_id
        .values()
        .filter(|record| record.opened.kind == CheckpointKind::Plan && record.withdrawn.is_none())
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

/// The coordinator's answers since you last sent its plan back.
fn attempts(state: &RunState) -> Vec<CoordinatedRecord> {
    let since = sent_back(state).map(|sent| sent.since);
    state
        .plan
        .coordinated
        .iter()
        .filter(|record| since.is_none_or(|since| record.seq > since))
        .cloned()
        .collect()
}

/// Why the coordinator's plan was rejected, once it has used every try since you last sent it
/// back without an answer that stands.
pub fn rejected(state: &RunState) -> Option<Rejected> {
    let attempts = attempts(state);
    if attempts.len() < ATTEMPTS || attempts.iter().any(|record| record.rejection.is_none()) {
        return None;
    }
    Some(Rejected {
        coordinator: attempts.last()?.coordinator.clone(),
        reasons: attempts
            .into_iter()
            .filter_map(|record| record.rejection)
            .collect(),
    })
}

fn reject(
    host: &Host,
    coordinator: &WorkerRef,
    session: Option<&SessionKey>,
    reason: String,
) -> EngineResult<()> {
    host.record(
        None,
        session,
        RunEventKind::Plan(PlanEvent::Coordinated {
            coordinator: coordinator.clone(),
            rejection: Some(reason),
        }),
    )?;
    Ok(())
}

fn cards_path(plan: &Path) -> PathBuf {
    plan.with_extension("cards.json")
}

/// The accepted coordinator's cards and assignments, for dividing the work.
pub fn load_cards(sealed: &SealedDraft) -> EngineResult<CoordinatedCards> {
    let bytes = std::fs::read(cards_path(&sealed.path))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Puts the coordinator's owners and reviewers on the cards built from its proposal, where the
/// proposal's n-th card became `card-00n`. Each acceptance-test card goes to its build card's
/// first reviewer, with a review ring of its own. A card left without an owner is the router's.
pub fn delegate(
    cards: &mut [CardSpec],
    delegated: &CoordinatedCards,
    settings: &Settings,
    family: &dyn Fn(&WorkerRef) -> String,
) -> Vec<Pick> {
    let ids: BTreeMap<&str, CardId> = delegated
        .cards
        .iter()
        .enumerate()
        .map(|(n, card)| (card.key.trim(), CardId::numbered(n as u32 + 1)))
        .collect();
    let mut picks = Vec::new();
    for assignment in &delegated.assignments {
        let Some(id) = ids.get(assignment.card.trim()) else {
            continue;
        };
        let (Some(card), Ok(owner)) = (
            cards.iter_mut().find(|card| &card.id == id),
            assignment.owner.parse::<WorkerRef>(),
        ) else {
            continue;
        };
        card.owner = Some(owner.clone());
        card.reviewers = assignment
            .reviewers
            .iter()
            .filter_map(|reviewer| reviewer.parse().ok())
            .collect();
        picks.push(Pick {
            card: card.id.clone(),
            algorithm: RoutingAlgorithm::Orchestrator,
            choice: Choice {
                worker: owner,
                explored: false,
                reason: "the orchestrator's assignment".to_owned(),
            },
        });
    }

    let built: BTreeMap<CardId, (WorkerRef, Vec<WorkerRef>)> = cards
        .iter()
        .filter_map(|card| {
            Some((
                card.id.clone(),
                (card.owner.clone()?, card.reviewers.clone()),
            ))
        })
        .collect();
    let pool = roles::candidates(settings, RoleName::Reviewer);
    for (offset, card) in cards.iter_mut().enumerate() {
        let CardKind::AcceptanceTests { for_card } = &card.kind else {
            continue;
        };
        let Some((owner, reviewers)) = built.get(for_card) else {
            continue;
        };
        let Some(writer) = reviewers.first() else {
            continue;
        };
        card.owner = Some(writer.clone());
        let mut reviewers = test_reviewers(writer, owner, &pool, settings, family, offset);
        let per_task = settings.review.per_task as usize;
        if reviewers.len() < per_task {
            // The writer is the build card's reviewer, often the only one allowed, so the agents
            // that may own cards review its tests.
            let builders: Vec<WorkerRef> = roles::candidates(settings, RoleName::Worker)
                .into_iter()
                .filter(|builder| !reviewers.contains(builder))
                .collect();
            let more = test_reviewers(writer, owner, &builders, settings, family, offset);
            let short = per_task - reviewers.len();
            reviewers.extend(more.into_iter().take(short));
        }
        card.reviewers = reviewers;
        picks.push(Pick {
            card: card.id.clone(),
            algorithm: RoutingAlgorithm::Orchestrator,
            choice: Choice {
                worker: writer.clone(),
                explored: false,
                reason: format!("{for_card}'s reviewer writes its acceptance tests"),
            },
        });
    }
    picks
}

/// The ring around a test card's writer, never the writer or the tested card's owner.
fn test_reviewers(
    writer: &WorkerRef,
    tested_owner: &WorkerRef,
    pool: &[WorkerRef],
    settings: &Settings,
    family: &dyn Fn(&WorkerRef) -> String,
    offset: usize,
) -> Vec<WorkerRef> {
    let clear = |worker: &WorkerRef, of: &WorkerRef| {
        roles::conflict(RoleName::Reviewer, worker, Some(of), family).is_none()
    };
    let candidates: Vec<(WorkerRef, String)> = pool
        .iter()
        .filter(|worker| clear(worker, writer) && clear(worker, tested_owner))
        .map(|worker| (worker.clone(), family(worker)))
        .collect();
    ring::reviewers(
        &family(writer),
        &candidates,
        settings.review.per_task as usize,
        settings.review.other_provider,
        offset,
    )
}

/// Everyone who may own or review, best first by the router's ranking for the run's task type.
fn ranking(host: &Host, card_roles: &CardRoles) -> Vec<WorkerRef> {
    let run_task_type = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .and_then(|started| started.estimate.as_ref())
            .map(|estimate| estimate.task_type.clone())
    });
    let task_type = divide_assign::task_type(&CardKind::Build, run_task_type.as_deref());
    let book = divide::track_records(host);
    let candidates: Vec<router::Candidate> = card_roles
        .listed()
        .into_iter()
        .map(|worker| divide::candidate(host, worker, &task_type, book.as_ref()))
        .collect();
    router::rank(&candidates)
}

/// The rules code checks the coordinator's output against, as the coordinator reads them.
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
    if let Some(limit) = card_limit(settings) {
        rules.push(format!(
            "Each task gets ${:.2} of the run's ${:.2}, so at most {limit} tasks fit.",
            settings.limits.budget.per_task_usd.unwrap_or_default(),
            settings.limits.budget.spend_per_run.unwrap_or_default(),
        ));
    }
    rules
}

/// How many cards the run budget holds at the per-card budget, when both are in dollars.
fn card_limit(settings: &Settings) -> Option<usize> {
    let budget = &settings.limits.budget;
    match (budget.spend_per_run, budget.per_task_usd) {
        (Some(run), Some(card)) if card > 0.0 => Some((run / card).floor() as usize),
        _ => None,
    }
}

/// Every rule the output breaks, one line each. Empty means it stands. `families` gives a
/// worker's model family from the registry.
pub fn check(
    output: &CoordinateOutput,
    settings: &Settings,
    families: &dyn Fn(&WorkerRef) -> String,
) -> Vec<String> {
    let card_roles = roles::cards::settle(settings, Default::default(), families);
    let mut broken = card_problems(&output.cards, settings);
    broken.extend(assignment_problems(output, &card_roles, families));
    broken
}

/// Every rule the cards break, whoever they go to.
fn card_problems(cards: &[CardProposal], settings: &Settings) -> Vec<String> {
    let mut broken = Vec::new();
    check_cards(cards, settings, &mut broken);
    check_paths(cards, settings, &mut broken);
    broken
}

fn check_cards(cards: &[CardProposal], settings: &Settings, broken: &mut Vec<String>) {
    if cards.is_empty() {
        broken.push("The plan has no tasks.".to_owned());
    }
    if cards.len() > settings.divide.max_tasks as usize {
        broken.push(format!(
            "{} tasks is more than the {} allowed.",
            cards.len(),
            settings.divide.max_tasks
        ));
    }
    if let Some(limit) = card_limit(settings)
        && cards.len() > limit
    {
        broken.push(format!(
            "{} tasks don't fit the run limit, which holds {limit}.",
            cards.len()
        ));
    }
    let mut keys = BTreeSet::new();
    for card in cards {
        if card.key.trim().is_empty() {
            broken.push(format!("Task \"{}\" has an empty key.", card.title));
        } else if !keys.insert(card.key.as_str()) {
            broken.push(format!("The key {} is used twice.", card.key));
        }
    }
    for card in cards {
        for dep in &card.depends_on {
            if dep == &card.key {
                broken.push(format!("Task {} depends on itself.", card.key));
            } else if !keys.contains(dep.as_str()) {
                broken.push(format!("Task {} depends on unknown task {dep}.", card.key));
            }
        }
    }
    if let Some(key) = first_cycle(cards) {
        broken.push(format!("Task {key} is part of a dependency cycle."));
    }
}

fn first_cycle(cards: &[CardProposal]) -> Option<String> {
    let deps: BTreeMap<&str, Vec<&str>> = cards
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

fn check_paths(cards: &[CardProposal], settings: &Settings, broken: &mut Vec<String>) {
    let protected = protected_set(settings);
    let mut owners: Vec<(&str, String)> = Vec::new();
    for card in cards {
        if card.owned_paths.is_empty() && cards.len() > 1 {
            broken.push(format!("Task {} owns no path.", card.key));
        }
        for path in &card.owned_paths {
            let base = path_base(path);
            let probe = format!("{base}/file");
            if base.is_empty() || base.starts_with('/') || base.split('/').any(|c| c == "..") {
                broken.push(format!("Task {} owns an invalid path {path}.", card.key));
                continue;
            }
            if protected.is_match(&base) || protected.is_match(&probe) {
                broken.push(format!("Task {} owns protected path {path}.", card.key));
            }
            for (other, other_base) in &owners {
                if *other != card.key.as_str() && overlaps(&base, other_base) {
                    broken.push(format!("Tasks {other} and {} both own {path}.", card.key));
                }
            }
        }
        for path in &card.owned_paths {
            owners.push((card.key.as_str(), path_base(path)));
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

/// Every rule the assignments break, in plain words with names: "Antigravity CLI can't own card
/// `tests`: only Claude Code and Codex may own cards."
fn assignment_problems(
    output: &CoordinateOutput,
    card_roles: &CardRoles,
    families: &dyn Fn(&WorkerRef) -> String,
) -> Vec<String> {
    let mut broken = Vec::new();
    let keys: BTreeSet<&str> = output.cards.iter().map(|c| c.key.as_str()).collect();
    let mut assigned: BTreeMap<&str, u32> = BTreeMap::new();
    for assignment in &output.assignments {
        let key = assignment.card.as_str();
        if !keys.contains(key) {
            broken.push(format!(
                "An assignment names task `{key}`, which the plan doesn't have."
            ));
            continue;
        }
        *assigned.entry(key).or_default() += 1;
        let Ok(owner) = assignment.owner.parse::<WorkerRef>() else {
            broken.push(format!(
                "Task `{key}` names `{}` as its worker, which is no agent.",
                assignment.owner
            ));
            continue;
        };
        if !card_roles.may_own(&owner) {
            broken.push(format!(
                "{} can't own task `{key}`: {}.",
                display_name(&owner),
                clause_case(&card_roles.why_not_owner(&owner))
            ));
        }
        let mut seen = BTreeSet::new();
        for text in &assignment.reviewers {
            let Ok(reviewer) = text.parse::<WorkerRef>() else {
                broken.push(format!(
                    "Task `{key}` names `{text}` as a reviewer, which is no agent."
                ));
                continue;
            };
            let name = display_name(&reviewer);
            if !seen.insert(reviewer.clone()) {
                broken.push(format!("{name} reviews task `{key}` twice."));
            } else if reviewer == owner {
                broken.push(format!("{name} would review its own task `{key}`."));
            } else if !card_roles
                .reviewers_for(&owner, families)
                .contains(&reviewer)
            {
                broken.push(format!(
                    "{name} can't review task `{key}`: {}.",
                    card_roles.why_not_reviewer(&reviewer, &owner, families)
                ));
            }
        }
        if (seen.len() as u32) < card_roles.per_task {
            broken.push(format!(
                "Task `{key}` has {} reviewer{}; it needs {}.",
                seen.len(),
                if seen.len() == 1 { "" } else { "s" },
                card_roles.per_task
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

/// Gives each card one assignment the rules allow: the coordinator's owner and reviewers where
/// they may take it, else whoever may, the least busy first and then the best by `ranked`.
/// Returns a line for each card whose owner or reviewers moved, saying who and why. A name the
/// crew list spells another way, such as `codex/gpt` for `codex`, is the same agent, not a move.
pub fn repair(
    output: &mut CoordinateOutput,
    card_roles: &CardRoles,
    ranked: &[WorkerRef],
    family: &dyn Fn(&WorkerRef) -> String,
) -> Vec<String> {
    let rank = |worker: &WorkerRef| {
        ranked
            .iter()
            .position(|ranked| ranked == worker)
            .unwrap_or(usize::MAX)
    };
    let pick = |pool: &[WorkerRef], load: &BTreeMap<WorkerRef, usize>| {
        pool.iter()
            .min_by_key(|worker| (load.get(*worker).copied().unwrap_or(0), rank(worker)))
            .cloned()
    };
    let mut owning: BTreeMap<WorkerRef, usize> = BTreeMap::new();
    let mut reviewing: BTreeMap<WorkerRef, usize> = BTreeMap::new();
    let mut assignments = Vec::new();
    let mut moved = Vec::new();
    for card in &output.cards {
        let key = card.key.trim();
        let given = output
            .assignments
            .iter()
            .find(|assignment| assignment.card.trim() == key);
        let named = given.and_then(|given| named_in(&given.owner, &card_roles.owners));
        let Some(owner) = named.clone().or_else(|| pick(&card_roles.owners, &owning)) else {
            assignments.extend(given.cloned());
            continue;
        };
        *owning.entry(owner.clone()).or_default() += 1;

        let allowed = card_roles.reviewers_for(&owner, family);
        let mut reviewers: Vec<WorkerRef> = Vec::new();
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
        while reviewers.len() < card_roles.per_task as usize {
            let open: Vec<WorkerRef> = allowed
                .iter()
                .filter(|worker| !reviewers.contains(worker))
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
            let was = given.and_then(|given| card_roles.named(&given.owner));
            changes.push(match was {
                Some(was) => format!(
                    "{} owns it, not {}, since {}",
                    display_name(&owner),
                    display_name(&was),
                    clause_case(&card_roles.why_not_owner(&was))
                ),
                None => format!("{} owns it", display_name(&owner)),
            });
        }
        let dropped: Vec<WorkerRef> = given
            .map(|given| given.reviewers.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|text| named_in(text, &allowed).is_none())
            .filter_map(|text| card_roles.named(text))
            .collect();
        let added: Vec<WorkerRef> = reviewers
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
        let reviews = |workers: &[WorkerRef]| {
            let verb = if workers.len() == 1 {
                "reviews"
            } else {
                "review"
            };
            format!("{} {verb} it", roles::cards::names(workers))
        };
        match (dropped.first(), added.is_empty()) {
            (Some(first), false) => changes.push(format!(
                "{}, not {}, since {}",
                reviews(&added),
                roles::cards::names(&dropped),
                card_roles.why_not_reviewer(first, &owner, family)
            )),
            (Some(first), true) => changes.push(format!(
                "{} doesn't review it, since {}",
                roles::cards::names(&dropped),
                card_roles.why_not_reviewer(first, &owner, family)
            )),
            (None, false) => changes.push(reviews(&added)),
            (None, true) => {}
        }
        if !changes.is_empty() {
            moved.push(format!("`{key}`: {}.", changes.join("; ")));
        }
        assignments.push(Assignment {
            card: card.key.clone(),
            owner: owner.to_string(),
            reviewers: reviewers.iter().map(ToString::to_string).collect(),
        });
    }
    output.assignments = assignments;
    moved
}

/// One note for what [`repair`] moved: the line says how many cards, and Ctrl+O shows each.
fn moved_note(host: &Host, moved: &[String]) -> EngineResult<()> {
    if moved.is_empty() {
        return Ok(());
    }
    let cards = if moved.len() == 1 {
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
                "Code moved who owns or reviews {cards} to agents your rules allow.\n{}",
                moved.join("\n")
            ),
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::{PlanDraft, ProposedKind};

    use super::*;

    fn family(worker: &WorkerRef) -> String {
        match worker.worker.as_str() {
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

    fn card(key: &str, paths: &[&str], deps: &[&str]) -> CardProposal {
        CardProposal {
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

    fn assign(key: &str, owner: &str, reviewers: &[&str]) -> Assignment {
        Assignment {
            card: key.to_owned(),
            owner: owner.to_owned(),
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

    fn good() -> CoordinateOutput {
        CoordinateOutput {
            plan: plan(),
            cards: vec![
                card("store", &["src/store/"], &[]),
                card("api", &["src/api.rs"], &["store"]),
            ],
            assignments: vec![
                assign("store", "claude-code", &["codex", "antigravity-cli"]),
                assign("api", "codex", &["claude-code", "antigravity-cli"]),
            ],
        }
    }

    fn broken(output: &CoordinateOutput) -> Vec<String> {
        check(output, &settings(), &family)
    }

    #[test]
    fn a_plan_that_keeps_the_rules_stands() {
        assert_eq!(broken(&good()), Vec::<String>::new());
    }

    #[test]
    fn reviewers_of_the_owners_family_or_the_owner_are_rejected() {
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
        output.cards[1].owned_paths = vec!["src/store/redis.rs".to_owned(), ".github/".to_owned()];
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
        output.cards[0].depends_on = vec!["api".to_owned()];
        output.cards.push(card("web", &["src/web.rs"], &["ghost"]));
        output.cards.push(card("web", &["src/web2.rs"], &[]));
        let broken = broken(&output);
        assert!(broken.iter().any(|b| b.contains("key web is used twice")));
        assert!(broken.iter().any(|b| b.contains("unknown task ghost")));
        assert!(broken.iter().any(|b| b.contains("dependency cycle")));
    }

    #[test]
    fn owners_outside_the_crew_and_missing_assignments_are_rejected() {
        let mut output = good();
        output.assignments[0] = assign("store", "stranger", &["codex", "antigravity-cli"]);
        output.assignments.pop();
        let broken = broken(&output);
        assert!(
            broken.contains(
                &"stranger can't own task `store`: only Claude Code, Codex, Antigravity CLI and \
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
    fn too_many_cards_for_the_budget_are_rejected() {
        let mut output = good();
        for n in 0..6 {
            let key = format!("extra{n}");
            output
                .cards
                .push(card(&key, &[&format!("src/extra{n}.rs")], &[]));
            output
                .assignments
                .push(assign(&key, "claude-code", &["codex", "antigravity-cli"]));
        }
        // With no limits set, any number of tasks fits.
        assert_eq!(broken(&output), Vec::<String>::new());
        assert!(!rules(&settings()).iter().any(|r| r.contains("tasks fit")));

        let mut limited = settings();
        limited.limits.budget.spend_per_run = Some(20.0);
        limited.limits.budget.per_task_usd = Some(3.0);
        assert_eq!(
            check(&output, &limited, &family),
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

    /// The sample repo's run: Claude Code and Codex may own, Antigravity CLI alone reviews, and
    /// the coordinator wrote each agent with its family where a model goes and rotated them
    /// through every role.
    fn trial_roles() -> CardRoles {
        let mut settings = Settings {
            crew: ["claude-code", "codex", "antigravity-cli"]
                .iter()
                .map(|w| w.parse().unwrap())
                .collect(),
            ..Settings::default()
        };
        settings.review.per_task = 1;
        let worker = |text: &str| text.parse::<WorkerRef>().unwrap();
        settings.roles.entry(RoleName::Worker).or_default().workers =
            vec![worker("claude-code"), worker("codex")];
        settings
            .roles
            .entry(RoleName::Reviewer)
            .or_default()
            .workers = vec![worker("antigravity-cli")];
        roles::cards::settle(&settings, Default::default(), &family)
    }

    fn trial_output() -> CoordinateOutput {
        let mut cards = vec![
            card("iface", &["src/types.js"], &[]),
            card("impl", &["src/store.js"], &["iface"]),
            card("tests", &["test/"], &["impl"]),
            card("review", &["docs/"], &["impl"]),
        ];
        cards[0].kind = ProposedKind::Interface;
        CoordinateOutput {
            plan: plan(),
            cards,
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
        let card_roles = trial_roles();
        let mut output = trial_output();
        let ranked: Vec<WorkerRef> = ["codex", "claude-code", "antigravity-cli"]
            .iter()
            .map(|w| w.parse().unwrap())
            .collect();
        let moved = repair(&mut output, &card_roles, &ranked, &family);
        assert_eq!(
            assignment_problems(&output, &card_roles, &family),
            Vec::<String>::new()
        );
        let given: Vec<(&str, &str, Vec<&str>)> = output
            .assignments
            .iter()
            .map(|a| {
                let reviewers = a.reviewers.iter().map(String::as_str).collect();
                (a.card.as_str(), a.owner.as_str(), reviewers)
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
                "`iface`: Antigravity CLI reviews it, not Claude Code, since only Antigravity CLI \
                 may review tasks.",
                "`tests`: Codex owns it, not Antigravity CLI, since only Claude Code and Codex may \
                 own tasks; Antigravity CLI reviews it, not Codex, since only Antigravity CLI may \
                 review tasks.",
                "`review`: Claude Code owns it, not Antigravity CLI, since only Claude Code and \
                 Codex may own tasks; Antigravity CLI reviews it, not Claude Code, since only \
                 Antigravity CLI may review tasks.",
            ]
        );
        // The same plan moves the same way every time.
        let mut again = trial_output();
        assert_eq!(repair(&mut again, &card_roles, &ranked, &family), moved);
        assert_eq!(again, output);
    }

    #[test]
    fn a_plan_that_keeps_the_rules_moves_nothing() {
        let card_roles = roles::cards::settle(&settings(), Default::default(), &family);
        let mut output = good();
        let ranked = card_roles.owners.clone();
        assert!(repair(&mut output, &card_roles, &ranked, &family).is_empty());
        assert_eq!(output, good());
    }
}
