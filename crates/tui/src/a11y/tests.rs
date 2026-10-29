use argonauts_core::api::DiffScope;
use argonauts_core::app::{AnswerTarget, AppChoice, AppEvent, ChoiceKind, UiCommand, ViewRequest};
use argonauts_core::events::approval::{AnswerAction, AnswerOption, FreeText};
use argonauts_core::names::{ApprovalId, ChoiceId};
use argonauts_core::{RunId, TaskId};
use serde_json::{Value, json};

use super::{Pick, QuestionList, Reader, Typed};
use crate::UiOptions;

const RUN: &str = "run-01";
const EVIDENCE: &str = "3f1a";

fn run_event(seq: u64, data: Value) -> AppEvent {
    serde_json::from_value(json!({"type": "run", "event": {"seq": seq,
        "ts": "2026-09-30T12:00:00Z", "run_id": RUN, "task_id": null, "session": null,
        "source": "argonauts-code", "kind": {"type": "approval", "data": data}}}))
    .expect("an approval event")
}

fn option(key: char, label: &str, action: Value) -> Value {
    json!({"key": key.to_string(), "label": label, "action": action, "recommended": false,
           "detail": null})
}

fn opened(id: &str, kind: &str, packet: Value, options: Vec<Value>, free_text: &str) -> Value {
    json!({"event": "opened", "id": id, "kind": kind, "title": format!("{kind} · {RUN}"),
        "task": null, "packet": packet, "evidence_sha256": EVIDENCE, "mode": "supervised",
        "decide_with": "rules", "options": options, "free_text": free_text, "needs_you": true})
}

fn merge_row(task: &str, title: &str, worker: Option<&str>) -> Value {
    json!({"task": task, "title": title, "kind": {"kind": "build"},
        "stat": {"files_changed": 2, "insertions": 30, "deletions": 4}, "worker": worker,
        "checks_passed": true, "review": "approve"})
}

fn merge(id: &str, rows: Vec<Value>) -> Value {
    let packet = json!({"kind": "merge", "target": "main",
        "branch": format!("argonauts/{RUN}/merged"), "rows": rows,
        "suite_passed_on_latest_main": true, "checks": [],
        "combined": {"files_changed": 4, "insertions": 60, "deletions": 8},
        "diff_path": "/nonexistent/argonauts/combined.diff", "tree_sha": "t1",
        "spent": {"input_tokens": 1200, "output_tokens": 900, "cached_tokens": 0,
                  "cost_usd": 0.46, "cost_partial": false, "cost_estimated": false},
        "limit": null, "partial": false});
    let options = vec![
        option('y', "Merge", json!({"action": "merge"})),
        option(
            'p',
            "Open a pull request",
            json!({"action": "pull_request"}),
        ),
        option(
            'b',
            "Request changes on some tasks",
            json!({"action": "request_changes"}),
        ),
        option('k', "Keep the branch", json!({"action": "keep_branch"})),
        option('x', "Abandon the run", json!({"action": "abandon"})),
        option('d', "Show the diff", json!({"action": "show_diff"})),
    ];
    opened(id, "merge", packet, options, "guidance")
}

fn two_task_merge() -> Value {
    merge(
        "approval-001",
        vec![
            merge_row("task-002", "Add the JSON writer", Some("claude-code/opus")),
            merge_row("task-003", "Wire the --json flag", Some("codex/gpt-5.6")),
        ],
    )
}

fn plan() -> Value {
    let packet = json!({"kind": "plan", "summary": "A JSON writer beside the CSV one",
        "judging": null, "near_tie": false, "decisions": [],
        "graph": {"tasks": [], "critical_path": []},
        "acceptance_tests": ["--json prints valid JSON"],
        "limit": {"lines": [
            {"line": "tasks", "amount": {"unit": "usd", "amount": 6.0}},
            {"line": "review", "amount": {"unit": "usd", "amount": 2.0}}],
            "total": {"unit": "usd", "amount": 10.0},
            "estimate_low": 7.0, "estimate_high": 9.5},
        "new_dependencies": [], "flags": []});
    let options = vec![
        option('y', "Approve the plan", json!({"action": "approve"})),
        option(
            'e',
            "Approve with edits",
            json!({"action": "approve_with_edits"}),
        ),
        option('x', "Abandon", json!({"action": "abandon"})),
        option('d', "Show the plan", json!({"action": "show_diff"})),
    ];
    opened("approval-002", "plan", packet, options, "edits")
}

