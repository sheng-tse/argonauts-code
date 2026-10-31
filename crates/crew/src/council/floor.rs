//! Who writes next: the seat you replied to or named, the seat the last message answered, one
//! that objected, then the quietest. One seat writes at a time. A seat that must write its
//! message again, because code held it or you stopped it for yours, goes before all of them.
//!
//! Code picks, never the seats: a seat can ask for the floor only by being answered, named, or
//! objecting, so the loudest seat can't keep it and the quietest still gets heard. The skeptic
//! also gets one turn on a point about to close on weak ground.

use argonauts_core::base::names::SeatId;
use argonauts_core::runs::events::council::{
    CouncilMessage, FloorReason, MessageKind, MovedBecause, PointState, SeatRole, Speaker, Stance,
    TextPart,
};

use super::private::has_extension;
use super::talk::Council;

/// The seat that writes next and why, or `None` when nobody sits at the table.
pub fn next(council: &Council) -> Option<(SeatId, FloorReason)> {
    let table = council.table();
    if table.is_empty() {
        return None;
    }
    if let Some(seat) = council.rewrite.as_ref().filter(|seat| table.contains(seat)) {
        return Some((seat.clone(), FloorReason::Rewrite));
    }
    if let Some(seat) = council
        .restarted
        .as_ref()
        .filter(|seat| table.contains(seat))
    {
        return Some((seat.clone(), FloorReason::You));
    }
    if let Some(yours) = asked_by_you(council, &table) {
        return Some((yours, FloorReason::You));
    }
    if let Some((skeptic, point)) = unproven(council, &table) {
        return Some((skeptic, FloorReason::Unproven { point }));
    }
    let last = council
        .messages
        .iter()
        .rev()
        .find(|said| matches!(said.message.from, Speaker::Seat(_)));
    if let Some(last) = last {
        let Speaker::Seat(by) = &last.message.from else {
            unreachable!("found as a seat's message");
        };
        let others = |seat: &&SeatId| *seat != by;
        if let Some(answered) = last
            .message
            .reply_to
            .as_ref()
            .and_then(|reply| council.author(&reply.message))
            .filter(|seat| table.contains(seat) && others(seat))
        {
            return Some((answered.clone(), FloorReason::RepliedTo { by: by.clone() }));
        }
        if let Some(named) = last
            .message
            .mentions
            .iter()
            .find(|seat| table.contains(seat) && others(seat))
        {
            return Some((named.clone(), FloorReason::Named { by: by.clone() }));
        }
        // The note keeper's own stances on its list count like anyone's.
        let writer = (last.message.kind != MessageKind::Notes).then_some(by);
        if let Some(objected) = objector(council, &last.message, &table, writer) {
            return Some((objected, FloorReason::Objected));
        }
    }
    let quietest = quietest(council, &table, last.map(|said| &said.message))?;
    Some((quietest, FloorReason::Quietest))
}

/// The seat your oldest unanswered message asks to answer: the seat whose message you replied
/// to, then each seat you named, in the order you named them.
fn asked_by_you(council: &Council, table: &[SeatId]) -> Option<SeatId> {
    for said in &council.messages {
        if said.message.from != Speaker::You {
            continue;
        }
        let replied = said
            .message
            .reply_to
            .as_ref()
            .and_then(|reply| council.author(&reply.message));
        let asked = replied.into_iter().chain(said.message.mentions.iter());
        for seat in asked {
            let answered = council.messages.iter().any(|later| {
                later.at > said.at
                    && later.message.kind == MessageKind::Talk
                    && later.message.from == Speaker::Seat(seat.clone())
            });
            if !answered && table.contains(seat) {
                return Some(seat.clone());
            }
        }
    }
    None
}

/// A seat that objected to the last message or asked about it, by its tapback, or by its stance
/// on a point the message took one on.
fn objector(
    council: &Council,
    last: &CouncilMessage,
    table: &[SeatId],
    writer: Option<&SeatId>,
) -> Option<SeatId> {
    let pushes_back = |stance: Stance| matches!(stance, Stance::Object | Stance::Question);
    let others = || table.iter().filter(|seat| Some(*seat) != writer);
    let by_tapback = others().find(|seat| {
        council
            .tapbacks
            .get(&last.id)
            .and_then(|tapbacks| tapbacks.get(*seat))
            .is_some_and(|(stance, _)| pushes_back(*stance))
    });
    let by_stance = || {
        others().find(|seat| {
            last.stances.iter().any(|taken| {
                council
                    .stances
                    .of(seat, taken.point)
                    .is_some_and(pushes_back)
            })
        })
    };
    by_tapback.or_else(by_stance).cloned()
}

