//! How a message reaches each seat: at once for a seat that isn't writing, and for one that
//! is, at its next tool call or its next turn as its CLI allows.
//!
//! A seat's turn holds everything it hasn't read, each line drawn for that seat alone: the
//! others by their letters, itself as "you", and the user as the user. What it reads ends with
//! its move and the points still open.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::council::{
    CouncilAction, CouncilEventKind, CouncilMessage, MessageKind, MovedBecause, Point, PointState,
    ReplyTo, Seat, Speaker, Stance,
};
use argonauts_core::names::{MessageId, SeatId, SeatLetter, SessionKey};
use argonauts_core::session_runner::{SessionControl, SteerSupport};
use argonauts_core::settings::council::WhenYouWrite;

use super::letters::{self, Draw};
use super::lexicon::{self, Lexicon};
use super::relay::{self, Ambiguous, Relay, YourMove};
use super::talk::{self, Council};
use super::{points, private, seats};
use crate::blackboard;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::ids::IdKind;

/// When a seat that is writing reads your message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// Inside the turn it's writing, with its next tool call's result.
    NextToolCall,
    /// Once its message is done, at the start of its next turn.
    NextTurn,
    /// It stops, and writes its message again with yours.
    Restart,
}

/// How your message reaches a seat that is writing. Only Claude Code takes input into a running
/// turn; Codex reads none once a turn starts, and agy and OpenCode queue it as the next turn.
pub fn reach(cli: &str, steer: SteerSupport, when: WhenYouWrite) -> Reach {
    if when == WhenYouWrite::Restart {
        return Reach::Restart;
    }
    if cli == crate::ultra::CLAUDE_CODE && steer == SteerSupport::Input {
        Reach::NextToolCall
    } else {
        Reach::NextTurn
    }
}

