//! The materiality rules, in code: wording differences are dropped; a difference at least two
//! thirds agree on, that no comparer tags in `understand.always_ask`, becomes a stated assumption;
//! everything else becomes a question, at most `understand.max_questions`, most important first
//! (scope, then security, data, cost, then detail).

use std::collections::BTreeSet;

use argonauts_core::events::goal::{Assumption, ChoiceOption, Decider, GoalQuestion};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::Answerer;
use argonauts_core::settings::protocol::UnderstandSettings;
use argonauts_core::steps::understand::{
    CompareOutput, DiffTag, Difference, McQuestion, TopicReading,
};

use super::read::label;
use crate::approvals;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settled {
    pub agreements: Vec<String>,
    pub assumptions: Vec<Assumption>,
    pub questions: Vec<GoalQuestion>,
    /// Wording differences dropped.
    pub dropped: u32,
    /// Differences in `always_ask` that didn't fit under the question limit. They're adopted
    /// with their recommended answer as assumptions, and the goal still waits for you, unless
    /// the main agent answers the crew's questions.
    pub held_back: u32,
    /// The tag and topic of each difference over the question limit, which
    /// `approvals.ask_you_about` may keep yours when the main agent answers.
    pub over_limit: Vec<(DiffTag, String)>,
}

/// One difference as every comparer listed it, merged by topic.
struct Merged {
    topic: String,
    /// The most important tag any comparer gave it.
    tag: DiffTag,
    /// Some comparer tagged it in `always_ask`.
    must_ask: bool,
    readings: Vec<TopicReading>,
    question: Option<McQuestion>,
}

/// The Understand settings differences settle by. The question limit spares your attention, so when
/// the main agent answers every question, in `autopilot` or because you chose it to, nothing is
/// held back over the limit.
pub fn understand_settings(settings: &Settings, autopilot: bool) -> UnderstandSettings {
    let mut understand = settings.understand.clone();
    if main_agent_answers_all(settings, autopilot) {
        understand.max_questions = u32::MAX;
    }
    understand
}

/// Whether the main agent, answering the crew's questions, may answer this one for you: any of
/// them, unless `approvals.ask_you_about` names its tag or topic, and every one in `autopilot`,
/// where nothing waits for you. `understand.always_ask` only makes a topic a question, never one
/// that waits for you.
pub fn main_agent_may_answer(
    question: &GoalQuestion,
    settings: &Settings,
    autopilot: bool,
) -> bool {
    autopilot || asked_about(question, settings).is_none()
}

/// The word of `approvals.ask_you_about` that keeps this question yours: its tag, such as
/// `security`, or its topic.
pub fn asked_about<'q>(question: &'q GoalQuestion, settings: &Settings) -> Option<&'q str> {
    std::iter::once(tag_name(question.tag))
        .chain(question.topic.as_deref())
        .find(|word| approvals::asks_you_about(settings, &[word]))
}

/// The main agent answers every question in autopilot, where the model you picked may answer in its
/// place, or once you chose the main agent to. A model you picked answers in autopilot only.
fn main_agent_answers_all(settings: &Settings, autopilot: bool) -> bool {
    autopilot || settings.approvals.answers == Some(Answerer::Main)
}

/// Settles every comparer's listing of `readers` readings.
pub fn settle(
    listings: &[CompareOutput],
    readers: usize,
    settings: &UnderstandSettings,
) -> Settled {
    let mut settled = Settled {
        agreements: agreements(listings),
        ..Settled::default()
    };
    let labels: Vec<String> = (0..readers).map(label).collect();

    let mut asked = Vec::new();
    for merged in merge(listings, settings) {
        if merged.tag == DiffTag::Wording {
            settled.dropped += 1;
            continue;
        }
        let groups = reading_groups(&merged.readings, &labels);
        let majority = groups
            .first()
            .filter(|group| readers > 0 && group.support * 3 >= readers * 2);
        match majority {
            Some(group) if settings.majority_assumptions && !merged.must_ask => {
                settled.assumptions.push(Assumption {
                    text: format!("{}: {}", merged.topic, group.text),
                    basis: format!(
                        "{}, and {} of {readers} read it that way",
                        tag_phrase(merged.tag),
                        group.support
                    ),
                    by: Some(Decider::Majority),
                });
            }
            _ => asked.push(merged),
        }
    }

    // A stable sort keeps the comparers' own order within a tag.
    asked.sort_by_key(|merged| importance(merged.tag));
    let limit = usize::try_from(settings.max_questions).unwrap_or(usize::MAX);
    let of = u8::try_from(asked.len().min(limit)).unwrap_or(u8::MAX);
    for (index, merged) in asked.into_iter().enumerate() {
        let groups = reading_groups(&merged.readings, &labels);
        let question = question(&merged, &groups, readers);
        if index < limit {
            let n = u8::try_from(index + 1).unwrap_or(u8::MAX);
            settled
                .questions
                .push(goal_question(n, of, &merged, question));
        } else {
            if merged.must_ask {
                settled.held_back += 1;
            }
            settled.over_limit.push((merged.tag, merged.topic.clone()));
            let answer = question
                .options
                .get(usize::from(question.recommended))
                .cloned()
                .unwrap_or_default();
            settled.assumptions.push(Assumption {
                text: if answer.is_empty() {
                    merged.topic.clone()
                } else {
                    format!("{}: {answer}", merged.topic)
                },
                basis: format!(
                    "the recommended answer, since it was over the {limit}-question limit"
                ),
                by: Some(Decider::Rules),
            });
        }
    }
    settled
}

