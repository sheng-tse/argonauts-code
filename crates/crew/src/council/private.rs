//! What you write to one seat alone: kept in that seat's session, never forwarded, and a
//! council message that repeats it held for you.
//!
//! A seat is one CLI session, so what you tell it in private sits in the same conversation as
//! the talk. The app never passes it on, but the seat might: before any council message goes out,
//! code holds back one that repeats 6 or more of your private words in a row, or a rare word of
//! yours that appears nowhere the seats can read, and you choose what happens.

use std::collections::BTreeSet;

use argonauts_core::base::names::{MessageId, SeatId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, CouncilPacket, CouncilQuestion,
    FreeText,
};
use argonauts_core::runs::events::council::{CouncilEventKind, Held, HeldChoice, HeldWhy};
use argonauts_core::safety::policy::EvidenceFacts;

use super::seats;
use super::talk::{Council, record};
use crate::approvals::{self, ApprovalRequest};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::workflow::StepCall;

/// How many of your private words in a row a council message may repeat before it's held.
pub const WORDS_IN_A_ROW: usize = 6;

/// How a private message reaches its seat. "You" in a seat's session means the seat, so your
/// words are named as the user's.
pub fn turn(text: &str) -> String {
    format!("From the user, privately: {}", text.trim())
}

/// What the other seats read when a private message changed a seat's mind: that it came from
/// you, never what you said.
pub const MOVED_IN_PRIVATE: &str = "the user, in private";

/// A message code holds back, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    /// The private message whose words it repeats.
    pub private: MessageId,
    pub why: HeldWhy,
    /// The runs of the message's words that match, as the message writes them.
    pub matching: Vec<String>,
}