/// What you do to the council that talks. Your message lands at once, and each seat reads it at
/// the start of its next turn; the seat writing now reads it as its CLI allows, or starts its
/// message again with it under `council.when_you_write = "restart"`.
pub async fn command(host: &Host, action: CouncilAction) -> Result<(), String> {
    let talking = host.read(|state| state.councils.talking().cloned());
    let council = talking.ok_or_else(|| {
        "No council is talking now. Write to a seat in its session, or send the plan back at \
         its approval to talk on."
            .to_owned()
    })?;
    let failed = |err: crate::error::EngineError| err.to_string();
    match action {
        CouncilAction::Write {
            text,
            reply_to,
            mentions,
            points: yours,
        } => {
            let text = text.trim();
            if text.is_empty() && yours.is_empty() {
                return Err("Write something for the council first.".to_owned());
            }
            let relay = relay_for(host, &council).map_err(failed)?;
            // Each seat you name is held as the seat, so every reader reads its own letter.
            let parts = relay
                .from_you(text)
                .map_err(|unclear| ambiguous(&relay, &unclear))?;
            let mut your_points = Vec::new();
            for words in yours.iter().filter(|words| !words.trim().is_empty()) {
                let held = relay
                    .from_you(words.trim())
                    .map_err(|unclear| ambiguous(&relay, &unclear))?;
                your_points.push(relay::with_markers(&held));
            }
            let reply_to = match reply_to {
                Some(id) => {
                    let replied = council
                        .message(&id)
                        .filter(|_| council.out_to_seats(&id))
                        .ok_or_else(|| format!("The council has no message {id} to reply to."))?;
                    // The words the bar over your input showed, never the whole message.
                    let words = relay::with_markers(&replied.text);
                    let quote = argonauts_core::text::last_sentence(&words).to_owned();
                    Some(ReplyTo { message: id, quote })
                }
                None => None,
            };
            if let Some(stranger) = mentions.iter().find(|seat| council.seat(seat).is_none()) {
                return Err(format!("{stranger} doesn't sit in this council."));
            }
            let mut mentions = mentions;
            for seat in relay::mentions(&parts) {
                if !mentions.contains(&seat) {
                    mentions.push(seat);
                }
            }
            let id = MessageId::numbered(host.allocate(IdKind::Message).map_err(failed)?);
            let mut message = talk::yours(id.clone(), text);
            if !parts.is_empty() {
                message.text = parts;
            }
            message.reply_to = reply_to;
            message.mentions = mentions;
            talk::record(
                host,
                &council.talk,
                CouncilEventKind::Message(Box::new(message.clone())),
            )
            .map_err(failed)?;
            if !your_points.is_empty() {
                let mut listed = council.points.clone();
                for words in &your_points {
                    points::add_yours(&mut listed, words);
                }
                talk::record(
                    host,
                    &council.talk,
                    CouncilEventKind::Points { points: listed },
                )
                .map_err(failed)?;
            }
            let now = talk::council_now(host, &council.talk).unwrap_or(council);
            if let Some((writing, _)) = now.writing.clone() {
                let words = line(&relay, &now, &writing, &message);
                reach_the_writer(host, &now, &writing, &id, words).await?;
            }
        }
        CouncilAction::Private { seat, text } => {
            let text = text.trim();
            if text.is_empty() {
                return Err("Write something for the seat first.".to_owned());
            }
            if council.seat(&seat).is_none() {
                return Err(format!("{seat} doesn't sit in this council."));
            }
            let relay = relay_for(host, &council).map_err(failed)?;
            // The seat reads every seat you name by its own letters, as the council does.
            let parts = relay
                .from_you(text)
                .map_err(|unclear| ambiguous(&relay, &unclear))?;
            let id = MessageId::numbered(host.allocate(IdKind::Message).map_err(failed)?);
            let mut message = talk::yours(id.clone(), text);
            if !parts.is_empty() {
                message.text = parts;
            }
            // Recorded as private first, so the words never count as the council's.
            talk::record(
                host,
                &council.talk,
                CouncilEventKind::Private {
                    seat: seat.clone(),
                    message: id.clone(),
                },
            )
            .map_err(failed)?;
            let words = private::turn(&relay.render_for(&seat, &message));
            talk::record(
                host,
                &council.talk,
                CouncilEventKind::Message(Box::new(message)),
            )
            .map_err(failed)?;
            let writing = council.writing.as_ref().map(|(writing, _)| writing);
            if writing == Some(&seat) {
                reach_the_writer(host, &council, &seat, &id, words).await?;
            }
        }
        CouncilAction::Share { message } => {
            if !council.private.iter().any(|(_, id, _)| id == &message) {
                return Err(format!("{message} is no private message of yours."));
            }
            if !council.shared.contains(&message) {
                talk::record(host, &council.talk, CouncilEventKind::Shared { message })
                    .map_err(failed)?;
            }
        }
        CouncilAction::CarryOn { seat } => {
            if council.seat(&seat).is_none() {
                return Err(format!("{seat} doesn't sit in this council."));
            }
            if !council.sat_out.contains(&seat) {
                talk::record(host, &council.talk, CouncilEventKind::SatOut { seat })
                    .map_err(failed)?;
            }
        }
        CouncilAction::Rejoin { seat, said } => {
            if council.sat_out.contains(&seat) {
                let relay = relay_for(host, &council).map_err(failed)?;
                for text in said
                    .iter()
                    .map(|text| text.trim())
                    .filter(|text| !text.is_empty())
                {
                    // Typed to the seat in its own CLI: your private message, which it read
                    // there, so a council message that repeats it waits for you.
                    let id = MessageId::numbered(host.allocate(IdKind::Message).map_err(failed)?);
                    let mut message = talk::yours(id.clone(), text);
                    if let Ok(parts) = relay.from_you(text)
                        && !parts.is_empty()
                    {
                        message.text = parts;
                    }
                    for kind in [
                        CouncilEventKind::Private {
                            seat: seat.clone(),
                            message: id.clone(),
                        },
                        CouncilEventKind::Message(Box::new(message)),
                        CouncilEventKind::Read {
                            message: id,
                            seat: seat.clone(),
                        },
                    ] {
                        talk::record(host, &council.talk, kind).map_err(failed)?;
                    }
                }
                let missed = u32::try_from(council.unread(&seat).len()).unwrap_or(u32::MAX);
                talk::record(
                    host,
                    &council.talk,
                    CouncilEventKind::Rejoined { seat, missed },
                )
                .map_err(failed)?;
            }
        }
    }
    Ok(())
}

