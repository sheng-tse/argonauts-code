//! What a seat reads of another seat's message: names hidden, a model naming itself and
//! signatures gone, tool marks in plain words and light Markdown dropped, with code left as
//! written.

use regex::Regex;

use argonauts_core::events::council::{HiddenName, TextPart};
use argonauts_core::names::SeatId;

use super::HIDDEN;
use crate::council::lexicon::Lexicon;

/// Where a seat stands in text being stripped, so stripping never touches it.
pub(super) const SEAT_OPEN: char = '\u{E000}';
pub(super) const SEAT_CLOSE: char = '\u{E001}';

/// What `text` still gives away outside code: a name that isn't a topic word. Every text a seat
/// or a check reads passes this.
pub(super) fn leaks(lexicon: &Lexicon, text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for segment in segments(text) {
        let Segment::Prose(prose) = segment else {
            continue;
        };
        found.extend(
            lexicon
                .find(&prose)
                .into_iter()
                .map(|hit| prose[hit.range].to_owned()),
        );
    }
    found
}

/// The words a seat reads of `parts`, with each seat a placeholder, and what was hidden.
pub(super) fn strip(lexicon: &Lexicon, parts: &[TextPart], plain: bool) -> Stripped {
    let mut held = String::new();
    let mut seat = 0;
    for part in parts {
        match part {
            TextPart::Words(words) => held.push_str(words),
            TextPart::Seat(_) => {
                held.push(SEAT_OPEN);
                held.push_str(&seat.to_string());
                held.push(SEAT_CLOSE);
                seat += 1;
            }
        }
    }
    let held = without_coded_self_references(lexicon, &held);
    let mut stripped = Stripped::default();
    let mut text = String::new();
    for segment in segments(&held) {
        match segment {
            Segment::Code(code) => text.push_str(&strip_code(lexicon, &code, &mut stripped)),
            Segment::Prose(prose) => {
                text.push_str(&strip_prose(lexicon, &prose, plain, &mut stripped))
            }
        }
    }
    stripped.leaks = !leaks(lexicon, &text).is_empty();
    stripped.text = tidy(&text);
    stripped
}

fn strip_prose(lexicon: &Lexicon, prose: &str, plain: bool, stripped: &mut Stripped) -> String {
    let text = plain_marks(prose);
    let text = without_signatures(&text, lexicon);
    let text = plain_tools(lexicon, &text, stripped);
    let text = without_self_references(lexicon, &text);
    let text = hide_names(lexicon, &text, stripped);
    // A bubble shows plain text anyway; a plan keeps its Markdown.
    if plain { light_markdown(&text) } else { text }
}

