//! Reading a request's own words: names, numbers, amounts, effort levels, and whether words
//! ask you first or loosen what asks you. Code reads them, never a model.

use argonauts_core::base::names::ModelRef;
use argonauts_core::input::EFFORT_LEVELS;

use super::request_words::FOR_WHAT_ROLES_DO;

/// An effort level from what a request says, such as "max effort" or "extra high".
pub(super) fn effort_level(text: &str) -> Option<String> {
    let text = text.to_lowercase().replace(['-', '_'], " ");
    let words: Vec<&str> = text.split_whitespace().collect();
    // "extra high" and "very high" are one level, so pairs go first.
    let pairs = words.windows(2).map(|pair| pair.concat());
    let singles = words.iter().map(|word| (*word).to_owned());
    pairs.chain(singles).find_map(|word| {
        let level = match word.as_str() {
            "maximum" | "max" | "highest" => "max",
            "extrahigh" | "xhigh" | "veryhigh" => "xhigh",
            "med" | "medium" | "normal" => "medium",
            "min" | "minimum" | "minimal" | "lowest" => "minimal",
            other => other,
        };
        EFFORT_LEVELS.contains(&level).then(|| level.to_owned())
    })
}

/// What the request's own words for an instruction say, with the value the model copied.
pub(super) fn words(value: &str, says: &str) -> String {
    format!("{} {}", value.trim(), says.trim()).to_lowercase()
}

/// A phrase's words, lowercase, without quote marks or punctuation, so "Don't block it!" reads
/// as `dont block it`. Each Chinese, Japanese, or Korean character is a word of its own, since
/// those languages put no spaces between words.
pub(super) fn tokens(text: &str) -> Vec<String> {
    fn flush(word: &mut String, words: &mut Vec<String>) {
        if !word.is_empty() {
            words.push(std::mem::take(word));
        }
    }
    let mut words = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if matches!(c, '\'' | '’' | '‘') {
            continue;
        }
        if ideograph(c) {
            flush(&mut word, &mut words);
            words.push(c.to_string());
        } else if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        } else {
            flush(&mut word, &mut words);
        }
    }
    flush(&mut word, &mut words);
    words
}

fn ideograph(c: char) -> bool {
    matches!(
        u32::from(c),
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
    )
}

/// Where `says` stands in your message word for word, as the place of its first word. It
/// takes two words at least, so a lone "the" or "captain" picked out of your message can't stand
/// for an instruction.
pub(super) fn quoted_at(message: &[String], says: &[String]) -> Option<usize> {
    if says.len() < 2 {
        return None;
    }
    message
        .windows(says.len())
        .position(|window| window == says)
}

/// Words that turn around what follows them in a clause.
const NEGATIONS: [&str; 16] = [
    "not", "dont", "no", "never", "without", "isnt", "doesnt", "wont", "cant", "cannot", "nor",
    "instead", "rather", "shouldnt", "mustnt", "neither",
];

/// The verbs of the roles an instruction gives.
const ROLE_VERBS: [&str; 12] = [
    "orchestrate",
    "orchestrates",
    "build",
    "builds",
    "review",
    "reviews",
    "plan",
    "plans",
    "check",
    "checks",
    "do",
    "does",
];

/// "Don't have codex build" and "codex shouldn't review": a negation before the agent or the
/// role in the same clause, so the words ask for the opposite of the role.
pub(super) fn turned_around(says: &str, names: &[&str]) -> bool {
    let names: Vec<String> = names.iter().flat_map(|name| tokens(name)).collect();
    says.split([',', ';', ':', '.', '!', '?', '(', ')', '\n'])
        .any(|clause| {
            let words = tokens(clause);
            words.iter().enumerate().any(|(at, word)| {
                let role_verb = ROLE_VERBS.contains(&word.as_str())
                    || FOR_WHAT_ROLES_DO.contains(&word.as_str());
                (names.contains(word) || role_verb) && negated(&words, at)
            })
        })
}

/// Whether words say yes or no to something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stance {
    /// Only for it.
    For,
    /// Against it somewhere, such as "not Captain", or for its opposite.
    Against,
    /// Neither.
    Silent,
}