/// Your words to the seat writing now, as far as its CLI takes them mid-turn.
async fn reach_the_writer(
    host: &Host,
    council: &Council,
    writing: &SeatId,
    message: &MessageId,
    words: String,
) -> Result<(), String> {
    let Some(seat) = council.seat(writing) else {
        return Ok(());
    };
    let Some(session) = live_session(host, council, seat) else {
        return Ok(());
    };
    let steer = host
        .services()
        .workers
        .get(&seat.model)
        .map_err(|err| err.to_string())?
        .capabilities()
        .steer;
    match reach(
        &seat.model.agent,
        steer,
        host.settings().council.when_you_write,
    ) {
        Reach::NextTurn => {}
        Reach::NextToolCall => {
            blackboard::commands::steer(host, &session, words, false).await?;
            talk::record(
                host,
                &council.talk,
                CouncilEventKind::Read {
                    message: message.clone(),
                    seat: writing.clone(),
                },
            )
            .map_err(|err| err.to_string())?;
        }
        Reach::Restart => {
            if let Some(live) = host.shared().live.get(&session) {
                live.cancel.cancel();
            }
            let _ = host.control(&session, SessionControl::Stop);
        }
    }
    Ok(())
}

/// The session a seat is writing in now, at the talk step.
fn live_session(host: &Host, council: &Council, seat: &Seat) -> Option<SessionKey> {
    let live = host.shared().live.keys();
    host.read(|state| {
        live.into_iter().find(|key| {
            state.sessions.get(key).is_some_and(|session| {
                session.started.workflow_step.as_ref() == Some(&council.talk)
                    && session.started.seat.as_ref() == Some(&seat.id)
            })
        })
    })
}

/// Everything a seat reads at the start of a turn, each line drawn for it alone and in the
/// order it happened: the messages it hasn't had, with the first answers in an order drawn for
/// it, the tapbacks on its own message or on one it reacted to, who moved and why, and the
/// points agreed or made yours. Then its move, and for a move to write or react, the points
/// still open with their sides in an order drawn for it.
pub fn turn(
    relay: &Relay,
    key: &DrawKey,
    council: &Council,
    reader: &SeatId,
    unread: &[&CouncilMessage],
    next: YourMove,
) -> String {
    let since = council.since(reader);
    let mut lines = messages(relay, key, council, reader, unread);
    lines.extend(tapbacks(relay, council, reader, since));
    lines.extend(moves(relay, council, reader, since));
    lines.extend(points_since(relay, council, reader, since));
    lines.sort_by_key(|(at, _)| *at);
    let lines: Vec<String> = lines.into_iter().map(|(_, line)| line).collect();
    // With no lines, the template's own line break would come first; its last would split the move.
    let mut turn = relay::turn(&lines, next).trim().to_owned();
    if next != YourMove::Wait {
        let open = open_points(relay, key, council, reader);
        if !open.is_empty() {
            turn.push('.');
            for point in open {
                turn.push(' ');
                turn.push_str(&point);
            }
        }
    }
    turn
}

/// One message as one seat reads it: stripped of who wrote it, with the reader's letters.
pub fn line(relay: &Relay, council: &Council, reader: &SeatId, message: &CouncilMessage) -> String {
    let replied = message
        .reply_to
        .as_ref()
        .and_then(|reply| council.message(&reply.message));
    relay.turn_line(reader, message, replied)
}

/// The unread messages with where each was said. First answers were written alone, all at
/// once, so each reader gets them in its own drawn order, never the order they finished in.
fn messages(
    relay: &Relay,
    key: &DrawKey,
    council: &Council,
    reader: &SeatId,
    unread: &[&CouncilMessage],
) -> Vec<(usize, String)> {
    let at = |message: &CouncilMessage| {
        council
            .messages
            .iter()
            .find(|said| said.message.id == message.id)
            .map_or(council.seen, |said| said.at)
    };
    let (first, rest): (Vec<&CouncilMessage>, Vec<&CouncilMessage>) = unread
        .iter()
        .copied()
        .partition(|message| message.kind == MessageKind::FirstAnswer);
    let mut places: Vec<usize> = first.iter().map(|message| at(message)).collect();
    places.sort_unstable();
    let drawn = letters::order_for(&key.draw(), reader, "first answers", &first, |message| {
        message.id.to_string()
    });
    let mut lines: Vec<(usize, String)> = places
        .into_iter()
        .zip(drawn)
        .map(|(place, message)| (place, line(relay, council, reader, message)))
        .collect();
    lines.extend(
        rest.into_iter()
            .map(|message| (at(message), line(relay, council, reader, message))),
    );
    lines
}

