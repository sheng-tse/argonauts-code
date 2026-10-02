//! Predictions: Brier skill against each worker's own base rate, reliability, and overconfidence.

mod records_support;

use argonauts_core::settings::protocol::RoutingSettings;
use argonauts_learning::outcomes::{LogLine, Outcome, Predictor};
use argonauts_learning::predictions::{self, Calibration};
use argonauts_learning::records::Book;
use argonauts_learning::types::Assignment;

use records_support::{AS_OF, close, outcome, prediction, worker};

/// Card `n`'s outcome and its owner's prediction of a first-try pass.
fn predicted(n: u32, owner: &str, task_type: &str, p: f64, first_try: bool) -> [LogLine; 2] {
    [
        LogLine::Prediction(prediction(n, owner, task_type, p)),
        LogLine::Outcome(outcome(n, owner, task_type, first_try, 0.5)),
    ]
}

fn find<'a>(scores: &'a [Calibration], owner: &str, metric: &str) -> &'a Calibration {
    scores
        .iter()
        .find(|score| score.worker == worker(owner) && score.metric == metric)
        .unwrap_or_else(|| panic!("no {metric} for {owner} in {scores:?}"))
}

#[test]
fn brier_skill_against_base_rate() {
    assert!(close(predictions::brier(0.7, true), 0.09, 1e-12));
    assert!(close(predictions::brier(0.7, false), 0.49, 1e-12));

    let outcomes = [true, true, true, false];
    let at = |p: f64| outcomes.map(|happened| (p, happened));
    assert!(
        close(predictions::skill(&at(0.75), 0.75), 0.0, 1e-12),
        "the average says nothing"
    );
    let perfect = outcomes.map(|happened| (if happened { 1.0 } else { 0.0 }, happened));
    assert!(close(predictions::skill(&perfect, 0.75), 1.0, 1e-12));
    let backwards = outcomes.map(|happened| (if happened { 0.0 } else { 1.0 }, happened));
    assert!(predictions::skill(&backwards, 0.75) < 0.0);
    assert_eq!(predictions::skill(&[], 0.5), 0.0);
    assert_eq!(predictions::skill(&[(0.8, true), (0.9, true)], 1.0), 0.0);

    // The base rate is the worker's own on each task type: backend passes three of four, docs
    // one of four. Predicting each type's rate scores 0; predicting the overall half loses.
    let mut lines = Vec::new();
    let mut n = 0;
    for (task_type, rate, results) in [
        ("backend", 0.75, [true, true, true, false]),
        ("docs", 0.25, [true, false, false, false]),
    ] {
        for first_try in results {
            n += 1;
            lines.extend(predicted(n, "codex", task_type, rate, first_try));
            lines.extend(predicted(100 + n, "agy", task_type, 0.5, first_try));
        }
    }
    let scores = predictions::calibration(&lines, &RoutingSettings::default());
    let typed = find(&scores, "codex", predictions::PASS_GATES_FIRST_TRY);
    assert_eq!(typed.count, 8);
    assert!(close(typed.skill, 0.0, 1e-12));
    assert!(find(&scores, "agy", predictions::PASS_GATES_FIRST_TRY).skill < 0.0);

    // A worker whose predictions tell its cards apart gets a positive routing score, shrunk
    // while there are few.
    let mut sharp = Vec::new();
    for i in 0..6 {
        let first_try = i % 2 == 0;
        sharp.extend(predicted(
            200 + i,
            "codex",
            "backend",
            if first_try { 0.9 } else { 0.1 },
            first_try,
        ));
    }
    let settings = RoutingSettings::default();
    let skill = find(
        &predictions::calibration(&sharp, &settings),
        "codex",
        predictions::PASS_GATES_FIRST_TRY,
    )
    .skill;
    assert!(close(skill, 0.96, 1e-9));
    let book = Book::build(&sharp, &settings, AS_OF);
    let score = book.record(&worker("codex"), "backend").prediction;
    assert!(close(score, 0.96 * 6.0 / 9.0, 1e-9), "{score}");
    assert_eq!(book.record(&worker("agy"), "backend").prediction, 0.0);
}

