//! The talk itself: first answers alone, then messages and tapbacks until every seat agrees on
//! every point.
//!
//! Everything the talk does is recorded as a council event, and [`Council`] is those events
//! folded. The talk reads its next move from that fold alone, so a resumed run carries on
//! from the last thing it recorded instead of asking anyone again.

pub mod folder;
mod missed;
mod plan;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::agents::session_runner::Access;
use argonauts_core::base::names::{MessageId, RoleName, SeatId, SessionKey};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::council::{
    CouncilEvent, CouncilEventKind, CouncilMessage, FloorReason, Held, HeldChoice, MessageKind,
    MissedTurn, Moved, MovedBecause, Point, PointStance, PointState, ReplyTo, Seat, Seating, Side,
    Speaker, Stance, TalkStop, TextPart,
};
use argonauts_core::runs::events::session::InputPartKind;
use argonauts_core::settings::council::WhenYouWrite;
use argonauts_core::workflows::steps::StepName;
use argonauts_core::workflows::steps::talk::{Talk, TalkInput, TalkStance, TalkTurn};
use argonauts_core::workflows::workflow::StepRef;
use argonauts_core::{Spend, TaskId};

use super::deliver::DrawKey;
use super::guards::{self, Limits, Spent};
use super::lift::Position;
use super::points::{self, Stances};
use super::relay::{Relay, Taken, YourMove};
use super::{deliver, floor, private, seats, ways};
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::{Answered, Ask, Host};
use crate::plan::context;
use crate::state::ids::IdKind;
use crate::understand::round;
use crate::workflow::{StepAt, StepCall, StepDone};

/// Every council of a run, by its talk step.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Councils(Vec<Council>);

impl Councils {
    pub fn apply(&mut self, event: &CouncilEvent) {
        self.council_of(event).apply(&event.kind);
    }

    /// [`Self::apply`] for an event recorded at `when`, which a private message keeps for the
    /// question that asks you about a message repeating it.
    pub fn apply_at(&mut self, event: &CouncilEvent, when: OffsetDateTime) {
        let council = self.council_of(event);
        council.apply(&event.kind);
        if let CouncilEventKind::Private { message, .. } = &event.kind {
            council.private_at.insert(message.clone(), when);
        }
    }

    fn council_of(&mut self, event: &CouncilEvent) -> &mut Council {
        let at = match self.0.iter().position(|council| council.talk == event.talk) {
            Some(at) => at,
            None => {
                self.0.push(Council::new(event.talk.clone()));
                self.0.len() - 1
            }
        };
        &mut self.0[at]
    }

    pub fn get(&self, talk: &StepRef) -> Option<&Council> {
        self.0.iter().find(|council| &council.talk == talk)
    }

    /// The council still talking, if one is: the one your messages go to.
    pub fn talking(&self) -> Option<&Council> {
        self.0
            .iter()
            .rev()
            .find(|council| council.seating.is_some() && council.ending.is_none())
    }

    /// The newest council, talking or done, for what you write to one seat after the talk.
    pub fn newest(&self) -> Option<&Council> {
        self.0.last()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Council> {
        self.0.iter()
    }
}

/// How a talk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ended", rename_all = "snake_case")]
pub enum Ending {
    Agreed,
    Stopped { why: TalkStop },
}

/// One council as its events tell it: who sits, what was said, who read it, and where each
/// point stands. Positions are places in the council's own event order, which say what came
/// before what.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Council {
    pub talk: StepRef,
    pub seating: Option<Seating>,
    /// Every message in the order it came in, with its place.
    pub messages: Vec<Said>,
    pub points: Vec<Point>,
    pub stances: Stances,
    /// Tapbacks by message, then by seat, each with its place.
    pub tapbacks: BTreeMap<MessageId, BTreeMap<SeatId, (Stance, usize)>>,
    /// Where each seat read each message, council and private alike.
    pub read: BTreeMap<MessageId, BTreeMap<SeatId, usize>>,
    /// The seat writing now and why it has the floor.
    pub writing: Option<(SeatId, FloorReason)>,
    /// Where the seat writing now took the floor.
    #[serde(default)]
    pub writing_since: usize,
    pub sat_out: BTreeSet<SeatId>,
    /// The token and minute limits, once set.
    pub limits: Option<(u64, u32)>,
    /// Seat messages in a row that moved nobody's stance and changed no point.
    pub unmoved: u32,
    /// Each changed stance with its reason, and where it was recorded.
    pub moved: Vec<(Moved, usize)>,
    pub ending: Option<Ending>,
    /// The note keeper's plan, once written after the talk ended.
    pub plan: Option<MessageId>,
    /// Every plan the note keeper wrote, the ones you sent back among them.
    #[serde(default)]
    pub plans: BTreeSet<MessageId>,
    pub held: BTreeMap<MessageId, Held>,
    pub released: BTreeMap<MessageId, HeldChoice>,
    /// Your private messages, each to one seat, with the place each was sent.
    pub private: Vec<(SeatId, MessageId, usize)>,
    /// What each private message says. One you share joins the council's messages.
    pub private_words: BTreeMap<MessageId, CouncilMessage>,
    /// When you wrote each private message.
    #[serde(default)]
    pub private_at: BTreeMap<MessageId, OffsetDateTime>,
    pub shared: BTreeSet<MessageId>,
    /// A seat stopped mid-message for yours, which writes again first.
    pub restarted: Option<SeatId>,
    /// A seat whose held message you sent back to be written again.
    pub rewrite: Option<SeatId>,
    /// Each seat's last turn's end: a message, a stopped message, a tapback or its stances.
    pub last_output: BTreeMap<SeatId, usize>,
    /// Where each point took its current words.
    #[serde(default)]
    pub worded_at: BTreeMap<u32, usize>,
    /// Where the skeptic took the floor to challenge each point.
    #[serde(default)]
    pub challenged: BTreeMap<u32, usize>,
    /// Where each point every seat agrees on came to be agreed.
    #[serde(default)]
    pub agreed_at: BTreeMap<u32, usize>,
    /// Words a seat raised that later words replaced, with the seats that backed them then.
    #[serde(default)]
    pub replaced: Vec<Position>,
    /// The run review has looked at this council.
    #[serde(default)]
    pub reviewed: bool,
    /// Tasks of its plan whose checker code picked, each recorded once.
    #[serde(default)]
    pub checked: BTreeSet<TaskId>,
    /// What the talk's moves after the note keeper's list cost, which the stop guard counts.
    #[serde(default)]
    pub worked: Worked,
    /// Where the round of tapbacks a seat last cast in began. Every seat's tapback is recorded
    /// once all are cast, so none of the round reached the seat before its next turn.
    #[serde(default)]
    pub round_from: BTreeMap<SeatId, usize>,
    /// How many turns each seat missed.
    #[serde(default)]
    pub missed: BTreeMap<SeatId, u32>,
    /// The reactions a seat missed, by message: it isn't asked for one again.
    #[serde(default)]
    pub reactions_missed: BTreeSet<(MessageId, SeatId)>,
    /// The seats that missed twice and sit out the rest of the talk.
    #[serde(default)]
    pub left: BTreeSet<SeatId>,
    /// Events folded so far.
    pub seen: usize,
}

/// The talk's wall time while some seat worked, and the seats' billable tokens, from the note
/// keeper's list on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worked {
    pub ms: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Said {
    pub message: CouncilMessage,
    pub at: usize,
}

impl Council {
    fn new(talk: StepRef) -> Self {
        Self {
            talk,
            seating: None,
            messages: Vec::new(),
            points: Vec::new(),
            stances: Stances::default(),
            tapbacks: BTreeMap::new(),
            read: BTreeMap::new(),
            writing: None,
            writing_since: 0,
            sat_out: BTreeSet::new(),
            limits: None,
            unmoved: 0,
            moved: Vec::new(),
            ending: None,
            plan: None,
            plans: BTreeSet::new(),
            held: BTreeMap::new(),
            released: BTreeMap::new(),
            private: Vec::new(),
            private_words: BTreeMap::new(),
            private_at: BTreeMap::new(),
            shared: BTreeSet::new(),
            restarted: None,
            rewrite: None,
            last_output: BTreeMap::new(),
            worded_at: BTreeMap::new(),
            challenged: BTreeMap::new(),
            agreed_at: BTreeMap::new(),
            replaced: Vec::new(),
            reviewed: false,
            checked: BTreeSet::new(),
            worked: Worked::default(),
            round_from: BTreeMap::new(),
            missed: BTreeMap::new(),
            reactions_missed: BTreeSet::new(),
            left: BTreeSet::new(),
            seen: 0,
        }
    }

