//! Every message passes through here on its way to a seat: stored once, stripped of who wrote
//! it, and drawn with the reader's letters.
//!
//! A message is kept once, as its writer wrote it, with every seat it names held as the seat
//! itself. Your screen draws those as names and logos. Each seat reads it through
//! [`Relay::render_for`]: model, maker and coding tool names become `[hidden]`, a model naming
//! itself and signatures go, tool marks become plain words, and seats become the reader's own
//! letters. Code is never renamed. Writing style can't be hidden, so this hides what it can,
//! cheaply and exactly, and never paraphrases.

use std::collections::BTreeMap;

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::council::{
    CouncilMessage, HiddenName, Point, Seat, SeatRole, Seating, Speaker, Stance, TextPart,
    draw_held, held_parts, held_seat,
};
use argonauts_core::names::{SeatId, SeatLetter};
use argonauts_core::steps::plan::{PlanChoice, PlanDraft, PlanPiece};
use regex::Regex;

mod strip;

use self::strip::{Segment, cap, declines, draw_placeholders, segments};
use super::letters::Letters;
use super::lexicon::Lexicon;
use super::seats;
use crate::prompts::council as prompt;

/// What a name becomes before a seat reads it.
pub const HIDDEN: &str = "[hidden]";
/// The longest message a seat reads. Length is a bias, judges favour longer answers, and a
/// fingerprint too.
pub const MESSAGE_CHARS: usize = 1200;
/// More hidden than this in one message, and its writer is asked once to write it again.
const MOST_HIDDEN: u32 = 3;

/// The one turn back a message gets when it names who runs a seat.
pub const REWRITE_ASK: &str = "Your message names a model, maker or coding tool, or guesses who \
                               a seat is. Write it again without that.";
/// The one turn back a message gets when it runs past [`MESSAGE_CHARS`].
pub const SHORTEN_ASK: &str =
    "Your message is longer than 1,200 characters. Write it again, shorter.";

/// A seat's message as code takes it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taken {
    /// The words as written, each seat held as the seat.
    pub text: Vec<TextPart>,
    /// The seats it named with @.
    pub mentions: Vec<SeatId>,
    /// Names the seats won't read, and what each named.
    pub hidden: u32,
    pub kinds: Vec<HiddenName>,
    /// Guesses at who runs a seat, hidden the same way.
    pub guesses_hidden: u32,
    /// It matches a refusal, so the seats read that it declined to answer.
    pub declined: bool,
    /// What its writer is asked once before any seat reads it, if anything.
    pub ask: Option<String>,
    /// A second try still named someone: the seats read code's own rewrite of it.
    pub rewritten: bool,
}

/// A letter that names no seat in its writer's map, refused so the writer can fix it once.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{ask}")]
pub struct UnknownLetter {
    pub letter: char,
    /// What the writer is told.
    pub ask: String,
}

/// A name in your message that fits more than one seat, which the input asks about before
/// sending: "Opus 5.5 · 1 or · 2?".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{written} could be {} seats", seats.len())]
pub struct Ambiguous {
    pub written: String,
    pub seats: Vec<SeatId>,
}

/// One council's relay: who sits, each reader's letters, and the names to hide.
#[derive(Debug, Clone)]
pub struct Relay {
    seating: Seating,
    letters: BTreeMap<SeatId, Letters>,
    lexicon: Lexicon,
    names: DisplayNames,
}

impl Relay {
    /// The names it hides, and the topic words it keeps.
    pub fn lexicon(&self) -> &Lexicon {
        &self.lexicon
    }

    pub fn new(
        seating: Seating,
        letters: BTreeMap<SeatId, Letters>,
        lexicon: Lexicon,
        names: DisplayNames,
    ) -> Self {
        Self {
            seating,
            letters,
            lexicon,
            names,
        }
    }

    pub fn seating(&self) -> &Seating {
        &self.seating
    }

    pub fn letters(&self, reader: &SeatId) -> Option<&Letters> {
        self.letters.get(reader)
    }

