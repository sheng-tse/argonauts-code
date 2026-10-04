//! The plan checkpoint: plan, graph, owners, reviewers, test list, and budget together.
//! Plans judged without a winner go to the pick card first. "Run it solo" switches to the solo
//! workflow your `solo` settings pick, whose `build` makes the solo card; "send back" re-plans
//! with your note; approve with edits records your note as a decision and adds it to every card
//! prompt, and a note that changes an owner, a card's paths, or a budget line goes through the
//! graph validator.

use argonauts_core::PhaseStatus;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardEvent, CardKind, CardState};
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointKind, CheckpointPacket, FreeText, PlanPacket,
    QuestionPacket,
};
use argonauts_core::events::plan::{BudgetPlan, DivideEvent, TaskGraph};
use argonauts_core::events::run::{PhaseChange, RunOutcome, RunPhase};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::{CheckpointId, DecisionId, WorkerRef};
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts, EvidenceSubject};
use argonauts_core::workflow::WorkflowRef;
use argonauts_core::{Note, NoteLevel};

use super::{Division, assign, budget_lines, dependencies, edits, paths, pick, plan_facts};
use crate::checkpoints::{self, CheckpointRequest, options};
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::ids::IdKind;
use crate::workflow::{Flow, StepCall, StepDone, runner};
use crate::{plan, roles};

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if host.read(|state| state.run.phase) != Some(RunPhase::Dividing) {
        phase(host, PhaseStatus::Started, None)?;
    }
    pick::pick_if_tied(host, call).await?;
    let division = super::division(host, call).await?;
    let mut graph = division.graph.clone();
    let budget = division.budget.clone();
    let new_dependencies = &division.plan.new_dependencies;
    let dependency_problem = dependencies::problem(&host.repo().root, new_dependencies)?;
    let subject = subject(host).await;
    let mut reopened: Vec<String> = Vec::new();
    let mut last: Option<CheckpointId> = None;
    loop {
        let settings = host.settings();
        let unlisted =
            dependencies::unlisted(new_dependencies, &settings.limits.dependency_allowlist);
        let blocking = blocking(&division.broken, &unlisted);
        let shown = Shown {
            division: &division,
            graph: &graph,
            budget: &budget,
            blocking: &blocking,
            reopened: &reopened,
            dependency_problem: dependency_problem.as_deref(),
            subject: subject.as_ref(),
        };
        let request = request(host, call, &shown);
        let decided = checkpoints::open_and_wait(host, request).await?;
        if last.as_ref() == Some(&decided.id) {
            return Err(EngineError::Refused(format!(
                "The plan checkpoint {} couldn't be reopened after: {}",
                decided.id,
                reopened.last().map_or("", String::as_str)
            )));
        }
        last = Some(decided.id.clone());
        match verdict(&decided.action, decided.text.as_deref(), &blocking) {
            Verdict::Approve => {
                approve(host, call, &graph, &division, dependency_problem.as_deref()).await?;
                return finish(host, &graph, "Plan approved");
            }
            Verdict::ApproveWithEdits(note) => {
                let family = |worker: &WorkerRef| roles::family(host, worker);
                let edited = edits::parse(&note)
                    .and_then(|parsed| edits::apply(&graph, &budget, &parsed, settings, &family));
                match edited {
                    Ok(edited) => {
                        record_edits(host, &note, &edited).await?;
                        graph = edited.graph;
                        approve(host, call, &graph, &division, dependency_problem.as_deref())
                            .await?;
                        return finish(host, &graph, "Plan approved with your edits");
                    }
                    Err(reason) => reopened.push(format!("Your edits weren't applied: {reason}")),
                }
            }
            Verdict::Reopen(reason) => reopened.push(reason),
            Verdict::SendBack => {
                phase(
                    host,
                    PhaseStatus::Finished,
                    Some("Sent back for a new plan"),
                )?;
                return Ok(StepDone::done("Sent back for a new plan with your note"));
            }
            Verdict::Solo => {
                phase(host, PhaseStatus::Finished, Some("Switched to solo"))?;
                return Ok(StepDone {
                    end: StepEnd::Done,
                    summary: Some("You chose to run it solo".to_owned()),
                    flow: Flow::Switch(WorkflowRef::new(runner::solo_workflow(settings))),
                });
            }
            Verdict::Abandon => {
                phase(host, PhaseStatus::Finished, Some("Abandoned"))?;
                return Ok(StepDone {
                    end: StepEnd::Done,
                    summary: Some("You abandoned the run".to_owned()),
                    flow: Flow::Finish(RunOutcome::Abandoned),
                });
            }
        }
    }
}