/// The tag's name as `understand.always_ask` spells it.
pub fn tag_name(tag: DiffTag) -> &'static str {
    match tag {
        DiffTag::Wording => "wording",
        DiffTag::Detail => "detail",
        DiffTag::Scope => "scope",
        DiffTag::Security => "security",
        DiffTag::Data => "data",
        DiffTag::Cost => "cost",
    }
}

/// Whether a question with this tag must reach you in every mode.
pub fn always_asks(tag: DiffTag, settings: &UnderstandSettings) -> bool {
    settings
        .always_ask
        .iter()
        .any(|name| name.trim().eq_ignore_ascii_case(tag_name(tag)))
}

fn importance(tag: DiffTag) -> u8 {
    match tag {
        DiffTag::Scope => 0,
        DiffTag::Security => 1,
        DiffTag::Data => 2,
        DiffTag::Cost => 3,
        DiffTag::Detail => 4,
        DiffTag::Wording => 5,
    }
}

fn tag_phrase(tag: DiffTag) -> String {
    match tag {
        DiffTag::Detail => "a detail".to_owned(),
        other => format!("a {} difference", tag_name(other)),
    }
}

fn agreements(listings: &[CompareOutput]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    listings
        .iter()
        .flat_map(|listing| &listing.agreements)
        .map(|agreement| one_line(agreement))
        .filter(|agreement| !agreement.is_empty() && seen.insert(normalize(agreement)))
        .collect()
}

/// Comparers name the same difference in their own words; the same topic merges, keeping the
/// most important tag, since one comparer's scope tag is enough to ask.
fn merge(listings: &[CompareOutput], settings: &UnderstandSettings) -> Vec<Merged> {
    let mut merged: Vec<(String, Merged)> = Vec::new();
    for difference in listings.iter().flat_map(|listing| &listing.differences) {
        let key = normalize(&difference.topic);
        let must_ask = always_asks(difference.tag, settings);
        match merged.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, into)) => absorb(into, difference, must_ask),
            None => merged.push((
                key,
                Merged {
                    topic: one_line(&difference.topic),
                    tag: difference.tag,
                    must_ask,
                    readings: difference.readings.clone(),
                    question: usable(difference.question.as_ref()),
                },
            )),
        }
    }
    merged.into_iter().map(|(_, merged)| merged).collect()
}

fn absorb(into: &mut Merged, difference: &Difference, must_ask: bool) {
    if importance(difference.tag) < importance(into.tag) {
        into.tag = difference.tag;
    }
    into.must_ask |= must_ask;
    into.readings.extend(difference.readings.iter().cloned());
    if into.question.is_none() {
        into.question = usable(difference.question.as_ref());
    }
}

fn usable(question: Option<&McQuestion>) -> Option<McQuestion> {
    let question = question?;
    let options: Vec<String> = question
        .options
        .iter()
        .map(|option| one_line(option))
        .filter(|option| !option.is_empty())
        .take(4)
        .collect();
    if options.is_empty() || one_line(&question.text).is_empty() {
        return None;
    }
    let recommended = if usize::from(question.recommended) < options.len() {
        question.recommended
    } else {
        0
    };
    Some(McQuestion {
        text: one_line(&question.text),
        options,
        recommended,
        basis: one_line(&question.basis),
    })
}

/// Readings that say the same thing, with every reading label that reads it that way.
#[derive(Debug)]
struct Group {
    text: String,
    support: usize,
}

/// Most supported first; ties keep the comparer's order.
fn reading_groups(readings: &[TopicReading], labels: &[String]) -> Vec<Group> {
    let mut groups: Vec<(String, String, BTreeSet<String>)> = Vec::new();
    for reading in readings {
        let text = one_line(&reading.reading);
        if text.is_empty() {
            continue;
        }
        let key = normalize(&text);
        let supporters = supporters(&reading.label, labels);
        match groups.iter_mut().find(|(seen, _, _)| *seen == key) {
            Some((_, _, into)) => into.extend(supporters),
            None => groups.push((key, text, supporters)),
        }
    }
    let mut groups: Vec<Group> = groups
        .into_iter()
        .map(|(_, text, supporters)| Group {
            text,
            support: supporters.len(),
        })
        .collect();
    groups.sort_by_key(|group| std::cmp::Reverse(group.support));
    groups
}

