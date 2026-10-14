//! The router: utility from the weights, seeded Thompson draws, and the limits on exploring.

mod records_support;

use rand::SeedableRng;
use rand::rngs::StdRng;

use argonauts_core::limits::TrackRecord;
use argonauts_core::settings::protocol::{RoutingSettings, RoutingWeights};
use argonauts_learning::records::{Beta, Record};
use argonauts_learning::routing::{self, RunSoFar};
use argonauts_learning::types::Assignment;

use records_support::{close, worker};

/// A record whose rates sit at `pass` and `approval`, worth `tasks` tasks each.
fn record(name: &str, pass: f64, approval: f64, tasks: f64) -> Record {
    Record {
        worker: worker(name),
        task_type: "backend".to_owned(),
        pooled_from: None,
        model_id: None,
        pass: Beta::prior(pass, tasks),
        approval: Beta::prior(approval, tasks),
        cost_ratio: 1.0,
        time_ratio: 1.0,
        prediction: 0.0,
        evidence: tasks,
        counts: TrackRecord { passed: 0, of: 0 },
        pinned: false,
    }
}

fn first(ranked: &[routing::Ranked]) -> String {
    ranked[0].worker.to_string()
}

#[test]
fn router_utility_follows_weights() {
    let mut scored = record("codex", 0.8, 0.6, 10.0);
    scored.prediction = 0.5;
    scored.cost_ratio = 1.5;
    scored.time_ratio = 0.5;
    let weights = RoutingWeights::default();
    let expected = 1.0 * 0.8 + 0.5 * 0.6 + 0.3 * 0.5 - 0.4 * 1.5 - 0.2 * 0.5;
    assert!(close(
        routing::utility(&scored, 0.8, 0.6, &weights),
        expected,
        1e-12
    ));
    let only_cost = RoutingWeights {
        pass_rate: 0.0,
        approval_rate: 0.0,
        prediction: 0.0,
        cost: 1.0,
        time: 0.0,
    };
    assert!(close(
        routing::utility(&scored, 0.8, 0.6, &only_cost),
        -1.5,
        1e-12
    ));

    // A better first-pass rate at twice the cost loses by default and wins when cost is free.
    let mut dear = record("claude-code", 0.7, 0.5, 1e6);
    dear.cost_ratio = 2.0;
    let mut cheap = record("codex", 0.6, 0.5, 1e6);
    cheap.cost_ratio = 0.5;
    let eligible = [dear, cheap];
    let mut rng = StdRng::seed_from_u64(1);
    let ranked = routing::rank(&eligible, &weights, &mut rng);
    assert_eq!(first(&ranked), "codex");
    assert!(close(ranked[0].mean, 0.6 + 0.25 - 0.2 - 0.2, 1e-9));
    let quality_only = RoutingWeights {
        cost: 0.0,
        ..weights.clone()
    };
    assert_eq!(
        first(&routing::rank(&eligible, &quality_only, &mut rng)),
        "claude-code"
    );
}

#[test]
fn router_draw_seeded_with_probability() {
    let weights = RoutingWeights::default();
    let eligible = [
        record("codex", 0.6, 0.5, 4.0),
        record("claude-code", 0.55, 0.5, 4.0),
        record("agy", 0.3, 0.5, 40.0),
    ];
    let draw = |seed| routing::rank(&eligible, &weights, &mut StdRng::seed_from_u64(seed));
    assert_eq!(draw(7), draw(7), "the same seed gives the same draw");
    let firsts: std::collections::BTreeSet<String> =
        (0..50).map(|seed| first(&draw(seed))).collect();
    assert!(
        firsts.len() > 1,
        "wide posteriors sometimes rank first: {firsts:?}"
    );
    for ranked in draw(3).windows(2) {
        assert!(ranked[0].sampled >= ranked[1].sampled);
    }

    // With one seed, every candidate's chance comes from the same draws, so they add up to 1.
    let probability = |name: &str| {
        let mut rng = StdRng::seed_from_u64(11);
        routing::choice_probability(&eligible, &worker(name), &weights, &mut rng, 2_000)
    };
    let chances: Vec<f64> = ["codex", "claude-code", "agy"].map(probability).to_vec();
    assert!(close(chances.iter().sum(), 1.0, 1e-12), "{chances:?}");
    assert!(
        chances[0] > chances[1] && chances[1] > chances[2],
        "{chances:?}"
    );
    assert_eq!(probability("codex"), probability("codex"));

    let mut rng = StdRng::seed_from_u64(5);
    assert_eq!(
        routing::choice_probability(&eligible, &worker("opencode"), &weights, &mut rng, 100),
        0.0
    );
    assert_eq!(
        routing::choice_probability(&eligible[..1], &worker("codex"), &weights, &mut rng, 100),
        1.0
    );
    let sure = [record("codex", 0.9, 0.9, 1e6), record("agy", 0.1, 0.1, 1e6)];
    assert_eq!(
        routing::choice_probability(&sure, &worker("codex"), &weights, &mut rng, 500),
        1.0
    );
}

#[test]
fn router_exploration_guardrails() {
    let settings = RoutingSettings::default();
    let labels = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    let room = RunSoFar {
        routed: 9,
        explored: 0,
    };
    let may = |labels: &[String], protected: bool, reason: Option<Assignment>, so_far: RunSoFar| {
        routing::may_explore(labels, protected, reason, so_far, &settings)
    };

    assert!(may(&labels(&["api"]), false, None, room));
    assert!(may(&[], false, Some(Assignment::Explore), room));
    assert!(!may(&labels(&["api", "Security"]), false, None, room));
    assert!(!may(&labels(&["MIGRATION"]), false, None, room));
    assert!(!may(&[], true, None, room), "protected paths never explore");
    assert!(!may(&[], false, Some(Assignment::Fallback), room));
    assert!(!may(&[], false, Some(Assignment::Retry), room));

    // One task in five: the fifth task may be the first to explore, the tenth the second.
    let so_far = |routed, explored| RunSoFar { routed, explored };
    assert!(!may(&[], false, None, so_far(0, 0)));
    assert!(!may(&[], false, None, so_far(3, 0)));
    assert!(may(&[], false, None, so_far(4, 0)));
    assert!(!may(&[], false, None, so_far(8, 1)));
    assert!(may(&[], false, None, so_far(9, 1)));

    let never = RoutingSettings {
        max_explore_share: 0.0,
        ..settings.clone()
    };
    assert!(!routing::may_explore(&[], false, None, room, &never));
    let always = RoutingSettings {
        max_explore_share: 1.0,
        no_explore_labels: vec!["infra".to_owned()],
        ..settings.clone()
    };
    assert!(routing::may_explore(
        &labels(&["security"]),
        false,
        None,
        so_far(0, 0),
        &always
    ));
    assert!(!routing::may_explore(
        &labels(&["Infra"]),
        false,
        None,
        so_far(0, 0),
        &always
    ));
}
