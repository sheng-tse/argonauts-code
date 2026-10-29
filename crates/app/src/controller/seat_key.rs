//! The key at the head of a council seat's own session: the letter the seat reads every seat by,
//! drawn on asking as the council's relay draws it, sent to your screen alone and never
//! recorded.

use std::path::Path;

use argonauts_core::RunId;
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::names::SeatId;
use argonauts_core::workflow::StepRef;
use argonauts_crew::council::letters;

use super::Ctx;

/// Sends the seat's key when the install's secret has drawn one, else nothing.
pub(super) fn answer(ctx: &Ctx, run: RunId, talk: StepRef, seats: &[SeatId], reader: SeatId) {
    let run_dir = ctx.env.run_dir(&run);
    if let Some(data) = key(&run_dir, run, talk, seats, reader) {
        ctx.send(AppEvent::View {
            data: Box::new(data),
        });
    }
}

/// The seat's key from the run's folder, every seat's letter in seat order.
fn key(
    run_dir: &Path,
    run: RunId,
    talk: StepRef,
    seats: &[SeatId],
    reader: SeatId,
) -> Option<ViewData> {
    let drawn = letters::reader_letters(run_dir, &run, &talk, seats, &reader)
        .inspect_err(|err| tracing::warn!(error = %err, "the council's secret can't be read"))
        .ok()??;
    let letters = seats
        .iter()
        .filter_map(|seat| Some((seat.clone(), drawn.of(seat)?)))
        .collect();
    Some(ViewData::SeatKey {
        run,
        talk,
        reader,
        letters,
    })
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::WorkflowId;

    use super::*;

    /// A seat's key holds the letters the council's relay draws for that seat, every seat's in
    /// seat order; before the install has a secret there is none.
    #[test]
    fn a_seats_key_comes_from_the_relays_own_draw() {
        let home = tempfile::tempdir().unwrap();
        let run_dir = home
            .path()
            .join("state")
            .join("repo")
            .join("runs")
            .join("run-limit");
        let run = RunId::new("run-limit").unwrap();
        let talk = StepRef {
            workflow: WorkflowId::numbered(1),
            phase: 1,
            path: vec![0],
        };
        let seats: Vec<SeatId> = (1..=3).map(SeatId::numbered).collect();
        let reader = seats[1].clone();
        let none = key(&run_dir, run.clone(), talk.clone(), &seats, reader.clone());
        assert_eq!(none, None, "no secret, no key");

        letters::secret_in(run_dir.ancestors().nth(3).unwrap()).unwrap();
        let Some(ViewData::SeatKey {
            reader: read_by,
            letters,
            ..
        }) = key(&run_dir, run.clone(), talk.clone(), &seats, reader.clone())
        else {
            panic!("a seat's key");
        };
        assert_eq!(read_by, reader);
        let drawn = letters::reader_letters(&run_dir, &run, &talk, &seats, &reader)
            .unwrap()
            .unwrap();
        let want: Vec<_> = seats
            .iter()
            .map(|seat| (seat.clone(), drawn.of(seat).unwrap()))
            .collect();
        assert_eq!(letters, want);
    }
}