    /// A seat's message on its way in: its seats held as seats through the writer's letters,
    /// what the seats won't read counted, and what to ask its writer once, if anything.
    pub fn take_in(&self, writer: &SeatId, raw: &str) -> Result<Taken, UnknownLetter> {
        let text = self.seats_in(writer, raw)?;
        let mentions = mentions(&text);
        let stripped = strip::strip(&self.lexicon, &text, true);
        let mut asks = Vec::new();
        if stripped.leaks || stripped.hidden + stripped.guesses > MOST_HIDDEN {
            asks.push(REWRITE_ASK);
        }
        if stripped.text.chars().count() > MESSAGE_CHARS {
            asks.push(SHORTEN_ASK);
        }
        Ok(Taken {
            mentions,
            hidden: stripped.hidden,
            kinds: stripped.kinds,
            guesses_hidden: stripped.guesses,
            declined: declines(raw),
            ask: (!asks.is_empty()).then(|| asks.join(" ")),
            rewritten: false,
            text,
        })
    }

    /// The message its writer wrote again after its one turn back. Nothing more is asked: if it
    /// still names someone, the seats read code's rewrite, and your bubble says so.
    pub fn take_again(&self, writer: &SeatId, raw: &str) -> Result<Taken, UnknownLetter> {
        let mut taken = self.take_in(writer, raw)?;
        taken.rewritten = taken
            .ask
            .as_deref()
            .is_some_and(|ask| ask.contains(REWRITE_ASK));
        taken.ask = None;
        Ok(taken)
    }

    /// Your message, with each seat it names held as the seat, so each reader reads its own
    /// letter: "@Opus 5.5 · 2" reaches Opus 5.5 · 1 as "@Seat T". A bare model name that fits
    /// two seats is asked about first. Anything else you write reaches the seats as written.
    pub fn from_you(&self, text: &str) -> Result<Vec<TextPart>, Ambiguous> {
        let named = self.named_seats();
        let mut parts = Vec::new();
        let mut words = String::new();
        for segment in segments(text) {
            let prose = match segment {
                Segment::Code(code) => {
                    words.push_str(&code);
                    continue;
                }
                Segment::Prose(prose) => prose,
            };
            let mut at = 0;
            while at < prose.len() {
                let starts_a_word = at == 0 || !is_word(prose.as_bytes()[at - 1]);
                let seat = named.iter().find_map(|(name, seat)| {
                    starts_with_name(&prose[at..], name).then(|| (name.len(), seat.clone()))
                });
                if let Some((len, seat)) = seat.filter(|_| starts_a_word) {
                    push_seat(&mut parts, &mut words, seat);
                    at += len;
                    continue;
                }
                let hit = starts_a_word
                    .then(|| self.lexicon.find(&prose[at..]).into_iter().next())
                    .flatten()
                    .filter(|hit| hit.range.start == 0);
                if let Some(hit) = hit {
                    let written = &prose[at..at + hit.range.end];
                    let seats = self.seats_called(written);
                    match seats.as_slice() {
                        [seat] => push_seat(&mut parts, &mut words, seat.clone()),
                        [] => words.push_str(written),
                        _ => {
                            return Err(Ambiguous {
                                written: written.to_owned(),
                                seats,
                            });
                        }
                    }
                    at += hit.range.end;
                    continue;
                }
                let next = next_boundary(&prose, at);
                words.push_str(&prose[at..next]);
                at = next;
            }
        }
        if !words.is_empty() {
            parts.push(TextPart::Words(words));
        }
        Ok(parts)
    }

