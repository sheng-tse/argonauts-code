//! Dividing the work: the chosen plan becomes cards (`PlanToCards`), which code validates; then
//! contracts first, owners by the router, the review ring, predictions, budget lines, and new
//! dependencies. A coordinator's plan comes with its cards, owners, and reviewers, so only what
//! it left open is routed. The plan checkpoint shows all of it together.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use futures::future::join_all;
use rand::SeedableRng;
use rand::rngs::StdRng;

use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::events::checkpoint::{AnswerAction, CheckpointKind};
use argonauts_core::events::plan::{BudgetPlan, DivideEvent, TaskGraph};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::limits::{HealthState, QuotaStatus};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::protocol::TestAuthor;
use argonauts_core::steps::plan::{
    GoalBrief, PlanDraft, PlanToCards, PlanToCardsInput, TaskGraphProposal,
};
use argonauts_core::{CardId, Note, NoteLevel, RunId};

use argonauts_learning::Store;
use argonauts_learning::records::Book;

use crate::coordinated::{self, CoordinatedCards, Rejected};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::retro::outcomes;
use crate::workflow::{StepCall, StepDone};
use crate::{lead, prompts, roles};

pub mod assign;
pub mod budget_lines;
pub mod contracts;
pub mod dependencies;
pub mod edits;
pub mod graph;
pub mod paths;
pub mod pick;
pub mod plan_checkpoint;
pub mod plan_facts;
pub mod predictions;
pub mod ring;
pub mod router;
pub mod workflow_plan;

/// The size of a typical card session, for comparing registry cost estimates.
const CARD_INPUT_TOKENS: u64 = 400_000;
const CARD_OUTPUT_TOKENS: u64 = 40_000;

/// `checkpoint: plan`: divide, then open the plan checkpoint and act on your answer.
pub async fn plan_checkpoint_step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if !host.read(|state| workflow_plan::drafts_plan(state, &call.at.step.workflow)) {
        return workflow_plan::run(host, call).await;
    }
    plan_checkpoint::run(host, call).await
}

/// The validated task graph with owners, reviewers, predictions, and budget lines, recorded as
/// `Divide` events. Retried once with the reasons when the proposal breaks a rule.
pub async fn divide(host: &Host, call: &StepCall) -> EngineResult<TaskGraph> {
    let division = division(host, call).await?;
    if division.broken.is_empty() {
        Ok(division.graph)
    } else {
        Err(EngineError::Refused(division.broken.join(" ")))
    }
}

/// Everything the plan checkpoint shows.
#[derive(Debug, Clone, PartialEq)]
pub struct Division {
    pub plan: PlanDraft,
    /// The sealed plan as the judges saw it, for the evidence packet.
    pub plan_text: String,
    pub plan_author: WorkerRef,
    /// Who turned the plan into cards.
    pub divided_by: WorkerRef,
    pub graph: TaskGraph,
    pub budget: BudgetPlan,
    /// Rules the graph still breaks after the retry. Any one blocks approval, and nothing was
    /// recorded.
    pub broken: Vec<String>,
}