fn fallback() -> Value {
    let packet = json!({"kind": "fallback", "task": "task-003", "title": "Middleware",
        "stopped": "claude-code/opus", "reason": "usage limit", "resets_at": null,
        "work": {"files_changed": 4, "insertions": 96, "deletions": 12}, "checks_ran": false,
        "candidates": [{"worker": "codex/gpt-5.6", "plan_usage_left": 0.62, "plan_usage_text": "62%",
            "track_record": null, "cost": {"unit": "free"}, "needs_confirm": false,
            "may_run_out": false}],
        "mode": "ask"});
    let options = vec![
        option(
            'a',
            "codex/gpt-5.6",
            json!({"action": "candidate", "worker": "codex/gpt-5.6"}),
        ),
        option('w', "Wait until the reset", json!({"action": "wait"})),
        option('d', "Show the diff", json!({"action": "show_diff"})),
    ];
    opened("approval-003", "fallback", packet, options, "none")
}

fn waiting_on(question: Value) -> Reader {
    let mut reader = Reader::new(&UiOptions::default());
    reader.event(&run_event(1, question));
    reader
}

fn approval(id: &str) -> AnswerTarget {
    AnswerTarget::Approval {
        run: RunId::new(RUN).expect("a run id"),
        id: ApprovalId::parse(id).expect("an approval id"),
    }
}

fn answer(id: &str, action: AnswerAction, text: Option<&str>, tasks: &[&str]) -> UiCommand {
    UiCommand::Answer {
        to: approval(id),
        action,
        text: text.map(str::to_owned),
        evidence_sha256: Some(EVIDENCE.to_owned()),
        tasks: tasks
            .iter()
            .map(|task| TaskId::new(*task).expect("a task id"))
            .collect(),
        remember: false,
    }
}

fn submit(text: &str) -> Vec<UiCommand> {
    vec![UiCommand::Submit {
        text: text.into(),
        workflow_requested: false,
        pictures: Vec::new(),
    }]
}

#[test]
fn words_after_a_number_and_letters_are_a_message() {
    let mut reader = waiting_on(two_task_merge());
    for text in [
        "1 more thing, the tests look flaky",
        "y is this failing?",
        "x marks the spot",
        "1 more thing: the tests look flaky",
        "y",
        "d",
    ] {
        let said = reader.typed(text);
        assert_eq!(said.commands, submit(text), "{text}");
    }
}

#[test]
fn merge_and_abandon_ask_again() {
    let mut reader = waiting_on(two_task_merge());
    let asked = reader.typed("1");
    assert!(asked.commands.is_empty());
    assert_eq!(asked.lines, vec!["merge: type 1 again to merge."]);
    // Conversation in between cancels, so a later `1` asks again.
    let conversation = reader.typed("is the flag tested?");
    assert_eq!(
        conversation.lines,
        vec!["merge: cancelled, nothing was sent."]
    );
    assert_eq!(conversation.commands, submit("is the flag tested?"));
    assert!(reader.typed("1: ship it").commands.is_empty());
    let merged = reader.typed("1");
    assert_eq!(
        merged.commands,
        vec![answer(
            "approval-001",
            AnswerAction::Merge,
            Some("ship it"),
            &[]
        )]
    );
    assert_eq!(merged.lines, vec!["you: 1. Merge, ship it"]);

    let mut reader = waiting_on(two_task_merge());
    assert!(reader.typed("5").commands.is_empty());
    let other = reader.typed("2");
    assert_eq!(
        other.lines,
        vec![
            "abandon the run: cancelled, nothing was sent.",
            "you: 2. Open a pull request"
        ]
    );
    assert_eq!(
        other.commands,
        vec![answer("approval-001", AnswerAction::PullRequest, None, &[])]
    );
}