/// "Tapbacks on Seat T's message: Seat M ✘ · yours ✘", once the tapbacks on the reader's own
/// message, or on one it reacted to, are all cast. Each was cast before its seat saw the others'.
fn tapbacks(
    relay: &Relay,
    council: &Council,
    reader: &SeatId,
    since: usize,
) -> Vec<(usize, String)> {
    council
        .messages
        .iter()
        .filter_map(|said| {
            let cast = council.tapbacks.get(&said.message.id)?;
            let last = cast.values().map(|(_, at)| *at).max()?;
            let own = said.message.from == Speaker::Seat(reader.clone());
            if last <= since || !(own || cast.contains_key(reader)) {
                return None;
            }
            let mut cast: Vec<(&SeatId, &(Stance, usize))> = cast.iter().collect();
            cast.sort_by_key(|(_, (_, at))| *at);
            let cast: Vec<(SeatId, Stance)> = cast
                .into_iter()
                .map(|(seat, (stance, _))| (seat.clone(), *stance))
                .collect();
            Some((last, relay.tapbacks_line(reader, &said.message, &cast)))
        })
        .collect()
}

/// Each other seat that changed its stance, and why: the words that moved it, what it read, or
/// that its reason came from the user in private, never what the user said.
fn moves(relay: &Relay, council: &Council, reader: &SeatId, since: usize) -> Vec<(usize, String)> {
    council
        .moved
        .iter()
        .filter(|(moved, at)| *at > since && &moved.seat != reader)
        .map(|(moved, at)| {
            let who = seat_name(relay, reader, &moved.seat);
            let to = relay.held_for(reader, &moved.to);
            let what = format!("{who} moved to \"{to}\" on point {}", moved.point);
            let why = match &moved.because {
                MovedBecause::Quote { message, words } => {
                    let from = message
                        .as_ref()
                        .and_then(|id| council.message(id))
                        .map_or(Speaker::Seat(moved.seat.clone()), |quoted| {
                            quoted.from.clone()
                        });
                    let words = relay.quote_for(reader, words, &from);
                    format!(", for the words \"{words}\".")
                }
                MovedBecause::Read { what } => {
                    let what = relay.quote_for(reader, what, &Speaker::Seat(moved.seat.clone()));
                    format!(" after reading {what}.")
                }
                MovedBecause::InPrivate => {
                    format!(". Its reason came from {}.", private::MOVED_IN_PRIVATE)
                }
            };
            (*at, format!("From the council · {what}{why}"))
        })
        .collect()
}

/// The points every seat came to agree on, and the points you made, which bind.
fn points_since(
    relay: &Relay,
    council: &Council,
    reader: &SeatId,
    since: usize,
) -> Vec<(usize, String)> {
    let (agreed, of) = points::agreed_of(&council.points);
    council
        .points
        .iter()
        .filter_map(|point| match point.state {
            PointState::Agreed => {
                let at = *council.agreed_at.get(&point.number)?;
                (at > since).then(|| (at, relay.agreed_line(reader, point, agreed, of)))
            }
            PointState::Yours => {
                let at = *council.worded_at.get(&point.number)?;
                let words = point_words(relay, reader, point);
                let line = format!(
                    "From the council · Point {} is the user's, and binds: {words}",
                    point.number
                );
                (at > since).then_some((at, line))
            }
            _ => None,
        })
        .collect()
}

/// "Point 4, token bucket or window counter, is open", for each point still open, with its
/// sides when the note keeper listed them.
fn open_points(relay: &Relay, key: &DrawKey, council: &Council, reader: &SeatId) -> Vec<String> {
    council
        .points
        .iter()
        .filter(|point| point.state == PointState::Open)
        .map(|point| {
            let words = point_words(relay, reader, point);
            let what = format!("point {} sides", point.number);
            let sides: Vec<String> =
                letters::order_for(&key.draw(), reader, &what, &point.sides, |side| {
                    side.words.clone()
                })
                .iter()
                .map(|side| relay.held_for(reader, &side.words))
                .filter(|side| !side.is_empty())
                .collect();
            match sides.as_slice() {
                [] => format!("Point {}, {words}, is open.", point.number),
                [one] => format!("Point {}, {words}, is open: {one}.", point.number),
                [rest @ .., last] => format!(
                    "Point {}, {words}, is open: {} or {last}.",
                    point.number,
                    rest.join(", ")
                ),
            }
        })
        .collect()
}

/// Every point as the talk settled it, for the note keeper writing the plan.
pub fn settled(relay: &Relay, council: &Council, reader: &SeatId) -> String {
    let mut lines = vec!["The points as the council settled them:".to_owned()];
    for point in &council.points {
        let how = match point.state {
            PointState::Open => "still open",
            PointState::Agreed => "agreed",
            PointState::Yours => "the user's, binding",
            PointState::Settled => "settled by the user",
            PointState::Voted => "settled by a vote",
        };
        let words = point_words(relay, reader, point);
        lines.push(format!("- Point {}, {how}: {words}", point.number));
    }
    lines.join("\n")
}

