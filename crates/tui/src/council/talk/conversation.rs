//! One council's talk as your screen keeps it, built from the engine's council events: who sits,
//! every message with its tapbacks and readers, the points and stances agreement is counted
//! from, and who writes now. Each event says what to print: the items it adds to the
//! conversation, and which message it changed while that message still shows live.

use std::collections::HashMap;

use argonauts_core::base::names::{MessageId, SeatId};
use argonauts_core::runs::events::council::{
    CouncilEventKind, CouncilMessage, FloorReason, HeldWhy, HiddenName, MessageKind, MovedBecause,
    Point, PointState, Seating, Speaker, TalkStop, TextPart, draw_held, held_parts,
};

use super::super::screens::reply::ReplyBar;
use super::super::screens::thread::Thread;
use super::faces::Faces;
use super::item::{Body, Header, Inset, Item, Quote, SeatSays, YouSay};
use super::words::{Mark, Piece, Tone, paragraphs};

/// When an event happened, as the conversation writes it: "Today" and "10:41 AM".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct When {
    pub day: String,
    pub time: String,
}

/// What one event asks of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Said {
    /// Items to add, in order. A message stays live, so its tapbacks and readers still show on
    /// it, until the next item lands.
    pub items: Vec<Item>,
    /// The message whose item changed, while it may still be live.
    pub changed: Option<MessageId>,
}

impl Said {
    fn item(item: Item) -> Self {
        Self {
            items: vec![item],
            changed: None,
        }
    }

    fn changed(message: &MessageId) -> Self {
        Self {
            items: Vec::new(),
            changed: Some(message.clone()),
        }
    }
}

/// A message as kept: what it said, and what has happened to it since.
#[derive(Debug, Clone)]
struct Kept {
    message: CouncilMessage,
    taps: Vec<(SeatId, Mark)>,
    read_by: Vec<SeatId>,
    /// The first message of its thread: itself, unless it replies to a talk message.
    root: Option<MessageId>,
    held: Option<Vec<String>>,
    /// What the seat read and ran while it wrote a first answer.
    did: Option<String>,
    /// The note keeper's proposal, inside the notes that list the points.
    proposal: Option<Inset>,
}

#[derive(Debug, Clone)]
pub struct Conversation {
    pub faces: Faces,
    pub notes: Option<SeatId>,
    kept: Vec<Kept>,
    pub points: Vec<Point>,
    /// Who writes now and why, in the order they began.
    pub writing: Vec<(SeatId, FloorReason)>,
    /// Each seat's latest stance on each point.
    stances: HashMap<SeatId, Vec<(u32, Mark)>>,
    /// The talk's token limit, for the working line.
    pub token_limit: Option<u64>,
    said_alone_first: bool,
    /// Every seat's first answer is in, and a line said so.
    said_all_answered: bool,
    /// Files each seat read and shell commands it ran since its last message.
    tools: HashMap<SeatId, (u32, u32)>,
    /// What you wrote to one seat alone, oldest first, and whether you shared it since.
    private: Vec<Private>,
    /// How the talk ended, until it talks on.
    ended: Option<Ended>,
    /// The note keeper's first list has shown as its proposal.
    proposed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Agreed,
    Stopped,
}

/// One of your private messages, as its seat's session view picks it to share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Private {
    pub seat: SeatId,
    pub message: MessageId,
    pub shared: bool,
}

/// A tool call as a first answer's name row counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolUse {
    Read,
    Shell,
    Other,
}

impl ToolUse {
    /// The kind of a tool by its name, as the CLIs name their file reads and shell commands.
    pub fn of(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "read" | "read_file" | "view" | "readfile" => ToolUse::Read,
            "bash" | "shell" | "local_shell" | "exec_command" | "run_shell_command" => {
                ToolUse::Shell
            }
            _ => ToolUse::Other,
        }
    }
}

/// "Read 5 files · ran 2 shell commands", or `None` for neither.
fn did_words((reads, shells): (u32, u32)) -> Option<String> {
    let plural = |count: u32| if count == 1 { "" } else { "s" };
    let read = (reads > 0).then(|| format!("Read {reads} file{}", plural(reads)));
    let ran = (shells > 0).then(|| format!("ran {shells} shell command{}", plural(shells)));
    match (read, ran) {
        (Some(read), Some(ran)) => Some(format!("{read} · {ran}")),
        (Some(read), None) => Some(read),
        (None, Some(ran)) => {
            let mut ran = ran;
            ran.replace_range(..1, "R");
            Some(ran)
        }
        (None, None) => None,
    }
}

impl Conversation {
    pub fn new(seating: &Seating, faces: Faces) -> Self {
        Self {
            faces,
            notes: seating.notes.clone(),
            kept: Vec::new(),
            points: Vec::new(),
            writing: Vec::new(),
            stances: HashMap::new(),
            token_limit: None,
            said_alone_first: false,
            said_all_answered: false,
            tools: HashMap::new(),
            private: Vec::new(),
            ended: None,
            proposed: false,
        }
    }

    /// Whether the council still talks, so what you write goes to its seats: from when it
    /// sits down until it agrees or stops. Limits set again after a stop let it talk on, and
    /// so does a message of yours to every seat, as when you send its plan back.
    pub fn talking(&self) -> bool {
        self.ended.is_none()
    }

    /// Your private messages to `seat`, oldest first.
    pub fn private_to<'a>(&'a self, seat: &'a SeatId) -> impl Iterator<Item = &'a Private> {
        self.private
            .iter()
            .filter(move |private| private.seat == *seat)
    }

    /// Whether `message` is one you wrote to a seat in private.
    pub fn is_private(&self, message: &MessageId) -> bool {
        self.private
            .iter()
            .any(|private| private.message == *message)
    }

    /// A seat called a tool, counted on its next first answer's name row.
    pub fn tool_used(&mut self, seat: &SeatId, tool: ToolUse) {
        let counts = self.tools.entry(seat.clone()).or_default();
        match tool {
            ToolUse::Read => counts.0 += 1,
            ToolUse::Shell => counts.1 += 1,
            ToolUse::Other => {}
        }
    }