    /// Your words that reach `reader` outside a message of yours, such as the goal, a point of
    /// yours or a note, with each seat they name in the reader's letters. A name that could be
    /// several seats is hidden, since nothing can ask you which you meant.
    pub fn yours_for(&self, reader: &SeatId, text: &str) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        let Ok(parts) = self.from_you(text) else {
            return hide_who_wrote(&self.lexicon, text);
        };
        parts
            .iter()
            .map(|part| match part {
                TextPart::Words(words) => words.clone(),
                TextPart::Seat(seat) => seat_words(letters, seat),
            })
            .collect()
    }

    /// Your words held as the seats they name, as a point of yours keeps them, or as written
    /// when a name could be several seats.
    pub fn yours_held(&self, text: &str) -> String {
        self.from_you(text)
            .map_or_else(|_| text.to_owned(), |parts| with_markers(&parts))
    }

    /// The names of models, makers and tools in your message that are no seat's, which the
    /// seats will read as written: the input says so in a dim line.
    pub fn read_as_written(&self, text: &str) -> Vec<String> {
        let mut written = Vec::new();
        for segment in segments(text) {
            let Segment::Prose(prose) = segment else {
                continue;
            };
            for hit in self.lexicon.find(&prose) {
                let name = &prose[hit.range];
                if self.seats_called(name).is_empty() && !written.iter().any(|seen| seen == name) {
                    written.push(name.to_owned());
                }
            }
        }
        written
    }

    /// A message's words as `reader` reads them: a seat's stripped of who wrote it, yours as
    /// you wrote them, and every seat drawn with the reader's letters.
    pub fn render_for(&self, reader: &SeatId, message: &CouncilMessage) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        match &message.from {
            Speaker::You => message.words(|seat| {
                let drawn = seat_words(letters, seat);
                if seat == reader {
                    format!("{drawn} (you)")
                } else {
                    drawn
                }
            }),
            Speaker::Seat(_) => {
                let stripped = strip::strip(&self.lexicon, &message.text, true);
                let drawn = draw_placeholders(&stripped.text, &message.text, |seat| {
                    seat_words(letters, seat)
                });
                cap(&drawn, MESSAGE_CHARS)
            }
        }
    }

    /// The line a message reaches `reader` as, in its session: who wrote it, whom it answers
    /// with the words it quotes, and its words, all in the reader's letters.
    pub fn turn_line(
        &self,
        reader: &SeatId,
        message: &CouncilMessage,
        replied_to: Option<&CouncilMessage>,
    ) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        let writer = match &message.from {
            Speaker::You => "the user".to_owned(),
            Speaker::Seat(seat) => seat_words(letters, seat),
        };
        if message.declined {
            return format!("From the council · {writer} declined to answer this point");
        }
        let body = self.render_for(reader, message);
        let reply = message
            .reply_to
            .as_ref()
            .zip(replied_to)
            .map(|(reply, to)| {
                let whom = match &to.from {
                    Speaker::Seat(seat) if seat == reader => "you".to_owned(),
                    Speaker::Seat(seat) => seat_words(letters, seat),
                    Speaker::You => "the user".to_owned(),
                };
                let quote = self.quote_for(reader, &reply.quote, &to.from);
                format!(", replying to {whom} (\"{quote}\")")
            });
        format!(
            "From the council · {writer}{}: {body}",
            reply.unwrap_or_default()
        )
    }

    /// The tapbacks on `on` as `reader` reads them, each cast before its seat saw the others':
    /// "Tapbacks on Seat T's message: Seat M ✘ · yours ✘".
    pub fn tapbacks_line(
        &self,
        reader: &SeatId,
        on: &CouncilMessage,
        tapbacks: &[(SeatId, Stance)],
    ) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        let whose = match &on.from {
            Speaker::Seat(seat) if seat == reader => "your".to_owned(),
            Speaker::Seat(seat) => format!("{}'s", seat_words(letters, seat)),
            Speaker::You => "the user's".to_owned(),
        };
        let cast: Vec<String> = tapbacks
            .iter()
            .map(|(seat, stance)| {
                let who = if seat == reader {
                    "yours".to_owned()
                } else {
                    seat_words(letters, seat)
                };
                format!("{who} {}", stance_mark(*stance))
            })
            .collect();
        format!(
            "From the council · Tapbacks on {whose} message: {}",
            cast.join(" · ")
        )
    }

    /// A point every seat now agrees on, as `reader` reads it, with how many of the points
    /// are agreed: "Point 4 agreed: a token bucket · 4 of 6 points agreed". A seat worded the
    /// point, so its words are stripped like a message, and its seats drawn in letters.
    pub fn agreed_line(&self, reader: &SeatId, point: &Point, agreed: usize, of: usize) -> String {
        if !self.letters.contains_key(reader) {
            return String::new();
        }
        format!(
            "From the council · Point {} agreed: {} · {agreed} of {of} points agreed",
            point.number,
            self.held_for(reader, &point.words)
        )
    }

    /// Words a seat wrote that code keeps with their seats held, such as a point's or a side's,
    /// as `reader` reads them: stripped like a message, its seats in the reader's letters.
    pub fn held_for(&self, reader: &SeatId, words: &str) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        let parts = held_seats(words);
        let stripped = strip::strip(&self.lexicon, &parts, true);
        draw_placeholders(&stripped.text, &parts, |seat| seat_words(letters, seat))
    }

    /// Words quoted from a message `from` wrote, as `reader` reads them: a seat's stripped of
    /// who wrote them as its message is, yours as you wrote them, each seat in the reader's
    /// letters, and never longer than a message.
    pub fn quote_for(&self, reader: &SeatId, quote: &str, from: &Speaker) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        let drawn = match from {
            Speaker::You => draw_markers(quote, |seat| seat_words(letters, seat)),
            Speaker::Seat(_) => self.held_for(reader, quote),
        };
        cap(&drawn, MESSAGE_CHARS)
    }

    /// A quote a seat wrote, in the form quotes are checked in: its seats held as seats.
    pub fn quote_in(&self, writer: &SeatId, quote: &str) -> Result<String, UnknownLetter> {
        let parts = self.seats_in(writer, quote)?;
        Ok(with_markers(&parts))
    }

    /// Whether `quote`, as [`Self::quote_in`] keeps it, is `message`'s words, word for word,
    /// as the quoting seat read them. A seat never read another's words as written, so a
    /// quote of them is a guess, and matching it would confirm the guess.
    pub fn quote_matches(&self, quote: &str, message: &CouncilMessage) -> bool {
        let quote =
            squeeze(quote.trim_matches(|c: char| c == '"' || c == '…' || c.is_whitespace()));
        if quote.is_empty() {
            return false;
        }
        let read = match message.from {
            Speaker::Seat(_) => {
                let stripped = strip::strip(&self.lexicon, &message.text, true);
                squeeze(&draw_placeholders(&stripped.text, &message.text, |seat| {
                    held_seat(seat)
                }))
            }
            Speaker::You => squeeze(&with_markers(&message.text)),
        };
        read.contains(&quote)
    }

    /// What `text` still gives away outside code: a model's, maker's or coding tool's name that
    /// isn't a topic word. Every text a seat or a check reads can be held to this.
    pub fn leaks(&self, text: &str) -> Vec<String> {
        strip::leaks(&self.lexicon, text)
    }

    /// The frame `reader` reads first: its own letter, the others', how many sit, who the
    /// skeptic is when it isn't the reader, its role, and the notes when it keeps them. The
    /// words are the same in every council but for the letters and the count, and say any seat
    /// may run the same model as the reader, whatever the truth.
    pub fn frame_for(&self, reader: &SeatId) -> String {
        let Some(letters) = self.letters.get(reader) else {
            return String::new();
        };
        let mut others: Vec<SeatLetter> = letters.others().map(|(_, letter)| letter).collect();
        // By letter, never in seat order, which would say who sat down first.
        others.sort();
        let named: Vec<String> = others
            .iter()
            .map(|letter| format!("Seat {letter}"))
            .collect();
        let skeptics: Vec<String> = seats::skeptics(&self.seating)
            .into_iter()
            .filter(|seat| seat.id != *reader)
            .map(|seat| {
                in_use("council/skeptic-note", prompt::SKEPTIC_NOTE)
                    .trim_end()
                    .replace("{{skeptic}}", &seat_words(letters, &seat.id))
            })
            .collect();
        let me = seats::find(&self.seating, reader);
        let mut role = match me.map(|seat| seat.role) {
            Some(SeatRole::Skeptic) => in_use("council/skeptic", prompt::SKEPTIC),
            Some(SeatRole::Checker) => in_use("council/checker", prompt::CHECKER),
            _ => in_use("council/proposer", prompt::PROPOSER),
        }
        .trim_end()
        .to_owned();
        if self.seating.notes.as_ref() == Some(reader) {
            role.push('\n');
            role.push_str(in_use("council/notes", prompt::NOTES).trim_end());
        }
        let example = others.first().copied().unwrap_or(letters.own());
        let frame = in_use("council/seat", prompt::SEAT)
            .replace("{{me}}", &letters.own().to_string())
            .replace("{{count}}", &self.seating.seats.len().to_string())
            .replace("{{others}}", &and_list(&named))
            .replace("{{example}}", &example.to_string())
            .replace("{{skeptic_note}}", &skeptics.join("\n"))
            .replace("{{role}}", &role);
        // A council without a skeptic leaves no empty line where the note would be.
        let mut lines: Vec<&str> = Vec::new();
        for line in frame.lines() {
            if line.is_empty() && lines.last().is_some_and(|last| last.starts_with("- ")) {
                continue;
            }
            lines.push(line);
        }
        let mut frame = lines.join("\n");
        frame.push('\n');
        frame
    }

    /// The seats `name` could mean: those whose name, as you read it, holds it.
    fn seats_called(&self, name: &str) -> Vec<SeatId> {
        let name = name.to_lowercase();
        self.seating
            .seats
            .iter()
            .filter(|seat| {
                seats::name(seat, &self.names)
                    .to_lowercase()
                    .contains(&name)
            })
            .map(|seat| seat.id.clone())
            .collect()
    }

    /// Each seat's whole name as you read it, the longest first, with its numbered form
    /// before the bare model's.
    fn named_seats(&self) -> Vec<(String, SeatId)> {
        let mut named: Vec<(String, SeatId)> = self
            .seating
            .seats
            .iter()
            .map(|seat: &Seat| (seats::name(seat, &self.names), seat.id.clone()))
            .collect();
        named.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
        named
    }

    /// The writer's "Seat T", "@T" and "@Seat T" held as the seats they name in its map. A bare
    /// "T" is left alone, since it may be code, and so is anything in code.
    fn seats_in(&self, writer: &SeatId, raw: &str) -> Result<Vec<TextPart>, UnknownLetter> {
        let Some(letters) = self.letters.get(writer) else {
            return Ok(vec![TextPart::Words(raw.to_owned())]);
        };
        let mut parts = Vec::new();
        let mut words = String::new();
        for segment in segments(raw) {
            let prose = match segment {
                Segment::Code(code) => {
                    words.push_str(&code);
                    continue;
                }
                Segment::Prose(prose) => prose,
            };
            let mut last = 0;
            for found in SEAT_REFERENCE.captures_iter(&prose) {
                let whole = found.get(0).expect("a match");
                let letter = found
                    .name("letter")
                    .or_else(|| found.name("bare"))
                    .and_then(|letter| letter.as_str().chars().next())
                    .expect("one letter");
                let seat = SeatLetter::new(letter).and_then(|letter| letters.seat(letter));
                let Some(seat) = seat else {
                    return Err(unknown_letter(letter, letters));
                };
                words.push_str(&prose[last..whole.start()]);
                if found.name("at").or_else(|| found.name("bare_at")).is_some() {
                    words.push('@');
                }
                push_seat(&mut parts, &mut words, seat.clone());
                last = whole.end();
            }
            words.push_str(&prose[last..]);
        }
        if !words.is_empty() {
            parts.push(TextPart::Words(words));
        }
        Ok(parts)
    }
}

