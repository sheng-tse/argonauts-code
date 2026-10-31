//! What your words ask of the checks. Which checks run is a safety setting, so words never skip,
//! drop or replace them. Words that ask for what the checks already do are met, and with no
//! checks set the start line says so and how to set them.

use argonauts_core::base::text::and_list;
use argonauts_core::settings::Settings;
use argonauts_core::workflows::steps::intake::Instruction;

use super::Effect;
use super::words::{Stance, stance, tokens};

/// Words that ask for less of something, as "skip the tests" does.
pub(super) const LOOSENING: [&str; 8] = [
    "skip", "no", "without", "dont", "disable", "off", "never", "ignore",
];

/// Words that take checks away, as "drop the lint check" does.
const DROPPING: [&str; 3] = ["drop", "remove", "stop"];

/// Words that put other checks in their place, as "run cargo check instead" does.
const REPLACING: [&str; 5] = ["instead", "replace", "only", "rather", "swap"];

/// Words that name checks of any kind, besides the names of the checks set.
const CHECK_WORDS: [&str; 12] = [
    "check",
    "checks",
    "test",
    "tests",
    "testing",
    "lint",
    "linter",
    "linting",
    "clippy",
    "typecheck",
    "ci",
    "verification",
];

/// Words that turn around the check word they reach: a negation, or a verb that takes checks
/// away. Two of them cancel out, as in "don't skip the tests".
const TURNS: [&str; 23] = [
    "no", "not", "dont", "never", "without", "nor", "cant", "cannot", "wont", "shouldnt", "mustnt",
    "doesnt", "isnt", "skip", "skipping", "disable", "ignore", "ignoring", "drop", "remove",
    "stop", "bypass", "off",
];

/// Words about checks failing, which turn the sense around too: "no tests fail" asks for them.
const FAILING: [&str; 12] = [
    "fail", "fails", "failing", "failed", "failure", "failures", "break", "breaks", "breaking",
    "broken", "regress", "red",
];

/// Words that start another part of a clause, so a cue before them is about something else, as
/// "stop" is in "don't stop until the tests pass".
const BREAKS: [&str; 10] = [
    "until", "when", "once", "after", "before", "unless", "if", "while", "then", "so",
];

/// How many words a cue reaches toward a check word, the same reach a negation has in
/// `words::stance`.
const REACH: usize = 3;

/// The start line's words when your words ask about checks and none are set.
pub(super) const NONE_SET: &str =
    "No checks will run, since none are set: /config checks.run sets them";

/// Your words about the checks, against the checks your settings run.
pub(super) fn resolve(instruction: &Instruction, settings: &Settings) -> Result<Effect, String> {
    let names: Vec<String> = settings
        .checks
        .run
        .iter()
        .map(|check| check.name.trim().to_owned())
        .collect();
    if names.is_empty() {
        return Ok(Effect {
            noted: Some(NONE_SET.to_owned()),
            ..Effect::default()
        });
    }
    match refusal(&instruction.says, &names) {
        Some(why) => Err(why),
        None => Ok(Effect {
            said: format!("{} must pass", and_list(&names)),
            ..Effect::default()
        }),
    }
}

/// Why words of yours can't hold against the checks `names`: they would stop some of them, or
/// run others in their place. It names the checks the words name, or all of them when the words
/// name none. `None` when the words ask for the checks, as "make sure no tests fail" does.
pub(crate) fn refusal(says: &str, names: &[String]) -> Option<String> {
    let reading = read(says, names);
    let named = |places: &[usize]| {
        let mut picked: Vec<String> = names
            .iter()
            .enumerate()
            .filter(|(place, _)| places.contains(place))
            .map(|(_, name)| name.clone())
            .collect();
        if picked.is_empty() {
            picked = names.to_vec();
        }
        and_list(&picked)
    };
    let changes = if reading.stops {
        format!("it would stop {} from running", named(&reading.stopped))
    } else if reading.replaces {
        format!(
            "it would run other checks in place of {}",
            named(&reading.replaced)
        )
    } else {
        return None;
    };
    Some(format!(
        "{changes}, and checks.run is a safety setting that /config changes"
    ))
}

/// What words do to the checks, with the places among the checks' names of those they name.
#[derive(Debug, Default)]
struct Reading {
    stops: bool,
    stopped: Vec<usize>,
    replaces: bool,
    replaced: Vec<usize>,
}

/// Reads words about checks clause by clause. A cue counts only when it reaches a check word in
/// its own clause, so "only merge once the tests pass" changes nothing. Words that name no
/// check at all are read by their cues alone.
fn read(says: &str, names: &[String]) -> Reading {
    let names: Vec<Vec<String>> = names.iter().map(|name| tokens(name)).collect();
    let mut reading = Reading::default();
    let mut names_a_check = false;
    for clause in says.split([',', ';', ':', '.', '!', '?', '(', ')', '\n']) {
        let words = tokens(clause);
        for at in 0..words.len() {
            let named: Vec<usize> = names
                .iter()
                .enumerate()
                .filter(|(_, name)| starts_with_name(&words[at..], name))
                .map(|(place, _)| place)
                .collect();
            if named.is_empty() && !CHECK_WORDS.contains(&words[at].as_str()) {
                continue;
            }
            names_a_check = true;
            let before = reach_back(&words, at);
            let after = reach_on(&words, at);
            let turns = before
                .iter()
                .filter(|word| TURNS.contains(word) || FAILING.contains(word))
                .count()
                + after
                    .iter()
                    .filter(|word| **word == "off" || FAILING.contains(word))
                    .count();
            if turns % 2 == 1 {
                reading.stops = true;
                reading.stopped.extend(&named);
            }
            let replacing = before
                .iter()
                .chain(after.iter().take(2))
                .any(|word| REPLACING.contains(word));
            if replacing {
                reading.replaces = true;
                reading.replaced.extend(&named);
            }
        }
    }
    if !names_a_check {
        let words = tokens(says);
        let has = |cues: &[&str]| words.iter().any(|word| cues.contains(&word.as_str()));
        reading.stops = has(&LOOSENING) || has(&DROPPING);
        reading.replaces = has(&REPLACING);
    }
    reading
}

/// Whether `words` start with a check's name, so "the test" names a check called `tests`.
fn starts_with_name(words: &[String], name: &[String]) -> bool {
    !name.is_empty()
        && name.len() <= words.len()
        && name
            .iter()
            .zip(words)
            .all(|(part, word)| part.trim_end_matches('s') == word.trim_end_matches('s'))
}

/// The words before `at` that a cue reaches it from, back to a break in the clause.
fn reach_back(words: &[String], at: usize) -> Vec<&str> {
    words[at.saturating_sub(REACH)..at]
        .iter()
        .rev()
        .map(String::as_str)
        .take_while(|word| !BREAKS.contains(word))
        .collect()
}

/// The words after `at` that a cue reaches it from, up to a break in the clause.
fn reach_on(words: &[String], at: usize) -> Vec<&str> {
    words[at + 1..]
        .iter()
        .take(REACH)
        .map(String::as_str)
        .take_while(|word| !BREAKS.contains(word))
        .collect()
}

/// Whether your message asks to keep checks, as "keep these checks" or "save the tests" does,
/// and not "don't keep the checks".
pub(crate) fn asks_to_keep_checks(message: &str) -> bool {
    let names_checks = tokens(message)
        .iter()
        .any(|word| matches!(word.as_str(), "check" | "checks" | "test" | "tests"));
    names_checks && stance(message, &["keep", "save"], &[]) == Stance::For
}
