//! Your edits at the plan checkpoint. Lines of the note in these forms change the graph, and
//! the whole note becomes a decision record:
//!
//! ```text
//! card-002 owner codex/gpt-5.6
//! card-002 reviewers agy, claude-code/opus
//! card-002 paths src/store/**, src/cache.rs
//! card-002 budget 4.5
//! budget cards 0.45
//! ```
//!
//! An edited graph goes through the same rules as a proposal, so an edit that breaks one
//! reopens the checkpoint with the reason.

use argonauts_core::CardId;
use argonauts_core::events::card::{CardKind, CardSpec};
use argonauts_core::events::plan::{BudgetPlan, DesignDecision, TaskGraph};
use argonauts_core::names::{DecisionId, RoleName, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::{BudgetAmount, BudgetLineKind, BudgetUnit};

use super::{budget_lines, graph};
use crate::roles;

/// Its title gives the record's file name, `D-NNN-your-edits.md`.
pub const DECISION_TITLE: &str = "Your edits";

#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Owner {
        card: CardId,
        worker: WorkerRef,
    },
    Reviewers {
        card: CardId,
        workers: Vec<WorkerRef>,
    },
    Paths {
        card: CardId,
        paths: Vec<String>,
    },
    CardBudget {
        card: CardId,
        amount: f64,
    },
    Line {
        line: BudgetLineKind,
        share: f64,
    },
}

/// The graph and budget after your edits, and what changed.
#[derive(Debug, Clone, PartialEq)]
pub struct Edited {
    pub graph: TaskGraph,
    pub budget: BudgetPlan,
    pub owners: Vec<CardId>,
    pub reviewers: Vec<CardId>,
    pub budget_changed: bool,
}

/// The edits in the note. A line that starts like an edit but doesn't parse is an error, so a
/// typo never passes as prose.
pub fn parse(note: &str) -> Result<Vec<Edit>, String> {
    let mut edits = Vec::new();
    for line in note.lines().map(str::trim) {
        let mut words = line.splitn(3, char::is_whitespace);
        let (Some(first), Some(second)) = (words.next(), words.next()) else {
            continue;
        };
        let rest = words.next().unwrap_or("").trim();
        let keyword = second.trim_end_matches(':').to_ascii_lowercase();
        if first.eq_ignore_ascii_case("budget") {
            let Some(line_kind) = budget_lines::line_named(second.trim_end_matches(':')) else {
                continue;
            };
            let share = number(rest, line)?;
            edits.push(Edit::Line {
                line: line_kind,
                share,
            });
            continue;
        }
        if !looks_like_card(first)
            || !matches!(keyword.as_str(), "owner" | "reviewers" | "paths" | "budget")
        {
            continue;
        }
        let card = CardId::new(first).map_err(|_| format!("`{first}` isn't a card id."))?;
        let edit = match keyword.as_str() {
            "owner" => Edit::Owner {
                card,
                worker: worker(rest, line)?,
            },
            "reviewers" => Edit::Reviewers {
                card,
                workers: list(rest)
                    .iter()
                    .map(|item| worker(item, line))
                    .collect::<Result<_, _>>()?,
            },
            "paths" => Edit::Paths {
                card,
                paths: list(rest),
            },
            _ => Edit::CardBudget {
                card,
                amount: number(rest, line)?,
            },
        };
        edits.push(edit);
    }
    Ok(edits)
}

/// Applies the edits and checks the result like a proposal. `Err` is the reason to reopen the
/// checkpoint with.
pub fn apply(
    graph: &TaskGraph,
    budget: &BudgetPlan,
    edits: &[Edit],
    settings: &Settings,
    family: &dyn Fn(&WorkerRef) -> String,
) -> Result<Edited, String> {
    let mut cards = graph.cards.clone();
    let mut budget = budget.clone();
    let (mut owners, mut reviewers, mut budget_changed) = (Vec::new(), Vec::new(), false);
    let owner_pool = roles::candidates(settings, RoleName::Owner);
    let reviewer_pool = roles::candidates(settings, RoleName::Reviewer);
    for edit in edits {
        match edit {
            Edit::Owner { card, worker } => {
                if !owner_pool.contains(worker) {
                    return Err(format!("{worker} isn't one of the owner candidates."));
                }
                find(&mut cards, card)?.owner = Some(worker.clone());
                owners.push(card.clone());
            }
            Edit::Reviewers { card, workers } => {
                if let Some(worker) = workers.iter().find(|w| !reviewer_pool.contains(w)) {
                    return Err(format!("{worker} isn't one of the reviewer candidates."));
                }
                find(&mut cards, card)?.reviewers.clone_from(workers);
                reviewers.push(card.clone());
            }
            Edit::Paths { card, paths } => {
                find(&mut cards, card)?.owned_paths.clone_from(paths);
            }
            Edit::CardBudget { card, amount } => {
                if !amount.is_finite() || *amount <= 0.0 {
                    return Err(format!("{card}'s budget must be more than 0."));
                }
                let spec = find(&mut cards, card)?;
                let unit = spec
                    .budget
                    .limit
                    .map_or(BudgetUnit::Usd, |limit| limit.unit);
                spec.budget.limit = Some(BudgetAmount {
                    unit,
                    amount: *amount,
                });
            }
            Edit::Line { line, share } => {
                if !(0.0..=1.0).contains(share) {
                    return Err(format!(
                        "The {} budget line's share must be between 0 and 1.",
                        budget_lines::line_name(*line)
                    ));
                }
                let total = budget.total.amount;
                let entry = budget
                    .lines
                    .iter_mut()
                    .find(|entry| entry.line == *line)
                    .ok_or_else(|| {
                        format!("The plan has no {} line.", budget_lines::line_name(*line))
                    })?;
                entry.amount.amount = total * share;
                budget_changed = true;
            }
        }
    }
    follow_reviewers(&mut cards, &reviewers, &owners);
    let mut broken = graph::rules(&cards, settings);
    broken.extend(conflicts(&cards, settings, family));
    if budget_changed && let Err(reason) = budget_lines::check(&budget) {
        broken.push(reason);
    }
    if !broken.is_empty() {
        return Err(broken.join(" "));
    }
    Ok(Edited {
        graph: TaskGraph {
            critical_path: graph.critical_path.clone(),
            cards,
        },
        budget,
        owners,
        reviewers,
        budget_changed,
    })
}