    pub fn apply(&mut self, kind: &CouncilEventKind) {
        let at = self.seen;
        self.seen += 1;
        match kind {
            CouncilEventKind::Seated(seating) => self.seating = Some(seating.clone()),
            CouncilEventKind::Writing { seat, why } => {
                if let FloorReason::Unproven { point } = why {
                    self.challenged.insert(*point, at);
                }
                self.writing = Some((seat.clone(), why.clone()));
                self.writing_since = at;
            }
            CouncilEventKind::WritingEnded { seat } => {
                self.stop_writing(seat);
                self.round_from.remove(seat);
                self.last_output.insert(seat.clone(), at);
                self.restarted = Some(seat.clone());
            }
            CouncilEventKind::Message(message) => self.take_message(message, at),
            CouncilEventKind::Tapback {
                message,
                seat,
                stance,
            } => {
                let cast = self.tapbacks.entry(message.clone()).or_default();
                cast.insert(seat.clone(), (*stance, at));
                let began = cast.values().map(|(_, at)| *at).min().unwrap_or(at);
                self.round_from.insert(seat.clone(), began);
                self.last_output.insert(seat.clone(), at);
            }
            CouncilEventKind::Read { message, seat } => {
                self.read
                    .entry(message.clone())
                    .or_default()
                    .insert(seat.clone(), at);
            }
            CouncilEventKind::Points { points } => {
                // New words alone move nobody: a seat that rewords a point each message must
                // not keep its talk from stopping.
                let states = |points: &[Point]| -> Vec<(u32, _)> {
                    points
                        .iter()
                        .map(|point| (point.number, point.state))
                        .collect()
                };
                if !self.points.is_empty() && states(&self.points) != states(points) {
                    self.unmoved = 0;
                }
                // Stances are on a point's words, so new words start them over, and the old
                // words stay a position with the seats that backed them until then.
                for point in points {
                    let was = self.points.iter().find(|was| was.number == point.number);
                    if let Some(was) = was.filter(|was| was.words != point.words) {
                        if let Some(raised_by) = &was.raised_by {
                            self.replaced.push(Position {
                                raised_by: raised_by.clone(),
                                backers: self.stances.backing(point.number),
                            });
                        }
                        self.stances.forget(point.number);
                    }
                    if was.is_none_or(|was| was.words != point.words) {
                        self.worded_at.insert(point.number, at);
                    }
                    let agreed_before = was.is_some_and(|was| {
                        was.state == PointState::Agreed && was.words == point.words
                    });
                    if point.state != PointState::Agreed {
                        self.agreed_at.remove(&point.number);
                    } else if !agreed_before {
                        self.agreed_at.insert(point.number, at);
                    }
                }
                self.points.clone_from(points);
            }
            CouncilEventKind::Stances { seat, stances } => {
                self.stances.replace(seat, stances);
                self.last_output.insert(seat.clone(), at);
            }
            CouncilEventKind::Moved(moved) => {
                self.unmoved = 0;
                self.moved.push((moved.clone(), at));
            }
            CouncilEventKind::Limits { tokens, minutes } => {
                // Limits set again after a stop are you letting the talk go on, so what moved
                // nobody counts afresh.
                if matches!(self.ending, Some(Ending::Stopped { .. })) {
                    self.ending = None;
                    self.unmoved = 0;
                }
                self.limits = Some((*tokens, *minutes));
            }
            CouncilEventKind::Agreed { .. } => self.ending = Some(Ending::Agreed),
            CouncilEventKind::Stopped { why } => self.ending = Some(Ending::Stopped { why: *why }),
            CouncilEventKind::Held(held) => {
                self.held
                    .insert(held.message.clone(), held.as_ref().clone());
            }
            CouncilEventKind::Released { message, choice } => {
                self.released.insert(message.clone(), *choice);
                if *choice == HeldChoice::Rewrite {
                    self.rewrite = self.held.get(message).map(|held| held.seat.clone());
                }
            }
            CouncilEventKind::Private { seat, message } => {
                self.private.push((seat.clone(), message.clone(), at));
            }
            CouncilEventKind::Shared { message } => {
                self.shared.insert(message.clone());
                if let Some(words) = self.private_words.get(message).cloned() {
                    self.unmoved = 0;
                    self.messages.push(Said { message: words, at });
                }
            }
            CouncilEventKind::SatOut { seat } => {
                self.sat_out.insert(seat.clone());
            }
            CouncilEventKind::Rejoined { seat, .. } => {
                self.sat_out.remove(seat);
            }
            CouncilEventKind::Missed(missed) => {
                self.stop_writing(&missed.seat);
                *self.missed.entry(missed.seat.clone()).or_default() += 1;
                if let MissedTurn::Reaction { message } = &missed.turn {
                    self.reactions_missed
                        .insert((message.clone(), missed.seat.clone()));
                }
                if missed.for_good {
                    self.left.insert(missed.seat.clone());
                }
            }
            CouncilEventKind::Check(pick) => {
                self.checked.insert(pick.task.clone());
            }
            CouncilEventKind::Worked { ms, tokens } => {
                self.worked.ms = self.worked.ms.saturating_add(*ms);
                self.worked.tokens = self.worked.tokens.saturating_add(*tokens);
            }
            CouncilEventKind::RunReview { .. } => self.reviewed = true,
            // The seat goes on with what it can read.
            CouncilEventKind::FolderBlocked { .. } => {}
        }
    }

    /// Every position a seat raised in the talk, with the seats that backed it: words that
    /// later words replaced, backed as they stood then, and each point's words now.
    pub fn positions(&self) -> Vec<Position> {
        let now = self.points.iter().filter_map(|point| {
            Some(Position {
                raised_by: point.raised_by.clone()?,
                backers: self.stances.backing(point.number),
            })
        });
        self.replaced.iter().cloned().chain(now).collect()
    }

    fn take_message(&mut self, message: &CouncilMessage, at: usize) {
        if self.private.iter().any(|(_, id, _)| id == &message.id) {
            self.private_words
                .insert(message.id.clone(), message.clone());
            return;
        }
        match &message.from {
            Speaker::Seat(seat) => {
                self.stop_writing(seat);
                if self.restarted.as_ref() == Some(seat) {
                    self.restarted = None;
                }
                if self.rewrite.as_ref() == Some(seat) {
                    self.rewrite = None;
                }
                self.last_output.insert(seat.clone(), at);
                self.round_from.remove(seat);
                if message.kind == MessageKind::Talk {
                    self.unmoved += 1;
                }
                if message.kind == MessageKind::Notes && self.ending.is_some() {
                    self.plan = Some(message.id.clone());
                    self.plans.insert(message.id.clone());
                }
            }
            // You gave the seats something new to weigh, and a council that had ended talks
            // on with it, as when you send its plan back.
            Speaker::You => {
                self.unmoved = 0;
                self.ending = None;
                self.plan = None;
            }
        }
        self.messages.push(Said {
            message: message.clone(),
            at,
        });
    }

    fn stop_writing(&mut self, seat: &SeatId) {
        if self
            .writing
            .as_ref()
            .is_some_and(|(writing, _)| writing == seat)
        {
            self.writing = None;
        }
    }

    pub fn seats(&self) -> &[Seat] {
        self.seating
            .as_ref()
            .map_or(&[], |seating| seating.seats.as_slice())
    }

    pub fn seat(&self, id: &SeatId) -> Option<&Seat> {
        self.seats().iter().find(|seat| &seat.id == id)
    }

    pub fn notes(&self) -> Option<&SeatId> {
        self.seating.as_ref()?.notes.as_ref()
    }

    /// The seats at the table now: every seat but those carrying on in their own CLI and those
    /// that missed two turns.
    pub fn table(&self) -> Vec<SeatId> {
        self.seats()
            .iter()
            .filter(|seat| !self.sat_out.contains(&seat.id) && !self.left.contains(&seat.id))
            .map(|seat| seat.id.clone())
            .collect()
    }

    pub fn message(&self, id: &MessageId) -> Option<&CouncilMessage> {
        self.messages
            .iter()
            .find(|said| &said.message.id == id)
            .map(|said| &said.message)
    }

    /// Who wrote a message: a seat, or `None` for yours.
    pub fn author(&self, id: &MessageId) -> Option<&SeatId> {
        match &self.message(id)?.from {
            Speaker::Seat(seat) => Some(seat),
            Speaker::You => None,
        }
    }

    /// The first answer each seat wrote alone, in seat order.
    pub fn first_answers(&self) -> Vec<&CouncilMessage> {
        self.messages
            .iter()
            .map(|said| &said.message)
            .filter(|message| message.kind == MessageKind::FirstAnswer)
            .collect()
    }

    pub fn has_first_answer(&self, seat: &SeatId) -> bool {
        self.first_answers()
            .iter()
            .any(|message| message.from == Speaker::Seat(seat.clone()))
    }

    /// Whether a message was given to this seat, in a turn that ended or in the one it is in.
    pub fn given_to(&self, message: &MessageId, seat: &SeatId) -> bool {
        self.read
            .get(message)
            .is_some_and(|by| by.contains_key(seat))
    }

    /// Whether a message reached this seat in a turn that has ended since.
    fn reached(&self, message: &MessageId, seat: &SeatId) -> bool {
        let Some(read_at) = self.read.get(message).and_then(|by| by.get(seat)) else {
            return false;
        };
        self.last_output.get(seat).is_some_and(|out| out > read_at)
    }

    /// The council messages a seat hasn't had yet, in landing order: none of its own, and only
    /// those that went out to the seats. One given to a turn that never ended goes again.
    pub fn unread(&self, seat: &SeatId) -> Vec<&CouncilMessage> {
        self.messages
            .iter()
            .map(|said| &said.message)
            .filter(|message| message.from != Speaker::Seat(seat.clone()))
            .filter(|message| self.out_to_seats(&message.id))
            .filter(|message| !self.reached(&message.id, seat))
            .collect()
    }

    /// Your private messages to this seat it hasn't had yet.
    pub fn unread_private(&self, seat: &SeatId) -> Vec<&MessageId> {
        self.private
            .iter()
            .filter(|(to, message, _)| to == seat && !self.reached(message, seat))
            .map(|(_, message, _)| message)
            .collect()
    }

    /// Whether the seats may read a message: one code held goes out only as you choose.
    pub fn out_to_seats(&self, message: &MessageId) -> bool {
        if !self.held.contains_key(message) {
            return true;
        }
        matches!(
            self.released.get(message),
            Some(HeldChoice::AsWritten | HeldChoice::Share)
        )
    }