/// What a seat is asked to do with the turn that reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YourMove {
    /// The floor is the seat's.
    Write,
    /// A short turn with no tools: a stance on each point the last message touches.
    React,
    /// It reads what was said and waits for the floor.
    Wait,
}

impl YourMove {
    fn words(self) -> &'static str {
        match self {
            Self::Write => "write your message",
            Self::React => "react with a stance on each point it touches",
            Self::Wait => "nothing yet",
        }
    }
}

/// A turn as it reaches a seat's session: what the council said since the seat's last turn,
/// a line each as [`Relay::turn_line`] and the others draw them for it, then its move. It
/// carries no times, no token or tool counts and no read receipts, which would say how each
/// seat works.
pub fn turn(lines: &[String], your_move: YourMove) -> String {
    in_use("council/turn", prompt::TURN)
        .replace("{{lines}}", &lines.join("\n"))
        .replace("{{move}}", your_move.words())
}

fn stance_mark(stance: Stance) -> &'static str {
    match stance {
        Stance::Agree => "✔",
        Stance::Object => "✘",
        Stance::Question => "?",
    }
}

/// "Seat T", by the reader's letter for the seat.
fn seat_words(letters: &Letters, seat: &SeatId) -> String {
    match letters.of(seat) {
        Some(letter) => format!("Seat {letter}"),
        None => "a seat".to_owned(),
    }
}

