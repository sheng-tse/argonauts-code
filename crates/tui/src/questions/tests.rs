use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::DiffScope;
use argonauts_core::app::{AppChoice, ChoiceKind, ViewRequest};
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::events::approval::{
    AgentDownPacket, ApprovalKind, ApprovalPacket, FreeText, MergePacket, MergeRow, QuestionPacket,
    StuckPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::task::TaskKind;
use argonauts_core::limits::{Candidate, CostEstimate, TrackRecord};
use argonauts_core::names::{AgentId, ChoiceId, ModelRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::fallback::{FallbackMode, FallbackStrategy};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{DiffStat, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use time::UtcOffset;
use time::macros::datetime;

use super::*;
use crate::bottom::{OptionKind, QuestionEvent};
use crate::bridge::Effects;
use crate::cell::HistoryCell;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

struct Screen {
    questions: Questions,
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
}

impl Screen {
    fn new() -> Self {
        Self {
            questions: Questions::default(),
            model: ViewModel::default(),
            state: AppState::default(),
            settings: UiSettings::default(),
        }
    }

    fn event(&mut self, event: AppEvent) -> Effects {
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.questions.on_event(&event, &mut cx);
        out
    }

    fn action(&mut self, action: UiAction) -> (bool, Effects) {
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        let taken = self.questions.on_action(&action, &mut cx);
        (taken, out)
    }

    fn question(&self) -> &QuestionPrompt {
        self.model
            .bottom
            .question
            .as_ref()
            .expect("a task on screen")
    }

    /// Presses keys on the question as the app does, and hands what they report to the questions.
    fn keys(&mut self, codes: &[KeyCode]) -> Vec<Effects> {
        let mut effects = Vec::new();
        for &code in codes {
            let question = self
                .model
                .bottom
                .question
                .as_mut()
                .expect("a task on screen");
            question.focused = true;
            let id = question.id;
            let action = match question.handle_key(KeyEvent::new(code, KeyModifiers::NONE)) {
                QuestionEvent::Answered {
                    index,
                    note,
                    checked,
                } => {
                    self.model.bottom.question = None;
                    Some(UiAction::Answer {
                        question: id,
                        option: index,
                        note,
                        checked,
                    })
                }
                QuestionEvent::OpenView { index } => Some(UiAction::OpenView {
                    question: id,
                    option: index,
                }),
                QuestionEvent::Dismissed => Some(UiAction::Dismiss { question: id }),
                QuestionEvent::BackToInput | QuestionEvent::Moved | QuestionEvent::Unhandled => {
                    None
                }
            };
            if let Some(action) = action {
                effects.push(self.action(action).1);
            }
        }
        effects
    }

    fn history(&self) -> Vec<String> {
        self.history_as(false)
    }

    /// As Ctrl+O shows it when `expanded`.
    fn history_as(&self, expanded: bool) -> Vec<String> {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now())).with_expanded(expanded);
        self.model
            .history
            .cells()
            .iter()
            .map(|cell| plain_text(&cell.lines(&ctx)))
            .collect()
    }
}

fn run() -> RunId {
    RunId::new("run-01").unwrap()
}

fn task_id(n: u32) -> TaskId {
    TaskId::new(format!("task-{n:03}")).unwrap()
}

fn worker(text: &str) -> ModelRef {
    text.parse().unwrap()
}

fn option(key: char, label: &str, action: AnswerAction) -> AnswerOption {
    AnswerOption {
        key,
        label: label.into(),
        action,
        recommended: false,
        detail: None,
    }
}

fn stat(files: u32, insertions: u32, deletions: u32) -> DiffStat {
    DiffStat {
        files_changed: files,
        insertions,
        deletions,
    }
}

fn opened(
    kind: ApprovalKind,
    title: &str,
    packet: ApprovalPacket,
    options: Vec<AnswerOption>,
) -> ApprovalOpened {
    ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind,
        title: title.into(),
        task: None,
        packet,
        evidence_sha256: HASH.into(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

/// The number the question shows beside the option the event gives `key`.
fn number(opened: &ApprovalOpened, key: char) -> KeyCode {
    let index = opened
        .options
        .iter()
        .position(|option| option.key == key)
        .unwrap_or_else(|| panic!("an option `{key}` on {}", opened.title));
    KeyCode::Char(char::from_digit(index as u32 + 1, 10).expect("one digit"))
}

fn merge() -> ApprovalOpened {
    let row = |n: u32, title: &str| MergeRow {
        task: task_id(n),
        title: title.into(),
        kind: TaskKind::Build,
        stat: stat(2, 40, 6),
        worker: Some(worker("codex/gpt-5.6")),
        checks_passed: true,
        review: Some(ReviewVerdict::Approve),
        debt: Vec::new(),
        questions: Vec::new(),
    };
    let packet = MergePacket {
        target: Some("main".into()),
        branch: "argonauts/run-01/merged".into(),
        rows: vec![
            row(2, "Add the JSON writer"),
            row(3, "Wire --json into the CLI"),
        ],
        suite_passed_on_latest_main: true,
        checks: Vec::new(),
        combined: stat(4, 80, 12),
        conductor: stat(0, 0, 0),
        diff_path: PathBuf::from("/nonexistent/argonauts/run-01.diff"),
        tree_sha: "abc".into(),
        spent: Spend::default(),
        limit: None,
        partial: false,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
        both_changed: Vec::new(),
        asks_which_branch: false,
        again: None,
    };
    opened(
        ApprovalKind::Merge,
        "Merge this run into main?",
        ApprovalPacket::Merge(packet),
        vec![
            option('y', "Merge", AnswerAction::Merge),
            option('p', "Open a pull request", AnswerAction::PullRequest),
            option(
                'b',
                "Request changes on some tasks",
                AnswerAction::RequestChanges,
            ),
            option('k', "Keep the branch", AnswerAction::KeepBranch),
            option('x', "Abandon the run", AnswerAction::Abandon),
            option('d', "Show the diff", AnswerAction::ShowDiff),
        ],
    )
}

fn fallback() -> ApprovalOpened {
    let candidate = |name: &str| Candidate {
        worker: worker(name),
        plan_usage_left: Some(0.62),
        plan_usage_text: "62%".into(),
        track_record: Some(TrackRecord { passed: 9, of: 11 }),
        cost: CostEstimate::PlanUsage(0.06),
        needs_confirm: false,
        may_run_out: false,
    };
    let plan = FallbackPlan {
        task: task_id(3),
        title: "Middleware".into(),
        stopped: worker("claude-code/opus"),
        reason: "usage limit, resets in 2h 10m".into(),
        resets_at: None,
        work: stat(4, 96, 12),
        checks_ran: false,
        candidates: vec![candidate("codex/gpt-5.6")],
        mode: FallbackMode::Ask,
        strategy: FallbackStrategy::First,
        race_count: 1,
        also_moving: Vec::new(),
    };
    let mut opened = opened(
        ApprovalKind::Fallback,
        "Fallback · task-003 Middleware",
        ApprovalPacket::Fallback(plan),
        vec![
            option(
                'a',
                "codex/gpt-5.6",
                AnswerAction::Candidate {
                    worker: worker("codex/gpt-5.6"),
                },
            ),
            option('t', "Take over myself", AnswerAction::TakeOver),
            option('w', "Wait until the reset", AnswerAction::Wait),
            option('o', "Pick another agent", AnswerAction::OtherCandidates),
            option('d', "Show the diff", AnswerAction::ShowDiff),
        ],
    );
    opened.task = Some(task_id(3));
    opened
}

fn stuck() -> ApprovalOpened {
    opened(
        ApprovalKind::Stuck,
        "Parser needs a decision",
        ApprovalPacket::Stuck(StuckPacket {
            task: task_id(4),
            title: "Parser".into(),
            state: argonauts_core::events::task::TaskState::NeedsDecision,
            tries: vec!["tests failed".into()],
            last_failure: "tests failed".into(),
        }),
        vec![
            option('r', "Retry with a note", AnswerAction::Retry),
            option('x', "Cancel the task", AnswerAction::Cancel),
            option('d', "Show the details", AnswerAction::ShowDiff),
        ],
    )
}

fn agent_down() -> ApprovalOpened {
    opened(
        ApprovalKind::AgentDown,
        "Claude Code is down",
        ApprovalPacket::AgentDown(AgentDownPacket {
            agent_id: AgentId("claude-code".into()),
            evidence: "overload errors on 3 sessions in 2 min".into(),
            test_at: Some(datetime!(2026-09-30 14:40 UTC)),
            paused: Vec::new(),
            fits_all: None,
            kept_by: None,
        }),
        vec![option('w', "Wait for Claude Code", AnswerAction::Wait)],
    )
}

fn choice(kind: ChoiceKind, options: Vec<AnswerOption>) -> AppChoice {
    AppChoice {
        id: ChoiceId::numbered(7),
        kind,
        title: "Save effort = high where?".into(),
        body: vec!["reasoning effort for the main agent".into()],
        options,
        free_text: FreeText::None,
        bell: false,
    }
}

fn approval(event: ApprovalEvent) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 1,
            ts: datetime!(2026-09-30 10:58 UTC),
            run_id: run(),
            task_id: None,
            session: None,
            source: "argonauts-code".into(),
            kind: RunEventKind::Approval(event),
        }),
    }
}

fn opened_event(opened: ApprovalOpened) -> AppEvent {
    approval(ApprovalEvent::Opened(Box::new(opened)))
}

fn decided(action: AnswerAction, by: Party) -> ApprovalDecided {
    ApprovalDecided {
        id: ApprovalId::numbered(1),
        action,
        text: None,
        by,
        evidence_sha256: HASH.into(),
        auto_preview: None,
        tasks: Vec::new(),
    }
}