    /// The council's header, as it sits down.
    pub fn header(&self) -> Item {
        let count = self.faces.len();
        let mut subtitle = format!(
            "{count} seat{} · each one is its own session",
            if count == 1 { "" } else { "s" }
        );
        if let Some(notes) = &self.notes {
            subtitle.push_str(&format!(
                " · {} keeps the notes",
                self.faces.face(notes).label()
            ));
        }
        Item::Header(Header {
            seats: self.faces.ids().cloned().collect(),
            title: "Council".to_owned(),
            subtitle,
        })
    }

    /// What `kind` adds, at `at` on the wall clock ("Today 10:41 AM", "10:46 AM"), when known.
    pub fn on(&mut self, kind: &CouncilEventKind, at: Option<&When>) -> Said {
        match kind {
            CouncilEventKind::Seated(_) => {
                let mut said = Said::item(self.header());
                if let Some(at) = at {
                    said.items.push(Item::Line(vec![Piece::dim(format!(
                        "{} {}",
                        at.day, at.time
                    ))]));
                }
                said
            }
            CouncilEventKind::Writing { seat, why } => {
                self.writing.retain(|(writer, _)| writer != seat);
                self.writing.push((seat.clone(), why.clone()));
                if *why == FloorReason::FirstAnswer && !self.said_alone_first {
                    self.said_alone_first = true;
                    return Said::item(Item::Line(vec![Piece::dim(
                        "Each seat answers alone first, without seeing the others",
                    )]));
                }
                Said::default()
            }
            CouncilEventKind::WritingEnded { seat } => {
                self.writing.retain(|(writer, _)| writer != seat);
                Said::default()
            }
            CouncilEventKind::Message(message) => self.arrived(message),
            CouncilEventKind::Tapback {
                message,
                seat,
                stance,
            } => match self.find_mut(message) {
                Some(kept) => {
                    kept.taps.retain(|(caster, _)| caster != seat);
                    kept.taps.push((seat.clone(), Mark::from(*stance)));
                    Said::changed(message)
                }
                None => Said::default(),
            },
            CouncilEventKind::Read { message, seat } => match self.find_mut(message) {
                Some(kept) if !kept.read_by.contains(seat) => {
                    kept.read_by.push(seat.clone());
                    Said::changed(message)
                }
                _ => Said::default(),
            },
            CouncilEventKind::Points { points } => {
                self.points = points.clone();
                self.proposal(points)
            }
            CouncilEventKind::Stances { seat, stances } => {
                let kept = self.stances.entry(seat.clone()).or_default();
                for stance in stances {
                    kept.retain(|(point, _)| *point != stance.point);
                    kept.push((stance.point, Mark::from(stance.stance)));
                }
                Said::default()
            }
            CouncilEventKind::Moved(moved) => {
                let to = self.drawn(&moved.to);
                let head = format!(" moved to {to}");
                let mut line = vec![Piece::Called(moved.seat.clone(), Tone::Dim)];
                match &moved.because {
                    MovedBecause::Quote { words, .. } => {
                        let words = self.drawn(words);
                        line.push(Piece::dim(format!("{head}: ")));
                        line.push(Piece::tone(format!("\"{words}\""), Tone::Quote));
                    }
                    MovedBecause::Read { what } => {
                        line.push(Piece::dim(format!("{head} after reading ")));
                        line.push(Piece::tone(what.clone(), Tone::Quote));
                    }
                    MovedBecause::InPrivate => {
                        line.push(Piece::dim(format!(
                            "{head} · its reason came from you, in private"
                        )));
                    }
                }
                line.push(Piece::dim(format!(" · {}", self.points_agreed())));
                Said::item(Item::Line(line))
            }
            CouncilEventKind::Limits { tokens, .. } => {
                self.token_limit = Some(*tokens);
                if self.ended == Some(Ended::Stopped) {
                    self.ended = None;
                }
                Said::default()
            }
            CouncilEventKind::Agreed { points } => {
                self.writing.clear();
                self.ended = Some(Ended::Agreed);
                let mut line = vec![
                    Piece::tone("✔  ", Tone::Ok),
                    Piece::tone("Everyone agreed", Tone::OkBold),
                    Piece::dim(format!(" · {points} of {points} points  ")),
                ];
                for (i, seat) in self.faces.ids().enumerate() {
                    if i > 0 {
                        line.push(Piece::dim(" "));
                    }
                    line.push(Piece::Badge(seat.clone()));
                }
                Said::item(Item::Line(line))
            }
            CouncilEventKind::Stopped { why } => {
                self.writing.clear();
                self.ended = Some(Ended::Stopped);
                let because = match why {
                    TalkStop::MovedNobody { messages } => {
                        format!("its last {messages} messages moved nobody")
                    }
                    TalkStop::Tokens { limit, .. } => {
                        format!("it used its {} tokens", crate::base::format::tokens(*limit))
                    }
                    TalkStop::Time { minutes } => format!("it used its {minutes} minutes"),
                    TalkStop::NoPoints => "the note keeper listed no points".to_owned(),
                    TalkStop::You => "you stopped it".to_owned(),
                };
                Said::item(Item::Line(vec![Piece::dim(format!(
                    "The talk stopped: {because} · {}",
                    self.points_agreed()
                ))]))
            }
            CouncilEventKind::Held(held) => {
                let matching = held.matching.clone();
                let matching = if matching.is_empty() {
                    match &held.why {
                        HeldWhy::RareWord { word } => vec![word.clone()],
                        HeldWhy::WordsInARow { .. } => Vec::new(),
                    }
                } else {
                    matching
                };
                match self.find_mut(&held.message) {
                    Some(kept) => {
                        kept.held = Some(matching);
                        Said::changed(&held.message)
                    }
                    None => Said::default(),
                }
            }
            CouncilEventKind::Released { message, .. } => match self.find_mut(message) {
                Some(kept) => {
                    kept.held = None;
                    Said::changed(message)
                }
                None => Said::default(),
            },
            CouncilEventKind::Private { seat, message } => {
                self.private.push(Private {
                    seat: seat.clone(),
                    message: message.clone(),
                    shared: false,
                });
                Said::item(Item::Line(vec![
                    Piece::dim("You wrote to "),
                    Piece::Called(seat.clone(), Tone::Dim),
                    Piece::dim(format!(
                        " in private{} · no other seat sees it",
                        at.map(|at| format!(" · {}", at.time)).unwrap_or_default()
                    )),
                ]))
            }
            CouncilEventKind::Shared { message } => {
                for private in &mut self.private {
                    private.shared |= private.message == *message;
                }
                Said::item(Item::Line(vec![Piece::dim(
                    "You shared what you wrote in private with every seat",
                )]))
            }
            CouncilEventKind::SatOut { seat } => {
                self.writing.retain(|(writer, _)| writer != seat);
                Said::item(Item::Line(vec![
                    Piece::Called(seat.clone(), Tone::Dim),
                    Piece::dim(" carries on in its own CLI and sits out the talk"),
                ]))
            }
            CouncilEventKind::Rejoined { seat, missed } => Said::item(Item::Line(vec![
                Piece::Called(seat.clone(), Tone::Dim),
                Piece::dim(format!(
                    " is back and read the {missed} message{} it missed",
                    if *missed == 1 { "" } else { "s" }
                )),
            ])),
            // The run review tells you its findings in lines of its own.
            CouncilEventKind::Check(_)
            | CouncilEventKind::Worked { .. }
            | CouncilEventKind::RunReview { .. } => Said::default(),
        }
    }

