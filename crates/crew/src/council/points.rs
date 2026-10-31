//! The points a council agrees on, and agreement counted from each seat's stances.
//!
//! Agreement is code's to count, never a model's to claim: a point is agreed once every seat at
//! the table has agree as its latest stance on the point's current wording. Your points bind
//! whatever the stances say, and an objection to one stays on record beside it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use argonauts_core::base::names::SeatId;
use argonauts_core::runs::events::approval::{
    AnswerAction, ApprovalKind, ApprovalPacket, CouncilPacket, CouncilQuestion, FreeText,
};
use argonauts_core::runs::events::council::{
    CouncilEventKind, Moved, MovedBecause, Point, PointStance, PointState, Side, Stance,
};
use argonauts_core::runs::events::run::Party;
use argonauts_core::safety::policy::EvidenceFacts;
use argonauts_core::settings::council::Unsettled;

use super::guards::TalkOn;
use super::talk::{self, Council, record};
use super::{relay, seats};
use crate::approvals::{self, ApprovalRequest, autopilot, options};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::workflow::StepCall;

/// Each seat's latest stance on each point, by point number.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stances(BTreeMap<SeatId, BTreeMap<u32, Stance>>);

/// One stance that changed with a seat's newest message or tapback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    pub point: u32,
    pub was: Option<Stance>,
    pub now: Stance,
}

impl Change {
    /// A seat that moved: it had a stance on the point and now holds another. A first stance
    /// moves nobody, since the seat had none to move from.
    pub fn moved(&self) -> bool {
        self.was.is_some_and(|was| was != self.now)
    }
}

impl Stances {
    pub fn of(&self, seat: &SeatId, point: u32) -> Option<Stance> {
        self.0.get(seat)?.get(&point).copied()
    }

    /// The seats whose latest stance on a point is agree, in seat order.
    pub fn backing(&self, point: u32) -> Vec<SeatId> {
        self.0
            .iter()
            .filter(|(_, stances)| stances.get(&point) == Some(&Stance::Agree))
            .map(|(seat, _)| seat.clone())
            .collect()
    }

    /// A seat's latest stance on every point it has taken one on, in point order.
    pub fn latest(&self, seat: &SeatId) -> Vec<PointStance> {
        self.0.get(seat).map_or_else(Vec::new, |stances| {
            stances
                .iter()
                .map(|(point, stance)| PointStance {
                    point: *point,
                    stance: *stance,
                })
                .collect()
        })
    }

    /// Takes a seat's newest stances, and says which of them changed.
    pub fn take(&mut self, seat: &SeatId, stances: &[PointStance]) -> Vec<Change> {
        let held = self.0.entry(seat.clone()).or_default();
        let mut changes = Vec::new();
        for taken in stances {
            let was = held.insert(taken.point, taken.stance);
            if was != Some(taken.stance) {
                changes.push(Change {
                    point: taken.point,
                    was,
                    now: taken.stance,
                });
            }
        }
        changes
    }

    /// A seat's whole set of latest stances, as a `Stances` event records it.
    pub fn replace(&mut self, seat: &SeatId, stances: &[PointStance]) {
        let held = stances
            .iter()
            .map(|taken| (taken.point, taken.stance))
            .collect();
        self.0.insert(seat.clone(), held);
    }

    /// Forgets every stance on a point whose wording changed: each seat takes one again on the
    /// new words.
    pub fn forget(&mut self, point: u32) {
        for held in self.0.values_mut() {
            held.remove(&point);
        }
    }
}

/// The state a point stands in with these seats at the table. A point you made, or one the
/// talk's end settled, keeps its state.
pub fn state(point: &Point, stances: &Stances, table: &[SeatId]) -> PointState {
    match point.state {
        PointState::Yours | PointState::Settled | PointState::Voted => point.state,
        PointState::Open | PointState::Agreed => {
            let everyone = !table.is_empty()
                && table
                    .iter()
                    .all(|seat| stances.of(seat, point.number) == Some(Stance::Agree));
            if everyone {
                PointState::Agreed
            } else {
                PointState::Open
            }
        }
    }
}