#[test]
fn answer_carries_rendered_evidence_hash() {
    let mut screen = Screen::new();
    let out = screen.event(opened_event(merge()));
    assert!(out.bell, "a task that needs you rings once");
    assert!(screen.question().focused, "and takes the keys as it shows");

    let id = screen.question().id;
    let (taken, out) = screen.action(UiAction::Answer {
        question: id,
        option: 1,
        note: Some("ship it".into()),
        checked: Vec::new(),
    });
    assert!(taken);
    assert_eq!(
        out.commands,
        vec![UiCommand::Answer {
            to: AnswerTarget::Approval {
                run: run(),
                id: ApprovalId::numbered(1),
            },
            action: AnswerAction::PullRequest,
            text: Some("ship it".into()),
            evidence_sha256: Some(HASH.into()),
            tasks: Vec::new(),
            remember: false,
        }]
    );
    assert!(screen.questions.open.is_empty());
    assert!(screen.model.bottom.question.is_none());
}

#[test]
fn request_changes_sends_picked_tasks() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let mut keys = vec![
        number(&merge(), 'b'),
        KeyCode::Down,
        KeyCode::Char(' '),
        KeyCode::Enter,
    ];
    // Enter on the ticks opens the note, since the changes are what the agents act on.
    keys.extend("split the writer".chars().map(KeyCode::Char));
    keys.push(KeyCode::Enter);
    let effects = screen.keys(&keys);
    let commands: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    let [
        UiCommand::Answer {
            action,
            tasks,
            text,
            ..
        },
    ] = commands.as_slice()
    else {
        panic!("one answer, got {commands:?}");
    };
    assert_eq!(*action, AnswerAction::RequestChanges);
    assert_eq!(*tasks, vec![task_id(3)]);
    assert_eq!(text.as_deref(), Some("split the writer"));
}

#[test]
fn retry_opens_the_note_before_it_answers() {
    let mut screen = Screen::new();
    screen.event(opened_event(stuck()));
    let mut keys = vec![number(&stuck(), 'r')];
    keys.extend("mock the clock".chars().map(KeyCode::Char));
    let effects = screen.keys(&keys);
    assert!(effects.iter().all(|out| out.commands.is_empty()));
    let effects = screen.keys(&[KeyCode::Enter]);
    let commands: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    let [UiCommand::Answer { action, text, .. }] = commands.as_slice() else {
        panic!("one answer, got {commands:?}");
    };
    assert_eq!(*action, AnswerAction::Retry);
    assert_eq!(text.as_deref(), Some("mock the clock"));
}

fn scope_choice() -> AppChoice {
    let scope = |key: char, label: &str, value: &str| {
        option(
            key,
            label,
            AnswerAction::Choose {
                value: value.into(),
            },
        )
    };
    choice(
        ChoiceKind::SettingScope,
        vec![
            scope('s', "Just this session", "session"),
            scope('m', "This project, just me", "project_local"),
            scope('p', "This project, shared", "project"),
            scope('e', "Everywhere", "global"),
        ],
    )
}

#[test]
fn a_question_asks_over_any_view() {
    // One work raised: a view on top would keep the keys.
    let mut screen = Screen::new();
    let out = screen.event(AppEvent::Choice {
        choice: choice(ChoiceKind::StartWork, Vec::new()),
    });
    assert!(out.close_views, "it would wait behind the view");
    // One a change in a view raised closes the views but one that stays up for it, as Config
    // does for the change it sent.
    for kind in [ChoiceKind::SettingScope, ChoiceKind::ConfirmSafetySetting] {
        let mut screen = Screen::new();
        let out = screen.event(AppEvent::Choice {
            choice: choice(kind, Vec::new()),
        });
        assert!(
            out.views_make_way && !out.close_views,
            "{kind:?} would wait behind the view, or close the one it asks for"
        );
    }
}

fn preview() -> AppChoice {
    choice(
        ChoiceKind::TryWorkflow,
        vec![
            option('y', "Yes, run it", AnswerAction::Approve),
            option('n', "No", AnswerAction::Choose { value: "no".into() }),
        ],
    )
}

/// A dry run's question is its plan, so a workflow of many steps shows them all.
#[test]
fn a_dry_runs_question_shows_its_whole_plan_and_answers_only_to_its_keys() {
    let mut tried = preview();
    tried.title = "spec-first for \"add due dates\" · a preview, nothing runs yet".into();
    tried.body = (1..=12).map(|step| format!("{step} Step {step}")).collect();
    let question = choice_question(1, &tried);
    assert_eq!(question.body.len(), 12, "{:?}", question.body);
    assert!(question.detail.is_empty());
    assert_eq!(question.esc_hint, "y runs it · n closes it");
    assert!(question.focused && question.esc_answers && question.keys_only);

    // A y/N question answers to its letters alone: Enter and Esc do nothing, whichever row
    // is lit.
    for key in [
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyCode::Esc,
    ] {
        let mut question = choice_question(1, &tried);
        question.selected = 0;
        let answered = question.handle_key(crossterm::event::KeyEvent::from(key));
        assert!(
            matches!(answered, QuestionEvent::Moved),
            "{key:?}: {answered:?}"
        );
    }
    for (key, index) in [('y', 0), ('n', 1)] {
        let mut question = choice_question(1, &tried);
        let answered = question.handle_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char(key),
        ));
        assert!(
            matches!(answered, QuestionEvent::Answered { index: i, .. } if i == index),
            "{key}: {answered:?}"
        );
    }

    // It asks on one line, the default's letter a bold capital.
    let mut asked = preview();
    asked.title = "Run it now?".into();
    asked.options[1].recommended = true;
    let question = choice_question(1, &asked);
    let theme = crate::theme::Theme::dark();
    let lines = question.lines(&crate::render::RenderCtx::new(
        60,
        &theme,
        crate::render::Clock::still(std::time::Instant::now()),
    ));
    assert_eq!(crate::text::plain_text(&lines), "  Run it now? y / N");
    let n = lines[0].spans.last().unwrap();
    assert!(
        n.style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)
    );
    let y = &lines[0].spans[3];
    assert_eq!(y.style, theme.style(crate::theme::Role::Dim));

    // Any other question keeps to a few rows.
    let mut long = choice(ChoiceKind::ConfirmConnect, Vec::new());
    long.body = tried.body.clone();
    let shortened = choice_question(2, &long);
    assert_eq!(shortened.body.len(), 8);
    assert_eq!(
        shortened.body.last().map(String::as_str),
        Some("… +5 more lines")
    );
}

#[test]
fn a_choice_you_set_off_opens_with_the_keys() {
    let mut screen = Screen::new();
    screen.event(AppEvent::Choice {
        choice: scope_choice(),
    });
    assert!(screen.question().focused && screen.question().takes_focus);
    for kind in [
        ChoiceKind::ConfirmSafetySetting,
        ChoiceKind::TermsNote,
        ChoiceKind::Init,
        ChoiceKind::ConfirmRewind,
        ChoiceKind::ConfirmConnect,
        ChoiceKind::TrustProject,
        ChoiceKind::DetectedChecks,
        ChoiceKind::TryWorkflow,
    ] {
        let question = choice_question(1, &choice(kind, Vec::new()));
        assert!(question.focused && question.esc_answers, "{kind:?}");
    }
    // The unclear request holds your request, so it takes the keys like a run's question that
    // needs you, and Esc sets it aside instead of answering.
    let unclear = choice_question(2, &choice(ChoiceKind::StartWork, Vec::new()));
    assert!(unclear.needs_you && !unclear.takes_focus && !unclear.esc_answers);

    // A number answers it, never a letter.
    let question = screen.model.bottom.question.as_mut().unwrap();
    for c in ['s', 'm', 'p', 'e', '/'] {
        assert_eq!(
            question.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
            QuestionEvent::Unhandled
        );
    }
    let effects = screen.keys(&[KeyCode::Up, KeyCode::Char('4')]);
    assert_eq!(
        effects[0].commands,
        vec![UiCommand::Answer {
            to: AnswerTarget::Choice {
                id: ChoiceId::numbered(7),
            },
            action: AnswerAction::Choose {
                value: "global".into(),
            },
            text: None,
            evidence_sha256: None,
            tasks: Vec::new(),
            remember: false,
        }]
    );
}

#[test]
fn a_choice_you_set_off_goes_before_a_waiting_run_question() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let run_question = screen.question().id;
    assert!(
        screen.question().focused,
        "a run's task that needs you takes the keys"
    );
    screen.event(AppEvent::Choice {
        choice: scope_choice(),
    });
    assert_ne!(screen.question().id, run_question);
    assert!(
        screen.question().focused,
        "the scope question takes the keys at once"
    );

    screen.keys(&[KeyCode::Char('1')]);
    screen.event(AppEvent::ChoiceClosed {
        id: ChoiceId::numbered(7),
        outcome: "Saved.".into(),
    });
    assert_eq!(
        screen.question().id,
        run_question,
        "the run's task comes back"
    );
    assert!(
        screen.question().focused,
        "and takes the keys again, since you never passed it on"
    );

    // Passed on, it stays off the screen after the next question.
    screen.keys(&[KeyCode::Esc]);
    screen.event(AppEvent::Choice {
        choice: scope_choice(),
    });
    screen.keys(&[KeyCode::Char('1')]);
    screen.event(AppEvent::ChoiceClosed {
        id: ChoiceId::numbered(7),
        outcome: "Saved.".into(),
    });
    assert!(screen.model.bottom.question.is_none());
    assert!(screen.questions.open.contains_key(&run_question));
}

