//! Each reader's letters for the other seats, drawn anew for every council and the same again
//! when a run resumes. A seat learns its own letter and the others', never what runs them: one
//! seat's T is another seat's B, so a letter means nothing outside the session that reads it.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use argonauts_core::RunId;
use argonauts_core::base::hash::sha256_hex;
use argonauts_core::base::names::{SeatId, SeatLetter};
use argonauts_core::workflows::workflow::StepRef;

use crate::engine::host::Host;

/// The file in the state folder holding the install's secret, which no session can read.
const SECRET_FILE: &str = "council-secret";

/// What a council's maps are drawn from: the install's secret, the run, and the council within
/// the run. The same inputs give the same maps, so a resumed run keeps its letters.
#[derive(Debug, Clone, Copy)]
pub struct Draw<'a> {
    pub secret: &'a str,
    pub run: &'a str,
    pub council: &'a str,
}

impl Draw<'_> {
    fn hash(&self, reader: &SeatId, what: &str) -> String {
        sha256_hex(
            format!(
                "{}\n{}\n{}\n{}\n{what}",
                self.secret,
                self.run,
                self.council,
                reader.as_str()
            )
            .as_bytes(),
        )
    }
}

/// One reader's map: a letter for every seat, its own included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Letters {
    reader: SeatId,
    by_seat: BTreeMap<SeatId, SeatLetter>,
}

impl Letters {
    pub fn reader(&self) -> &SeatId {
        &self.reader
    }

    /// The letter the reader calls itself by.
    pub fn own(&self) -> SeatLetter {
        self.by_seat[&self.reader]
    }

    pub fn of(&self, seat: &SeatId) -> Option<SeatLetter> {
        self.by_seat.get(seat).copied()
    }

    /// The seat a letter names in this reader's map. `None` for a letter no seat has here.
    pub fn seat(&self, letter: SeatLetter) -> Option<&SeatId> {
        self.by_seat
            .iter()
            .find(|(_, mine)| **mine == letter)
            .map(|(seat, _)| seat)
    }

    /// Every other seat's letter, in seat order.
    pub fn others(&self) -> impl Iterator<Item = (&SeatId, SeatLetter)> {
        self.by_seat
            .iter()
            .filter(|(seat, _)| **seat != self.reader)
            .map(|(seat, letter)| (seat, *letter))
    }
}

/// Every reader's map for `seats`. Each map is one to one, drawn from the pool of twenty in an
/// order of its own; with three or more seats, no two readers' maps are all alike.
pub fn draw(key: &Draw<'_>, seats: &[SeatId]) -> BTreeMap<SeatId, Letters> {
    let mut maps: BTreeMap<SeatId, Letters> = BTreeMap::new();
    let mut first: Option<BTreeMap<SeatId, SeatLetter>> = None;
    for reader in seats {
        let mut redraw = 0u32;
        let by_seat = loop {
            let by_seat = one_map(key, seats, reader, redraw);
            let alike = first.as_ref() == Some(&by_seat);
            if !(alike && seats.len() >= 3) {
                break by_seat;
            }
            redraw += 1;
        };
        first.get_or_insert_with(|| by_seat.clone());
        maps.insert(
            reader.clone(),
            Letters {
                reader: reader.clone(),
                by_seat,
            },
        );
    }
    maps
}

fn one_map(
    key: &Draw<'_>,
    seats: &[SeatId],
    reader: &SeatId,
    redraw: u32,
) -> BTreeMap<SeatId, SeatLetter> {
    let mut pool: Vec<(String, char)> = SeatLetter::POOL
        .iter()
        .map(|letter| {
            (
                key.hash(reader, &format!("letters {redraw} {letter}")),
                *letter,
            )
        })
        .collect();
    pool.sort();
    seats
        .iter()
        .zip(pool)
        .filter_map(|(seat, (_, letter))| Some((seat.clone(), SeatLetter::new(letter)?)))
        .collect()
}

/// `items` in an order drawn for `reader`, never the order they came in: first answers and
/// the sides of a point reach each seat this way, so finishing first says nothing. `what`
/// tells two orders for one reader apart, such as one point's sides from another's.
pub fn order_for<T: Clone>(
    key: &Draw<'_>,
    reader: &SeatId,
    what: &str,
    items: &[T],
    id: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut keyed: Vec<(String, T)> = items
        .iter()
        .map(|item| {
            (
                key.hash(reader, &format!("order {what} {}", id(item))),
                item.clone(),
            )
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, item)| item).collect()
}

/// `items` in an order drawn from `seed` alone, the same for the same seed and items: the order
/// each judge, critic or mapping agent reads the plans in comes this way, keyed by the run and
/// the reader.
pub fn drawn_order<T: Clone>(seed: &str, items: &[T], id: impl Fn(&T) -> String) -> Vec<T> {
    let mut keyed: Vec<(String, T)> = items
        .iter()
        .map(|item| {
            (
                sha256_hex(format!("{seed}\n{}", id(item)).as_bytes()),
                item.clone(),
            )
        })
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed.into_iter().map(|(_, item)| item).collect()
}

/// The install's secret, made once in the state folder, which sessions are kept from reading.
pub fn secret(host: &Host) -> io::Result<String> {
    secret_in(&state_folder(&host.repo().run_dir))
}

/// The state folder a run's folder sits in, `<state>/runs/<repo>/<run>`.
fn state_folder(run_dir: &Path) -> PathBuf {
    run_dir
        .ancestors()
        .nth(3)
        .map_or_else(|| run_dir.to_path_buf(), Path::to_path_buf)
}

/// The council a talk step names, as its maps are drawn for it.
pub fn council_of(talk: &StepRef) -> String {
    let place: Vec<String> = talk.path.iter().map(u32::to_string).collect();
    format!("{}/{}/{}", talk.workflow, talk.phase, place.join("."))
}