    fn arrived(&mut self, message: &CouncilMessage) -> Said {
        // Words you wrote to one seat alone land right after their `Private`, and stay in
        // that seat's session: the talk shows only that you wrote.
        if self.is_private(&message.id) {
            return Said::default();
        }
        if message.from == Speaker::You {
            self.ended = None;
        }
        let mut did = None;
        if let Speaker::Seat(seat) = &message.from {
            self.writing.retain(|(writer, _)| writer != seat);
            let counts = self.tools.remove(seat).unwrap_or_default();
            if message.kind == MessageKind::FirstAnswer {
                did = did_words(counts);
            }
        }
        let root = message.reply_to.as_ref().and_then(|reply| {
            let answered = self.find(&reply.message)?;
            match answered.message.kind {
                MessageKind::FirstAnswer => None,
                _ => Some(answered.root.clone().unwrap_or(reply.message.clone())),
            }
        });
        self.kept.push(Kept {
            message: message.clone(),
            taps: Vec::new(),
            read_by: Vec::new(),
            root,
            held: None,
            did,
            proposal: None,
        });
        let item = self.item(&message.id).expect("the message just kept");
        let mut said = Said::item(item);
        if message.kind == MessageKind::FirstAnswer
            && let Some(line) = self.all_answered()
        {
            said.items.push(line);
        }
        said
    }

    /// Once the last seat's first answer is in, the line that says so and who gathers the
    /// points: "Everyone answered · Opus 5.5 keeps the notes and gathers the points".
    fn all_answered(&mut self) -> Option<Item> {
        let answered = |seat: &SeatId| {
            self.kept.iter().any(|kept| {
                kept.message.kind == MessageKind::FirstAnswer
                    && kept.message.from == Speaker::Seat(seat.clone())
            })
        };
        if self.said_all_answered || self.faces.is_empty() || !self.faces.ids().all(answered) {
            return None;
        }
        self.said_all_answered = true;
        let words = match &self.notes {
            Some(notes) => format!(
                "Everyone answered · {} keeps the notes and gathers the points",
                self.faces.face(notes).label()
            ),
            None => "Everyone answered".to_owned(),
        };
        Some(Item::Line(vec![Piece::dim(words)]))
    }

    /// The note keeper's first list, as its proposal inside the notes it came with: the points
    /// the answers agree on, each open one with its sides and the seats that took them, and
    /// your ways, which bind. Then a line on how far they agree.
    fn proposal(&mut self, points: &[Point]) -> Said {
        let after_notes = self
            .kept
            .last()
            .is_some_and(|kept| kept.message.kind == MessageKind::Notes);
        let listed: Vec<&Point> = points
            .iter()
            .filter(|point| point.state != PointState::Yours)
            .collect();
        if self.proposed || !after_notes || self.ended.is_some() || listed.is_empty() {
            return Said::default();
        }
        self.proposed = true;
        let yours: Vec<Vec<Piece>> = points
            .iter()
            .filter(|point| point.state == PointState::Yours)
            .map(|point| self.kept_words(&point.words))
            .collect();
        let answers = self
            .kept
            .iter()
            .filter(|kept| kept.message.kind == MessageKind::FirstAnswer)
            .count();
        let mut head = format!(
            " · {} from {}",
            counted(listed.len(), "point"),
            counted(answers, "answer")
        );
        if !yours.is_empty() {
            head.push_str(&format!(" · {} from you", counted(yours.len(), "way")));
        }
        let mut rows = vec![vec![Piece::tone("Proposal", Tone::Bold), Piece::dim(head)]];
        for point in &listed {
            let mut row = Vec::new();
            if point.sides.is_empty() {
                row.push(Piece::tone("✔  ", Tone::Ok));
                row.extend(self.kept_words(&point.words));
            } else {
                row.push(Piece::tone("?  ", Tone::WarnBold));
                for (i, side) in point.sides.iter().enumerate() {
                    if i > 0 {
                        row.push(Piece::dim("   or   "));
                    }
                    row.extend(self.kept_words(&side.words));
                    for seat in &side.backers {
                        row.push(Piece::dim(" "));
                        row.push(Piece::Badge(seat.clone()));
                    }
                }
            }
            rows.push(row);
        }
        let agreed = listed.iter().filter(|point| point.sides.is_empty()).count();
        let Some(kept) = self.kept.last_mut() else {
            return Said::default();
        };
        kept.proposal = Some(Inset::Proposal { rows, yours });
        let mut said = Said::changed(&kept.message.id);
        said.items.push(Item::Line(vec![Piece::dim(format!(
            "{agreed} of {} points agreed · they talk until every seat agrees",
            listed.len()
        ))]));
        said
    }

    /// Words code keeps with seats held in them, as pieces with each seat drawn as itself.
    fn kept_words(&self, words: &str) -> Vec<Piece> {
        held_parts(words)
            .into_iter()
            .map(|part| match part {
                TextPart::Words(words) => Piece::text(words),
                TextPart::Seat(seat) => Piece::Seat(seat),
            })
            .collect()
    }