/// Whether a seat's council message repeats what you wrote to it in private. `readable` is
/// everything the seats can read, your request and the council's messages among it: a rare word
/// found there isn't yours alone.
pub fn check(message: &str, private: &[(MessageId, &str)], readable: &str) -> Option<Hold> {
    let said = words(message);
    for (id, text) in private {
        let yours = words(text);
        if let Some((start, count)) = longest_run(&said, &yours)
            && count >= WORDS_IN_A_ROW
        {
            let run = said[start..start + count]
                .iter()
                .map(|word| word.written.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            return Some(Hold {
                private: id.clone(),
                why: HeldWhy::WordsInARow {
                    count: u32::try_from(count).unwrap_or(u32::MAX),
                },
                matching: vec![run],
            });
        }
    }
    let known: BTreeSet<String> = words(readable).into_iter().map(|word| word.key).collect();
    for (id, text) in private {
        for rare in rare_words(text) {
            if known.contains(&rare.key) {
                continue;
            }
            if let Some(found) = said.iter().find(|word| word.key == rare.key) {
                return Some(Hold {
                    private: id.clone(),
                    why: HeldWhy::RareWord {
                        word: rare.written.clone(),
                    },
                    matching: vec![found.written.clone()],
                });
            }
        }
    }
    None
}

/// Whether a seat's council message waits for you: it repeats what you wrote that seat in
/// private and haven't shared.
pub fn held(
    host: &Host,
    council: &Council,
    seat: &SeatId,
    said: &str,
) -> EngineResult<Option<Hold>> {
    let yours: Vec<(MessageId, String)> = council
        .private
        .iter()
        .filter(|(to, id, _)| to == seat && !council.shared.contains(id))
        .filter_map(|(_, id, _)| {
            let words = council.private_words.get(id)?.words(|_| String::new());
            Some((id.clone(), words))
        })
        .collect();
    if yours.is_empty() {
        return Ok(None);
    }
    let readable = readable(host, council)?;
    let yours: Vec<(MessageId, &str)> = yours
        .iter()
        .map(|(id, words)| (id.clone(), words.as_str()))
        .collect();
    Ok(check(said, &yours, &readable))
}

/// Everything the seats can read: your request, the goal, every council message that went out,
/// and the points.
fn readable(host: &Host, council: &Council) -> EngineResult<String> {
    let (request, goal) = host.read(|state| {
        let goal = crate::plan::context::goal_brief(state);
        (
            crate::understand::round::request(state).unwrap_or_default(),
            [goal.summary, goal.criteria.join("\n")].join("\n"),
        )
    });
    let mut text = vec![request, goal];
    text.extend(
        council
            .messages
            .iter()
            .filter(|said| council.out_to_seats(&said.message.id))
            .map(|said| said.message.words(|_| String::new())),
    );
    text.extend(council.points.iter().map(|point| point.words.clone()));
    Ok(text.join("\n"))
}

/// What you can do with a held message, in the order your question lists it, each with the
/// value its option answers with.
const CHOICES: [(&str, HeldChoice); 4] = [
    ("rewrite", HeldChoice::Rewrite),
    ("share", HeldChoice::Share),
    ("as_written", HeldChoice::AsWritten),
    ("drop", HeldChoice::Drop),
];

/// A choice's option as your question words it, for the seat named `name`, with what it does
/// where the label doesn't say.
fn option_words(choice: HeldChoice, name: &str) -> (String, Option<&'static str>) {
    match choice {
        HeldChoice::Rewrite => (
            format!("Ask {name} to say it without your words"),
            Some("It gets one more try, told only that it repeated what you wrote to it"),
        ),
        HeldChoice::Share => (
            "Share your private message with the council".to_owned(),
            Some("Every seat reads it, and it binds them like anything you write to the council"),
        ),
        HeldChoice::AsWritten => (
            "Send it as written: the seats read these words, not your message".to_owned(),
            None,
        ),
        HeldChoice::Drop => ("Drop it: no seat ever reads it".to_owned(), None),
    }
}

/// Asks you what happens to a held message, and records your choice. Sharing sends your
/// private message to every seat first, so the held one reads in its light.
pub async fn decide(
    host: &Host,
    call: &StepCall,
    council: &Council,
    held: &Held,
) -> EngineResult<()> {
    let name = council.seat(&held.seat).map_or_else(
        || "A seat".to_owned(),
        |seat| seats::name(seat, &DisplayNames::default()),
    );
    let words = council
        .message(&held.message)
        .map(|message| message.words(|_| String::new()))
        .unwrap_or_default();
    let options = CHOICES
        .iter()
        .enumerate()
        .map(|(index, (value, choice))| {
            let (label, detail) = option_words(*choice, &name);
            AnswerOption {
                key: char::from(b'a' + u8::try_from(index).unwrap_or(0)),
                label,
                action: AnswerAction::Choose {
                    value: (*value).to_owned(),
                },
                recommended: index == 0,
                detail: detail.map(str::to_owned),
            }
        })
        .collect();
    let request = ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Question,
        title: format!("{name}'s message repeats your private words"),
        task: None,
        packet: ApprovalPacket::Council(CouncilPacket {
            talk: council.talk.clone(),
            about: CouncilQuestion::Held {
                message: held.message.clone(),
                why: Some(held.why.clone()),
                wrote_at: council.private_at.get(&held.private).copied(),
            },
            text: held.why.question_text(None),
        }),
        facts: EvidenceFacts::default(),
        // The message itself, so two held messages are two questions.
        agent_outputs: vec![words],
        documents: Vec::new(),
        options: Some(options),
        free_text: FreeText::None,
        authors: Vec::new(),
    };
    let decided = approvals::open_and_wait(host, request).await?;
    let choice = match &decided.action {
        AnswerAction::Choose { value } => CHOICES
            .iter()
            .find(|(choice, _)| choice == value)
            .map_or(HeldChoice::Rewrite, |(_, choice)| *choice),
        _ => HeldChoice::Rewrite,
    };
    let talk = &council.talk;
    if choice == HeldChoice::Share {
        record(
            host,
            talk,
            CouncilEventKind::Shared {
                message: held.private.clone(),
            },
        )?;
    }
    record(
        host,
        talk,
        CouncilEventKind::Released {
            message: held.message.clone(),
            choice,
        },
    )?;
    // The rest of the turn counts only where the seats read its message.
    if matches!(choice, HeldChoice::AsWritten | HeldChoice::Share) {
        for event in &held.turn {
            record(host, talk, event.clone())?;
        }
    }
    Ok(())
}