/// Divides the chosen plan, or finds the division already recorded for it. A coordinator's
/// plan that broke the rules twice has nothing to divide; its reasons block approval.
pub async fn division(host: &Host, call: &StepCall) -> EngineResult<Division> {
    if let Some(rejected) = host.read(coordinated::rejected) {
        return Ok(rejected_division(host.settings(), rejected));
    }
    let chosen = chosen_plan(host)?;
    let divided_by = if chosen.delegated.is_some() {
        chosen.author.clone()
    } else {
        divider(host)
    };
    if let Some((graph, budget)) = current(host) {
        return Ok(Division {
            plan: chosen.plan,
            plan_text: chosen.text,
            plan_author: chosen.author,
            divided_by,
            graph,
            budget,
            broken: Vec::new(),
        });
    }
    let settings = host.settings();
    let (cards, mut broken) = match &chosen.delegated {
        Some(delegated) => {
            let proposal = TaskGraphProposal {
                cards: delegated.cards.clone(),
            };
            match build(host.run_id(), &proposal, settings) {
                Ok(cards) => (cards, Vec::new()),
                Err(reasons) => (graph::convert(&proposal).0, reasons),
            }
        }
        None => {
            let input = PlanToCardsInput {
                goal: goal_brief(host)?,
                plan: chosen.plan.clone(),
                decisions: decisions(host),
                forbidden_paths: forbidden_paths(settings),
                max_cards: settings.divide.max_cards,
            };
            let first = ask_cards(host, call, &divided_by, input.clone(), &[]).await?;
            match build(host.run_id(), &first, settings) {
                Ok(cards) => (cards, Vec::new()),
                Err(reasons) => {
                    let second = ask_cards(host, call, &divided_by, input, &reasons).await?;
                    match build(host.run_id(), &second, settings) {
                        Ok(cards) => (cards, Vec::new()),
                        Err(reasons) => (graph::convert(&second).0, reasons),
                    }
                }
            }
        }
    };
    let budget = match budget_lines::plan(&settings.budget) {
        Ok(budget) => budget,
        Err(reason) => {
            broken.push(reason);
            empty_budget()
        }
    };
    if !broken.is_empty() {
        return Ok(Division {
            plan: chosen.plan,
            plan_text: chosen.text,
            plan_author: chosen.author,
            divided_by,
            graph: TaskGraph {
                critical_path: lead::critical_path(&cards),
                cards,
            },
            budget,
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
    let cards = with_budgets(host, cards, run_task_type.as_deref())?;
    let book = track_records(host);
    let protected = forbidden_paths(settings);
    let assignment = {
        let owners = |task_type: &str| {
            roles::candidates(settings, RoleName::Owner)
                .into_iter()
                .map(|worker| candidate(host, worker, task_type, book.as_ref()))
                .collect()
        };
        let family = |worker: &WorkerRef| roles::family(host, worker);
        let crew = assign::Crew {
            settings,
            labels: &labels,
            run_task_type: run_task_type.as_deref(),
            owners: &owners,
            family: &family,
            protected: &protected,
        };
        let mut rng = StdRng::seed_from_u64(seed(host.run_id()));
        match &chosen.delegated {
            Some(delegated) => delegated_assignment(cards, delegated, &crew, &mut rng),
            None => routed(cards, &crew, &mut rng),
        }
    };
    let mut cards = assignment.cards;
    log_draws(host, &assignment.draws)?;
    if settings.divide.predictions {
        predict(host, call, &mut cards).await?;
        predictions::log(host, &cards, run_task_type.as_deref())?;
    }

    for pick in &assignment.picks {
        let event = DivideEvent::Owner {
            card: pick.card.clone(),
            worker: pick.choice.worker.clone(),
            algorithm: pick.algorithm,
            explored: pick.choice.explored,
            reason: pick.choice.reason.clone(),
        };
        host.record(Some(&pick.card), None, RunEventKind::Divide(event))?;
    }
    for card in cards.iter().filter(|card| card.owner.is_some()) {
        let event = DivideEvent::Reviewers {
            card: card.id.clone(),
            reviewers: card.reviewers.clone(),
            blind: settings.review.blind,
        };
        host.record(Some(&card.id), None, RunEventKind::Divide(event))?;
        if let Some(predictions) = card.predictions {
            let event = DivideEvent::Predicted {
                card: card.id.clone(),
                predictions,
            };
            host.record(Some(&card.id), None, RunEventKind::Divide(event))?;
        }
    }
    host.record(
        None,
        None,
        RunEventKind::Divide(DivideEvent::Budget(budget.clone())),
    )?;
    let graph = TaskGraph {
        critical_path: lead::critical_path(&cards),
        cards,
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
        budget,
        broken: Vec::new(),
    })
}

/// Every owner predicts its card at once. Predictions only feed the retro, so one that fails is
/// left out with a note.
async fn predict(host: &Host, call: &StepCall, cards: &mut [CardSpec]) -> EngineResult<()> {
    let asks = cards
        .iter()
        .enumerate()
        .filter(|(_, card)| card.owner.is_some())
        .map(|(index, card)| async move {
            let at = Some(call.at.clone());
            (index, predictions::predict(host, at, card).await)
        });
    for (index, answer) in join_all(asks.collect::<Vec<_>>()).await {
        match answer {
            Ok(predicted) => cards[index].predictions = Some(predicted),
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            Err(error) => {
                host.record(
                    Some(&cards[index].id),
                    None,
                    RunEventKind::Note(Note {
                        level: NoteLevel::Warn,
                        text: format!("{}'s prediction is left out: {error}", cards[index].id),
                    }),
                )?;
            }
        }
    }
    Ok(())
}

/// Owners and reviewers by the router and the review ring, each pick named by the algorithm
/// that made it.
fn routed(cards: Vec<CardSpec>, crew: &assign::Crew, rng: &mut StdRng) -> assign::Assignment {
    let mut assignment = assign::assign(cards, crew, rng);
    for pick in &mut assignment.picks {
        pick.algorithm = router::effective(pick.algorithm);
    }
    assignment
}

/// The coordinator's owners and reviewers, and the router's for any card it left without one.
fn delegated_assignment(
    mut cards: Vec<CardSpec>,
    delegated: &CoordinatedCards,
    crew: &assign::Crew,
    rng: &mut StdRng,
) -> assign::Assignment {
    let mut picks = coordinated::delegate(&mut cards, delegated, crew.settings, crew.family);
    let unowned: Vec<CardSpec> = cards
        .iter()
        .filter(|card| card.owner.is_none())
        .cloned()
        .collect();
    if unowned.is_empty() {
        return assign::Assignment {
            cards,
            picks,
            draws: Vec::new(),
        };
    }
    let routed = routed(unowned, crew, rng);
    picks.extend(routed.picks);
    let draws = routed.draws;
    let mut by_id: BTreeMap<CardId, CardSpec> = routed
        .cards
        .into_iter()
        .map(|card| (card.id.clone(), card))
        .collect();
    let cards = cards
        .into_iter()
        .map(|card| by_id.remove(&card.id).unwrap_or(card))
        .collect();
    assign::Assignment {
        cards,
        picks,
        draws,
    }
}

/// The track records as of now, rebuilt from the outcome log at divide time so this run routes
/// on every run before it. `None` when the log can't be read, and the router falls back to the
/// plain counts handoff cards read.
fn track_records(host: &Host) -> Option<Book> {
    let store = Store::new(repo_state_dir(host), host.repo().root.clone());
    match Book::rebuild(&store, &host.settings().routing, host.now()) {
        Ok(book) => Some(book),
        Err(err) => {
            tracing::warn!("track records unavailable, routing on plain counts: {err}");
            None
        }
    }
}

/// Each draw goes to the outcome log before any card starts, a draw already there once.
fn log_draws(host: &Host, draws: &[argonauts_learning::routing::Draw]) -> EngineResult<()> {
    let state_dir = repo_state_dir(host);
    let logged: Vec<CardId> = outcomes::run_lines(&state_dir, host.run_id())?
        .into_iter()
        .filter_map(|line| match line {
            outcomes::LogLine::Draw(draw) => Some(draw.card),
            _ => None,
        })
        .collect();
    let lines: Vec<outcomes::LogLine> = draws
        .iter()
        .filter(|draw| !logged.contains(&draw.card))
        .cloned()
        .map(outcomes::LogLine::Draw)
        .collect();
    outcomes::append_lines(&state_dir, host.run_id(), &lines)
}

/// Nothing to divide: the plan checkpoint shows why the coordinator's plan was rejected, and
/// you send it back, run it solo, or abandon the run.
fn rejected_division(settings: &Settings, rejected: Rejected) -> Division {
    let summary = "The coordinator's plan broke the rules twice.".to_owned();
    let mut broken = vec![format!(
        "{summary} Send it back with a note, run it solo, or abandon the run."
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
        plan_author: rejected.coordinator.clone(),
        divided_by: rejected.coordinator,
        graph: TaskGraph {
            cards: Vec::new(),
            critical_path: Vec::new(),
        },
        budget: budget_lines::plan(&settings.budget).unwrap_or_else(|_| empty_budget()),
        broken,
    }
}

/// Validated cards with contracts first applied, or every rule they break.
pub fn build(
    run: &RunId,
    proposal: &TaskGraphProposal,
    settings: &Settings,
) -> Result<Vec<CardSpec>, Vec<String>> {
    let cards = graph::validate(run, proposal, settings)?;
    if !settings.divide.contracts_first {
        return Ok(cards);
    }
    let interfaces = cards
        .iter()
        .filter(|card| card.kind == CardKind::Interface)
        .count();
    let builds = cards.iter().any(|card| card.kind == CardKind::Build);
    if builds && interfaces != 1 {
        return Err(vec![format!(
            "Contracts first needs exactly one interface card (kind `interface`) with the \
             stubs every build card codes against. The plan has {interfaces}."
        )]);
    }
    let tests = settings.divide.acceptance_tests_by == TestAuthor::Reviewer;
    let cards = contracts::contracts_first(cards, tests);
    let broken = graph::rules(&cards, settings);
    if broken.is_empty() {
        Ok(cards)
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

/// The recorded division, unless a plan checkpoint after it sent the plan back. The pick card
/// of the next round is a plan checkpoint too, so any send back since counts, not only the last.
fn current(host: &Host) -> Option<(TaskGraph, BudgetPlan)> {
    host.read(|state| {
        let graph = state.divide.graph.clone()?;
        let seq = state.divide.graph_seq?;
        let replanned = state.checkpoints.by_id.values().any(|record| {
            record.opened.kind == CheckpointKind::Plan
                && record.since > seq
                && record.withdrawn.is_none()
                && record
                    .decided
                    .as_ref()
                    .is_some_and(|decided| decided.action == AnswerAction::SendBack)
        });
        let budget = state.divide.budget.clone().unwrap_or_else(empty_budget);
        (!replanned).then_some((graph, budget))
    })
}

struct ChosenPlan {
    plan: PlanDraft,
    text: String,
    author: WorkerRef,
    /// The coordinator's cards and assignments, when the plan is the coordinator's.
    delegated: Option<CoordinatedCards>,
}

/// The chosen draft. Its structured form is read from `<draft>.json` beside the sealed text
/// when it's there, or from the text itself when that is JSON; otherwise the text is the
/// approach and the new dependencies come from its "New dependencies" section.
fn chosen_plan(host: &Host) -> EngineResult<ChosenPlan> {
    let (chosen, coordinated) = host.read(|state| {
        (
            state.plan.chosen().cloned(),
            state.plan.coordinated_plan_chosen(),
        )
    });
    let chosen = chosen
        .ok_or_else(|| EngineError::Refused("No plan has been chosen to divide.".to_owned()))?;
    let delegated = if coordinated {
        Some(coordinated::load_cards(&chosen)?)
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

/// The phase lead turns the plan into cards; the default agent when there is none.
fn divider(host: &Host) -> WorkerRef {
    let settings = host.settings();
    host.read(|state| lead::phase_lead(state, settings))
        .unwrap_or_else(|| settings.default_agent.clone())
}

async fn ask_cards(
    host: &Host,
    call: &StepCall,
    worker: &WorkerRef,
    input: PlanToCardsInput,
    broken: &[String],
) -> EngineResult<TaskGraphProposal> {
    let mut ask = Ask::new(
        Some(call.at.clone()),
        RoleName::PhaseLead,
        worker.clone(),
        prompts::divide::PLAN_TO_CARDS,
        input,
    );
    if !broken.is_empty() {
        let text = format!(
            "Your last proposal broke these rules. Propose the cards again with each fixed.\n\n- {}",
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
        "plan checkpoint",
    ));
    Ok(host.ask::<PlanToCards>(ask).await?.output)
}

/// Each card's budget: the median spend of past cards of its task type, else `per_card_*`.
fn with_budgets(
    host: &Host,
    mut cards: Vec<CardSpec>,
    run_task_type: Option<&str>,
) -> EngineResult<Vec<CardSpec>> {
    let state_dir = repo_state_dir(host);
    let mut history: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for card in &mut cards {
        let task_type = assign::task_type(&card.kind, run_task_type);
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
        card.budget = budget_lines::card_budget(past, &host.settings().budget);
    }
    Ok(cards)
}

fn candidate(
    host: &Host,
    worker: WorkerRef,
    task_type: &str,
    book: Option<&Book>,
) -> router::Candidate {
    let services = host.services();
    let vendor = worker.vendor();
    let health = services.health.state(&vendor);
    let quota = services.quota.get(&vendor);
    router::Candidate {
        record: services.track_records.get(&worker, task_type),
        health: if health.disabled.is_some() {
            HealthState::Down
        } else {
            health.state
        },
        quota_used: quota.and_then(|reading| match reading.status {
            QuotaStatus::Rejected => Some(1.0),
            _ => reading.used,
        }),
        cost: services
            .registry
            .estimate(&worker, CARD_INPUT_TOKENS, CARD_OUTPUT_TOKENS),
        running_cards: 0,
        learned: book.map(|book| book.record(&worker, task_type)),
        worker,
    }
}

pub(crate) fn empty_budget() -> BudgetPlan {
    BudgetPlan {
        lines: Vec::new(),
        total: BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 0.0,
        },
        estimate_low: 0.0,
        estimate_high: 0.0,
    }
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
    use argonauts_core::steps::plan::{CardProposal, ProposedKind};

    use super::*;

    fn proposal(interfaces: usize) -> TaskGraphProposal {
        let card = |key: &str, kind, owned: &str| CardProposal {
            key: key.to_owned(),
            title: key.to_owned(),
            task: key.to_owned(),
            kind,
            owned_paths: vec![owned.to_owned()],
            read_only_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: vec!["G1".to_owned()],
        };
        let mut cards = vec![card("store", ProposedKind::Build, "src/store/**")];
        for n in 0..interfaces {
            cards.push(card(
                &format!("api{n}"),
                ProposedKind::Interface,
                &format!("src/api{n}.rs"),
            ));
        }
        TaskGraphProposal { cards }
    }

    #[test]
    fn contracts_first_needs_one_interface_card() {
        let run = RunId::new("run-01").unwrap();
        let settings = Settings::default();
        assert_eq!(build(&run, &proposal(1), &settings).unwrap().len(), 3);
        let reasons = build(&run, &proposal(0), &settings).unwrap_err();
        assert!(
            reasons[0].contains("exactly one interface card"),
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