    fn find(&self, id: &MessageId) -> Option<&Kept> {
        self.kept.iter().find(|kept| kept.message.id == *id)
    }

    fn find_mut(&mut self, id: &MessageId) -> Option<&mut Kept> {
        self.kept.iter_mut().find(|kept| kept.message.id == *id)
    }

    /// How many replies the thread rooted at `root` has, up to and including `upto`.
    fn thread_count(&self, root: &MessageId, upto: &MessageId) -> u32 {
        let mut count = 0;
        for kept in &self.kept {
            if kept.root.as_ref() == Some(root) {
                count += 1;
            }
            if kept.message.id == *upto {
                break;
            }
        }
        count
    }

    /// The message's item as it stands now.
    pub fn item(&self, id: &MessageId) -> Option<Item> {
        let kept = self.find(id)?;
        let message = &kept.message;
        let words = paragraphs(&message.text, Tone::Text);
        let reply = message.reply_to.as_ref().map(|reply| {
            let seat =
                self.find(&reply.message)
                    .and_then(|answered| match &answered.message.from {
                        Speaker::Seat(seat) => Some(seat.clone()),
                        Speaker::You => None,
                    });
            let replies = kept
                .root
                .as_ref()
                .map_or(0, |root| self.thread_count(root, id));
            Quote {
                seat,
                words: self.drawn(&reply.quote),
                replies,
            }
        });
        Some(match &message.from {
            Speaker::You => Item::You(YouSay {
                paragraphs: words,
                reply,
                read_by: kept.read_by.clone(),
                read_after: Vec::new(),
                picked: false,
            }),
            Speaker::Seat(seat) => {
                let (paragraphs, held) = match &kept.held {
                    Some(matching) => (
                        vec![marked(
                            &message.words(|seat| self.faces.face(seat).label()),
                            matching,
                        )],
                        true,
                    ),
                    None => (words, false),
                };
                let mut says = SeatSays::new(
                    seat.clone(),
                    Body::Words {
                        paragraphs,
                        shown: (message.kind == MessageKind::FirstAnswer).then_some(3),
                    },
                );
                says.reply = reply;
                says.inset = kept.proposal.clone();
                says.right = kept.did.clone();
                says.taps = kept.taps.clone();
                says.held = held;
                says.note = self.note(message, seat, held);
                if !held {
                    for note in hidden_notes(message) {
                        says.note = Some(says.note.map_or(note.clone(), |before| before + &note));
                    }
                }
                Item::Seat(vec![says])
            }
        })
    }

    fn note(&self, message: &CouncilMessage, seat: &SeatId, held: bool) -> Option<String> {
        if held {
            return Some(" · held back: no seat has read it".to_owned());
        }
        if message.started_before.is_some() {
            return Some(" · started before it read yours".to_owned());
        }
        if message.kind == MessageKind::Notes && self.notes.as_ref() == Some(seat) {
            return Some(" · keeps the notes".to_owned());
        }
        None
    }

    /// Every message you can pick, newest first.
    pub fn newest_first(&self) -> Vec<MessageId> {
        self.kept
            .iter()
            .rev()
            .map(|kept| kept.message.id.clone())
            .collect()
    }

    /// What replying to a message names above the input: whose it is, and its last sentence,
    /// the claim a reply most often answers.
    pub fn reply_bar(&self, id: &MessageId) -> Option<ReplyBar> {
        let kept = self.find(id)?;
        let words = kept.message.words(|seat| self.faces.face(seat).label());
        Some(ReplyBar {
            message: id.clone(),
            face: match &kept.message.from {
                Speaker::Seat(seat) => Some(self.faces.face(seat)),
                Speaker::You => None,
            },
            quote: argonauts_core::base::text::last_sentence(&words).to_owned(),
        })
    }

    /// The seat that wrote message `id`; `None` for one of yours.
    pub fn seat_of(&self, id: &MessageId) -> Option<SeatId> {
        match &self.find(id)?.message.from {
            Speaker::Seat(seat) => Some(seat.clone()),
            Speaker::You => None,
        }
    }

    /// The first message of the thread `id` is in: its own when it starts one.
    pub fn thread_root(&self, id: &MessageId) -> Option<MessageId> {
        let kept = self.find(id)?;
        Some(kept.root.clone().unwrap_or_else(|| id.clone()))
    }

    /// A message and its thread's replies, as the thread view shows them: a reply to the first
    /// message drops its quote, a reply to a reply keeps it, and no count shows.
    pub fn thread(&self, root: &MessageId) -> Option<Thread> {
        let first = self.find(root)?;
        let mut items = vec![self.item(root)?];
        let mut replies = 0;
        for kept in self
            .kept
            .iter()
            .filter(|kept| kept.root.as_ref() == Some(root))
        {
            replies += 1;
            let Some(mut item) = self.item(&kept.message.id) else {
                continue;
            };
            let answers_root = kept
                .message
                .reply_to
                .as_ref()
                .is_some_and(|reply| reply.message == *root);
            let quote = match &mut item {
                Item::Seat(run) => run.first_mut().map(|says| &mut says.reply),
                Item::You(you) => Some(&mut you.reply),
                _ => None,
            };
            if let Some(quote) = quote {
                if answers_root {
                    *quote = None;
                } else if let Some(quote) = quote {
                    quote.replies = 0;
                }
            }
            items.push(item);
        }
        let starter = match &first.message.from {
            Speaker::Seat(seat) => Some(seat.clone()),
            Speaker::You => None,
        };
        Some(Thread {
            starter,
            replies,
            items,
        })
    }

    /// "2/4 seats agree", for the working line.
    pub fn agree_note(&self) -> String {
        let (agreeing, seats) = self.seats_agreeing();
        format!("{agreeing}/{seats} seats agree")
    }

    /// "4 of 6 points agreed": your own points bind and aren't counted.
    pub fn points_agreed(&self) -> String {
        let counted: Vec<&Point> = self
            .points
            .iter()
            .filter(|point| point.state != PointState::Yours)
            .collect();
        let agreed = counted
            .iter()
            .filter(|point| {
                matches!(
                    point.state,
                    PointState::Agreed | PointState::Settled | PointState::Voted
                )
            })
            .count();
        format!("{agreed} of {} points agreed", counted.len())
    }