/// What you can do with a held plan, in the order your question lists it. A council needs its
/// plan, so it can't be dropped.
const PLAN_CHOICES: [(&str, HeldChoice); 3] = [
    ("rewrite", HeldChoice::Rewrite),
    ("share", HeldChoice::Share),
    ("as_written", HeldChoice::AsWritten),
];

/// Asks you what happens to the note keeper's plan that repeats your private words, before it
/// is sealed, and gives your choice. Sharing sends your private message to every seat first.
pub async fn decide_plan(
    host: &Host,
    call: &StepCall,
    council: &Council,
    keeper: &SeatId,
    hold: &Hold,
    plan: &str,
) -> EngineResult<HeldChoice> {
    let name = council.seat(keeper).map_or_else(
        || "The note keeper".to_owned(),
        |seat| seats::name(seat, &DisplayNames::default()),
    );
    let options = PLAN_CHOICES
        .iter()
        .enumerate()
        .map(|(index, (value, choice))| {
            let (label, detail) = match choice {
                HeldChoice::Rewrite => (
                    format!("Ask {name} to write the plan again without your words"),
                    Some("It gets one more try, told only that it repeated what you wrote to it"),
                ),
                HeldChoice::AsWritten => (
                    "Seal it as written: every task reads these words, not your message".to_owned(),
                    None,
                ),
                other => option_words(*other, &name),
            };
            AnswerOption {
                key: char::from(b'a' + u8::try_from(index).unwrap_or(0)),
                label,
                action: AnswerAction::Choose {
                    value: (*value).to_owned(),
                },
                recommended: index == 0,
                detail: detail.map(str::to_owned),
            }
        })
        .collect();
    let request = ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Question,
        title: format!("{name}'s plan repeats your private words"),
        task: None,
        packet: ApprovalPacket::Council(CouncilPacket {
            talk: council.talk.clone(),
            about: CouncilQuestion::HeldPlan {
                keeper: keeper.clone(),
                why: Some(hold.why.clone()),
                wrote_at: council.private_at.get(&hold.private).copied(),
            },
            text: hold.why.plan_question_text(None),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: vec![plan.to_owned()],
        documents: Vec::new(),
        options: Some(options),
        free_text: FreeText::None,
        authors: Vec::new(),
    };
    let decided = approvals::open_and_wait(host, request).await?;
    let choice = match &decided.action {
        AnswerAction::Choose { value } => PLAN_CHOICES
            .iter()
            .find(|(choice, _)| choice == value)
            .map_or(HeldChoice::Rewrite, |(_, choice)| *choice),
        _ => HeldChoice::Rewrite,
    };
    if choice == HeldChoice::Share {
        record(
            host,
            &council.talk,
            CouncilEventKind::Shared {
                message: hold.private.clone(),
            },
        )?;
    }
    Ok(choice)
}

/// A word as written, and the key it matches by: lowercase, without the punctuation around it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    written: String,
    key: String,
    /// The first word of a sentence, whose capital says nothing.
    opens: bool,
}

fn words(text: &str) -> Vec<Word> {
    let mut found = Vec::new();
    let mut opens = true;
    for raw in text.split_whitespace() {
        let trimmed = raw.trim_matches(|c: char| !c.is_alphanumeric() && !"/_~".contains(c));
        if !trimmed.is_empty() {
            found.push(Word {
                written: trimmed.to_owned(),
                key: trimmed.to_lowercase(),
                opens,
            });
        }
        opens = raw.ends_with(['.', '!', '?', ':']);
    }
    found
}