/// "Show the file" on a trust question asks the app, which prints the files above it: the
/// question's own words aren't what you need to read.
#[test]
fn a_trust_question_s_files_are_shown_by_the_app() {
    let mut screen = Screen::new();
    let trust = choice(
        ChoiceKind::TrustProject,
        vec![
            option('y', "Trust them", AnswerAction::Approve),
            option('n', "Not now", AnswerAction::Cancel),
            option('d', "Show the file", AnswerAction::ShowDiff),
        ],
    );
    screen.event(AppEvent::Choice { choice: trust });
    let effects = screen.keys(&[KeyCode::Char('3')]);
    let commands: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    assert!(
        matches!(
            commands.as_slice(),
            [UiCommand::Answer {
                action: AnswerAction::ShowDiff,
                ..
            }]
        ),
        "{commands:?}"
    );
    assert!(screen.model.bottom.question.is_some(), "the question stays");
}

#[test]
fn app_choice_esc_sends_set_aside() {
    let mut screen = Screen::new();
    let scope = choice(
        ChoiceKind::SettingScope,
        vec![
            option(
                's',
                "Just this session",
                AnswerAction::Choose {
                    value: "session".into(),
                },
            ),
            option(
                'e',
                "Everywhere",
                AnswerAction::Choose {
                    value: "global".into(),
                },
            ),
        ],
    );
    screen.event(AppEvent::Choice { choice: scope });
    assert_eq!(screen.question().esc_hint, "Esc to cancel");
    // Esc answers it, so only Esc on the focused question does. An unclear request just waits.
    assert!(screen.question().esc_answers);
    let unclear = choice_question(2, &choice(ChoiceKind::StartWork, Vec::new()));
    assert!(!unclear.esc_answers);

    let effects = screen.keys(&[KeyCode::Esc]);
    let commands: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    assert_eq!(
        commands,
        vec![&UiCommand::SetAside {
            to: AnswerTarget::Choice {
                id: ChoiceId::numbered(7),
            },
        }]
    );

    // The controller closes it with its outcome line.
    screen.event(AppEvent::ChoiceClosed {
        id: ChoiceId::numbered(7),
        outcome: "Not changed.".into(),
    });
    assert!(screen.questions.open.is_empty());
    assert!(screen.model.bottom.question.is_none());
    assert_eq!(
        screen.history(),
        vec!["◆ Save effort = high where? · Not changed.".to_owned()]
    );
}

#[test]
fn not_changed_leaves_a_neutral_mark() {
    let tone = |outcome: &str| match records::choice_closed_cell(Some("Save model where?"), outcome)
    {
        crate::cell::Cell::Notice(notice) => notice.tone,
        _ => panic!("a notice"),
    };
    assert_eq!(
        tone(argonauts_core::app::NOT_CHANGED),
        crate::cell::Tone::Dim
    );
    assert_eq!(tone("Saved to this project."), crate::cell::Tone::Success);
}

/// Esc on a run's question or approval hands it on: the engine takes an option in your place,
/// and its decision closes the question.
#[test]
fn esc_hands_a_run_question_on() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let effects = screen.keys(&[KeyCode::Esc]);
    let sent: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    assert!(
        matches!(
            sent.as_slice(),
            [UiCommand::SetAside {
                to: AnswerTarget::Approval { .. }
            }]
        ),
        "the engine hears it, so whoever asked decides: {sent:?}"
    );
    assert!(
        screen.model.bottom.question.is_none(),
        "no line stays behind"
    );
    let record = screen.history().last().cloned().unwrap_or_default();
    assert!(
        record.starts_with("◆ Merge approval left to the main agent · you, "),
        "{record}"
    );

    // Until the engine settles it, the list of what needs you still opens it, as it was.
    assert_eq!(screen.questions.open.len(), 1);
    let (taken, _) = screen.action(UiAction::AnswerQuestion(AnswerTarget::Approval {
        run: run(),
        id: merge().id,
    }));
    assert!(taken);
    assert!(screen.question().focused && !screen.question().dismissed);
}

/// Esc's line says the question was left to the agent, so the decision the engine records for
/// it prints nothing more: the line shows once.
#[test]
fn esc_s_line_prints_once() {
    use argonauts_core::events::approval::LEFT_TO_THE_AGENT;

    let mut screen = Screen::new();
    let mut opened = merge();
    opened.kind = ApprovalKind::Question;
    screen.event(opened_event(opened));
    screen.keys(&[KeyCode::Esc]);
    let before = screen.history().len();
    let mut left = decided(AnswerAction::Answer, Party::Declined);
    left.text = Some(LEFT_TO_THE_AGENT.to_owned());
    screen.event(approval(ApprovalEvent::Decided(left)));
    let history = screen.history();
    assert_eq!(history.len(), before, "{history:#?}");
    let lines = history
        .iter()
        .filter(|line| line.contains("left to the agent"))
        .count();
    assert_eq!(lines, 1, "{history:#?}");
}

/// What Esc left reads as left, by you, and never quotes what the agent was told.
#[test]
fn a_question_you_left_is_recorded_as_left_to_the_agent() {
    use argonauts_core::events::approval::LEFT_TO_THE_AGENT;

    let mut left = decided(AnswerAction::Answer, Party::Declined);
    left.text = Some(LEFT_TO_THE_AGENT.to_owned());
    let mut opened = merge();
    opened.kind = ApprovalKind::Question;
    let record = records::record(
        &opened,
        &left,
        None,
        None,
        time::UtcOffset::UTC,
        &DisplayNames::default(),
        &records::by_id,
    );
    assert_eq!(record.text, "Question left to the agent");
    assert_eq!(record.decided.as_deref(), Some("you"));
    assert!(record.detail.is_empty(), "{:?}", record.detail);
}

#[test]
fn an_agents_own_question_is_left_to_that_agent() {
    let at = datetime!(2026-10-06 20:47 UTC);
    let record = |kind| {
        let cell = records::passed_on_cell(kind, at, UtcOffset::UTC);
        let crate::cell::Cell::Notice(notice) = cell else {
            panic!("a notice");
        };
        assert_eq!(
            notice.tone,
            crate::cell::Tone::Dim,
            "nothing is settled yet"
        );
        format!("{} · {}", notice.text, notice.decided.unwrap_or_default())
    };
    assert_eq!(
        record(ApprovalKind::Question),
        "Question left to the agent · you, 8:47 PM"
    );
    assert_eq!(
        record(ApprovalKind::Plan),
        "Plan approval left to the main agent · you, 8:47 PM"
    );
}

#[test]
fn fallback_remember_toggle_sets_remember() {
    let mut screen = Screen::new();
    screen.event(opened_event(fallback()));
    let toggle = screen.question().options.last().unwrap().clone();
    assert_eq!(toggle.kind, OptionKind::Toggle);
    assert_eq!(toggle.label, REMEMBER_LABEL);

    let id = screen.question().id;
    let number = char::from_digit(screen.question().options.len() as u32, 10).unwrap();
    screen.keys(&[KeyCode::Char('m')]);
    assert!(
        !screen.questions.open[&id].remember,
        "a letter does nothing"
    );
    screen.keys(&[KeyCode::Char(number)]);
    assert!(
        screen.questions.open[&id].remember,
        "its number turns it on"
    );
    assert!(screen.question().options.last().unwrap().on);
    screen.keys(&[KeyCode::Char(number)]);
    assert!(
        !screen.questions.open[&id].remember,
        "a second press turns it off"
    );
    screen.keys(&[KeyCode::Char(number)]);

    let candidate = fallback()
        .options
        .iter()
        .position(|option| matches!(option.action, AnswerAction::Candidate { .. }))
        .unwrap();
    let candidate = char::from_digit(candidate as u32 + 1, 10).unwrap();
    let effects = screen.keys(&[KeyCode::Char(candidate)]);
    let commands: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    let [
        UiCommand::Answer {
            action, remember, ..
        },
    ] = commands.as_slice()
    else {
        panic!("one answer, got {commands:?}");
    };
    assert!(matches!(action, AnswerAction::Candidate { .. }));
    assert!(*remember);
    assert_eq!(
        screen.model.bottom.answered_pick, None,
        "a pick saved as you made it isn't offered again"
    );
}

#[test]
fn an_answered_pick_is_offered_as_a_rule() {
    let mut screen = Screen::new();
    screen.event(opened_event(fallback()));
    screen.keys(&[number(&fallback(), 'a')]);
    let pick = screen
        .model
        .bottom
        .answered_pick
        .clone()
        .expect("the pick is offered");
    assert_eq!(
        (&pick.run, &pick.id, pick.selected),
        (&run(), &ApprovalId::numbered(1), false)
    );
    assert!(
        pick.outcome.starts_with("task-003 fell back to "),
        "{}",
        pick.outcome
    );

    // Answering anything else lets it go.
    screen.event(opened_event(merge()));
    screen.keys(&[number(&merge(), 'p')]);
    assert_eq!(screen.model.bottom.answered_pick, None);
}

#[test]
fn fallback_remember_stays_off_for_other_answers() {
    let mut screen = Screen::new();
    screen.event(opened_event(fallback()));
    let remember =
        KeyCode::Char(char::from_digit(screen.question().options.len() as u32, 10).unwrap());
    let effects = screen.keys(&[remember, number(&fallback(), 'w')]);
    let commands: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    let [
        UiCommand::Answer {
            action, remember, ..
        },
    ] = commands.as_slice()
    else {
        panic!("one answer, got {commands:?}");
    };
    assert_eq!(*action, AnswerAction::Wait);
    assert!(!*remember, "only a picked candidate is remembered");
    assert_eq!(screen.model.bottom.answered_pick, None);
}

