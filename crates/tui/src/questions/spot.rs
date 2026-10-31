//! The spot check of an auto decision: Agree `y`, Overrule `n`, Show the evidence `d`. It
//! answers with `UiCommand::SpotCheck` and never rings. Nothing waits on it, so Esc closes it,
//! and `/approvals` still lists it.

use argonauts_core::RunId;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalOpened,
};

use super::options::answer;
use super::records::{by_id, outcome};
use super::words;
use crate::bottom::{QuestionOption, QuestionPrompt};

pub fn spot_check_question(
    id: u64,
    opened: &ApprovalOpened,
    decided: Option<&ApprovalDecided>,
    run: Option<&RunId>,
    names: &DisplayNames,
) -> QuestionPrompt {
    let options = vec![
        QuestionOption::new("Agree"),
        QuestionOption::new("Overrule"),
        QuestionOption::view("Show the evidence"),
    ];
    let title = format!("Spot check · {}", opened.title.trim());
    let mut question = QuestionPrompt::new(id, title.clone(), title, options);
    if let Some(decided) = decided {
        question.detail.push(format!(
            "{} · decided by {}",
            outcome(opened, decided, run, names, &by_id),
            words::party(&decided.by, names)
        ));
    }
    question.question = Some("Was this the right call?".into());
    // Nothing waits on it, but it asks you, so it takes the keys as every question that asks does.
    question.needs_you = true;
    question.esc_hint = "Esc to skip".to_owned();
    question
}

/// What each option means, in the question's order. Only Agree counts as correct.
pub fn spot_check_options() -> Vec<AnswerOption> {
    vec![
        answer('y', "Agree", AnswerAction::Approve),
        answer('n', "Overrule", AnswerAction::Cancel),
        answer('d', "Show the evidence", AnswerAction::ShowDiff),
    ]
}
