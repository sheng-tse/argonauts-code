//! Predictions (`divide.predictions`): each owner runs `Predict` for its card. Each becomes a
//! `prediction` line in the outcome log before any card starts, and the retro scores it. A
//! prediction never changes a card's path and never skips a gate.

use argonauts_core::events::card::CardSpec;
use argonauts_core::names::RoleName;
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::BudgetUnit;
use argonauts_core::steps::card::{Predict, PredictInput, Predictions};
use argonauts_core::worker::Access;

use crate::divide::{assign, repo_state_dir};
use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts;
use crate::retro::outcomes::{self, LogLine, Prediction, Predictor};
use crate::workflow::StepAt;

/// The card's owner, asked without tools.
pub async fn predict(
    host: &Host,
    at: Option<StepAt>,
    card: &CardSpec,
) -> EngineResult<Predictions> {
    let owner = card
        .owner
        .clone()
        .ok_or_else(|| EngineError::Refused(format!("{} has no owner to predict it", card.id)))?;
    let input = PredictInput {
        card: format!("{}\n\n{}", card.title, card.task),
        time_cap_min: time_cap_min(card, host.settings()),
    };
    let mut ask = Ask::new(at, RoleName::Owner, owner, prompts::divide::PREDICT, input);
    ask.card = Some(card.id.clone());
    ask.item.clone_from(&card.item);
    ask.access = Access::NoTools;
    Ok(clamped(host.ask::<Predict>(ask).await?.output))
}

/// Appends a `prediction` line for each predicted card not logged yet, so a re-divided plan
/// logs nothing twice.
pub fn log(host: &Host, cards: &[CardSpec], run_task_type: Option<&str>) -> EngineResult<()> {
    let state_dir = repo_state_dir(host);
    let logged: Vec<_> = outcomes::run_lines(&state_dir, host.run_id())?
        .into_iter()
        .filter_map(|line| match line {
            LogLine::Prediction(prediction) => Some(prediction.card),
            _ => None,
        })
        .collect();
    let lines: Vec<LogLine> = cards
        .iter()
        .filter(|card| !logged.contains(&card.id))
        .filter_map(|card| {
            let predicted = card.predictions?;
            Some(LogLine::Prediction(Prediction {
                run: host.run_id().clone(),
                card: card.id.clone(),
                worker: card.owner.clone()?,
                role: Predictor::Owner,
                task_type: assign::task_type(&card.kind, run_task_type),
                at: host.now(),
                p_pass_gates_first_try: Some(predicted.p_pass_gates_first_try),
                p_within_time_cap: Some(predicted.p_within_time_cap),
                p_at_most_two_attempts: Some(predicted.p_at_most_two_attempts),
                p_passes_integration: None,
                expected_minutes: None,
            }))
        })
        .collect();
    outcomes::append_lines(&state_dir, host.run_id(), &lines)
}

/// The card's own time limit, else a per-card budget in minutes, else the wall-clock watchdog.
pub fn time_cap_min(card: &CardSpec, settings: &Settings) -> f64 {
    let in_minutes = card
        .budget
        .limit
        .filter(|limit| limit.unit == BudgetUnit::Minutes)
        .map(|limit| limit.amount);
    card.budget
        .max_minutes
        .or(in_minutes)
        .or(settings.budget.per_card_minutes)
        .unwrap_or(settings.watchdogs.wall_clock_min)
}

/// Probabilities kept between 0 and 1; a missing one says nothing either way.
pub fn clamped(predictions: Predictions) -> Predictions {
    let clamp = |p: f64| {
        if p.is_finite() {
            p.clamp(0.0, 1.0)
        } else {
            0.5
        }
    };
    Predictions {
        p_pass_gates_first_try: clamp(predictions.p_pass_gates_first_try),
        p_within_time_cap: clamp(predictions.p_within_time_cap),
        p_at_most_two_attempts: clamp(predictions.p_at_most_two_attempts),
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::CardId;
    use argonauts_core::events::card::{CardBudget, CardKind};
    use argonauts_core::settings::budget::BudgetAmount;

    use super::*;

    #[test]
    fn probabilities_are_clamped() {
        let wild = Predictions {
            p_pass_gates_first_try: 1.4,
            p_within_time_cap: -0.2,
            p_at_most_two_attempts: f64::NAN,
        };
        let fixed = clamped(wild);
        assert_eq!(fixed.p_pass_gates_first_try, 1.0);
        assert_eq!(fixed.p_within_time_cap, 0.0);
        assert_eq!(fixed.p_at_most_two_attempts, 0.5);
    }

    #[test]
    fn the_time_cap_comes_from_the_card_first() {
        let settings = Settings::default();
        let mut card = CardSpec {
            id: CardId::numbered(1),
            title: "t".into(),
            task: "t".into(),
            kind: CardKind::Build,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            owner: None,
            reviewers: Vec::new(),
            budget: CardBudget::default(),
            predictions: None,
            item: None,
        };
        assert_eq!(
            time_cap_min(&card, &settings),
            settings.watchdogs.wall_clock_min
        );
        card.budget.limit = Some(BudgetAmount {
            unit: BudgetUnit::Minutes,
            amount: 12.0,
        });
        assert_eq!(time_cap_min(&card, &settings), 12.0);
        card.budget.max_minutes = Some(9.0);
        assert_eq!(time_cap_min(&card, &settings), 9.0);
    }
}
