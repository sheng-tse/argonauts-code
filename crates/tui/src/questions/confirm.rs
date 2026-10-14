//! The confirm built here before `t` takes over a task. It opens with the keys, and Esc cancels
//! with nothing sent.

use argonauts_core::events::approval::{AnswerAction, AnswerOption};
use argonauts_core::{RunId, TaskId};

use super::options::answer;
use crate::bottom::{QuestionOption, QuestionPrompt};

const CANCEL_HINT: &str = "Esc to cancel";

pub fn confirm_take_over(id: u64, run: &RunId, task: &TaskId, agent: &str) -> QuestionPrompt {
    let _ = run;
    let mut prompt = QuestionPrompt::new(
        id,
        format!("Take over {task} from {agent}?"),
        format!("Take over {task}?"),
        vec![QuestionOption::new("Take over")],
    )
    .yours();
    prompt.esc_hint = CANCEL_HINT.to_owned();
    prompt.esc_answers = true;
    prompt
}

pub fn take_over_options() -> Vec<AnswerOption> {
    vec![answer('t', "Take over", AnswerAction::TakeOver)]
}
