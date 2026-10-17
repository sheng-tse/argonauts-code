//! A question for you that was answered in your place on autopilot: who answered, in which part,
//! what and why, and what the task does next.

use argonauts_core::events::shared::AnsweredForYou;
use argonauts_core::names::RoleName;

use crate::cell::{Notice, Tone};
use crate::text::sanitize_line;

use super::{Cx, emit};

/// `⏺ Autopilot answered build:fix-the-money's question`, then under it the answer, who gave
/// it and why, and what follows. `by` is the answerer as the screen names it.
pub(super) fn answered_for_you(answered: &AnsweredForYou, by: Option<&str>, cx: &mut Cx<'_>) {
    let asker = sanitize_line(answered.asker.trim());
    let title = if asker.is_empty() {
        "Autopilot answered a question".to_owned()
    } else {
        format!("Autopilot answered {asker}'s question")
    };
    let notice = Notice::headline(Tone::Normal, title, None)
        .with_detail(answer_line(answered, by))
        .with_detail(follows(answered));
    emit(cx, notice);
}

/// `Block the merge · Opus 5.5, the main agent: the sign-off already decides`.
fn answer_line(answered: &AnsweredForYou, by: Option<&str>) -> String {
    let who = match (by, answered.role) {
        (Some(by), Some(role)) => Some(format!("{by}, {}", role_words(role))),
        (Some(by), None) => Some(by.to_owned()),
        (None, _) => None,
    };
    let mut line = answered.answer.trim().to_owned();
    match (who, answered.why.as_deref().map(str::trim)) {
        (Some(who), Some(why)) if !why.is_empty() => line.push_str(&format!(" · {who}: {why}")),
        (Some(who), _) => line.push_str(&format!(" · {who}")),
        (None, Some(why)) if !why.is_empty() => line.push_str(&format!(" · {why}")),
        (None, _) => {}
    }
    line
}

fn role_words(role: RoleName) -> String {
    match role {
        RoleName::Main => "the main agent".to_owned(),
        role => format!("the {}", role.as_str()),
    }
}

fn follows(answered: &AnsweredForYou) -> String {
    let next = if answered.finished {
        "the task goes on to its review rounds"
    } else {
        "the task carries on with this answer"
    };
    format!("{next}; the merge question will show the question and this answer")
}