    /// A message code held that you haven't chosen for yet.
    pub fn waiting_on_you(&self) -> Option<&Held> {
        self.held
            .values()
            .find(|held| !self.released.contains_key(&held.message))
    }

    /// Tapbacks cast on a message after this place, by seat.
    pub fn tapbacks_after(&self, message: &MessageId, after: usize) -> Vec<(&SeatId, Stance)> {
        self.tapbacks
            .get(message)
            .map(|by| {
                by.iter()
                    .filter(|(_, (_, at))| *at > after)
                    .map(|(seat, (stance, _))| (seat, *stance))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The talk messages a seat has written, which the quietest is counted by.
    pub fn spoken(&self, seat: &SeatId) -> usize {
        self.messages
            .iter()
            .filter(|said| {
                said.message.kind == MessageKind::Talk
                    && said.message.from == Speaker::Seat(seat.clone())
            })
            .count()
    }

    /// Where the seat's last talk message came in, `None` before its first.
    pub fn last_spoke(&self, seat: &SeatId) -> Option<usize> {
        self.messages
            .iter()
            .rev()
            .find(|said| {
                said.message.kind == MessageKind::Talk
                    && said.message.from == Speaker::Seat(seat.clone())
            })
            .map(|said| said.at)
    }

    /// The newest message from anyone, and its place.
    pub fn last_message(&self) -> Option<&Said> {
        self.messages.last()
    }

    /// The place a seat's next turn reads on from: where its last turn ended, or for a turn of
    /// tapbacks, where that round began, since the others' were cast while it reacted.
    pub fn since(&self, seat: &SeatId) -> usize {
        match self.round_from.get(seat) {
            Some(began) => began.saturating_sub(1),
            None => self.last_output.get(seat).copied().unwrap_or(0),
        }
    }

    pub fn latest(&self, seat: &SeatId) -> Vec<PointStance> {
        self.stances.latest(seat)
    }
}

/// The prompts of a seat's turns, one for each move.
pub const FIRST_ANSWER: &str = argonauts_core::template!("prompts/council/talk-first.md");
pub const MESSAGE: &str = argonauts_core::template!("prompts/council/talk-message.md");
pub const TAPBACK: &str = argonauts_core::template!("prompts/council/talk-tapback.md");
pub const POINTS: &str = argonauts_core::template!("prompts/council/talk-points.md");
pub const PLAN: &str = argonauts_core::template!("prompts/council/talk-plan.md");
pub const PRIVATE: &str = argonauts_core::template!("prompts/council/talk-private.md");

/// A tapback is a short turn: no tools, and this long at most.
const TAPBACK_TIME: Duration = Duration::from_secs(120);

/// What the talk does next, read from the council's record alone.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// These seats haven't written their first answer.
    FirstAnswers(Vec<SeatId>),
    /// The note keeper lists the points from the first answers.
    ListPoints(SeatId),
    /// These seats react to a message that changed a point.
    React {
        message: MessageId,
        seats: Vec<SeatId>,
    },
    /// Code held a message, and you choose what happens to it.
    Held(Held),
    /// A seat answers what you wrote it in private.
    Private(SeatId),
    Write(SeatId, FloorReason),
    Agreed,
    Stop(TalkStop),
    /// The talk stopped with points open, which you settle, or the seats vote on.
    Settle,
    /// The note keeper writes the plan.
    Plan(SeatId),
    Done,
}

/// The talk's next move. Holding for you comes first, then the first answers, the note keeper's
/// list and the reactions a changed point needs, then your private messages, and only then
/// agreement, the stop guard and the floor. `spent` gives the tokens and the working time; the
/// messages that moved nobody are the council's own count.
pub fn next_move(council: &Council, spent: &Spent, limits: &Limits) -> Next {
    // A keeper that missed two turns hands the notes to the next seat still in.
    let keeper = || {
        council
            .notes()
            .filter(|keeper| !council.left.contains(*keeper))
            .cloned()
            .or_else(|| {
                council
                    .seats()
                    .iter()
                    .find(|seat| !council.left.contains(&seat.id))
                    .map(|seat| seat.id.clone())
            })
    };
    if council.ending.is_some() {
        if council.plan.is_some() {
            return Next::Done;
        }
        if matches!(council.ending, Some(Ending::Stopped { .. }))
            && !points::all_settled(&council.points)
        {
            return Next::Settle;
        }
        return keeper().map_or(Next::Done, Next::Plan);
    }
    if let Some(held) = council.waiting_on_you() {
        return Next::Held(held.clone());
    }
    let table = council.table();
    let missing: Vec<SeatId> = table
        .iter()
        .filter(|seat| !council.has_first_answer(seat))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Next::FirstAnswers(missing);
    }
    let lists = council
        .messages
        .iter()
        .filter(|said| said.message.kind == MessageKind::Notes)
        .count();
    // A list with no points is a notes turn that failed: the keeper gets one more, then the
    // talk stops for you rather than agreeing on nothing.
    let no_points = council.points.is_empty();
    if (lists == 0 || (no_points && lists == 1))
        && let Some(keeper) = keeper()
    {
        return Next::ListPoints(keeper);
    }
    if no_points {
        return Next::Stop(TalkStop::NoPoints);
    }
    if let Some((message, seats)) = reactions_due(council, &table) {
        return Next::React { message, seats };
    }
    if let Some(seat) = table
        .iter()
        .find(|seat| !council.unread_private(seat).is_empty())
    {
        return Next::Private(seat.clone());
    }
    if points::all_settled(&council.points) {
        return Next::Agreed;
    }
    let spent = Spent {
        unmoved: council.unmoved,
        ..*spent
    };
    if let Some(why) = guards::stop(&spent, limits) {
        return Next::Stop(why);
    }
    match floor::next(council) {
        Some((seat, why)) => Next::Write(seat, why),
        // Every seat is carrying on in its own CLI.
        None => Next::Stop(TalkStop::You),
    }
}

/// The newest seat message, when it changed a point and some seat at the table hasn't
/// reacted: every seat to the note keeper's list, the others to anyone else's.
fn reactions_due(council: &Council, table: &[SeatId]) -> Option<(MessageId, Vec<SeatId>)> {
    let last = council
        .messages
        .iter()
        .rev()
        .find(|said| matches!(said.message.from, Speaker::Seat(_)))?;
    let message = &last.message;
    // A plan settles points rather than changing one, so nobody reacts to it.
    if council.plans.contains(&message.id) {
        return None;
    }
    let changed = message.kind == MessageKind::Notes || !message.stances.is_empty();
    if !changed || !council.out_to_seats(&message.id) {
        return None;
    }
    let reacted = council.tapbacks.get(&message.id);
    let seats: Vec<SeatId> = table
        .iter()
        .filter(|seat| {
            message.kind == MessageKind::Notes || message.from != Speaker::Seat((*seat).clone())
        })
        .filter(|seat| reacted.is_none_or(|by| !by.contains_key(*seat)))
        .filter(|seat| {
            !council
                .reactions_missed
                .contains(&(message.id.clone(), (*seat).clone()))
        })
        .cloned()
        .collect();
    (!seats.is_empty()).then(|| (message.id.clone(), seats))
}

/// `talk`: the council sits, every seat answers alone, the note keeper lists the points, and
/// the seats talk one at a time until every seat agrees on every point or the stop guard ends
/// it. Then the note keeper writes the plan, which goes on to your plan approval like any
/// other.
pub async fn step(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    // The seats answer your request as you wrote it, so it stands as the goal, and the plan is
    // the first thing you approve.
    crate::roles::captain::prompt_is_the_goal(host).await?;
    context::phase_started(host)?;
    let talk = call.at.step.clone();
    if council_now(host, &talk).is_none_or(|council| council.seating.is_none()) {
        sit_down(host, call)?;
    }
    talk_on_if_sent_back(host, &talk)?;
    let seated = council_now(host, &talk)
        .ok_or_else(|| EngineError::Refused("the council has no record".to_owned()))?;
    let mut table = Table::new(
        deliver::relay_for(host, &seated)?,
        DrawKey::of(host, &seated)?,
    );
    loop {
        let council = council_now(host, &talk)
            .ok_or_else(|| EngineError::Refused("the council has no record".to_owned()))?;
        let limits = Limits::of(&host.settings().council.stop_after, council.limits);
        let next = next_move(&council, &spent(host, &call.at), &limits);
        // The first answers and the list are the talk's start; what you decide isn't the
        // seats' work. Every other move counts toward the stop guard.
        let counted = matches!(
            next,
            Next::React { .. } | Next::Write(..) | Next::Private(_)
        );
        let began = Instant::now();
        match next {
            Next::Done => break,
            Next::FirstAnswers(seats) => {
                first_answers(host, call, &council, &seats, &mut table).await?
            }
            Next::ListPoints(keeper) => {
                list_points(host, call, &council, &keeper, &mut table).await?
            }
            Next::React { message, seats } => {
                react(host, call, &council, &message, &seats, &mut table).await?;
            }
            Next::Held(held) => private::decide(host, call, &council, &held).await?,
            Next::Private(seat) => {
                answer_in_private(host, call, &council, &seat, &mut table).await?
            }
            Next::Write(seat, why) => write(host, call, &council, &seat, why, &mut table).await?,
            Next::Agreed => {
                let points = u32::try_from(council.points.len()).unwrap_or(u32::MAX);
                record(host, &talk, CouncilEventKind::Agreed { points })?;
            }
            Next::Stop(why) => record(host, &talk, CouncilEventKind::Stopped { why })?,
            Next::Settle => points::settle(host, call, &council).await?,
            Next::Plan(keeper) => {
                plan::write_plan(host, call, &council, &keeper, &mut table).await?
            }
        }
        let tokens = table.tokens.swap(0, Ordering::Relaxed);
        if counted {
            let ms = u64::try_from(began.elapsed().as_millis()).unwrap_or(u64::MAX);
            record(host, &talk, CouncilEventKind::Worked { ms, tokens })?;
        }
    }
    let council = council_now(host, &talk).unwrap_or_else(|| Council::new(talk.clone()));
    let (agreed, of) = points::agreed_of(&council.points);
    let summary = match council.ending {
        Some(Ending::Stopped {
            why: TalkStop::NoPoints,
        }) => "The talk stopped: the note keeper listed no points".to_owned(),
        Some(Ending::Stopped { .. }) => format!(
            "The talk stopped with {agreed} of {of} points agreed, and the rest were settled"
        ),
        _ => format!("Every seat agreed on {of} points"),
    };
    Ok(StepDone::done(summary))
}

pub(crate) fn council_now(host: &Host, talk: &StepRef) -> Option<Council> {
    host.read(|state| state.councils.get(talk).cloned())
}

pub(crate) fn record(host: &Host, talk: &StepRef, kind: CouncilEventKind) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Council(CouncilEvent {
            talk: talk.clone(),
            kind,
        }),
    )?;
    Ok(())
}