/// What the run does with an answer to the plan checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    ApproveWithEdits(String),
    SendBack,
    Solo,
    Abandon,
    /// Open the checkpoint again, saying why.
    Reopen(String),
}

/// `blocking` holds what stops approval: unlisted dependencies and rules the graph breaks.
pub fn verdict(action: &AnswerAction, text: Option<&str>, blocking: &[String]) -> Verdict {
    let note = text.map(str::trim).filter(|note| !note.is_empty());
    match action {
        AnswerAction::Approve | AnswerAction::ApproveWithEdits if !blocking.is_empty() => {
            Verdict::Reopen(format!(
                "The plan can't be approved yet. {}",
                blocking.join(" ")
            ))
        }
        AnswerAction::Approve => Verdict::Approve,
        AnswerAction::ApproveWithEdits => note.map_or(Verdict::Approve, |note| {
            Verdict::ApproveWithEdits(note.to_owned())
        }),
        AnswerAction::SendBack => Verdict::SendBack,
        AnswerAction::SwitchToSolo => Verdict::Solo,
        AnswerAction::Abandon => Verdict::Abandon,
        other => Verdict::Reopen(format!(
            "{other:?} isn't an answer the plan checkpoint takes."
        )),
    }
}

/// The card's options: approval is left off while anything blocks it.
pub fn answer_options(blocked: bool) -> Option<Vec<AnswerOption>> {
    blocked.then(|| {
        options::standard(CheckpointKind::Plan)
            .into_iter()
            .filter(|option| {
                !matches!(
                    option.action,
                    AnswerAction::Approve | AnswerAction::ApproveWithEdits
                )
            })
            .collect()
    })
}

pub fn blocking(broken: &[String], unlisted: &[String]) -> Vec<String> {
    let mut reasons = broken.to_vec();
    for package in unlisted {
        reasons.push(format!(
            "The plan adds {package}, which isn't on your dependency allowlist. Add it with \
             /permissions, or send the plan back."
        ));
    }
    reasons
}

/// One line per acceptance-test card, for the test list.
pub fn test_list(graph: &TaskGraph) -> Vec<String> {
    graph
        .cards
        .iter()
        .filter_map(|card| {
            let CardKind::AcceptanceTests { for_card } = &card.kind else {
                return None;
            };
            let criteria = if card.criteria.is_empty() {
                String::new()
            } else {
                format!(", covering {}", card.criteria.join(", "))
            };
            let by = card
                .owner
                .as_ref()
                .map_or_else(String::new, |owner| format!(", written by {owner}"));
            Some(format!("{} tests {for_card}{criteria}{by}", card.id))
        })
        .collect()
}

/// What one opening of the plan checkpoint shows.
struct Shown<'a> {
    division: &'a Division,
    graph: &'a TaskGraph,
    budget: &'a BudgetPlan,
    blocking: &'a [String],
    /// Why it opened again, one reason per reopening. They're part of the evidence, so each
    /// reopening is a checkpoint of its own.
    reopened: &'a [String],
    /// Why the conductor can't add the new dependencies.
    dependency_problem: Option<&'a str>,
    subject: Option<&'a EvidenceSubject>,
}