#[test]
fn show_the_diff_answers_nothing() {
    let mut reader = waiting_on(two_task_merge());
    let shown = reader.typed("6");
    assert!(shown.commands.is_empty(), "{:?}", shown.commands);
    assert_eq!(shown.lines[0], "you: 6. Show the diff");
    assert!(
        shown.lines[1].starts_with("warning: Can't open the diff at "),
        "{:?}",
        shown.lines
    );
    // The question still takes numbers.
    assert_eq!(
        reader.typed("2").commands,
        vec![answer("approval-001", AnswerAction::PullRequest, None, &[])]
    );

    // One task's merge asks the app for that task's diff.
    let one = merge(
        "approval-001",
        vec![merge_row("task-002", "Add the JSON writer", None)],
    );
    let mut reader = waiting_on(one);
    assert_eq!(
        reader.typed("6").commands,
        vec![UiCommand::Request {
            view: ViewRequest::Diff {
                run: RunId::new(RUN).expect("a run id"),
                task: TaskId::new("task-002").expect("a task id"),
                scope: DiffScope::Task,
            }
        }]
    );
    assert!(matches!(
        reader.typed("2").commands.as_slice(),
        [UiCommand::Answer { .. }]
    ));
}

#[test]
fn show_the_plan_reads_the_details() {
    let mut reader = waiting_on(plan());
    let shown = reader.typed("4");
    assert!(shown.commands.is_empty());
    insta::assert_snapshot!("show_the_plan", shown.lines.join("\n"));
}

#[test]
fn request_changes_names_tasks() {
    let mut reader = waiting_on(two_task_merge());
    let unknown = reader.typed("3 task-009: tighten it");
    assert!(unknown.commands.is_empty());
    assert_eq!(
        unknown.lines,
        vec![
            "warning: task-009 isn't one of this merge's tasks (task-002, task-003), so nothing was sent."
        ]
    );
    let named = reader.typed("3 task-003: keep the old flag");
    assert_eq!(
        named.commands,
        vec![answer(
            "approval-001",
            AnswerAction::RequestChanges,
            Some("keep the old flag"),
            &["task-003"]
        )]
    );
    assert_eq!(
        named.lines,
        vec!["you: 3. Request changes on some tasks, task-003, keep the old flag"]
    );

    let mut every = waiting_on(two_task_merge());
    let said = every.typed("3");
    assert_eq!(
        said.commands,
        vec![answer(
            "approval-001",
            AnswerAction::RequestChanges,
            None,
            &[]
        )]
    );
    assert_eq!(
        said.lines,
        vec!["you: 3. Request changes on some tasks, every task"]
    );
}

#[test]
fn a_spot_check_takes_its_numbers() {
    let mut reader = Reader::new(&UiOptions::default());
    let mut quiet = two_task_merge();
    quiet["needs_you"] = json!(false);
    reader.event(&run_event(1, quiet));
    reader.event(&run_event(
        2,
        json!({"event": "decided", "id": "approval-001", "action": {"action": "merge"},
            "text": null, "by": {"party": "rules"}, "evidence_sha256": EVIDENCE,
            "auto_preview": null, "tasks": []}),
    ));
    let queued = reader.event(&run_event(
        3,
        json!({"event": "spot_check_queued", "id": "approval-001"}),
    ));
    insta::assert_snapshot!("spot_check", queued.lines.join("\n"));

    let evidence = reader.typed("3");
    assert!(evidence.commands.is_empty());
    assert!(
        evidence
            .lines
            .iter()
            .any(|line| line.starts_with("details: merge")),
        "{:?}",
        evidence.lines
    );
    let checked = reader.typed("2");
    assert_eq!(
        checked.commands,
        vec![UiCommand::SpotCheck {
            run: RunId::new(RUN).expect("a run id"),
            id: ApprovalId::parse("approval-001").expect("an id"),
            correct: false,
        }]
    );
    // Answered, the spot check no longer takes numbers.
    assert_eq!(reader.typed("1").commands, submit("1"));
}

