//! A council's seats as rows under its request while it talks and has no tasks yet: each
//! seat's mark, its logo and name, what it does now in the bar's place, and the time and
//! tokens of its own sessions. The rows show whether or not the agents' rows do.

use std::time::Instant;

use argonauts_core::base::names::SeatId;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::look::logos::logo_for;
use argonauts_core::runs::events::approval::{ApprovalOpened, ApprovalPacket, CouncilQuestion};
use argonauts_core::runs::events::council::Seat;
use time::OffsetDateTime;

use super::{agent_ref, time_of};
use crate::bottom::{RunRowKind, WorkflowRow, WorkflowRowState};
use crate::frame::state::{RunState, SeatDoing, SeatNow, SessionState, Talk};

/// A row for each seat of `talk`, in seat order.
pub(super) fn rows(
    run: &RunState,
    talk: &Talk,
    names: &DisplayNames,
    wall: OffsetDateTime,
    now: Instant,
) -> Vec<WorkflowRow> {
    let labels: Vec<(SeatId, String)> = talk
        .seats()
        .map(|seat| (seat.seat.id.clone(), label(seat.seat, names)))
        .collect();
    let named = |seat: &SeatId| {
        labels
            .iter()
            .find(|(id, _)| id == seat)
            .map_or_else(|| "a seat".to_owned(), |(_, label)| label.clone())
    };
    let objections = |seat: &SeatId| objections(run, seat);
    talk.seats()
        .map(|seat| {
            let sessions: Vec<&SessionState> = run
                .sessions_in_order()
                .filter(|session| session.seat.as_ref() == Some(&seat.seat.id))
                .collect();
            let key = match sessions.last() {
                Some(session) => format!("{}#{}", run.id, session.key),
                None => format!("{}#{}", run.id, seat.seat.id),
            };
            let mut row = WorkflowRow::new(key, named(&seat.seat.id), state(&seat));
            row.kind = RunRowKind::Work;
            row.depth = 1;
            row.answers = seat
                .held
                .then(|| super::question_of(run, |opened| holds(talk, opened, &seat.seat.id)))
                .flatten();
            row.logo = Some(logo_for(
                &seat.seat.model.agent,
                seat.seat.model.model.as_deref(),
            ));
            row.number = seat.seat.number;
            row.model_named = true;
            row.doing = Some(words(&seat, &named, objections(&seat.seat.id), names));
            row.tokens = sessions.iter().map(|session| session.tokens_out).sum();
            row.session = sessions.last().map(|session| agent_ref(run, session));
            time_of(&mut row, &sessions, wall, now);
            row
        })
        .collect()
}

/// The seat's name as every council row writes it: its model beside its logo, and its place
/// among the seats of that model, `Opus 5.5 · 2`, when the model sits more than once.
fn label(seat: &Seat, names: &DisplayNames) -> String {
    crate::council::Face::of_seat(seat, names).label()
}

/// Whether the question asks what happens to a message or the plan of the seat that's held for
/// you.
fn holds(talk: &Talk, opened: &ApprovalOpened, seat: &SeatId) -> bool {
    let ApprovalPacket::Council(packet) = &opened.packet else {
        return false;
    };
    match &packet.about {
        CouncilQuestion::Held { message, .. } => talk.held_by(message) == Some(seat),
        CouncilQuestion::HeldPlan { keeper, .. } => keeper == seat,
        CouncilQuestion::Point { .. } => false,
    }
}

/// ✔ once a seat backs every point, ◆ while a message of its waits on you, and ○ otherwise.
fn state(seat: &SeatNow<'_>) -> WorkflowRowState {
    let (backed, points) = seat.backs;
    if seat.held {
        WorkflowRowState::NeedsYou
    } else if points > 0 && backed == points {
        WorkflowRowState::Done
    } else {
        WorkflowRowState::Running
    }
}