    /// Seats that back every counted point, and all seats, for "2/4 seats agree".
    pub fn seats_agreeing(&self) -> (usize, usize) {
        let counted: Vec<u32> = self
            .points
            .iter()
            .filter(|point| point.state != PointState::Yours)
            .map(|point| point.number)
            .collect();
        let agreeing = self
            .faces
            .ids()
            .filter(|seat| {
                !counted.is_empty()
                    && counted.iter().all(|number| {
                        self.stances
                            .get(*seat)
                            .is_some_and(|stances| stances.contains(&(*number, Mark::Agree)))
                    })
            })
            .count();
        (agreeing, self.faces.len())
    }

    /// The points still open, each by its sides ("Redis or memory") or else by its words.
    pub fn still_open(&self) -> Vec<String> {
        self.points
            .iter()
            .filter(|point| point.state == PointState::Open)
            .map(|point| match point.sides.as_slice() {
                [] | [_] => self.drawn(&point.words),
                sides => sides
                    .iter()
                    .map(|side| self.drawn(&side.words))
                    .collect::<Vec<_>>()
                    .join(" or "),
            })
            .collect()
    }

    /// Words code keeps with their seats held, a point's, a side's or a quote's, with each
    /// seat by its name here.
    fn drawn(&self, words: &str) -> String {
        draw_held(words, |seat| self.faces.face(seat).label())
    }

    /// "Still open: Redis or memory · calls with no key", under the working line while the
    /// seats talk; `None` once nothing is open.
    pub fn still_open_line(&self) -> Option<String> {
        let open = self.still_open();
        (!open.is_empty()).then(|| format!("Still open: {}", open.join(" · ")))
    }

    /// Who writes now, and why in a few dim words: who it answers, or why code gave it the
    /// floor. Each seat is named as its badge names it.
    pub fn typing(&self) -> (Vec<SeatId>, Vec<Piece>) {
        let writers: Vec<SeatId> = self.writing.iter().map(|(seat, _)| seat.clone()).collect();
        let name = |seat: &SeatId| Piece::Name(seat.clone(), Tone::Dim);
        let reason = match self.writing.as_slice() {
            [] => Vec::new(),
            [(seat, why)] => match self.why(seat, why) {
                Some(mut why) => {
                    why.insert(0, name(seat));
                    why
                }
                None => Vec::new(),
            },
            many => {
                // You came in while one was mid-message: it finishes first.
                let you = many.iter().find(|(_, why)| *why == FloorReason::You);
                let finishing = many.iter().find(|(_, why)| *why != FloorReason::You);
                match (you, finishing) {
                    (Some((answering, _)), Some((first, _))) => vec![
                        name(answering),
                        Piece::dim(" is answering you · "),
                        name(first),
                        Piece::dim(" finishes first, then reads yours"),
                    ],
                    _ => Vec::new(),
                }
            }
        };
        (writers, reason)
    }

    /// Why `seat` writes, in the words after its name.
    fn why(&self, seat: &SeatId, why: &FloorReason) -> Option<Vec<Piece>> {
        let words = |words: &str| Some(vec![Piece::dim(format!(" {words}"))]);
        let by = |by: &SeatId, after: &str| {
            Some(vec![
                Piece::dim(" is writing · "),
                Piece::Name(by.clone(), Tone::Dim),
                Piece::dim(format!(" {after}")),
            ])
        };
        match why {
            FloorReason::FirstAnswer => None,
            FloorReason::You => words("is answering you"),
            FloorReason::RepliedTo { by: replier } => by(replier, "replied to its message"),
            FloorReason::Named { by: asker } => by(asker, "asked it"),
            FloorReason::Objected => words("is writing · it objected"),
            FloorReason::Quietest => words("is writing · it has been quiet the longest"),
            FloorReason::Unproven { point } => words(&format!(
                "is writing · point {point} would close on a reason no seat can read"
            )),
            FloorReason::Notes => {
                let agreed = self
                    .points
                    .iter()
                    .all(|point| point.state != PointState::Open);
                (agreed && !self.points.is_empty() && self.notes.as_ref() == Some(seat)).then(
                    || {
                        vec![Piece::dim(
                            " keeps the notes: it writes the plan and who does what",
                        )]
                    },
                )
            }
            FloorReason::Rewrite => words("writes its message again"),
        }
    }
}

/// The last sentence of `words`: after the last ". " that isn't at its end.
/// "6 points", "1 answer".
fn counted(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

/// Words of a held message, faded, with the runs that match what you wrote in private marked.
fn marked(text: &str, matching: &[String]) -> Vec<Piece> {
    let words: Vec<&str> = text.split(' ').collect();
    let mut lit = vec![false; words.len()];
    for run in matching {
        let run: Vec<&str> = run.split(' ').filter(|word| !word.is_empty()).collect();
        if run.is_empty() {
            continue;
        }
        for start in 0..words.len().saturating_sub(run.len() - 1) {
            if words[start..start + run.len()] == run[..] {
                lit[start..start + run.len()].fill(true);
            }
        }
    }
    let mut pieces = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if i > 0 {
            let tone = if lit[i] && lit[i - 1] {
                Tone::Matching
            } else {
                Tone::Faded
            };
            pieces.push(Piece::tone(" ", tone));
        }
        let tone = if lit[i] { Tone::Matching } else { Tone::Faded };
        pieces.push(Piece::tone(*word, tone));
    }
    pieces
}

