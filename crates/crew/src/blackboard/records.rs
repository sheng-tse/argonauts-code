//! Decision records. A settled question reopens only when a new message cites evidence the
//! record lacked, such as a failing test or a new file; otherwise it's answered with the
//! record's id.

use std::collections::BTreeSet;

use argonauts_core::base::names::MessageId;
use argonauts_core::runs::events::shared::BlackboardMessage;

use crate::blackboard::refs;
use crate::state::RunState;

/// How alike two messages' words must be to ask the same thing.
const SAME_QUESTION: f64 = 0.8;

/// The settled record this message asks about again, if any: the record of an earlier message
/// of the same kind that asks the same thing.
pub fn settled_by(state: &RunState, message: &BlackboardMessage) -> Option<MessageId> {
    let board = &state.blackboard;
    board
        .messages
        .iter()
        .take_while(|earlier| earlier.id != message.id)
        .filter(|earlier| earlier.kind == message.kind && same_question(earlier, message))
        .find_map(|earlier| board.settled(&earlier.id))
        .map(|record| record.id.clone())
}

/// Whether the message cites evidence the record lacked. Citing messages or decisions, the
/// record itself included, isn't evidence.
pub fn reopens(record: &BlackboardMessage, message: &BlackboardMessage) -> bool {
    message
        .refs
        .iter()
        .filter(|cited| !is_record(cited))
        .any(|cited| !record.refs.contains(cited))
}

fn is_record(reference: &str) -> bool {
    let id = |prefix: &str| {
        reference
            .strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    id("m-") || id("D-")
}

fn same_question(a: &BlackboardMessage, b: &BlackboardMessage) -> bool {
    let (a, b) = (words(&a.text), words(&b.text));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let shared = a.intersection(&b).count() as f64;
    let all = a.union(&b).count() as f64;
    shared / all >= SAME_QUESTION
}

fn words(text: &str) -> BTreeSet<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

/// The refs a record carries: what the question cited, plus what the answer cites.
pub fn record_refs(question: &BlackboardMessage, answer: &str) -> Vec<String> {
    let mut cited = question.refs.clone();
    for reference in refs::refs_in(answer) {
        if !cited.contains(&reference) {
            cited.push(reference);
        }
    }
    cited
}