/// Tool rows and calls in plain words: "⏺ Bash(npm test) ⎿ 12 passed" reads "Ran npm
/// test: 12 passed"; a citation reads `path:line`; a CLI's home folder is hidden.
fn plain_tools(lexicon: &Lexicon, text: &str, stripped: &mut Stripped) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        let mark = lexicon
            .row_marks()
            .iter()
            .find(|mark| trimmed.starts_with(**mark));
        match mark {
            Some(mark) if continues_a_row(mark) => {
                let rest = trimmed[mark.len()..].trim();
                match lines.last_mut() {
                    Some(last) if !last.trim().is_empty() => {
                        last.push_str(": ");
                        last.push_str(rest);
                    }
                    _ => lines.push(rest.to_owned()),
                }
            }
            Some(mark) => {
                let words = mark
                    .trim_start_matches(|c: char| !c.is_alphanumeric())
                    .trim();
                let rest = trimmed[mark.len()..].trim_start();
                lines.push(if words.is_empty() {
                    rest.to_owned()
                } else {
                    format!("{words} {rest}")
                });
            }
            None => lines.push(line.to_owned()),
        }
    }
    let mut text = lines.join("\n");
    for mark in lexicon.row_marks() {
        if continues_a_row(mark) {
            text = text.replace(&format!(" {mark} "), ": ");
        }
    }
    for tool in lexicon.tools() {
        let plain = lexicon.plain_tool(tool).unwrap_or("a tool");
        let call = Regex::new(&format!(r"\b{}\(([^()\n]*)\)", regex::escape(tool)))
            .expect("a tool call's pattern");
        text = call
            .replace_all(&text, |found: &regex::Captures<'_>| {
                tool_words(plain, found[1].trim())
            })
            .into_owned();
        let named =
            Regex::new(&format!(r"\b{} tool\b", regex::escape(tool))).expect("a tool's pattern");
        text = named
            .replace_all(&text, format!("{plain} tool").as_str())
            .into_owned();
    }
    for tool in lexicon.distinct_tools() {
        let plain = lexicon.plain_tool(tool).unwrap_or("a tool");
        let bare = Regex::new(&format!(r"\b{}\b", regex::escape(tool))).expect("a pattern");
        text = bare.replace_all(&text, plain).into_owned();
    }
    for citation in lexicon.citations() {
        let pattern = format!(
            r"{}([^{}{}\s]+)(?:{}(\d+)(?:-L?\d+)?)?{}",
            regex::escape(citation.open),
            regex::escape(citation.lines),
            regex::escape(citation.close),
            regex::escape(citation.lines),
            regex::escape(citation.close)
        );
        let cited = Regex::new(&pattern).expect("a citation's pattern");
        text = cited
            .replace_all(&text, |found: &regex::Captures<'_>| match found.get(2) {
                Some(line) => format!("{}:{}", &found[1], line.as_str()),
                None => found[1].to_owned(),
            })
            .into_owned();
    }
    for folder in lexicon.home_folders() {
        let count = text.matches(folder).count() as u32;
        if count > 0 {
            text = text.replace(folder, HIDDEN);
            stripped.hidden += count;
            stripped.kinds.push(HiddenName::Cli);
        }
    }
    text
}

/// A model naming itself goes: "As Claude, I'd keep the window" reads "I'd keep the
/// window", and "Codex here." or "I'm an AI model made by OpenAI." go whole, even when the
/// talk is about the name it uses.
fn without_self_references(lexicon: &Lexicon, text: &str) -> String {
    let mut text = text.to_owned();
    for _ in 0..8 {
        let hits = lexicon.every_name(&text);
        if hits.is_empty() {
            break;
        }
        let mut written: Vec<String> = hits
            .iter()
            .map(|hit| regex::escape(&text[hit.range.clone()]))
            .collect();
        written.sort_by_key(|name| std::cmp::Reverse(name.len()));
        written.dedup();
        let name = written.join("|");
        let made = r"(?:made|trained|built|developed|created)";
        // "as Claude", "as a Claude model", "as a fellow Claude", "being trained by OpenAI".
        let kind = r"(?:\s+(?:model|assistant|AI)s?)?";
        let opening = Regex::new(&format!(
            r"(?i)\b(?:as (?:(?:an?|the) )?(?:fellow )?(?:{name}){kind}|as an AI[^,.!?\n]{{0,40}}?\b{made} by (?:{name})|being {made} by (?:{name}))\b[^,.!?\n]{{0,30}},\s*"
        ))
        .expect("a pattern");
        let intro = Regex::new(&format!(
            r"(?i)\b(?:i'm|i am|this is|it's|you're talking to)\s+(?:(?:an?|the) )?(?:{name}){kind}\b|\b(?:{name})\s+here\b|\bi(?:'m| am) an AI\b[^.!?\n]{{0,60}}\b{made} by (?:{name})\b"
        ))
        .expect("a pattern");
        let opens = opening
            .find_iter(&text)
            .map(|found| found.range())
            .find(|range| starts_a_clause(&text, range.start));
        let introduces = intro
            .find(&text)
            .map(|found| clause_around(&text, found.start(), found.end()));
        let changed = match (opens, introduces) {
            (Some(range), _) => {
                let start = range.start;
                text.replace_range(range, "");
                capitalize_at(&mut text, start);
                true
            }
            (None, Some((start, end))) => {
                text.replace_range(start..end, "");
                capitalize_at(&mut text, start);
                true
            }
            (None, None) => false,
        };
        if !changed {
            break;
        }
    }
    text
}