#[test]
fn spot_check_sends_spot_check() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    let out = screen.event(opened_event(auto));
    assert!(!out.bell, "nothing rings while your rules decide");
    screen.event(approval(ApprovalEvent::Decided(decided(
        AnswerAction::Merge,
        Party::Rules,
    ))));
    let out = screen.event(approval(ApprovalEvent::SpotCheckQueued {
        id: ApprovalId::numbered(1),
    }));
    assert!(!out.bell, "a spot check never rings");
    let question = screen.question();
    assert_eq!(question.title, "Spot check · Merge this run into main?");
    let labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, vec!["Agree", "Overrule", "Show the evidence"]);

    let effects = screen.keys(&[KeyCode::Char('2')]);
    assert_eq!(
        effects[0].commands,
        vec![UiCommand::SpotCheck {
            run: run(),
            id: ApprovalId::numbered(1),
            correct: false,
        }]
    );
}

/// A spot check takes the keys like any question, so Esc has to give them back: it closes
/// the question and answers nothing, and the check still waits in `/approvals`.
#[test]
fn esc_closes_a_spot_check_and_answers_nothing() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    screen.event(approval(ApprovalEvent::Decided(decided(
        AnswerAction::Merge,
        Party::Rules,
    ))));
    screen.event(approval(ApprovalEvent::SpotCheckQueued {
        id: ApprovalId::numbered(1),
    }));
    assert_eq!(
        screen.question().title,
        "Spot check · Merge this run into main?"
    );

    let effects = screen.keys(&[KeyCode::Esc]);
    let sent: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    assert!(sent.is_empty(), "nothing is decided for you: {sent:?}");
    assert!(
        screen.model.bottom.question.is_none(),
        "the keys are yours again"
    );
    assert!(screen.questions.open.is_empty());
}

#[test]
fn a_question_auto_leaves_to_you_rings_and_says_why_once() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    let out = screen.event(opened_event(auto));
    assert!(!out.bell, "nothing rings while auto decides");
    let check = |auto_preview: bool, reasons: &[&str]| {
        approval(ApprovalEvent::Checked(
            argonauts_core::events::approval::PolicyCheck {
                id: ApprovalId::numbered(1),
                violations: Vec::new(),
                verdict: auto_preview.then_some(argonauts_core::policy::Verdict::AskYou),
                by: None,
                judge: None,
                reasons: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
                auto_preview,
            },
        ))
    };
    let out = screen.event(check(false, &["a hard limit, the checks didn't run"]));
    assert!(out.bell, "a task auto couldn't settle rings");
    // The auto preview record that follows carries every reason, which Ctrl+O shows instead.
    screen.event(check(true, &["the checks didn't run", "no review result"]));
    screen.event(check(false, &["a hard limit, the checks didn't run"]));
    let why: Vec<&String> = screen
        .question()
        .detail
        .iter()
        .filter(|line| line.starts_with("Auto couldn't decide"))
        .collect();
    assert_eq!(
        why,
        ["Auto couldn't decide: a hard limit, the checks didn't run"]
    );
}

/// Through the whole bridge, as the screen sends it: once your answer goes out, the spot check
/// no longer waits, so `/approvals` can't list it or send a second answer.
#[test]
fn a_spot_check_you_answered_stops_waiting() {
    let mut bridge = crate::bridge::Bridge::new(&crate::UiOptions::default());
    let mut model = ViewModel::default();
    let now = Instant::now();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    let id = ApprovalId::numbered(1);
    for event in [
        opened_event(auto),
        approval(ApprovalEvent::Decided(decided(
            AnswerAction::Merge,
            Party::Rules,
        ))),
        approval(ApprovalEvent::SpotCheckQueued { id: id.clone() }),
    ] {
        bridge.on_event(&mut model, &event, now);
    }
    let waiting = |bridge: &crate::bridge::Bridge| {
        bridge
            .state
            .runs
            .get(&run())
            .map_or(0, |run| run.spot_checks.len())
    };
    assert_eq!(waiting(&bridge), 1);

    let answer = UiCommand::SpotCheck {
        run: run(),
        id,
        correct: true,
    };
    let out = bridge.on_action(&mut model, &UiAction::Command(answer.clone()), now);
    assert_eq!(out.commands, vec![answer]);
    assert_eq!(waiting(&bridge), 0);
}

#[test]
fn options_keep_event_order() {
    for opened in [merge(), fallback(), stuck()] {
        let question = approval_question(1, &opened, None);
        let drawn: Vec<&str> = question
            .options
            .iter()
            .take(opened.options.len())
            .map(|option| option.label.as_str())
            .collect();
        let sent: Vec<&str> = opened
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(drawn, sent, "{}", opened.title);
    }

    let kinds = |opened: &ApprovalOpened| -> Vec<OptionKind> {
        approval_question(1, opened, None)
            .options
            .iter()
            .map(|option| option.kind)
            .collect()
    };
    use OptionKind::*;
    assert_eq!(
        kinds(&merge()),
        vec![Irreversible, Answer, Checklist, Answer, Irreversible, View]
    );
    assert_eq!(kinds(&stuck()), vec![Answer, Irreversible, View]);

    // An app choice's Cancel is only Not now or Keep it as it is.
    let trust = choice(
        ChoiceKind::TrustProject,
        vec![
            option('y', "Trust them", AnswerAction::Approve),
            option('n', "Not now", AnswerAction::Cancel),
            option('d', "Show the file", AnswerAction::ShowDiff),
        ],
    );
    let question = choice_question(1, &trust);
    let kinds: Vec<OptionKind> = question.options.iter().map(|option| option.kind).collect();
    assert_eq!(kinds, vec![Answer, Answer, View]);
}

#[test]
fn record_line_after_decided() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let about = screen.history();
    assert_eq!(about.len(), 1, "the merge table goes in first");
    assert!(about[0].contains("task-002") && about[0].contains("Suite passed"));

    screen.event(approval(ApprovalEvent::Decided(decided(
        AnswerAction::Merge,
        Party::User,
    ))));
    assert!(screen.questions.open.is_empty());
    assert!(screen.model.bottom.question.is_none());
    assert_eq!(
        screen.history().last().map(String::as_str),
        // The screen shows one run, so its record says "the run" rather than its id.
        Some("◆ Approved merging the run into main · you, 10:58 AM")
    );
}

/// Keeping the branch says what was kept, in words, rather than naming the branch.
#[test]
fn keeping_the_branch_says_the_work_stays_unmerged() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    screen.event(approval(ApprovalEvent::Decided(decided(
        AnswerAction::KeepBranch,
        Party::User,
    ))));
    let record = screen.history().last().cloned().unwrap_or_default();
    assert_eq!(
        record,
        "◆ Kept the run's work on its branch, not merged · you, 10:58 AM"
    );
    assert!(!record.contains("argonauts/"), "{record}");
}

#[test]
fn question_times_are_in_the_terminals_zone() {
    let mut screen = Screen::new();
    screen.state.terminal.utc_offset = Some(UtcOffset::from_hms(-7, 0, 0).unwrap());
    screen.event(opened_event(agent_down()));
    let about = screen.history();
    assert!(
        about
            .iter()
            .any(|line| line.contains("tries it again at 7:40 AM")),
        "{about:?}"
    );
    // On screen the wait counts down to that time. A reader that draws nothing is given the
    // clock time, in the same zone.
    let waits = &screen.question().options[0];
    assert!(waits.counts_to.is_some(), "{waits:?}");
    let zone = UtcOffset::from_hms(-7, 0, 0).unwrap();
    let read = approval_question_local(1, &agent_down(), None, zone, &DisplayNames::default());
    assert_eq!(
        read.options[0].description.as_deref(),
        Some("tried again at 7:40 AM")
    );

    screen.event(approval(ApprovalEvent::Decided(decided(
        AnswerAction::Wait,
        Party::User,
    ))));
    assert!(
        screen
            .history()
            .last()
            .is_some_and(|line| line.ends_with("you, 3:58 AM")),
        "{:?}",
        screen.history().last()
    );
}

#[test]
fn a_withdrawn_question_goes_with_a_dim_line() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    screen.event(approval(ApprovalEvent::Withdrawn {
        id: ApprovalId::numbered(1),
        reason: "the evidence changed".into(),
    }));
    assert!(screen.model.bottom.question.is_none());
    assert_eq!(
        screen.history().last().map(String::as_str),
        Some("◆ Merge this run into main? · no longer needed: the evidence changed")
    );
}

#[test]
fn several_questions_show_the_oldest_and_answer_question_focuses_one() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let mut second = fallback();
    second.id = ApprovalId::numbered(2);
    screen.event(opened_event(second));
    assert_eq!(screen.question().title, "Merge this run into main?");
    assert_eq!(screen.questions.open.len(), 2);

    // Handing the oldest on brings the next one up.
    screen.keys(&[KeyCode::Esc]);
    assert_eq!(screen.question().title, "Fallback · task-003 Middleware");
    assert!(!screen.question().dismissed);

    let (taken, out) = screen.action(UiAction::AnswerQuestion(AnswerTarget::Approval {
        run: run(),
        id: ApprovalId::numbered(1),
    }));
    assert!(taken && out.close_views);
    assert_eq!(screen.question().title, "Merge this run into main?");
    assert!(screen.question().focused && !screen.question().dismissed);
}

