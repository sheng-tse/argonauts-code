//! A council's talk as its request's rows show it: how many seats back every point, and where
//! each seat stands and what it does now.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::base::names::{MessageId, SeatId};
use argonauts_core::runs::events::council::{
    CouncilEvent, CouncilEventKind, FloorReason, MessageKind, PointState, Seat, Side, Speaker,
    Stance,
};
use argonauts_core::workflows::workflow::StepRef;

/// The run's latest talk: who sits, where each point stands, each seat's latest stance, and
/// what each seat does now.
#[derive(Debug, Clone, PartialEq)]
pub struct Talk {
    /// The talk step whose council it is.
    pub step: StepRef,
    /// Each seat as it sat down, in seat order.
    seats: Vec<Seat>,
    /// The seat that lists the points and writes the plan.
    notes: Option<SeatId>,
    /// Each point by its number.
    points: BTreeMap<u32, PointState>,
    /// The sides of each point still open, with the seats that back each.
    sides: BTreeMap<u32, Vec<Side>>,
    /// Each seat's latest stance on each point it has taken one on.
    stances: BTreeMap<SeatId, BTreeMap<u32, Stance>>,
    agreed: bool,
    /// The seat code gave the floor and why, until its message lands or it stops.
    writing: Option<(SeatId, FloorReason)>,
    /// Who wrote the latest message, whom a seat that objects to it answers.
    last_from: Option<Speaker>,
    /// The seats whose first answer is in.
    answered: BTreeSet<SeatId>,
    /// The note keeper listed the points from the first answers. Your points bind from the
    /// start, but nobody stands on them before the first answers are in.
    listed: bool,
    /// The note keeper's notes came in after the talk agreed: the plan and who does what.
    plan_written: bool,
    /// Messages held for you, by whose they are, until you choose what happens.
    held: BTreeMap<MessageId, SeatId>,
    /// Seats carried on in their own CLI, out of the talk until they come back.
    out: BTreeSet<SeatId>,
    /// Seats that missed their latest turn, `true` once they sit out the rest of the talk.
    missed: BTreeMap<SeatId, bool>,
}

/// What a seat does now, as its row under the request says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatDoing {
    /// It writes its first answer alone.
    FirstAnswer,
    /// Its first answer is in, and the points aren't listed yet.
    Answered,
    /// It keeps the notes and lists the points from the first answers.
    GatheringPoints,
    /// It answers you, or `Some` seat.
    Answering(Option<SeatId>),
    /// It writes for another reason: it had been quiet longest, or a point needs evidence.
    Writing,
    /// It writes its last message again.
    Rewriting,
    /// The talk agreed, and it keeps the notes: it writes the plan and who does what.
    WritingPlan,
    /// It wrote the plan.
    WrotePlan,
    /// It waits its turn, where its stances say it stands.
    Standing,
    /// It carries on in its own CLI and sits out the talk.
    CarriedOn,
    /// Its turn ran out or failed, so it sat that turn out.
    SatTurnOut,
    /// It failed twice and sits out the rest of the talk.
    SitsOut,
}

/// One seat of the talk as its row shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatNow<'a> {
    pub seat: &'a Seat,
    /// The points it backs, of all of them; none are listed before the first answers.
    pub backs: (u32, u32),
    pub doing: SeatDoing,
    /// A message of its waits on you.
    pub held: bool,
    /// The plan is written, and the seat backs every point it was built from.
    pub signed: bool,
}

impl Talk {
    /// The seat whose message is held for you under this id.
    pub fn held_by(&self, message: &MessageId) -> Option<&SeatId> {
        self.held.get(message)
    }

