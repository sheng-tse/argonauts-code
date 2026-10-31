//! A council's questions as the screens ask them, opened through the bridge as the app opens
//! them, then focused: a held message, a point left open, and the plan. Each follows every run
//! question: the rule above, a blank row under the title, Carry on without me last, and the
//! keys, where Esc goes back to the list. Each question as drawn is its own golden, and its
//! screen holds it whole in the input's place, as a question that has the keys stands in for
//! the input.

use argonauts_core::base::names::{ApprovalId, MessageId};
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalEvent, ApprovalKind, ApprovalOpened, ApprovalPacket,
    CouncilPacket, CouncilQuestion, FreeText,
};
use argonauts_core::runs::events::council::{HeldWhy, Point, PointState, Side};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;

use super::*;
use crate::council::Look;
use crate::council::tests::golden;
use crate::council::tests::story;

const OPUS: (&str, &str) = ("claude-code", "claude-opus-5-5");
const LUNA: (&str, &str) = ("codex", "gpt-5.6-luna");
const FLASH: (&str, &str) = ("antigravity-cli", "gemini-3.8-flash");
const QWEN: (&str, &str) = ("opencode", "qwen3-coder:30b");

/// A council of `models` at the table, seat 1 keeping the notes.
fn seated(models: &[(&str, &str)]) -> (Bridge, ViewModel) {
    let mut bridge = bridge();
    let mut model = ViewModel::default();
    tasks::happen(&mut bridge, &mut model, 0, tasks::started(), None, 0);
    send(
        &mut bridge,
        &mut model,
        1,
        CouncilEventKind::Seated(Seating {
            seats: (1..)
                .zip(models)
                .map(|(n, (agent, model))| seat(n, agent, model))
                .collect(),
            notes: Some(SeatId::numbered(1)),
        }),
    );
    (bridge, model)
}

fn choose(label: &str, detail: Option<&str>) -> AnswerOption {
    AnswerOption {
        key: 'a',
        label: label.to_owned(),
        action: AnswerAction::Choose {
            value: label.to_owned(),
        },
        recommended: false,
        detail: detail.map(str::to_owned),
    }
}

fn opened(
    kind: ApprovalKind,
    title: &str,
    packet: ApprovalPacket,
    options: Vec<AnswerOption>,
) -> RunEventKind {
    RunEventKind::Approval(ApprovalEvent::Opened(Box::new(ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind,
        title: title.to_owned(),
        task: None,
        packet,
        evidence_sha256: "0".repeat(64),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    })))
}

