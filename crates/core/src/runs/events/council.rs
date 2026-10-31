//! A council's talk as the engine records it and your screen draws it: who sits, what each seat
//! writes, the stances that agreement is counted from, and how the talk ends. Seats are named by
//! [`SeatId`] here and your screen shows their models; the letters a seat reads the others by live
//! only inside its own session and the key your screen draws over it on asking, so none is ever
//! recorded.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::TaskId;
use crate::base::names::{MessageId, ModelRef, SeatId};
use crate::safety::macos_folders::FolderBlocked;
use crate::workflows::workflow::StepRef;

/// One moment of a council, written by the council's engine as it happens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CouncilEvent {
    /// The talk step whose council it belongs to, since a run can hold more than one council.
    pub talk: StepRef,
    pub kind: CouncilEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CouncilEventKind {
    /// The council sat down, before anyone writes.
    Seated(Seating),
    /// Code gave a seat the floor, and your screen shows its typing bubble with why it writes now
    /// until its message lands or it stops.
    Writing { seat: SeatId, why: FloorReason },
    /// A seat stopped writing with no message, as when `council.when_you_write = "restart"`
    /// stopped it for yours.
    WritingEnded { seat: SeatId },
    /// A message from you or a seat, as it lands.
    Message(Box<CouncilMessage>),
    /// One seat's tapback, cast before it saw the others' so they show together. A later one
    /// from the same seat on the same message replaces it.
    Tapback {
        message: MessageId,
        seat: SeatId,
        stance: Stance,
    },
    /// A seat read a message: one more logo under its "Read by".
    Read { message: MessageId, seat: SeatId },
    /// Every point as it stands now, replacing the last list: the note keeper's list after the
    /// first answers, then again whenever a point's wording, state or backers change.
    Points { points: Vec<Point> },
    /// A seat's latest stance on each point it has taken one on, after a message or a tapback of
    /// its own. Agreement is counted from these, never from the words.
    Stances {
        seat: SeatId,
        stances: Vec<PointStance>,
    },
    /// A seat changed its stance on a point and said what moved it, as one centred line.
    Moved(Moved),
    /// The talk's limits, which stop it when reached: set as it starts, and raised when you let
    /// it talk on.
    Limits { tokens: u64, minutes: u32 },
    /// Every seat agrees on every point.
    Agreed { points: u32 },
    /// The talk stopped without agreement, and the points still open come to you.
    Stopped { why: TalkStop },
    /// A seat's message is held before any seat reads it, for you to choose what happens.
    Held(Box<Held>),
    /// What you chose for a held message.
    Released {
        message: MessageId,
        choice: HeldChoice,
    },
    /// You wrote to one seat in private. What you wrote stays in that seat's session and is
    /// never forwarded; the council's view shows only that it happened.
    Private { seat: SeatId, message: MessageId },
    /// You shared a private message with the council, which now reads it as a message of yours.
    Shared { message: MessageId },
    /// A seat carries on in its own CLI and sits out the talk until it comes back.
    SatOut { seat: SeatId },
    /// It came back, and read the messages it missed first.
    Rejoined { seat: SeatId, missed: u32 },
    /// A seat's turn ran out or failed, so it sits that turn out and the talk goes on.
    Missed(Missed),
    /// macOS kept a seat's CLI out of a folder, said once per run and CLI. `cli` is the CLI's
    /// id, which the screen names.
    FolderBlocked {
        seat: SeatId,
        cli: String,
        blocked: FolderBlocked,
    },
    /// Who checks one task, and which rung of the ladder they came from.
    Check(CheckPick),
    /// What one move of the talk after the note keeper's list cost: its wall time, while some
    /// seat worked, and the seats' billable tokens. The stop guard counts these.
    Worked { ms: u64, tokens: u64 },
    /// What the run review found in the council once the run was over, recorded once.
    RunReview {
        /// How often each seat backed what another seat of its own maker raised, against what
        /// the other makers' seats raised. Empty where that says nothing: it takes two makers
        /// at the table, and two seats from one of them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        lift: Vec<SeatLift>,
        /// Names code hid from the seats, over every message.
        #[serde(default, skip_serializing_if = "is_zero")]
        hidden: u32,
        /// Guesses at who runs a seat that code hid the same way.
        #[serde(default, skip_serializing_if = "is_zero")]
        guesses_hidden: u32,
    },
}

