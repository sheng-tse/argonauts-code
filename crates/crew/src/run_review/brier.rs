//! The Brier score of a probability forecast against what happened: 0 is perfect, 1 is worst.

use argonauts_core::TaskId;
use argonauts_core::runs::events::shared::RunReviewEvent;
use argonauts_core::workflows::steps::task::Predictions;

pub fn brier(predicted: f64, happened: bool) -> f64 {
    let outcome = if happened { 1.0 } else { 0.0 };
    (predicted - outcome).powi(2)
}

/// What happened to a task, in the terms its worker predicted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Happened {
    pub passed_checks_first_try: bool,
    pub within_time_cap: bool,
    pub at_most_two_tries: bool,
}

/// One `RunReview::Scored` per prediction, named by its field.
pub fn score(task: &TaskId, predictions: &Predictions, happened: &Happened) -> Vec<RunReviewEvent> {
    [
        (
            "p_pass_checks_first_try",
            predictions.p_pass_checks_first_try,
            happened.passed_checks_first_try,
        ),
        (
            "p_within_time_cap",
            predictions.p_within_time_cap,
            happened.within_time_cap,
        ),
        (
            "p_at_most_two_tries",
            predictions.p_at_most_two_tries,
            happened.at_most_two_tries,
        ),
    ]
    .into_iter()
    .map(|(metric, predicted, outcome)| RunReviewEvent::Scored {
        task: task.clone(),
        metric: metric.to_owned(),
        predicted,
        outcome,
        brier: brier(predicted, outcome),
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_review_scores_predictions_brier() {
        assert_eq!(brier(1.0, true), 0.0);
        assert_eq!(brier(0.0, true), 1.0);
        assert!((brier(0.7, true) - 0.09).abs() < 1e-12);
        assert!((brier(0.7, false) - 0.49).abs() < 1e-12);
        assert_eq!(brier(0.5, false), 0.25);

        let predictions = Predictions {
            p_pass_checks_first_try: 0.8,
            p_within_time_cap: 0.6,
            p_at_most_two_tries: 0.9,
        };
        let happened = Happened {
            passed_checks_first_try: false,
            within_time_cap: true,
            at_most_two_tries: true,
        };
        let events = score(&TaskId::numbered(2), &predictions, &happened);
        let scores: Vec<(String, bool, f64)> = events
            .into_iter()
            .map(|event| match event {
                RunReviewEvent::Scored {
                    task,
                    metric,
                    outcome,
                    brier,
                    ..
                } => {
                    assert_eq!(task, TaskId::numbered(2));
                    (metric, outcome, (brier * 100.0).round() / 100.0)
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            scores,
            [
                ("p_pass_checks_first_try".to_owned(), false, 0.64),
                ("p_within_time_cap".to_owned(), true, 0.16),
                ("p_at_most_two_tries".to_owned(), true, 0.01),
            ]
        );
    }
}