/// What the seat's row says in its bar's place: what it does now, else where it stands.
fn words(
    seat: &SeatNow<'_>,
    named: &dyn Fn(&SeatId) -> String,
    objections: usize,
    names: &DisplayNames,
) -> String {
    let (backed, points) = seat.backs;
    if seat.held && points == 0 {
        return "Held for you".to_owned();
    }
    if seat.held {
        return format!("{}, held for you", backs(backed, points, false));
    }
    match &seat.doing {
        SeatDoing::FirstAnswer => "Writing its first answer".to_owned(),
        SeatDoing::Answered => "Answered".to_owned(),
        SeatDoing::GatheringPoints => "Gathering the points".to_owned(),
        SeatDoing::Answering(None) => "Answering you".to_owned(),
        SeatDoing::Answering(Some(by)) => format!("Answering {}", named(by)),
        SeatDoing::Writing => "Writing".to_owned(),
        SeatDoing::Rewriting => "Writing it again".to_owned(),
        SeatDoing::WritingPlan => "Writing the plan".to_owned(),
        SeatDoing::WrotePlan => "Wrote the plan".to_owned(),
        SeatDoing::CarriedOn => format!("Carries on in {}", names.cli(&seat.seat.model.agent)),
        SeatDoing::Standing if seat.signed => match objections {
            0 => "Signed the plan".to_owned(),
            1 => "Signed, 1 objection".to_owned(),
            many => format!("Signed, {many} objections"),
        },
        SeatDoing::Standing => backs(backed, points, true),
    }
}

/// Where a seat stands on the points: `Backs all 6 points`, `Backs 5 of 6 points`, or without
/// the word `points`, as a held message's row says it, `Backs all 6`.
fn backs(backed: u32, points: u32, whole: bool) -> String {
    let noun = if points == 1 { "point" } else { "points" };
    match (backed == points, points) {
        (_, 0) => "Waits its turn".to_owned(),
        (true, 1) => "Backs the point".to_owned(),
        (true, 2) => format!("Backs both {noun}"),
        (true, _) if whole => format!("Backs all {points} {noun}"),
        (true, _) => format!("Backs all {points}"),
        (false, _) if whole => format!("Backs {backed} of {points} {noun}"),
        (false, _) => format!("Backs {backed} of {points}"),
    }
}