/// The seat with the fewest talk messages, then the one quiet longest, then the first in seat
/// order. The seat that wrote last answers itself only when it sits alone.
fn quietest(council: &Council, table: &[SeatId], last: Option<&CouncilMessage>) -> Option<SeatId> {
    let last_writer = last.and_then(|message| match &message.from {
        Speaker::Seat(seat) => Some(seat),
        Speaker::You => None,
    });
    let candidates: Vec<&SeatId> = table
        .iter()
        .filter(|seat| table.len() == 1 || Some(*seat) != last_writer)
        .collect();
    let order = |seat: &SeatId| table.iter().position(|at| at == seat);
    candidates
        .into_iter()
        .min_by_key(|seat| {
            (
                council.spoken(seat),
                council.last_spoke(seat).map_or(-1, |at| at as i64),
                order(seat),
            )
        })
        .cloned()
}

/// Why a point that is about to close stands on weak ground.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weak {
    /// A seat moved to it for what you told it in private, a reason no other seat can read.
    InPrivate,
    /// Nothing behind it names a file, a test run or a doc.
    NoEvidence,
}

/// The skeptic and the point it may challenge now: every other seat at the table agrees with
/// the point's words, the skeptic doesn't, and the point stands on weak ground. It gets that
/// turn once for each wording of a point, so it can't stall the talk.
fn unproven(council: &Council, table: &[SeatId]) -> Option<(SeatId, u32)> {
    let skeptic = |seat: &&SeatId| {
        council
            .seat(seat)
            .is_some_and(|seat| seat.role == SeatRole::Skeptic)
    };
    let skeptics: Vec<&SeatId> = table.iter().filter(skeptic).collect();
    let others: Vec<&SeatId> = table.iter().filter(|seat| !skeptic(seat)).collect();
    if skeptics.is_empty() || others.is_empty() {
        return None;
    }
    council
        .points
        .iter()
        .filter(|point| point.state == PointState::Open)
        .find_map(|point| {
            let number = point.number;
            let since = worded_at(council, number);
            if council
                .challenged
                .get(&number)
                .is_some_and(|at| *at >= since)
            {
                return None;
            }
            let agrees = |seat: &SeatId| council.stances.of(seat, number) == Some(Stance::Agree);
            if !others.iter().all(|seat| agrees(seat)) {
                return None;
            }
            let challenger = skeptics.iter().find(|seat| !agrees(seat))?;
            weak(council, number).map(|_| ((*challenger).clone(), number))
        })
}

/// Why a point stands on weak ground with the seats that back it, if it does: a backer moved
/// to it in private, or neither its words, nor its backers' reasons and messages since it took
/// those words, name any evidence.
pub fn weak(council: &Council, point: u32) -> Option<Weak> {
    let since = worded_at(council, point);
    let backs = |seat: &SeatId| council.stances.of(seat, point) == Some(Stance::Agree);
    let moves: Vec<&MovedBecause> = council
        .moved
        .iter()
        .filter(|(moved, at)| *at >= since && moved.point == point && backs(&moved.seat))
        .map(|(moved, _)| &moved.because)
        .collect();
    let latest_in_private = council
        .table()
        .iter()
        .filter(|seat| backs(seat))
        .any(|seat| {
            council
                .moved
                .iter()
                .rev()
                .find(|(moved, at)| *at >= since && moved.point == point && moved.seat == *seat)
                .is_some_and(|(moved, _)| moved.because == MovedBecause::InPrivate)
        });
    if latest_in_private {
        return Some(Weak::InPrivate);
    }
    let words = council
        .points
        .iter()
        .find(|known| known.number == point)
        .is_some_and(|known| names_evidence(&known.words));
    let reasons = moves.iter().any(|because| match because {
        MovedBecause::Read { .. } => true,
        MovedBecause::Quote { words, .. } => names_evidence(words),
        MovedBecause::InPrivate => false,
    });
    let messages = council.messages.iter().any(|said| {
        let backer = match &said.message.from {
            Speaker::Seat(seat) => backs(seat),
            Speaker::You => false,
        };
        let touches = said
            .message
            .stances
            .iter()
            .any(|taken| taken.point == point && taken.stance == Stance::Agree);
        said.at >= since && backer && touches && names_evidence(&plain(&said.message))
    });
    (!(words || reasons || messages)).then_some(Weak::NoEvidence)
}

