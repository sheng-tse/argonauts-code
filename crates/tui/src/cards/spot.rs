//! The spot check of an auto decision: Agree `y`, Overrule `n`, Show the evidence `d`. It
//! answers with `UiCommand::SpotCheck`, never rings, and can be set aside like a run's card.

use argonauts_core::RunId;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, CheckpointDecided, CheckpointOpened,
};

use super::options::answer;
use super::records::{by_id, outcome};
use super::words;
use crate::bottom::{CardOption, CardPrompt};

pub fn spot_check_card(
    id: u64,
    opened: &CheckpointOpened,
    decided: Option<&CheckpointDecided>,
    run: Option<&RunId>,
    names: &DisplayNames,
) -> CardPrompt {
    let options = vec![
        CardOption::new("Agree"),
        CardOption::new("Overrule"),
        CardOption::view("Show the evidence"),
    ];
    let title = format!("Spot check · {}", opened.title.trim());
    let mut card = CardPrompt::new(id, title.clone(), title, options);
    if let Some(decided) = decided {
        card.detail.push(format!(
            "{} · decided by {}",
            outcome(opened, decided, run, names, &by_id),
            words::party(&decided.by, names)
        ));
    }
    card.question = Some("Was this the right call?".into());
    // Nothing waits on it, but it asks you, so it takes the keys as every card that asks does.
    card.needs_you = true;
    card
}

/// What each option means, in the card's order. Only Agree counts as correct.
pub fn spot_check_options() -> Vec<AnswerOption> {
    vec![
        answer('y', "Agree", AnswerAction::Approve),
        answer('n', "Overrule", AnswerAction::Cancel),
        answer('d', "Show the evidence", AnswerAction::ShowDiff),
    ]
}
