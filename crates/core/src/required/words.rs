//! What a request's own words require: a task list it fixed ("do exactly these three tasks:
//! ..."), and steps every workflow must contain ("every workflow must end with a benchmark
//! run"). Only plain cues count, so a request about code that says a file "must end with a
//! newline" requires nothing.

use std::sync::LazyLock;

use regex::Regex;

use super::RequiredStep;
use crate::prompt_parts::CrewRole;

#[derive(Debug, Default)]
pub(super) struct Said {
    pub steps: Vec<RequiredStep>,
    pub tasks: Vec<String>,
    pub problems: Vec<String>,
}

pub(super) fn said(request: &str) -> Said {
    let mut said = Said::default();
    match task_list(request) {
        Some(Ok(tasks)) => said.tasks = tasks,
        Some(Err(problem)) => said.problems.push(problem),
        None => {}
    }
    said.steps = steps(request);
    said
}

const NUMBERS: [&str; 12] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
    "twelve",
];

/// "exactly these three tasks", "only these tasks", "just the following 2 tasks".
static TASK_CUE: LazyLock<Regex> = LazyLock::new(|| {
    let numbers = NUMBERS.join("|");
    Regex::new(&format!(
        r"(?i)\b(?:exactly|only|just)\s+(?P<these>(?:these|this|the\s+following)\s+)?(?:(?P<count>{numbers}|\d+)\s+)?tasks?\b"
    ))
    .unwrap_or_else(|err| panic!("task cue: {err}"))
});

/// A bullet or a number in front of a listed task: `- `, `* `, `1. `, `2) `, `(3) `.
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[-*•]|\(?\d+[.)])\s+").unwrap_or_else(|err| panic!("marker: {err}"))
});

/// The tasks a request fixed, `None` when it fixed none, or why a list it gave isn't used.
fn task_list(request: &str) -> Option<Result<Vec<String>, String>> {
    TASK_CUE.captures_iter(request).find_map(|cue| {
        let count = cue.name("count").map(|count| number(count.as_str()));
        if cue.name("these").is_none() && count.is_none() {
            return None;
        }
        let whole = cue.get(0)?;
        let rest = &request[whole.end()..];
        let colon = rest.find(':')?;
        if sentence_end(rest).is_some_and(|end| end < colon) {
            return None;
        }
        let mut tasks = listed(&rest[colon + 1..]);
        if let Some(Some(count)) = count {
            split_last_and(&mut tasks, count);
            if tasks.len() != count {
                return Some(Err(format!(
                    "the request said \"{}\" and listed {}, so its task list wasn't fixed",
                    whole.as_str(),
                    tasks.len()
                )));
            }
        }
        (!tasks.is_empty()).then_some(Ok(tasks))
    })
}

/// The items after the colon: one a line when the list starts on the next line, else up to
/// the sentence's end, split at `;` or else at `,`.
fn listed(text: &str) -> Vec<String> {
    let (first, after) = text.split_once('\n').unwrap_or((text, ""));
    if first.trim().is_empty() {
        let mut tasks = Vec::new();
        let mut marked = false;
        for line in after.lines() {
            if line.trim().is_empty() {
                if tasks.is_empty() {
                    continue;
                }
                break;
            }
            let has_marker = MARKER.is_match(line);
            if marked && !has_marker {
                break;
            }
            marked |= has_marker;
            tasks.push(item(line));
        }
        return tasks.into_iter().filter(|task| !task.is_empty()).collect();
    }
    let sentence = &text[..sentence_end(text).unwrap_or(text.len())];
    let separator = if sentence.contains(';') { ';' } else { ',' };
    sentence
        .split(separator)
        .map(item)
        .filter(|task| !task.is_empty())
        .collect()
}

/// One listed task without its marker, its joining word or its full stop.
fn item(text: &str) -> String {
    let mut text = MARKER.replace(text, "").trim().to_owned();
    for joining in ["and then ", "and ", "then "] {
        if let Some(rest) = strip_word(&text, joining) {
            text = rest.to_owned();
            break;
        }
    }
    text.trim_end_matches(['.', '!', '?'])
        .trim_matches(['"', '\'', '`', '“', '”'])
        .trim()
        .to_owned()
}

/// "a, b and c" lists three when the request said three.
fn split_last_and(tasks: &mut Vec<String>, count: usize) {
    if tasks.len() + 1 != count {
        return;
    }
    let Some(last) = tasks.pop() else { return };
    match last.rsplit_once(" and ") {
        Some((head, tail)) if !head.trim().is_empty() && !tail.trim().is_empty() => {
            tasks.push(head.trim().to_owned());
            tasks.push(tail.trim().to_owned());
        }
        _ => tasks.push(last),
    }
}

fn number(word: &str) -> Option<usize> {
    word.parse().ok().or_else(|| {
        NUMBERS
            .iter()
            .position(|known| known.eq_ignore_ascii_case(word))
            .map(|at| at + 1)
    })
}

/// Where the first sentence of `text` ends: a full stop, `!` or `?` before a space or the end,
/// or a line break. `v1.2` doesn't end one.
fn sentence_end(text: &str) -> Option<usize> {
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next_is_space = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        if c == '\n' || (matches!(c, '.' | '!' | '?') && next_is_space) {
            return Some(at);
        }
    }
    None
}

/// "every workflow must end with X", "each run has to include X after build".
static MUST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:(?:every|each|any|the|this|your|our)\s+workflows?|(?:every|each|any)\s+runs?)\s+(?:must|should|shall|has\s+to|have\s+to|needs?\s+to)\s+(?:always\s+)?(?P<verb>end|finish|start|begin|include|contain|have|run)\b(?:\s+with\b)?\s*",
    )
    .unwrap_or_else(|err| panic!("must cue: {err}"))
});