#[test]
fn remember_this_pick_goes_with_a_candidate() {
    let mut reader = Reader::new(&UiOptions::default());
    let said = reader.event(&run_event(1, fallback()));
    assert!(
        said.lines
            .contains(&"  4. Remember this pick, off".to_owned()),
        "{:?}",
        said.lines
    );
    let on = reader.typed("4");
    assert!(on.commands.is_empty());
    assert_eq!(on.lines, vec!["remember this pick: on"]);
    let picked = reader.typed("1");
    let [
        UiCommand::Answer {
            action, remember, ..
        },
    ] = picked.commands.as_slice()
    else {
        panic!("{:?}", picked.commands);
    };
    assert!(matches!(action, AnswerAction::Candidate { .. }));
    assert!(*remember);
}

#[test]
fn a_plan_question_reads_its_limit_by_header() {
    let AppEvent::Run { event } = run_event(1, plan()) else {
        unreachable!()
    };
    let argonauts_core::events::RunEventKind::Approval(
        argonauts_core::events::approval::ApprovalEvent::Opened(opened),
    ) = &event.kind
    else {
        unreachable!()
    };
    let names = Default::default();
    let question =
        QuestionList::approval(&event.run_id, opened, None, time::UtcOffset::UTC, &names);
    insta::assert_snapshot!("plan_question", question.lines().join("\n"));
}

#[test]
fn a_merge_row_without_a_worker_keeps_its_columns() {
    let mut reader = Reader::new(&UiOptions::default());
    let said = reader.event(&run_event(
        1,
        merge(
            "approval-001",
            vec![
                merge_row("task-002", "Add the JSON writer", None),
                merge_row("task-003", "Wire the --json flag", Some("codex/gpt-5.6")),
            ],
        ),
    ));
    insta::assert_snapshot!("merge_row_without_worker", said.lines.join("\n"));
}

fn choice(free_text: FreeText) -> QuestionList {
    let option = |key: char, label: &str, action: AnswerAction| AnswerOption {
        key,
        label: label.to_owned(),
        action,
        recommended: false,
        detail: None,
    };
    QuestionList::choice(&AppChoice {
        id: ChoiceId::numbered(1),
        kind: ChoiceKind::StartWork,
        title: "Start work?".to_owned(),
        body: vec!["It changes 3 files.".to_owned()],
        options: vec![
            option('s', "Captain", AnswerAction::Approve),
            option('c', "Crew run", AnswerAction::SendBack),
        ],
        free_text,
        bell: true,
    })
}

#[test]
fn numbers_pick_options_and_letters_never_do() {
    let question = choice(FreeText::None);
    let captain = Typed::Pick(Pick::Option {
        index: 0,
        note: None,
        tasks: Vec::new(),
    });
    assert_eq!(question.parse("1"), captain);
    assert_eq!(question.parse(" 1. "), captain);
    assert!(matches!(
        question.parse("2"),
        Typed::Pick(Pick::Option { index: 1, .. })
    ));
    for message in ["3", "0", "s", "C", "sure", "2 more tests please", ": 1"] {
        assert_eq!(question.parse(message), Typed::Message, "{message}");
    }
    assert!(matches!(
        question.parse("2: more tests please"),
        Typed::Warning(_)
    ));
}

#[test]
fn notes_and_own_answers_follow_a_colon() {
    let guided = choice(FreeText::Guidance);
    assert_eq!(
        guided.parse("2: keep the old flag"),
        Typed::Pick(Pick::Option {
            index: 1,
            note: Some("keep the old flag".to_owned()),
            tasks: Vec::new(),
        })
    );
    assert_eq!(guided.parse("2 keep the old flag"), Typed::Message);
    let open = choice(FreeText::Answer);
    assert_eq!(open.own_answer_number(), Some(3));
    assert_eq!(
        open.parse("3: use the v2 schema"),
        Typed::Pick(Pick::OwnAnswer {
            text: Some("use the v2 schema".to_owned())
        })
    );
    assert_eq!(open.parse("3"), Typed::Pick(Pick::OwnAnswer { text: None }));
    assert_eq!(open.command(&Pick::OwnAnswer { text: None }), None);
    let lines = open.lines();
    assert!(
        lines.contains(&"  3. Type my own answer".to_owned()),
        "{lines:?}"
    );
}