/// The commit the run started from, which the plan was made against. `None`, which asks you,
/// when it can't be read.
pub(super) async fn subject(host: &Host) -> Option<EvidenceSubject> {
    let base = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.base_commit.clone())
    })?;
    let tree_sha = host.git().tree_of(&host.repo().root, &base).await.ok()?;
    Some(EvidenceSubject {
        base_commit: base.clone(),
        head_commit: base,
        tree_sha,
    })
}

fn request(host: &Host, call: &StepCall, shown: &Shown) -> CheckpointRequest {
    let Shown {
        division,
        graph,
        budget,
        blocking,
        reopened,
        dependency_problem,
        subject,
    } = shown;
    let settings = host.settings();
    let review = plan_facts::review(host);
    let (decisions, owners) = host.read(|state| {
        let owners: Vec<String> = state
            .divide
            .owners
            .iter()
            .map(|(card, pick)| format!("{card} goes to {}: {}", pick.worker, pick.reason))
            .collect();
        (state.plan.decisions.clone(), owners)
    });
    let mut flags = blocking.to_vec();
    flags.extend(review.flags());
    if let Some(problem) = dependency_problem {
        flags.push(dependency_flag(&division.plan.new_dependencies, problem));
    }
    flags.extend(assign::shortfalls(graph, settings));
    flags.extend(reopened.iter().cloned());
    let owned: Vec<String> = graph
        .cards
        .iter()
        .flat_map(|card| card.owned_paths.iter().cloned())
        .collect();
    let protected: Vec<String> = owned
        .iter()
        .filter(|path| {
            settings
                .limits
                .protected_paths
                .iter()
                .any(|area| paths::inside(path, area) || paths::contains(path, area))
        })
        .cloned()
        .collect();
    let planning = review.facts();
    let facts = EvidenceFacts {
        open_questions: planning
            .open_questions
            .map(|open| open + u32::from(dependency_problem.is_some())),
        estimate_usd: budget_lines::estimate_usd(&graph.cards, budget),
        new_dependencies: division.plan.new_dependencies.clone(),
        agent_paths: owned,
        protected_paths: protected,
        subject: subject.cloned(),
        ..planning
    };
    let mut documents = vec![EvidenceDocument {
        label: "plan".to_owned(),
        text: division.plan_text.clone(),
        path: None,
        sha256: None,
    }];
    if !reopened.is_empty() {
        documents.push(EvidenceDocument {
            label: "why it opened again".to_owned(),
            text: reopened.join("\n"),
            path: None,
            sha256: None,
        });
    }
    let packet = PlanPacket {
        summary: division.plan.summary.clone(),
        near_tie: review
            .judging
            .as_ref()
            .is_some_and(|result| result.near_tie),
        judging: review.judging,
        decisions,
        graph: (*graph).clone(),
        acceptance_tests: test_list(graph),
        budget: (*budget).clone(),
        new_dependencies: division.plan.new_dependencies.clone(),
        flags,
        workflow: host.read(super::workflow_plan::plan_workflow),
    };
    CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Plan,
        title: "Approve this plan?".to_owned(),
        card: None,
        packet: CheckpointPacket::Plan(Box::new(packet)),
        facts,
        agent_outputs: owners,
        documents,
        options: answer_options(!blocking.is_empty()),
        free_text: FreeText::Edits,
        authors: vec![division.plan_author.clone(), division.divided_by.clone()],
    }
}

fn dependency_flag(packages: &[String], problem: &str) -> String {
    format!(
        "The conductor can't add {}: {problem} Add them yourself on the integration branch, \
         then approve.",
        packages.join(", ")
    )
}