/// Counts every point again, and says whether any point's state changed.
pub fn recount(points: &mut [Point], stances: &Stances, table: &[SeatId]) -> bool {
    let mut changed = false;
    for point in points.iter_mut() {
        let now = state(point, stances, table);
        if now != point.state {
            point.state = now;
            changed = true;
        }
    }
    changed
}

/// Nothing is left open: every point is agreed, yours, or settled when the talk stopped.
pub fn all_settled(points: &[Point]) -> bool {
    points.iter().all(|point| point.state != PointState::Open)
}

/// How many points are settled, of how many: "4 of 6 points agreed".
pub fn agreed_of(points: &[Point]) -> (usize, usize) {
    let settled = points
        .iter()
        .filter(|point| point.state != PointState::Open)
        .count();
    (settled, points.len())
}

/// A new open point, numbered after the others.
pub fn add(points: &mut Vec<Point>, words: &str) -> u32 {
    let number = next_number(points);
    points.push(Point {
        number,
        words: words.trim().to_owned(),
        state: PointState::Open,
        sides: Vec::new(),
        raised_by: None,
    });
    number
}

/// A point of yours, which binds from the moment you make it.
pub fn add_yours(points: &mut Vec<Point>, words: &str) -> u32 {
    let number = add(points, words);
    if let Some(point) = points.last_mut() {
        point.state = PointState::Yours;
    }
    number
}

/// Words a seat gave a point, which only an open or agreed point takes: yours change only when
/// you change them. `None` when nothing changed.
pub fn reword(points: &mut [Point], number: u32, words: &str) -> Option<u32> {
    let point = points.iter_mut().find(|point| point.number == number)?;
    let words = words.trim();
    let open = matches!(point.state, PointState::Open | PointState::Agreed);
    if !open || words.is_empty() || point.words == words {
        return None;
    }
    point.words = words.to_owned();
    point.state = PointState::Open;
    point.sides.clear();
    Some(number)
}

/// The note keeper's list after the first answers: each point with its sides, numbered from 1.
pub fn listed(listing: impl IntoIterator<Item = (String, Vec<Side>)>) -> Vec<Point> {
    let mut points = Vec::new();
    for (words, sides) in listing {
        if words.trim().is_empty() {
            continue;
        }
        add(&mut points, &words);
        if let Some(point) = points.last_mut() {
            point.sides = sides;
        }
    }
    points
}

/// Whether the council has a point by this number, as a seat cites it.
pub fn known(points: &[Point], number: u32) -> bool {
    points.iter().any(|point| point.number == number)
}

/// A stance a seat offered in its turn, with what moved it when it moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offered {
    pub point: u32,
    pub stance: Stance,
    pub because: Option<MovedBecause>,
}

/// A seat's turn as code takes it: the stances that count, who moved, and the points after.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Taken {
    /// The stances this turn took for the first time or changed, which its message records.
    pub changed: Vec<PointStance>,
    /// The seat's whole set of latest stances after the turn.
    pub latest: Vec<PointStance>,
    /// Each stance that moved, with its reason, for the centred line.
    pub moved: Vec<Moved>,
    /// The points after the turn, when the turn changed any.
    pub points: Option<Vec<Point>>,
    pub stances: Stances,
}