/// The seats in seat order, and the one that keeps the notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seating {
    pub seats: Vec<Seat>,
    /// Lists the points, then writes the plan and who does what. `None` when no seat does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<SeatId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seat {
    pub id: SeatId,
    pub model: ModelRef,
    #[serde(default)]
    pub role: SeatRole,
    /// Its place among the seats of the same model, from 1, which your screen writes as
    /// "Opus 5.5 · 2". `None` when its model sits once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u8>,
}

/// What a seat does in the talk. Keeping the notes is a duty on top of a role, so it lives in
/// [`Seating::notes`].
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SeatRole {
    /// Says what it thinks is right and why, with evidence.
    #[default]
    Proposer,
    /// Argues the other side on purpose, and the other seats are told so. Its stances are its
    /// own view, and they are what counts.
    Skeptic,
    /// Talks like the others, builds only when tasks outnumber the other seats, and checks each
    /// task in a fresh session.
    Checker,
}

/// Who speaks: you, or a seat.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    You,
    Seat(SeatId),
}

/// Why code gave a seat the floor, which the typing bubble says in a few dim words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum FloorReason {
    /// Every seat first answers alone, before it reads anyone else's.
    FirstAnswer,
    /// You replied to its message or named it, so it answers you first.
    You,
    /// The last message replied to its message.
    RepliedTo { by: SeatId },
    /// The last message named it.
    Named { by: SeatId },
    /// It objected to the last message, or asked about it.
    Objected,
    /// No one else had a reason to write, and it had been quiet the longest.
    Quietest,
    /// A point was about to close on a reason no seat can read, or with no evidence named: the
    /// skeptic's turn.
    Unproven { point: u32 },
    /// It keeps the notes: it lists the points, then writes the plan and who does what.
    Notes,
    /// It writes its last message again: without a name code found in it, or without your
    /// private words.
    Rewrite,
}

/// One message to the council. Code keeps it once, in this form, and draws it for each reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CouncilMessage {
    pub id: MessageId,
    pub from: Speaker,
    #[serde(default)]
    pub kind: MessageKind,
    /// The words, with every seat it names kept as a seat, so each reader draws its own names:
    /// your screen a logo and the model, a seat a letter.
    pub text: Vec<TextPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<ReplyTo>,
    /// Seats it names with @, which code asks to answer next.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<SeatId>,
    /// Its stance on each point it touches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stances: Vec<PointStance>,
    /// Model, maker and coding tool names code turned into `[hidden]` before the seats read it,
    /// which your screen counts on the name row.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hidden: u32,
    /// What each hidden name named, in the order code found them, which your screen words its
    /// note by: "a CLI's name hidden from the seats".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_names: Vec<HiddenName>,
    /// Guesses at who runs a seat that code hid the same way, which your screen says in a dim line.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub guesses_hidden: u32,
    /// It still named one after its one rewrite, so the seats read code's rewritten text and
    /// your bubble says "names hidden by code".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rewritten: bool,
    /// It matched a known refusal, so the seats read that it declined to answer while you see
    /// what it wrote.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declined: bool,
    /// A message of yours the seat hadn't read when it began writing this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_before: Option<MessageId>,
}

impl CouncilMessage {
    /// The words with each seat it names written by `name`.
    pub fn words(&self, name: impl Fn(&SeatId) -> String) -> String {
        self.text
            .iter()
            .map(|part| match part {
                TextPart::Words(words) => words.clone(),
                TextPart::Seat(seat) => name(seat),
            })
            .collect()
    }
}

fn is_zero(count: &u32) -> bool {
    *count == 0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Written alone, before the seat read anyone's.
    FirstAnswer,
    #[default]
    Talk,
    /// The note keeper's notes: the points after the first answers, then the plan and who does
    /// what.
    Notes,
}