/// Every card is created Waiting, then the conductor adds the new dependencies, so each card
/// branches from a tree that already has them. When the checkpoint said the conductor can't add
/// them, they're yours to add; when adding fails anyway, a question asks you.
async fn approve(
    host: &Host,
    call: &StepCall,
    graph: &TaskGraph,
    division: &Division,
    dependency_problem: Option<&str>,
) -> EngineResult<()> {
    for card in &graph.cards {
        if host.read(|state| state.cards.by_id.contains_key(&card.id)) {
            continue;
        }
        let created = CardEvent::Created(Box::new(card.clone()));
        host.record(Some(&card.id), None, RunEventKind::Card(created))?;
        let waiting = CardEvent::State {
            from: None,
            to: CardState::Waiting,
            reason: "created from the approved plan".to_owned(),
        };
        host.record(Some(&card.id), None, RunEventKind::Card(waiting))?;
    }
    let packages = &division.plan.new_dependencies;
    if packages.is_empty() || dependencies_added(host, packages) {
        return Ok(());
    }
    if let Some(problem) = dependency_problem {
        // The checkpoint told you, and you approved: they're yours to add.
        host.record(
            None,
            None,
            RunEventKind::Note(Note {
                level: NoteLevel::Warn,
                text: dependency_flag(packages, problem),
            }),
        )?;
        return Ok(());
    }
    match dependencies::add(host, packages).await {
        Ok(_) => Ok(()),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(error) => ask_to_add(host, call, graph, division, &error.to_string()).await,
    }
}

fn dependencies_added(host: &Host, packages: &[String]) -> bool {
    host.read(|state| {
        packages
            .iter()
            .all(|package| state.integration.dependencies.contains(package))
    })
}

/// The conductor couldn't add the packages: you add them yourself, and say when.
async fn ask_to_add(
    host: &Host,
    call: &StepCall,
    graph: &TaskGraph,
    division: &Division,
    failure: &str,
) -> EngineResult<()> {
    let Some(first) = graph.cards.first() else {
        return Ok(());
    };
    let text = dependency_flag(&division.plan.new_dependencies, failure);
    let offered = vec![ADDED_THEM.to_owned(), WITHOUT_THEM.to_owned()];
    let request = CheckpointRequest {
        at: Some(call.at.clone()),
        kind: CheckpointKind::Question,
        title: "Add the new dependencies yourself?".to_owned(),
        card: Some(first.id.clone()),
        packet: CheckpointPacket::Question(QuestionPacket {
            from: division.divided_by.clone(),
            card: first.id.clone(),
            text: text.clone(),
            options: offered.clone(),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: vec![EvidenceDocument {
            label: "why".to_owned(),
            text,
            path: None,
            sha256: None,
        }],
        options: Some(options::question(&offered)),
        free_text: FreeText::Answer,
        authors: Vec::new(),
    };
    let decided = checkpoints::open_and_wait(host, request).await?;
    let answer = match &decided.action {
        AnswerAction::Choose { value } => value.clone(),
        _ => decided.text.clone().unwrap_or_default(),
    };
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!(
                "The conductor didn't add {}. You answered: {answer}",
                division.plan.new_dependencies.join(", ")
            ),
        }),
    )?;
    Ok(())
}

const ADDED_THEM: &str = "I added them, go on";
const WITHOUT_THEM: &str = "Go on without them";

async fn record_edits(host: &Host, note: &str, edited: &edits::Edited) -> EngineResult<()> {
    // The record takes the next number itself; this one only fills the field.
    let next = DecisionId::numbered(host.read(|state| state.ids.last(IdKind::Decision)) + 1);
    plan::decisions::record(host, edits::decision(next, note)).await?;
    let algorithm = host.settings().routing.algorithm;
    for card in edited.graph.cards.iter() {
        if edited.owners.contains(&card.id)
            && let Some(owner) = &card.owner
        {
            let event = DivideEvent::Owner {
                card: card.id.clone(),
                worker: owner.clone(),
                algorithm,
                explored: false,
                reason: "you chose this owner at the plan checkpoint".to_owned(),
            };
            host.record(Some(&card.id), None, RunEventKind::Divide(event))?;
        }
        if edited.reviewers.contains(&card.id) {
            let event = DivideEvent::Reviewers {
                card: card.id.clone(),
                reviewers: card.reviewers.clone(),
                blind: host.settings().review.blind,
            };
            host.record(Some(&card.id), None, RunEventKind::Divide(event))?;
        }
    }
    if edited.budget_changed {
        let event = DivideEvent::Budget(edited.budget.clone());
        host.record(None, None, RunEventKind::Divide(event))?;
    }
    let event = DivideEvent::Graph(edited.graph.clone());
    host.record(None, None, RunEventKind::Divide(event))?;
    Ok(())
}

