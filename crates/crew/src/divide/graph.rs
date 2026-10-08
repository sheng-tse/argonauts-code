//! Validating a task graph proposal: unique keys, no cycles, owned paths disjoint, no owned
//! path in `.argonauts/**`, `limits.protected_paths`, or the card's forbidden paths, and at most
//! `divide.max_tasks`.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
use argonauts_core::labels;
use argonauts_core::settings::Settings;
use argonauts_core::steps::plan::{ProposedKind, TaskGraphProposal};
use argonauts_core::{CardId, RunId};

use super::paths;

/// No card may own anything here, whatever the settings say.
pub const CONDUCTOR_AREA: &str = ".argonauts/**";

/// The cards, with ids given in order, or every reason the proposal fails. Ids don't depend on
/// the run; `_run` stays for callers that check a coordinator's cards the same way.
pub fn validate(
    _run: &RunId,
    proposal: &TaskGraphProposal,
    settings: &Settings,
) -> Result<Vec<CardSpec>, Vec<String>> {
    let (cards, mut reasons) = convert(proposal);
    let max = settings.divide.max_tasks as usize;
    if proposal.cards.len() > max {
        reasons.push(format!(
            "The plan has {} tasks, more than divide.max_tasks ({max}).",
            proposal.cards.len()
        ));
    }
    reasons.extend(rules(&cards, settings));
    if reasons.is_empty() {
        Ok(cards)
    } else {
        Err(reasons)
    }
}

/// The proposal as cards numbered in order, with the key problems found on the way. A
/// dependency on an unknown key is left out of the card and reported.
pub fn convert(proposal: &TaskGraphProposal) -> (Vec<CardSpec>, Vec<String>) {
    let mut reasons = Vec::new();
    if proposal.cards.is_empty() {
        reasons.push("The plan has no tasks.".to_owned());
    }
    let mut ids: BTreeMap<&str, CardId> = BTreeMap::new();
    for (n, card) in proposal.cards.iter().enumerate() {
        let key = card.key.trim();
        if key.is_empty() {
            reasons.push(format!("Task {} (\"{}\") has no key.", n + 1, card.title));
        } else if ids.contains_key(key) {
            reasons.push(format!("The key `{key}` is used by more than one task."));
        } else {
            ids.insert(key, CardId::numbered(n as u32 + 1));
        }
    }
    let cards = proposal
        .cards
        .iter()
        .enumerate()
        .map(|(n, card)| {
            let mut depends_on = Vec::new();
            for key in &card.depends_on {
                match ids.get(key.trim()) {
                    Some(id) if !depends_on.contains(id) => depends_on.push(id.clone()),
                    Some(_) => {}
                    None => reasons.push(format!(
                        "`{}` depends on `{key}`, which isn't a task in the plan.",
                        card.key
                    )),
                }
            }
            CardSpec {
                id: CardId::numbered(n as u32 + 1),
                title: card.title.trim().to_owned(),
                task: card.task.clone(),
                kind: match card.kind {
                    ProposedKind::Interface => CardKind::Interface,
                    ProposedKind::Build => CardKind::Build,
                    ProposedKind::Chore => CardKind::Chore,
                    ProposedKind::Docs => CardKind::Docs,
                },
                owned_paths: trimmed(&card.owned_paths),
                read_only_paths: trimmed(&card.read_only_paths),
                forbidden_paths: Vec::new(),
                hidden_paths: Vec::new(),
                depends_on,
                criteria: trimmed(&card.criteria),
                owner: None,
                reviewers: Vec::new(),
                budget: CardBudget::default(),
                predictions: None,
                item: Some(labels::slug(&card.key, 3)).filter(|item| !item.is_empty()),
            }
        })
        .collect();
    (cards, reasons)
}