/// What a name code hid from the seats named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HiddenName {
    Maker,
    Model,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextPart {
    Words(String),
    Seat(SeatId),
}

/// How words that code keeps hold a seat, a point's, a side's or a quote's: `[seat-002]`. Each
/// reader, and your screen, draws the seat in its own words.
pub fn held_seat(seat: &SeatId) -> String {
    format!("[{}]", seat.as_str())
}

static HELD_SEAT: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\[(seat-[0-9]{3})\]").expect("a pattern"));

/// Words kept with their seats held, as the words between and each seat.
pub fn held_parts(text: &str) -> Vec<TextPart> {
    let mut parts = Vec::new();
    let mut words = String::new();
    let mut last = 0;
    for found in HELD_SEAT.captures_iter(text) {
        let whole = found.get(0).expect("a match");
        let Some(seat) = SeatId::parse(&found[1]) else {
            continue;
        };
        words.push_str(&text[last..whole.start()]);
        if !words.is_empty() {
            parts.push(TextPart::Words(std::mem::take(&mut words)));
        }
        parts.push(TextPart::Seat(seat));
        last = whole.end();
    }
    words.push_str(&text[last..]);
    if !words.is_empty() {
        parts.push(TextPart::Words(words));
    }
    parts
}

/// Words kept with their seats held, each seat written by `name`.
pub fn draw_held(text: &str, name: impl Fn(&SeatId) -> String) -> String {
    held_parts(text)
        .into_iter()
        .map(|part| match part {
            TextPart::Words(words) => words,
            TextPart::Seat(seat) => name(&seat),
        })
        .collect()
}

/// The message a reply answers, and the words of it that it quotes, checked word for word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub message: MessageId,
    pub quote: String,
}

/// A tapback, and a stance on a point: ✔ agree, ✘ object, ? a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Stance {
    Agree,
    Object,
    Question,
}

/// A stance on one point, by its number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PointStance {
    pub point: u32,
    pub stance: Stance,
}

/// One point the council has to agree on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    /// From 1, as "Point 4" reads.
    pub number: u32,
    /// Its current wording, which the stances are on.
    pub words: String,
    pub state: PointState,
    /// The sides of a point still open, with the seats that back each, as the note keeper's list
    /// and your question draw them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sides: Vec<Side>,
    /// The seat that gave the point its words now, as a new point or new words in the talk.
    /// The note keeper's list, your points and a side you settle on have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_by: Option<SeatId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    pub words: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backers: Vec<SeatId>,
    /// Why its seats take it, in one sentence, as the note keeper sums it up, with every seat
    /// it names held as `[seat-NNN]`. Empty where the notes give none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PointState {
    /// Not every seat agrees on it.
    #[default]
    Open,
    /// Every seat's latest stance on its current wording is agree.
    Agreed,
    /// One of yours, as a way you named for a task: it binds, a seat can object with a reason,
    /// and only you change it.
    Yours,
    /// The talk stopped and you picked a side.
    Settled,
    /// The talk stopped in Autopilot, and the seats voted.
    Voted,
}

/// A seat's turn that ran out or failed, for the centred line: "Qwen3 Coder 30B didn't react
/// within 2 minutes, so it sits this turn out".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Missed {
    pub seat: SeatId,
    pub turn: MissedTurn,
    /// The turn's time limit in seconds, when it ran past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_secs: Option<u32>,
    /// Its second miss, so it sits out the rest of the talk and the seats still in agree.
    #[serde(default)]
    pub for_good: bool,
}

/// The turn a seat missed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "turn", rename_all = "snake_case")]
pub enum MissedTurn {
    FirstAnswer,
    /// Its reaction to this message, so its stance on the points stays as it was.
    Reaction {
        message: MessageId,
    },
    Message,
    /// The note keeper's plan, which the next seat still in writes once the keeper sat out.
    Plan,
}

/// A changed stance, for the centred line: "5.6 Luna moved to a token bucket".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Moved {
    pub seat: SeatId,
    pub point: u32,
    /// The side it moved to, in the point's words.
    pub to: String,
    pub because: MovedBecause,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "because", rename_all = "snake_case")]