fn finish(host: &Host, graph: &TaskGraph, what: &str) -> EngineResult<StepDone> {
    let owners: std::collections::BTreeSet<&WorkerRef> = graph
        .cards
        .iter()
        .filter_map(|card| card.owner.as_ref())
        .collect();
    let summary = format!(
        "{what}: {} cards for {} agents",
        graph.cards.len(),
        owners.len()
    );
    phase(host, PhaseStatus::Finished, Some(&summary))?;
    Ok(StepDone::done(summary))
}

fn phase(host: &Host, status: PhaseStatus, summary: Option<&str>) -> EngineResult<()> {
    let change = PhaseChange {
        phase: RunPhase::Dividing,
        status,
        summary: summary.map(str::to_owned),
    };
    host.record(None, None, RunEventKind::Phase(change))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_dependency_blocks_plan_approval() {
        let new = vec!["ioredis@5".to_owned(), "zod".to_owned()];
        let allowlist = vec!["zod".to_owned()];
        let unlisted = dependencies::unlisted(&new, &allowlist);
        assert_eq!(unlisted, ["ioredis@5"]);
        let blocked = blocking(&[], &unlisted);
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].contains("adds ioredis@5, which isn't on your dependency allowlist"));

        // Approval isn't offered, and an approval that arrives anyway reopens the card.
        let offered = answer_options(true).unwrap();
        assert!(offered.iter().all(|option| !matches!(
            option.action,
            AnswerAction::Approve | AnswerAction::ApproveWithEdits
        )));
        assert!(matches!(
            verdict(&AnswerAction::Approve, None, &blocked),
            Verdict::Reopen(reason) if reason.contains("ioredis@5")
        ));
        assert!(matches!(
            verdict(
                &AnswerAction::ApproveWithEdits,
                Some("card-001 owner codex"),
                &blocked
            ),
            Verdict::Reopen(_)
        ));
        // Sending it back still works while blocked.
        assert_eq!(
            verdict(&AnswerAction::SendBack, Some("no redis"), &blocked),
            Verdict::SendBack
        );

        // Once allowlisted, it goes through.
        let allowlist = vec!["zod".to_owned(), "ioredis".to_owned()];
        let unlisted = dependencies::unlisted(&new, &allowlist);
        let blocked = blocking(&[], &unlisted);
        assert!(blocked.is_empty());
        assert_eq!(answer_options(false), None);
        assert_eq!(
            verdict(&AnswerAction::Approve, None, &blocked),
            Verdict::Approve
        );
    }

    #[test]
    fn each_answer_has_its_verdict() {
        assert_eq!(
            verdict(
                &AnswerAction::ApproveWithEdits,
                Some(" card-001 owner agy "),
                &[]
            ),
            Verdict::ApproveWithEdits("card-001 owner agy".to_owned())
        );
        assert_eq!(
            verdict(&AnswerAction::ApproveWithEdits, Some("  "), &[]),
            Verdict::Approve
        );
        assert_eq!(
            verdict(&AnswerAction::SwitchToSolo, None, &[]),
            Verdict::Solo
        );
        assert_eq!(verdict(&AnswerAction::Abandon, None, &[]), Verdict::Abandon);
        assert!(matches!(
            verdict(&AnswerAction::Merge, None, &[]),
            Verdict::Reopen(_)
        ));
        let broken = vec!["card-001 owns no paths.".to_owned()];
        assert!(matches!(
            verdict(&AnswerAction::Approve, None, &broken),
            Verdict::Reopen(_)
        ));
    }
}