/// The rules on finished cards, also run after contracts first adds its cards and after your
/// edits at the plan checkpoint.
pub fn rules(cards: &[CardSpec], settings: &Settings) -> Vec<String> {
    let mut reasons = Vec::new();
    let known: BTreeSet<&CardId> = cards.iter().map(|card| &card.id).collect();
    if known.len() != cards.len() {
        reasons.push("Two tasks share an id.".to_owned());
    }
    for card in cards {
        for dependency in &card.depends_on {
            if !known.contains(dependency) {
                reasons.push(format!(
                    "{} depends on {dependency}, which isn't in the graph.",
                    name(card)
                ));
            }
        }
    }
    let cycle = in_cycles(cards);
    if !cycle.is_empty() {
        let names: Vec<String> = cycle.iter().map(|card| name(card)).collect();
        reasons.push(format!(
            "These tasks depend on each other in a cycle: {}.",
            names.join(", ")
        ));
    }
    for card in cards {
        reasons.extend(path_problems(card, settings));
    }
    for (i, a) in cards.iter().enumerate() {
        for b in &cards[i + 1..] {
            for x in &a.owned_paths {
                if let Some(y) = b.owned_paths.iter().find(|y| paths::overlap(x, y)) {
                    reasons.push(format!(
                        "{} owns `{x}` and {} owns `{y}`, which overlap. Owned paths must be disjoint.",
                        name(a),
                        name(b)
                    ));
                }
            }
        }
    }
    reasons
}

fn path_problems(card: &CardSpec, settings: &Settings) -> Vec<String> {
    let mut reasons = Vec::new();
    if card.owned_paths.is_empty() && card.kind != CardKind::Solo {
        reasons.push(format!("{} owns no paths.", name(card)));
    }
    let guarded: Vec<&str> = std::iter::once(CONDUCTOR_AREA)
        .chain(settings.limits.protected_paths.iter().map(String::as_str))
        .collect();
    let areas: Vec<&str> = guarded
        .iter()
        .copied()
        .chain(card.forbidden_paths.iter().map(String::as_str))
        .collect();
    for owned in &card.owned_paths {
        if let Some(problem) = paths::malformed(owned) {
            reasons.push(format!("{}: {problem}.", name(card)));
            continue;
        }
        // A card may own a directory with forbidden parts, which its sandbox keeps it out of,
        // but never one holding the conductor's area or a protected path.
        if let Some(area) = guarded.iter().find(|area| paths::contains(owned, area)) {
            reasons.push(format!(
                "{} owns `{owned}`, which holds `{area}`, which no task may own.",
                name(card)
            ));
            continue;
        }
        if let Some(area) = areas.iter().find(|area| paths::inside(owned, area)) {
            reasons.push(format!(
                "{} owns `{owned}`, inside `{area}`, which no task may own.",
                name(card)
            ));
        }
    }
    reasons
}

/// Cards on a dependency cycle, or depending on one, found by peeling off cards whose
/// dependencies are all peeled.
fn in_cycles(cards: &[CardSpec]) -> Vec<&CardSpec> {
    let mut done: BTreeSet<&CardId> = BTreeSet::new();
    loop {
        let before = done.len();
        for card in cards {
            if !done.contains(&card.id)
                && card
                    .depends_on
                    .iter()
                    .all(|d| done.contains(d) || !cards.iter().any(|c| &c.id == d))
            {
                done.insert(&card.id);
            }
        }
        if done.len() == before {
            break;
        }
    }
    cards
        .iter()
        .filter(|card| !done.contains(&card.id))
        .collect()
}

fn name(card: &CardSpec) -> String {
    format!("{} (\"{}\")", card.id, card.title)
}