/// Takes one seat's turn: its new points and new words first, which it backs, then its
/// stances. A seat that changes its mind must say why, so a changed stance with no reason
/// isn't counted, and a stance on a point the council doesn't have is dropped.
pub fn take(
    before: &[Point],
    stances: &Stances,
    table: &[SeatId],
    seat: &SeatId,
    offered: &[Offered],
    new_points: &[String],
    reworded: &[(u32, String)],
) -> Taken {
    let mut points = before.to_vec();
    let mut stances = stances.clone();
    let mut backs = Vec::new();
    for (number, words) in reworded {
        if reword(&mut points, *number, words).is_some() {
            stances.forget(*number);
            backs.push(*number);
        }
    }
    for words in new_points.iter().filter(|words| !words.trim().is_empty()) {
        backs.push(add(&mut points, words));
    }
    for point in points
        .iter_mut()
        .filter(|point| backs.contains(&point.number))
    {
        point.raised_by = Some(seat.clone());
    }
    let mut accepted: Vec<&Offered> = Vec::new();
    for offer in offered {
        if !known(&points, offer.point) || accepted.iter().any(|a| a.point == offer.point) {
            continue;
        }
        let was = stances.of(seat, offer.point);
        let changes_mind = was.is_some_and(|was| was != offer.stance);
        if changes_mind && offer.because.is_none() {
            continue;
        }
        accepted.push(offer);
    }
    let mut taking: Vec<PointStance> = backs
        .iter()
        .filter(|number| !accepted.iter().any(|offer| offer.point == **number))
        .map(|number| PointStance {
            point: *number,
            stance: Stance::Agree,
        })
        .collect();
    taking.extend(accepted.iter().map(|offer| PointStance {
        point: offer.point,
        stance: offer.stance,
    }));
    let changes = stances.take(seat, &taking);
    let moved = changes
        .iter()
        .filter(|change| change.moved())
        .filter_map(|change| {
            let because = accepted
                .iter()
                .find(|offer| offer.point == change.point)?
                .because
                .clone()?;
            let point = points.iter().find(|point| point.number == change.point)?;
            Some(Moved {
                seat: seat.clone(),
                point: change.point,
                to: side_words(&point.words, change.now),
                because,
            })
        })
        .collect();
    recount(&mut points, &stances, table);
    Taken {
        changed: changes
            .iter()
            .map(|change| PointStance {
                point: change.point,
                stance: change.now,
            })
            .collect(),
        latest: stances.latest(seat),
        moved,
        points: (points != before).then_some(points),
        stances,
    }
}

/// The side a stance takes on a point, in the point's words.
fn side_words(words: &str, stance: Stance) -> String {
    match stance {
        Stance::Agree => words.to_owned(),
        Stance::Object => format!("Against: {words}"),
        Stance::Question => format!("A question on: {words}"),
    }
}

/// One tapback for a seat's stances on what a message touched: ✘ if it objects to any of it,
/// ? if it asks about any, ✔ otherwise.
pub fn tapback(stances: &[PointStance], touched: &[u32]) -> Stance {
    let on: Vec<Stance> = stances
        .iter()
        .filter(|taken| touched.is_empty() || touched.contains(&taken.point))
        .map(|taken| taken.stance)
        .collect();
    if on.contains(&Stance::Object) {
        Stance::Object
    } else if on.contains(&Stance::Question) {
        Stance::Question
    } else {
        Stance::Agree
    }
}

/// The points still open when the talk stopped, settled one at a time. In Autopilot, or with
/// `council.unsettled = "vote"`, the seats' latest stances vote; a tie, or a council that
/// leaves it to you, asks one question per point, which your approval model answers in
/// Autopilot. A question you leave to the council goes the side the most seats back.
pub async fn settle(host: &Host, call: &StepCall, council: &Council) -> EngineResult<()> {
    let vote = host.settings().council.unsettled == Unsettled::Vote
        || autopilot::applies_now(host, ApprovalKind::Question);
    let table = council.table();
    let mut points = council.points.clone();
    let open: Vec<Point> = points
        .iter()
        .filter(|point| point.state == PointState::Open)
        .cloned()
        .collect();
    let alone = open.len() == 1;
    for point in open {
        let voted = vote
            .then(|| tally(&point, &council.stances, &table))
            .flatten();
        let (words, state) = match voted {
            Some(words) => (words, PointState::Voted),
            None => match ask_you(host, call, council, &point, alone).await? {
                Answer::Side(words) => (words, PointState::Settled),
                Answer::Voted(words) => (words, PointState::Voted),
                Answer::TalkOn(on) => {
                    let (tokens, minutes) = on.limits(&talk::spent(host, &call.at));
                    return record(
                        host,
                        &council.talk,
                        CouncilEventKind::Limits { tokens, minutes },
                    );
                }
            },
        };
        if let Some(settled) = points.iter_mut().find(|at| at.number == point.number) {
            // A side you pick is yours; words kept by a vote stay the seat's that raised them.
            if settled.words != words {
                settled.raised_by = None;
            }
            settled.words = words;
            settled.state = state;
            settled.sides.clear();
        }
        record(
            host,
            &council.talk,
            CouncilEventKind::Points {
                points: points.clone(),
            },
        )?;
    }
    Ok(())
}