/// A free-text question reads as the answer to type, as the question shows its field open: no
/// list of one, and `1:` with the answer, or `1` and the answer on the next line, sends it.
#[test]
fn a_free_text_question_reads_as_an_answer_to_type() {
    let asked = QuestionList::choice(&AppChoice {
        id: ChoiceId::numbered(1),
        kind: ChoiceKind::StartWork,
        title: "Question from build:greet".to_owned(),
        body: vec!["Should greet.py say hello or hi?".to_owned()],
        options: vec![AnswerOption {
            key: 'a',
            label: "Type my own answer".to_owned(),
            action: AnswerAction::Answer,
            recommended: false,
            detail: None,
        }],
        free_text: FreeText::Answer,
        bell: true,
    });
    assert!(asked.is_answer_field());
    let lines = asked.lines();
    assert_eq!(
        lines,
        [
            "needs you: Question from build:greet",
            "  Should greet.py say hello or hi?",
            "answer: type 1: and then your answer, or 1 alone to type it on the next line.",
        ]
    );
    assert_eq!(
        asked.parse("1: hello"),
        Typed::Pick(Pick::OwnAnswer {
            text: Some("hello".to_owned())
        })
    );
    assert_eq!(
        asked.parse("1"),
        Typed::Pick(Pick::OwnAnswer { text: None })
    );
    // Conversation still goes on as conversation.
    assert_eq!(asked.parse("hello"), Typed::Message);
    // A question with choices still lists them with your own answer after.
    assert!(!choice(FreeText::Answer).is_answer_field());
}

#[test]
fn an_answer_goes_to_its_question() {
    let question = choice(FreeText::None);
    let pick = Pick::Option {
        index: 1,
        note: None,
        tasks: Vec::new(),
    };
    assert_eq!(
        question.command(&pick),
        Some(UiCommand::Answer {
            to: AnswerTarget::Choice {
                id: ChoiceId::numbered(1)
            },
            action: AnswerAction::SendBack,
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        })
    );
    assert_eq!(question.picked(&pick), "you: 2. Crew run");
}

/// Screen-reader mode reads Ultracode after the levels, on the slider that offers it, and `a`
/// flips it there, since Tab never reaches a typed line.
#[test]
fn the_effort_slider_reads_ultracode_and_a_flips_it() {
    use argonauts_core::app::ViewData;
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::input::SlashCommand;
    use argonauts_core::model_picker::{EffortSlider, EffortStop};

    let stops = ["low", "high"]
        .iter()
        .map(|&level| EffortStop {
            level: level.into(),
            description: String::new(),
        })
        .collect();
    let slider = EffortSlider::new(
        None,
        "claude-code/claude-opus-5-5".parse().unwrap(),
        "Opus 5.5",
        stops,
        Some("high"),
        None,
    )
    .unwrap();
    let read = |slider: EffortSlider| {
        super::views::view_lines(&ViewData::Effort { slider }, &DisplayNames::default())
    };
    assert_eq!(
        read(slider.clone().with_ultracode(true)),
        [
            "effort for Opus 5.5: high",
            "levels: low, high",
            "ultracode: on"
        ]
    );
    assert!(
        !read(slider.clone())
            .iter()
            .any(|line| line.contains("ultracode"))
    );

    let mut reader = Reader::new(&UiOptions::default());
    let view = AppEvent::View {
        data: Box::new(ViewData::Effort {
            slider: slider.clone().with_ultracode(false),
        }),
    };
    let said = reader.event(&view);
    assert_eq!(
        said.lines.last().map(String::as_str),
        Some("ultracode: type a to turn it on.")
    );
    assert!(
        !said.lines.iter().any(|line| line.contains("tab")),
        "{:?}",
        said.lines
    );
    assert_eq!(
        reader.typed("a").commands,
        [UiCommand::Slash {
            command: SlashCommand::Effort {
                role: None,
                level: None,
                ultracode: Some(true),
            },
        }]
    );
    // The offer is used up, so `a` is a message again.
    assert_eq!(reader.typed("a").commands, submit("a"));

    // A model that takes no effort setting still offers it.
    let haiku =
        EffortSlider::without_levels(None, "claude-code/haiku".parse().unwrap(), "Haiku 4.5");
    assert_eq!(
        read(haiku.with_ultracode(true)),
        [
            "effort for Haiku 4.5: none, it takes no effort setting",
            "ultracode: on"
        ]
    );
}