/// "always end the workflow with X".
static ALWAYS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\balways\s+(?P<verb>end|finish|start|begin)\s+(?:the\s+|every\s+|each\s+|a\s+)?workflows?\s+with\b\s*",
    )
    .unwrap_or_else(|err| panic!("always cue: {err}"))
});

/// "always include a secrets scan step after build".
static STEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:always|must)\s+(?:include|add|have|run)\s+(?:a|an|the)\s+(?P<what>[^.;,!?\n]+?)\s+step\b\s*",
    )
    .unwrap_or_else(|err| panic!("step cue: {err}"))
});

fn steps(request: &str) -> Vec<RequiredStep> {
    let mut steps: Vec<RequiredStep> = Vec::new();
    for cue in MUST
        .captures_iter(request)
        .chain(ALWAYS.captures_iter(request))
    {
        let (Some(whole), Some(verb)) = (cue.get(0), cue.name("verb")) else {
            continue;
        };
        let what = clause(&request[whole.end()..]);
        let (name, place) = match verb.as_str().to_ascii_lowercase().as_str() {
            "end" | "finish" => (what, Some("last".to_owned())),
            "start" | "begin" => (what, Some("first".to_owned())),
            _ => split_place(what),
        };
        push(&mut steps, request, whole.start(), name, place);
    }
    for cue in STEP.captures_iter(request) {
        let (Some(whole), Some(what)) = (cue.get(0), cue.name("what")) else {
            continue;
        };
        let place = place_of(clause(&request[whole.end()..]));
        push(&mut steps, request, whole.start(), what.as_str(), place);
    }
    steps
}

/// A step said at `at` in `request`: named from `what`, done as the whole sentence says.
fn push(steps: &mut Vec<RequiredStep>, request: &str, at: usize, what: &str, when: Option<String>) {
    let name = step_name(what);
    if name.is_empty() || steps.iter().any(|step| super::same_name(&step.name, &name)) {
        return;
    }
    steps.push(RequiredStep {
        role: role_for(&name),
        does: Some(sentence_at(request, at)),
        name,
        when,
    });
}

/// The words after a cue, up to the sentence's end or the next `,` or `;`.
fn clause(text: &str) -> &str {
    let end = sentence_end(text).unwrap_or(text.len());
    let text = &text[..end];
    let end = text.find([',', ';']).unwrap_or(text.len());
    text[..end].trim()
}

/// `a secrets scan after build` is the step `a secrets scan`, placed `after build`.
fn split_place(what: &str) -> (&str, Option<String>) {
    let plain = what.to_ascii_lowercase();
    for word in [" before ", " after "] {
        if let Some(at) = plain.find(word)
            && let Some(place) = place_of(&what[at..])
        {
            return (&what[..at], Some(place));
        }
    }
    (what, None)
}

/// `first`, `last`, `before <step>` or `after <step>` as words put it: `before the sign-off` is
/// `before sign-off`.
fn place_of(text: &str) -> Option<String> {
    let text = text.trim();
    let plain = text.to_ascii_lowercase();
    if plain == "first" || plain == "last" {
        return Some(plain);
    }
    ["before ", "after "].into_iter().find_map(|word| {
        let target = strip_article(strip_word(text, word)?);
        (!target.is_empty()).then(|| format!("{}{target}", word))
    })
}

fn strip_article(text: &str) -> &str {
    ["a ", "an ", "the "]
        .into_iter()
        .find_map(|article| strip_word(text, article))
        .unwrap_or(text)
}

/// `text` after `word`, whatever its case, when it starts with it and goes on.
fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let head = text.get(..word.len())?;
    let rest = text[word.len()..].trim_start();
    (head.eq_ignore_ascii_case(word) && !rest.is_empty()).then_some(rest)
}

/// `a benchmark run` is `Benchmark run`, and `the secrets scan step` is `Secrets scan`.
fn step_name(what: &str) -> String {
    let mut name = strip_article(what.trim()).trim();
    for word in [" step", " phase"] {
        let cut = name.len().saturating_sub(word.len());
        if cut > 0
            && name
                .get(cut..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(word))
        {
            name = name[..cut].trim_end();
        }
    }
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Who a step said in words falls to: a review, an audit or a scan to the reviewers, a sign-off
/// to the judge, anything else to the workers.
fn role_for(name: &str) -> CrewRole {
    let plain = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| plain.contains(word));
    if has(&["review", "audit", "scan", "inspect"]) {
        CrewRole::Reviewers
    } else if has(&["sign off", "sign-off", "signoff", "approv"]) {
        CrewRole::Judge
    } else {
        CrewRole::Workers
    }
}

/// The sentence of `request` around byte `at`, as the request said it.
fn sentence_at(request: &str, at: usize) -> String {
    let start = request[..at]
        .rfind(['.', '!', '?', '\n'])
        .map_or(0, |end| end + 1);
    let end = sentence_end(&request[at..]).map_or(request.len(), |end| at + end + 1);
    let sentence = request[start..end].trim();
    let mut chars = sentence.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Words a short label leaves out, so `backfill the old rows` is `backfill-rows`.
const FILLER: [&str; 22] = [
    "a", "an", "the", "to", "of", "for", "in", "on", "into", "onto", "from", "with", "by", "at",
    "and", "or", "old", "new", "all", "our", "my", "its",
];

/// The short label a task goes by, such as `migrate-schema` for `migrate the schema`: its
/// first two words that say something.
pub fn label(task: &str) -> String {
    let words: Vec<String> = task
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let telling: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|word| !FILLER.contains(word))
        .take(2)
        .collect();
    if telling.is_empty() {
        words.iter().take(2).cloned().collect::<Vec<_>>().join("-")
    } else {
        telling.join("-")
    }
}