fn worded_at(council: &Council, point: u32) -> usize {
    council.worded_at.get(&point).copied().unwrap_or(0)
}

fn plain(message: &CouncilMessage) -> String {
    message
        .text
        .iter()
        .filter_map(|part| match part {
            TextPart::Words(words) => Some(words.as_str()),
            TextPart::Seat(_) => None,
        })
        .collect()
}

/// Whether words point to something a seat can check: code quoted in backticks, as a test or a
/// command is, a link, a path, or a file's name.
pub fn names_evidence(text: &str) -> bool {
    if text.matches('`').count() >= 2 {
        return true;
    }
    text.split_whitespace().any(|raw| {
        let word = raw.trim_matches(|c: char| !c.is_alphanumeric() && !"/_~.".contains(c));
        let word = word.trim_end_matches('.');
        let link = raw.contains("://");
        // "and/or" is two words, while "./run.sh", "~/notes" and "crates/crew/src" are paths.
        let path = word.contains('/')
            && (word.starts_with(['/', '.', '~']) || word.matches('/').count() >= 2);
        // "e.g" is an abbreviation, not "limits.rs".
        let file = word
            .rsplit('/')
            .next()
            .is_some_and(|name| has_extension(name) && name.split('.').any(|part| part.len() >= 2));
        link || path || file
    })
}

