//! The question a CLI that is down asks: which tasks wait on it, and whether to wait with
//! them, give them all to one agent, or choose an agent for each. Its title names the CLI, what
//! went wrong sits beside it, and every option says what picking it means.

use std::time::Instant;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::{AgentDownPacket, AnswerAction, AnswerOption};
use time::{OffsetDateTime, UtcOffset};

use super::words;
use crate::bottom::{Countdown, OptionModel, QuestionPrompt};

/// What the waiting option says once your own setting is what keeps the tasks waiting.
const YOUR_SETTING: &str = "your setting";

pub(super) fn title(down: &AgentDownPacket, names: &DisplayNames) -> String {
    format!("{} is down", names.cli(&down.agent_id.0))
}

/// Fills in what went wrong, what is paused, and each option's explanation. An explanation
/// the event carries itself is kept. The pointer starts on waiting when a setting of yours
/// already has the tasks wait.
pub(super) fn question(
    question: &mut QuestionPrompt,
    down: &AgentDownPacket,
    answers: &[AnswerOption],
    offset: UtcOffset,
    names: &DisplayNames,
) {
    let cli = names.cli(&down.agent_id.0);
    question.detail.push(down.evidence.clone());
    if let Some(paused) = paused_words(down, &cli) {
        question.detail.push(paused);
    }
    let paused = down.paused.len();
    for (index, (option, answer)) in question.options.iter_mut().zip(answers).enumerate() {
        if option.description.is_some() {
            continue;
        }
        match &answer.action {
            AnswerAction::Wait => {
                let retried = down
                    .test_at
                    .map(|at| format!("tried again at {}", words::clock(at, offset)));
                let yours = down.kept_by.as_ref().map(|_| YOUR_SETTING.to_owned());
                let said: Vec<String> = yours.into_iter().chain(retried).collect();
                option.description = (!said.is_empty()).then(|| said.join(" · "));
                if down.kept_by.is_some() {
                    question.selected = index;
                }
            }
            AnswerAction::OneForAll => {
                let Some(worker) = &down.fits_all else {
                    continue;
                };
                let (name, cli) = (names.short(worker), names.cli(&worker.agent));
                let fits = match paused {
                    0 | 1 => "can take it".to_owned(),
                    2 => "fits both".to_owned(),
                    all => format!("fits all {all}"),
                };
                // An agent that names no model goes by its CLI, which is then said once.
                option.description = Some(if name == cli {
                    fits
                } else {
                    format!("on {cli} {fits}")
                });
                option.model = Some(OptionModel {
                    name,
                    cli: worker.agent.clone(),
                });
            }
            AnswerAction::PerTask => {
                let each = match paused {
                    0 | 1 => "an agent of your choice",
                    _ => "an agent for each",
                };
                option.description = Some(format!("{each}, from every installed CLI"));
            }
            _ => {}
        }
    }
}

/// Has the waiting option count down to the CLI's next test, where the clock time it would
/// otherwise name can't be read at a glance. `wall` is the time on the wall clock at `now`.
pub(super) fn count_down(
    question: &mut QuestionPrompt,
    down: &AgentDownPacket,
    answers: &[AnswerOption],
    now: Instant,
    wall: OffsetDateTime,
) {
    let Some(test_at) = down.test_at else {
        return;
    };
    let left = std::time::Duration::try_from(test_at - wall).unwrap_or_default();
    for (option, answer) in question.options.iter_mut().zip(answers) {
        if answer.action != AnswerAction::Wait || answer.detail.is_some() {
            continue;
        }
        option.description = down.kept_by.as_ref().map(|_| YOUR_SETTING.to_owned());
        option.counts_to = Some(Countdown {
            words: "tried again in".to_owned(),
            at: now + left,
        });
    }
}

/// `Two tasks wait for it, their work so far kept. same_cli keeps them on Claude Code.`
fn paused_words(down: &AgentDownPacket, cli: &str) -> Option<String> {
    let (count, wait, their, them) = match down.paused.len() {
        0 => return None,
        1 => ("One task".to_owned(), "waits", "its", "it"),
        many => (
            format!("{} tasks", number_word(many)),
            "wait",
            "their",
            "them",
        ),
    };
    let mut said = format!("{count} {wait} for it, {their} work so far kept.");
    if let Some(setting) = down.kept_by.as_deref().map(str::trim)
        && !setting.is_empty()
    {
        said.push_str(&format!(" {setting} keeps {them} on {cli}."));
    }
    Some(said)
}

/// A count as a sentence starts with it: a word up to twelve, digits after.
fn number_word(count: usize) -> String {
    const WORDS: [&str; 11] = [
        "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten", "Eleven", "Twelve",
    ];
    count
        .checked_sub(2)
        .and_then(|at| WORDS.get(at))
        .map_or_else(|| count.to_string(), |word| (*word).to_owned())
}

#[cfg(test)]
mod tests;