/// The ways `seat` objects to in the plan waiting on you.
fn objections(run: &RunState, seat: &SeatId) -> usize {
    run.approvals
        .iter()
        .filter_map(|open| match &open.opened.packet {
            ApprovalPacket::Plan(plan) => Some(plan),
            _ => None,
        })
        .flat_map(|plan| &plan.objections)
        .filter(|objection| objection.seat == *seat)
        .count()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use argonauts_core::RunId;
    use argonauts_core::app::AppEvent;
    use argonauts_core::base::names::{MessageId, RoleName, SeatId, SessionKey, WorkflowId};
    use argonauts_core::runs::events::council::{
        CouncilEvent, CouncilEventKind, CouncilMessage, FloorReason, Held, HeldWhy, MessageKind,
        Point, PointStance, PointState, Seat, SeatRole, Seating, Speaker, Stance, TextPart,
    };
    use argonauts_core::runs::events::{RunEvent, RunEventKind};
    use argonauts_core::workflows::workflow::StepRef;
    use time::OffsetDateTime;

    use crate::base::draw::render::{Clock, RenderCtx};
    use crate::base::theme::Theme;
    use crate::bottom::{AgentPanel, WorkflowPanel, panel_lines};
    use crate::frame::state::{AppState, RunState, SessionState, SessionStatus};

    const T0: OffsetDateTime = time::macros::datetime!(2026-10-14 10:41 UTC);
    const OPUS: &str = "claude-code/claude-opus-5-5";
    const LUNA: &str = "codex/gpt-5.6-luna";
    const FLASH: &str = "antigravity-cli/gemini-3.8-flash";
    const QWEN: &str = "opencode/ollama/qwen3-coder:30b";

    /// A council at the table: the run, its seats, and the events it has had.
    struct Table {
        state: AppState,
        run: RunId,
        seq: u64,
    }

    impl Table {
        /// `models` sit in seat order, seat 1 keeping the notes, in a request `seconds` old; a
        /// model named twice sits numbered.
        fn seated(models: &[&str], seconds: i64) -> Self {
            let mut run = RunState::new(RunId::new("run-limit").unwrap());
            run.title = "Rate limit the API".into();
            run.started = Some(T0 - time::Duration::seconds(seconds));
            let mut state = AppState::default();
            let id = run.id.clone();
            state.runs.insert(id.clone(), run);
            let mut table = Self {
                state,
                run: id,
                seq: 0,
            };
            let seats = (1..)
                .zip(models)
                .map(|(n, model)| {
                    let twice = models.iter().filter(|other| *other == model).count() > 1;
                    let number = models[..n as usize - 1]
                        .iter()
                        .filter(|other| *other == model)
                        .count() as u8
                        + 1;
                    Seat {
                        id: seat(n),
                        model: model.parse().unwrap(),
                        role: SeatRole::default(),
                        number: twice.then_some(number),
                    }
                })
                .collect();
            table.tell(CouncilEventKind::Seated(Seating {
                seats,
                notes: Some(seat(1)),
            }));
            table
        }

        fn tell(&mut self, kind: CouncilEventKind) {
            self.seq += 1;
            self.state.apply(&AppEvent::Run {
                event: Box::new(RunEvent {
                    seq: self.seq,
                    ts: T0,
                    run_id: self.run.clone(),
                    task_id: None,
                    session: None,
                    source: "argonauts-code".into(),
                    kind: RunEventKind::Council(CouncilEvent {
                        talk: StepRef {
                            workflow: WorkflowId::numbered(1),
                            phase: 0,
                            path: vec![0],
                        },
                        kind,
                    }),
                }),
            });
        }

        /// Seat `n`'s session: `seconds` of work and `tokens` out, still writing when `at_work`.
        fn session(&mut self, n: u32, model: &str, seconds: i64, tokens: u64, at_work: bool) {
            let run = self.state.runs.get_mut(&self.run).unwrap();
            let worker: argonauts_core::base::names::ModelRef = model.parse().unwrap();
            let key = SessionKey::numbered(n);
            let started = T0 - time::Duration::seconds(if at_work { seconds } else { 280 });
            let mut session = SessionState::new(
                key.clone(),
                worker.clone(),
                worker.agent.clone(),
                RoleName::Worker,
                started,
            );
            session.seat = Some(seat(n));
            session.tokens_out = tokens;
            if at_work {
                session.status = SessionStatus::Working;
            } else {
                session.status = SessionStatus::Done;
                session.ended = Some(started + time::Duration::seconds(seconds));
            }
            run.session_order.push(key.clone());
            run.sessions.insert(key, session);
        }

        /// A question of the run's opens, waiting on you.
        fn asks(&mut self, opened: argonauts_core::runs::events::approval::ApprovalOpened) {
            let run = self.state.runs.get_mut(&self.run).unwrap();
            run.approvals.push(crate::frame::state::OpenApproval {
                opened: Box::new(opened),
                set_aside: false,
                replayed: false,
            });
        }

        fn message(&mut self, n: u32, kind: MessageKind) {
            self.tell(CouncilEventKind::Message(Box::new(CouncilMessage {
                id: MessageId::numbered(u32::try_from(self.seq).unwrap() + 1),
                from: Speaker::Seat(seat(n)),
                kind,
                text: vec![TextPart::Words("…".into())],
                reply_to: None,
                mentions: Vec::new(),
                stances: Vec::new(),
                hidden: 0,
                hidden_names: Vec::new(),
                guesses_hidden: 0,
                rewritten: false,
                declined: false,
                started_before: None,
            })));
        }

        fn points(&mut self, open: u32) {
            self.tell(CouncilEventKind::Points {
                points: (1..=open)
                    .map(|number| Point {
                        number,
                        words: format!("Point {number}"),
                        state: PointState::Open,
                        sides: Vec::new(),
                        raised_by: None,
                    })
                    .collect(),
            });
        }

        /// Seat `n` agrees with every point but those in `not`.
        fn backs(&mut self, n: u32, points: u32, not: &[u32]) {
            self.tell(CouncilEventKind::Stances {
                seat: seat(n),
                stances: (1..=points)
                    .map(|point| PointStance {
                        point,
                        stance: if not.contains(&point) {
                            Stance::Question
                        } else {
                            Stance::Agree
                        },
                    })
                    .collect(),
            });
        }

        /// The logos each of the seats' rows draws, as [`Table::rows`] draws them.
        fn logos(&self) -> Vec<Vec<String>> {
            let now = Instant::now();
            let panel = WorkflowPanel {
                rows: super::super::run_rows(&self.state, T0, now),
                max_rows: 10,
                ..WorkflowPanel::default()
            };
            let theme = crate::base::kit::logo::golden::pictured(&Theme::dark());
            let ctx = RenderCtx::new(120, &theme, Clock::still(now));
            panel_lines(&panel, &AgentPanel::default(), &ctx)
                .iter()
                .map(crate::base::kit::logo::golden::logos_drawn)
                .collect()
        }

        /// The seats' rows as the panel draws them at 120 columns, as a terminal that draws
        /// pictures draws them, as the council's screens are.
        fn rows(&self) -> Vec<String> {
            self.rows_as(
                &crate::base::kit::logo::golden::pictured(&Theme::dark()),
                120,
            )
        }

        /// The seats' rows as the panel draws them `width` columns wide in `theme`, logos as
        /// the screens write them.
        fn rows_as(&self, theme: &Theme, width: u16) -> Vec<String> {
            let now = Instant::now();
            let panel = WorkflowPanel {
                rows: super::super::run_rows(&self.state, T0, now),
                max_rows: 10,
                ..WorkflowPanel::default()
            };
            let ctx = RenderCtx::new(width, theme, Clock::still(now));
            panel_lines(&panel, &AgentPanel::default(), &ctx)
                .iter()
                .map(|line| {
                    crate::base::kit::logo::golden::council_text(line)
                        .trim_end()
                        .to_owned()
                })
                .collect()
        }
    }

    fn seat(n: u32) -> SeatId {
        SeatId::numbered(n)
    }

    /// The request's and the seats' rows of a council screen, rows 34 to 38.
    fn screen(name: &str) -> Vec<String> {
        let path = format!(
            "{}/tests/golden/council/{name}.txt",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .skip(33)
            .take(5)
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    /// The request's row and its seats' rows match the screen's cell for cell.
    #[track_caller]
    fn assert_reads_as(table: &Table, name: &str) {
        assert_rows_from(table, name, 0);
    }

    /// The rows from `first` on, 0 being the request's, match the screen's cell for cell.
    #[track_caller]
    fn assert_rows_from(table: &Table, name: &str, first: usize) {
        assert_drawn_as(&table.rows()[first..], name, first);
    }

    /// The rows from `first` on, as drawn, match the screen's cell for cell.
    #[track_caller]
    fn assert_drawn_as(rows: &[String], name: &str, first: usize) {
        if crate::council::tests::golden::record(name, rows, 33 + first) {
            return;
        }
        let want = &screen(name)[first..];
        assert_eq!(rows.len(), want.len(), "{rows:#?}");
        for (got, want) in rows.iter().zip(want) {
            assert_eq!(got, want, "\n  got  |{got}|\n  want |{want}|");
        }
    }

    /// Before the points, each seat writes its first answer alone.
    #[test]
    fn every_seat_writes_its_first_answer_alone() {
        let mut table = Table::seated(&[OPUS, LUNA, FLASH, QWEN], 14);
        for (n, model, tokens) in [
            (1, OPUS, 1_100),
            (2, LUNA, 840),
            (3, FLASH, 760),
            (4, QWEN, 510),
        ] {
            table.session(n, model, 14, tokens, true);
            table.tell(CouncilEventKind::Writing {
                seat: seat(n),
                why: FloorReason::FirstAnswer,
            });
        }
        assert_reads_as(&table, "sent");
        // Each seat wears the logo of whoever serves it, the local model Ollama's.
        assert_eq!(
            table.logos(),
            [
                vec![],
                vec!["claudecode".to_owned()],
                vec!["codex".to_owned()],
                vec!["antigravity".to_owned()],
                vec!["ollama".to_owned()],
            ]
        );
    }

    /// Once every first answer is in, the note keeper gathers the points, and the rest say
    /// they answered, each with its own time.
    #[test]
    fn the_note_keeper_gathers_the_points_once_every_seat_answered() {
        let mut table = Table::seated(&[OPUS, LUNA, FLASH, QWEN], 108);
        table.session(1, OPUS, 77, 9_800, true);
        table.session(2, LUNA, 52, 6_100, false);
        table.session(3, FLASH, 90, 11_200, false);
        table.session(4, QWEN, 96, 7_400, false);
        for n in 1..=4 {
            table.message(n, MessageKind::FirstAnswer);
        }
        assert_reads_as(&table, "first_answers");
    }

    /// While they talk, a seat that backs every point reads ✔, the rest how many they back,
    /// and the seat with the floor whom it answers.
    #[test]
    fn each_seat_says_how_many_points_it_backs_or_whom_it_answers() {
        let mut table = Table::seated(&[OPUS, LUNA, FLASH, QWEN], 200);
        table.session(1, OPUS, 118, 19_600, false);
        table.session(2, LUNA, 66, 12_400, false);
        table.session(3, FLASH, 122, 17_100, false);
        table.session(4, QWEN, 112, 12_100, true);
        table.points(6);
        table.backs(1, 6, &[]);
        table.backs(2, 6, &[4]);
        table.backs(3, 6, &[]);
        table.backs(4, 6, &[4]);
        table.tell(CouncilEventKind::Writing {
            seat: seat(4),
            why: FloorReason::RepliedTo { by: seat(3) },
        });
        assert_reads_as(&table, "talk");
        // The same talk drawn as blocks, as a transcript where no picture draws, and at 80
        // columns.
        assert_reads_as(&table, "blocks");
        assert_drawn_as(&table.rows_as(&Theme::dark(), 120), "transcript", 0);
        let pictured = crate::base::kit::logo::golden::pictured(&Theme::dark());
        assert_drawn_as(&table.rows_as(&pictured, 80), "talk_at_80_columns", 0);
    }

    /// A council of `models` `age` seconds into its request, talking over six points: each
    /// seat's session its seconds of work, its tokens out and whether it writes now, and each
    /// seat backing every point but those it doesn't.
    fn talked(
        models: &[&str],
        age: i64,
        sessions: [(i64, u64, bool); 4],
        not_backed: [&[u32]; 4],
    ) -> Table {
        let mut table = Table::seated(models, age);
        for (n, ((seconds, tokens, at_work), model)) in (1..).zip(sessions.into_iter().zip(models))
        {
            table.session(n, model, seconds, tokens, at_work);
        }
        table.points(6);
        for (n, not) in (1..).zip(not_backed) {
            table.backs(n, 6, not);
        }
        table
    }

    fn answering(table: &mut Table, n: u32, by: u32) {
        table.tell(CouncilEventKind::Writing {
            seat: seat(n),
            why: FloorReason::RepliedTo { by: seat(by) },
        });
    }

    /// Each screen of the talk shows its seats where they stand: one answering a seat while
    /// the rest say how many points they back, a model that sits twice by its number, and the
    /// note keeper writing the plan once every seat backs every point.
    #[test]
    fn every_talk_screen_shows_its_seats_as_they_stand() {
        let mut proposal = talked(
            &[OPUS, LUNA, FLASH, QWEN],
            150,
            [
                (100, 14_100, true),
                (66, 8_900, false),
                (90, 13_000, false),
                (96, 10_000, false),
            ],
            [&[], &[5, 6], &[], &[5, 6]],
        );
        answering(&mut proposal, 1, 2);
        assert_reads_as(&proposal, "proposal");

        let mut you_reply = talked(
            &[OPUS, LUNA, FLASH, QWEN],
            206,
            [
                (118, 19_600, false),
                (66, 12_400, false),
                (122, 17_100, false),
                (118, 12_900, true),
            ],
            [&[], &[4], &[], &[4]],
        );
        answering(&mut you_reply, 4, 3);
        assert_reads_as(&you_reply, "you_reply");

        let mut twice = talked(
            &[OPUS, OPUS, LUNA, FLASH],
            170,
            [
                (118, 18_900, false),
                (100, 16_200, false),
                (72, 17_300, true),
                (90, 13_000, false),
            ],
            [&[], &[6], &[6], &[]],
        );
        answering(&mut twice, 3, 1);
        assert_reads_as(&twice, "two_seats_one_model");
        assert_drawn_as(
            &twice.rows_as(&Theme::dark(), 120),
            "two_seats_one_model_transcript",
            0,
        );

        let mut moved = talked(
            &[OPUS, OPUS, LUNA, FLASH],
            242,
            [
                (151, 24_000, false),
                (140, 21_800, true),
                (112, 24_600, false),
                (101, 17_200, false),
            ],
            [&[], &[6], &[], &[]],
        );
        answering(&mut moved, 2, 3);
        assert_reads_as(&moved, "moved_in_private");

        let mut agreed = talked(
            &[OPUS, LUNA, FLASH, QWEN],
            280,
            [
                (151, 25_000, true),
                (74, 16_100, false),
                (160, 23_900, false),
                (145, 17_900, false),
            ],
            [&[], &[], &[], &[]],
        );
        agreed.tell(CouncilEventKind::Agreed { points: 6 });
        assert_reads_as(&agreed, "everyone_agreed");
    }

    /// A seat you named or replied to answers you.
    #[test]
    fn a_seat_you_wrote_to_answers_you() {
        let mut table = Table::seated(&[OPUS, LUNA, FLASH, QWEN], 211);
        table.session(1, OPUS, 118, 19_900, false);
        table.session(2, LUNA, 66, 12_600, false);
        table.session(3, FLASH, 125, 17_600, true);
        table.session(4, QWEN, 123, 13_300, true);
        table.points(6);
        for n in 1..=4 {
            table.backs(n, 6, &[6]);
        }
        table.tell(CouncilEventKind::Writing {
            seat: seat(4),
            why: FloorReason::RepliedTo { by: seat(3) },
        });
        table.tell(CouncilEventKind::Writing {
            seat: seat(3),
            why: FloorReason::You,
        });
        // Two seats write at once here: the floor is one seat's, so the one answering 3.8
        // Flash reads where it stands once you take the floor's turn from it.
        assert_reads_as(&table, "you_mid_talk");
    }

    /// Once the plan is written, its writer says so, the other seats signed it, and a seat
    /// that objects to a way you asked for says how many.
    #[test]
    fn the_plan_is_written_and_signed_with_each_seats_objections() {
        let mut table = Table::seated(&[OPUS, LUNA, FLASH, QWEN], 310);
        table.session(1, OPUS, 181, 29_000, false);
        table.session(2, LUNA, 78, 16_900, false);
        table.session(3, FLASH, 164, 25_300, false);
        table.session(4, QWEN, 149, 19_600, false);
        table.points(6);
        for n in 1..=4 {
            table.backs(n, 6, &[]);
        }
        table.tell(CouncilEventKind::Agreed { points: 6 });
        let writing = table.rows();
        assert!(writing[1].contains("Writing the plan"), "{writing:#?}");
        table.message(1, MessageKind::Notes);
        table.asks(plan_with_objection(seat(3)));
        assert_reads_as(&table, "plan");
    }

    /// Approving with edits keeps the plan's seats as they stood, the run's row waiting on
    /// you.
    #[test]
    fn approving_with_edits_keeps_the_plans_seats() {
        let mut table = talked(
            &[OPUS, LUNA, FLASH, QWEN],
            322,
            [
                (181, 29_000, false),
                (78, 16_900, false),
                (164, 25_300, false),
                (149, 19_600, false),
            ],
            [&[], &[], &[], &[]],
        );
        table.tell(CouncilEventKind::Agreed { points: 6 });
        table.message(1, MessageKind::Notes);
        table.asks(plan_with_objection(seat(3)));
        assert_reads_as(&table, "approve_with_edits");
    }

    /// A point left open when the talk stopped asks you, so the run's row waits on you while
    /// each seat says how many points it backs.
    #[test]
    fn a_point_left_open_waits_on_you() {
        use argonauts_core::runs::events::approval::CouncilQuestion;
        use argonauts_core::runs::events::approval::{ApprovalKind, ApprovalPacket, CouncilPacket};

        let table_of = |age| {
            talked(
                &[OPUS, LUNA, FLASH, QWEN],
                age,
                [
                    (160, 35_000, false),
                    (112, 24_400, false),
                    (171, 33_100, false),
                    (167, 25_500, false),
                ],
                [&[], &[6], &[], &[6]],
            )
        };
        let mut table = table_of(362);
        let mut point = plan_with_objection(seat(1));
        point.kind = ApprovalKind::Question;
        point.packet = ApprovalPacket::Council(CouncilPacket {
            talk: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 0,
                path: vec![0],
            },
            about: CouncilQuestion::Point { point: 6 },
            text: "Calls with no key: which way?".into(),
        });
        table.asks(point);
        assert_reads_as(&table, "no_agreement");
    }

    /// A model that sits twice reads by its number, and a seat whose message is held reads
    /// ◆ until you choose what happens to it.
    #[test]
    fn a_held_message_marks_its_seat_until_you_choose() {
        use argonauts_core::runs::events::approval::{
            ApprovalPacket, CouncilPacket, CouncilQuestion,
        };

        let mut table = Table::seated(&[OPUS, OPUS, LUNA, FLASH], 271);
        table.session(1, OPUS, 151, 24_000, false);
        table.session(2, OPUS, 169, 25_100, false);
        table.session(3, LUNA, 139, 27_900, false);
        table.session(4, FLASH, 101, 17_200, false);
        table.points(6);
        table.backs(1, 6, &[]);
        table.backs(2, 6, &[3]);
        table.backs(3, 6, &[]);
        table.backs(4, 6, &[]);
        table.message(3, MessageKind::Talk);
        let held = MessageId::numbered(u32::try_from(table.seq).unwrap());
        table.tell(CouncilEventKind::Held(Box::new(Held {
            message: held.clone(),
            seat: seat(3),
            private: MessageId::numbered(1),
            why: HeldWhy::WordsInARow { count: 17 },
            matching: Vec::new(),
            turn: Vec::new(),
        })));
        // The screen's request row waits on the question the held message asks, which this
        // table doesn't open, so its seats' rows are compared alone.
        assert_rows_from(&table, "held_message", 1);
        // Once it opens, Enter on the seat's row brings it up, though the plan's asks first.
        let mut asks = plan_with_objection(seat(1));
        asks.id = argonauts_core::base::names::ApprovalId::numbered(2);
        asks.kind = argonauts_core::runs::events::approval::ApprovalKind::Question;
        asks.packet = ApprovalPacket::Council(CouncilPacket {
            talk: StepRef {
                workflow: WorkflowId::numbered(1),
                phase: 0,
                path: vec![0],
            },
            about: CouncilQuestion::Held {
                message: held.clone(),
                why: None,
                wrote_at: None,
            },
            text: "Send it as written?".into(),
        });
        table.asks(plan_with_objection(seat(1)));
        table.asks(asks);
        // With its question open the run's row waits on you too.
        assert_reads_as(&table, "held_message");
        let rows = super::super::run_rows(&table.state, T0, Instant::now());
        let numbered = |n| Some(argonauts_core::base::names::ApprovalId::numbered(n));
        let answers: Vec<_> = rows.iter().map(|row| row.answers.clone()).collect();
        assert_eq!(
            answers,
            [numbered(1), None, None, numbered(2), None],
            "{rows:#?}"
        );
        table.tell(CouncilEventKind::Released {
            message: held,
            choice: argonauts_core::runs::events::council::HeldChoice::Drop,
        });
        let rows = table.rows();
        assert!(rows[3].contains("✔  ▐▌ 5.6 Luna"), "{rows:#?}");
        assert!(rows[3].contains("Backs all 6 points"), "{rows:#?}");
    }

    fn plan_with_objection(by: SeatId) -> argonauts_core::runs::events::approval::ApprovalOpened {
        use argonauts_core::base::names::ApprovalId;
        use argonauts_core::runs::events::approval::{
            ApprovalKind, ApprovalOpened, ApprovalPacket, FreeText, PlanPacket, WayObjection,
        };
        use argonauts_core::runs::events::plan::{LimitPlan, TaskGraph};
        use argonauts_core::settings::approvals::{ApprovalMode, DecideWith, WhenDeclined};
        use argonauts_core::settings::limit::{LimitAmount, LimitUnit};

        let packet = PlanPacket {
            summary: "Rate limit the API".into(),
            judging: None,
            near_tie: false,
            decisions: Vec::new(),
            graph: TaskGraph {
                tasks: Vec::new(),
                critical_path: Vec::new(),
            },
            acceptance_tests: Vec::new(),
            limit: LimitPlan {
                lines: Vec::new(),
                total: LimitAmount {
                    unit: LimitUnit::Usd,
                    amount: 20.0,
                },
                estimate_low: 0.0,
                estimate_high: 0.0,
            },
            new_dependencies: Vec::new(),
            flags: Vec::new(),
            workflow: None,
            seats: Vec::new(),
            objections: vec![WayObjection {
                task: argonauts_core::TaskId::numbered(2),
                seat: by,
                reason: Vec::new(),
            }],
            offer: None,
        };
        ApprovalOpened {
            id: ApprovalId::numbered(1),
            kind: ApprovalKind::Plan,
            title: "Approve this plan?".into(),
            task: None,
            packet: ApprovalPacket::Plan(Box::new(packet)),
            evidence_sha256: "0".repeat(64),
            mode: ApprovalMode::Supervised,
            decide_with: DecideWith::Rules,
            when_declined: WhenDeclined::Decide,
            options: Vec::new(),
            free_text: FreeText::Guidance,
            needs_you: true,
        }
    }
}