/// The decision record for an approval with edits.
pub fn decision(id: DecisionId, note: &str) -> DesignDecision {
    DesignDecision {
        id,
        title: DECISION_TITLE.to_owned(),
        choice: note.trim().to_owned(),
        rationale: "You approved the plan with these edits at the plan checkpoint.".to_owned(),
        alternatives: Vec::new(),
        path: None,
    }
}

/// A test card is written by its card's first reviewer, so it follows a reviewer edit unless
/// you set its owner yourself.
fn follow_reviewers(cards: &mut [CardSpec], reviewed: &[CardId], owned: &[CardId]) {
    let firsts: Vec<(CardId, WorkerRef)> = cards
        .iter()
        .filter(|card| reviewed.contains(&card.id))
        .filter_map(|card| Some((card.id.clone(), card.reviewers.first()?.clone())))
        .collect();
    for card in cards.iter_mut() {
        if let CardKind::AcceptanceTests { for_card } = &card.kind
            && !owned.contains(&card.id)
            && let Some((_, first)) = firsts.iter().find(|(id, _)| id == for_card)
        {
            card.owner = Some(first.clone());
        }
    }
}

fn conflicts(
    cards: &[CardSpec],
    settings: &Settings,
    family: &dyn Fn(&WorkerRef) -> String,
) -> Vec<String> {
    let mut broken = Vec::new();
    for card in cards {
        let Some(owner) = &card.owner else { continue };
        for reviewer in &card.reviewers {
            if let Some(reason) = roles::conflict(RoleName::Reviewer, reviewer, Some(owner), family)
            {
                broken.push(format!("{reason} ({}).", card.id));
            } else if settings.review.other_family && family(reviewer) == family(owner) {
                broken.push(format!(
                    "{reviewer} is in the same model family as {owner}, {}'s owner.",
                    card.id
                ));
            }
        }
        if let CardKind::AcceptanceTests { for_card } = &card.kind
            && let Some(tested) = cards.iter().find(|c| &c.id == for_card)
            && let Some(tested_owner) = &tested.owner
            && roles::conflict(RoleName::Reviewer, owner, Some(tested_owner), family).is_some()
        {
            broken.push(format!(
                "{owner} can't write the acceptance tests for its own card {for_card}."
            ));
        }
    }
    broken
}

fn find<'a>(cards: &'a mut [CardSpec], id: &CardId) -> Result<&'a mut CardSpec, String> {
    cards
        .iter_mut()
        .find(|card| &card.id == id)
        .ok_or_else(|| format!("There's no {id} in the plan."))
}

fn looks_like_card(word: &str) -> bool {
    word.split_once('-').is_some_and(|(prefix, number)| {
        !prefix.is_empty()
            && prefix.chars().all(|c| c.is_ascii_lowercase())
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    })
}

fn list(text: &str) -> Vec<String> {
    text.split(',')
        .map(|item| item.trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

fn worker(text: &str, line: &str) -> Result<WorkerRef, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("\"{line}\" doesn't name an agent."))
}