/// The reading labels a reading names, such as "A and C" or "A, C".
fn supporters(field: &str, labels: &[String]) -> BTreeSet<String> {
    field
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .filter_map(|token| {
            labels
                .iter()
                .find(|label| label.eq_ignore_ascii_case(token))
                .cloned()
        })
        .collect()
}

/// The comparer's question, or one built from the readings when it gave none.
fn question(merged: &Merged, groups: &[Group], readers: usize) -> McQuestion {
    if let Some(question) = &merged.question {
        return question.clone();
    }
    let options: Vec<String> = groups
        .iter()
        .take(4)
        .map(|group| group.text.clone())
        .collect();
    let basis = groups
        .first()
        .filter(|group| group.support > 0)
        .map(|group| format!("{} of {readers} readings read it this way", group.support))
        .unwrap_or_default();
    McQuestion {
        text: format!("Which reading of \"{}\" is right?", merged.topic),
        options,
        recommended: 0,
        basis,
    }
}

fn goal_question(n: u8, of: u8, merged: &Merged, question: McQuestion) -> GoalQuestion {
    let tag = merged.tag;
    let options = question
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let recommended = index == usize::from(question.recommended);
            ChoiceOption {
                key: option_key(index),
                label: option.clone(),
                recommended,
                basis: (recommended && !question.basis.is_empty()).then(|| question.basis.clone()),
            }
        })
        .collect();
    GoalQuestion {
        n,
        of,
        text: question.text,
        options,
        tag,
        topic: Some(merged.topic.clone()),
    }
}

fn option_key(index: usize) -> char {
    u8::try_from(index)
        .ok()
        .and_then(|index| b'a'.checked_add(index))
        .map_or('?', char::from)
}