/// The seats in order, as the council's setup lists them, and the note keeper, who rotates
/// from council to council.
fn sit_down(host: &Host, call: &StepCall) -> EngineResult<()> {
    let settings = host.settings();
    let seats =
        seats::seat(settings, host.names()).map_err(|err| EngineError::Refused(err.to_string()))?;
    let talk = &call.at.step;
    let path: Vec<String> = talk.path.iter().map(u32::to_string).collect();
    // Drawn the same when the run resumes and anew for each run, then one seat on for each
    // council the run held before.
    let salt = floor::salt(&[
        host.run_id().as_str(),
        talk.workflow.as_str(),
        &talk.phase.to_string(),
        &path.join("."),
    ]);
    let before = host.read(|state| {
        state
            .councils
            .iter()
            .filter(|council| &council.talk != talk)
            .count()
    });
    let nth = (salt as usize).wrapping_add(before);
    let notes = seats::note_keeper(&seats, nth);
    record(
        host,
        talk,
        CouncilEventKind::Seated(Seating { seats, notes }),
    )?;
    // Your request opens the talk as your first message, which each seat reads as it writes
    // its first answer.
    let request = host.read(round::request).unwrap_or_default();
    if !request.trim().is_empty() {
        let id = MessageId::numbered(host.allocate(IdKind::Message)?);
        let message = Box::new(yours(id, &request));
        record(host, talk, CouncilEventKind::Message(message))?;
    }
    let limits = Limits::of(&settings.council.stop_after, None);
    record(
        host,
        talk,
        CouncilEventKind::Limits {
            tokens: limits.tokens,
            minutes: limits.minutes(),
        },
    )?;
    // A way your request names for a task binds from the start, as a point of yours, with
    // each seat it names held as the seat.
    let mut yours = Vec::new();
    let council = council_now(host, talk);
    let relay = council
        .as_ref()
        .map(|council| deliver::relay_for(host, council))
        .transpose()?;
    for asked in ways::asked_in(&request, &ways::workflows_for(host)) {
        let clause = match &relay {
            Some(relay) => relay.yours_held(&asked.clause),
            None => asked.clause.clone(),
        };
        points::add_yours(&mut yours, &clause);
    }
    if yours.is_empty() {
        return Ok(());
    }
    record(host, talk, CouncilEventKind::Points { points: yours })
}

/// A plan the council wrote that you sent back: your note joins the talk as your message, and
/// the council talks on from where it stood.
fn talk_on_if_sent_back(host: &Host, talk: &StepRef) -> EngineResult<()> {
    let Some(council) = council_now(host, talk) else {
        return Ok(());
    };
    // Your newest plan approval sent back the plan the talk settled, and no plan came since.
    let note = host.read(crate::state::plan::sent_back);
    let (Some(_), Some(note)) = (&council.plan, note) else {
        return Ok(());
    };
    let words = if note.trim().is_empty() {
        "I sent the plan back. Talk on, and write it again.".to_owned()
    } else {
        note
    };
    let id = MessageId::numbered(host.allocate(IdKind::Message)?);
    let mut message = yours(id, &words);
    // Each seat your note names reads as its own letter, as in any message of yours.
    if let Ok(parts) = deliver::relay_for(host, &council)?.from_you(words.trim())
        && !parts.is_empty()
    {
        message.text = parts;
    }
    record(host, talk, CouncilEventKind::Message(Box::new(message)))
}