/// What the seats' latest stances decide: the point as worded when more agree than object,
/// left out when more object, and `None` on a tie.
pub fn tally(point: &Point, stances: &Stances, table: &[SeatId]) -> Option<String> {
    let count = |wanted: Stance| {
        table
            .iter()
            .filter(|seat| stances.of(seat, point.number) == Some(wanted))
            .count()
    };
    let (agree, object) = (count(Stance::Agree), count(Stance::Object));
    match agree.cmp(&object) {
        std::cmp::Ordering::Greater => Some(point.words.clone()),
        std::cmp::Ordering::Less => Some(left_out(&point.words)),
        std::cmp::Ordering::Equal => None,
    }
}

fn left_out(words: &str) -> String {
    format!("Leave out: {words}")
}

/// What you answered for an open point: the words it settles on, the way the council goes
/// when you leave it to the council, or more talk.
enum Answer {
    Side(String),
    Voted(String),
    TalkOn(TalkOn),
}

/// One question for one open point: its sides when the note keeper listed them, else keep it
/// or leave it out, then letting the seats talk on, or your own words. `alone` says it is the
/// only point still open.
async fn ask_you(
    host: &Host,
    call: &StepCall,
    council: &Council,
    point: &Point,
    alone: bool,
) -> EngineResult<Answer> {
    // Each side as you read it, with the seats it names by name, beside the words it keeps.
    let names = host.names();
    let for_you = |words: &str| {
        relay::draw_markers(words, |seat| {
            council
                .seat(seat)
                .map_or_else(|| "a seat".to_owned(), |seat| seats::name(seat, names))
        })
    };
    let mut sides: Vec<(String, String)> = point
        .sides
        .iter()
        .filter(|side| !side.words.is_empty())
        .map(|side| (for_you(&side.words), side.words.clone()))
        .collect();
    // Each side's reason, as the note keeper gave it, under the side.
    let mut reasons: Vec<String> = point
        .sides
        .iter()
        .filter(|side| !side.words.is_empty())
        .map(|side| for_you(&side.reason))
        .collect();
    if sides.is_empty() {
        sides = vec![
            (for_you(&point.words), point.words.clone()),
            (LEAVE_IT_OUT.to_owned(), LEAVE_IT_OUT.to_owned()),
        ];
        reasons.clear();
    }
    let on = TalkOn::of(&host.settings().council.stop_after);
    let talk_on = on.words();
    let mut offered: Vec<String> = sides.iter().map(|(shown, _)| shown.clone()).collect();
    offered.push(talk_on.clone());
    let mut options = options::question(&offered);
    for (option, reason) in options.iter_mut().zip(&reasons) {
        option.detail = Some(reason.clone()).filter(|reason| !reason.trim().is_empty());
    }
    let text = if alone {
        "The seats agree on everything else. Pick a side, let them talk on, or write your own."
    } else {
        "Pick a side, let them talk on, or write your own."
    };
    let request = ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Question,
        title: format!("{}: which way?", as_asked(&for_you(&point.words))),
        task: None,
        packet: ApprovalPacket::Council(CouncilPacket {
            talk: council.talk.clone(),
            about: CouncilQuestion::Point {
                point: point.number,
            },
            text: text.to_owned(),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: Vec::new(),
        options: Some(options),
        free_text: FreeText::Answer,
        authors: council
            .seats()
            .iter()
            .map(|seat| seat.model.clone())
            .collect(),
    };
    let decided = approvals::open_and_wait(host, request).await?;
    if decided.by == Party::Declined {
        return Ok(Answer::Voted(councils_way(point)));
    }
    Ok(Answer::Side(match decided.action {
        AnswerAction::Choose { value } if value == talk_on => return Ok(Answer::TalkOn(on)),
        AnswerAction::Choose { value } if value == LEAVE_IT_OUT => left_out(&point.words),
        AnswerAction::Choose { value } => sides
            .into_iter()
            .find(|(shown, _)| *shown == value)
            .map_or(value, |(_, kept)| kept),
        AnswerAction::Answer => decided
            .text
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| point.words.clone()),
        _ => point.words.clone(),
    }))
}