fn number(text: &str, line: &str) -> Result<f64, String> {
    text.trim()
        .trim_start_matches('$')
        .parse()
        .map_err(|_| format!("\"{line}\" doesn't end in a number."))
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::card::CardBudget;
    use argonauts_core::events::plan::BudgetLine;

    use super::*;

    fn worker(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    fn family(worker: &WorkerRef) -> String {
        worker.worker.clone()
    }

    fn spec(id: &str, kind: CardKind, owned: &str, owner: &str, reviewer: &str) -> CardSpec {
        CardSpec {
            id: CardId::new(id).unwrap(),
            title: id.to_owned(),
            task: id.to_owned(),
            kind,
            owned_paths: vec![owned.to_owned()],
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            owner: Some(worker(owner)),
            reviewers: vec![worker(reviewer)],
            budget: CardBudget::default(),
            predictions: None,
        }
    }

    fn fixture() -> (TaskGraph, BudgetPlan, Settings) {
        let settings = Settings {
            crew: vec![worker("claude-code"), worker("codex"), worker("agy")],
            ..Settings::default()
        };
        let tests = CardKind::AcceptanceTests {
            for_card: CardId::numbered(1),
        };
        let graph = TaskGraph {
            cards: vec![
                spec(
                    "test-002",
                    tests,
                    "tests/acceptance/card-001/**",
                    "codex",
                    "agy",
                ),
                spec(
                    "card-001",
                    CardKind::Build,
                    "src/store/**",
                    "claude-code",
                    "codex",
                ),
            ],
            critical_path: Vec::new(),
        };
        let budget = BudgetPlan {
            lines: [(BudgetLineKind::Cards, 6.0), (BudgetLineKind::Reserve, 4.0)]
                .map(|(line, amount)| BudgetLine {
                    line,
                    amount: BudgetAmount {
                        unit: BudgetUnit::Usd,
                        amount,
                    },
                })
                .to_vec(),
            total: BudgetAmount {
                unit: BudgetUnit::Usd,
                amount: 10.0,
            },
            estimate_low: 5.0,
            estimate_high: 9.0,
        };
        (graph, budget, settings)
    }

    #[test]
    fn edits_parse_from_the_note() {
        let note = "Keep the store small.\ncard-001 owner: codex\ncard-001 paths src/store/**, src/cache.rs\nbudget cards 0.5\ntest-002 budget $2\nThe budget matters.";
        let edits = parse(note).unwrap();
        assert_eq!(
            edits,
            [
                Edit::Owner {
                    card: CardId::numbered(1),
                    worker: worker("codex")
                },
                Edit::Paths {
                    card: CardId::numbered(1),
                    paths: vec!["src/store/**".to_owned(), "src/cache.rs".to_owned()]
                },
                Edit::Line {
                    line: BudgetLineKind::Cards,
                    share: 0.5
                },
                Edit::CardBudget {
                    card: CardId::new("test-002").unwrap(),
                    amount: 2.0
                },
            ]
        );
        assert!(parse("card-001 budget lots").is_err());
    }

    #[test]
    fn edits_are_checked_like_a_proposal() {
        let (graph, budget, settings) = fixture();
        let try_edit =
            |note: &str| apply(&graph, &budget, &parse(note).unwrap(), &settings, &family);

        // The test card follows its card's new reviewer, and then needs another reviewer.
        let reason = try_edit("card-001 reviewers agy").unwrap_err();
        assert!(
            reason.contains("agy can't review its own card (test-002)"),
            "{reason}"
        );
        let edited =
            try_edit("card-001 reviewers agy\ntest-002 reviewers codex\ncard-001 budget 4")
                .unwrap();
        let tests = &edited.graph.cards[0];
        assert_eq!(
            tests.owner,
            Some(worker("agy")),
            "the test card follows the reviewer"
        );
        assert_eq!(
            edited.reviewers,
            [CardId::numbered(1), CardId::new("test-002").unwrap()]
        );
        assert_eq!(edited.graph.cards[1].budget.limit.unwrap().amount, 4.0);
        assert!(!edited.budget_changed);

        let reason = try_edit("card-001 owner codex").unwrap_err();
        assert!(
            reason.contains("codex can't review its own card"),
            "{reason}"
        );
        assert!(
            reason.contains("acceptance tests for its own card card-001"),
            "{reason}"
        );
        assert_eq!(
            try_edit("card-001 owner agy").unwrap().owners,
            [CardId::numbered(1)]
        );
        let reason = try_edit("card-001 paths tests/acceptance/**").unwrap_err();
        assert!(reason.contains("overlap"), "{reason}");
        let reason = try_edit("budget cards 0.7").unwrap_err();
        assert_eq!(
            reason,
            "The budget lines add up to $11.00, not the run budget of $10.00."
        );
        assert!(
            try_edit("budget cards 0.7\nbudget reserve 0.3")
                .unwrap()
                .budget_changed
        );
        assert!(
            try_edit("card-009 owner agy")
                .unwrap_err()
                .contains("no card-009")
        );
        assert!(
            try_edit("card-001 owner ollama")
                .unwrap_err()
                .contains("owner candidates")
        );
        assert!(try_edit("Just be careful.").is_ok());
    }

    #[test]
    fn plan_edits_become_a_decision_titled_your_edits() {
        let record = decision(DecisionId::numbered(4), "  card-001 owner agy\n");
        assert_eq!(record.title, "Your edits");
        assert_eq!(record.choice, "card-001 owner agy");
        assert_eq!(record.id.as_str(), "D-004");
    }
}