/// What `says` makes of something that `cues` name and `opposite` names the other side of. A
/// cue counts for it, and the same cue after a negation in its clause ("not Captain", "don't do
/// this alone") against it; the opposite's cues count the other way round.
pub(super) fn stance(says: &str, cues: &[&str], opposite: &[&str]) -> Stance {
    let (mut pro, mut con) = (false, false);
    for clause in says.split([',', ';', ':', '.', '!', '?', '(', ')', '\n']) {
        let words = tokens(clause);
        for (cues, toward) in [(cues, true), (opposite, false)] {
            for at in cue_places(&words, cues) {
                if negated(&words, at) != toward {
                    pro = true;
                } else {
                    con = true;
                }
            }
        }
    }
    match (pro, con) {
        (true, false) => Stance::For,
        (_, true) => Stance::Against,
        (false, false) => Stance::Silent,
    }
}

/// Where each cue starts among `words`. A cue of several words matches them in a row.
fn cue_places(words: &[String], cues: &[&str]) -> Vec<usize> {
    let mut places = Vec::new();
    for cue in cues {
        let cue: Vec<&str> = cue.split(' ').collect();
        for at in 0..words.len() {
            let fits = cue.len() <= words.len() - at
                && cue
                    .iter()
                    .zip(&words[at..])
                    .all(|(want, word)| want == word);
            if fits && !places.contains(&at) {
                places.push(at);
            }
        }
    }
    places
}

/// A negation in the three words before `at`.
fn negated(words: &[String], at: usize) -> bool {
    words[at.saturating_sub(3)..at]
        .iter()
        .any(|word| NEGATIONS.contains(&word.as_str()))
}

/// Words that loosen what asks you, or turn it off: with any of them an instruction about
/// approvals or the merge is never followed.
const LOOSER: [&str; 20] = [
    "off",
    "disable",
    "disabled",
    "stop",
    "dont",
    "not",
    "never",
    "without",
    "no",
    "skip",
    "auto",
    "automatic",
    "automatically",
    "yourself",
    "bypass",
    "fewer",
    "less",
    "unless",
    "loosen",
    "relax",
];

/// Words that ask you first.
const ASKS_YOU: [&str; 16] = [
    "ask",
    "asking",
    "check with me",
    "wait for me",
    "wait for my",
    "let me",
    "confirm",
    "confirmation",
    "approve",
    "approval",
    "approvals",
    "my ok",
    "supervised",
    "before",
    "show me",
    "until i",
];

/// Whether words plainly ask you first, with nothing that loosens or turns it off: "ask me
/// before merging" does, "turn approvals off", "stop asking me" and "merge it, no need to check
/// with me" don't. "Don't merge without asking me" and "don't merge until I say so" ask too,
/// as the negation only turns the "without" or the "until" around.
pub(super) fn plainly_asks_you(says: &str) -> bool {
    let mut asks = false;
    for clause in says.split([',', ';', ':', '.', '!', '?', '(', ')', '\n']) {
        let mut words = tokens(clause);
        if words.is_empty() {
            continue;
        }
        if let Some(rest) = double_negative(&words) {
            words = rest;
            asks = true;
        }
        if words.iter().any(|word| LOOSER.contains(&word.as_str())) {
            return false;
        }
        asks |= !cue_places(&words, &ASKS_YOU).is_empty()
            || words.iter().any(|word| word.starts_with("approv"));
    }
    asks
}

/// "Don't merge without asking me" and "never merge until I say so": the words after the
/// negation's "without" or "until", which ask you. `None` when the clause isn't one.
fn double_negative(words: &[String]) -> Option<Vec<String>> {
    let negation = words
        .iter()
        .position(|word| matches!(word.as_str(), "dont" | "never" | "not" | "nothing"))?;
    let turn = words[negation..]
        .iter()
        .position(|word| matches!(word.as_str(), "without" | "until" | "unless"))?
        + negation;
    let after = &words[turn + 1..];
    let asks = after.first().is_some_and(|word| {
        matches!(
            word.as_str(),
            "asking" | "checking" | "i" | "my" | "me" | "approval" | "confirming" | "you"
        )
    });
    asks.then(|| after.to_vec())
}