#[test]
fn take_over_confirm_sends_take_over() {
    let mut screen = Screen::new();
    let (taken, out) = screen.action(UiAction::ConfirmTakeOver {
        run: run(),
        task: task_id(2),
    });
    assert!(taken && out.close_views);
    assert!(
        screen.question().focused,
        "you asked for it, so it has the keys"
    );
    assert_eq!(screen.question().esc_hint, "Esc to cancel");
    assert!(screen.question().esc_answers);
    let effects = screen.keys(&[KeyCode::Char('1')]);
    assert_eq!(
        effects[0].commands,
        vec![UiCommand::TakeOver {
            run: run(),
            task: task_id(2),
        }]
    );

    // Esc cancels with nothing sent.
    screen.action(UiAction::ConfirmTakeOver {
        run: run(),
        task: task_id(2),
    });
    let effects = screen.keys(&[KeyCode::Esc]);
    assert!(effects.iter().all(|out| out.commands.is_empty()));
    assert!(screen.questions.open.is_empty() && screen.model.bottom.question.is_none());
}

#[test]
fn show_the_diff_asks_for_the_task_diff_and_keeps_the_question() {
    let mut screen = Screen::new();
    screen.event(opened_event(fallback()));
    let effects = screen.keys(&[number(&fallback(), 'd')]);
    assert_eq!(
        effects[0].commands,
        vec![UiCommand::Request {
            view: ViewRequest::Diff {
                run: run(),
                task: task_id(3),
                scope: DiffScope::Task,
            },
        }]
    );
    assert_eq!(screen.questions.open.len(), 1);
    assert!(screen.model.bottom.question.is_some());
}

#[test]
fn a_choice_rings_only_when_it_asks_to() {
    let mut screen = Screen::new();
    let mut start = choice(
        ChoiceKind::StartWork,
        vec![option(
            's',
            "Captain",
            AnswerAction::Choose {
                value: "captain".into(),
            },
        )],
    );
    assert!(
        !screen
            .event(AppEvent::Choice {
                choice: start.clone()
            })
            .bell
    );
    start.id = ChoiceId::numbered(8);
    start.bell = true;
    assert!(screen.event(AppEvent::Choice { choice: start }).bell);
}

/// A review the main agent picked, with no plan of its own: the workflow is what you approve.
fn review_plan() -> ApprovalOpened {
    use argonauts_core::events::approval::{PlanPacket, PlanWorkflow};
    use argonauts_core::events::plan::{LimitPlan, TaskGraph};
    use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
    use argonauts_core::workflow::WorkflowRef;

    let workflow = serde_json::from_value(serde_json::json!({
        "name": "review-panel",
        "description": "Two reviewers from other providers",
        "params": {"target": "diff", "reviewers": 2},
        "phases": [
            {"name": "Review", "standard": "check", "steps": [{
                "kind": "fan_out", "role": "reviewer", "count": 2, "blind": true,
                "different_provider_from": "author"
            }]},
            {"name": "Settle", "standard": "decide", "steps": [{
                "kind": "judge", "role": "judge", "when": "reviewers_disagree"
            }]}
        ]
    }))
    .expect("a workflow");
    let why = "A review: two reviewers from other providers";
    let packet = PlanPacket {
        summary: why.into(),
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
        workflow: Some(PlanWorkflow {
            from: Some(WorkflowRef::new("review-panel")),
            why: Some(why.into()),
            workflow,
        }),
    };
    opened(
        ApprovalKind::Plan,
        "Approve this plan?",
        ApprovalPacket::Plan(Box::new(packet)),
        vec![
            option('y', "Approve the plan", AnswerAction::Approve),
            option('x', "Abandon", AnswerAction::Abandon),
        ],
    )
}

/// The main view says the workflow in plain words, and its conditions and loop rules wait for
/// Ctrl+O.
#[test]
fn a_plan_shows_the_workflow_it_follows() {
    let mut screen = Screen::new();
    screen.event(opened_event(review_plan()));
    let history = screen.history().join("\n");
    let item = &history[history.find("The plan").expect("the plan's item")..];
    for shown in [
        "Workflow  Review-panel · Two reviewers from other providers",
        "reviewers 2 · target diff",
        "    review → settle (if needed)\n    … +2 lines (ctrl+o to expand)",
    ] {
        assert!(item.contains(shown), "{shown} missing from:\n{item}");
    }
    for hidden in ["fan_out", "reviewers_disagree"] {
        assert!(!item.contains(hidden), "{hidden} shown in:\n{item}");
    }
    // The main agent's reason is the plan's summary here, so it shows once.
    assert_eq!(item.matches("A review: two reviewers").count(), 1, "{item}");
    assert_eq!(
        screen.question().detail.first().map(String::as_str),
        Some("review-panel · review → settle (if needed) · limit $20.00")
    );

    let expanded = screen.history_as(true).join("\n");
    for shown in [
        "Review: fan_out reviewer ×2, blind, another provider than author",
        "Settle: judge, when reviewers_disagree",
    ] {
        assert!(
            expanded.contains(shown),
            "{shown} missing from:\n{expanded}"
        );
    }
    assert!(!expanded.contains("ctrl+o to expand"), "{expanded}");
}

/// A loop reads as its rounds: `understand (up to 2 rounds) → plan (up to 3 rounds) → build`.
#[test]
fn a_looping_phase_says_its_rounds() {
    let mut opened = review_plan();
    let ApprovalPacket::Plan(plan) = &mut opened.packet else {
        unreachable!("a plan approval")
    };
    let shown = plan.workflow.as_mut().expect("a workflow");
    shown.workflow = serde_json::from_value(serde_json::json!({
        "name": "council",
        "description": "",
        "phases": [
            {"name": "Understand", "standard": "understand", "steps": [{
                "kind": "loop_until", "until": "understood", "max": 2,
                "steps": [{"kind": "read"}, {"kind": "approval", "approval": "understand"}]
            }]},
            {"name": "Build", "standard": "do", "steps": [{"kind": "build", "role": "worker"}]},
            {"name": "Merge", "standard": "decide", "steps": [{"kind": "merge"}]}
        ]
    }))
    .expect("a workflow");
    let mut screen = Screen::new();
    screen.event(opened_event(opened));
    let history = screen.history().join("\n");
    assert!(
        history.contains("understand (up to 2 rounds) → build → merge"),
        "{history}"
    );
    assert!(!history.contains("understood"), "{history}");
}

/// Steps the runner runs have no step kinds, so each phase shows the line its script gave it.
#[test]
fn a_plan_of_a_scripts_steps_shows_each_phase_with_its_line() {
    use argonauts_core::workflow::WorkflowRef;

    let mut opened = review_plan();
    let ApprovalPacket::Plan(plan) = &mut opened.packet else {
        unreachable!("a plan approval")
    };
    plan.summary = "The orchestrator wrote these steps for your request".into();
    let shown = plan.workflow.as_mut().expect("a workflow");
    shown.from = Some(WorkflowRef::new("council"));
    shown.why = None;
    shown.workflow = serde_json::from_value(serde_json::json!({
        "name": "add-due-dates",
        "description": "Add due dates to tasks",
        "phases": [
            {"name": "Plan", "detail": "orchestrator · 1 agent", "standard": "do", "steps": []},
            {"name": "Build", "detail": "workers · one per task", "standard": "do", "steps": []},
            {"name": "Sign off", "standard": "do", "steps": []}
        ]
    }))
    .expect("a workflow");
    let mut screen = Screen::new();
    screen.event(opened_event(opened));
    let history = screen.history().join("\n");
    for shown in [
        "Workflow  Add-due-dates · Add due dates to tasks · started from Council",
        "plan → build → sign off",
    ] {
        assert!(history.contains(shown), "{shown} missing from:\n{history}");
    }
    assert_eq!(
        screen.question().detail.first().map(String::as_str),
        Some("add-due-dates · plan → build → sign off · limit $20.00")
    );
    let expanded = screen.history_as(true).join("\n");
    for shown in [
        "Plan: orchestrator · 1 agent",
        "Build: workers · one per task",
        "Sign off",
    ] {
        assert!(
            expanded.contains(shown),
            "{shown} missing from:\n{expanded}"
        );
    }
    assert!(!expanded.contains("Sign off:"), "{expanded}");
}

#[test]
fn a_plan_names_the_agents_its_workflow_names() {
    use argonauts_core::workflow::StepModels;

    let mut opened = review_plan();
    let ApprovalPacket::Plan(plan) = &mut opened.packet else {
        unreachable!("a plan approval")
    };
    let shown = plan.workflow.as_mut().expect("a workflow");
    shown.workflow.phases[0].steps[0].models =
        StepModels::Named(vec!["codex/gpt-5.6".parse().expect("a model")]);
    let mut screen = Screen::new();
    screen.event(opened_event(opened));
    let history = screen.history_as(true).join("\n");
    assert!(
        history.contains("Review: fan_out reviewer (GPT 5.6 on Codex) ×2"),
        "{history}"
    );
}

/// The plan question's title asks the question, so the question doesn't ask it a second time.
#[test]
fn a_question_whose_title_asks_asks_once() {
    let question = approval_question(1, &review_plan(), None);
    assert_eq!(question.title, "Approve this plan?");
    assert_eq!(question.question, None);
}