#[test]
fn reliability_bins_and_decomposition() {
    let pairs = [
        (0.05, false),
        (0.15, false),
        (0.3, true),
        (0.35, false),
        (0.9, true),
        (1.0, true),
        (1.0, false),
    ];
    let bins = predictions::reliability(&pairs);
    let shape: Vec<(f64, u32)> = bins.iter().map(|bin| (bin.lower, bin.count)).collect();
    assert_eq!(
        shape,
        [(0.0, 1), (0.1, 1), (0.3, 2), (0.9, 3)],
        "empty bins left out"
    );
    let top = &bins[3];
    assert!(close(top.upper, 1.0, 1e-12));
    assert!(close(top.mean_predicted, 2.9 / 3.0, 1e-12));
    assert!(close(top.actual_rate, 2.0 / 3.0, 1e-12));
    assert!(close(bins[2].mean_predicted, 0.325, 1e-12));
    assert!(close(bins[2].actual_rate, 0.5, 1e-12));

    let parts = predictions::decompose(&pairs);
    let brier = pairs
        .iter()
        .map(|&(p, happened)| predictions::brier(p, happened))
        .sum::<f64>()
        / pairs.len() as f64;
    assert!(close(
        parts.reliability - parts.resolution + parts.uncertainty,
        brier,
        1e-12
    ));
    assert!(close(parts.uncertainty, (3.0 / 7.0) * (4.0 / 7.0), 1e-12));

    // Sure and right: nothing to calibrate, and every bit of uncertainty resolved.
    let sharp = [(1.0, true), (0.0, false), (1.0, true), (0.0, false)];
    let parts = predictions::decompose(&sharp);
    assert!(close(parts.reliability, 0.0, 1e-12));
    assert!(close(parts.resolution, parts.uncertainty, 1e-12));
    assert_eq!(predictions::decompose(&[]), Default::default());
    assert!(predictions::reliability(&[]).is_empty());
}

#[test]
fn overconfidence_needs_enough_predictions() {
    let settings = RoutingSettings::default();
    assert_eq!(settings.overconfident_after, 10);
    let mut lines = Vec::new();
    for n in 0..9 {
        lines.extend(predicted(n, "codex", "backend", 0.9, n < 3));
        lines.extend(predicted(100 + n, "claude-code", "backend", 0.4, n < 4));
    }
    let scores = predictions::calibration(&lines, &settings);
    let codex = find(&scores, "codex", predictions::PASS_GATES_FIRST_TRY);
    assert_eq!(codex.count, 9);
    assert!(codex.mean_predicted - codex.actual_rate > settings.overconfident_margin);
    assert!(
        !codex.overconfident,
        "nine predictions aren't enough to say"
    );

    lines.extend(predicted(9, "codex", "backend", 0.9, false));
    lines.extend(predicted(109, "claude-code", "backend", 0.4, false));
    let scores = predictions::calibration(&lines, &settings);
    assert!(find(&scores, "codex", predictions::PASS_GATES_FIRST_TRY).overconfident);
    assert!(!find(&scores, "claude-code", predictions::PASS_GATES_FIRST_TRY).overconfident);
    let lenient = RoutingSettings {
        overconfident_margin: 0.7,
        ..settings.clone()
    };
    let scores = predictions::calibration(&lines, &lenient);
    assert!(!find(&scores, "codex", predictions::PASS_GATES_FIRST_TRY).overconfident);
}

#[test]
fn predictions_pair_with_the_right_outcome() {
    let settings = RoutingSettings::default();
    let mut owner = prediction(1, "codex", "backend", 0.8);
    owner.p_within_time_cap = Some(0.6);
    owner.p_at_most_two_attempts = Some(0.9);
    let mut reviewer = prediction(1, "claude-code", "backend", 0.0);
    reviewer.role = Predictor::Reviewer;
    reviewer.p_pass_gates_first_try = None;
    reviewer.p_passes_integration = Some(0.3);
    // The first owner stopped; a fallback finished the card and it integrated.
    let mut stopped = prediction(2, "agy", "backend", 0.7);
    stopped.p_within_time_cap = None;
    let mut taken_over: Outcome = outcome(2, "opencode", "backend", false, 0.5);
    taken_over.assigned = Some(Assignment::Fallback);

    let mut finished = outcome(1, "codex", "backend", false, 0.5);
    finished.attempts = 3;
    finished.within_time_cap = Some(true);
    finished.integrated = false;
    let lines = vec![
        LogLine::Prediction(owner),
        LogLine::Prediction(reviewer),
        LogLine::Prediction(stopped),
        LogLine::Outcome(finished),
        LogLine::Outcome(taken_over),
    ];
    let scores = predictions::calibration(&lines, &settings);
    let brier_of = |owner: &str, metric: &str| find(&scores, owner, metric).brier;
    assert!(close(
        brier_of("codex", predictions::PASS_GATES_FIRST_TRY),
        0.64,
        1e-12
    ));
    assert!(close(
        brier_of("codex", predictions::WITHIN_TIME_CAP),
        0.16,
        1e-12
    ));
    assert!(close(
        brier_of("codex", predictions::AT_MOST_TWO_ATTEMPTS),
        0.81,
        1e-12
    ));
    assert!(close(
        brier_of("claude-code", predictions::PASSES_INTEGRATION),
        0.09,
        1e-12
    ));
    assert!(
        scores.iter().all(|score| score.worker != worker("agy")),
        "a fallback's result never scores the owner it replaced"
    );
    assert_eq!(scores.len(), 4);
}