/// Every name left becomes `[hidden]`. One in a sentence that names a seat, or that says
/// who something sounds like, is a guess at who runs that seat. A topic word stays, unless
/// a seat is said to be it, or it names what the writer or a seat is.
fn hide_names(lexicon: &Lexicon, text: &str, stripped: &mut Stripped) -> String {
    let hits = lexicon.every_name(text).into_iter().filter(|hit| {
        !lexicon.is_topic(&text[hit.range.clone()])
            || said_to_be(text, hit.range.start)
            || names_who_someone_is(text, hit.range.clone())
    });
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for hit in hits {
        out.push_str(&text[last..hit.range.start]);
        out.push_str(HIDDEN);
        let (start, end) = sentence_around(text, hit.range.start, hit.range.end);
        if guesses(&text[start..end]) {
            stripped.guesses += 1;
        } else {
            stripped.hidden += 1;
            stripped.kinds.push(hit.kind);
        }
        last = hit.range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// What a seat reads of a message, with seats as placeholders, and what was hidden from it.
#[derive(Debug, Default)]
pub(super) struct Stripped {
    pub(super) text: String,
    pub(super) hidden: u32,
    pub(super) guesses: u32,
    pub(super) kinds: Vec<HiddenName>,
    pub(super) leaks: bool,
}

/// A part of a message: words, or code, which is never renamed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Segment {
    Prose(String),
    Code(String),
}

/// `text` cut into prose and code: fenced blocks and inline code spans.
pub(super) fn segments(text: &str) -> Vec<Segment> {
    let mut segments: Vec<Segment> = Vec::new();
    let mut push = |segment: Segment| match (segments.last_mut(), segment) {
        (Some(Segment::Prose(last)), Segment::Prose(more)) => last.push_str(&more),
        (Some(Segment::Code(last)), Segment::Code(more)) => last.push_str(&more),
        (_, segment) => segments.push(segment),
    };
    let mut fenced = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            push(Segment::Code(line.to_owned()));
            continue;
        }
        if fenced {
            push(Segment::Code(line.to_owned()));
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let Some(close) = rest[open + 1..].find('`') else {
                break;
            };
            let close = open + 1 + close;
            push(Segment::Prose(rest[..open].to_owned()));
            push(Segment::Code(rest[open..=close].to_owned()));
            rest = &rest[close + 1..];
        }
        push(Segment::Prose(rest.to_owned()));
    }
    segments
}

/// Inside code a name stays as written, since code means what it says. What only a CLI
/// writes goes all the same: signatures, its home folders, the tools no other CLI has, and
/// its transcript rows, which read in plain words as they do in prose.
fn strip_code(lexicon: &Lexicon, code: &str, stripped: &mut Stripped) -> String {
    code.split_inclusive('\n')
        .filter(|line| !is_signature_line(line))
        .map(|line| {
            let trimmed = line.trim_start();
            let backtick = trimmed.trim_start_matches('`');
            let row = lexicon
                .row_marks()
                .iter()
                .any(|mark| backtick.starts_with(*mark));
            if row {
                return plain_tools(lexicon, line, stripped);
            }
            let mut line = line.to_owned();
            for tool in lexicon.distinct_tools() {
                let plain = lexicon.plain_tool(tool).unwrap_or("a tool");
                let bare = Regex::new(&format!(r"\b{}\b", regex::escape(tool))).expect("a pattern");
                line = bare.replace_all(&line, plain).into_owned();
            }
            for folder in lexicon.home_folders() {
                let count = line.matches(folder).count() as u32;
                if count > 0 {
                    line = line.replace(folder, HIDDEN);
                    stripped.hidden += count;
                    stripped.kinds.push(HiddenName::Cli);
                }
            }
            line
        })
        .collect()
}

