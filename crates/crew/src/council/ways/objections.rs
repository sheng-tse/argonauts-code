//! A seat's objection to a way you asked for. Your way binds whatever the stances say: a seat
//! may object with a reason, only you change the way, and the objection stays beside the task
//! on the plan, at the approval and in the decision record.

use argonauts_core::base::names::SeatId;
use argonauts_core::runs::events::approval::WayObjection;
use argonauts_core::runs::events::council::{
    CouncilMessage, Point, PointState, Speaker, Stance, TextPart,
};
use argonauts_core::runs::events::task::TaskSpec;

use super::asked::{asked_in, best_match};
use super::{done_by, yours_stands};

/// The most of a seat's words an objection keeps, so it fits on the task's own line.
const REASON_CHARS: usize = 120;

/// Stands in for a seat a reason names while its sentence is found and cut, so a seat stays a
/// seat and each screen names it its own way. Typed words never hold it.
const SEAT: char = '\u{FFFC}';

/// A council's talk as far as objections go: its points, its messages in the order they came,
/// and each seat's latest stance on a point.
pub struct Talk<'a> {
    pub points: &'a [Point],
    pub said: &'a [&'a CouncilMessage],
    pub seats: &'a [SeatId],
    pub stance_of: &'a dyn Fn(&SeatId, u32) -> Option<Stance>,
}

/// Every seat whose latest stance objects to a point of yours that names a way for a task of
/// the plan, while that task is still done that way. `workflows` is every workflow a task could
/// run.
pub fn objections(tasks: &[TaskSpec], talk: &Talk<'_>, workflows: &[String]) -> Vec<WayObjection> {
    let mut found = Vec::new();
    for point in talk.points.iter().filter(|p| p.state == PointState::Yours) {
        let objecting: Vec<&SeatId> = talk
            .seats
            .iter()
            .filter(|seat| (talk.stance_of)(seat, point.number) == Some(Stance::Object))
            .collect();
        if objecting.is_empty() {
            continue;
        }
        for asked in asked_in(&point.words, workflows) {
            let done_so: Vec<&TaskSpec> = tasks
                .iter()
                .filter(|task| {
                    task.way
                        .as_ref()
                        .is_some_and(|way| way.asked_by_you && asked.way.matches(&way.how))
                })
                .collect();
            let task = match done_so.as_slice() {
                [only] => Some(*only),
                many => best_match(many.iter().copied(), &asked.about).map(|at| many[at]),
            };
            let Some(task) = task else { continue };
            for seat in &objecting {
                found.push(WayObjection {
                    task: task.id.clone(),
                    seat: (*seat).clone(),
                    reason: reason(talk.said, seat, point.number),
                });
            }
        }
    }
    found
}

/// The decision record's line for an objection your way outlasted: "Plan limits is done in its
/// own session, as you asked. A seat objected: every request reads this table, so
/// Maker-checker · yours stands". Seats are never named in it, as workers and checkers read the
/// record: a seat its reason names reads "another seat".
pub fn objection_line(task: &TaskSpec, objection: &WayObjection) -> Option<String> {
    let way = task.way.as_ref()?;
    let against = match objection.words(|_| "another seat".to_owned()).as_str() {
        "" => "A seat objected".to_owned(),
        reason => format!("A seat objected: {reason}"),
    };
    Some(format!(
        "{} is done {}, as you asked. {}",
        task.title,
        done_by(&way.how),
        yours_stands(&against)
    ))
}

/// The decision record's lines for every objection your ways outlasted, one per seat.
pub fn objection_lines(tasks: &[TaskSpec], talk: &Talk<'_>, workflows: &[String]) -> Vec<String> {
    objections(tasks, talk, workflows)
        .iter()
        .filter_map(|objection| {
            let task = tasks.iter().find(|task| task.id == objection.task)?;
            objection_line(task, objection)
        })
        .collect()
}

/// The first sentence of `seat`'s newest message that objects to `point`, cut to fit.
fn reason(said: &[&CouncilMessage], seat: &SeatId, point: u32) -> Vec<TextPart> {
    let newest = said.iter().rev().find(|message| {
        message.from == Speaker::Seat(seat.clone())
            && message
                .stances
                .iter()
                .any(|taken| taken.point == point && taken.stance == Stance::Object)
    });
    newest.map_or_else(Vec::new, |message| first_sentence(&message.text))
}