/// Text with each seat it holds as `[seat-002]`, the form quotes are kept and checked in.
pub fn with_markers(parts: &[TextPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            TextPart::Words(words) => words.clone(),
            TextPart::Seat(seat) => held_seat(seat),
        })
        .collect()
}

/// `text` with who wrote it hidden, as an agent reads a plan another agent drafted: names of
/// models, makers and coding tools hidden, a model naming itself and signatures gone, and tool
/// marks in plain words. Its Markdown stays, and so does its code.
pub fn hide_who_wrote(lexicon: &Lexicon, text: &str) -> String {
    strip::strip(lexicon, &[TextPart::Words(text.to_owned())], false).text
}

/// `plan` as every later step reads it, with who wrote it hidden from each sentence by
/// [`hide_who_wrote`]. Its names, paths, interfaces and dependencies stay as written, since
/// later steps match them.
pub fn plan_without_who_wrote(lexicon: &Lexicon, plan: PlanDraft) -> PlanDraft {
    let hide = |text: String| hide_who_wrote(lexicon, &text);
    PlanDraft {
        summary: hide(plan.summary),
        approach: hide(plan.approach),
        pieces: plan
            .pieces
            .into_iter()
            .map(|piece| PlanPiece {
                description: hide(piece.description),
                ..piece
            })
            .collect(),
        choices: plan
            .choices
            .into_iter()
            .map(|choice| PlanChoice {
                topic: hide(choice.topic),
                choice: hide(choice.choice),
                rationale: hide(choice.rationale),
            })
            .collect(),
        risks: plan.risks.into_iter().map(hide).collect(),
        test_strategy: hide(plan.test_strategy),
        new_dependencies: plan.new_dependencies,
    }
}