/// The merge table names each worker as the transcript did: by the model its session ran, else
/// by the model its CLI's settings give.
#[test]
fn the_merge_table_names_workers_as_the_run_knows_them() {
    let mut opened = merge();
    if let ApprovalPacket::Merge(packet) = &mut opened.packet {
        packet.rows[0].worker = Some(worker("claude-code"));
        packet.rows[1].worker = Some(worker("codex"));
    }
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        worker("claude-code/claude-haiku-4-5"),
        "claude-code",
        argonauts_core::names::RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.task = Some(task_id(2));
    run.sessions.insert(session.key.clone(), session);
    let names = DisplayNames {
        configured: std::collections::BTreeMap::from([(
            "codex".to_owned(),
            "gpt-5.6-luna".to_owned(),
        )]),
        ..DisplayNames::default()
    };
    let named = with_run_workers(&opened, Some(&run), &names);
    let ApprovalPacket::Merge(packet) = &named.packet else {
        unreachable!("a merge approval")
    };
    let workers: Vec<String> = packet
        .rows
        .iter()
        .map(|row| names.worker(row.worker.as_ref().unwrap()))
        .collect();
    assert_eq!(
        workers,
        ["Haiku 4.5 on Claude Code", "GPT 5.6 Luna on Codex"]
    );
}

/// The merge table above the question names each task by its label, as the conversation does, and a
/// task with none yet by its id.
#[test]
fn the_merge_table_names_tasks_by_label() {
    let opened = merge();
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        worker("codex/gpt-5.6"),
        "codex",
        argonauts_core::names::RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.task = Some(task_id(2));
    session.step = Some(argonauts_core::steps::StepName::Build);
    session.label = Some("build:writer".into());
    run.session_order.push(session.key.clone());
    run.sessions.insert(session.key.clone(), session);
    let table = |run: Option<&crate::state::RunState>| {
        about_item(&opened, run, UtcOffset::UTC, &DisplayNames::default())
            .map(|item| item.plain(100).join("\n"))
            .unwrap_or_default()
    };
    let labeled = table(Some(&run));
    assert!(
        labeled.contains("build:writer  Add the JSON writer"),
        "{labeled}"
    );
    assert!(!labeled.contains("task-002"), "{labeled}");
    assert!(labeled.contains("task-003"), "{labeled}");
    // Without the run, as the screen reader reads it to type ids, the ids stay.
    assert!(table(None).contains("task-002"));

    // A task titled with the label its worker goes by is named once.
    let mut opened = opened;
    if let ApprovalPacket::Merge(merge) = &mut opened.packet {
        "build:writer".clone_into(&mut merge.rows[0].title);
    }
    let once = about_item(
        &opened,
        Some(&run),
        UtcOffset::UTC,
        &DisplayNames::default(),
    )
    .map(|item| item.plain(100).join("\n"))
    .unwrap_or_default();
    assert_eq!(once.matches("build:writer").count(), 1, "{once}");
}

/// With no command checks set nothing ran on the latest main, and the question says so rather than
/// calling the suite failed.
#[test]
fn a_merge_without_a_suite_says_none_ran() {
    use argonauts_core::events::task::{CheckKind, CheckOutcome};

    let suite = |passed: bool, command: Option<Vec<String>>| {
        let mut opened = merge();
        if let ApprovalPacket::Merge(packet) = &mut opened.packet {
            packet.suite_passed_on_latest_main = passed;
            packet.checks = vec![CheckOutcome {
                name: "tests".into(),
                kind: CheckKind::Command,
                passed,
                command,
                exit_code: None,
                duration_ms: 0,
                summary: String::new(),
                output_tail: String::new(),
                violations: Vec::new(),
                sandboxed: false,
                skipped: false,
            }];
        }
        let question = approval_question(1, &opened, None).detail.join("\n");
        let item = about_item(&opened, None, UtcOffset::UTC, &DisplayNames::default())
            .map(|item| item.plain(100).join("\n"))
            .unwrap_or_default();
        (question, item)
    };
    let (question, item) = suite(false, None);
    assert!(question.contains("no test suite set"), "{question}");
    assert!(item.contains("No test suite ran"), "{item}");
    let (question, item) = suite(false, Some(vec!["npm".into(), "test".into()]));
    assert!(
        question.contains("suite failed on the latest main"),
        "{question}"
    );
    assert!(item.contains("Suite didn't pass"), "{item}");
    let (question, _) = suite(true, Some(vec!["npm".into(), "test".into()]));
    assert!(
        question.contains("suite passed on the latest main"),
        "{question}"
    );
}

/// A question a task's agent asked you that nothing waited on is in the merge question beside
/// its task, and so is one answered in your place, with who answered and what.
#[test]
fn the_merge_question_shows_the_questions_its_tasks_asked_you() {
    use argonauts_core::events::approval::MergeQuestion;

    let mut opened = merge();
    if let ApprovalPacket::Merge(packet) = &mut opened.packet {
        packet.rows[0].questions = vec![MergeQuestion {
            text: "Should the judge's \"not ready\" block the merge,\nor only warn?".into(),
            answer: None,
            answered_by: None,
        }];
        packet.rows[1].questions = vec![MergeQuestion {
            text: "Keep the old flag?".into(),
            answer: Some("Yes, as an alias".into()),
            answered_by: Some(worker("claude-code/opus")),
        }];
    }
    let item = about_item(&opened, None, UtcOffset::UTC, &DisplayNames::default())
        .map(|item| item.plain(100))
        .unwrap_or_default();
    let at = |text: &str| {
        item.iter()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("{text:?} in {item:#?}"))
    };
    let open =
        at("task-002 asked you: Should the judge's \"not ready\" block the merge, or only warn?");
    assert!(
        item[open + 1]
            .contains("Nothing waited for your answer, so the task went on to its reviews")
    );
    let answered = at("task-003 asked you: Keep the old flag?");
    assert!(
        item[answered + 1].contains("answered in your place: Yes, as an alias"),
        "{item:#?}"
    );
}

/// The question counts the run's spend as the bottom line and the working line do, even when the
/// packet's count lags behind the sessions' own reports.
#[test]
fn the_merge_question_counts_the_spend_the_bottom_row_does() {
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        worker("claude-code"),
        "claude-code",
        argonauts_core::names::RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.cost_usd = Some(0.26);
    run.sessions.insert(session.key.clone(), session);
    let question = approval_question(1, &merge(), Some(&run));
    let detail = question.detail.join("\n");
    assert!(detail.contains("$0.26 spent"), "{detail}");
    assert!((run.cost_usd() - 0.26).abs() < f64::EPSILON);
    // Without the run, the packet's count is all there is.
    let question = approval_question(1, &merge(), None);
    assert!(question.detail.join("\n").contains("$0.00 spent"));
}

/// A try that ended without a report lists nothing in the packet, so the question counts the
/// task's worker sessions.
#[test]
fn an_stuck_counts_tries_that_reported_nothing() {
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    for n in 1..=2 {
        let mut session = crate::state::SessionState::new(
            argonauts_core::names::SessionKey::numbered(n),
            worker("opencode"),
            "opencode",
            argonauts_core::names::RoleName::Worker,
            datetime!(2026-09-30 10:00 UTC),
        );
        session.task = Some(task_id(4));
        session.step = Some(argonauts_core::steps::StepName::Build);
        run.sessions.insert(session.key.clone(), session);
    }
    let mut opened = stuck();
    if let ApprovalPacket::Stuck(packet) = &mut opened.packet {
        packet.tries.clear();
    }
    let detail = approval_question(1, &opened, Some(&run)).detail.join("\n");
    assert!(detail.contains("Parser · 2 tries"), "{detail}");
    assert!(!detail.contains("task-004"), "{detail}");
    let detail = approval_question(1, &opened, None).detail.join("\n");
    assert!(detail.contains("· 0 tries"), "{detail}");
}

/// A plan's author ran its CLI's default, so the question names it by that model, as the
/// transcript named the draft's session.
#[test]
fn a_plans_author_reads_as_its_session_did() {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::events::plan::{JudgingResult, PlanTally};
    use argonauts_core::settings::protocol::JudgingMethod;

    let mut opened = review_plan();
    if let ApprovalPacket::Plan(packet) = &mut opened.packet {
        packet.judging = Some(JudgingResult {
            method: JudgingMethod::Cross,
            plans: vec![PlanTally {
                label: 1,
                author: worker("codex"),
                mean: 8.0,
                spread: 0.0,
                counted: 1,
                dropped: 0,
            }],
            winner: Some(1),
            margin: 1.5,
            near_tie: false,
        });
    }
    let mut names = DisplayNames::default();
    names
        .defaults
        .insert("codex".to_owned(), "gpt-5.6-luna".to_owned());
    let question = approval_question_local(1, &opened, None, UtcOffset::UTC, &names);
    let detail = question.detail.join("\n");
    assert!(
        detail.contains("Plan 1 by GPT 5.6 Luna on Codex scored best"),
        "{detail}"
    );
}

/// The merge waits on your untracked files: the conversation names them, the question asks only
/// what to do, and its record never says the run merged.
#[test]
fn files_in_the_way_wait_to_merge() {
    let mut waiting = merge();
    waiting.title = "Waiting to merge into main".into();
    waiting.free_text = FreeText::None;
    if let ApprovalPacket::Merge(packet) = &mut waiting.packet {
        packet.in_the_way = vec![
            "textkit/__pycache__/core.cpython-312.pyc".into(),
            "notes.md".into(),
        ];
    }
    let choose = |value: &str| AnswerAction::Choose {
        value: value.into(),
    };
    waiting.options = vec![
        option(
            'm',
            "Move them aside and merge",
            choose("Move them aside and merge"),
        ),
        option('t', "Try again", choose("Try again")),
        option('l', "Merge later", choose("Merge later")),
    ];

    let question = approval_question(1, &waiting, None);
    assert_eq!(
        question.detail,
        ["textkit/__pycache__/core.cpython-312.pyc and 1 other in the way"]
    );
    assert_eq!(
        question.question.as_deref(),
        Some("Move them aside and merge, or merge later?")
    );
    assert!(!question.allow_note, "a choice, not something to type");

    let mut screen = Screen::new();
    screen.event(opened_event(waiting));
    let about = screen.history().join("\n");
    assert!(about.contains("Waiting to merge into main"), "{about}");
    assert!(
        about.contains("2 untracked files in your checkout"),
        "{about}"
    );
    assert!(about.contains("notes.md"), "{about}");
    assert!(!about.contains("Suite passed"), "{about}");

    screen.event(approval(ApprovalEvent::Decided(decided(
        choose("Merge later"),
        Party::User,
    ))));
    let record = screen.history().last().cloned().unwrap_or_default();
    assert_eq!(
        record,
        "◆ Waiting to merge into main · Merge later · you, 10:58 AM"
    );
    assert!(!record.contains("Merged"));
}