#[test]
fn a_shell_command_reads_as_typed_then_its_output_and_end() {
    use argonauts_core::app::ShellEvent;

    use super::lines_for;
    use crate::state::AppState;

    let state = AppState::default();
    let said = |event| lines_for(&AppEvent::Shell { event }, &state);
    assert_eq!(
        said(ShellEvent::Started {
            id: 1,
            command: "echo hi".into()
        }),
        ["shell: ! echo hi"]
    );
    assert_eq!(
        said(ShellEvent::Output {
            id: 1,
            text: "hi\n\nthere\n".into()
        }),
        ["output: hi", "output: there"]
    );
    assert_eq!(
        said(ShellEvent::Finished {
            id: 1,
            exit_code: Some(2),
            stopped: false
        }),
        ["shell: exit code 2"]
    );
    assert_eq!(
        said(ShellEvent::Finished {
            id: 1,
            exit_code: None,
            stopped: true
        }),
        ["shell: stopped"]
    );
}

#[test]
fn a_bang_line_runs_in_your_shell() {
    let mut reader = Reader::new(&UiOptions::default());
    let said = reader.typed("!git status");
    assert_eq!(
        said.commands,
        [UiCommand::Shell {
            command: "git status".into()
        }]
    );
    assert!(reader.typed("!   ").commands.is_empty());
}

/// `/workflows` shows what the run's row opens on screen: what the run waits for
/// before any agent, the main agent's session while it designs, then the workflow.
#[test]
fn workflows_says_the_workflow_is_being_written_while_it_is_designed() {
    use crate::views::waiting::fixture::{RunLog, run_id};

    let workflows = |log: &RunLog| {
        let mut reader = Reader::new(&UiOptions::default());
        for event in &log.events {
            reader.event(event);
        }
        reader.typed("/workflows")
    };
    let mut log = RunLog::started();
    let said = workflows(&log);
    assert!(said.commands.is_empty(), "{:?}", said.commands);
    assert_eq!(
        said.lines,
        ["Waiting for the main agent to start designing the workflow."]
    );
    log.design("s-001", "main:design");
    let said = workflows(&log);
    assert!(said.commands.is_empty(), "{:?}", said.commands);
    assert_eq!(said.lines, [crate::views::waiting::BEING_WRITTEN]);
    log.ended("s-001");
    log.declare();
    assert_eq!(
        workflows(&log).commands,
        [UiCommand::Request {
            view: ViewRequest::Workflow {
                run: run_id(),
                workflow: None,
            },
        }]
    );
}

/// A fallback for a CLI that turned out signed out says how to sign it in again.
#[test]
fn a_signed_out_fallback_says_how_to_sign_in_again() {
    use argonauts_core::app::{ConversationEvent, SignedOut};
    use argonauts_core::names::{ModelRef, TurnId};

    let event = AppEvent::Conversation {
        event: ConversationEvent::FellBack {
            turn: TurnId::numbered(1),
            from: ModelRef::new("claude-code", None),
            to: ModelRef::new("codex", None),
            why: "is signed out (its sign-in was revoked)".into(),
            told: "your request".into(),
            signed_out: Some(SignedOut {
                why: "its sign-in was revoked".into(),
                login: Some("claude /login".into()),
            }),
        },
    };
    let lines = super::lines::lines_for(&event, &crate::state::AppState::default());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("is signed out"), "{lines:?}");
    assert!(lines[0].ends_with("signs it in again"), "{lines:?}");
}