/// A salt that stays the same for the same words on every machine and every version, so a
/// resumed run draws the same note keeper.
pub fn salt(words: &[&str]) -> u64 {
    // FNV-1a: small, and fixed by its definition.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for word in words {
        for byte in word.bytes().chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::names::MessageId;
    use argonauts_core::runs::events::council::{CouncilEventKind, Moved, PointStance, ReplyTo};

    use super::*;
    use crate::council::talk::tests::{council, from_seat, said, seat, seated};

    fn yours(id: u32, words: &str) -> CouncilMessage {
        said(id, Speaker::You, MessageKind::Talk, words)
    }

    fn message(kind: CouncilMessage) -> CouncilEventKind {
        CouncilEventKind::Message(Box::new(kind))
    }

    fn replying(mut message: CouncilMessage, to: u32) -> CouncilMessage {
        message.reply_to = Some(ReplyTo {
            message: MessageId::numbered(to),
            quote: "a bucket".to_owned(),
        });
        message
    }

    #[test]
    fn the_seat_you_replied_to_answers_first_then_each_seat_you_named() {
        let mut named = replying(yours(3, "Why a bucket? @Luna too."), 1);
        named.mentions = vec![seat(3)];
        let mut events = vec![
            seated(3),
            from_seat(1, 1, "A bucket per key."),
            from_seat(2, 2, "A window is simpler."),
            message(named),
        ];
        assert_eq!(
            next(&council(events.clone())),
            Some((seat(1), FloorReason::You))
        );
        events.push(from_seat(4, 1, "A bucket allows a burst."));
        // Seat 1 answered you, so the seat you named is next, though seat 1's message names
        // nobody.
        assert_eq!(
            next(&council(events.clone())),
            Some((seat(3), FloorReason::You))
        );
        events.push(from_seat(5, 3, "A burst of 20 is fine."));
        assert_eq!(
            next(&council(events)),
            Some((seat(2), FloorReason::Quietest)),
            "with you answered, the quietest writes"
        );
    }

    #[test]
    fn a_reply_gives_the_floor_to_the_seat_it_answers_then_a_mention() {
        let replied = replying(said(2, Speaker::Seat(seat(2)), MessageKind::Talk, "No."), 1);
        let events = vec![seated(3), from_seat(1, 1, "A bucket."), message(replied)];
        assert_eq!(
            next(&council(events)),
            Some((seat(1), FloorReason::RepliedTo { by: seat(2) }))
        );

        let mut naming = said(
            2,
            Speaker::Seat(seat(2)),
            MessageKind::Talk,
            "@M what do you say?",
        );
        naming.mentions = vec![seat(2), seat(3)];
        let events = vec![seated(3), from_seat(1, 1, "A bucket."), message(naming)];
        assert_eq!(
            next(&council(events)),
            Some((seat(3), FloorReason::Named { by: seat(2) })),
            "a seat naming itself doesn't give itself the floor"
        );
    }

    #[test]
    fn a_seat_that_objected_writes_before_the_quietest() {
        let mut touching = said(
            2,
            Speaker::Seat(seat(2)),
            MessageKind::Talk,
            "Point 1 holds.",
        );
        touching.stances = vec![PointStance {
            point: 1,
            stance: Stance::Agree,
        }];
        let events = vec![
            seated(3),
            from_seat(1, 1, "A bucket."),
            message(touching.clone()),
            CouncilEventKind::Tapback {
                message: MessageId::numbered(2),
                seat: seat(3),
                stance: Stance::Object,
            },
        ];
        assert_eq!(
            next(&council(events)),
            Some((seat(3), FloorReason::Objected))
        );

        let events = vec![
            seated(3),
            from_seat(1, 1, "A bucket."),
            CouncilEventKind::Stances {
                seat: seat(1),
                stances: vec![PointStance {
                    point: 1,
                    stance: Stance::Question,
                }],
            },
            message(touching),
        ];
        assert_eq!(
            next(&council(events)),
            Some((seat(1), FloorReason::Objected)),
            "a standing question on the point the message touches counts too"
        );
    }

    #[test]
    fn the_quietest_is_the_fewest_messages_then_quiet_longest_never_the_last_writer() {
        let events = vec![
            seated(3),
            from_seat(1, 1, "One."),
            from_seat(2, 2, "Two."),
            from_seat(3, 1, "Three."),
            from_seat(4, 3, "Four."),
        ];
        // Seat 2 and seat 3 wrote once each, and seat 3 wrote last.
        assert_eq!(
            next(&council(events)),
            Some((seat(2), FloorReason::Quietest))
        );
        let events = vec![seated(2), from_seat(1, 2, "One.")];
        assert_eq!(
            next(&council(events)),
            Some((seat(1), FloorReason::Quietest))
        );
        let alone = vec![seated(1), from_seat(1, 1, "One.")];
        assert_eq!(
            next(&council(alone)),
            Some((seat(1), FloorReason::Quietest))
        );
    }

    #[test]
    fn a_seat_sitting_out_never_gets_the_floor() {
        let mut named = yours(2, "@Luna?");
        named.mentions = vec![seat(2)];
        let events = vec![
            seated(2),
            CouncilEventKind::SatOut { seat: seat(2) },
            from_seat(1, 1, "One."),
            message(named),
        ];
        assert_eq!(
            next(&council(events)),
            Some((seat(1), FloorReason::Quietest))
        );
        let empty = vec![seated(1), CouncilEventKind::SatOut { seat: seat(1) }];
        assert_eq!(next(&council(empty)), None);
    }

    /// Three seats, the third the skeptic, with one point every seat but the skeptic backs.
    fn about_to_close(words: &str) -> Vec<CouncilEventKind> {
        let CouncilEventKind::Seated(mut seating) = seated(3) else {
            unreachable!()
        };
        seating.seats[2].role = SeatRole::Skeptic;
        let mut events = vec![
            CouncilEventKind::Seated(seating),
            from_seat(1, 1, "Calls with no key share one small bucket."),
            CouncilEventKind::Points {
                points: crate::council::points::listed([(words.to_owned(), Vec::new())]),
            },
        ];
        for (n, stance) in [(1, Stance::Agree), (2, Stance::Agree), (3, Stance::Object)] {
            events.push(CouncilEventKind::Stances {
                seat: seat(n),
                stances: vec![PointStance { point: 1, stance }],
            });
        }
        events
    }

    fn backing(id: u32, n: u32, words: &str) -> CouncilEventKind {
        let mut message = said(id, Speaker::Seat(seat(n)), MessageKind::Talk, words);
        message.stances = vec![PointStance {
            point: 1,
            stance: Stance::Agree,
        }];
        CouncilEventKind::Message(Box::new(replying(message, 1)))
    }

    fn challenged() -> Option<(SeatId, FloorReason)> {
        Some((seat(3), FloorReason::Unproven { point: 1 }))
    }

    #[test]
    fn the_skeptic_challenges_a_point_about_to_close_on_a_reason_no_seat_can_read() {
        let mut events = about_to_close("One shared bucket");
        events.push(CouncilEventKind::Moved(Moved {
            seat: seat(2),
            point: 1,
            to: "One shared bucket".to_owned(),
            because: MovedBecause::InPrivate,
        }));
        events.push(backing(
            5,
            2,
            "I now back one shared bucket, see `cargo test`.",
        ));
        // The reply would give seat 1 the floor, and evidence doesn't make up for a reason
        // nobody can read.
        assert_eq!(next(&council(events.clone())), challenged());
        assert_eq!(weak(&council(events.clone()), 1), Some(Weak::InPrivate));

        events.push(CouncilEventKind::Writing {
            seat: seat(3),
            why: FloorReason::Unproven { point: 1 },
        });
        events.push(from_seat(6, 3, "A bucket hides abuse."));
        assert_eq!(
            next(&council(events.clone())),
            Some((seat(1), FloorReason::Quietest)),
            "one challenge for each wording"
        );

        events.push(CouncilEventKind::Points {
            points: crate::council::points::listed([(
                "One shared bucket, 10 a second".to_owned(),
                Vec::new(),
            )]),
        });
        for n in [1, 2] {
            events.push(CouncilEventKind::Stances {
                seat: seat(n),
                stances: vec![PointStance {
                    point: 1,
                    stance: Stance::Agree,
                }],
            });
        }
        events.push(CouncilEventKind::Moved(Moved {
            seat: seat(2),
            point: 1,
            to: "One shared bucket, 10 a second".to_owned(),
            because: MovedBecause::InPrivate,
        }));
        assert_eq!(
            next(&council(events)),
            challenged(),
            "new words may be challenged again"
        );
    }

    #[test]
    fn evidence_named_lets_a_point_close_without_the_skeptics_turn() {
        let mut events = about_to_close("One shared bucket");
        events.push(backing(5, 2, "A bucket is fine for now."));
        assert_eq!(next(&council(events.clone())), challenged());
        assert_eq!(weak(&council(events), 1), Some(Weak::NoEvidence));

        let mut cited = about_to_close("One shared bucket");
        cited.push(backing(5, 2, "test/edge.test.ts fails with the counter."));
        assert_eq!(
            next(&council(cited)),
            Some((seat(1), FloorReason::RepliedTo { by: seat(2) }))
        );

        let mut read = about_to_close("One shared bucket");
        read.push(CouncilEventKind::Moved(Moved {
            seat: seat(2),
            point: 1,
            to: "One shared bucket".to_owned(),
            because: MovedBecause::Read {
                what: "docs/limits.md".to_owned(),
            },
        }));
        read.push(backing(5, 2, "A bucket is fine for now."));
        assert_ne!(next(&council(read)), challenged());

        let mut open = about_to_close("One shared bucket");
        open.push(CouncilEventKind::Stances {
            seat: seat(2),
            stances: vec![PointStance {
                point: 1,
                stance: Stance::Question,
            }],
        });
        assert_ne!(
            next(&council(open)),
            challenged(),
            "a point that isn't about to close needs no challenge"
        );
    }

    #[test]
    fn evidence_is_a_path_a_file_a_link_or_quoted_code() {
        for cited in [
            "test/edge.test.ts fails",
            "as limits.rs says.",
            "Run `cargo test` and see.",
            "https://example.com/docs",
            "./run.sh prints it",
            "crates/crew/src holds it",
        ] {
            assert!(names_evidence(cited), "{cited}");
        }
        for plain in [
            "and/or a window",
            "e.g. a bucket",
            "Opus 5.5 and 3.8 Flash agree",
            "10 a second",
        ] {
            assert!(!names_evidence(plain), "{plain}");
        }
    }

    #[test]
    fn the_salt_is_the_same_only_for_the_same_words() {
        assert_eq!(salt(&["run-1", "talk"]), salt(&["run-1", "talk"]));
        assert_ne!(salt(&["run-1", "talk"]), salt(&["run-2", "talk"]));
        assert_ne!(salt(&["ab", "c"]), salt(&["a", "bc"]));
    }
}