/// Agent text on one line, control characters removed.
pub(crate) fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize(text: &str) -> String {
    one_line(text)
        .trim_end_matches(['.', '!', '?', ';', ':'])
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(label: &str, reading: &str) -> TopicReading {
        TopicReading {
            label: label.to_owned(),
            reading: reading.to_owned(),
        }
    }

    fn difference(topic: &str, tag: DiffTag, readings: Vec<TopicReading>) -> Difference {
        Difference {
            topic: topic.to_owned(),
            readings,
            tag,
            question: None,
        }
    }

    fn two_to_one(topic: &str, tag: DiffTag) -> Difference {
        difference(
            topic,
            tag,
            vec![
                reading("A", "per user"),
                reading("B", "per user"),
                reading("C", "per key"),
            ],
        )
    }

    fn listing(differences: Vec<Difference>) -> CompareOutput {
        CompareOutput {
            agreements: vec!["Add a rate limit.".to_owned()],
            differences,
        }
    }

    #[test]
    fn wording_differences_are_dropped() {
        let settled = settle(
            &[listing(vec![two_to_one("phrasing", DiffTag::Wording)])],
            3,
            &UnderstandSettings::default(),
        );
        assert_eq!(settled.dropped, 1);
        assert!(settled.assumptions.is_empty() && settled.questions.is_empty());
        assert_eq!(settled.agreements, vec!["Add a rate limit.".to_owned()]);
    }

    #[test]
    fn a_two_thirds_detail_becomes_an_assumption() {
        let settled = settle(
            &[listing(vec![two_to_one(
                "limit per user or per key",
                DiffTag::Detail,
            )])],
            3,
            &UnderstandSettings::default(),
        );
        assert!(settled.questions.is_empty());
        assert_eq!(
            settled.assumptions,
            vec![Assumption {
                text: "limit per user or per key: per user".to_owned(),
                basis: "a detail, and 2 of 3 read it that way".to_owned(),
                by: Some(Decider::Majority),
            }]
        );
    }

    #[test]
    fn always_ask_tags_become_questions_even_with_a_majority() {
        for tag in [
            DiffTag::Scope,
            DiffTag::Security,
            DiffTag::Data,
            DiffTag::Cost,
        ] {
            let settled = settle(
                &[listing(vec![two_to_one("topic", tag)])],
                3,
                &UnderstandSettings::default(),
            );
            assert!(settled.assumptions.is_empty(), "{tag:?}");
            assert_eq!(settled.questions.len(), 1, "{tag:?}");
            assert_eq!(settled.questions[0].tag, tag);
        }
    }

    #[test]
    fn a_split_detail_becomes_a_question() {
        let split = difference(
            "storage",
            DiffTag::Detail,
            vec![
                reading("A", "redis"),
                reading("B", "memory"),
                reading("C", "sqlite"),
            ],
        );
        let settled = settle(&[listing(vec![split])], 3, &UnderstandSettings::default());
        assert_eq!(settled.questions.len(), 1);
        let question = &settled.questions[0];
        assert_eq!((question.n, question.of), (1, 1));
        assert_eq!(question.text, "Which reading of \"storage\" is right?");
        let labels: Vec<&str> = question.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, ["redis", "memory", "sqlite"]);
        assert_eq!(question.options[0].key, 'a');
        assert!(question.options[0].recommended);
        assert_eq!(
            question.options[0].basis.as_deref(),
            Some("1 of 3 readings read it this way")
        );
    }

    #[test]
    fn majority_assumptions_off_asks_instead() {
        let settings = UnderstandSettings {
            majority_assumptions: false,
            ..UnderstandSettings::default()
        };
        let settled = settle(
            &[listing(vec![two_to_one("t", DiffTag::Detail)])],
            3,
            &settings,
        );
        assert!(settled.assumptions.is_empty());
        assert_eq!(settled.questions.len(), 1);
    }

    #[test]
    fn the_comparers_question_is_used_when_given() {
        let mut asked = two_to_one("limit per user or per key", DiffTag::Scope);
        asked.question = Some(McQuestion {
            text: "Limit per user or per API key?".to_owned(),
            options: vec!["Per user".to_owned(), "Per API key".to_owned()],
            recommended: 1,
            basis: "2 of 3 readings read it this way".to_owned(),
        });
        let settled = settle(&[listing(vec![asked])], 3, &UnderstandSettings::default());
        let question = &settled.questions[0];
        assert_eq!(question.text, "Limit per user or per API key?");
        assert!(!question.options[0].recommended && question.options[1].recommended);
        assert_eq!(question.options[1].key, 'b');
        assert_eq!(question.options[0].basis, None);
    }

    #[test]
    fn questions_are_ordered_by_importance_and_capped() {
        let differences = vec![
            difference(
                "d1",
                DiffTag::Detail,
                vec![reading("A", "x"), reading("B", "y")],
            ),
            two_to_one("cost", DiffTag::Cost),
            two_to_one("data", DiffTag::Data),
            two_to_one("security", DiffTag::Security),
            two_to_one("scope", DiffTag::Scope),
        ];
        let settled = settle(&[listing(differences)], 3, &UnderstandSettings::default());
        let topics: Vec<DiffTag> = settled.questions.iter().map(|q| q.tag).collect();
        assert_eq!(topics, [DiffTag::Scope, DiffTag::Security, DiffTag::Data]);
        assert!(settled.questions.iter().all(|q| q.of == 3));
        assert_eq!(
            settled.questions.iter().map(|q| q.n).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // The cost question and the detail question didn't fit. Cost always reaches you.
        assert_eq!(settled.held_back, 1);
        assert_eq!(settled.assumptions.len(), 2);
        assert_eq!(settled.assumptions[0].text, "cost: per user");
        assert!(settled.assumptions[0].basis.contains("3-question limit"));
    }

    #[test]
    fn comparers_listings_merge_by_topic_keeping_the_most_important_tag() {
        let first = listing(vec![two_to_one(
            "Limit per user or per key",
            DiffTag::Wording,
        )]);
        let second = CompareOutput {
            agreements: vec!["add a rate limit".to_owned(), "Return 429.".to_owned()],
            differences: vec![two_to_one("limit per user or per key.", DiffTag::Scope)],
        };
        let settled = settle(&[first, second], 3, &UnderstandSettings::default());
        assert_eq!(settled.dropped, 0);
        assert_eq!(settled.questions.len(), 1);
        assert_eq!(settled.questions[0].tag, DiffTag::Scope);
        assert_eq!(settled.agreements, ["Add a rate limit.", "Return 429."]);
    }

    #[test]
    fn readings_can_name_several_labels() {
        let joint = difference(
            "limit",
            DiffTag::Detail,
            vec![reading("A and C", "per user"), reading("B", "per key")],
        );
        let settled = settle(&[listing(vec![joint])], 3, &UnderstandSettings::default());
        assert_eq!(settled.assumptions.len(), 1);
        assert!(
            settled.assumptions[0]
                .basis
                .ends_with("2 of 3 read it that way")
        );
    }

    #[test]
    fn unknown_labels_never_make_a_majority() {
        let vague = difference(
            "limit",
            DiffTag::Detail,
            vec![
                reading("the first two", "per user"),
                reading("the third", "per key"),
            ],
        );
        let settled = settle(&[listing(vec![vague])], 3, &UnderstandSettings::default());
        assert!(settled.assumptions.is_empty());
        assert_eq!(settled.questions.len(), 1);
    }

    #[test]
    fn no_listing_settles_nothing() {
        let settled = settle(&[], 3, &UnderstandSettings::default());
        assert_eq!(settled, Settled::default());
    }
}