/// A model naming itself in code, as "I'm `claude-opus-5-5`, and I'd keep it", goes as it
/// does in prose: the clause that names it, and the rest stays.
fn without_coded_self_references(lexicon: &Lexicon, text: &str) -> String {
    static INTRO: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:i'm|i am|this is|it's|as)\s+`([^`\n]+)`[^,.!?\n`]{0,30}(?:,\s*)?")
            .expect("a pattern")
    });
    let mut text = text.to_owned();
    for _ in 0..8 {
        let found = INTRO.captures_iter(&text).find_map(|found| {
            let code = found.get(1)?.as_str();
            let names_itself = lexicon
                .every_name(code)
                .iter()
                .any(|hit| hit.range == (0..code.len()));
            names_itself.then(|| found.get(0).map(|whole| whole.range()))?
        });
        let Some(range) = found else {
            break;
        };
        let start = range.start;
        text.replace_range(range, "");
        capitalize_at(&mut text, start);
    }
    text
}

/// Curly quotes, long dashes and hyphens as plain ones, the spaces that don't break as plain
/// spaces, and no emoji or zero-width marks, so a name can't hide behind them. It costs
/// nothing and hides little.
pub(super) fn plain_marks(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' => Some('\''),
            '\u{201C}' | '\u{201D}' | '\u{201F}' => Some('"'),
            '\u{2010}' | '\u{2011}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => Some('-'),
            '\u{00A0}' | '\u{2007}' | '\u{202F}' => Some(' '),
            '\u{FE0F}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}' | '\u{FEFF}' => None,
            c if ('\u{1F000}'..='\u{1FAFF}').contains(&c) => None,
            c => Some(c),
        })
        .collect()
}

fn is_signature_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    lower.starts_with("co-authored-by:")
        || lower.starts_with("generated with ")
        || lower.starts_with("generated by ")
        || lower.starts_with("sent from ")
        || lower.starts_with("(sent from ")
        || lower.starts_with("🤖")
}

/// Signature lines go, and so does a last line that is a dash and a name.
fn without_signatures(text: &str, lexicon: &Lexicon) -> String {
    let mut lines: Vec<&str> = text
        .split('\n')
        .filter(|line| !is_signature_line(line))
        .collect();
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    if let Some(last) = last {
        let trimmed = lines[last].trim();
        let signed = trimmed
            .strip_prefix("--")
            .or_else(|| trimmed.strip_prefix('-'))
            .map(str::trim)
            .is_some_and(|name| {
                !name.is_empty()
                    && name.len() <= 40
                    && (!lexicon.every_name(name).is_empty() || name.starts_with(SEAT_OPEN))
            });
        // "Best,\nClaude": a closing, then a line that is only a name.
        let closing = last > 0 && {
            let before = lines[last - 1].trim();
            before.ends_with(',') && before.len() <= 20
        };
        let only_a_name = trimmed.len() <= 40
            && lexicon
                .every_name(trimmed)
                .first()
                .is_some_and(|hit| hit.range == (0..trimmed.len()));
        if signed {
            lines.remove(last);
        } else if only_a_name && closing {
            lines.remove(last);
            lines.remove(last - 1);
        }
    }
    lines.join("\n")
}

/// A mark that carries the row above on, such as `⎿` or `└`.
fn continues_a_row(mark: &str) -> bool {
    mark.chars()
        .all(|c| c == '\u{23BF}' || ('\u{2500}'..='\u{257F}').contains(&c) || c == ' ')
}

/// A tool call in plain words: "Ran npm test", "Read src/lib.rs".
fn tool_words(plain: &str, args: &str) -> String {
    let verb = match plain {
        "shell" => "Ran",
        "read" => "Read",
        "edit" => "Edited",
        "write" => "Wrote",
        "patch" => "Patched",
        "search" | "file search" => "Searched for",
        "web fetch" => "Fetched",
        "web search" => "Searched the web for",
        "to-do list" => return "Updated the to-do list".to_owned(),
        "plan" => return "Updated the plan".to_owned(),
        "agent" => "Asked an agent:",
        _ => "Used a tool on",
    };
    if args.is_empty() {
        verb.to_owned()
    } else {
        format!("{verb} {args}")
    }
}