/// The first sentence of `text`, cut to fit, with its seats kept as seats.
fn first_sentence(text: &[TextPart]) -> Vec<TextPart> {
    let mut seats = text.iter().filter_map(|part| match part {
        TextPart::Seat(seat) => Some(seat),
        TextPart::Words(_) => None,
    });
    let flat: String = text
        .iter()
        .map(|part| match part {
            TextPart::Words(words) => words.clone(),
            TextPart::Seat(_) => SEAT.to_string(),
        })
        .collect();
    let mut parts = Vec::new();
    for (at, piece) in sentence_of(&flat).split(SEAT).enumerate() {
        if at > 0
            && let Some(seat) = seats.next()
        {
            parts.push(TextPart::Seat(seat.clone()));
        }
        if !piece.is_empty() {
            parts.push(TextPart::Words(piece.to_owned()));
        }
    }
    parts
}

fn sentence_of(words: &str) -> String {
    let words = words.trim();
    let end = words
        .char_indices()
        .find(|(at, c)| {
            matches!(c, '.' | '!' | '?' | '\n')
                && words[at + c.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map_or(words.len(), |(at, _)| at);
    let sentence = words[..end].trim().trim_end_matches(':');
    match sentence.char_indices().nth(REASON_CHARS) {
        Some((cut, _)) => format!("{}…", sentence[..cut].trim_end()),
        None => sentence.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::TaskId;
    use argonauts_core::base::names::MessageId;
    use argonauts_core::runs::events::council::{MessageKind, PointStance};
    use argonauts_core::runs::events::task::{TaskKind, TaskWay, Way};
    use argonauts_core::workflows::workflow::WorkflowRef;

    fn task(n: u32, title: &str, how: Way, asked_by_you: bool) -> TaskSpec {
        TaskSpec {
            id: TaskId::numbered(n),
            title: title.to_owned(),
            task: format!("Build the {}.", title.to_lowercase()),
            kind: TaskKind::Build,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: None,
            reviewers: Vec::new(),
            limit: Default::default(),
            predictions: None,
            item: None,
            way: Some(TaskWay {
                how,
                asked_by_you,
                steps: None,
                why: None,
            }),
        }
    }

    fn yours(number: u32, words: &str) -> Point {
        Point {
            number,
            words: words.to_owned(),
            state: PointState::Yours,
            raised_by: None,
            sides: Vec::new(),
        }
    }

    fn message(n: u32, seat: &SeatId, text: Vec<TextPart>, against: &[u32]) -> CouncilMessage {
        CouncilMessage {
            id: MessageId::numbered(n),
            from: Speaker::Seat(seat.clone()),
            kind: MessageKind::Talk,
            text,
            reply_to: None,
            mentions: Vec::new(),
            stances: against
                .iter()
                .map(|point| PointStance {
                    point: *point,
                    stance: Stance::Object,
                })
                .collect(),
            hidden: 0,
            hidden_names: Vec::new(),
            guesses_hidden: 0,
            rewritten: false,
            declined: false,
            started_before: None,
        }
    }

    fn workflows() -> Vec<String> {
        vec!["maker-checker".to_owned(), "best-of-n".to_owned()]
    }

    fn maker_checker() -> Way {
        Way::Workflow {
            name: WorkflowRef::new("maker-checker"),
        }
    }

    #[test]
    fn a_seat_against_your_way_is_kept_with_the_first_sentence_of_why() {
        let tasks = [
            task(1, "Redis bucket", maker_checker(), true),
            task(2, "Plan limits", Way::OwnSession, true),
            task(3, "Memory store", Way::OwnSession, false),
        ];
        let points = [
            yours(1, "the bucket by Maker-checker"),
            yours(2, "the plan limits in its own session"),
        ];
        let (one, three) = (SeatId::numbered(1), SeatId::numbered(3));
        let said = [
            message(
                1,
                &three,
                vec![TextPart::Words("An early thought.".to_owned())],
                &[2],
            ),
            message(
                2,
                &three,
                vec![
                    TextPart::Words(
                        "Every request reads this table, so Maker-checker. As ".to_owned(),
                    ),
                    TextPart::Seat(one.clone()),
                    TextPart::Words(" said, it holds.".to_owned()),
                ],
                &[2],
            ),
        ];
        let said: Vec<&CouncilMessage> = said.iter().collect();
        let stance_of = |seat: &SeatId, point: u32| {
            (*seat == SeatId::numbered(3) && point == 2).then_some(Stance::Object)
        };
        let seats = [one, three.clone()];
        let talk = Talk {
            points: &points,
            said: &said,
            seats: &seats,
            stance_of: &stance_of,
        };
        let found = objections(&tasks, &talk, &workflows());
        assert_eq!(
            found,
            [WayObjection {
                task: TaskId::numbered(2),
                seat: three,
                reason: vec![TextPart::Words(
                    "Every request reads this table, so Maker-checker".to_owned()
                )],
            }]
        );
        assert_eq!(
            objection_line(&tasks[1], &found[0]).unwrap(),
            "Plan limits is done in its own session, as you asked. A seat objected: Every \
             request reads this table, so Maker-checker · yours stands"
        );
    }

    #[test]
    fn a_way_you_changed_at_the_approval_leaves_the_objection_behind() {
        // You asked for Maker-checker, a seat objected, and you changed it to Best-of-n.
        let tasks = [task(
            1,
            "Redis bucket",
            Way::Workflow {
                name: WorkflowRef::new("best-of-n"),
            },
            true,
        )];
        let points = [yours(1, "the bucket by Maker-checker")];
        let seat = SeatId::numbered(2);
        let stance_of = |_: &SeatId, _: u32| Some(Stance::Object);
        let seats = [seat];
        let talk = Talk {
            points: &points,
            said: &[],
            seats: &seats,
            stance_of: &stance_of,
        };
        assert!(objections(&tasks, &talk, &workflows()).is_empty());
    }

    #[test]
    fn a_tapback_against_your_way_is_kept_without_words() {
        let tasks = [task(1, "Redis bucket", maker_checker(), true)];
        let points = [yours(1, "the bucket by Maker-checker")];
        let stance_of = |_: &SeatId, _: u32| Some(Stance::Object);
        let seats = [SeatId::numbered(1)];
        let talk = Talk {
            points: &points,
            said: &[],
            seats: &seats,
            stance_of: &stance_of,
        };
        let found = objections(&tasks, &talk, &workflows());
        assert_eq!(found.len(), 1);
        assert!(found[0].reason.is_empty());
        assert_eq!(
            objection_line(&tasks[0], &found[0]).unwrap(),
            "Redis bucket is done by Maker-checker, as you asked. A seat objected · yours stands"
        );
    }

    #[test]
    fn the_record_names_no_seat_a_reason_names() {
        let tasks = [task(1, "Plan limits", Way::OwnSession, true)];
        let points = [yours(1, "the plan limits in its own session")];
        let (one, two) = (SeatId::numbered(1), SeatId::numbered(2));
        let said = [message(
            1,
            &two,
            vec![
                TextPart::Words("As ".to_owned()),
                TextPart::Seat(one.clone()),
                TextPart::Words(
                    " said, every request reads this table. So Maker-checker.".to_owned(),
                ),
            ],
            &[1],
        )];
        let said: Vec<&CouncilMessage> = said.iter().collect();
        let stance_of =
            |seat: &SeatId, _: u32| (*seat == SeatId::numbered(2)).then_some(Stance::Object);
        let seats = [one, two];
        let talk = Talk {
            points: &points,
            said: &said,
            seats: &seats,
            stance_of: &stance_of,
        };
        assert_eq!(
            objection_lines(&tasks, &talk, &workflows()),
            [
                "Plan limits is done in its own session, as you asked. A seat objected: As another \
              seat said, every request reads this table · yours stands"
            ]
        );
    }

    #[test]
    fn a_reason_keeps_the_seats_it_names_for_each_screen_to_name() {
        let (one, two) = (SeatId::numbered(1), SeatId::numbered(2));
        let text = [
            TextPart::Seat(one.clone()),
            TextPart::Words(" and ".to_owned()),
            TextPart::Seat(two.clone()),
            TextPart::Words(" missed it. Later, ".to_owned()),
            TextPart::Seat(one.clone()),
            TextPart::Words(" agreed.".to_owned()),
        ];
        assert_eq!(
            first_sentence(&text),
            [
                TextPart::Seat(one),
                TextPart::Words(" and ".to_owned()),
                TextPart::Seat(two),
                TextPart::Words(" missed it".to_owned()),
            ]
        );
    }

    #[test]
    fn a_long_reason_is_cut_to_fit_its_line() {
        let long = format!("{} and more.", "word ".repeat(40));
        let cut = sentence_of(&long);
        assert!(cut.ends_with('…') && cut.chars().count() <= REASON_CHARS + 1);
        assert_eq!(sentence_of("Version 2.1 holds. Then"), "Version 2.1 holds");
    }
}
