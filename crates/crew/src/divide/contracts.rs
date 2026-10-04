//! Contracts first (`divide.contracts_first`): one interface card (stubs that compile and fail
//! "not implemented"), then one acceptance-test card per build card, owned by that card's ring
//! reviewer, with the build card depending on it. Once the test cards are integrated, every goal
//! criterion must appear in some test card's `criteria_covered`.

use argonauts_core::CardId;
use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
use argonauts_core::events::goal::Criterion;

/// Where each build card's acceptance tests go: `tests/acceptance/<card>/`.
pub const ACCEPTANCE_DIR: &str = "tests/acceptance";

/// The graph with the test cards added and the edges set: every other card depends on the
/// interface cards and reads their paths, and each build card depends on its test card. The
/// order is interfaces, test cards, then the rest. `tests` is off when nobody writes them.
pub fn contracts_first(cards: Vec<CardSpec>, tests: bool) -> Vec<CardSpec> {
    let mut next = cards.len() as u32 + 1;
    let (interfaces, rest): (Vec<CardSpec>, Vec<CardSpec>) = cards
        .into_iter()
        .partition(|card| card.kind == CardKind::Interface);
    let interface_ids: Vec<CardId> = interfaces.iter().map(|card| card.id.clone()).collect();
    let frozen: Vec<String> = interfaces
        .iter()
        .flat_map(|card| card.owned_paths.iter().cloned())
        .collect();
    let mut test_cards = Vec::new();
    let mut others = Vec::with_capacity(rest.len());
    for mut card in rest {
        add_missing(&mut card.depends_on, &interface_ids);
        add_missing(&mut card.read_only_paths, &frozen);
        if tests && card.kind == CardKind::Build {
            let test = test_card(&card, next, &interface_ids, &frozen);
            next += 1;
            card.depends_on.push(test.id.clone());
            add_missing(&mut card.read_only_paths, &test.owned_paths);
            test_cards.push(test);
        }
        others.push(card);
    }
    interfaces
        .into_iter()
        .chain(test_cards)
        .chain(others)
        .collect()
}

/// Goal criteria no test card covers, by id. Any one blocks the build cards and escalates to
/// you. `covered` holds ids such as `G1`, or `G1: text`.
pub fn uncovered(criteria: &[Criterion], covered: &[String]) -> Vec<String> {
    let covered: Vec<&str> = covered.iter().map(|item| criterion_id(item)).collect();
    criteria
        .iter()
        .filter(|criterion| {
            !covered
                .iter()
                .any(|id| id.eq_ignore_ascii_case(criterion.id.trim()))
        })
        .map(|criterion| criterion.id.clone())
        .collect()
}

fn criterion_id(item: &str) -> &str {
    item.split(':').next().unwrap_or(item).trim()
}

fn test_card(build: &CardSpec, n: u32, interfaces: &[CardId], frozen: &[String]) -> CardSpec {
    let id = CardId::new(format!("test-{n:03}")).unwrap_or_else(|_| CardId::numbered(n));
    let dir = format!("{ACCEPTANCE_DIR}/{}", build.id);
    let criteria = if build.criteria.is_empty() {
        "the goal criteria it serves".to_owned()
    } else {
        build.criteria.join(", ")
    };
    CardSpec {
        id,
        title: format!("Acceptance tests for {}", build.title),
        task: format!(
            "Write acceptance tests for {} (\"{}\") in {dir}/. Test it only through its \
             interface. The tests must fail against the stubs, since nothing is built yet, and \
             together they must cover {criteria}. Don't edit the card's own paths.\n\nThe \
             card's task:\n\n{}",
            build.id, build.title, build.task
        ),
        kind: CardKind::AcceptanceTests {
            for_card: build.id.clone(),
        },
        owned_paths: vec![format!("{dir}/**")],
        read_only_paths: frozen.to_vec(),
        forbidden_paths: build.owned_paths.clone(),
        hidden_paths: Vec::new(),
        depends_on: interfaces.to_vec(),
        criteria: build.criteria.clone(),
        owner: None,
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: build.item.clone(),
    }
}