/// Your edits to files the run changes, or git's own reason, wait the same way: the
/// conversation says which and why, and the question offers only trying again or merging later.
#[test]
fn edits_and_git_refusals_wait_to_merge_too() {
    let choose = |value: &str| AnswerAction::Choose {
        value: value.into(),
    };
    let waiting = |edited: Vec<String>, refused: Option<String>| {
        let mut waiting = merge();
        waiting.title = "Waiting to merge into main".into();
        waiting.free_text = FreeText::None;
        if let ApprovalPacket::Merge(packet) = &mut waiting.packet {
            packet.edited = edited;
            packet.refused = refused;
        }
        waiting.options = vec![
            option('t', "Try again", choose("Try again")),
            option('l', "Merge later", choose("Merge later")),
        ];
        waiting
    };

    let edited = waiting(vec!["README.md".into()], None);
    let question = approval_question(1, &edited, None);
    assert_eq!(question.detail, ["README.md is in the way"]);
    assert_eq!(
        question.question.as_deref(),
        Some("Try again, or merge later?")
    );
    let mut screen = Screen::new();
    screen.event(opened_event(edited));
    let about = screen.history().join("\n");
    assert!(
        about.contains("You have uncommitted edits to a file the run changes"),
        "{about}"
    );
    assert!(about.contains("README.md"), "{about}");

    let lock = "fatal: Unable to create '.git/index.lock': File exists.";
    let refused = waiting(Vec::new(), Some(lock.into()));
    let question = approval_question(1, &refused, None);
    assert_eq!(question.detail, [format!("Git said: {lock}")]);
    let mut screen = Screen::new();
    screen.event(opened_event(refused));
    let about = screen.history().join("\n");
    assert!(
        about.contains("Git wouldn't move your checkout of main forward:"),
        "{about}"
    );
    assert!(about.contains("index.lock"), "{about}");
}

fn check(verdict: Option<Verdict>, auto_preview: bool) -> AppEvent {
    approval(ApprovalEvent::Checked(PolicyCheck {
        id: ApprovalId::numbered(1),
        violations: Vec::new(),
        verdict,
        by: Some(DecideWith::Rules),
        judge: None,
        reasons: vec!["The plan goes over the limit".into()],
        auto_preview,
    }))
}

#[test]
fn a_question_your_rules_still_decide_takes_the_keys_once_it_needs_you() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    assert_eq!(
        screen.model.bottom.question, None,
        "it asks nothing while your rules decide, so it stays off the screen"
    );
    let out = screen.event(check(Some(Verdict::AskYou), true));
    assert!(
        !out.bell && screen.model.bottom.question.is_none(),
        "an auto preview check is only recorded"
    );
    // The engine's own shape for "ask the user": no verdict, and maybe no reason either.
    let out = screen.event(approval(ApprovalEvent::Checked(PolicyCheck {
        id: ApprovalId::numbered(1),
        violations: Vec::new(),
        verdict: None,
        by: None,
        judge: None,
        reasons: Vec::new(),
        auto_preview: false,
    })));
    assert!(out.bell);
    assert!(screen.question().needs_you && screen.question().focused);

    // Your rules leaving it to you says the same.
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    screen.event(check(Some(Verdict::AskYou), false));
    assert!(screen.question().needs_you && screen.question().focused);
}

#[test]
fn a_question_that_needs_you_goes_before_a_waiting_spot_check() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    screen.event(approval(ApprovalEvent::Decided(decided(
        AnswerAction::Merge,
        Party::Rules,
    ))));
    screen.event(approval(ApprovalEvent::SpotCheckQueued {
        id: ApprovalId::numbered(1),
    }));
    assert!(screen.question().title.starts_with("Spot check"));
    assert!(
        screen.question().focused,
        "a spot check asks, so it takes the keys"
    );

    let mut next = fallback();
    next.id = ApprovalId::numbered(2);
    screen.event(opened_event(next));
    assert_eq!(screen.question().title, "Fallback · task-003 Middleware");
    assert!(screen.question().focused);
    screen.keys(&[KeyCode::Esc]);
    assert!(screen.question().title.starts_with("Spot check"));
    assert!(
        screen.question().focused,
        "and it asks again once the fallback is passed on"
    );
}

/// A run's question takes the keys as it shows, even mid-draft: the draft goes aside, and comes
/// back once the question is passed on.
#[test]
fn a_question_that_shows_while_you_type_sets_the_draft_aside() {
    let mut screen = Screen::new();
    screen.model.bottom.composer.set_text("keep the CSV pa");
    screen.event(opened_event(merge()));
    assert!(screen.question().focused, "the question took the keys");
    assert!(screen.model.bottom.composer.is_empty());
    screen.keys(&[KeyCode::Esc]);
    assert!(screen.model.bottom.question.is_none());
    assert!(screen.model.bottom.settle_draft());
    assert_eq!(screen.model.bottom.composer.text(), "keep the CSV pa");
}

/// An answered question's record goes by its title, which names the asking agent's label, so
/// no task id shows around the question.
#[test]
fn an_answered_question_goes_by_its_label() {
    let question = opened(
        ApprovalKind::Question,
        "Question from build:fix-money",
        ApprovalPacket::Question(QuestionPacket {
            from: worker("claude-code/opus"),
            task: task_id(2),
            text: "Round half up?".into(),
            options: Vec::new(),
        }),
        vec![option(' ', "Type my own answer", AnswerAction::Answer)],
    );
    let mut answer = decided(AnswerAction::Answer, Party::User);
    answer.text = Some("Yes, half up.".into());
    let names = DisplayNames::default();
    assert_eq!(
        records::outcome(&question, &answer, None, &names, &records::by_id),
        "Question from build:fix-money answered · Yes, half up."
    );
    let chosen = decided(
        AnswerAction::Choose {
            value: "half up".into(),
        },
        Party::User,
    );
    assert_eq!(
        records::outcome(&question, &chosen, None, &names, &records::by_id),
        "Question from build:fix-money answered · half up"
    );
}

/// The goal's own-answer option opens a line inside the question, and only that answer goes out.
#[test]
fn a_goal_question_takes_your_own_answer_inside_the_question() {
    let mut screen = Screen::new();
    let mut question = opened(
        ApprovalKind::Question,
        "Keep a CSV fallback?",
        ApprovalPacket::Question(QuestionPacket {
            from: worker("claude-code/opus"),
            task: task_id(2),
            text: "Keep a CSV fallback?".into(),
            options: vec!["Yes, keep it".into(), "No, drop it".into()],
        }),
        vec![
            option(
                'y',
                "Yes, keep it",
                AnswerAction::Choose {
                    value: "yes".into(),
                },
            ),
            option(
                'n',
                "No, drop it",
                AnswerAction::Choose { value: "no".into() },
            ),
            option(' ', "Type my own answer", AnswerAction::Answer),
        ],
    );
    question.free_text = FreeText::Answer;
    screen.event(opened_event(question));
    assert!(screen.question().focused);
    let own = screen.question().options[2].clone();
    assert_eq!(own.kind, OptionKind::FreeText);
    screen.keys(&[KeyCode::Char('3')]);
    assert!(screen.question().has_open_note());
    let typed: Vec<KeyCode> = "only behind a flag".chars().map(KeyCode::Char).collect();
    screen.keys(&typed);
    assert!(
        screen.model.bottom.composer.is_empty(),
        "nothing reaches the input"
    );
    let effects = screen.keys(&[KeyCode::Enter]);
    let UiCommand::Answer { action, text, .. } = &effects[0].commands[0] else {
        panic!("{:?}", effects[0].commands);
    };
    assert_eq!(*action, AnswerAction::Answer);
    assert_eq!(text.as_deref(), Some("only behind a flag"));
}

/// Every kind of choice, each naming the one after it, so a new kind can't build until it's
/// walked here too.
fn next_choice_kind(kind: Option<ChoiceKind>) -> Option<ChoiceKind> {
    use ChoiceKind::*;
    match kind {
        None => Some(StartWork),
        Some(StartWork) => Some(ConfirmTakeOver),
        Some(ConfirmTakeOver) => Some(ConfirmSafetySetting),
        Some(ConfirmSafetySetting) => Some(TermsNote),
        Some(TermsNote) => Some(Init),
        Some(Init) => Some(SettingScope),
        Some(SettingScope) => Some(DetectedChecks),
        Some(DetectedChecks) => Some(TrustProject),
        Some(TrustProject) => Some(ConfirmRewind),
        Some(ConfirmRewind) => Some(ConfirmConnect),
        Some(ConfirmConnect) => Some(KeepInstructions),
        Some(KeepInstructions) => Some(Permission),
        Some(Permission) => Some(RefusedCommands),
        Some(RefusedCommands) => Some(RewindTurn),
        Some(RewindTurn) => Some(GoAhead),
        Some(GoAhead) => Some(Fallback),
        Some(Fallback) => Some(TryWorkflow),
        Some(TryWorkflow) => Some(Setup),
        Some(Setup) => Some(Git),
        Some(Git) => Some(Templates),
        Some(Templates) => Some(NeededUpdate),
        Some(NeededUpdate) => Some(RenamedSettings),
        Some(RenamedSettings) => None,
    }
}