/// A start Argonauts Code turned down is said once the turn ends, in the screen's short words,
/// and not at all when a later start in the same turn works.
#[test]
fn a_refused_start_is_said_at_the_turns_end_unless_a_retry_worked() {
    use argonauts_core::app::{ConversationEvent, TurnReply};
    use argonauts_core::names::{ModelRef, TurnId};
    use argonauts_core::{AgentEvent, Spend};

    const START: &str = "mcp__argonauts_status__argonauts_start_workflow";
    const WHY_NOT: &str = "No workflow started. With Captain, one starts only when you asked \
        for a workflow in this message or the one before, and you said how it should run.";
    let turn = TurnId::numbered(1);
    let said = |reader: &mut Reader, event| {
        reader
            .event(&AppEvent::Conversation { event })
            .lines
            .join("\n")
    };
    let agent = |event| ConversationEvent::Agent {
        turn: turn.clone(),
        event,
    };
    let start = |id: &str, how: Option<&str>| {
        agent(AgentEvent::ToolStarted {
            id: id.into(),
            name: START.into(),
            input: json!({"request": "fix the money", "how_to_run": how}),
            parent: None,
        })
    };
    let answered = |id: &str, output: &str| {
        agent(AgentEvent::ToolFinished {
            id: id.into(),
            ok: true,
            output: Some(output.into()),
            parent: None,
        })
    };
    let finished = || ConversationEvent::TurnFinished {
        turn: turn.clone(),
        reply: TurnReply::Answer {
            markdown: "Done.".into(),
        },
        spend: Spend::default(),
        elapsed_ms: 4_000,
    };
    let asked = |reader: &mut Reader| {
        said(
            reader,
            ConversationEvent::TurnStarted {
                turn: turn.clone(),
                text: "use a workflow for this".into(),
                worker: ModelRef::new("claude-code", Some("claude-haiku-5-5".into())),
                read_only: false,
                commit: "8b41d07".into(),
                uncommitted: Vec::new(),
                settings_sha256: None,
                continues: false,
            },
        )
    };

    let mut reader = Reader::new(&UiOptions::default());
    asked(&mut reader);
    assert_eq!(said(&mut reader, start("s1", None)), "");
    assert_eq!(said(&mut reader, answered("s1", WHY_NOT)), "");
    let ended = said(&mut reader, finished());
    assert!(
        ended.starts_with("claude-code: No workflow started\n"),
        "{ended}"
    );
    assert!(!ended.contains("With Captain"), "{ended}");

    let mut reader = Reader::new(&UiOptions::default());
    asked(&mut reader);
    said(&mut reader, start("s1", None));
    said(&mut reader, answered("s1", WHY_NOT));
    said(&mut reader, start("s2", Some("captain")));
    assert_eq!(
        said(&mut reader, answered("s2", "Starting a workflow: Captain.")),
        ""
    );
    let ended = said(&mut reader, finished());
    assert!(!ended.contains("No workflow started"), "{ended}");
}

/// The crew your words set for a request is read a role a line, each with its models and where
/// they came from.
#[test]
fn the_crew_for_a_request_is_read_a_role_a_line() {
    use argonauts_core::app::CrewRow;

    let row = |role: &str, who: &str, from: &str| CrewRow {
        role: role.into(),
        who: who.into(),
        from: from.into(),
    };
    let event = AppEvent::CrewForRequest {
        rows: vec![
            row(
                "orchestrator",
                "Haiku 5.5 on Claude Code",
                "your main session",
            ),
            row("reviewers", "GPT 5.6 Luna on Codex", "as you asked"),
        ],
    };
    let lines = super::lines::lines_for(&event, &crate::state::AppState::default());
    assert_eq!(
        lines,
        [
            "crew for this request: orchestrator, Haiku 5.5 on Claude Code, your main session",
            "crew for this request: reviewers, GPT 5.6 Luna on Codex, as you asked",
        ]
    );
}