/// What code hid from the seats in a seat's message, as notes on its name row: the names it
/// hid, by what they named, or that code hid them itself where the message still named one after
/// its one rewrite; and the guesses it hid at who runs a seat.
fn hidden_notes(message: &CouncilMessage) -> Vec<String> {
    let mut notes = Vec::new();
    if message.rewritten {
        notes.push(" · names hidden by code".to_owned());
    } else if message.hidden > 0 {
        let names = match (message.hidden, message.hidden_names.as_slice()) {
            (1, [HiddenName::Cli]) => "a CLI's name",
            (1, [HiddenName::Model]) => "a model's name",
            (1, [HiddenName::Maker]) => "a maker's name",
            (1, _) => "a name",
            _ => "names",
        };
        notes.push(format!(" · {names} hidden from the seats"));
    }
    match message.guesses_hidden {
        0 => {}
        1 => notes.push(" · a guess hidden from the seats".to_owned()),
        many => notes.push(format!(" · {many} guesses hidden from the seats")),
    }
    notes
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::events::council::{
        Moved, PointState, ReplyTo, Stance, TextPart, held_seat,
    };

    use super::*;
    use crate::council::tests::story::{self, flash, luna, opus, qwen};

    fn id(n: u32) -> MessageId {
        MessageId::numbered(n)
    }

    fn message(n: u32, from: Speaker, kind: MessageKind, words: &str) -> CouncilMessage {
        CouncilMessage {
            id: id(n),
            from,
            kind,
            text: vec![TextPart::Words(words.to_owned())],
            reply_to: None,
            mentions: Vec::new(),
            stances: Vec::new(),
            hidden: 0,
            hidden_names: Vec::new(),
            guesses_hidden: 0,
            rewritten: false,
            declined: false,
            started_before: None,
        }
    }

    fn replying(mut message: CouncilMessage, to: u32, quote: &str) -> CouncilMessage {
        message.reply_to = Some(ReplyTo {
            message: id(to),
            quote: quote.to_owned(),
        });
        message
    }

    fn council() -> Conversation {
        let seating = Seating {
            seats: Vec::new(),
            notes: Some(opus()),
        };
        Conversation::new(&seating, story::faces())
    }

    impl Conversation {
        fn on_now(&mut self, kind: &CouncilEventKind) -> Said {
            self.on(kind, None)
        }
    }

    fn land(council: &mut Conversation, message: CouncilMessage) {
        council.on_now(&CouncilEventKind::Message(Box::new(message)));
    }

    fn tap(council: &mut Conversation, n: u32, seat: SeatId, stance: Stance) {
        council.on_now(&CouncilEventKind::Tapback {
            message: id(n),
            seat,
            stance,
        });
    }

    fn seat(seat: SeatId) -> Speaker {
        Speaker::Seat(seat)
    }

    /// The last seat's first answer brings the line that says every seat answered and who
    /// gathers the points, once.
    #[test]
    fn the_last_first_answer_says_everyone_answered() {
        let mut council = council();
        let answers = [
            (opus(), "A token bucket."),
            (luna(), "A counter."),
            (flash(), "A bucket."),
        ];
        for (n, (seat_id, words)) in (1..).zip(answers) {
            let said = council.on_now(&CouncilEventKind::Message(Box::new(message(
                n,
                seat(seat_id),
                MessageKind::FirstAnswer,
                words,
            ))));
            assert_eq!(said.items.len(), 1, "{:?}", said.items);
        }
        let said = council.on_now(&CouncilEventKind::Message(Box::new(message(
            4,
            seat(qwen()),
            MessageKind::FirstAnswer,
            "In memory.",
        ))));
        assert_eq!(said.items.len(), 2, "{:?}", said.items);
        assert_eq!(said.items[1], story::everyone_answered());
        let said = council.on_now(&CouncilEventKind::Message(Box::new(message(
            5,
            seat(qwen()),
            MessageKind::Talk,
            "Still in memory.",
        ))));
        assert_eq!(said.items.len(), 1, "{:?}", said.items);
    }

    #[test]
    fn the_engines_events_make_the_talk_the_screen_shows() {
        let mut council = council();
        land(
            &mut council,
            message(1, seat(opus()), MessageKind::FirstAnswer, "A token bucket."),
        );
        land(
            &mut council,
            message(2, seat(qwen()), MessageKind::FirstAnswer, "In memory."),
        );
        let bucket = "A bucket has two numbers to tune, burst and refill. A window counter is \
            one INCR and one EXPIRE, and nobody has to pick a burst size.";
        land(
            &mut council,
            replying(
                message(3, seat(luna()), MessageKind::Talk, bucket),
                1,
                "A token bucket per API key, in Redis",
            ),
        );
        for caster in [opus(), flash(), qwen()] {
            tap(&mut council, 3, caster, Stance::Object);
        }
        assert_eq!(council.item(&id(3)), Some(story::luna_on_buckets(true)));

        let nodes = story::FLASH_ON_NODES
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        land(
            &mut council,
            replying(
                message(4, seat(flash()), MessageKind::Talk, &nodes),
                2,
                "An in-memory bucket on each node",
            ),
        );
        tap(&mut council, 4, opus(), Stance::Agree);
        tap(&mut council, 4, luna(), Stance::Object);
        assert_eq!(council.item(&id(4)), Some(story::flash_on_nodes(false)));

        council.on_now(&CouncilEventKind::Writing {
            seat: qwen(),
            why: FloorReason::RepliedTo { by: flash() },
        });
        let (writers, reason) = council.typing();
        assert_eq!(writers, vec![qwen()]);
        // Each seat is named as its badge names it.
        let read = |reason: &[Piece]| -> String {
            let theme =
                crate::base::kit::logo::golden::pictured(&crate::base::theme::Theme::dark());
            super::super::words::spans(reason, &story::faces(), &theme)
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        assert_eq!(
            read(&reason),
            "Qwen3 Coder 30B is writing · 3.8 Flash replied to its message"
        );

        land(
            &mut council,
            replying(
                message(5, Speaker::You, MessageKind::Talk, story::NO_KEY),
                4,
                "And the balancer's probe calls /health with no key",
            ),
        );
        council.on_now(&CouncilEventKind::Writing {
            seat: flash(),
            why: FloorReason::You,
        });
        let (_, reason) = council.typing();
        assert_eq!(
            read(&reason),
            "3.8 Flash is answering you · Qwen3 Coder 30B finishes first, then reads yours"
        );
        for reader in [opus(), luna(), flash(), qwen()] {
            council.on_now(&CouncilEventKind::Read {
                message: id(5),
                seat: reader,
            });
        }
        assert_eq!(council.item(&id(5)), Some(story::your_reply(true)));

        let mut agrees = replying(
            message(
                6,
                seat(qwen()),
                MessageKind::Talk,
                "Agreed: Redis for real, the in-memory store for tests. On calls with no key \
                 I'm with ",
            ),
            4,
            "The balancer spreads a client over all 3 nodes",
        );
        agrees.text.push(TextPart::Seat(luna()));
        agrees.text.push(TextPart::Words(": 401.".to_owned()));
        agrees.started_before = Some(id(5));
        land(&mut council, agrees);
        tap(&mut council, 6, opus(), Stance::Object);
        tap(&mut council, 6, luna(), Stance::Agree);
        tap(&mut council, 6, flash(), Stance::Object);
        assert_eq!(
            council.item(&id(6)),
            Some(story::qwen_agrees(&[
                (opus(), Mark::Object),
                (luna(), Mark::Agree),
                (flash(), Mark::Object)
            ]))
        );

        let deprecation = story::FLASH_ON_DEPRECATION
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        land(
            &mut council,
            replying(
                message(7, seat(flash()), MessageKind::Talk, &deprecation),
                5,
                "Calls with no key: don't break them yet",
            ),
        );
        for caster in [opus(), luna(), qwen()] {
            tap(&mut council, 7, caster, Stance::Agree);
        }
        assert_eq!(council.item(&id(7)), Some(story::flash_on_deprecation(3)));
    }

    #[test]
    fn a_thread_drops_the_quote_of_a_reply_to_its_first_message() {
        let mut council = council();
        land(
            &mut council,
            message(1, seat(qwen()), MessageKind::FirstAnswer, "In memory."),
        );
        let nodes = story::FLASH_ON_NODES
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        land(
            &mut council,
            replying(
                message(2, seat(flash()), MessageKind::Talk, &nodes),
                1,
                "An in-memory bucket on each node",
            ),
        );
        land(
            &mut council,
            replying(
                message(3, Speaker::You, MessageKind::Talk, story::NO_KEY),
                2,
                "And the balancer's probe calls /health with no key",
            ),
        );
        let deprecation = story::FLASH_ON_DEPRECATION
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        land(
            &mut council,
            replying(
                message(4, seat(flash()), MessageKind::Talk, &deprecation),
                3,
                "Calls with no key: don't break them yet",
            ),
        );
        let thread = council.thread(&id(2)).expect("a thread");
        assert_eq!(thread.replies, 2);
        assert_eq!(thread.starter, Some(flash()));
        let Item::You(you) = &thread.items[1] else {
            panic!("{:?}", thread.items[1]);
        };
        assert_eq!(you.reply, None);
        let Item::Seat(run) = &thread.items[2] else {
            panic!("{:?}", thread.items[2]);
        };
        assert_eq!(run[0].reply.as_ref().map(|quote| quote.replies), Some(0));

        let bar = council.reply_bar(&id(2)).expect("a reply bar");
        assert_eq!(
            bar.quote,
            "And the balancer's probe calls /health with no key, so a plain 401 takes a node out."
        );
        assert_eq!(council.newest_first().first(), Some(&id(4)));
    }

    #[test]
    fn a_move_names_its_reason_and_how_many_points_are_agreed() {
        let mut council = council();
        let point = |number, state| Point {
            number,
            words: format!("Point {number}"),
            state,
            sides: Vec::new(),
            raised_by: None,
        };
        council.on_now(&CouncilEventKind::Points {
            points: vec![
                point(1, PointState::Agreed),
                point(2, PointState::Agreed),
                point(3, PointState::Agreed),
                point(4, PointState::Agreed),
                point(5, PointState::Open),
                point(6, PointState::Open),
                point(7, PointState::Yours),
            ],
        });
        let said = council.on_now(&CouncilEventKind::Moved(Moved {
            seat: luna(),
            point: 4,
            to: "a token bucket".to_owned(),
            because: MovedBecause::Quote {
                message: None,
                words: "twice the limit at a window's edge".to_owned(),
            },
        }));
        assert_eq!(said.items, vec![story::luna_moved(true)]);
        assert_eq!(council.still_open(), vec!["Point 5", "Point 6"]);
    }

    /// A bubble whose words code hid a name in says what the name named.
    #[test]
    fn a_hidden_name_says_what_it_named() {
        let note = |names: Vec<HiddenName>| {
            let mut council = council();
            let mut said = message(
                1,
                seat(luna()),
                MessageKind::Talk,
                "I ran it with [hidden].",
            );
            said.hidden = u32::try_from(names.len()).unwrap();
            said.hidden_names = names;
            let said = council.on_now(&CouncilEventKind::Message(Box::new(said)));
            match said.items.first() {
                Some(Item::Seat(says)) => says[0].note.clone(),
                other => panic!("{other:?}"),
            }
        };
        let hidden = |words: &str| Some(format!(" · {words} hidden from the seats"));
        assert_eq!(note(vec![HiddenName::Cli]), hidden("a CLI's name"));
        assert_eq!(note(vec![HiddenName::Model]), hidden("a model's name"));
        assert_eq!(note(vec![HiddenName::Maker]), hidden("a maker's name"));
        assert_eq!(
            note(vec![HiddenName::Cli, HiddenName::Model]),
            hidden("names")
        );
        assert_eq!(note(Vec::new()), None);
    }

    /// A bubble whose words guessed who runs a seat says a guess was hidden, and one that still
    /// named a model after its rewrite says code hid the names, in place of what they named.
    #[test]
    fn a_hidden_guess_and_a_rewrite_by_code_say_so() {
        let mut council = council();
        let mut guessed = message(1, seat(luna()), MessageKind::Talk, "[hidden] keeps it.");
        guessed.guesses_hidden = 1;
        let mut rewritten = message(2, seat(luna()), MessageKind::Talk, "[hidden] keeps it.");
        rewritten.hidden = 2;
        rewritten.rewritten = true;
        rewritten.guesses_hidden = 2;
        let mut rows = Vec::new();
        for said in [guessed, rewritten] {
            let said = council.on_now(&CouncilEventKind::Message(Box::new(said)));
            rows.extend(crate::council::tests::golden::printed(
                &said.items,
                crate::council::Look::Pictures,
                120,
            ));
        }
        let notes: Vec<&String> = rows.iter().filter(|row| row.contains(" · ")).collect();
        assert!(
            notes[0].ends_with(" · a guess hidden from the seats"),
            "{rows:#?}"
        );
        assert!(
            notes[1].ends_with(" · names hidden by code · 2 guesses hidden from the seats"),
            "{rows:#?}"
        );
        crate::council::tests::golden::assert_rows_from("hidden_by_code", &rows, 0);
    }

    /// Points, sides, quotes and moves keep their seats held, and the screen names each one.
    #[test]
    fn the_seats_that_kept_words_hold_read_by_their_names() {
        let mut council = council();
        let open = |number, words: String, sides: Vec<String>| Point {
            number,
            words,
            state: PointState::Open,
            raised_by: None,
            sides: sides
                .into_iter()
                .map(|words| argonauts_core::runs::events::council::Side {
                    words,
                    backers: Vec::new(),
                    reason: String::new(),
                })
                .collect(),
        };
        let luna_held = held_seat(&luna());
        council.on_now(&CouncilEventKind::Points {
            points: vec![
                open(
                    1,
                    format!("Keep /health open, as {luna_held} asked"),
                    Vec::new(),
                ),
                open(
                    2,
                    "Where counters live".to_owned(),
                    vec![
                        format!("Redis, as {luna_held} said"),
                        "In memory".to_owned(),
                    ],
                ),
            ],
        });
        assert_eq!(
            council.still_open(),
            vec![
                "Keep /health open, as GPT 5.6 Luna asked",
                "Redis, as GPT 5.6 Luna said or In memory"
            ]
        );

        let said = council.on_now(&CouncilEventKind::Moved(Moved {
            seat: flash(),
            point: 2,
            to: format!("Redis, as {luna_held} said"),
            because: MovedBecause::Quote {
                message: None,
                words: format!("{} ran it on 3 nodes", held_seat(&opus())),
            },
        }));
        let Some(Item::Line(line)) = said.items.first() else {
            panic!("{:?}", said.items);
        };
        assert_eq!(
            line[..3],
            [
                Piece::Called(flash(), Tone::Dim),
                Piece::dim(" moved to Redis, as GPT 5.6 Luna said: "),
                Piece::tone("\"Opus 5.5 ran it on 3 nodes\"", Tone::Quote),
            ]
        );

        land(
            &mut council,
            message(1, seat(luna()), MessageKind::Talk, "Redis."),
        );
        let reply = replying(
            message(2, seat(qwen()), MessageKind::Talk, "Agreed."),
            1,
            &format!("{luna_held} said Redis"),
        );
        let said = council.on_now(&CouncilEventKind::Message(Box::new(reply)));
        let quoted = said
            .items
            .iter()
            .find_map(|item| match item {
                Item::Seat(says) => says.first().and_then(|says| says.reply.clone()),
                _ => None,
            })
            .expect("the reply quotes");
        assert_eq!(quoted.words, "GPT 5.6 Luna said Redis");
    }

    #[test]
    fn a_first_answer_says_what_its_seat_read_and_ran() {
        let mut council = council();
        for tool in ["Read", "Read", "Bash", "Grep"] {
            council.tool_used(&luna(), ToolUse::of(tool));
        }
        land(
            &mut council,
            message(1, seat(luna()), MessageKind::FirstAnswer, "A counter."),
        );
        let Some(Item::Seat(run)) = council.item(&id(1)) else {
            panic!("a seat's message");
        };
        assert_eq!(
            run[0].right.as_deref(),
            Some("Read 2 files · ran 1 shell command")
        );
        assert_eq!(did_words((0, 2)).as_deref(), Some("Ran 2 shell commands"));
        assert_eq!(did_words((4, 0)).as_deref(), Some("Read 4 files"));
    }

    #[test]
    fn a_council_talks_until_it_agrees_or_stops_and_on_when_you_let_it() {
        let mut council = council();
        assert!(council.talking());
        council.on_now(&CouncilEventKind::Agreed { points: 4 });
        assert!(!council.talking());
        council.on_now(&CouncilEventKind::Limits {
            tokens: 400_000,
            minutes: 20,
        });
        assert!(!council.talking(), "limits talk on only after a stop");

        council.on_now(&CouncilEventKind::Private {
            seat: luna(),
            message: id(1),
        });
        land(
            &mut council,
            message(1, Speaker::You, MessageKind::Talk, "Psst."),
        );
        assert!(!council.talking(), "words to one seat don't reopen it");
        land(
            &mut council,
            message(
                2,
                Speaker::You,
                MessageKind::Talk,
                "Send it back: keep /health open.",
            ),
        );
        assert!(council.talking(), "your message to every seat does");

        council.on_now(&CouncilEventKind::Stopped { why: TalkStop::You });
        assert!(!council.talking());
        council.on_now(&CouncilEventKind::Limits {
            tokens: 600_000,
            minutes: 30,
        });
        assert!(council.talking());
    }

    #[test]
    fn your_private_words_stay_out_of_the_talk() {
        let mut council = council();
        let wrote = council.on_now(&CouncilEventKind::Private {
            seat: luna(),
            message: id(4),
        });
        assert_eq!(wrote.items.len(), 1, "the line saying you wrote in private");
        let words = message(4, Speaker::You, MessageKind::Talk, "Between us: keep it.");
        let said = council.on_now(&CouncilEventKind::Message(Box::new(words)));
        assert!(said.items.is_empty(), "{:?}", said.items);
        assert!(council.item(&id(4)).is_none());
        assert!(council.is_private(&id(4)));

        land(
            &mut council,
            message(5, Speaker::You, MessageKind::Talk, story::NO_KEY),
        );
        assert!(matches!(council.item(&id(5)), Some(Item::You(_))));
    }

    #[test]
    fn a_held_message_marks_each_run_of_your_private_words() {
        let pieces = marked("one two three four", &["two three".to_owned()]);
        assert_eq!(
            pieces,
            vec![
                Piece::tone("one", Tone::Faded),
                Piece::tone(" ", Tone::Faded),
                Piece::tone("two", Tone::Matching),
                Piece::tone(" ", Tone::Matching),
                Piece::tone("three", Tone::Matching),
                Piece::tone(" ", Tone::Faded),
                Piece::tone("four", Tone::Faded),
            ]
        );
    }
}