/// Every kind of approval, walked the same way.
fn next_approval_kind(kind: Option<ApprovalKind>) -> Option<ApprovalKind> {
    use ApprovalKind::*;
    match kind {
        None => Some(Understand),
        Some(Understand) => Some(Plan),
        Some(Plan) => Some(Merge),
        Some(Merge) => Some(Fallback),
        Some(Fallback) => Some(AgentDown),
        Some(AgentDown) => Some(Stuck),
        Some(Stuck) => Some(Limit),
        Some(Limit) => Some(ChangeRequest),
        Some(ChangeRequest) => Some(GoalChange),
        Some(GoalChange) => Some(WorkflowPromotion),
        Some(WorkflowPromotion) => Some(TakeOverReturn),
        Some(TakeOverReturn) => Some(Question),
        Some(Question) => Some(OriginalReturned),
        Some(OriginalReturned) => None,
    }
}

/// The bottom of the screen with `question` up and an input nobody touched, after a frame.
fn bottom_with(question: QuestionPrompt) -> (crate::bottom::BottomState, Vec<String>) {
    let mut bottom = crate::bottom::BottomState::new();
    bottom.question = Some(question);
    let now = Instant::now();
    bottom.hand_keys_to_question(now);
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let view = bottom.render(&RenderCtx::new(100, &theme, Clock::still(now)));
    let rows = plain_text(&view.lines).lines().map(str::to_owned).collect();
    (bottom, rows)
}

/// As a rule rather than a list: every question that asks you something takes the keys
/// once the input is free, and the input box goes while it holds them, whatever its kind. A new
/// kind of choice or approval fails here until it does.
#[test]
fn every_question_that_asks_takes_the_keys_and_hides_the_input() {
    let mut questions = Vec::new();
    let mut kind = next_choice_kind(None);
    while let Some(each) = kind {
        let options = vec![
            option('y', "Yes", AnswerAction::Approve),
            option('n', "No", AnswerAction::Cancel),
        ];
        questions.push(choice::choice_question(1, &choice(each, options)));
        kind = next_choice_kind(Some(each));
    }
    let mut kind = next_approval_kind(None);
    while let Some(each) = kind {
        let mut opened = merge();
        opened.kind = each;
        questions.push(approval::approval_question(1, &opened, None));
        kind = next_approval_kind(Some(each));
    }
    let names = DisplayNames::default();
    questions.push(spot::spot_check_question(1, &merge(), None, None, &names));

    for question in questions {
        let title = question.title.clone();
        assert!(question.asks(), "{title} asks you, so it says so");
        let (bottom, rows) = bottom_with(question);
        assert!(
            bottom.focused_question().is_some(),
            "{title} takes the keys"
        );
        assert!(
            !rows.iter().any(|row| row.starts_with('❯')),
            "{title} stands in for the input:\n{}",
            rows.join("\n")
        );
    }
}

/// A go-ahead in autopilot, as the council's was: while autopilot decides, "Run this
/// workflow?" asks nothing and stays off the screen. Once its judge can't check it and full
/// auto leaves it to you, it asks and holds the keys with the input hidden, never waiting for ↑
/// under a ruled input.
#[test]
fn a_go_ahead_autopilot_leaves_to_you_holds_the_keys() {
    let mut screen = Screen::new();
    let mut go_ahead = merge();
    go_ahead.kind = ApprovalKind::Understand;
    go_ahead.title = "Run this workflow?".into();
    go_ahead.mode = ApprovalMode::Autopilot;
    go_ahead.decide_with = DecideWith::RulesAndModel;
    go_ahead.needs_you = false;
    go_ahead.options = vec![
        option('y', "Yes, run it", AnswerAction::Approve),
        option('x', "No, stop here", AnswerAction::Abandon),
    ];
    screen.event(opened_event(go_ahead));
    assert_eq!(
        screen.model.bottom.question, None,
        "a task autopilot still decides isn't on screen"
    );
    screen.event(approval(ApprovalEvent::Checked(PolicyCheck {
        id: ApprovalId::numbered(1),
        violations: Vec::new(),
        verdict: None,
        by: None,
        judge: None,
        reasons: vec!["the judge's session ended early: auth".into()],
        auto_preview: false,
    })));
    let question = screen.question();
    assert_eq!(question.title, "Run this workflow?");
    assert!(question.asks());
    assert!(question.focused, "it took the keys as it showed");
    let (bottom, rows) = bottom_with(question.clone());
    assert!(bottom.focused_question().is_some(), "{}", rows.join("\n"));
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("to answer the question") || row.starts_with('❯')),
        "{}",
        rows.join("\n")
    );
}

/// The setup question as it was approved: the settings in plain text with only the notes dim, the
/// four places to keep it listed, and `d` for what would be written.
#[test]
fn a_setup_question_shows_its_settings_plain_its_notes_dim_and_four_choices() {
    let mut screen = Screen::new();
    let place = |key, label: &str, value: &str| {
        option(
            key,
            label,
            AnswerAction::Choose {
                value: value.into(),
            },
        )
    };
    let setup = AppChoice {
        id: ChoiceId::numbered(9),
        kind: ChoiceKind::Setup,
        title: "Your setup".into(),
        body: vec![
            "limits        spend_per_day = 5.00   \twhen reached: fallback".into(),
            "reviewers     \tadds to\t roles/reviewer.md".into(),
            String::new(),
            "You asked: never more than $5 a day.".into(),
        ],
        options: vec![
            place('c', "This conversation", "conversation"),
            place('p', "Save for this repository", "repository"),
            place('e', "Save for all my repositories", "everywhere"),
            option('n', "Not now", AnswerAction::Cancel),
            option('d', "Show what would be written", AnswerAction::ShowDiff),
        ],
        free_text: FreeText::None,
        bell: false,
    };
    screen.event(AppEvent::Choice { choice: setup });
    let question = screen.question();
    let labels: Vec<&str> = question.options.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "This conversation",
            "Save for this repository",
            "Save for all my repositories",
            "Not now"
        ]
    );

    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
    let lines = question.lines(&ctx);
    let dim = theme.style(crate::theme::Role::Dim);
    let style_of = |text: &str| {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .find(|span| span.content.contains(text))
            .map(|span| span.style)
    };
    assert_eq!(style_of("when reached: fallback"), Some(dim));
    assert_eq!(style_of("adds to"), Some(dim));
    assert_ne!(style_of("spend_per_day = 5.00"), Some(dim));
    assert_ne!(style_of("roles/reviewer.md"), Some(dim));
    let shown = plain_text(&lines);
    assert!(
        shown.contains("limits        spend_per_day = 5.00   when reached: fallback"),
        "{shown}"
    );
    assert!(!shown.contains("You asked"), "{shown}");

    let effects = screen.keys(&[KeyCode::Char('d')]);
    assert_eq!(effects.len(), 1, "d opens what would be written");
    assert!(!effects[0].views.is_empty(), "a view opens");
    assert!(screen.model.bottom.question.is_some(), "the question stays");
}

/// A preview runs only on y. Enter and Esc leave it as it is, and n says no.
#[test]
fn a_preview_runs_only_on_its_own_key() {
    let answers = |keys: &[KeyCode]| {
        let mut screen = Screen::new();
        screen.event(AppEvent::Choice { choice: preview() });
        let effects = screen.keys(keys);
        effects
            .into_iter()
            .flat_map(|out| out.commands)
            .filter_map(|command| match command {
                UiCommand::Answer { action, .. } => Some(action),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // Enter and Esc answer nothing; only y runs it.
    for key in [KeyCode::Enter, KeyCode::Esc] {
        assert_eq!(answers(&[key]), []);
    }
    assert_eq!(answers(&[KeyCode::Char('y')]), [AnswerAction::Approve]);
    assert_eq!(
        answers(&[KeyCode::Char('n')]),
        [AnswerAction::Choose { value: "no".into() }]
    );
}

#[test]
fn abandoning_at_the_merge_says_what_goes_before_you_confirm() {
    let mut question = approval_question(1, &merge(), None);
    question.focus();
    question.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
    assert_eq!(question.armed, Some(4));
    let theme = Theme::dark();
    let ctx = RenderCtx::new(100, &theme, Clock::still(Instant::now()));
    let rows = question.lines(&ctx);
    let text = plain_text(&rows);
    let last = text.lines().last().unwrap_or_default();
    assert_eq!(
        last,
        "  Enter to abandon the run, its work is removed · Esc to cancel"
    );
}

#[test]
fn a_merge_asked_again_says_why_under_its_title() {
    let mut opened = merge();
    let ApprovalPacket::Merge(packet) = &mut opened.packet else {
        panic!("a merge question");
    };
    packet.again = Some(
        "pushing argonauts/run-01/merged to origin failed, so no pull request was opened: \
         remote rejected"
            .to_owned(),
    );
    let question = approval_question(1, &opened, None);
    assert_eq!(
        question.detail[1],
        "Asked again: pushing argonauts/run-01/merged to origin failed, so no pull request \
         was opened: remote rejected"
    );
    // The first time round nothing is said.
    assert_eq!(approval_question(1, &merge(), None).detail.len(), 1);
}

#[test]
fn asking_for_a_pull_request_is_recorded_as_asked() {
    let names = DisplayNames::default();
    let asked = decided(AnswerAction::PullRequest, Party::User);
    assert_eq!(
        records::outcome(&merge(), &asked, None, &names, &records::by_id),
        "Asked for a pull request for the run"
    );
}