/// A point's words as a question asks about them: a point written as a sentence loses its
/// closing stop, so the question doesn't read "growth.: which way?".
fn as_asked(words: &str) -> &str {
    words
        .trim()
        .trim_end_matches(['.', ';', ':', ','])
        .trim_end()
}

const LEAVE_IT_OUT: &str = "Leave it out";

/// The way a point you leave to the council goes: the side the most seats back, the first of
/// them on a tie, else the point as worded.
fn councils_way(point: &Point) -> String {
    let mut most: Option<&Side> = None;
    for side in point.sides.iter().filter(|side| !side.words.is_empty()) {
        if most.is_none_or(|most| side.backers.len() > most.backers.len()) {
            most = Some(side);
        }
    }
    most.map_or_else(|| point.words.clone(), |side| side.words.clone())
}

fn next_number(points: &[Point]) -> u32 {
    points.iter().map(|point| point.number).max().unwrap_or(0) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(n: u32) -> SeatId {
        SeatId::numbered(n)
    }

    #[test]
    fn a_point_written_as_a_sentence_is_asked_without_its_stop() {
        assert_eq!(
            as_asked("Eviction waits, despite per-key map growth."),
            "Eviction waits, despite per-key map growth"
        );
        assert_eq!(as_asked("Redis or memory"), "Redis or memory");
    }

    fn stance(point: u32, stance: Stance) -> PointStance {
        PointStance { point, stance }
    }

    fn two_points() -> Vec<Point> {
        listed([
            ("A token bucket per key".to_owned(), Vec::new()),
            ("Counters live in Redis".to_owned(), Vec::new()),
        ])
    }

    #[test]
    fn a_point_is_agreed_only_when_every_seat_at_the_table_agrees() {
        let table = [seat(1), seat(2), seat(3)];
        let mut points = two_points();
        let mut stances = Stances::default();
        stances.take(&seat(1), &[stance(1, Stance::Agree)]);
        stances.take(&seat(2), &[stance(1, Stance::Agree)]);
        assert!(!recount(&mut points, &stances, &table));
        assert_eq!(points[0].state, PointState::Open);

        stances.take(&seat(3), &[stance(1, Stance::Question)]);
        recount(&mut points, &stances, &table);
        assert_eq!(points[0].state, PointState::Open);

        stances.take(&seat(3), &[stance(1, Stance::Agree)]);
        assert!(recount(&mut points, &stances, &table));
        assert_eq!(points[0].state, PointState::Agreed);
        assert_eq!(points[1].state, PointState::Open);
        assert_eq!(agreed_of(&points), (1, 2));
        assert!(!all_settled(&points));

        // A seat that sits out doesn't hold the point open while it's away.
        stances.take(&seat(1), &[stance(2, Stance::Agree)]);
        stances.take(&seat(2), &[stance(2, Stance::Agree)]);
        recount(&mut points, &stances, &[seat(1), seat(2)]);
        assert!(all_settled(&points));
    }

    #[test]
    fn an_objection_reopens_an_agreed_point_and_counts_as_a_move() {
        let table = [seat(1), seat(2)];
        let mut points = two_points();
        let mut stances = Stances::default();
        for at in &table {
            stances.take(at, &[stance(1, Stance::Agree)]);
        }
        recount(&mut points, &stances, &table);
        assert_eq!(points[0].state, PointState::Agreed);

        let changes = stances.take(&seat(2), &[stance(1, Stance::Object)]);
        assert_eq!(
            changes,
            [Change {
                point: 1,
                was: Some(Stance::Agree),
                now: Stance::Object
            }]
        );
        assert!(changes[0].moved());
        assert!(recount(&mut points, &stances, &table));
        assert_eq!(points[0].state, PointState::Open);

        // The same stance again changes nothing, and a first stance moves nobody.
        let again = stances.take(&seat(2), &[stance(1, Stance::Object)]);
        assert!(again.is_empty());
        let first = stances.take(&seat(2), &[stance(2, Stance::Agree)]);
        assert!(!first[0].moved());
    }

    #[test]
    fn your_points_bind_whatever_the_seats_say() {
        let table = [seat(1), seat(2)];
        let mut points = two_points();
        let yours = add_yours(&mut points, "Build the limiter with Maker-checker");
        assert_eq!(yours, 3);
        let mut stances = Stances::default();
        stances.take(&seat(1), &[stance(yours, Stance::Object)]);
        recount(&mut points, &stances, &table);
        assert_eq!(points[2].state, PointState::Yours);
        assert_eq!(reword(&mut points, yours, "Build it alone"), None);
        assert_eq!(points[2].words, "Build the limiter with Maker-checker");
    }

    #[test]
    fn new_words_reopen_a_point_and_its_stances_are_taken_again() {
        let table = [seat(1), seat(2)];
        let mut points = two_points();
        let mut stances = Stances::default();
        for at in &table {
            stances.take(at, &[stance(2, Stance::Agree)]);
        }
        recount(&mut points, &stances, &table);
        assert_eq!(points[1].state, PointState::Agreed);

        let reworded = "Counters live in Redis, one key per route";
        assert_eq!(reword(&mut points, 2, reworded), Some(2));
        stances.forget(2);
        assert_eq!(stances.of(&seat(1), 2), None);
        recount(&mut points, &stances, &table);
        assert_eq!(points[1].state, PointState::Open);
        assert_eq!(reword(&mut points, 2, reworded), None);
        assert_eq!(reword(&mut points, 9, "No such point"), None);
    }

    fn offer(point: u32, stance: Stance, because: Option<&str>) -> Offered {
        Offered {
            point,
            stance,
            because: because.map(|what| MovedBecause::Read {
                what: what.to_owned(),
            }),
        }
    }

    #[test]
    fn a_changed_stance_counts_only_with_its_reason() {
        let table = [seat(1), seat(2)];
        let points = two_points();
        let mut stances = Stances::default();
        stances.take(&seat(1), &[stance(1, Stance::Object)]);

        let silent = take(
            &points,
            &stances,
            &table,
            &seat(1),
            &[offer(1, Stance::Agree, None), offer(2, Stance::Agree, None)],
            &[],
            &[],
        );
        assert_eq!(silent.changed, [stance(2, Stance::Agree)]);
        assert!(silent.moved.is_empty());
        assert_eq!(silent.stances.of(&seat(1), 1), Some(Stance::Object));

        let moved = take(
            &points,
            &stances,
            &table,
            &seat(1),
            &[
                offer(1, Stance::Agree, Some("limits.rs already buckets")),
                offer(9, Stance::Agree, None),
            ],
            &[],
            &[],
        );
        assert_eq!(moved.changed, [stance(1, Stance::Agree)]);
        assert_eq!(moved.moved.len(), 1);
        assert_eq!(moved.moved[0].to, "A token bucket per key");
        assert_eq!(moved.points, None, "seat 2 hasn't agreed yet");
    }

    #[test]
    fn a_seat_backs_the_points_it_adds_and_the_words_it_gives() {
        let table = [seat(1)];
        let points = two_points();
        let taken = take(
            &points,
            &Stances::default(),
            &table,
            &seat(1),
            &[offer(1, Stance::Agree, None)],
            &["Keep /health open".to_owned()],
            &[(2, "Counters live in Redis, one key per route".to_owned())],
        );
        let points = taken.points.expect("points changed");
        let states: Vec<(u32, &str, PointState)> = points
            .iter()
            .map(|point| (point.number, point.words.as_str(), point.state))
            .collect();
        assert_eq!(
            states,
            [
                (1, "A token bucket per key", PointState::Agreed),
                (
                    2,
                    "Counters live in Redis, one key per route",
                    PointState::Agreed
                ),
                (3, "Keep /health open", PointState::Agreed),
            ]
        );
        assert_eq!(taken.latest.len(), 3);
        let raised: Vec<Option<SeatId>> = points.iter().map(|p| p.raised_by.clone()).collect();
        assert_eq!(
            raised,
            [None, Some(seat(1)), Some(seat(1))],
            "the seat raised the words it gave and the point it added, not the listed one"
        );
    }

    #[test]
    fn a_tapback_objects_to_any_objection_on_what_the_message_touched() {
        let given = [stance(1, Stance::Agree), stance(2, Stance::Object)];
        assert_eq!(tapback(&given, &[1]), Stance::Agree);
        assert_eq!(tapback(&given, &[1, 2]), Stance::Object);
        assert_eq!(tapback(&given, &[]), Stance::Object);
        assert_eq!(
            tapback(&[stance(3, Stance::Question)], &[3]),
            Stance::Question
        );
        assert_eq!(tapback(&[], &[1]), Stance::Agree);
    }

    #[test]
    fn the_note_keepers_list_is_numbered_from_one_and_skips_empty_words() {
        let points = listed([
            ("  Where counters live ".to_owned(), Vec::new()),
            (String::new(), Vec::new()),
            (
                "How a burst is allowed".to_owned(),
                vec![Side {
                    words: "A bucket of 20".to_owned(),
                    backers: vec![seat(2)],
                    reason: String::new(),
                }],
            ),
        ]);
        let numbers: Vec<(u32, &str)> = points
            .iter()
            .map(|point| (point.number, point.words.as_str()))
            .collect();
        assert_eq!(
            numbers,
            [(1, "Where counters live"), (2, "How a burst is allowed")]
        );
        assert_eq!(points[1].sides.len(), 1);
        assert!(known(&points, 2));
        assert!(!known(&points, 3));
        assert_eq!(agreed_of(&[]), (0, 0));
        assert!(all_settled(&[]));
    }

    /// A point left to the council goes the side the most seats back, the first of them on a
    /// tie, and a point with no sides stays as worded.
    #[test]
    fn a_point_left_to_the_council_goes_the_side_most_seats_back() {
        let side = |words: &str, backers: &[u32]| Side {
            words: words.to_owned(),
            backers: backers.iter().map(|&n| seat(n)).collect(),
            reason: String::new(),
        };
        let mut point = Point {
            number: 2,
            words: "Calls with no key".to_owned(),
            state: PointState::Open,
            sides: vec![side("401", &[2]), side("One shared bucket", &[1, 3])],
            raised_by: None,
        };
        assert_eq!(councils_way(&point), "One shared bucket");
        point.sides = vec![side("401", &[2]), side("One shared bucket", &[1])];
        assert_eq!(councils_way(&point), "401", "a tie goes the first side");
        point.sides.clear();
        assert_eq!(councils_way(&point), "Calls with no key");
    }
}
