//! A turn that ends with a question instead of its answer. Nobody reads the end
//! of a turn, so while a run decides without you the session is told to decide and carry on,
//! `approvals.ask_limit` times, then once to take the option it recommended. One more question
//! after that is an answer that doesn't match, like any other.

use crate::events::session::{InputPart, InputPartKind, SessionEnd};

/// What the session reads in place of an answer, when its turn ended with a question and it
/// may still be told. `asked_before` is how many of its turns were answered this way already.
/// `ask_limit` is `None` where nobody replies for you, as in supervised mode; a limit of 0
/// tells it at once to take the option it recommended. `None` past the limit, or when it
/// didn't ask.
pub fn reply(end: &SessionEnd, asked_before: u32, ask_limit: Option<u32>) -> Option<InputPart> {
    let ask_limit = ask_limit?;
    if asked_before > ask_limit || !ends_asking(end) {
        return None;
    }
    Some(decide_part(asked_before >= ask_limit))
}

/// Whether the session's last words ask something: its final line ends with a question mark,
/// once quotes, emphasis and brackets after it are set aside.
pub fn ends_asking(end: &SessionEnd) -> bool {
    let said = end.last_message.as_deref().or(end.result_text.as_deref());
    let Some(last) = said.and_then(|text| text.lines().rev().find(|line| !line.trim().is_empty()))
    else {
        return false;
    };
    let closing = |c: char| c.is_whitespace() || "\"'”’`*_)]>".contains(c);
    last.trim_end_matches(closing).ends_with(['?', '？'])
}

/// The label of the part [`reply`] returns, as the session's input records it.
pub const LABEL: &str = "decide and carry on";

/// Past the limit the session is told to take the option it recommended.
fn decide_part(past_the_limit: bool) -> InputPart {
    let text = if past_the_limit {
        "No one will answer that. Take the option you recommended, or the safest one if you \
         recommended none, finish the work, and reply with the JSON object you were asked for. \
         Say in it what you assumed."
    } else {
        "No one will answer that: the user asked not to be stopped for questions. Decide \
         yourself, carry on with the work, and reply with the JSON object you were asked for. \
         Say in it what you assumed."
    };
    InputPart {
        kind: InputPartKind::Message,
        label: LABEL.to_owned(),
        approx_tokens: (text.len() / 4) as u32,
        text: text.to_owned(),
        source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Spend;
    use crate::events::session::EndReason;

    fn said(text: &str) -> SessionEnd {
        SessionEnd {
            vendor_session: Some("v-1".to_owned()),
            exit_code: Some(0),
            signal: None,
            duration_ms: 1,
            reason: EndReason::Completed,
            spend: Spend::default(),
            tool_calls: 0,
            result_text: Some(text.to_owned()),
            structured: None,
            last_message: None,
            raw_log: None,
        }
    }

    #[test]
    fn a_question_is_known_by_how_the_last_line_ends() {
        for asking in [
            "Should I proceed?",
            "Done with the store.\n\n**Do you want the CLI too?**",
            "Which one (A or B)?\n\n",
            "要继续吗？",
        ] {
            assert!(ends_asking(&said(asking)), "{asking:?}");
        }
        for telling in [
            "Why did it fail? The date had no year.",
            "All 6 tests pass.",
            "",
        ] {
            assert!(!ends_asking(&said(telling)), "{telling:?}");
        }
    }

    /// With a limit of 2: decide, decide, take the recommended option, then nothing.
    #[test]
    fn the_limit_counts_the_times_it_is_told_to_decide() {
        let asks = said("Which do you prefer, A or B?");
        let told = |asked_before| reply(&asks, asked_before, Some(2)).map(|part| part.text);
        for asked_before in [0, 1] {
            let text = told(asked_before).expect("told to decide");
            assert!(text.contains("Decide yourself"), "{text}");
        }
        let last = told(2).expect("told to take its option");
        assert!(last.contains("Take the option you recommended"), "{last}");
        assert_eq!(told(3), None);
        // No limit replies to nothing, and neither does a turn that doesn't ask.
        assert_eq!(reply(&asks, 0, None), None);
        assert_eq!(reply(&said("All 6 tests pass."), 0, Some(2)), None);
    }

    /// A limit of 0 means take the recommended option at once, never "don't reply".
    #[test]
    fn a_limit_of_0_takes_the_recommended_option_at_once() {
        let asks = said("Which do you prefer, A or B?");
        let first = reply(&asks, 0, Some(0)).expect("told at once").text;
        assert!(first.contains("Take the option you recommended"), "{first}");
        assert_eq!(reply(&asks, 1, Some(0)), None);
    }
}