/// Words that turn the network off, as a phrase.
const NETWORK_OFF: [&str; 22] = [
    "no network",
    "no internet",
    "no net",
    "no web",
    "without network",
    "without the network",
    "without internet",
    "without the internet",
    "network off",
    "internet off",
    "offline",
    "off the network",
    "off the internet",
    "block the network",
    "block network",
    "block the internet",
    "disable the network",
    "disable network",
    "cut the network",
    "dont use the network",
    "dont use the internet",
    "never use the network",
];

/// Whether words plainly turn the network off: "no network" and "keep it offline" do, "keep the
/// network on, don't block it" doesn't.
pub(super) fn turns_network_off(says: &str) -> bool {
    let words = tokens(says);
    let mut covered = vec![false; words.len()];
    for cue in NETWORK_OFF {
        let length = cue.split(' ').count();
        for at in cue_places(&words, &[cue]) {
            covered[at..at + length]
                .iter_mut()
                .for_each(|word| *word = true);
        }
    }
    if !covered.contains(&true) {
        return false;
    }
    let against = ["on", "allow", "enable", "need", "needs", "online"];
    let loose = words.iter().zip(&covered).any(|(word, covered)| {
        !covered && (NEGATIONS.contains(&word.as_str()) || against.contains(&word.as_str()))
    });
    let with_it = [
        "with network",
        "with the network",
        "with internet",
        "with the internet",
        "network access",
        "internet access",
    ];
    let wants_it = cue_places(&words, &with_it)
        .into_iter()
        .any(|at| !covered[at]);
    !loose && !wants_it
}

/// "Gemini and Codex", "claude, codex & agy": each agent named.
pub(super) fn names_in(who: &str) -> Vec<&str> {
    let mut names = vec![who];
    for separator in [",", "&", "+", " and ", " plus ", " with "] {
        names = names
            .into_iter()
            .flat_map(|name| name.split(separator))
            .collect();
    }
    names
        .into_iter()
        .map(|name| name.trim().trim_start_matches("the ").trim())
        .filter(|name| !name.is_empty() && !is_generic(name))
        .collect()
}

/// Words for "the crew" rather than for an agent.
pub(super) fn is_generic(who: &str) -> bool {
    matches!(
        key(who).as_str(),
        "" | "crew" | "thecrew" | "everyone" | "all" | "agents" | "allagents" | "models" | "team"
    )
}

/// "three", "3".
pub(super) fn number_in(text: &str) -> Option<u32> {
    const WORDS: [&str; 10] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .find_map(|word| {
            word.parse::<u32>().ok().or_else(|| {
                WORDS
                    .iter()
                    .position(|known| *known == word)
                    .and_then(|at| u32::try_from(at + 1).ok())
            })
        })
}

/// "$2", "2 dollars", "2.50 usd".
pub(super) fn dollars_in(text: &str) -> Option<f64> {
    let text = text.to_lowercase();
    if let Some(at) = text.find('$') {
        let digits: String = text[at + 1..]
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        return digits.trim_end_matches('.').parse().ok();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(2).find_map(|pair| {
        let unit = pair[1].trim_matches(|c: char| !c.is_alphanumeric());
        matches!(unit, "dollars" | "dollar" | "usd" | "bucks")
            .then(|| pair[0].parse().ok())
            .flatten()
    })
}

pub(super) fn money(amount: f64) -> String {
    if amount.fract() == 0.0 {
        format!("{amount:.0}")
    } else {
        format!("{amount:.2}")
    }
}

/// "A", "A and B", "A, B and C".
pub(super) fn joined(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

pub(super) fn unknown(name: &str) -> String {
    format!("no agent here is called {}, /agents adds one", name.trim())
}

/// Lowercase letters and digits only, so "Claude Code", "claude-code", and "ClaudeCode" match.
pub(super) fn key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `claude` of `claude-code`, `Claude` of `Claude Code`.
pub(super) fn first_word(text: &str) -> &str {
    text.split(['-', ' ', '_']).next().unwrap_or(text)
}

pub(super) fn toml_text(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

pub(super) fn toml_list(workers: &[ModelRef]) -> String {
    let items: Vec<String> = workers
        .iter()
        .map(|worker| toml_text(&worker.to_string()))
        .collect();
    format!("[{}]", items.join(", "))
}