/// A message of yours to the whole council.
pub(crate) fn yours(id: MessageId, words: &str) -> CouncilMessage {
    CouncilMessage {
        id,
        from: Speaker::You,
        kind: MessageKind::Talk,
        text: vec![TextPart::Words(words.trim().to_owned())],
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

/// What the talk has spent since the note keeper's list: the seats' tokens, and its wall time
/// while some seat worked. The first answers and the list itself are the talk's start, which
/// no guard cuts short.
pub(super) fn spent(host: &Host, at: &StepAt) -> Spent {
    let worked = council_now(host, &at.step)
        .map(|council| council.worked)
        .unwrap_or_default();
    Spent {
        unmoved: 0,
        tokens: worked.tokens,
        working: Duration::from_millis(worked.ms),
    }
}

/// The relay the talk's messages pass through, the key each reader's orders are drawn with,
/// and each seat's newest session in this step, so its next turn carries the same
/// conversation on.
#[derive(Debug)]
struct Table {
    relay: Relay,
    key: DrawKey,
    sessions: BTreeMap<SeatId, SessionKey>,
    /// The tokens the seats wrote in their turns since the talk last counted them.
    tokens: AtomicU64,
}

impl Table {
    fn new(relay: Relay, key: DrawKey) -> Self {
        Self {
            relay,
            key,
            sessions: BTreeMap::new(),
            tokens: AtomicU64::new(0),
        }
    }

    /// The session a seat's next turn resumes: the one it last used in this step, else the
    /// newest the record marks as its own here.
    fn session(&self, host: &Host, council: &Council, seat: &Seat) -> Option<SessionKey> {
        if let Some(key) = self.sessions.get(&seat.id) {
            return Some(key.clone());
        }
        host.read(|state| {
            state
                .sessions
                .in_order()
                .into_iter()
                .rev()
                .filter(|session| session.started.workflow_step.as_ref() == Some(&council.talk))
                .find(|session| session.started.seat.as_ref() == Some(&seat.id))
                .map(|session| session.key.clone())
        })
    }
}

/// One turn of one seat, ready to ask.
struct Turn<'a> {
    seat: &'a Seat,
    template: &'static str,
    access: Access,
    text: String,
}

async fn ask(
    host: &Host,
    call: &StepCall,
    council: &Council,
    table: &Table,
    turn: Turn<'_>,
) -> EngineResult<Answered<TalkTurn>> {
    let Turn {
        seat,
        template,
        access,
        text,
    } = turn;
    // The goal is yours, so a seat it names reads as that seat's letter.
    let mut goal = host.read(context::goal_brief);
    goal.summary = table.relay.yours_for(&seat.id, &goal.summary);
    for line in goal.criteria.iter_mut().chain(goal.assumptions.iter_mut()) {
        *line = table.relay.yours_for(&seat.id, line);
    }
    let input = TalkInput { goal };
    let mut asked = Ask::new(
        Some(call.at.clone()),
        RoleName::Draft,
        seat.model.clone(),
        template,
        input,
    );
    asked.access = access;
    // Every turn of a seat works in the seat's own folder, as a CLI keeps a session with the
    // folder it began in, and the task the seat builds takes that folder over.
    asked.workdir = Some(folder::ensure(host, &council.talk, &seat.id).await?);
    asked.seat = Some(seat.id.clone());
    asked.resume = table.session(host, council, seat);
    asked.slot = slot(host, &call.at, &seat.id);
    if access == Access::NoTools {
        asked.time_limit = Some(TAPBACK_TIME);
    }
    // A session that carries on already holds its seat's frame.
    if asked.resume.is_none() {
        asked.parts.push(context::part(
            InputPartKind::Prompt,
            "your seat",
            table.relay.frame_for(&seat.id),
        ));
    }
    if !text.is_empty() {
        asked
            .parts
            .push(context::part(InputPartKind::Message, "the council", text));
    }
    let answered = host.ask::<Talk>(asked).await?;
    let tokens = host.read(|state| {
        state
            .sessions
            .get(&answered.session)
            .and_then(|session| session.end.as_ref())
            .map_or(0, |end| talk_tokens(&end.spend))
    });
    table.tokens.fetch_add(tokens, Ordering::Relaxed);
    Ok(answered)
}

/// What a seat's turn adds to the talk's token count: the tokens it wrote. The limit is there
/// to stop a talk that goes round in circles, and what the seats write is the talk; the context
/// a CLI reads again each turn grows with the CLI and the folder, not with the talk, and one
/// turn of it can pass the whole limit. It is the count every seat's row, the working line and
/// the stop line show.
pub(crate) fn talk_tokens(spend: &Spend) -> u64 {
    spend.output_tokens
}

/// The slot of a seat's next turn: its newest turn here while that can still resume, else the
/// next free one. Each seat counts only its own turns, since each keeps a session of its own.
fn slot(host: &Host, at: &StepAt, seat: &SeatId) -> u32 {
    host.read(|state| {
        let (from, until) = crate::engine::host::round_window(state, Some(at));
        let mut taken = 0u32;
        let mut newest_resumable = false;
        for session in state.sessions.in_order() {
            let here = session.since >= from
                && until.is_none_or(|until| session.since < until)
                && session.started.workflow_step.as_ref() == Some(&at.step)
                && session.started.seat.as_ref() == Some(seat)
                && session.started.step == Some(StepName::Talk);
            if here {
                taken = taken.saturating_add(1);
                newest_resumable = session.resumable();
            }
        }
        if newest_resumable {
            taken.saturating_sub(1)
        } else {
            taken
        }
    })
}

async fn first_answers(
    host: &Host,
    call: &StepCall,
    council: &Council,
    missing: &[SeatId],
    table: &mut Table,
) -> EngineResult<()> {
    let talk = &call.at.step;
    let seats: Vec<&Seat> = missing.iter().filter_map(|id| council.seat(id)).collect();
    // Your request, the talk's first message, which each seat reads with its first answer.
    let request = council.messages.first().map(|said| &said.message);
    if let Some(request) = request.filter(|message| message.from == Speaker::You) {
        for seat in &seats {
            let (message, seat) = (request.id.clone(), seat.id.clone());
            record(host, talk, CouncilEventKind::Read { message, seat })?;
        }
    }
    for seat in &seats {
        record(
            host,
            talk,
            CouncilEventKind::Writing {
                seat: seat.id.clone(),
                why: FloorReason::FirstAnswer,
            },
        )?;
    }
    let asks = seats.iter().map(|seat| {
        let turn = Turn {
            seat,
            template: FIRST_ANSWER,
            access: Access::ReadOnly,
            text: String::new(),
        };
        ask(host, call, council, table, turn)
    });
    let answers = join_all(asks).await;
    for (seat, answer) in seats.iter().zip(answers) {
        let answered = match answer {
            Ok(answered) => answered,
            Err(err) => {
                missed::missed(
                    host,
                    talk,
                    council,
                    &seat.id,
                    MissedTurn::FirstAnswer,
                    None,
                    err,
                )?;
                continue;
            }
        };
        table
            .sessions
            .insert(seat.id.clone(), answered.session.clone());
        let (turn, taken) =
            taken_in(host, call, council, table, seat, FIRST_ANSWER, answered).await?;
        let now = council_now(host, talk).unwrap_or_else(|| council.clone());
        land(
            host,
            &table.relay,
            &now,
            seat,
            &turn,
            Landing::of(MessageKind::FirstAnswer, taken),
        )?;
    }
    Ok(())
}

/// A seat's message as the relay takes it in. One that names who runs a seat, runs past the
/// length a seat reads, or names a letter that is no seat's gets one turn back to write it
/// again, and what it writes then goes out as code strips it.
async fn taken_in(
    host: &Host,
    call: &StepCall,
    council: &Council,
    table: &mut Table,
    seat: &Seat,
    template: &'static str,
    answered: Answered<TalkTurn>,
) -> EngineResult<(TalkTurn, Taken)> {
    let back = match table
        .relay
        .take_in(&seat.id, answered.output.message.trim())
    {
        Ok(taken) => match taken.ask.clone() {
            Some(back) => back,
            None => return Ok((answered.output, taken)),
        },
        Err(unknown) => unknown.ask,
    };
    let turn = Turn {
        seat,
        template,
        access: Access::ReadOnly,
        text: back,
    };
    let again = ask(host, call, council, table, turn).await?;
    table
        .sessions
        .insert(seat.id.clone(), again.session.clone());
    let taken = taken_again(&table.relay, &seat.id, again.output.message.trim());
    Ok((again.output, taken))
}

/// A message after its one turn back. A letter that still names no seat reads "a seat", so no
/// reader takes it for a seat it knows by that letter.
fn taken_again(relay: &Relay, writer: &SeatId, written: &str) -> Taken {
    let mut words = written.to_owned();
    loop {
        match relay.take_again(writer, &words) {
            Ok(taken) => return taken,
            Err(unknown) => {
                let named = regex::Regex::new(&format!(r"@?\bSeat {0}\b|@{0}\b", unknown.letter))
                    .expect("a pattern");
                let without = named.replace_all(&words, "a seat").into_owned();
                if without == words {
                    return Taken {
                        text: vec![TextPart::Words(words)],
                        mentions: Vec::new(),
                        hidden: 0,
                        kinds: Vec::new(),
                        guesses_hidden: 0,
                        declined: false,
                        ask: None,
                        rewritten: false,
                    };
                }
                words = without;
            }
        }
    }
}

async fn list_points(
    host: &Host,
    call: &StepCall,
    council: &Council,
    keeper: &SeatId,
    table: &mut Table,
) -> EngineResult<()> {
    let talk = &call.at.step;
    let Some(seat) = council.seat(keeper) else {
        return Err(EngineError::Refused(format!("{keeper} doesn't sit")));
    };
    record(
        host,
        talk,
        CouncilEventKind::Writing {
            seat: keeper.clone(),
            why: FloorReason::Notes,
        },
    )?;
    let text = handed(host, table, council, keeper, YourMove::Write)?;
    let turn = Turn {
        seat,
        template: POINTS,
        access: Access::ReadOnly,
        text,
    };
    let answered = ask(host, call, council, table, turn).await?;
    table
        .sessions
        .insert(keeper.clone(), answered.session.clone());
    let listing = answered.output.points.iter().map(|point| {
        let sides = point
            .sides
            .iter()
            .map(|side| Side {
                words: held(&table.relay, keeper, &side.words),
                backers: side
                    .seats
                    .iter()
                    .filter_map(|name| named_seat(&table.relay, keeper, name))
                    .collect(),
                reason: held(&table.relay, keeper, &side.why),
            })
            .collect();
        (held(&table.relay, keeper, &point.words), sides)
    });
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    let listed = after_yours(&now.points, points::listed(listing));
    let notes = TalkTurn {
        message: answered.output.message.clone(),
        ..TalkTurn::default()
    };
    // The notes come with the points, so they go out as code strips them, with no turn back.
    let taken = taken_again(&table.relay, keeper, notes.message.trim());
    let listing = vec![CouncilEventKind::Points { points: listed }];
    land(
        host,
        &table.relay,
        &now,
        seat,
        &notes,
        Landing {
            kind: MessageKind::Notes,
            written: taken,
            after: listing,
        },
    )
}

/// Your points keep their numbers, and the note keeper's list goes on after them.
fn after_yours(points: &[Point], listed: Vec<Point>) -> Vec<Point> {
    let mut all: Vec<Point> = points
        .iter()
        .filter(|point| point.state == PointState::Yours)
        .cloned()
        .collect();
    let after = all.iter().map(|point| point.number).max().unwrap_or(0);
    all.extend(listed.into_iter().map(|point| Point {
        number: point.number + after,
        ..point
    }));
    all
}

/// "Seat T" or "T", as a seat writes another, the seat it means.
fn named_seat(relay: &Relay, reader: &SeatId, name: &str) -> Option<SeatId> {
    let letter = name
        .trim()
        .trim_start_matches("Seat")
        .trim()
        .chars()
        .next()?;
    deliver::seat_by_letter(relay, reader, letter)
}

/// The turn text for a seat: what it hasn't read, marked read as it's handed over.
fn handed(
    host: &Host,
    table: &Table,
    council: &Council,
    seat: &SeatId,
    next: YourMove,
) -> EngineResult<String> {
    let unread = council.unread(seat);
    for message in &unread {
        record(
            host,
            &council.talk,
            CouncilEventKind::Read {
                message: message.id.clone(),
                seat: seat.clone(),
            },
        )?;
    }
    Ok(deliver::turn(
        &table.relay,
        &table.key,
        council,
        seat,
        &unread,
        next,
    ))
}

async fn react(
    host: &Host,
    call: &StepCall,
    council: &Council,
    message: &MessageId,
    seats: &[SeatId],
    table: &mut Table,
) -> EngineResult<()> {
    let talk = &call.at.step;
    let touched: Vec<u32> = match council.message(message) {
        Some(message) if message.kind != MessageKind::Notes => {
            message.stances.iter().map(|taken| taken.point).collect()
        }
        _ => Vec::new(),
    };
    let seats: Vec<&Seat> = seats.iter().filter_map(|id| council.seat(id)).collect();
    let mut texts = Vec::with_capacity(seats.len());
    for seat in &seats {
        texts.push(handed(host, table, council, &seat.id, YourMove::React)?);
    }
    // Each casts its tapback before it sees the others', so they show together.
    let asks = seats.iter().zip(texts).map(|(seat, text)| {
        let turn = Turn {
            seat,
            template: TAPBACK,
            access: Access::NoTools,
            text,
        };
        ask(host, call, council, table, turn)
    });
    let answers = join_all(asks).await;
    for (seat, answer) in seats.iter().zip(answers) {
        let answered = match answer {
            Ok(answered) => answered,
            Err(err) => {
                let turn = MissedTurn::Reaction {
                    message: message.clone(),
                };
                missed::missed(host, talk, council, &seat.id, turn, Some(TAPBACK_TIME), err)?;
                continue;
            }
        };
        table
            .sessions
            .insert(seat.id.clone(), answered.session.clone());
        let now = council_now(host, talk).unwrap_or_else(|| council.clone());
        let taken = take_turn(
            &table.relay,
            &now,
            &seat.id,
            &answered.output,
            Because::AsGiven,
        );
        let stance = points::tapback(&taken.latest, &touched);
        let taken = reasons_kept_private(host, &now, &seat.id, taken)?;
        record(
            host,
            talk,
            CouncilEventKind::Tapback {
                message: message.clone(),
                seat: seat.id.clone(),
                stance,
            },
        )?;
        record_taken(host, talk, &now, &seat.id, taken)?;
    }
    Ok(())
}

async fn answer_in_private(
    host: &Host,
    call: &StepCall,
    council: &Council,
    seat: &SeatId,
    table: &mut Table,
) -> EngineResult<()> {
    let talk = &call.at.step;
    let Some(sitting) = council.seat(seat) else {
        return Ok(());
    };
    let mut lines = Vec::new();
    for id in council.unread_private(seat) {
        if let Some(message) = council.private_words.get(id) {
            lines.push(private::turn(&table.relay.render_for(seat, message)));
        }
        record(
            host,
            talk,
            CouncilEventKind::Read {
                message: id.clone(),
                seat: seat.clone(),
            },
        )?;
    }
    let turn = Turn {
        seat: sitting,
        template: PRIVATE,
        access: Access::ReadOnly,
        text: lines.join("\n"),
    };
    let answered = ask(host, call, council, table, turn).await?;
    table
        .sessions
        .insert(seat.clone(), answered.session.clone());
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    let taken = take_turn(
        &table.relay,
        &now,
        seat,
        &answered.output,
        Because::InPrivate,
    );
    record_taken(host, talk, &now, seat, taken)
}

async fn write(
    host: &Host,
    call: &StepCall,
    council: &Council,
    seat: &SeatId,
    why: FloorReason,
    table: &mut Table,
) -> EngineResult<()> {
    let talk = &call.at.step;
    let Some(sitting) = council.seat(seat) else {
        return Ok(());
    };
    let rewriting = why == FloorReason::Rewrite;
    let challenge = match why {
        FloorReason::Unproven { point } => floor::weak(council, point).map(|weak| (point, weak)),
        _ => None,
    };
    record(
        host,
        talk,
        CouncilEventKind::Writing {
            seat: seat.clone(),
            why,
        },
    )?;
    let mut text = handed(host, table, council, seat, YourMove::Write)?;
    if rewriting {
        text.push_str(
            "\nYour last message repeated what the user told you in private, so no seat read it. \
             Write it again without that.",
        );
    }
    if let Some((point, weak)) = challenge {
        let ground = match weak {
            floor::Weak::InPrivate => {
                "on a reason no seat can read: a seat moved for what the user told it in private"
            }
            floor::Weak::NoEvidence => "with no evidence named: no file, test run or doc",
        };
        text.push_str(&format!(
            "\nPoint {point} is about to close {ground}. You are the skeptic: challenge it once \
             with a concrete risk, or agree if it holds."
        ));
    }
    let turn = Turn {
        seat: sitting,
        template: MESSAGE,
        access: Access::ReadOnly,
        text,
    };
    let answered = match ask(host, call, council, table, turn).await {
        Ok(answered) => answered,
        // Stopped for your message: it writes again with yours.
        Err(EngineError::Step(_))
            if host.settings().council.when_you_write == WhenYouWrite::Restart
                && yours_since_writing(host, talk, seat) =>
        {
            return record(
                host,
                talk,
                CouncilEventKind::WritingEnded { seat: seat.clone() },
            );
        }
        Err(err) => {
            return missed::missed(host, talk, council, seat, MissedTurn::Message, None, err);
        }
    };
    table
        .sessions
        .insert(seat.clone(), answered.session.clone());
    let (turn, taken) = taken_in(host, call, council, table, sitting, MESSAGE, answered).await?;
    let now = council_now(host, talk).unwrap_or_else(|| council.clone());
    land(
        host,
        &table.relay,
        &now,
        sitting,
        &turn,
        Landing::of(MessageKind::Talk, taken),
    )
}

/// Whether you wrote to the council, or to this seat alone, after it took the floor.
fn yours_since_writing(host: &Host, talk: &StepRef, seat: &SeatId) -> bool {
    council_now(host, talk).is_some_and(|council| {
        let since = council.last_output.get(seat).copied().unwrap_or(0);
        let to_all = council
            .messages
            .iter()
            .any(|said| said.message.from == Speaker::You && said.at > since);
        let to_it = council
            .private
            .iter()
            .any(|(to, _, at)| to == seat && *at > since);
        to_all || to_it
    })
}

/// Where a moved stance's reason comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Because {
    /// As the seat gave it: words it quotes, or what it read.
    AsGiven,
    /// From what you told it in private, which the others never read.
    InPrivate,
}

