//! Maker lift: whether a seat backs what its own maker's seats raise more often than what the
//! others raise. Judges favour models of their own maker, so the run review shows it after a
//! council, and a benchmark can count it. It means something only with two makers at the table
//! and two seats from one of them.

use serde::{Deserialize, Serialize};

use argonauts_core::base::log::{Note, NoteLevel};
use argonauts_core::base::names::{ModelRef, SeatId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::council::{Backed, CouncilEventKind, SeatLift, Seating};

use super::seats;
use super::talk::{self, Council};
use crate::engine::error::EngineResult;
use crate::engine::host::Host;
use crate::roles;

/// A position the council took up: the seat that raised it, and every seat that backed it in
/// the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub raised_by: SeatId,
    pub backers: Vec<SeatId>,
}

/// The run review's look at each council of the run, once: each seat's lift where it says
/// something, and how many names and guesses code hid from the seats, recorded for benchmarks
/// and told to you in a line each.
pub fn review(host: &Host) -> EngineResult<()> {
    let councils: Vec<Council> = host.read(|state| {
        state
            .councils
            .iter()
            .filter(|council| council.seating.is_some() && !council.reviewed)
            .cloned()
            .collect()
    });
    let maker = |model: &ModelRef| roles::provider(host, model);
    let names = host.names();
    for council in councils {
        let Some(seating) = &council.seating else {
            continue;
        };
        let lift = if shown(seating, &maker) {
            of(seating, &council.positions(), &maker)
        } else {
            Vec::new()
        };
        let hidden = council
            .messages
            .iter()
            .map(|said| said.message.hidden)
            .sum();
        let guesses_hidden = council
            .messages
            .iter()
            .map(|said| said.message.guesses_hidden)
            .sum();
        talk::record(
            host,
            &council.talk,
            CouncilEventKind::RunReview {
                lift: lift.clone(),
                hidden,
                guesses_hidden,
            },
        )?;
        let told = hidden_line(hidden, guesses_hidden)
            .into_iter()
            .chain(lines(seating, &lift, names));
        for text in told {
            let note = Note {
                level: NoteLevel::Info,
                text,
            };
            host.record(None, None, RunEventKind::Note(note))?;
        }
    }
    Ok(())
}

/// Whether the figures say anything: two makers sit, and two seats share one of them.
pub fn shown(seating: &Seating, maker: &dyn Fn(&ModelRef) -> String) -> bool {
    let makers: Vec<String> = seating
        .seats
        .iter()
        .map(|seat| maker(&seat.model))
        .collect();
    let twice = makers
        .iter()
        .any(|one| makers.iter().filter(|other| *other == one).count() >= 2);
    let two_makers = makers
        .first()
        .is_some_and(|first| makers.iter().any(|one| one != first));
    twice && two_makers
}

/// Each seat's figures over `positions`, in seat order. A seat's own positions count for
/// neither side, and one raised by a seat that no longer sits counts for nobody.
pub fn of(
    seating: &Seating,
    positions: &[Position],
    maker: &dyn Fn(&ModelRef) -> String,
) -> Vec<SeatLift> {
    seating
        .seats
        .iter()
        .map(|seat| {
            let own_maker = maker(&seat.model);
            let mut lift = SeatLift {
                seat: seat.id.clone(),
                same_maker: Backed::default(),
                other_makers: Backed::default(),
                same_model: Backed::default(),
            };
            for position in positions {
                if position.raised_by == seat.id {
                    continue;
                }
                let Some(raiser) = seating
                    .seats
                    .iter()
                    .find(|other| other.id == position.raised_by)
                else {
                    continue;
                };
                let backed = position.backers.contains(&seat.id);
                if maker(&raiser.model) == own_maker {
                    lift.same_maker.count(backed);
                    if raiser.model == seat.model {
                        lift.same_model.count(backed);
                    }
                } else {
                    lift.other_makers.count(backed);
                }
            }
            lift
        })
        .collect()
}