fn add_missing<T: Clone + PartialEq>(into: &mut Vec<T>, items: &[T]) {
    for item in items {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::Settings;
    use argonauts_core::steps::plan::{CardProposal, ProposedKind, TaskGraphProposal};

    use super::*;
    use crate::divide::graph;

    fn proposal() -> TaskGraphProposal {
        let card =
            |key: &str, kind, owned: &str, depends: &[&str], criteria: &[&str]| CardProposal {
                key: key.to_owned(),
                title: key.to_owned(),
                task: format!("Do {key}."),
                kind,
                owned_paths: vec![owned.to_owned()],
                read_only_paths: Vec::new(),
                depends_on: depends.iter().map(|d| (*d).to_owned()).collect(),
                criteria: criteria.iter().map(|c| (*c).to_owned()).collect(),
            };
        TaskGraphProposal {
            cards: vec![
                card("store", ProposedKind::Build, "src/store/**", &[], &["G1"]),
                card("api", ProposedKind::Interface, "src/api.rs", &[], &[]),
                card(
                    "middleware",
                    ProposedKind::Build,
                    "src/mw/**",
                    &["store"],
                    &["G2", "G3"],
                ),
                card(
                    "docs",
                    ProposedKind::Docs,
                    "docs/rate.md",
                    &["middleware"],
                    &["G3"],
                ),
            ],
        }
    }

    fn cards() -> Vec<CardSpec> {
        graph::convert(&proposal()).0
    }

    #[test]
    fn divide_contracts_first() {
        let cards = contracts_first(cards(), true);
        let ids: Vec<&str> = cards.iter().map(|card| card.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "card-002", "test-005", "test-006", "card-001", "card-003", "card-004"
            ]
        );
        let get = |id: &str| cards.iter().find(|card| card.id.as_str() == id).unwrap();
        let interface = CardId::new("card-002").unwrap();
        let test = get("test-005");
        assert_eq!(
            test.kind,
            CardKind::AcceptanceTests {
                for_card: CardId::numbered(1)
            }
        );
        assert_eq!(test.owned_paths, ["tests/acceptance/card-001/**"]);
        assert_eq!(test.forbidden_paths, ["src/store/**"]);
        assert_eq!(test.depends_on, std::slice::from_ref(&interface));
        assert_eq!(test.read_only_paths, ["src/api.rs"]);
        let store = get("card-001");
        assert_eq!(
            store.depends_on,
            [interface.clone(), CardId::new("test-005").unwrap()]
        );
        assert!(store.read_only_paths.contains(&"src/api.rs".to_owned()));
        assert!(
            store
                .read_only_paths
                .contains(&"tests/acceptance/card-001/**".to_owned())
        );
        // Docs aren't built, so they get no test card, but still wait for the interface.
        assert!(get("card-004").depends_on.contains(&interface));
        assert_eq!(cards.len(), 6);
        assert!(graph::rules(&cards, &Settings::default()).is_empty());

        let without_tests = contracts_first(self::cards(), false);
        assert_eq!(without_tests.len(), 4);
        assert_eq!(without_tests[0].kind, CardKind::Interface);
    }

    #[test]
    fn goal_criteria_become_acceptance_tests() {
        let cards = contracts_first(cards(), true);
        let tested: Vec<String> = cards
            .iter()
            .filter(|card| matches!(card.kind, CardKind::AcceptanceTests { .. }))
            .flat_map(|card| card.criteria.clone())
            .collect();
        assert_eq!(tested, ["G1", "G2", "G3"]);
        let criterion = |id: &str| Criterion::new(id, format!("criterion {id}"));
        let goal = [
            criterion("G1"),
            criterion("G2"),
            criterion("G3"),
            criterion("G4"),
        ];
        // What the test cards reported covering once they were integrated.
        let covered = ["G1".to_owned(), "g2: limits per key".to_owned()];
        assert_eq!(uncovered(&goal, &covered), ["G3", "G4"]);
        let all = ["G1", "G2", "G3", "G4"].map(str::to_owned);
        assert!(uncovered(&goal, &all).is_empty());
    }
}
