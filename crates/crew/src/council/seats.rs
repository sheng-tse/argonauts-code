//! The seats of a council: each one's id, model, role and number, and the names you read them
//! by. A seat is a seat, not a model: the same model may sit two or three times, each seat its
//! own session sharing nothing with the others.

use argonauts_core::base::names::{ModelRef, SeatId};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::council::{Seat, SeatRole, Seating};
use argonauts_core::settings::Settings;
use argonauts_core::settings::council::CouncilSeat;
use argonauts_core::settings::crew::CrewRole;

use crate::state::RunState;

/// A council needs someone to disagree with.
pub const FEWEST: usize = 2;
/// More seats talk longer for little more: six is where the setup stops adding.
pub const MOST: usize = 6;

/// Why a council can't sit as set up.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeatingError {
    #[error("a council needs at least {FEWEST} seats, and {0} is set up")]
    TooFew(usize),
    #[error("a council seats at most {MOST}, and {0} are set up")]
    TooMany(usize),
    #[error("a council of only skeptics has no side to argue against")]
    OnlySkeptics,
}

/// What is unusual about a council that sits anyway, which the setup explains under its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusual {
    /// One model in several seats: sessions that share nothing, numbered on your screen.
    SitsAgain { model: ModelRef, times: u8 },
    /// More than one skeptic, which is allowed but slows agreement.
    Skeptics(u8),
}

/// The council's seats in order: `[[council.seat]]` when you listed any, else each of the
/// crew's workers once as a proposer. Seats get ids from `seat-001` in that order, and a model
/// that sits more than once is numbered among its own seats.
pub fn seat(settings: &Settings, names: &DisplayNames) -> Result<Vec<Seat>, SeatingError> {
    let listed: Vec<CouncilSeat> = if settings.council.seats.is_empty() {
        settings
            .models_for(CrewRole::Worker)
            .into_iter()
            .map(|model| CouncilSeat {
                model,
                role: SeatRole::Proposer,
            })
            .collect()
    } else {
        settings.council.seats.clone()
    };
    seat_listed(&listed, names)
}

/// The seats `listed` gives, checked: two to six, and not all skeptics. Two seats count as one
/// model when you'd read them by the same name, so `opus` and its full id are numbered too.
pub fn seat_listed(
    listed: &[CouncilSeat],
    names: &DisplayNames,
) -> Result<Vec<Seat>, SeatingError> {
    if listed.len() < FEWEST {
        return Err(SeatingError::TooFew(listed.len()));
    }
    if listed.len() > MOST {
        return Err(SeatingError::TooMany(listed.len()));
    }
    if listed.iter().all(|seat| seat.role == SeatRole::Skeptic) {
        return Err(SeatingError::OnlySkeptics);
    }
    let shown: Vec<String> = listed.iter().map(|seat| names.short(&seat.model)).collect();
    let seats = listed
        .iter()
        .enumerate()
        .map(|(index, seat)| {
            let alike = |other: &&String| **other == shown[index];
            let sits = shown.iter().filter(alike).count();
            let place = shown[..=index].iter().filter(alike).count();
            Seat {
                id: SeatId::numbered(index as u32 + 1),
                model: seat.model.clone(),
                role: seat.role,
                number: (sits > 1).then_some(place as u8),
            }
        })
        .collect();
    Ok(seats)
}

/// The seat that keeps the notes in the `nth` council these seats hold, from 0: rotation goes
/// round the seats, not the models, so two seats of one model each take a turn. A skeptic or a
/// checker keeps them only when no proposer sits, since the notes must stay neutral.
pub fn note_keeper(seats: &[Seat], nth: usize) -> Option<SeatId> {
    let proposers: Vec<&Seat> = seats
        .iter()
        .filter(|seat| seat.role == SeatRole::Proposer)
        .collect();
    let pool: Vec<&Seat> = if proposers.is_empty() {
        seats.iter().collect()
    } else {
        proposers
    };
    pool.get(nth % pool.len().max(1))
        .map(|seat| seat.id.clone())
}

/// The seats that argue the other side on purpose, whom every seat is told about.
pub fn skeptics(seating: &Seating) -> Vec<&Seat> {
    seating
        .seats
        .iter()
        .filter(|seat| seat.role == SeatRole::Skeptic)
        .collect()
}