/// The letters `reader` reads every seat of the council in `talk` by, its own among them, for
/// the key at the head of that seat's session on your screen alone: drawn as the relay draws
/// them, from the install's secret, which this never makes. `None` while there is no secret
/// yet, or for a seat that doesn't sit there.
pub fn reader_letters(
    run_dir: &Path,
    run: &RunId,
    talk: &StepRef,
    seats: &[SeatId],
    reader: &SeatId,
) -> io::Result<Option<Letters>> {
    let secret = match std::fs::read_to_string(state_folder(run_dir).join(SECRET_FILE)) {
        Ok(secret) if !secret.trim().is_empty() => secret.trim().to_owned(),
        Ok(_) => return Ok(None),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let council = council_of(talk);
    let key = Draw {
        secret: &secret,
        run: run.as_str(),
        council: &council,
    };
    Ok(draw(&key, seats).remove(reader))
}

/// The secret in `folder`, made there with 32 random bytes when it has none yet.
pub fn secret_in(folder: &Path) -> io::Result<String> {
    let path = folder.join(SECRET_FILE);
    match std::fs::read_to_string(&path) {
        Ok(secret) if !secret.trim().is_empty() => return Ok(secret.trim().to_owned()),
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::create_dir_all(folder)?;
    let secret = format!(
        "{:032x}{:032x}",
        rand::random::<u128>(),
        rand::random::<u128>()
    );
    write_private(&path, &secret)?;
    Ok(secret)
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(text.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn seats(count: u32) -> Vec<SeatId> {
        (1..=count).map(SeatId::numbered).collect()
    }

    fn key<'a>(run: &'a str) -> Draw<'a> {
        Draw {
            secret: "made-up-secret",
            run,
            council: "wf-001/1",
        }
    }

    /// Your screen's key for a seat draws the very map its relay reads the seats by, and
    /// draws none before the install has a secret.
    #[test]
    fn a_readers_key_is_the_map_its_relay_draws() {
        let home = tempfile::tempdir().unwrap();
        let run_dir = home
            .path()
            .join("state")
            .join("repo-1")
            .join("runs")
            .join("run-1");
        let run = RunId::new("run-1").unwrap();
        let talk = StepRef {
            workflow: argonauts_core::base::names::WorkflowId::numbered(1),
            phase: 1,
            path: vec![0],
        };
        let seats = seats(4);
        let none = reader_letters(&run_dir, &run, &talk, &seats, &seats[1]).unwrap();
        assert_eq!(none, None, "no secret yet");
        let secret = secret_in(&state_folder(&run_dir)).unwrap();
        let council = council_of(&talk);
        let relay = draw(
            &Draw {
                secret: &secret,
                run: "run-1",
                council: &council,
            },
            &seats,
        );
        let key = reader_letters(&run_dir, &run, &talk, &seats, &seats[1]).unwrap();
        assert_eq!(key.as_ref(), relay.get(&seats[1]));
        let away = SeatId::numbered(9);
        assert_eq!(
            reader_letters(&run_dir, &run, &talk, &seats, &away).unwrap(),
            None
        );
    }

    #[test]
    fn each_readers_map_is_one_to_one_and_names_every_seat() {
        let seats = seats(6);
        for letters in draw(&key("run-1"), &seats).values() {
            let used: BTreeSet<SeatLetter> =
                seats.iter().filter_map(|seat| letters.of(seat)).collect();
            assert_eq!(used.len(), seats.len());
            for seat in &seats {
                let letter = letters.of(seat).unwrap();
                assert_eq!(letters.seat(letter), Some(seat));
            }
            assert_eq!(letters.others().count(), seats.len() - 1);
            assert_eq!(letters.of(letters.reader()), Some(letters.own()));
        }
    }

    #[test]
    fn the_same_inputs_give_the_same_maps_and_other_readers_and_runs_other_ones() {
        let seats = seats(3);
        let once = draw(&key("run-1"), &seats);
        assert_eq!(
            once,
            draw(&key("run-1"), &seats),
            "a resumed run keeps its letters"
        );
        let maps: Vec<Vec<Option<SeatLetter>>> = once
            .values()
            .map(|letters| seats.iter().map(|seat| letters.of(seat)).collect())
            .collect();
        assert!(
            maps.windows(2).any(|pair| pair[0] != pair[1]),
            "no two readers' maps are all alike"
        );
        assert_ne!(
            once,
            draw(&key("run-2"), &seats),
            "another run, other letters"
        );
        let other_council = Draw {
            council: "wf-001/4",
            ..key("run-1")
        };
        assert_ne!(
            once,
            draw(&other_council, &seats),
            "another council, other letters"
        );
    }

    #[test]
    fn first_answers_reach_each_reader_in_an_order_of_its_own() {
        let seats = seats(5);
        let in_finish_order: Vec<SeatId> = seats.clone();
        let orders: BTreeSet<Vec<SeatId>> = seats
            .iter()
            .map(|reader| {
                order_for(
                    &key("run-1"),
                    reader,
                    "first answers",
                    &in_finish_order,
                    |seat| seat.as_str().to_owned(),
                )
            })
            .collect();
        assert!(orders.len() > 1, "readers read them in different orders");
        let again = order_for(
            &key("run-1"),
            &seats[0],
            "first answers",
            &in_finish_order,
            |seat| seat.as_str().to_owned(),
        );
        assert!(orders.contains(&again));
    }

    #[test]
    fn the_secret_is_made_once_and_kept() {
        let folder = tempfile::tempdir().unwrap();
        let made = secret_in(folder.path()).unwrap();
        assert_eq!(made.len(), 64);
        assert_eq!(secret_in(folder.path()).unwrap(), made);
    }
}