/// A line for each seat that had both kinds of position to back: "Opus 5.5 · 1 backed 2 of 2
/// positions its maker's seats raised, 1 of 1 its own model's, and 0 of 2 other makers'".
pub fn lines(seating: &Seating, lifts: &[SeatLift], names: &DisplayNames) -> Vec<String> {
    lifts
        .iter()
        .filter(|lift| lift.lift().is_some())
        .filter_map(|lift| {
            let seat = seats::find(seating, &lift.seat)?;
            let same_model = if lift.same_model.of > 0 {
                format!(", {} its own model's,", of_how_many(lift.same_model))
            } else {
                ",".to_owned()
            };
            Some(format!(
                "{} backed {} positions its maker's seats raised{same_model} and {} other makers'",
                seats::name(seat, names),
                of_how_many(lift.same_maker),
                of_how_many(lift.other_makers),
            ))
        })
        .collect()
}

fn of_how_many(backed: Backed) -> String {
    format!("{} of {}", backed.backed, backed.of)
}

/// "Code hid 3 names and 1 guess at who runs a seat from the council's seats", or nothing when
/// code hid nothing.
fn hidden_line(hidden: u32, guesses: u32) -> Option<String> {
    let names = match hidden {
        0 => None,
        1 => Some("1 name".to_owned()),
        n => Some(format!("{n} names")),
    };
    let guesses = match guesses {
        0 => None,
        1 => Some("1 guess at who runs a seat".to_owned()),
        n => Some(format!("{n} guesses at who runs a seat")),
    };
    let what = match (names, guesses) {
        (None, None) => return None,
        (Some(one), None) | (None, Some(one)) => one,
        (Some(names), Some(guesses)) => format!("{names} and {guesses}"),
    };
    Some(format!("Code hid {what} from the council's seats"))
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::names::WorkflowId;
    use argonauts_core::runs::events::council::{
        CouncilEvent, Point, PointStance, PointState, Seat, SeatRole, Stance,
    };
    use argonauts_core::workflows::workflow::StepRef;

    use super::*;
    use crate::council::talk::Councils;

    const OPUS: &str = "claude-code/claude-opus-5-5";
    const SONNET: &str = "claude-code/claude-sonnet-5";
    const LUNA: &str = "codex/gpt-5.6-luna";

    fn maker(model: &ModelRef) -> String {
        match model.agent.as_str() {
            "claude-code" => "anthropic".to_owned(),
            _ => "openai".to_owned(),
        }
    }

    fn seating(models: &[&str]) -> Seating {
        Seating {
            seats: models
                .iter()
                .zip(1..)
                .map(|(model, n)| Seat {
                    id: SeatId::numbered(n),
                    model: model.parse().unwrap(),
                    role: SeatRole::Proposer,
                    number: None,
                })
                .collect(),
            notes: None,
        }
    }

    fn raised(by: u32, backers: &[u32]) -> Position {
        Position {
            raised_by: SeatId::numbered(by),
            backers: backers.iter().copied().map(SeatId::numbered).collect(),
        }
    }

    #[test]
    fn it_shows_only_with_two_makers_and_two_seats_of_one() {
        assert!(shown(&seating(&[OPUS, OPUS, LUNA]), &maker));
        assert!(shown(&seating(&[OPUS, SONNET, LUNA]), &maker));
        assert!(!shown(&seating(&[OPUS, OPUS, OPUS]), &maker), "one maker");
        assert!(
            !shown(&seating(&[OPUS, LUNA]), &maker),
            "no maker sits twice"
        );
    }

    #[test]
    fn a_seat_backing_its_makers_positions_more_shows_a_lift() {
        let seating = seating(&[OPUS, OPUS, SONNET, LUNA]);
        let positions = [
            raised(1, &[1, 2, 3]),
            raised(2, &[2, 1]),
            raised(3, &[3, 1, 4]),
            raised(4, &[4]),
            raised(4, &[4, 2]),
        ];
        let lifts = of(&seating, &positions, &maker);
        let first = &lifts[0];
        assert_eq!(first.same_maker, Backed { backed: 2, of: 2 });
        assert_eq!(first.same_model, Backed { backed: 1, of: 1 });
        assert_eq!(first.other_makers, Backed { backed: 0, of: 2 });
        assert_eq!(first.lift(), Some(1.0));
        assert_eq!(first.same_model_lift(), Some(1.0));
        let luna = &lifts[3];
        assert_eq!(
            luna.same_maker,
            Backed::default(),
            "nobody else is its maker's"
        );
        assert_eq!(luna.lift(), None);
        assert_eq!(luna.other_makers, Backed { backed: 1, of: 3 });
    }

    #[test]
    fn a_seat_that_left_raises_nothing_counted() {
        let seating = seating(&[OPUS, LUNA]);
        let lifts = of(&seating, &[raised(9, &[1, 2])], &maker);
        assert!(lifts.iter().all(|lift| lift.other_makers.of == 0));
    }

    #[test]
    fn each_seat_with_both_kinds_to_back_gets_a_line() {
        let mut seating = seating(&[OPUS, OPUS, LUNA]);
        seating.seats[0].number = Some(1);
        seating.seats[1].number = Some(2);
        let positions = [raised(2, &[1, 2]), raised(3, &[3]), raised(1, &[1, 3])];
        let lifts = of(&seating, &positions, &maker);
        let lines = lines(&seating, &lifts, &DisplayNames::default());
        assert_eq!(
            lines,
            [
                "Opus 5.5 · 1 backed 1 of 1 positions its maker's seats raised, 1 of 1 its own \
                 model's, and 0 of 1 other makers'",
                "Opus 5.5 · 2 backed 0 of 1 positions its maker's seats raised, 0 of 1 its own \
                 model's, and 0 of 1 other makers'",
            ],
            "5.6 Luna had no position of its own maker's to back"
        );
        assert_eq!(
            hidden_line(3, 1).as_deref(),
            Some("Code hid 3 names and 1 guess at who runs a seat from the council's seats")
        );
        assert_eq!(
            hidden_line(1, 0).as_deref(),
            Some("Code hid 1 name from the council's seats")
        );
        assert_eq!(hidden_line(0, 0), None);
    }

    fn point(number: u32, words: &str, raised_by: Option<u32>) -> Point {
        Point {
            number,
            words: words.to_owned(),
            state: PointState::Open,
            sides: Vec::new(),
            raised_by: raised_by.map(SeatId::numbered),
        }
    }

    fn agree(point: u32) -> Vec<PointStance> {
        vec![PointStance {
            point,
            stance: Stance::Agree,
        }]
    }

    #[test]
    fn new_words_close_the_old_position_with_the_seats_that_backed_it_then() {
        let talk = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 1,
            path: vec![0],
        };
        let mut councils = Councils::default();
        let mut apply = |kind: CouncilEventKind| {
            councils.apply(&CouncilEvent {
                talk: talk.clone(),
                kind,
            });
        };
        apply(CouncilEventKind::Seated(seating(&[OPUS, OPUS, LUNA])));
        apply(CouncilEventKind::Points {
            points: vec![point(1, "Counters live in Redis", Some(1))],
        });
        for seat in [1, 2] {
            apply(CouncilEventKind::Stances {
                seat: SeatId::numbered(seat),
                stances: agree(1),
            });
        }
        apply(CouncilEventKind::Points {
            points: vec![point(1, "Counters live in memory", Some(3))],
        });
        apply(CouncilEventKind::Stances {
            seat: SeatId::numbered(3),
            stances: agree(1),
        });
        let council = councils.newest().unwrap();
        assert_eq!(
            council.positions(),
            [raised(1, &[1, 2]), raised(3, &[3])],
            "the Redis words were backed by both Opus seats before 5.6 Luna gave new ones"
        );
    }
}