/// A quote's held seats drawn by `name`, as each reader or your screen names them.
pub fn draw_markers(text: &str, name: impl Fn(&SeatId) -> String) -> String {
    draw_held(text, name)
}

/// Whitespace runs as one space, so a quote across a line break still matches.
fn squeeze(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` with its held seats as seats again, so it strips like a message.
fn held_seats(text: &str) -> Vec<TextPart> {
    held_parts(text)
}

/// The seats the text names with "@", in the order it names them.
pub fn mentions(parts: &[TextPart]) -> Vec<SeatId> {
    let mut named = Vec::new();
    for pair in parts.windows(2) {
        if let [TextPart::Words(words), TextPart::Seat(seat)] = pair
            && words.ends_with('@')
            && !named.contains(seat)
        {
            named.push(seat.clone());
        }
    }
    named
}

fn push_seat(parts: &mut Vec<TextPart>, words: &mut String, seat: SeatId) {
    if !words.is_empty() {
        parts.push(TextPart::Words(std::mem::take(words)));
    }
    parts.push(TextPart::Seat(seat));
}

fn unknown_letter(letter: char, letters: &Letters) -> UnknownLetter {
    let mut others: Vec<String> = letters
        .others()
        .map(|(_, letter)| format!("Seat {letter}"))
        .collect();
    others.sort();
    UnknownLetter {
        letter,
        ask: format!(
            "There is no Seat {letter} in this council. The other seats are {}, and you are \
             Seat {}. Write it again with these letters.",
            and_list(&others),
            letters.own()
        ),
    }
}

/// "Seat T, Seat M and Seat W".
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn in_use(id: &str, built_in: &str) -> String {
    argonauts_core::prompt_files::in_use(id, built_in)
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn next_boundary(text: &str, at: usize) -> usize {
    let mut next = at + 1;
    while next < text.len() && !text.is_char_boundary(next) {
        next += 1;
    }
    next
}

/// Whether `text` opens with `name`, whole, whatever its case.
fn starts_with_name(text: &str, name: &str) -> bool {
    let Some(head) = text.get(..name.len()) else {
        return false;
    };
    head.to_lowercase() == name.to_lowercase()
        && !text[name.len()..].bytes().next().is_some_and(is_word)
}

/// "Seat T", "@Seat T" or "@T", with a letter that stands alone.
static SEAT_REFERENCE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?P<at>@)?\bSeat (?P<letter>[A-Z])\b|(?P<bare_at>@)(?P<bare>[A-Z])\b")
        .expect("a pattern")
});

#[cfg(test)]
mod tests;