pub enum MovedBecause {
    /// Words of a message, quoted.
    Quote {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        words: String,
    },
    /// Something it read or ran, such as a file.
    Read { what: String },
    /// Something you told it in private. Your words stay private, so the line says only that.
    InPrivate,
}

/// Why a talk stopped without agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum TalkStop {
    /// Its last messages, this many, changed nobody's stance.
    MovedNobody { messages: u32 },
    /// It used its tokens: `used` of `limit`.
    Tokens { used: u64, limit: u64 },
    /// It used its working time, in minutes.
    Time { minutes: u32 },
    /// The note keeper listed no points, twice.
    NoPoints,
    /// You stopped it.
    You,
}

/// A seat's turn that repeats what you wrote to it in private, in its message, a reason, a
/// quote or a point, held before any seat reads any of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Held {
    /// The held message, recorded as a [`CouncilEventKind::Message`] before this.
    pub message: MessageId,
    pub seat: SeatId,
    /// The private message whose words it repeats.
    pub private: MessageId,
    pub why: HeldWhy,
    /// The runs of the held turn's words that match, which your screen marks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matching: Vec<String>,
    /// The rest of the turn: its points, stances and moves, recorded only once the seats may
    /// read the message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turn: Vec<CouncilEventKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum HeldWhy {
    /// It repeats this many of your private words in a row, 6 or more.
    WordsInARow { count: u32 },
    /// It holds a rare word of yours, such as a name, a path or a long number, that appears
    /// nowhere the seats can read.
    RareWord { word: String },
}

impl HeldWhy {
    /// The question's words for it, with the time you wrote the private message where the
    /// reader can tell it: "17 words in a row match what you wrote to it in private at 10:46
    /// AM. The talk goes on without it."
    pub fn question_text(&self, wrote_at: Option<&str>) -> String {
        format!("{}. The talk goes on without it.", self.reason(wrote_at))
    }

    /// As [`HeldWhy::question_text`], for the note keeper's plan, which no task reads until you
    /// choose.
    pub fn plan_question_text(&self, wrote_at: Option<&str>) -> String {
        format!(
            "{}. No task reads the plan until you choose.",
            self.reason(wrote_at)
        )
    }

    fn reason(&self, wrote_at: Option<&str>) -> String {
        let at = wrote_at.map(|at| format!(" at {at}")).unwrap_or_default();
        match self {
            HeldWhy::WordsInARow { count } => {
                format!("{count} words in a row match what you wrote to it in private{at}")
            }
            HeldWhy::RareWord { word } => {
                format!("It holds \"{word}\", a word you wrote only to it in private{at}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldChoice {
    /// The seat gets one more try, told only that it repeated what you wrote to it.
    Rewrite,
    /// Your private message goes to every seat, and binds them like anything you write to the
    /// council.
    Share,
    /// The seats read the message as written, not your private one.
    AsWritten,
    /// No seat ever reads it.
    Drop,
}

/// The checker code picked for a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckPick {
    pub task: TaskId,
    /// The model whose fresh session checks it. `None` when you check it yourself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<ModelRef>,
    pub rung: CheckRung,
}

/// Where a task's checker came from. Every check runs in a fresh session that never reads the
/// talk or learns who made the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRung {
    /// A model of another maker than the task's: a seat's, or another one set up.
    OtherMaker,
    /// A fresh session of the model that made it, as no other maker is set up. Your screen marks
    /// it "Same maker".
    SameMaker,
    /// The checker your settings name.
    Named,
    /// You check it yourself.
    You,
}

/// One seat's backing of what the other seats raised, by whose maker raised it. Its own
/// positions count for neither side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatLift {
    pub seat: SeatId,
    /// Raised by another seat of its maker.
    pub same_maker: Backed,
    /// Raised by a seat of another maker.
    pub other_makers: Backed,
    /// Raised by another seat of its very model, a part of `same_maker`.
    pub same_model: Backed,
}

impl SeatLift {
    /// How much more often it backed its maker's positions than the others', from -1 to 1.
    /// `None` until it had both kinds to back.
    pub fn lift(&self) -> Option<f64> {
        Some(self.same_maker.rate()? - self.other_makers.rate()?)
    }