    /// Follows one moment of a council. A council sitting down starts a fresh talk, and the
    /// rest of a talk that is no longer the latest is left out.
    pub(super) fn follow(talk: &mut Option<Talk>, event: &CouncilEvent) {
        if let CouncilEventKind::Seated(seating) = &event.kind {
            *talk = Some(Talk {
                step: event.talk.clone(),
                seats: seating.seats.clone(),
                notes: seating.notes.clone(),
                points: BTreeMap::new(),
                sides: BTreeMap::new(),
                stances: BTreeMap::new(),
                agreed: false,
                writing: None,
                last_from: None,
                answered: BTreeSet::new(),
                listed: false,
                plan_written: false,
                held: BTreeMap::new(),
                out: BTreeSet::new(),
                missed: BTreeMap::new(),
            });
            return;
        }
        let Some(talk) = talk.as_mut().filter(|talk| talk.step == event.talk) else {
            return;
        };
        match &event.kind {
            CouncilEventKind::Points { points } => {
                talk.points = points
                    .iter()
                    .map(|point| (point.number, point.state))
                    .collect();
                talk.sides = points
                    .iter()
                    .map(|point| (point.number, point.sides.clone()))
                    .collect();
            }
            CouncilEventKind::Stances { seat, stances } => {
                talk.back_from_a_missed_turn(seat);
                let latest = talk.stances.entry(seat.clone()).or_default();
                for stance in stances {
                    latest.insert(stance.point, stance.stance);
                }
            }
            CouncilEventKind::Agreed { .. } => talk.agreed = true,
            CouncilEventKind::Writing { seat, why } => {
                talk.back_from_a_missed_turn(seat);
                talk.writing = Some((seat.clone(), why.clone()));
            }
            CouncilEventKind::WritingEnded { seat } => talk.stop_writing(seat),
            CouncilEventKind::Message(message) => {
                if let Speaker::Seat(seat) = &message.from {
                    talk.stop_writing(seat);
                    match message.kind {
                        MessageKind::FirstAnswer => {
                            talk.answered.insert(seat.clone());
                        }
                        MessageKind::Notes if talk.agreed => talk.plan_written = true,
                        MessageKind::Notes => talk.listed = true,
                        _ => {}
                    }
                }
                talk.last_from = Some(message.from.clone());
            }
            CouncilEventKind::Held(held) => {
                talk.held.insert(held.message.clone(), held.seat.clone());
            }
            CouncilEventKind::Released { message, .. } => {
                talk.held.remove(message);
            }
            CouncilEventKind::SatOut { seat } => {
                talk.out.insert(seat.clone());
            }
            CouncilEventKind::Rejoined { seat, .. } => {
                talk.out.remove(seat);
            }
            CouncilEventKind::Missed(missed) => {
                talk.stop_writing(&missed.seat);
                let for_good = talk.missed.entry(missed.seat.clone()).or_default();
                *for_good |= missed.for_good;
            }
            _ => {}
        }
    }

    fn back_from_a_missed_turn(&mut self, seat: &SeatId) {
        if self.missed.get(seat) == Some(&false) {
            self.missed.remove(seat);
        }
    }

    /// The seats that missed two turns and sit out the rest of the talk, in seat order.
    pub fn sat_out(&self) -> Vec<SeatId> {
        self.seats
            .iter()
            .filter(|seat| self.missed.get(&seat.id) == Some(&true))
            .map(|seat| seat.id.clone())
            .collect()
    }

    /// The seats still in the talk: a seat that sits out the rest no longer counts.
    fn sitting(&self) -> impl Iterator<Item = &Seat> {
        self.seats
            .iter()
            .filter(|seat| self.missed.get(&seat.id) != Some(&true))
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

    /// The seats that back every point, of all its seats, so a talk that agrees ends at 4/4:
    /// the seats the panel marks ✔ and the working line's "2/4 seats agree". Before the first
    /// list of points nobody backs anything yet.
    pub fn backing(&self) -> (u32, u32) {
        let backing = self
            .sitting()
            .filter(|seat| self.backs_every_point(&seat.id))
            .count();
        (count(backing), count(self.sitting().count()))
    }

    /// The seat `id` as it sat down.
    pub fn seat(&self, id: &SeatId) -> Option<&Seat> {
        self.seats.iter().find(|seat| seat.id == *id)
    }

    /// The sides of point `number`, with the seats that back each; none once it is settled.
    pub fn sides(&self, number: u32) -> &[Side] {
        self.sides.get(&number).map_or(&[], Vec::as_slice)
    }

    /// The plan is written and every seat but the one that wrote it signed it: each backs every
    /// point the plan was built from.
    pub fn every_seat_signed(&self) -> bool {
        self.plan_written
            && self
                .sitting()
                .filter(|seat| self.notes.as_ref() != Some(&seat.id))
                .all(|seat| self.backs_every_point(&seat.id))
    }

    /// Each seat in seat order: where it stands and what it does now.
    pub fn seats(&self) -> impl Iterator<Item = SeatNow<'_>> {
        self.seats.iter().map(|seat| SeatNow {
            seat,
            backs: (self.points_backed(&seat.id), self.listed_points()),
            doing: self.doing(&seat.id),
            held: self.held.values().any(|held| *held == seat.id),
            signed: self.plan_written
                && self.notes.as_ref() != Some(&seat.id)
                && self.backs_every_point(&seat.id),
        })
    }