fn trimmed(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::steps::plan::CardProposal;

    use super::*;

    fn card(key: &str, kind: ProposedKind, owned: &[&str], depends_on: &[&str]) -> CardProposal {
        CardProposal {
            key: key.to_owned(),
            title: format!("The {key}"),
            task: format!("Build the {key}."),
            kind,
            owned_paths: owned.iter().map(|p| (*p).to_owned()).collect(),
            read_only_paths: Vec::new(),
            depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
            criteria: vec!["G1".to_owned()],
        }
    }

    fn run() -> RunId {
        RunId::new("run-01").unwrap()
    }

    #[test]
    fn divide_task_graph_valid() {
        let mut settings = Settings::default();
        settings.limits.protected_paths = vec!["**/*.lock".to_owned(), ".github/**".to_owned()];
        let good = TaskGraphProposal {
            cards: vec![
                card("api", ProposedKind::Interface, &["src/limiter/api.rs"], &[]),
                card(
                    "store",
                    ProposedKind::Build,
                    &["src/limiter/store/**"],
                    &["api"],
                ),
                card(
                    "middleware",
                    ProposedKind::Build,
                    &["src/middleware.rs"],
                    &["api"],
                ),
                card(
                    "docs",
                    ProposedKind::Docs,
                    &["docs/limiter.md"],
                    &["middleware"],
                ),
            ],
        };
        let cards = validate(&run(), &good, &settings).unwrap();
        let ids: Vec<&str> = cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["task-001", "task-002", "task-003", "task-004"]);
        assert_eq!(cards[1].depends_on, vec![CardId::numbered(1)]);
        assert_eq!(cards[3].kind, CardKind::Docs);

        let bad = TaskGraphProposal {
            cards: vec![
                card("a", ProposedKind::Build, &["src/a/**"], &["b"]),
                card("b", ProposedKind::Build, &["src/a/b.rs"], &["a"]),
                card("a", ProposedKind::Build, &[".argonauts/notes/x.md"], &[]),
                card("c", ProposedKind::Build, &["Cargo.lock"], &["nowhere"]),
                card("d", ProposedKind::Chore, &[], &[]),
                card("e", ProposedKind::Build, &["/etc/hosts"], &[]),
            ],
        };
        settings.divide.max_tasks = 5;
        let reasons = validate(&run(), &bad, &settings).unwrap_err();
        let has = |needle: &str| reasons.iter().any(|reason| reason.contains(needle));
        assert!(has("used by more than one task"), "{reasons:#?}");
        assert!(has("cycle"), "{reasons:#?}");
        assert!(has("which overlap"), "{reasons:#?}");
        assert!(has("inside `.argonauts/**`"), "{reasons:#?}");
        assert!(has("inside `**/*.lock`"), "{reasons:#?}");
        assert!(has("`nowhere`, which isn't a task"), "{reasons:#?}");
        assert!(has("owns no paths"), "{reasons:#?}");
        assert!(has("isn't relative to the repository"), "{reasons:#?}");
        assert!(has("more than divide.max_tasks (5)"), "{reasons:#?}");
    }

    #[test]
    fn a_cards_own_forbidden_paths_are_checked() {
        let settings = Settings::default();
        let proposal = TaskGraphProposal {
            cards: vec![card("a", ProposedKind::Build, &["src/a/**"], &[])],
        };
        let (mut cards, reasons) = convert(&proposal);
        assert!(reasons.is_empty());
        cards[0].forbidden_paths = vec!["src/a/secret/**".to_owned()];
        cards[0].owned_paths.push("src/a/secret/key.rs".to_owned());
        let reasons = rules(&cards, &settings);
        assert_eq!(reasons.len(), 1, "{reasons:#?}");
        assert!(
            reasons
                .iter()
                .any(|r| r.contains("inside `src/a/secret/**`"))
        );
    }

    #[test]
    fn no_card_owns_the_whole_repository_or_a_protected_area() {
        let mut settings = Settings::default();
        settings.limits.protected_paths = vec!["src/secrets/**".to_owned()];
        for owned in [".", "./"] {
            let whole = TaskGraphProposal {
                cards: vec![card("all", ProposedKind::Chore, &[owned], &[])],
            };
            let reasons = validate(&run(), &whole, &settings).unwrap_err();
            assert!(
                reasons
                    .iter()
                    .any(|r| r.contains("names the whole repository")),
                "{reasons:#?}"
            );
        }
        let holding = TaskGraphProposal {
            cards: vec![card("src", ProposedKind::Chore, &["src"], &[])],
        };
        let reasons = validate(&run(), &holding, &settings).unwrap_err();
        assert!(
            reasons.iter().any(|r| r.contains("holds `src/secrets/**`")),
            "{reasons:#?}"
        );
    }
}