/// The seats that may take a task when the plan has `tasks` of them: every seat but the
/// checkers, who build only when the tasks outnumber the other seats.
pub fn builders(seating: &Seating, tasks: usize) -> Vec<&Seat> {
    let others = seating
        .seats
        .iter()
        .filter(|seat| seat.role != SeatRole::Checker)
        .count();
    seating
        .seats
        .iter()
        .filter(|seat| seat.role != SeatRole::Checker || tasks > others)
        .collect()
}

/// What the setup explains about these seats, in seat order.
pub fn unusual(seats: &[Seat], names: &DisplayNames) -> Vec<Unusual> {
    let mut notes = Vec::new();
    for first in seats.iter().filter(|seat| seat.number == Some(1)) {
        let shown = names.short(&first.model);
        let times = seats
            .iter()
            .filter(|seat| seat.number.is_some() && names.short(&seat.model) == shown)
            .count();
        notes.push(Unusual::SitsAgain {
            model: first.model.clone(),
            times: times as u8,
        });
    }
    let skeptics = seats
        .iter()
        .filter(|seat| seat.role == SeatRole::Skeptic)
        .count();
    if skeptics > 1 {
        notes.push(Unusual::Skeptics(skeptics as u8));
    }
    notes
}

/// The seat as you read it: its model, numbered when the model sits more than once, as
/// "Opus 5.5 · 2". Seats never read this: they read one another by letter.
pub fn name(seat: &Seat, names: &DisplayNames) -> String {
    let model = names.short(&seat.model);
    match seat.number {
        Some(number) => format!("{model} · {number}"),
        None => model,
    }
}

/// The name with the seat's role word where it isn't a plain proposer, as the name row above
/// its bubble reads: "Opus 5.5 · 2 · skeptic", "Opus 5.5 · 1 · keeps the notes".
pub fn name_with_role(seat: &Seat, seating: &Seating, names: &DisplayNames) -> String {
    let mut words = vec![name(seat, names)];
    match seat.role {
        SeatRole::Proposer => {}
        SeatRole::Skeptic => words.push("skeptic".to_owned()),
        SeatRole::Checker => words.push("checker".to_owned()),
    }
    if seating.notes.as_ref() == Some(&seat.id) {
        words.push("keeps the notes".to_owned());
    }
    words.join(" · ")
}

/// Whether the run's workflow is a council.
pub fn in_council(settings: &Settings) -> bool {
    settings.workflow == "council"
}

/// Whether a council sat in this run: a session spoke for a seat. Its work is the council's,
/// and no prompt credits it to a seat or a model.
pub fn council_sat(state: &RunState) -> bool {
    state
        .sessions
        .by_key
        .values()
        .any(|session| session.started.seat.is_some())
}

/// What a frame that names whoever made the work says in a council: never the seat or the model.
pub const COUNCIL_AUTHOR: &str = "a seat of this council";