/// Headings, bold and italics as plain text, since a bubble shows plain text anyway.
fn light_markdown(text: &str) -> String {
    static HEADING: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?m)^(\s*)#{1,6}\s+").expect("a pattern"));
    static BOLD: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"\*\*([^*\n]+)\*\*|__([^_\n]+)__").expect("a pattern")
    });
    static ITALIC: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(^|[^\w*])\*([^*\s](?:[^*\n]*[^*\s])?)\*($|[^\w*])").expect("a pattern")
    });
    let text = HEADING.replace_all(text, "$1");
    let text = BOLD.replace_all(&text, "$1$2");
    ITALIC.replace_all(&text, "$1$2$3").into_owned()
}

/// Spaces that removals left doubled, and lines left with nothing, tidied.
fn tidy(text: &str) -> String {
    static DOUBLED: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(\S) {2,}").expect("a pattern"));
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| DOUBLED.replace_all(line.trim_end(), "$1 ").into_owned())
        .collect();
    lines.join("\n").trim().to_owned()
}

/// Whether the text before `at` ends a clause, so what starts at `at` opens one.
fn starts_a_clause(text: &str, at: usize) -> bool {
    let before = text[..at].trim_end_matches(' ');
    before.is_empty()
        || before.ends_with(['.', '!', '?', '\n', ':', ';'])
        || before.ends_with(SEAT_CLOSE)
}

/// The sentence holding `start..end`: from the end of the one before to its own end,
/// with the spaces after it.
fn sentence_around(text: &str, start: usize, end: usize) -> (usize, usize) {
    let from = text[..start]
        .rfind(['.', '!', '?', '\n'])
        .map_or(0, |at| at + 1);
    let from = from + text[from..start].len() - text[from..start].trim_start().len();
    let to = text[end..]
        .find(['.', '!', '?', '\n'])
        .map_or(text.len(), |at| end + at + 1);
    let to = to + text[to..].len() - text[to..].trim_start_matches(' ').len();
    (from, to.min(text.len()))
}

/// Upper-cases the letter at `at`, which a removal left opening a sentence.
fn capitalize_at(text: &mut String, at: usize) {
    if !starts_a_clause(text, at) {
        return;
    }
    if let Some(first) = text[at..].chars().next()
        && first.is_lowercase()
    {
        let upper: String = first.to_uppercase().collect();
        text.replace_range(at..at + first.len_utf8(), &upper);
    }
}

/// Whether a sentence guesses who runs a seat: it names a seat, or says whom something
/// sounds like.
fn guesses(sentence: &str) -> bool {
    const LIKE: [&str; 7] = [
        "sounds like",
        "reads like",
        "writes like",
        "must be",
        "is probably",
        "seems to be",
        "you're",
    ];
    let lower = sentence.to_lowercase();
    sentence.contains(SEAT_OPEN) || LIKE.iter().any(|like| lower.contains(like))
}

/// Whether the name at `at` is what a seat is said to be, as "Seat T sounds like Codex" or
/// "you must be a GPT". The talk may be about Codex, but never about who runs a seat.
fn said_to_be(text: &str, at: usize) -> bool {
    const SAID: [&str; 15] = [
        "sounds like",
        "sound like",
        "reads like",
        "writes like",
        "must be",
        "is probably",
        "are probably",
        "seems to be",
        "is actually",
        "is clearly",
        "runs on",
        "is running on",
        "is powered by",
        "is built on",
        "fellow",
    ];
    let (start, end) = sentence_around(text, at, at);
    let to_a_seat = text[start..end].contains(SEAT_OPEN)
        || text[start..end]
            .to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
            .any(|word| matches!(word, "you" | "you're" | "your"));
    let before = text[start..at].trim_end().to_ascii_lowercase();
    let before = ["a", "an", "the"]
        .iter()
        .find_map(|article| {
            before
                .strip_suffix(article)
                .filter(|rest| rest.ends_with(' '))
        })
        .map_or(before.as_str(), str::trim_end);
    to_a_seat && SAID.iter().any(|said| before.ends_with(said))
}