fn take_turn(
    relay: &Relay,
    council: &Council,
    seat: &SeatId,
    turn: &TalkTurn,
    because: Because,
) -> points::Taken {
    let offered: Vec<points::Offered> = turn
        .stances
        .iter()
        .map(|given| points::Offered {
            point: given.point,
            stance: given.stance,
            because: reason(relay, council, seat, given, because),
        })
        .collect();
    let reworded: Vec<(u32, String)> = turn
        .reword
        .iter()
        .map(|reworded| (reworded.point, held(relay, seat, &reworded.words)))
        .collect();
    let new_points: Vec<String> = turn
        .new_points
        .iter()
        .map(|words| held(relay, seat, words))
        .collect();
    points::take(
        &council.points,
        &council.stances,
        &council.table(),
        seat,
        &offered,
        &new_points,
        &reworded,
    )
}

/// Words a seat gave the council to keep, a point's or a side's, with each seat they name held
/// as the seat, so every reader reads its own letters. A letter that names no seat stays as
/// written.
fn held(relay: &Relay, writer: &SeatId, words: &str) -> String {
    let words = words.trim();
    relay
        .quote_in(writer, words)
        .unwrap_or_else(|_| words.to_owned())
}

fn reason(
    relay: &Relay,
    council: &Council,
    seat: &SeatId,
    given: &TalkStance,
    because: Because,
) -> Option<MovedBecause> {
    if because == Because::InPrivate {
        return Some(MovedBecause::InPrivate);
    }
    let filled = |text: &Option<String>| {
        text.as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    if let Some(quote) = filled(&given.quote) {
        let message = filled(&given.message)
            .and_then(|whom| answered(relay, council, seat, &whom, &quote))
            .map(|reply| reply.message);
        // Kept with its seats held as seats, so every reader reads them by its own letters.
        return Some(MovedBecause::Quote {
            message,
            words: held(relay, seat, &quote),
        });
    }
    filled(&given.because).map(|what| MovedBecause::Read { what })
}

/// The message a seat answers, by whom it names, "Seat T" or "the user", and the words it
/// quotes: the newest of theirs given to the seat that holds the quote word for word, else
/// their newest, with the quote left out. Nothing when no message of theirs reached the seat.
fn answered(
    relay: &Relay,
    council: &Council,
    reader: &SeatId,
    whom: &str,
    quote: &str,
) -> Option<ReplyTo> {
    let whom = whom.trim();
    let from = if matches!(whom.to_lowercase().as_str(), "the user" | "user") {
        Speaker::You
    } else {
        Speaker::Seat(named_seat(relay, reader, whom)?)
    };
    let theirs: Vec<&CouncilMessage> = council
        .messages
        .iter()
        .rev()
        .map(|said| &said.message)
        .filter(|message| message.from == from && council.given_to(&message.id, reader))
        .collect();
    let quote = relay
        .quote_in(reader, quote.trim())
        .ok()
        .filter(|quote| !quote.is_empty());
    if let Some(quote) = quote
        && let Some(message) = theirs
            .iter()
            .find(|message| relay.quote_matches(&quote, message))
    {
        return Some(ReplyTo {
            message: message.id.clone(),
            quote,
        });
    }
    theirs.first().map(|message| ReplyTo {
        message: message.id.clone(),
        quote: String::new(),
    })
}

/// What a seat's turn changed, in the order it's recorded: the points, then its stances and
/// each move. New words drop every stance on a point, so the seat's own come after.
fn turn_events(before: &Council, seat: &SeatId, taken: points::Taken) -> Vec<CouncilEventKind> {
    let mut events = Vec::new();
    if let Some(points) = taken.points.filter(|points| *points != before.points) {
        events.push(CouncilEventKind::Points { points });
    }
    events.push(CouncilEventKind::Stances {
        seat: seat.clone(),
        stances: taken.latest,
    });
    events.extend(taken.moved.into_iter().map(CouncilEventKind::Moved));
    events
}

/// Records what a seat's turn changed, as [`turn_events`] lists it.
fn record_taken(
    host: &Host,
    talk: &StepRef,
    before: &Council,
    seat: &SeatId,
    taken: points::Taken,
) -> EngineResult<()> {
    turn_events(before, seat, taken)
        .into_iter()
        .try_for_each(|event| record(host, talk, event))
}

/// A tapback has no message to hold, so a reason or quote in it that repeats your private
/// words to the seat reads as having come from you in private, never as written.
fn reasons_kept_private(
    host: &Host,
    council: &Council,
    seat: &SeatId,
    mut taken: points::Taken,
) -> EngineResult<points::Taken> {
    for moved in &mut taken.moved {
        let words = match &moved.because {
            MovedBecause::Quote { words, .. } => words,
            MovedBecause::Read { what } => what,
            MovedBecause::InPrivate => continue,
        };
        if private::held(host, council, seat, words)?.is_some() {
            moved.because = MovedBecause::InPrivate;
        }
    }
    Ok(taken)
}

/// Everything of a seat's turn another seat could read: its message, the words of each point
/// it added or changed with their sides and why, and the words and reasons it moved for.
fn turn_words(message: &CouncilMessage, before: &Council, turn: &[CouncilEventKind]) -> String {
    let mut said = vec![message.words(|_| String::new())];
    for event in turn {
        match event {
            CouncilEventKind::Points { points } => {
                for point in points.iter().filter(|point| !before.points.contains(point)) {
                    said.push(point.words.clone());
                    for side in &point.sides {
                        said.push(side.words.clone());
                        said.push(side.reason.clone());
                    }
                }
            }
            CouncilEventKind::Moved(moved) => match &moved.because {
                MovedBecause::Quote { words, .. } => said.push(words.clone()),
                MovedBecause::Read { what } => said.push(what.clone()),
                MovedBecause::InPrivate => {}
            },
            _ => {}
        }
    }
    said.join("\n")
}

/// How a seat's message lands: its kind, its words as code keeps them, and what its turn
/// records after it besides its stances, such as the note keeper's list.
struct Landing {
    kind: MessageKind,
    written: Taken,
    after: Vec<CouncilEventKind>,
}

impl Landing {
    fn of(kind: MessageKind, written: Taken) -> Self {
        Self {
            kind,
            written,
            after: Vec::new(),
        }
    }
}

/// A seat's message lands: its words as code keeps them, what it answers, the seats it names
/// and the stances it changed. A turn that repeats your private words anywhere, in its message,
/// a reason, a quote or a point, is held whole for you before any seat reads any of it.
fn land(
    host: &Host,
    relay: &Relay,
    council: &Council,
    seat: &Seat,
    turn: &TalkTurn,
    landing: Landing,
) -> EngineResult<()> {
    let Landing {
        kind,
        written,
        after,
    } = landing;
    let talk = &council.talk;
    let reply_to = turn.reply_to.as_deref().and_then(|whom| {
        let quote = turn.quote.as_deref().unwrap_or_default();
        answered(relay, council, &seat.id, whom, quote)
    });
    let taken = if kind == MessageKind::Talk {
        take_turn(relay, council, &seat.id, turn, Because::AsGiven)
    } else {
        points::Taken::default()
    };
    let id = MessageId::numbered(host.allocate(IdKind::Message)?);
    let message = CouncilMessage {
        id: id.clone(),
        from: Speaker::Seat(seat.id.clone()),
        kind,
        text: written.text,
        reply_to,
        mentions: written.mentions,
        stances: taken.changed.clone(),
        hidden: written.hidden,
        hidden_names: written.kinds,
        guesses_hidden: written.guesses_hidden,
        rewritten: written.rewritten,
        declined: written.declined,
        started_before: None,
    };
    let mut message = message;
    message.started_before = yours_unread_since_writing(council, &seat.id);
    let mut rest = if kind == MessageKind::Talk {
        turn_events(council, &seat.id, taken)
    } else {
        Vec::new()
    };
    rest.extend(after);
    let said = turn_words(&message, council, &rest);
    let held = private::held(host, council, &seat.id, &said)?;
    record(host, talk, CouncilEventKind::Message(Box::new(message)))?;
    match held {
        Some(hold) => record(
            host,
            talk,
            CouncilEventKind::Held(Box::new(Held {
                message: id,
                seat: seat.id.clone(),
                private: hold.private,
                why: hold.why,
                matching: hold.matching,
                turn: rest,
            })),
        ),
        None => rest
            .into_iter()
            .try_for_each(|event| record(host, talk, event)),
    }
}

/// Your newest message that came in after this seat took the floor and never reached it, which
/// its message was begun without.
fn yours_unread_since_writing(council: &Council, seat: &SeatId) -> Option<MessageId> {
    let writing = council.writing.as_ref().map(|(writing, _)| writing);
    if writing != Some(seat) {
        return None;
    }
    council
        .messages
        .iter()
        .rev()
        .filter(|said| said.at > council.writing_since)
        .find(|said| {
            said.message.from == Speaker::You
                && council
                    .read
                    .get(&said.message.id)
                    .is_none_or(|by| !by.contains_key(seat))
        })
        .map(|said| said.message.id.clone())
}

#[cfg(test)]
pub(crate) mod tests {
    use argonauts_core::base::names::WorkflowId;
    use argonauts_core::look::display_names::DisplayNames;
    use argonauts_core::runs::events::council::{SeatRole, TextPart};

    use super::*;

    pub(crate) fn seat(n: u32) -> SeatId {
        SeatId::numbered(n)
    }

    pub(crate) fn talk() -> StepRef {
        StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 1,
            path: vec![0],
        }
    }

    pub(crate) fn seated(count: u32) -> CouncilEventKind {
        CouncilEventKind::Seated(Seating {
            seats: (1..=count)
                .map(|n| Seat {
                    id: seat(n),
                    model: "claude-code/opus".parse().unwrap(),
                    role: SeatRole::Proposer,
                    number: Some(u8::try_from(n).unwrap()),
                })
                .collect(),
            notes: Some(seat(1)),
        })
    }

    pub(crate) fn said(id: u32, from: Speaker, kind: MessageKind, words: &str) -> CouncilMessage {
        CouncilMessage {
            id: MessageId::numbered(id),
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

    pub(crate) fn from_seat(id: u32, n: u32, words: &str) -> CouncilEventKind {
        CouncilEventKind::Message(Box::new(said(
            id,
            Speaker::Seat(seat(n)),
            MessageKind::Talk,
            words,
        )))
    }

    pub(crate) fn council(events: impl IntoIterator<Item = CouncilEventKind>) -> Council {
        let mut councils = Councils::default();
        for kind in events {
            councils.apply(&CouncilEvent { talk: talk(), kind });
        }
        councils.get(&talk()).cloned().unwrap()
    }

    fn next_of(events: Vec<CouncilEventKind>) -> Next {
        let limits = Limits::of(
            &argonauts_core::settings::council::TalkStop::default(),
            None,
        );
        next_move(&council(events), &Spent::default(), &limits)
    }

    fn first(id: u32, n: u32) -> CouncilEventKind {
        CouncilEventKind::Message(Box::new(said(
            id,
            Speaker::Seat(seat(n)),
            MessageKind::FirstAnswer,
            "Alone.",
        )))
    }

    fn notes(id: u32) -> CouncilEventKind {
        CouncilEventKind::Message(Box::new(said(
            id,
            Speaker::Seat(seat(1)),
            MessageKind::Notes,
            "Two points.",
        )))
    }

    fn listed() -> CouncilEventKind {
        CouncilEventKind::Points {
            points: points::listed([("A token bucket".to_owned(), Vec::new())]),
        }
    }

    fn tapback(message: u32, n: u32, stance: Stance) -> CouncilEventKind {
        CouncilEventKind::Tapback {
            message: MessageId::numbered(message),
            seat: seat(n),
            stance,
        }
    }

    fn stances(n: u32, stance: Stance) -> CouncilEventKind {
        CouncilEventKind::Stances {
            seat: seat(n),
            stances: vec![PointStance { point: 1, stance }],
        }
    }

    #[test]
    fn the_talk_goes_first_answers_then_notes_then_reactions_then_the_floor() {
        assert_eq!(
            next_of(vec![seated(2), first(1, 2)]),
            Next::FirstAnswers(vec![seat(1)])
        );
        let answered = vec![seated(2), first(1, 1), first(2, 2)];
        assert_eq!(next_of(answered.clone()), Next::ListPoints(seat(1)));

        let mut listed_out = answered;
        listed_out.extend([notes(3), listed()]);
        assert_eq!(
            next_of(listed_out.clone()),
            Next::React {
                message: MessageId::numbered(3),
                seats: vec![seat(1), seat(2)],
            },
            "the note keeper reacts to its own list too"
        );

        listed_out.extend([
            tapback(3, 1, Stance::Agree),
            stances(1, Stance::Agree),
            tapback(3, 2, Stance::Object),
            stances(2, Stance::Object),
        ]);
        assert_eq!(
            next_of(listed_out.clone()),
            Next::Write(seat(2), FloorReason::Objected)
        );

        let mut agreed = listed_out;
        agreed.push(stances(2, Stance::Agree));
        agreed.push(CouncilEventKind::Points {
            points: vec![Point {
                state: PointState::Agreed,
                ..points::listed([("A token bucket".to_owned(), Vec::new())]).remove(0)
            }],
        });
        assert_eq!(next_of(agreed.clone()), Next::Agreed);
        agreed.push(CouncilEventKind::Agreed { points: 1 });
        assert_eq!(next_of(agreed.clone()), Next::Plan(seat(1)));
        agreed.push(CouncilEventKind::Message(Box::new(said(
            9,
            Speaker::Seat(seat(1)),
            MessageKind::Notes,
            "The plan.",
        ))));
        assert_eq!(next_of(agreed), Next::Done);
    }

    #[test]
    fn a_held_message_and_your_private_words_come_before_the_floor() {
        let mut events = vec![
            seated(2),
            first(1, 1),
            first(2, 2),
            notes(3),
            listed(),
            tapback(3, 1, Stance::Object),
            tapback(3, 2, Stance::Object),
        ];
        events.push(CouncilEventKind::Private {
            seat: seat(2),
            message: MessageId::numbered(4),
        });
        events.push(CouncilEventKind::Message(Box::new(said(
            4,
            Speaker::You,
            MessageKind::Talk,
            "Between us: keep the window.",
        ))));
        let talk = council(events.clone());
        assert!(talk.unread(&seat(1)).iter().all(|m| m.from != Speaker::You));
        assert_eq!(talk.unread_private(&seat(2)).len(), 1);
        assert_eq!(next_of(events.clone()), Next::Private(seat(2)));

        events.push(CouncilEventKind::Read {
            message: MessageId::numbered(4),
            seat: seat(2),
        });
        events.push(stances(2, Stance::Agree));
        events.push(from_seat(5, 2, "Keep the window."));
        events.push(CouncilEventKind::Held(Box::new(Held {
            message: MessageId::numbered(5),
            seat: seat(2),
            private: MessageId::numbered(4),
            why: argonauts_core::runs::events::council::HeldWhy::WordsInARow { count: 6 },
            matching: Vec::new(),
            turn: Vec::new(),
        })));
        assert!(matches!(next_of(events.clone()), Next::Held(_)));
        events.push(CouncilEventKind::Released {
            message: MessageId::numbered(5),
            choice: HeldChoice::Rewrite,
        });
        assert_eq!(
            next_of(events.clone()),
            Next::Write(seat(2), FloorReason::Rewrite)
        );
        events.push(CouncilEventKind::Shared {
            message: MessageId::numbered(4),
        });
        let shared = council(events);
        let unread: Vec<&MessageId> = shared.unread(&seat(1)).iter().map(|m| &m.id).collect();
        assert_eq!(
            unread,
            [&MessageId::numbered(2), &MessageId::numbered(4)],
            "a shared message reaches every seat, and a held one sent back to be written again none"
        );
    }

    /// A turn counts what its seat wrote, never the context its CLI read again: one turn that
    /// read 373k tokens of it moves the talk's count by its 900 written.
    #[test]
    fn a_turn_counts_the_tokens_its_seat_wrote() {
        let spend = Spend {
            input_tokens: 373_000,
            cached_tokens: 12_000,
            output_tokens: 900,
            ..Spend::default()
        };
        assert_eq!(talk_tokens(&spend), 900);
    }

    #[test]
    fn the_stop_guard_ends_a_talk_that_moves_nobody() {
        let mut events = vec![
            seated(2),
            first(1, 1),
            first(2, 2),
            notes(3),
            listed(),
            tapback(3, 1, Stance::Agree),
            tapback(3, 2, Stance::Object),
        ];
        for id in 4..10 {
            events.push(from_seat(id, 1 + id % 2, "Still."));
        }
        assert_eq!(
            next_of(events.clone()),
            Next::Stop(TalkStop::MovedNobody { messages: 6 })
        );
        events.push(CouncilEventKind::Stopped {
            why: TalkStop::MovedNobody { messages: 6 },
        });
        assert_eq!(next_of(events.clone()), Next::Settle);

        // You let it talk on: new limits reopen it, and what moved nobody counts afresh.
        events.push(CouncilEventKind::Limits {
            tokens: 300_000,
            minutes: 23,
        });
        let talking_on = council(events.clone());
        assert_eq!(talking_on.ending, None);
        assert_eq!(talking_on.unmoved, 0);
        assert!(matches!(next_of(events), Next::Write(..)));
    }

    #[test]
    fn a_message_a_seat_was_handed_counts_as_read_once_its_turn_ends() {
        let mut events = vec![seated(2), from_seat(1, 1, "A bucket per key.")];
        events.push(CouncilEventKind::Read {
            message: MessageId::numbered(1),
            seat: seat(2),
        });
        let handed = council(events.clone());
        // Its turn never ended, so the message goes again.
        assert_eq!(handed.unread(&seat(2)).len(), 1);

        events.push(from_seat(2, 2, "A window is simpler."));
        let answered = council(events);
        assert!(answered.unread(&seat(2)).is_empty());
        assert_eq!(answered.unread(&seat(1)).len(), 1, "seat 1 hasn't read 2's");
        assert_eq!(answered.spoken(&seat(2)), 1);
        assert!(answered.last_spoke(&seat(2)) > answered.last_spoke(&seat(1)));
    }

    #[test]
    fn a_held_message_reaches_no_seat_until_you_let_it() {
        let held = Held {
            message: MessageId::numbered(1),
            seat: seat(1),
            private: MessageId::numbered(9),
            why: argonauts_core::runs::events::council::HeldWhy::RareWord {
                word: "acme-billing".to_owned(),
            },
            matching: vec!["acme-billing".to_owned()],
            turn: Vec::new(),
        };
        let mut events = vec![
            seated(2),
            from_seat(1, 1, "Use acme-billing's limiter."),
            CouncilEventKind::Held(Box::new(held)),
        ];
        let council_now = council(events.clone());
        assert!(council_now.unread(&seat(2)).is_empty());
        assert!(council_now.waiting_on_you().is_some());

        events.push(CouncilEventKind::Released {
            message: MessageId::numbered(1),
            choice: HeldChoice::AsWritten,
        });
        let released = council(events);
        assert_eq!(released.unread(&seat(2)).len(), 1);
        assert!(released.waiting_on_you().is_none());
    }

    #[test]
    fn messages_that_move_nobody_are_counted_until_a_stance_moves_or_you_write() {
        let mut events = vec![seated(2)];
        for id in 1..=3 {
            events.push(from_seat(id, 1 + id % 2, "Still a window."));
        }
        assert_eq!(council(events.clone()).unmoved, 3);
        events.push(CouncilEventKind::Message(Box::new(said(
            4,
            Speaker::You,
            MessageKind::Talk,
            "Keep /health open.",
        ))));
        assert_eq!(council(events.clone()).unmoved, 0);
        events.push(from_seat(5, 2, "Agreed on /health."));
        events.push(CouncilEventKind::Moved(Moved {
            seat: seat(2),
            point: 1,
            to: "A bucket".to_owned(),
            because: argonauts_core::runs::events::council::MovedBecause::InPrivate,
        }));
        assert_eq!(council(events).unmoved, 0);
    }

    /// A seat that rewords a point each message moves nobody, so its talk still stops.
    #[test]
    fn rewording_a_point_moves_nobody() {
        let worded = |words: &str| CouncilEventKind::Points {
            points: vec![Point {
                number: 1,
                words: words.to_owned(),
                state: PointState::Open,
                sides: Vec::new(),
                raised_by: None,
            }],
        };
        let mut events = vec![seated(2), worded("A bucket per key")];
        for (id, words) in [(1, "A bucket per API key"), (2, "One bucket per API key")] {
            events.push(from_seat(id, 1, "Better words."));
            events.push(worded(words));
        }
        assert_eq!(council(events).unmoved, 2);
    }

    #[test]
    fn a_seat_carrying_on_alone_leaves_the_table_until_it_rejoins() {
        let mut events = vec![seated(3), CouncilEventKind::SatOut { seat: seat(2) }];
        assert_eq!(council(events.clone()).table(), [seat(1), seat(3)]);
        events.push(CouncilEventKind::Rejoined {
            seat: seat(2),
            missed: 2,
        });
        assert_eq!(council(events).table(), [seat(1), seat(2), seat(3)]);
    }

    /// A seat names whom it answers and quotes their words; code finds the message they're from.
    #[test]
    fn a_reply_finds_the_message_by_whom_it_names_and_the_words_it_quotes() {
        let read = |message: u32| CouncilEventKind::Read {
            message: MessageId::numbered(message),
            seat: seat(2),
        };
        let talk = council([
            seated(3),
            from_seat(1, 1, "Use a bucket per key."),
            from_seat(2, 1, "And keep the counters in Redis."),
            from_seat(3, 3, "A window is one INCR."),
            read(1),
            read(2),
        ]);
        let key = DrawKey {
            secret: "made-up-secret".to_owned(),
            run: "run-1".to_owned(),
            council: "1/1/0".to_owned(),
        };
        let relay = deliver::seated_relay(
            &DisplayNames::default(),
            &talk,
            &key.draw(),
            crate::council::lexicon::Lexicon::built_in(),
        )
        .unwrap();
        let one = deliver::letter(&relay, &seat(2), &seat(1)).unwrap();
        let three = deliver::letter(&relay, &seat(2), &seat(3)).unwrap();
        let reply = |whom: &str, quote: &str| answered(&relay, &talk, &seat(2), whom, quote);

        let quoted = reply(&format!("Seat {one}"), "a bucket per key").unwrap();
        assert_eq!(quoted.message, MessageId::numbered(1));
        assert_eq!(quoted.quote, "a bucket per key");
        // Words it never wrote leave the quote out, and the reply goes to their newest message.
        let unquoted = reply(&format!("Seat {one}"), "a sliding log").unwrap();
        assert_eq!(unquoted.message, MessageId::numbered(2));
        assert_eq!(unquoted.quote, "");
        assert_eq!(
            reply(&format!("{three}"), "one INCR"),
            None,
            "nothing from that seat reached it yet"
        );
        assert_eq!(reply("the user", "anything"), None);
    }

    #[test]
    fn the_talking_council_is_the_one_not_ended() {
        let mut councils = Councils::default();
        councils.apply(&CouncilEvent {
            talk: talk(),
            kind: seated(2),
        });
        assert!(councils.talking().is_some());
        councils.apply(&CouncilEvent {
            talk: talk(),
            kind: CouncilEventKind::Agreed { points: 0 },
        });
        assert!(councils.talking().is_none());
        assert!(councils.newest().is_some());
    }
}