/// The seat with `id`, if it sits.
pub fn find<'s>(seating: &'s Seating, id: &SeatId) -> Option<&'s Seat> {
    seating.seats.iter().find(|seat| seat.id == *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(seats: &[(&str, SeatRole)]) -> Vec<CouncilSeat> {
        seats
            .iter()
            .map(|(model, role)| CouncilSeat {
                model: model.parse().unwrap(),
                role: *role,
            })
            .collect()
    }

    fn two_opus_and_luna() -> Vec<Seat> {
        let seats = listed(&[
            ("claude-code/claude-opus-5-5", SeatRole::Proposer),
            ("claude-code/claude-opus-5-5", SeatRole::Skeptic),
            ("codex/gpt-5.6-luna", SeatRole::Proposer),
        ]);
        seat_listed(&seats, &DisplayNames::default()).unwrap()
    }

    #[test]
    fn one_model_sits_twice_as_two_seats_numbered_in_seat_order() {
        let seats = two_opus_and_luna();
        let ids: Vec<&str> = seats.iter().map(|seat| seat.id.as_str()).collect();
        assert_eq!(ids, ["seat-001", "seat-002", "seat-003"]);
        let numbers: Vec<Option<u8>> = seats.iter().map(|seat| seat.number).collect();
        assert_eq!(numbers, [Some(1), Some(2), None]);
        let names = DisplayNames::default();
        assert_eq!(name(&seats[0], &names), "Opus 5.5 · 1");
        assert_eq!(name(&seats[1], &names), "Opus 5.5 · 2");
        assert!(
            !name(&seats[2], &names).contains('·'),
            "a model that sits once"
        );
    }

    #[test]
    fn the_name_row_adds_the_role_word_and_the_note_keeper() {
        let seats = two_opus_and_luna();
        let notes = note_keeper(&seats, 0);
        let seating = Seating {
            seats: seats.clone(),
            notes,
        };
        let names = DisplayNames::default();
        assert_eq!(
            name_with_role(&seats[0], &seating, &names),
            "Opus 5.5 · 1 · keeps the notes"
        );
        assert_eq!(
            name_with_role(&seats[1], &seating, &names),
            "Opus 5.5 · 2 · skeptic"
        );
        assert_eq!(skeptics(&seating).len(), 1);
    }

    #[test]
    fn the_notes_go_round_the_seats_not_the_models() {
        let seats = seat_listed(
            &listed(&[
                ("claude-code/claude-opus-5-5", SeatRole::Proposer),
                ("claude-code/claude-opus-5-5", SeatRole::Proposer),
                ("codex/gpt-5.6-luna", SeatRole::Skeptic),
            ]),
            &DisplayNames::default(),
        )
        .unwrap();
        let keepers: Vec<String> = (0..4)
            .map(|nth| note_keeper(&seats, nth).unwrap().as_str().to_owned())
            .collect();
        assert_eq!(keepers, ["seat-001", "seat-002", "seat-001", "seat-002"]);
    }

    #[test]
    fn a_council_sits_two_to_six_and_never_only_skeptics() {
        let names = DisplayNames::default();
        let one = listed(&[("claude-code/claude-opus-5-5", SeatRole::Proposer)]);
        assert_eq!(seat_listed(&one, &names), Err(SeatingError::TooFew(1)));
        let seven = listed(&[("claude-code/claude-opus-5-5", SeatRole::Proposer); 7]);
        assert_eq!(seat_listed(&seven, &names), Err(SeatingError::TooMany(7)));
        let skeptics = listed(&[
            ("claude-code/claude-opus-5-5", SeatRole::Skeptic),
            ("codex/gpt-5.6-luna", SeatRole::Skeptic),
        ]);
        assert_eq!(
            seat_listed(&skeptics, &names),
            Err(SeatingError::OnlySkeptics)
        );
        let three_opus = listed(&[
            ("claude-code/claude-opus-5-5", SeatRole::Skeptic),
            ("claude-code/claude-opus-5-5", SeatRole::Skeptic),
            ("claude-code/claude-opus-5-5", SeatRole::Proposer),
            ("codex/gpt-5.6-luna", SeatRole::Proposer),
        ]);
        let seats = seat_listed(&three_opus, &names).unwrap();
        assert_eq!(
            unusual(&seats, &names),
            [
                Unusual::SitsAgain {
                    model: "claude-code/claude-opus-5-5".parse().unwrap(),
                    times: 3
                },
                Unusual::Skeptics(2)
            ]
        );
    }

    #[test]
    fn unset_seats_are_the_crews_workers_once_each_as_proposers() {
        let mut settings = Settings::default();
        settings.crew.worker = vec![
            "claude-code/claude-opus-5-5".parse().unwrap(),
            "codex/gpt-5.6-luna".parse().unwrap(),
        ];
        let seats = seat(&settings, &DisplayNames::default()).unwrap();
        assert_eq!(seats.len(), 2);
        assert!(seats.iter().all(|seat| seat.role == SeatRole::Proposer));
        assert!(seats.iter().all(|seat| seat.number.is_none()));
    }

    #[test]
    fn a_checker_builds_only_when_the_tasks_outnumber_the_other_seats() {
        let seats = seat_listed(
            &listed(&[
                ("claude-code/claude-opus-5-5", SeatRole::Proposer),
                ("codex/gpt-5.6-luna", SeatRole::Proposer),
                ("antigravity/gemini-3.8-flash", SeatRole::Checker),
            ]),
            &DisplayNames::default(),
        )
        .unwrap();
        let seating = Seating { seats, notes: None };
        assert_eq!(builders(&seating, 2).len(), 2);
        assert_eq!(builders(&seating, 3).len(), 3);
    }
}