/// Whether the name in `range` says what the writer or a seat is, in a sentence about
/// themselves: "a Claude model", "we Claude models", "trained by OpenAI". The name of a tool
/// the talk is about, "the Codex adapter", stays.
fn names_who_someone_is(text: &str, range: std::ops::Range<usize>) -> bool {
    let (start, end) = sentence_around(text, range.start, range.end);
    let sentence = text[start..end].to_ascii_lowercase();
    let about_someone = sentence.contains(SEAT_OPEN)
        || sentence
            .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
            .any(|word| {
                matches!(
                    word,
                    "i" | "i'm"
                        | "i'd"
                        | "i've"
                        | "i'll"
                        | "me"
                        | "my"
                        | "we"
                        | "we're"
                        | "us"
                        | "our"
                        | "you"
                        | "you're"
                        | "your"
                )
            });
    if !about_someone {
        return false;
    }
    let after = text[range.end..].trim_start().to_ascii_lowercase();
    let is_a_kind = ["model", "assistant", "ai "]
        .iter()
        .any(|kind| after.starts_with(kind));
    let before = text[start..range.start].trim_end().to_ascii_lowercase();
    let made_by = [
        "made by",
        "trained by",
        "built by",
        "developed by",
        "created by",
    ]
    .iter()
    .any(|made| before.ends_with(made));
    is_a_kind || made_by
}

/// The clause holding `start..end`, with the comma and the "and" that join it to the rest of
/// its sentence, so "I'm a Claude model and I'd keep it" keeps "I'd keep it". A clause that
/// is the whole sentence takes the sentence.
fn clause_around(text: &str, start: usize, end: usize) -> (usize, usize) {
    static JOIN: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"^[^,;.!?\n]{0,40}?[,;]?\s*(?:(?:and|but|so)\s+)?").expect("a pattern")
    });
    let (_, sentence_end) = sentence_around(text, start, end);
    let joined = JOIN.find(&text[end..]).map_or(0, |found| found.end());
    let clause_end = end + joined;
    let rest = &text[clause_end..sentence_end];
    if rest
        .trim_matches(|c: char| c.is_whitespace() || ".!?".contains(c))
        .is_empty()
    {
        return sentence_around(text, start, end);
    }
    (start, clause_end)
}

/// Whether a message is a refusal, which the other seats read as declining to answer.
pub(super) fn declines(raw: &str) -> bool {
    const REFUSALS: [&str; 9] = [
        "i can't help with",
        "i can't assist with",
        "i cannot help with",
        "i cannot assist with",
        "i'm not able to help with",
        "i am not able to help with",
        "i won't be able to help with",
        "sorry, but i can't",
        "i'm sorry, but i can't",
    ];
    let start = plain_marks(raw.trim()).to_lowercase();
    REFUSALS.iter().any(|refusal| start.starts_with(refusal))
}

/// The stripped text with its placeholders drawn as the seats `parts` holds, in order.
pub(super) fn draw_placeholders(
    text: &str,
    parts: &[TextPart],
    name: impl Fn(&SeatId) -> String,
) -> String {
    let seats: Vec<&SeatId> = parts
        .iter()
        .filter_map(|part| match part {
            TextPart::Seat(seat) => Some(seat),
            TextPart::Words(_) => None,
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(SEAT_OPEN) {
        out.push_str(&rest[..open]);
        let after = &rest[open + SEAT_OPEN.len_utf8()..];
        let Some(close) = after.find(SEAT_CLOSE) else {
            rest = after;
            continue;
        };
        if let Some(seat) = after[..close]
            .parse::<usize>()
            .ok()
            .and_then(|n| seats.get(n))
        {
            out.push_str(&name(seat));
        }
        rest = &after[close + SEAT_CLOSE.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// The text cut to `most` characters at a word, with an ellipsis.
pub(super) fn cap(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let cut: String = text.chars().take(most.saturating_sub(1)).collect();
    let at = cut.rfind(char::is_whitespace).unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end())
}