/// The question as drawn once it has the keys, at 120 columns, as a terminal that draws
/// pictures draws it, the council's screens' way, each logo written as they write it.
fn asked(model: &mut ViewModel) -> Vec<String> {
    let question = model.bottom.question.as_mut().expect("a question");
    question.focused = true;
    let theme = crate::base::kit::logo::golden::pictured(&Theme::dark());
    let ctx = RenderCtx::new(120, &theme, Clock::still(Instant::now()));
    question
        .lines(&ctx)
        .iter()
        .map(|line| {
            crate::base::kit::logo::golden::council_text(line)
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The question as drawn is its own golden.
#[track_caller]
fn assert_golden(rows: &[String], golden: &str) {
    let path = format!(
        "{}/tests/golden/council/{golden}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    let want = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
    let want: Vec<&str> = want.lines().map(str::trim_end).collect();
    assert_eq!(rows, want, "{golden}");
}

/// A question's screen down to the runs' rows, rows 0 to 32 of the 120 × 38 screen, as the app
/// draws it: the conversation, then the working line while the seats talk on, the question with
/// the keys in the input's place, and the bottom line, which names no key while the question
/// holds them. The runs' rows under it are compared with the seats' own rows.
fn above_the_runs(
    model: &mut ViewModel,
    conversation: Vec<String>,
    working: Option<(u64, u64, &str)>,
) -> Vec<String> {
    use std::time::Duration;

    use crate::bottom::{WorkflowRow, WorkflowRowState, WorkingLine};

    let now = Instant::now();
    let bottom = &mut model.bottom;
    bottom.composer.label = Some("council".to_owned());
    bottom.working = working.map(|(seconds, tokens, agree)| {
        let started = now - Duration::from_secs(seconds);
        let mut line = WorkingLine::new(crate::council::TALKING, started);
        line.tokens_down = tokens;
        line.tokens_limit = Some(200_000);
        line.details = vec![agree.to_owned()];
        line
    });
    bottom.workflows.rows = vec![WorkflowRow::new(
        "run-rate-limit",
        "Rate limit the API",
        WorkflowRowState::NeedsYou,
    )];
    let theme = crate::base::kit::logo::golden::pictured(&Theme::dark());
    let ctx = RenderCtx::new(120, &theme, Clock::still(now));
    let mut live: Vec<String> = bottom
        .render_live(&ctx, 34, false)
        .lines
        .iter()
        .map(|line| {
            crate::base::kit::logo::golden::council_text(line)
                .trim_end()
                .to_owned()
        })
        .collect();
    // The spinner turns with the clock; the screens show it at its ✻.
    if let Some(row) = live
        .iter_mut()
        .find(|row| row.contains(crate::council::TALKING))
        && let Some(spinner) = row.chars().next()
    {
        row.replace_range(..spinner.len_utf8(), "✻");
    }
    let runs = live
        .iter()
        .position(|row| row.contains("Rate limit the API"))
        .expect("the run's row");
    let rows: Vec<String> = conversation
        .into_iter()
        .chain(live[..runs].iter().cloned())
        .collect();
    let mut screen = rows[rows.len().saturating_sub(33)..].to_vec();
    while screen.len() < 33 {
        screen.insert(0, String::new());
    }
    screen
}

#[test]
fn a_held_message_asks_what_happens_to_it() {
    let (mut bridge, mut model) = seated(&[OPUS, OPUS, LUNA, FLASH]);
    let why = HeldWhy::WordsInARow { count: 17 };
    let packet = ApprovalPacket::Council(CouncilPacket {
        talk: step(),
        about: CouncilQuestion::Held {
            message: MessageId::numbered(9),
            why: Some(why.clone()),
            wrote_at: Some(datetime!(2026-10-25 10:46 UTC)),
        },
        text: why.question_text(None),
    });
    let options = vec![
        choose(
            "Ask GPT 5.6 Luna to say it without your words",
            Some("It gets one more try, told only that it repeated what you wrote to it"),
        ),
        choose(
            "Share your private message with the council",
            Some("Every seat reads it, and it binds them like anything you write to the council"),
        ),
        choose(
            "Send it as written: the seats read these words, not your message",
            None,
        ),
        choose("Drop it: no seat ever reads it", None),
    ];
    let title = "GPT 5.6 Luna's message repeats your private words";
    let held = opened(ApprovalKind::Question, title, packet, options);
    tasks::happen(&mut bridge, &mut model, 2, held, None, 0);
    let rows = asked(&mut model);
    assert_golden(&rows, "question_held");
    // The seats talk on while the question waits: the working line stays over it.
    let faces = crate::council::tests::story::faces_one_model_twice();
    let conversation = golden::printed_with(&story::held(), Look::Pictures, 120, &faces);
    let screen = above_the_runs(
        &mut model,
        conversation,
        Some((271, 76_000, "3/4 seats agree")),
    );
    golden::assert_rows_from("held_message", &screen, 0);
}

/// The note keeper's plan that repeats your private words asks before it is sealed, with no way
/// to drop it, since the council needs its plan.
#[test]
fn a_held_plan_asks_before_it_is_sealed() {
    let (mut bridge, mut model) = seated(&[OPUS, OPUS, LUNA, FLASH]);
    let why = HeldWhy::WordsInARow { count: 9 };
    let packet = ApprovalPacket::Council(CouncilPacket {
        talk: step(),
        about: CouncilQuestion::HeldPlan {
            keeper: SeatId::numbered(1),
            why: Some(why.clone()),
            wrote_at: Some(datetime!(2026-10-25 10:46 UTC)),
        },
        text: why.plan_question_text(None),
    });
    let options = vec![
        choose(
            "Ask Opus 5.5 · 1 to write the plan again without your words",
            Some("It gets one more try, told only that it repeated what you wrote to it"),
        ),
        choose(
            "Share your private message with the council",
            Some("Every seat reads it, and it binds them like anything you write to the council"),
        ),
        choose(
            "Seal it as written: every task reads these words, not your message",
            None,
        ),
    ];
    let title = "Opus 5.5 · 1's plan repeats your private words";
    let held = opened(ApprovalKind::Question, title, packet, options);
    tasks::happen(&mut bridge, &mut model, 2, held, None, 0);
    let rows = asked(&mut model);
    let path = format!(
        "{}/tests/golden/council/question_held_plan.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    if let Some(dir) = std::env::var_os("ARGONAUTS_PICTURE_RECORD") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            std::path::Path::new(&dir).join("question_held_plan.txt"),
            rows.join("\n") + "\n",
        );
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
    let want: Vec<&str> = want.lines().map(str::trim_end).collect();
    assert_eq!(rows, want, "question_held_plan");
}

#[test]
fn a_point_left_open_asks_which_side_with_its_seats_and_reasons() {
    let (mut bridge, mut model) = seated(&[OPUS, LUNA, FLASH, QWEN]);
    let side = |words: &str, backers: [u32; 2], reason: &str| Side {
        words: words.to_owned(),
        backers: backers.map(SeatId::numbered).to_vec(),
        reason: reason.to_owned(),
    };
    let shared = side(
        "One shared bucket, 10 a second",
        [1, 3],
        "The balancer's probe can't send a key, and a 401 breaks the curl examples in our docs",
    );
    let refused = side(
        "401",
        [2, 4],
        "One shared bucket is still a bucket that one script can use up",
    );
    send(
        &mut bridge,
        &mut model,
        2,
        CouncilEventKind::Points {
            points: vec![Point {
                number: 6,
                words: "Calls with no key".to_owned(),
                state: PointState::Open,
                sides: vec![shared.clone(), refused.clone()],
                raised_by: None,
            }],
        },
    );
    let packet = ApprovalPacket::Council(CouncilPacket {
        talk: step(),
        about: CouncilQuestion::Point { point: 6 },
        text: "The seats agree on everything else. Pick a side, let them talk on, or write your \
               own."
            .to_owned(),
    });
    let mut options = vec![
        choose(&shared.words, Some(&shared.reason)),
        choose(&refused.words, Some(&refused.reason)),
        choose("Talk on, up to 100k more tokens", None),
    ];
    options.push(AnswerOption {
        key: 't',
        label: "Type my own answer".to_owned(),
        action: AnswerAction::Answer,
        recommended: false,
        detail: None,
    });
    let point = opened(
        ApprovalKind::Question,
        "Calls with no key: which way?",
        packet,
        options,
    );
    tasks::happen(&mut bridge, &mut model, 3, point, None, 0);
    let rows = asked(&mut model);
    assert_golden(&rows, "question_point");
    let conversation = golden::printed(&story::no_agreement(), Look::Pictures, 120);
    let screen = above_the_runs(&mut model, conversation, None);
    golden::assert_rows_from("no_agreement", &screen, 0);
}

/// The council's plan question, opened through the bridge as the app opens it: approve it,
/// with edits, send it back, run it as Captain, abandon it or show it.
pub(crate) fn plan_question() -> ViewModel {
    use argonauts_core::runs::events::approval::WaysOffer;
    use argonauts_core::runs::events::task::Way;
    use argonauts_core::workflows::workflow::WorkflowRef;

    let (mut bridge, mut model) = seated(&[OPUS, LUNA, FLASH, QWEN]);
    let luna = ModelRef::new(LUNA.0, Some(LUNA.1.to_owned()));
    let mut packet = plan::packet(
        vec![plan::spec(2, "Plan limits", luna, Way::OwnSession, true)],
        Vec::new(),
    );
    packet.offer = Some(WaysOffer {
        ways: vec![
            Way::OwnSession,
            Way::Workflow {
                name: WorkflowRef::new("maker-checker"),
            },
        ],
        not_offered: None,
    });
    let option = |key: char, label: &str, action: AnswerAction| AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    };
    let options = vec![
        option('y', "Approve the plan", AnswerAction::Approve),
        option('e', "Approve with edits", AnswerAction::ApproveWithEdits),
        option('b', "Send back", AnswerAction::SendBack),
        option('s', "Run it as Captain", AnswerAction::SwitchToCaptain),
        option('x', "Abandon", AnswerAction::Abandon),
        option('d', "Show the plan", AnswerAction::ShowDiff),
    ];
    let plan = opened(
        ApprovalKind::Plan,
        "Approve this plan?",
        ApprovalPacket::Plan(Box::new(packet)),
        options,
    );
    tasks::happen(&mut bridge, &mut model, 2, plan, None, 0);
    model
}

#[test]
fn a_councils_plan_asks_to_approve_it_and_says_what_edits_and_sending_back_do() {
    let mut model = plan_question();
    let rows = asked(&mut model);
    assert_golden(&rows, "question_plan");
    let items = [
        story::flash_on_deprecation(3),
        story::everyone_agreed(),
        story::plan_and_who(),
        story::signed(),
    ];
    let mut conversation = golden::printed(&items, Look::Pictures, 120);
    // The app leaves a blank row between the printed talk and the question under it.
    conversation.push(String::new());
    let screen = above_the_runs(&mut model, conversation, None);
    golden::assert_rows_from("plan", &screen, 0);
}

/// The council's line takes the plan's summary's place, and a flag that keeps the plan from
/// auto-approval still says so under it.
#[test]
fn a_councils_plan_question_keeps_its_flags() {
    use argonauts_core::runs::events::approval::WaysOffer;
    use argonauts_core::runs::events::task::Way;

    let (mut bridge, mut model) = seated(&[OPUS, LUNA, FLASH, QWEN]);
    let luna = ModelRef::new(LUNA.0, Some(LUNA.1.to_owned()));
    let mut packet = plan::packet(
        vec![plan::spec(2, "Plan limits", luna, Way::OwnSession, true)],
        Vec::new(),
    );
    packet.offer = Some(WaysOffer {
        ways: vec![Way::OwnSession],
        not_offered: None,
    });
    packet.flags = vec!["The plan adds a dependency".to_owned()];
    let options = vec![
        AnswerOption {
            key: 'y',
            label: "Approve the plan".to_owned(),
            action: AnswerAction::Approve,
            recommended: false,
            detail: None,
        },
        AnswerOption {
            key: 'e',
            label: "Approve with edits".to_owned(),
            action: AnswerAction::ApproveWithEdits,
            recommended: false,
            detail: None,
        },
    ];
    let plan = opened(
        ApprovalKind::Plan,
        "Approve this plan?",
        ApprovalPacket::Plan(Box::new(packet)),
        options,
    );
    tasks::happen(&mut bridge, &mut model, 2, plan, None, 0);
    let rows = asked(&mut model);
    assert_eq!(rows[1], format!("  {}", crate::questions::COUNCIL_PLAN));
    assert_eq!(rows[2], "  A flag above keeps it from auto-approval");
}

/// Once the plan is written and every seat backs every point it was built from, the line under
/// the plan says every seat signed both, before who objects to a way you asked for.
#[test]
fn the_line_under_the_plan_says_every_seat_signed_both() {
    use argonauts_core::runs::events::approval::WayObjection;
    use argonauts_core::runs::events::council::{PointStance, Stance};
    use argonauts_core::runs::events::task::Way;

    let (mut bridge, mut model) = seated(&[OPUS, LUNA, FLASH, QWEN]);
    send(
        &mut bridge,
        &mut model,
        2,
        CouncilEventKind::Points {
            points: vec![Point {
                number: 1,
                words: "Counters live in Redis".to_owned(),
                state: PointState::Open,
                sides: Vec::new(),
                raised_by: None,
            }],
        },
    );
    for n in 1..=4 {
        send(
            &mut bridge,
            &mut model,
            2 + u64::from(n),
            CouncilEventKind::Stances {
                seat: SeatId::numbered(n),
                stances: vec![PointStance {
                    point: 1,
                    stance: Stance::Agree,
                }],
            },
        );
    }
    send(
        &mut bridge,
        &mut model,
        7,
        CouncilEventKind::Agreed { points: 1 },
    );
    let CouncilEventKind::Message(mut notes) =
        message("msg-010", Speaker::Seat(SeatId::numbered(1)), "The plan.")
    else {
        unreachable!("a message");
    };
    notes.kind = MessageKind::Notes;
    send(&mut bridge, &mut model, 8, CouncilEventKind::Message(notes));
    rows(&mut model);

    let luna = ModelRef::new(LUNA.0, Some(LUNA.1.to_owned()));
    let packet = plan::packet(
        vec![plan::spec(2, "Plan limits", luna, Way::OwnSession, true)],
        vec![WayObjection {
            task: argonauts_core::TaskId::numbered(2),
            seat: SeatId::numbered(3),
            reason: Vec::new(),
        }],
    );
    let plan = opened(
        ApprovalKind::Plan,
        "Approve this plan?",
        ApprovalPacket::Plan(Box::new(packet)),
        Vec::new(),
    );
    tasks::happen(&mut bridge, &mut model, 9, plan, None, 0);
    let (printed, live) = rows(&mut model);
    let all: Vec<&String> = printed.iter().chain(&live).collect();
    let under = all
        .iter()
        .find(|row| row.contains("signed"))
        .unwrap_or_else(|| panic!("{all:#?}"));
    assert!(
        under.contains("Every seat signed both · ")
            && under.ends_with("3.8 Flash objects to a way you asked for; only you change it"),
        "{under}"
    );
}