    /// The same, for positions its own model raised.
    pub fn same_model_lift(&self) -> Option<f64> {
        Some(self.same_model.rate()? - self.other_makers.rate()?)
    }
}

/// How many positions of a kind a seat backed, of how many it could have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backed {
    pub backed: u32,
    pub of: u32,
}

impl Backed {
    pub fn count(&mut self, backed: bool) {
        self.of += 1;
        self.backed += u32::from(backed);
    }

    /// The part backed, from 0 to 1. `None` when there was nothing to back.
    pub fn rate(self) -> Option<f64> {
        (self.of > 0).then(|| f64::from(self.backed) / f64::from(self.of))
    }
}

/// What you do to a council while it talks, sent to its run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CouncilAction {
    /// You write to every seat. A reply to one message, or a seat you name, makes that seat
    /// answer first; each point of yours binds the council.
    Write {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<MessageId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mentions: Vec<SeatId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        points: Vec<String>,
    },
    /// You write to one seat alone. Only that seat reads it.
    Private { seat: SeatId, text: String },
    /// One of your private messages goes to every seat, and binds them.
    Share { message: MessageId },
    /// A seat carries on in its own CLI and sits out the talk meanwhile.
    CarryOn { seat: SeatId },
    /// A seat that sat out comes back, and reads what it missed first. `said` is what you typed
    /// to it in its own CLI meanwhile: your private messages to it, which it read there.
    Rejoin {
        seat: SeatId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        said: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::base::names::WorkflowId;

    fn seat(n: u32) -> SeatId {
        SeatId::numbered(n)
    }

    fn event(kind: CouncilEventKind) -> CouncilEvent {
        CouncilEvent {
            talk: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 1,
                path: vec![0],
            },
            kind,
        }
    }

    #[test]
    fn a_message_keeps_its_seats_as_seats_and_draws_them_for_each_reader() {
        let message = CouncilMessage {
            id: MessageId::numbered(7),
            from: Speaker::Seat(seat(1)),
            kind: MessageKind::Talk,
            text: vec![
                TextPart::Words("With ".into()),
                TextPart::Seat(seat(3)),
                TextPart::Words("'s Lua script a bucket is one call.".into()),
            ],
            reply_to: Some(ReplyTo {
                message: MessageId::numbered(5),
                quote: "A window counter is one INCR".into(),
            }),
            mentions: vec![seat(2)],
            stances: vec![PointStance {
                point: 4,
                stance: Stance::Object,
            }],
            hidden: 1,
            hidden_names: Vec::new(),
            guesses_hidden: 0,
            rewritten: false,
            declined: false,
            started_before: None,
        };
        assert_eq!(
            message.words(|seat| format!("Seat {}", seat.as_str())),
            "With Seat seat-003's Lua script a bucket is one call."
        );
        let written =
            serde_json::to_value(event(CouncilEventKind::Message(Box::new(message.clone()))))
                .unwrap();
        assert_eq!(written["kind"]["event"], "message");
        assert_eq!(written["kind"]["from"], json!({ "seat": "seat-001" }));
        assert_eq!(written["kind"]["text"][1], json!({ "seat": "seat-003" }));
        assert!(written["kind"].get("rewritten").is_none());
        let back: CouncilEvent = serde_json::from_value(written).unwrap();
        assert_eq!(back.kind, CouncilEventKind::Message(Box::new(message)));
    }

    #[test]
    fn what_you_do_to_a_council_reads_back_as_written() {
        let write: CouncilAction = serde_json::from_value(json!({
            "action": "write",
            "text": "Keep /health open.",
        }))
        .unwrap();
        assert_eq!(
            write,
            CouncilAction::Write {
                text: "Keep /health open.".to_owned(),
                reply_to: None,
                mentions: Vec::new(),
                points: Vec::new(),
            }
        );
        for action in [
            CouncilAction::Write {
                text: "Why a bucket?".to_owned(),
                reply_to: Some(MessageId::numbered(4)),
                mentions: vec![seat(2)],
                points: vec!["Build it with Maker-checker".to_owned()],
            },
            CouncilAction::Private {
                seat: seat(3),
                text: "Between us: keep the window.".to_owned(),
            },
            CouncilAction::Share {
                message: MessageId::numbered(9),
            },
            CouncilAction::CarryOn { seat: seat(1) },
            CouncilAction::Rejoin {
                seat: seat(1),
                said: vec!["Keep the window.".to_owned()],
            },
        ] {
            let written = serde_json::to_value(&action).unwrap();
            let back: CouncilAction = serde_json::from_value(written).unwrap();
            assert_eq!(back, action);
        }
    }

    #[test]
    fn a_short_message_reads_with_its_defaults() {
        let written = json!({
            "event": "message",
            "id": "m-001",
            "from": "you",
            "text": [{ "words": "Keep /health open." }],
        });
        let CouncilEventKind::Message(message) = serde_json::from_value(written).unwrap() else {
            panic!("not a message");
        };
        assert_eq!(message.from, Speaker::You);
        assert_eq!(message.kind, MessageKind::Talk);
        assert!(message.mentions.is_empty() && message.stances.is_empty());
        assert_eq!((message.hidden, message.rewritten), (0, false));
    }

    #[test]
    fn every_kind_reads_back_as_written() {
        let kinds = vec![
            CouncilEventKind::Seated(Seating {
                seats: vec![
                    Seat {
                        id: seat(1),
                        model: "claude-code/opus".parse().unwrap(),
                        role: SeatRole::Proposer,
                        number: Some(1),
                    },
                    Seat {
                        id: seat(2),
                        model: "claude-code/opus".parse().unwrap(),
                        role: SeatRole::Skeptic,
                        number: Some(2),
                    },
                ],
                notes: Some(seat(1)),
            }),
            CouncilEventKind::Writing {
                seat: seat(2),
                why: FloorReason::Unproven { point: 6 },
            },
            CouncilEventKind::WritingEnded { seat: seat(2) },
            CouncilEventKind::Tapback {
                message: MessageId::numbered(3),
                seat: seat(1),
                stance: Stance::Question,
            },
            CouncilEventKind::Read {
                message: MessageId::numbered(3),
                seat: seat(2),
            },
            CouncilEventKind::Points {
                points: vec![Point {
                    number: 1,
                    words: "Kept in Redis".into(),
                    state: PointState::Open,
                    sides: vec![
                        Side {
                            words: "in Redis".into(),
                            backers: vec![seat(1)],
                            reason: String::new(),
                        },
                        Side {
                            words: "in memory on each node".into(),
                            backers: vec![seat(2)],
                            reason: String::new(),
                        },
                    ],
                    raised_by: Some(seat(1)),
                }],
            },
            CouncilEventKind::Stances {
                seat: seat(1),
                stances: vec![PointStance {
                    point: 1,
                    stance: Stance::Agree,
                }],
            },
            CouncilEventKind::Moved(Moved {
                seat: seat(2),
                point: 1,
                to: "in Redis".into(),
                because: MovedBecause::Read {
                    what: "src/session/store.ts".into(),
                },
            }),
            CouncilEventKind::Writing {
                seat: seat(2),
                why: FloorReason::Rewrite,
            },
            CouncilEventKind::Limits {
                tokens: 300_000,
                minutes: 15,
            },
            CouncilEventKind::Agreed { points: 6 },
            CouncilEventKind::Stopped {
                why: TalkStop::MovedNobody { messages: 6 },
            },
            CouncilEventKind::Held(Box::new(Held {
                message: MessageId::numbered(9),
                seat: seat(2),
                private: MessageId::numbered(8),
                why: HeldWhy::WordsInARow { count: 17 },
                matching: vec!["their old SDK keeps working until March".into()],
                turn: vec![CouncilEventKind::Stances {
                    seat: seat(2),
                    stances: Vec::new(),
                }],
            })),
            CouncilEventKind::Released {
                message: MessageId::numbered(9),
                choice: HeldChoice::Rewrite,
            },
            CouncilEventKind::Private {
                seat: seat(2),
                message: MessageId::numbered(8),
            },
            CouncilEventKind::Shared {
                message: MessageId::numbered(8),
            },
            CouncilEventKind::SatOut { seat: seat(1) },
            CouncilEventKind::Rejoined {
                seat: seat(1),
                missed: 3,
            },
            CouncilEventKind::Missed(Missed {
                seat: seat(2),
                turn: MissedTurn::Reaction {
                    message: MessageId::numbered(4),
                },
                limit_secs: Some(120),
                for_good: false,
            }),
            CouncilEventKind::Missed(Missed {
                seat: seat(2),
                turn: MissedTurn::Message,
                limit_secs: None,
                for_good: true,
            }),
            CouncilEventKind::Check(CheckPick {
                task: TaskId::numbered(1),
                checker: Some("claude-code/opus".parse().unwrap()),
                rung: CheckRung::SameMaker,
            }),
            CouncilEventKind::RunReview {
                lift: vec![SeatLift {
                    seat: seat(1),
                    same_maker: Backed { backed: 2, of: 2 },
                    other_makers: Backed { backed: 1, of: 4 },
                    same_model: Backed::default(),
                }],
                hidden: 3,
                guesses_hidden: 1,
            },
            CouncilEventKind::RunReview {
                lift: Vec::new(),
                hidden: 0,
                guesses_hidden: 0,
            },
        ];
        for kind in kinds {
            let written = serde_json::to_string(&event(kind.clone())).unwrap();
            let back: CouncilEvent = serde_json::from_str(&written).unwrap();
            assert_eq!(back.kind, kind, "{written}");
        }
    }

    #[test]
    fn a_council_event_sits_in_the_run_log_under_its_own_type() {
        let kind = crate::runs::events::RunEventKind::Council(event(CouncilEventKind::Agreed {
            points: 6,
        }));
        let written = serde_json::to_value(&kind).unwrap();
        assert_eq!(written["type"], "council");
        assert_eq!(written["data"]["kind"]["event"], "agreed");
        assert_eq!(written["data"]["talk"]["workflow"], "wf-001");
        let back: crate::runs::events::RunEventKind = serde_json::from_value(written).unwrap();
        assert_eq!(back, kind);
    }

    #[test]
    fn a_held_messages_question_says_when_you_wrote_where_the_reader_can_tell() {
        let why = HeldWhy::WordsInARow { count: 17 };
        assert_eq!(
            why.question_text(Some("10:46 AM")),
            "17 words in a row match what you wrote to it in private at 10:46 AM. The talk goes \
             on without it."
        );
        assert_eq!(
            why.question_text(None),
            "17 words in a row match what you wrote to it in private. The talk goes on without it."
        );
        let rare = HeldWhy::RareWord {
            word: "Acme".into(),
        };
        assert_eq!(
            rare.question_text(None),
            "It holds \"Acme\", a word you wrote only to it in private. The talk goes on without it."
        );
    }

    #[test]
    fn a_seat_with_no_role_is_a_proposer() {
        let seat: Seat =
            serde_json::from_value(json!({ "id": "seat-001", "model": "codex/gpt-5.6-luna" }))
                .unwrap();
        assert_eq!((seat.role, seat.number), (SeatRole::Proposer, None));
    }

    #[test]
    fn kept_words_hold_their_seats_until_a_reader_names_them() {
        let kept = format!(
            "As {} asked, and {} too",
            held_seat(&seat(2)),
            held_seat(&seat(1))
        );
        assert_eq!(kept, "As [seat-002] asked, and [seat-001] too");
        assert_eq!(
            held_parts(&kept),
            [
                TextPart::Words("As ".into()),
                TextPart::Seat(seat(2)),
                TextPart::Words(" asked, and ".into()),
                TextPart::Seat(seat(1)),
                TextPart::Words(" too".into()),
            ]
        );
        let named = draw_held(&kept, |held| format!("Seat {}", held.as_str()));
        assert_eq!(named, "As Seat seat-002 asked, and Seat seat-001 too");
        assert_eq!(
            draw_held("[seat-x] stays", |_| String::new()),
            "[seat-x] stays"
        );
    }
}