/// The longest run of words both texts share, as its start in `said` and its length.
fn longest_run(said: &[Word], yours: &[Word]) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    let mut previous = vec![0usize; yours.len() + 1];
    for (i, word) in said.iter().enumerate() {
        let mut current = vec![0usize; yours.len() + 1];
        for (j, mine) in yours.iter().enumerate() {
            if word.key == mine.key {
                current[j + 1] = previous[j] + 1;
                let count = current[j + 1];
                if best.is_none_or(|(_, longest)| count > longest) {
                    best = Some((i + 1 - count, count));
                }
            }
        }
        previous = current;
    }
    best
}

/// Words of yours a reader could trace back to you: a name, a path or a long number.
fn rare_words(text: &str) -> Vec<Word> {
    words(text).into_iter().filter(is_rare).collect()
}

fn is_rare(word: &Word) -> bool {
    let text = word.written.as_str();
    let path = text.contains('/') || text.contains('\\') || has_extension(text);
    let digits = text.chars().filter(char::is_ascii_digit).count();
    let number = digits >= 5
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || ",._-".contains(c));
    let mut chars = text.chars();
    let capital = chars.next().is_some_and(char::is_uppercase);
    let name = !word.opens && capital && text.chars().count() >= 3;
    // An inner capital or an underscore marks an identifier, wherever it sits.
    let identifier = chars.any(char::is_uppercase) || text.contains('_');
    path || number || name || identifier
}

/// `limits.rs`, `acme.toml`: a dot with letters after it, short like a file's extension.
pub(super) fn has_extension(text: &str) -> bool {
    let Some((stem, extension)) = text.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (1..=5).contains(&extension.len())
        && extension.chars().all(|c| c.is_ascii_alphanumeric())
        && extension.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(text: &str) -> Vec<(MessageId, &str)> {
        vec![(MessageId::numbered(9), text)]
    }

    #[test]
    fn your_private_words_reach_the_seat_as_the_users() {
        assert_eq!(
            turn("  keep the old window for now "),
            "From the user, privately: keep the old window for now"
        );
    }

    #[test]
    fn six_of_your_words_in_a_row_hold_a_message_and_five_dont() {
        let yours = private("Between us, the billing team wants the window kept until March.");
        let six = "I think the billing team wants the window, so let's keep it.";
        let held = check(six, &yours, "").unwrap();
        assert_eq!(held.private, MessageId::numbered(9));
        assert_eq!(held.why, HeldWhy::WordsInARow { count: 6 });
        assert_eq!(held.matching, ["the billing team wants the window"]);

        let five = "The billing team wants the bucket, I hear.";
        assert_eq!(check(five, &yours, ""), None);
    }

    #[test]
    fn a_rare_word_of_yours_is_held_unless_the_seats_can_read_it_elsewhere() {
        let yours = private("Ask Priya before touching src/limits/old_window.rs or ticket 48213.");
        for (message, rare) in [
            ("Priya should sign off first.", "Priya"),
            (
                "Leave src/limits/old_window.rs alone.",
                "src/limits/old_window.rs",
            ),
            ("This is about ticket 48213.", "48213"),
        ] {
            let held = check(message, &yours, "Add a rate limiter to the API.")
                .unwrap_or_else(|| panic!("{message}"));
            assert_eq!(
                held.why,
                HeldWhy::RareWord {
                    word: rare.to_owned()
                },
                "{message}"
            );
        }
        let readable = "Add a rate limiter. The old code is in src/limits/old_window.rs.";
        assert_eq!(
            check("Leave src/limits/old_window.rs alone.", &yours, readable),
            None
        );
        // "Ask" opens a sentence, so its capital marks no name.
        assert_eq!(check("Ask before you merge.", &yours, ""), None);
    }

    #[test]
    fn the_check_matches_whole_words_without_case_or_punctuation() {
        let yours = private("We must not break the mobile clients this quarter.");
        let said = "\"WE MUST NOT break the mobile\" clients, says the plan.";
        assert_eq!(
            check(said, &yours, "").map(|held| held.why),
            Some(HeldWhy::WordsInARow { count: 7 })
        );
        assert_eq!(check("Nothing alike here at all.", &yours, ""), None);
        assert_eq!(check("anything", &[], ""), None);
    }
}