/// A point's words as `reader` reads them: a seat's stripped like a message, yours as you
/// wrote them, and the seats in either drawn with the reader's letters.
fn point_words(relay: &Relay, reader: &SeatId, point: &Point) -> String {
    if point.state != PointState::Yours {
        return relay.held_for(reader, &point.words);
    }
    relay::draw_markers(&point.words, |seat| {
        let name = seat_name(relay, reader, seat);
        if seat == reader {
            letter(relay, reader, seat).map_or(name, |l| format!("Seat {l} (you)"))
        } else {
            name
        }
    })
}

/// What a council's letters and each reader's orders are drawn from: the install's secret, the
/// run, and the council within the run, so a resumed run draws them the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawKey {
    pub secret: String,
    pub run: String,
    pub council: String,
}

impl DrawKey {
    pub fn of(host: &Host, council: &Council) -> EngineResult<Self> {
        Ok(Self {
            secret: letters::secret(host)?,
            run: host.run_id().as_str().to_owned(),
            council: letters::council_of(&council.talk),
        })
    }

    pub fn draw(&self) -> Draw<'_> {
        Draw {
            secret: &self.secret,
            run: &self.run,
            council: &self.council,
        }
    }
}

/// The relay a council's messages pass through: its seats, each reader's letters, and the
/// run's names to hide.
pub fn relay_for(host: &Host, council: &Council) -> EngineResult<Relay> {
    seated_relay(
        council,
        &DrawKey::of(host, council)?.draw(),
        lexicon::of_run(host),
    )
}

/// The relay for `council`'s seats, with their letters drawn from `draw`.
pub fn seated_relay(council: &Council, draw: &Draw<'_>, lexicon: Lexicon) -> EngineResult<Relay> {
    let seating = council
        .seating
        .clone()
        .ok_or_else(|| EngineError::Refused("the council hasn't sat down yet".to_owned()))?;
    let seats: Vec<SeatId> = seating.seats.iter().map(|seat| seat.id.clone()).collect();
    let letters = letters::draw(draw, &seats);
    Ok(Relay::new(
        seating,
        letters,
        lexicon,
        DisplayNames::default(),
    ))
}

/// "Seat T" as `reader` knows `seat`, or "you" for the reader itself.
pub fn seat_name(relay: &Relay, reader: &SeatId, seat: &SeatId) -> String {
    if seat == reader {
        return "you".to_owned();
    }
    letter(relay, reader, seat).map_or_else(|| "a seat".to_owned(), |l| format!("Seat {l}"))
}

/// The letter `reader` knows `seat` by.
pub fn letter(relay: &Relay, reader: &SeatId, seat: &SeatId) -> Option<SeatLetter> {
    relay.letters(reader)?.of(seat)
}

/// The seat `reader` means by `letter`, if one goes by it.
pub fn seat_by_letter(relay: &Relay, reader: &SeatId, letter: char) -> Option<SeatId> {
    let letter = SeatLetter::new(letter.to_ascii_uppercase())?;
    relay.letters(reader)?.seat(letter).cloned()
}

