//! The confirm built here before `t` takes over a card. It opens with the keys, and Esc cancels
//! with nothing sent.

use argonauts_core::events::checkpoint::{AnswerAction, AnswerOption};
use argonauts_core::{CardId, RunId};

use super::options::answer;
use crate::bottom::{CardOption, CardPrompt};

const CANCEL_HINT: &str = "Esc to cancel";

pub fn confirm_take_over(id: u64, run: &RunId, card: &CardId, agent: &str) -> CardPrompt {
    let _ = run;
    let mut prompt = CardPrompt::new(
        id,
        format!("Take over {card} from {agent}?"),
        format!("Take over {card}?"),
        vec![CardOption::new("Take over")],
    )
    .yours();
    prompt.esc_hint = CANCEL_HINT.to_owned();
    prompt.esc_answers = true;
    prompt
}

pub fn take_over_options() -> Vec<AnswerOption> {
    vec![answer('t', "Take over", AnswerAction::TakeOver)]
}