    fn doing(&self, seat: &SeatId) -> SeatDoing {
        let keeps_notes = self.notes.as_ref() == Some(seat);
        if self.out.contains(seat) {
            return SeatDoing::CarriedOn;
        }
        match self.missed.get(seat) {
            Some(true) => return SeatDoing::SitsOut,
            Some(false) => return SeatDoing::SatTurnOut,
            None => {}
        }
        if keeps_notes && self.plan_written {
            return SeatDoing::WrotePlan;
        }
        if let Some((_, why)) = self.writing.as_ref().filter(|(writing, _)| writing == seat) {
            return match why {
                FloorReason::FirstAnswer => SeatDoing::FirstAnswer,
                FloorReason::You => SeatDoing::Answering(None),
                FloorReason::RepliedTo { by } | FloorReason::Named { by } => {
                    SeatDoing::Answering(Some(by.clone()))
                }
                FloorReason::Objected => match &self.last_from {
                    Some(Speaker::Seat(by)) if by != seat => SeatDoing::Answering(Some(by.clone())),
                    Some(Speaker::You) => SeatDoing::Answering(None),
                    _ => SeatDoing::Writing,
                },
                FloorReason::Notes if self.agreed => SeatDoing::WritingPlan,
                FloorReason::Notes => SeatDoing::GatheringPoints,
                FloorReason::Rewrite => SeatDoing::Rewriting,
                FloorReason::Quietest | FloorReason::Unproven { .. } => SeatDoing::Writing,
            };
        }
        if self.agreed && keeps_notes {
            return SeatDoing::WritingPlan;
        }
        if !self.listed {
            return if !self.answered.contains(seat) {
                SeatDoing::FirstAnswer
            } else if keeps_notes && self.answered.len() == self.seats.len() {
                SeatDoing::GatheringPoints
            } else {
                SeatDoing::Answered
            };
        }
        SeatDoing::Standing
    }

    /// The points the seats stand on: none before the note keeper lists them. Yours bind
    /// whoever objects, so they aren't counted, and a seat that objects to one still signs.
    fn listed_points(&self) -> u32 {
        if self.listed || self.agreed {
            count(self.counted().count())
        } else {
            0
        }
    }

    fn counted(&self) -> impl Iterator<Item = u32> + '_ {
        self.points
            .iter()
            .filter(|(_, state)| **state != PointState::Yours)
            .map(|(number, _)| *number)
    }

    /// How many points `seat` backs: once the talk agrees, all of them.
    fn points_backed(&self, seat: &SeatId) -> u32 {
        if self.agreed {
            return self.listed_points();
        }
        if !self.listed {
            return 0;
        }
        let backed = self
            .counted()
            .filter(|number| self.backs_point(seat, *number))
            .count();
        count(backed)
    }

    fn backs_point(&self, seat: &SeatId, number: u32) -> bool {
        let Some(state) = self.points.get(&number) else {
            return false;
        };
        let stance = self
            .stances
            .get(seat)
            .and_then(|stances| stances.get(&number));
        backs(
            state,
            stance == Some(&Stance::Agree),
            stance == Some(&Stance::Object),
        )
    }

    fn backs_every_point(&self, seat: &SeatId) -> bool {
        self.agreed
            || (self.listed_points() > 0 && self.points_backed(seat) == self.listed_points())
    }
}

/// Whether a seat backs a point, the one rule every council count follows: a settled point is
/// everyone's, yours binds unless the seat objects, and an open one needs the seat to agree.
pub fn backs(state: &PointState, agrees: bool, objects: bool) -> bool {
    match state {
        PointState::Open => agrees,
        PointState::Yours => !objects,
        PointState::Agreed | PointState::Settled | PointState::Voted => true,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