/// What you wrote when a name in it fits more than one seat.
fn ambiguous(relay: &Relay, ambiguous: &Ambiguous) -> String {
    let names = DisplayNames::default();
    let seats: Vec<String> = ambiguous
        .seats
        .iter()
        .filter_map(|seat| seats::find(relay.seating(), seat))
        .map(|seat| seats::name(seat, &names))
        .collect();
    format!(
        "\"{}\" could be {}. Name the seat with its number.",
        ambiguous.written,
        seats.join(" or ")
    )
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::council::{CouncilEventKind, PointStance, ReplyTo, Side};
    use argonauts_core::names::MessageId;

    use super::*;
    use crate::council::talk::tests::{council, from_seat, said, seat, seated};

    #[test]
    fn only_a_writing_claude_code_seat_reads_you_mid_turn() {
        let finish = WhenYouWrite::Finish;
        assert_eq!(
            reach("claude-code", SteerSupport::Input, finish),
            Reach::NextToolCall
        );
        assert_eq!(
            reach("codex", SteerSupport::InterruptResume, finish),
            Reach::NextTurn
        );
        assert_eq!(reach("agy", SteerSupport::Input, finish), Reach::NextTurn);
        assert_eq!(
            reach("opencode", SteerSupport::NextTurnOnly, finish),
            Reach::NextTurn
        );
        assert_eq!(
            reach("claude-code", SteerSupport::Input, WhenYouWrite::Restart),
            Reach::Restart
        );
    }

    /// The key a test council's letters and orders are drawn from, with a made-up secret.
    fn key() -> DrawKey {
        DrawKey {
            secret: "made-up-secret".to_owned(),
            run: "run-1".to_owned(),
            council: "1/1/0".to_owned(),
        }
    }

    fn relay_of(council: &Council) -> Relay {
        seated_relay(council, &key().draw(), Lexicon::built_in()).expect("a seated council")
    }

    /// "Seat T", as `reader` knows `seat`, the reader itself included.
    fn known(relay: &Relay, reader: u32, seat_n: u32) -> String {
        let letter = letter(relay, &seat(reader), &seat(seat_n)).expect("a letter");
        format!("Seat {letter}")
    }

    fn turn_of(talk: &Council, reader: u32, next: YourMove) -> String {
        let relay = relay_of(talk);
        let reader = seat(reader);
        turn(&relay, &key(), talk, &reader, &talk.unread(&reader), next)
    }

    fn points(listing: &[(&str, &[&str])]) -> CouncilEventKind {
        let listing = listing.iter().map(|(words, sides)| {
            let sides = sides
                .iter()
                .map(|words| Side {
                    words: (*words).to_owned(),
                    backers: Vec::new(),
                    reason: String::new(),
                })
                .collect();
            ((*words).to_owned(), sides)
        });
        CouncilEventKind::Points {
            points: points::listed(listing),
        }
    }

    #[test]
    fn each_readers_letter_for_a_seat_names_that_seat_again() {
        let seated = council([seated(4)]);
        let relay = relay_of(&seated);
        for reader in 1..=4 {
            for other in 1..=4 {
                let letter = letter(&relay, &seat(reader), &seat(other)).unwrap();
                assert_eq!(
                    seat_by_letter(&relay, &seat(reader), letter.as_char().to_ascii_lowercase()),
                    Some(seat(other))
                );
            }
            assert_eq!(seat_name(&relay, &seat(reader), &seat(reader)), "you");
        }
        let again = relay_of(&seated);
        assert_eq!(
            letter(&relay, &seat(1), &seat(2)),
            letter(&again, &seat(1), &seat(2)),
            "a resumed run draws the same letters"
        );
    }

    /// A quote holds its seats as seats, so each reader reads them by its own letters.
    #[test]
    fn a_quote_names_its_seats_by_the_readers_letters() {
        let mut reply = said(
            2,
            Speaker::Seat(seat(2)),
            MessageKind::Talk,
            "Then the window holds.",
        );
        reply.reply_to = Some(ReplyTo {
            message: MessageId::numbered(1),
            quote: "as [seat-003] said".to_owned(),
        });
        let events = vec![
            seated(3),
            from_seat(1, 1, "As Seat Q said, use a bucket per key."),
            CouncilEventKind::Message(Box::new(reply.clone())),
        ];
        let talk = council(events);
        let relay = relay_of(&talk);
        for reader in [1, 3] {
            let three = known(&relay, reader, 3);
            let read = line(&relay, &talk, &seat(reader), &reply);
            assert!(
                read.contains(&format!("(\"as {three} said\")")),
                "{reader}: {read}"
            );
        }
    }

    /// A seat's words as written name its CLI, sign and refer to itself. The test lexicon
    /// knows no CLI's tool names, which the relay's own tests cover.
    const SIGNED: &str = "As Claude, I'd keep the window. Claude Code's Bash tool shows 12 \
                          passed.\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>";

    fn gives_away_nothing(read: &str) {
        for word in ["Claude", "Anthropic", "anthropic", "Co-Authored-By", "As "] {
            assert!(!read.contains(word), "{word:?} in {read}");
        }
    }

    #[test]
    fn your_reply_quotes_the_seats_words_as_each_other_seat_read_them() {
        let mut yours = said(2, Speaker::You, MessageKind::Talk, "Why?");
        yours.reply_to = Some(ReplyTo {
            message: MessageId::numbered(1),
            quote: SIGNED.to_owned(),
        });
        let talk = council(vec![
            seated(3),
            from_seat(1, 1, SIGNED),
            CouncilEventKind::Message(Box::new(yours.clone())),
        ]);
        let relay = relay_of(&talk);
        for reader in [2, 3] {
            let read = line(&relay, &talk, &seat(reader), &yours);
            assert!(read.contains("the user, replying to"), "{read}");
            gives_away_nothing(&read);
        }
    }

    #[test]
    fn a_move_for_a_seats_words_quotes_them_as_its_reader_read_them() {
        let talk = council(vec![
            seated(3),
            from_seat(1, 1, SIGNED),
            CouncilEventKind::Moved(argonauts_core::events::council::Moved {
                seat: seat(2),
                point: 1,
                to: "A token bucket per key".to_owned(),
                because: MovedBecause::Quote {
                    message: Some(MessageId::numbered(1)),
                    words: SIGNED.to_owned(),
                },
            }),
        ]);
        let text = turn_of(&talk, 3, YourMove::Write);
        assert!(text.contains("for the words"), "{text}");
        gives_away_nothing(&text);
    }

    #[test]
    fn a_stance_moved_in_private_shows_that_it_came_from_the_user_and_nothing_more() {
        let events = vec![
            seated(2),
            from_seat(1, 1, "Use a bucket per key."),
            CouncilEventKind::Moved(argonauts_core::events::council::Moved {
                seat: seat(2),
                point: 1,
                to: "A token bucket per key".to_owned(),
                because: MovedBecause::InPrivate,
            }),
        ];
        let talk = council(events);
        let relay = relay_of(&talk);
        let text = turn_of(&talk, 1, YourMove::Write);
        let two = known(&relay, 1, 2);
        assert!(
            text.contains(&format!(
                "From the council · {two} moved to \"A token bucket per key\" on point 1. Its \
                 reason came from the user, in private."
            )),
            "{text}"
        );
        // The seat that moved knows why, so its own turn doesn't say it again.
        let own = turn_of(&talk, 2, YourMove::Write);
        assert!(!own.contains("moved to"), "{own}");
    }

    #[test]
    fn a_turn_names_the_others_by_letter_and_ends_with_the_move_and_the_open_points() {
        let mut reply = said(
            2,
            Speaker::Seat(seat(2)),
            MessageKind::Talk,
            "A window is one INCR.",
        );
        reply.reply_to = Some(ReplyTo {
            message: MessageId::numbered(1),
            quote: "a bucket per key".to_owned(),
        });
        reply.stances = vec![PointStance {
            point: 1,
            stance: Stance::Object,
        }];
        let events = vec![
            seated(3),
            from_seat(1, 1, "Use a bucket per key."),
            points(&[(
                "Rate limits by key",
                &["a token bucket", "a window counter"],
            )]),
            CouncilEventKind::Message(Box::new(reply)),
            CouncilEventKind::Tapback {
                message: MessageId::numbered(1),
                seat: seat(3),
                stance: Stance::Agree,
            },
        ];
        let talk = council(events);
        let relay = relay_of(&talk);
        let text = turn_of(&talk, 1, YourMove::Write);
        let two = known(&relay, 1, 2);
        let three = known(&relay, 1, 3);
        let sides = letters::order_for(
            &key().draw(),
            &seat(1),
            "point 1 sides",
            &["a token bucket", "a window counter"],
            |side| (*side).to_owned(),
        );
        assert_eq!(
            text,
            format!(
                "From the council · {two}, replying to you (\"a bucket per key\"): A window is \
                 one INCR.\n\
                 From the council · Tapbacks on your message: {three} ✔\n\
                 Your move: write your message. Point 1, Rate limits by key, is open: {} or {}.",
                sides[0], sides[1]
            )
        );
        let waiting = turn_of(&talk, 1, YourMove::Wait);
        assert!(waiting.ends_with("Your move: nothing yet"), "{waiting}");
    }

    #[test]
    fn first_answers_reach_each_seat_in_an_order_drawn_for_it() {
        let first = |id: u32, n: u32| {
            CouncilEventKind::Message(Box::new(said(
                id,
                Speaker::Seat(seat(n)),
                MessageKind::FirstAnswer,
                &format!("Answer {n}."),
            )))
        };
        let talk = council([
            seated(5),
            first(1, 1),
            first(2, 2),
            first(3, 3),
            first(4, 4),
            first(5, 5),
        ]);
        let relay = relay_of(&talk);
        let mut orders = Vec::new();
        for reader in 1..=5 {
            let text = turn_of(&talk, reader, YourMove::Write);
            let read: Vec<u32> = text
                .lines()
                .filter_map(|line| line.strip_suffix('.')?.rsplit(' ').next()?.parse().ok())
                .collect();
            let finished: Vec<u32> = (1..=5).filter(|n| *n != reader).collect();
            let drawn = letters::order_for(
                &key().draw(),
                &seat(reader),
                "first answers",
                &finished,
                |n| MessageId::numbered(*n).to_string(),
            );
            assert_eq!(read, drawn, "seat {reader} reads them as drawn for it");
            for n in &finished {
                let writer = known(&relay, reader, *n);
                assert!(
                    text.contains(&format!("From the council · {writer}: Answer {n}.")),
                    "{text}"
                );
            }
            orders.push(read != finished);
        }
        assert!(
            orders.iter().any(|shuffled| *shuffled),
            "some seat reads them out of the order they finished in"
        );
    }

    #[test]
    fn a_points_words_reach_each_seat_stripped_and_in_its_own_letters() {
        let events = vec![
            seated(3),
            points(&[("Keep [seat-002]'s bucket, as Claude Code wrote it", &[])]),
        ];
        let talk = council(events);
        let relay = relay_of(&talk);
        for reader in [1, 3] {
            let text = turn_of(&talk, reader, YourMove::Write);
            let two = known(&relay, reader, 2);
            assert!(
                text.contains(&format!(
                    "Point 1, Keep {two}'s bucket, as {} wrote it, is open.",
                    relay::HIDDEN
                )),
                "{reader}: {text}"
            );
            assert!(!text.contains("Claude"), "{text}");
        }
    }

    /// A seat reacts before it sees the others' tapbacks, so its next turn shows the whole round.
    #[test]
    fn a_seat_that_reacted_reads_every_tapback_of_the_round_and_what_was_agreed() {
        let agreed = || {
            let mut listed = points::listed([("A token bucket per key".to_owned(), Vec::new())]);
            listed[0].state = PointState::Agreed;
            CouncilEventKind::Points { points: listed }
        };
        let round = |n: u32| {
            [
                CouncilEventKind::Read {
                    message: MessageId::numbered(1),
                    seat: seat(n),
                },
                CouncilEventKind::Tapback {
                    message: MessageId::numbered(1),
                    seat: seat(n),
                    stance: Stance::Agree,
                },
                CouncilEventKind::Stances {
                    seat: seat(n),
                    stances: vec![PointStance {
                        point: 1,
                        stance: Stance::Agree,
                    }],
                },
            ]
        };
        let mut events = vec![
            seated(3),
            points(&[("A token bucket per key", &[])]),
            from_seat(1, 1, "A bucket per key."),
        ];
        events.extend(round(2));
        events.extend(round(3));
        events.push(agreed());
        let talk = council(events);
        let relay = relay_of(&talk);
        let one = known(&relay, 2, 1);
        let three = known(&relay, 2, 3);
        let text = turn_of(&talk, 2, YourMove::Write);
        assert_eq!(
            text,
            format!(
                "From the council · Tapbacks on {one}'s message: yours ✔ · {three} ✔\n\
                 From the council · Point 1 agreed: A token bucket per key · 1 of 1 points \
                 agreed\n\
                 Your move: write your message"
            )
        );
    }

    #[test]
    fn your_point_reaches_each_seat_once_as_binding_with_its_seats_in_their_letters() {
        let mut listed = Vec::new();
        points::add_yours(&mut listed, "Keep /health open, as [seat-002] asked");
        let talk = council([seated(2), CouncilEventKind::Points { points: listed }]);
        let relay = relay_of(&talk);
        let mine = known(&relay, 2, 2);
        assert_eq!(
            turn_of(&talk, 2, YourMove::Write),
            format!(
                "From the council · Point 1 is the user's, and binds: Keep /health open, as \
                 {mine} (you) asked\n\
                 Your move: write your message"
            )
        );
    }

    #[test]
    fn the_note_keeper_writing_the_plan_reads_every_point_as_settled() {
        let mut listed = points::listed([
            ("A token bucket per key".to_owned(), Vec::new()),
            ("Counters live in Redis".to_owned(), Vec::new()),
        ]);
        listed[0].state = PointState::Agreed;
        listed[1].state = PointState::Voted;
        let talk = council([seated(2), CouncilEventKind::Points { points: listed }]);
        let relay = relay_of(&talk);
        assert_eq!(
            settled(&relay, &talk, &seat(1)),
            "The points as the council settled them:\n\
             - Point 1, agreed: A token bucket per key\n\
             - Point 2, settled by a vote: Counters live in Redis"
        );
    }
}
