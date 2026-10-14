use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::DiffScope;
use argonauts_core::app::{AppChoice, ChoiceKind, ViewRequest};
use argonauts_core::events::card::CardKind;
use argonauts_core::events::checkpoint::{
    CheckpointKind, CheckpointPacket, EscalationPacket, FreeText, MergePacket, MergeRow,
    QuestionPacket, VendorDownPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::limits::{Candidate, CostEstimate, TrackRecord};
use argonauts_core::names::{ChoiceId, VendorId, WorkerRef};
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::handoff::{HandoffMode, HandoffStrategy};
use argonauts_core::settings::ui::UiSettings;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{DiffStat, Spend};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use time::UtcOffset;
use time::macros::datetime;

use super::*;
use crate::bottom::{CardEvent, OptionKind};
use crate::bridge::Effects;
use crate::cell::HistoryCell;
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

struct Screen {
    cards: Cards,
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
}

impl Screen {
    fn new() -> Self {
        Self {
            cards: Cards::default(),
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
        self.cards.on_event(&event, &mut cx);
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
        let taken = self.cards.on_action(&action, &mut cx);
        (taken, out)
    }

    fn card(&self) -> &CardPrompt {
        self.model.bottom.card.as_ref().expect("a task on screen")
    }

    /// Presses keys on the card as the app does, and hands what they report to the cards.
    fn keys(&mut self, codes: &[KeyCode]) -> Vec<Effects> {
        let mut effects = Vec::new();
        for &code in codes {
            let card = self.model.bottom.card.as_mut().expect("a task on screen");
            card.focused = true;
            let id = card.id;
            let action = match card.handle_key(KeyEvent::new(code, KeyModifiers::NONE)) {
                CardEvent::Answered {
                    index,
                    note,
                    checked,
                } => {
                    self.model.bottom.card = None;
                    Some(UiAction::Answer {
                        card: id,
                        option: index,
                        note,
                        checked,
                    })
                }
                CardEvent::OpenView { index } => Some(UiAction::OpenView {
                    card: id,
                    option: index,
                }),
                CardEvent::Dismissed => Some(UiAction::Dismiss { card: id }),
                CardEvent::BackToInput | CardEvent::Moved | CardEvent::Unhandled => None,
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

fn card_id(n: u32) -> CardId {
    CardId::new(format!("task-{n:03}")).unwrap()
}

fn worker(text: &str) -> WorkerRef {
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
    kind: CheckpointKind,
    title: &str,
    packet: CheckpointPacket,
    options: Vec<AnswerOption>,
) -> CheckpointOpened {
    CheckpointOpened {
        id: CheckpointId::numbered(1),
        kind,
        title: title.into(),
        card: None,
        packet,
        evidence_sha256: HASH.into(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

/// The number the card shows beside the option the event gives `key`.
fn number(opened: &CheckpointOpened, key: char) -> KeyCode {
    let index = opened
        .options
        .iter()
        .position(|option| option.key == key)
        .unwrap_or_else(|| panic!("an option `{key}` on {}", opened.title));
    KeyCode::Char(char::from_digit(index as u32 + 1, 10).expect("one digit"))
}

fn merge() -> CheckpointOpened {
    let row = |n: u32, title: &str| MergeRow {
        card: card_id(n),
        title: title.into(),
        kind: CardKind::Build,
        stat: stat(2, 40, 6),
        owner: Some(worker("codex/gpt-5.6")),
        gates_passed: true,
        review: Some(ReviewVerdict::Approve),
        debt: Vec::new(),
    };
    let packet = MergePacket {
        target: Some("main".into()),
        branch: "argonauts/run-01/integration".into(),
        rows: vec![
            row(2, "Add the JSON writer"),
            row(3, "Wire --json into the CLI"),
        ],
        suite_passed_on_latest_main: true,
        gates: Vec::new(),
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
    };
    opened(
        CheckpointKind::Merge,
        "Merge this run into main?",
        CheckpointPacket::Merge(packet),
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

fn handoff() -> CheckpointOpened {
    let candidate = |name: &str| Candidate {
        worker: worker(name),
        quota_left: Some(0.62),
        quota_text: "62%".into(),
        track_record: Some(TrackRecord { passed: 9, of: 11 }),
        cost: CostEstimate::Quota(0.06),
        needs_confirm: false,
        may_run_out: false,
    };
    let plan = HandoffPlan {
        card: card_id(3),
        title: "Middleware".into(),
        stopped: worker("claude-code/opus"),
        reason: "usage limit, resets in 2h 10m".into(),
        resets_at: None,
        work: stat(4, 96, 12),
        gates_ran: false,
        candidates: vec![candidate("codex/gpt-5.6")],
        mode: HandoffMode::Ask,
        strategy: HandoffStrategy::First,
        race_count: 1,
        also_moving: Vec::new(),
    };
    let mut opened = opened(
        CheckpointKind::Handoff,
        "Fallback · task-003 Middleware",
        CheckpointPacket::Handoff(plan),
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
    opened.card = Some(card_id(3));
    opened
}

fn escalation() -> CheckpointOpened {
    opened(
        CheckpointKind::Escalation,
        "task-004 is stuck",
        CheckpointPacket::Escalation(EscalationPacket {
            card: card_id(4),
            title: "Parser".into(),
            state: argonauts_core::events::card::CardState::NeedsDecision,
            attempts: vec!["tests failed".into()],
            last_failure: "tests failed".into(),
        }),
        vec![
            option('r', "Retry with a note", AnswerAction::Retry),
            option('x', "Cancel the task", AnswerAction::Cancel),
            option('d', "Show the details", AnswerAction::ShowDiff),
        ],
    )
}

fn vendor_down() -> CheckpointOpened {
    opened(
        CheckpointKind::VendorDown,
        "Claude Code is down",
        CheckpointPacket::VendorDown(VendorDownPacket {
            vendor: VendorId("claude-code".into()),
            evidence: "overload errors on 3 sessions in 2 min".into(),
            test_at: Some(datetime!(2026-09-30 14:40 UTC)),
            paused: Vec::new(),
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

fn checkpoint(event: CheckpointEvent) -> AppEvent {
    AppEvent::Run {
        event: Box::new(RunEvent {
            seq: 1,
            ts: datetime!(2026-09-30 10:58 UTC),
            run_id: run(),
            card_id: None,
            session: None,
            source: "conductor".into(),
            kind: RunEventKind::Checkpoint(event),
        }),
    }
}

fn opened_event(opened: CheckpointOpened) -> AppEvent {
    checkpoint(CheckpointEvent::Opened(Box::new(opened)))
}

fn decided(action: AnswerAction, by: Party) -> CheckpointDecided {
    CheckpointDecided {
        id: CheckpointId::numbered(1),
        action,
        text: None,
        by,
        evidence_sha256: HASH.into(),
        shadow: None,
        cards: Vec::new(),
    }
}

#[test]
fn answer_carries_rendered_evidence_hash() {
    let mut screen = Screen::new();
    let out = screen.event(opened_event(merge()));
    assert!(out.bell, "a task that needs you rings once");
    assert!(screen.card().focused, "and takes the keys as it shows");

    let id = screen.card().id;
    let (taken, out) = screen.action(UiAction::Answer {
        card: id,
        option: 1,
        note: Some("ship it".into()),
        checked: Vec::new(),
    });
    assert!(taken);
    assert_eq!(
        out.commands,
        vec![UiCommand::Answer {
            to: AnswerTarget::Checkpoint {
                run: run(),
                id: CheckpointId::numbered(1),
            },
            action: AnswerAction::PullRequest,
            text: Some("ship it".into()),
            evidence_sha256: Some(HASH.into()),
            cards: Vec::new(),
            remember: false,
        }]
    );
    assert!(screen.cards.open.is_empty());
    assert!(screen.model.bottom.card.is_none());
}

#[test]
fn request_changes_sends_picked_cards() {
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
            cards,
            text,
            ..
        },
    ] = commands.as_slice()
    else {
        panic!("one answer, got {commands:?}");
    };
    assert_eq!(*action, AnswerAction::RequestChanges);
    assert_eq!(*cards, vec![card_id(3)]);
    assert_eq!(text.as_deref(), Some("split the writer"));
}

#[test]
fn retry_opens_the_note_before_it_answers() {
    let mut screen = Screen::new();
    screen.event(opened_event(escalation()));
    let mut keys = vec![number(&escalation(), 'r')];
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
    // One a change in a view raised, and one work raised: a view on top would keep the keys.
    for kind in [
        ChoiceKind::SettingScope,
        ChoiceKind::ConfirmSafetySetting,
        ChoiceKind::StartWork,
    ] {
        let mut screen = Screen::new();
        let out = screen.event(AppEvent::Choice {
            choice: choice(kind, Vec::new()),
        });
        assert!(out.close_views, "{kind:?} would wait behind the view");
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

/// A dry run's card is its plan, so a workflow of many steps shows them all.
#[test]
fn a_dry_runs_card_shows_its_whole_plan_and_answers_only_to_its_keys() {
    let mut tried = preview();
    tried.title = "spec-first for \"add due dates\" · a preview, nothing runs yet".into();
    tried.body = (1..=12).map(|step| format!("{step} Step {step}")).collect();
    let card = choice_card(1, &tried);
    assert_eq!(card.body.len(), 12, "{:?}", card.body);
    assert!(card.detail.is_empty());
    assert_eq!(card.esc_hint, "y runs it · n closes it");
    assert!(card.focused && card.esc_answers && card.keys_only);

    // A y/N question answers to its letters alone: Enter and Esc do nothing, whichever row
    // is lit.
    for key in [
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyCode::Esc,
    ] {
        let mut card = choice_card(1, &tried);
        card.selected = 0;
        let answered = card.handle_key(crossterm::event::KeyEvent::from(key));
        assert!(
            matches!(answered, CardEvent::Moved),
            "{key:?}: {answered:?}"
        );
    }
    for (key, index) in [('y', 0), ('n', 1)] {
        let mut card = choice_card(1, &tried);
        let answered = card.handle_key(crossterm::event::KeyEvent::from(
            crossterm::event::KeyCode::Char(key),
        ));
        assert!(
            matches!(answered, CardEvent::Answered { index: i, .. } if i == index),
            "{key}: {answered:?}"
        );
    }

    // It asks on one line, the default's letter a bold capital.
    let mut asked = preview();
    asked.title = "Run it now?".into();
    asked.options[1].recommended = true;
    let card = choice_card(1, &asked);
    let theme = crate::theme::Theme::dark();
    let lines = card.lines(&crate::render::RenderCtx::new(
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

    // Any other card keeps to a few rows.
    let mut long = choice(ChoiceKind::ConfirmConnect, Vec::new());
    long.body = tried.body.clone();
    let shortened = choice_card(2, &long);
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
    assert!(screen.card().focused && screen.card().takes_focus);
    for kind in [
        ChoiceKind::ConfirmSafetySetting,
        ChoiceKind::TermsNote,
        ChoiceKind::Init,
        ChoiceKind::ConfirmRevert,
        ChoiceKind::ConfirmConnect,
        ChoiceKind::TrustProject,
        ChoiceKind::DetectedGates,
        ChoiceKind::TryWorkflow,
    ] {
        let card = choice_card(1, &choice(kind, Vec::new()));
        assert!(card.focused && card.esc_answers, "{kind:?}");
    }
    // The unclear request holds your request, so it takes the keys like a run's card that
    // needs you, and Esc sets it aside instead of answering.
    let unclear = choice_card(2, &choice(ChoiceKind::StartWork, Vec::new()));
    assert!(unclear.needs_you && !unclear.takes_focus && !unclear.esc_answers);

    // A number answers it, never a letter, and after letters only once a key meant for the
    // card comes first.
    let card = screen.model.bottom.card.as_mut().unwrap();
    for c in ['s', 'm', 'p', 'e', '/', '4'] {
        assert_eq!(
            card.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
            CardEvent::Unhandled
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
            cards: Vec::new(),
            remember: false,
        }]
    );
}

#[test]
fn a_choice_you_set_off_goes_before_a_waiting_run_card() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let run_card = screen.card().id;
    assert!(
        screen.card().focused,
        "a run's task that needs you takes the keys"
    );
    screen.event(AppEvent::Choice {
        choice: scope_choice(),
    });
    assert_ne!(screen.card().id, run_card);
    assert!(
        screen.card().focused,
        "the scope question takes the keys at once"
    );

    screen.keys(&[KeyCode::Char('1')]);
    screen.event(AppEvent::ChoiceClosed {
        id: ChoiceId::numbered(7),
        outcome: "Saved.".into(),
    });
    assert_eq!(screen.card().id, run_card, "the run's task comes back");
    assert!(
        screen.card().focused,
        "and takes the keys again, since you never handed it on"
    );

    // Handed on, it stays off the screen after the next question.
    screen.keys(&[KeyCode::Esc]);
    screen.event(AppEvent::Choice {
        choice: scope_choice(),
    });
    screen.keys(&[KeyCode::Char('1')]);
    screen.event(AppEvent::ChoiceClosed {
        id: ChoiceId::numbered(7),
        outcome: "Saved.".into(),
    });
    assert!(screen.model.bottom.card.is_none());
    assert!(screen.cards.open.contains_key(&run_card));
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
    assert!(screen.model.bottom.card.is_some(), "the question stays");
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
    assert_eq!(screen.card().esc_hint, "Esc to cancel");
    // Esc answers it, so only Esc on the focused card does. An unclear request just waits.
    assert!(screen.card().esc_answers);
    let unclear = choice_card(2, &choice(ChoiceKind::StartWork, Vec::new()));
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
    assert!(screen.cards.open.is_empty());
    assert!(screen.model.bottom.card.is_none());
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
fn esc_hands_a_run_card_on() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let effects = screen.keys(&[KeyCode::Esc]);
    let sent: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    assert!(
        matches!(
            sent.as_slice(),
            [UiCommand::SetAside {
                to: AnswerTarget::Checkpoint { .. }
            }]
        ),
        "the engine hears it, so whoever asked decides: {sent:?}"
    );
    assert!(screen.model.bottom.card.is_none(), "no line stays behind");
    let record = screen.history().last().cloned().unwrap_or_default();
    assert!(
        record.starts_with("◆ Merge approval left to the main agent · you, "),
        "{record}"
    );

    // Until the engine settles it, the list of what needs you still opens it, as it was.
    assert_eq!(screen.cards.open.len(), 1);
    let (taken, _) = screen.action(UiAction::AnswerCard(AnswerTarget::Checkpoint {
        run: run(),
        id: merge().id,
    }));
    assert!(taken);
    assert!(screen.card().focused && !screen.card().dismissed);
}

/// What Esc left reads as left, by you, and never quotes what the agent was told.
#[test]
fn a_question_you_left_is_recorded_as_left_to_the_agent() {
    use argonauts_core::events::checkpoint::LEFT_TO_THE_AGENT;

    let mut left = decided(AnswerAction::Answer, Party::Declined);
    left.text = Some(LEFT_TO_THE_AGENT.to_owned());
    let mut opened = merge();
    opened.kind = CheckpointKind::Question;
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
        let cell = records::handed_on_cell(kind, at, UtcOffset::UTC);
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
        record(CheckpointKind::Question),
        "Question left to the agent · you, 8:47 PM"
    );
    assert_eq!(
        record(CheckpointKind::Plan),
        "Plan approval left to the main agent · you, 8:47 PM"
    );
}

#[test]
fn handoff_remember_toggle_sets_remember() {
    let mut screen = Screen::new();
    screen.event(opened_event(handoff()));
    let toggle = screen.card().options.last().unwrap().clone();
    assert_eq!(toggle.kind, OptionKind::Toggle);
    assert_eq!(toggle.label, REMEMBER_LABEL);

    let id = screen.card().id;
    let number = char::from_digit(screen.card().options.len() as u32, 10).unwrap();
    screen.keys(&[KeyCode::Char('m')]);
    assert!(!screen.cards.open[&id].remember, "a letter does nothing");
    screen.keys(&[KeyCode::Char(number)]);
    assert!(
        !screen.cards.open[&id].remember,
        "nor the number after it, until a key meant for the task"
    );
    screen.keys(&[KeyCode::Down, KeyCode::Char(number)]);
    assert!(screen.cards.open[&id].remember);
    assert!(screen.card().options.last().unwrap().on);
    screen.keys(&[KeyCode::Char(number)]);
    assert!(
        !screen.cards.open[&id].remember,
        "a second press turns it off"
    );
    screen.keys(&[KeyCode::Char(number)]);

    let candidate = handoff()
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
    screen.event(opened_event(handoff()));
    screen.keys(&[number(&handoff(), 'a')]);
    let pick = screen
        .model
        .bottom
        .answered_pick
        .clone()
        .expect("the pick is offered");
    assert_eq!(
        (&pick.run, &pick.id, pick.selected),
        (&run(), &CheckpointId::numbered(1), false)
    );
    assert!(
        pick.outcome.starts_with("task-003 handed to "),
        "{}",
        pick.outcome
    );

    // Answering anything else lets it go.
    screen.event(opened_event(merge()));
    screen.keys(&[number(&merge(), 'p')]);
    assert_eq!(screen.model.bottom.answered_pick, None);
}

#[test]
fn handoff_remember_stays_off_for_other_answers() {
    let mut screen = Screen::new();
    screen.event(opened_event(handoff()));
    let remember = KeyCode::Char(char::from_digit(screen.card().options.len() as u32, 10).unwrap());
    let effects = screen.keys(&[remember, number(&handoff(), 'w')]);
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
    screen.event(checkpoint(CheckpointEvent::Decided(decided(
        AnswerAction::Merge,
        Party::Rules,
    ))));
    let out = screen.event(checkpoint(CheckpointEvent::SpotCheckQueued {
        id: CheckpointId::numbered(1),
    }));
    assert!(!out.bell, "a spot check never rings");
    let card = screen.card();
    assert_eq!(card.title, "Spot check · Merge this run into main?");
    let labels: Vec<&str> = card
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
            id: CheckpointId::numbered(1),
            correct: false,
        }]
    );
}

/// A spot check takes the keys like any question, so Esc has to give them back: it closes
/// the card and answers nothing, and the check still waits in `/approvals`.
#[test]
fn esc_closes_a_spot_check_and_answers_nothing() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    screen.event(checkpoint(CheckpointEvent::Decided(decided(
        AnswerAction::Merge,
        Party::Rules,
    ))));
    screen.event(checkpoint(CheckpointEvent::SpotCheckQueued {
        id: CheckpointId::numbered(1),
    }));
    assert_eq!(
        screen.card().title,
        "Spot check · Merge this run into main?"
    );

    let effects = screen.keys(&[KeyCode::Esc]);
    let sent: Vec<&UiCommand> = effects.iter().flat_map(|out| &out.commands).collect();
    assert!(sent.is_empty(), "nothing is decided for you: {sent:?}");
    assert!(
        screen.model.bottom.card.is_none(),
        "the keys are yours again"
    );
    assert!(screen.cards.open.is_empty());
}

#[test]
fn a_card_auto_leaves_to_you_rings_and_says_why_once() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    let out = screen.event(opened_event(auto));
    assert!(!out.bell, "nothing rings while auto decides");
    let check = |shadow: bool, reasons: &[&str]| {
        checkpoint(CheckpointEvent::Checked(
            argonauts_core::events::checkpoint::PolicyCheck {
                id: CheckpointId::numbered(1),
                violations: Vec::new(),
                verdict: shadow.then_some(argonauts_core::policy::Verdict::Escalate),
                by: None,
                approver: None,
                reasons: reasons.iter().map(|reason| (*reason).to_owned()).collect(),
                shadow,
            },
        ))
    };
    let out = screen.event(check(false, &["a hard limit, the checks didn't run"]));
    assert!(out.bell, "a task auto couldn't settle rings");
    // The shadow record that follows carries every reason, which Ctrl+O shows instead.
    screen.event(check(true, &["the checks didn't run", "no review result"]));
    screen.event(check(false, &["a hard limit, the checks didn't run"]));
    let why: Vec<&String> = screen
        .card()
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
    let id = CheckpointId::numbered(1);
    for event in [
        opened_event(auto),
        checkpoint(CheckpointEvent::Decided(decided(
            AnswerAction::Merge,
            Party::Rules,
        ))),
        checkpoint(CheckpointEvent::SpotCheckQueued { id: id.clone() }),
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
    for opened in [merge(), handoff(), escalation()] {
        let card = checkpoint_card(1, &opened, None);
        let drawn: Vec<&str> = card
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

    let kinds = |opened: &CheckpointOpened| -> Vec<OptionKind> {
        checkpoint_card(1, opened, None)
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
    assert_eq!(kinds(&escalation()), vec![Answer, Irreversible, View]);

    // An app choice's Cancel is only Not now or Keep it as it is.
    let trust = choice(
        ChoiceKind::TrustProject,
        vec![
            option('y', "Trust them", AnswerAction::Approve),
            option('n', "Not now", AnswerAction::Cancel),
            option('d', "Show the file", AnswerAction::ShowDiff),
        ],
    );
    let card = choice_card(1, &trust);
    let kinds: Vec<OptionKind> = card.options.iter().map(|option| option.kind).collect();
    assert_eq!(kinds, vec![Answer, Answer, View]);
}

#[test]
fn record_line_after_decided() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let about = screen.history();
    assert_eq!(about.len(), 1, "the merge table goes in first");
    assert!(about[0].contains("task-002") && about[0].contains("Suite passed"));

    screen.event(checkpoint(CheckpointEvent::Decided(decided(
        AnswerAction::Merge,
        Party::User,
    ))));
    assert!(screen.cards.open.is_empty());
    assert!(screen.model.bottom.card.is_none());
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
    screen.event(checkpoint(CheckpointEvent::Decided(decided(
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
fn card_times_are_in_the_terminals_zone() {
    let mut screen = Screen::new();
    screen.state.terminal.utc_offset = Some(UtcOffset::from_hms(-7, 0, 0).unwrap());
    screen.event(opened_event(vendor_down()));
    let about = screen.history();
    assert!(
        about
            .iter()
            .any(|line| line.contains("tries it again at 7:40 AM")),
        "{about:?}"
    );
    assert!(
        screen
            .card()
            .detail
            .iter()
            .any(|line| line.ends_with("test task at 7:40 AM")),
        "{:?}",
        screen.card().detail
    );

    screen.event(checkpoint(CheckpointEvent::Decided(decided(
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
fn a_withdrawn_card_goes_with_a_dim_line() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    screen.event(checkpoint(CheckpointEvent::Withdrawn {
        id: CheckpointId::numbered(1),
        reason: "the evidence changed".into(),
    }));
    assert!(screen.model.bottom.card.is_none());
    assert_eq!(
        screen.history().last().map(String::as_str),
        Some("◆ Merge this run into main? · no longer needed: the evidence changed")
    );
}

#[test]
fn several_cards_show_the_oldest_and_answer_card_focuses_one() {
    let mut screen = Screen::new();
    screen.event(opened_event(merge()));
    let mut second = handoff();
    second.id = CheckpointId::numbered(2);
    screen.event(opened_event(second));
    assert_eq!(screen.card().title, "Merge this run into main?");
    assert_eq!(screen.cards.open.len(), 2);

    // Handing the oldest on brings the next one up.
    screen.keys(&[KeyCode::Esc]);
    assert_eq!(screen.card().title, "Fallback · task-003 Middleware");
    assert!(!screen.card().dismissed);

    let (taken, out) = screen.action(UiAction::AnswerCard(AnswerTarget::Checkpoint {
        run: run(),
        id: CheckpointId::numbered(1),
    }));
    assert!(taken && out.close_views);
    assert_eq!(screen.card().title, "Merge this run into main?");
    assert!(screen.card().focused && !screen.card().dismissed);
}

#[test]
fn take_over_confirm_sends_take_over() {
    let mut screen = Screen::new();
    let (taken, out) = screen.action(UiAction::ConfirmTakeOver {
        run: run(),
        card: card_id(2),
    });
    assert!(taken && out.close_views);
    assert!(
        screen.card().focused,
        "you asked for it, so it has the keys"
    );
    assert_eq!(screen.card().esc_hint, "Esc to cancel");
    assert!(screen.card().esc_answers);
    let effects = screen.keys(&[KeyCode::Char('1')]);
    assert_eq!(
        effects[0].commands,
        vec![UiCommand::TakeOver {
            run: run(),
            card: card_id(2),
        }]
    );

    // Esc cancels with nothing sent.
    screen.action(UiAction::ConfirmTakeOver {
        run: run(),
        card: card_id(2),
    });
    let effects = screen.keys(&[KeyCode::Esc]);
    assert!(effects.iter().all(|out| out.commands.is_empty()));
    assert!(screen.cards.open.is_empty() && screen.model.bottom.card.is_none());
}

#[test]
fn show_the_diff_asks_for_the_card_diff_and_keeps_the_card() {
    let mut screen = Screen::new();
    screen.event(opened_event(handoff()));
    let effects = screen.keys(&[number(&handoff(), 'd')]);
    assert_eq!(
        effects[0].commands,
        vec![UiCommand::Request {
            view: ViewRequest::Diff {
                run: run(),
                card: card_id(3),
                scope: DiffScope::Card,
            },
        }]
    );
    assert_eq!(screen.cards.open.len(), 1);
    assert!(screen.model.bottom.card.is_some());
}

#[test]
fn a_choice_rings_only_when_it_asks_to() {
    let mut screen = Screen::new();
    let mut start = choice(
        ChoiceKind::StartWork,
        vec![option(
            's',
            "Solo",
            AnswerAction::Choose {
                value: "solo".into(),
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

/// A review the lead picked, with no plan of its own: the workflow is what you approve.
fn review_plan() -> CheckpointOpened {
    use argonauts_core::events::checkpoint::{PlanPacket, PlanWorkflow};
    use argonauts_core::events::plan::{BudgetPlan, TaskGraph};
    use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
    use argonauts_core::workflow::WorkflowRef;

    let workflow = serde_json::from_value(serde_json::json!({
        "name": "review-panel",
        "description": "Two reviewers from other providers",
        "params": {"target": "diff", "reviewers": 2},
        "phases": [
            {"name": "Review", "standard": "check", "steps": [{
                "kind": "fan_out", "role": "reviewer", "count": 2, "blind": true,
                "different_family_from": "author"
            }]},
            {"name": "Adjudicate", "standard": "decide", "steps": [{
                "kind": "judge", "role": "adjudicator", "when": "reviewers_disagree"
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
            cards: Vec::new(),
            critical_path: Vec::new(),
        },
        acceptance_tests: Vec::new(),
        budget: BudgetPlan {
            lines: Vec::new(),
            total: BudgetAmount {
                unit: BudgetUnit::Usd,
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
        CheckpointKind::Plan,
        "Approve this plan?",
        CheckpointPacket::Plan(Box::new(packet)),
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
        "    review → adjudicate (if needed)\n    … +2 lines (ctrl+o to expand)",
    ] {
        assert!(item.contains(shown), "{shown} missing from:\n{item}");
    }
    for hidden in ["fan_out", "reviewers_disagree"] {
        assert!(!item.contains(hidden), "{hidden} shown in:\n{item}");
    }
    // The lead's reason is the plan's summary here, so it shows once.
    assert_eq!(item.matches("A review: two reviewers").count(), 1, "{item}");
    assert_eq!(
        screen.card().detail.first().map(String::as_str),
        Some("review-panel · review → adjudicate (if needed) · limit $20.00")
    );

    let expanded = screen.history_as(true).join("\n");
    for shown in [
        "Review: fan_out reviewer ×2, blind, another provider than author",
        "Adjudicate: judge adjudicator, when reviewers_disagree",
    ] {
        assert!(
            expanded.contains(shown),
            "{shown} missing from:\n{expanded}"
        );
    }
    assert!(!expanded.contains("ctrl+o to expand"), "{expanded}");
}

/// A loop reads as its rounds: `goal (up to 2 rounds) → plan (up to 3 rounds) → build`.
#[test]
fn a_looping_phase_says_its_rounds() {
    let mut opened = review_plan();
    let CheckpointPacket::Plan(plan) = &mut opened.packet else {
        unreachable!("a plan approval")
    };
    let shown = plan.workflow.as_mut().expect("a workflow");
    shown.workflow = serde_json::from_value(serde_json::json!({
        "name": "council",
        "description": "",
        "phases": [
            {"name": "Goal", "standard": "understand", "steps": [{
                "kind": "loop_until", "until": "goal_approved", "max": 2,
                "steps": [{"kind": "restate"}, {"kind": "checkpoint", "checkpoint": "goal"}]
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
        history.contains("goal (up to 2 rounds) → build → merge"),
        "{history}"
    );
    assert!(!history.contains("goal_approved"), "{history}");
}

/// Steps the runner runs have no step kinds, so each phase shows the line its script gave it.
#[test]
fn a_plan_of_a_scripts_steps_shows_each_phase_with_its_line() {
    use argonauts_core::workflow::WorkflowRef;

    let mut opened = review_plan();
    let CheckpointPacket::Plan(plan) = &mut opened.packet else {
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
        screen.card().detail.first().map(String::as_str),
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
    use argonauts_core::workflow::StepWorkers;

    let mut opened = review_plan();
    let CheckpointPacket::Plan(plan) = &mut opened.packet else {
        unreachable!("a plan approval")
    };
    let shown = plan.workflow.as_mut().expect("a workflow");
    shown.workflow.phases[0].steps[0].workers =
        StepWorkers::Named(vec!["codex/gpt-5.6".parse().expect("a worker")]);
    let mut screen = Screen::new();
    screen.event(opened_event(opened));
    let history = screen.history_as(true).join("\n");
    assert!(
        history.contains("Review: fan_out reviewer (GPT 5.6 on Codex) ×2"),
        "{history}"
    );
}

/// The plan card's title asks the question, so the card doesn't ask it a second time.
#[test]
fn a_card_whose_title_asks_asks_once() {
    let card = checkpoint_card(1, &review_plan(), None);
    assert_eq!(card.title, "Approve this plan?");
    assert_eq!(card.question, None);
}

/// The merge table names each owner as the transcript did: by the model its session ran, else
/// by the model its CLI's settings give.
#[test]
fn the_merge_table_names_owners_as_the_run_knows_them() {
    let mut opened = merge();
    if let CheckpointPacket::Merge(packet) = &mut opened.packet {
        packet.rows[0].owner = Some(worker("claude-code"));
        packet.rows[1].owner = Some(worker("codex"));
    }
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        worker("claude-code/claude-haiku-4-5"),
        "claude-code",
        argonauts_core::names::RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.card = Some(card_id(2));
    run.sessions.insert(session.key.clone(), session);
    let names = DisplayNames {
        configured: std::collections::BTreeMap::from([(
            "codex".to_owned(),
            "gpt-5.6-luna".to_owned(),
        )]),
        ..DisplayNames::default()
    };
    let named = with_run_owners(&opened, Some(&run), &names);
    let CheckpointPacket::Merge(packet) = &named.packet else {
        unreachable!("a merge approval")
    };
    let owners: Vec<String> = packet
        .rows
        .iter()
        .map(|row| names.worker(row.owner.as_ref().unwrap()))
        .collect();
    assert_eq!(
        owners,
        ["Haiku 4.5 on Claude Code", "GPT 5.6 Luna on Codex"]
    );
}

/// The merge table above the card names each card by its label, as the chat does, and a card
/// with none yet by its id.
#[test]
fn the_merge_table_names_cards_by_label() {
    let opened = merge();
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    let mut session = crate::state::SessionState::new(
        argonauts_core::names::SessionKey::numbered(1),
        worker("codex/gpt-5.6"),
        "codex",
        argonauts_core::names::RoleName::Worker,
        datetime!(2026-09-30 10:00 UTC),
    );
    session.card = Some(card_id(2));
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
}

/// With no command gates set nothing ran on the latest main, and the card says so rather than
/// calling the suite failed.
#[test]
fn a_merge_without_a_suite_says_none_ran() {
    use argonauts_core::events::card::{GateKind, GateOutcome};

    let suite = |passed: bool, command: Option<Vec<String>>| {
        let mut opened = merge();
        if let CheckpointPacket::Merge(packet) = &mut opened.packet {
            packet.suite_passed_on_latest_main = passed;
            packet.gates = vec![GateOutcome {
                name: "tests".into(),
                kind: GateKind::Command,
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
        let card = checkpoint_card(1, &opened, None).detail.join("\n");
        let item = about_item(&opened, None, UtcOffset::UTC, &DisplayNames::default())
            .map(|item| item.plain(100).join("\n"))
            .unwrap_or_default();
        (card, item)
    };
    let (card, item) = suite(false, None);
    assert!(card.contains("no test suite set"), "{card}");
    assert!(item.contains("No test suite ran"), "{item}");
    let (card, item) = suite(false, Some(vec!["npm".into(), "test".into()]));
    assert!(card.contains("suite failed on the latest main"), "{card}");
    assert!(item.contains("Suite didn't pass"), "{item}");
    let (card, _) = suite(true, Some(vec!["npm".into(), "test".into()]));
    assert!(card.contains("suite passed on the latest main"), "{card}");
}

/// The card counts the run's spend as the mode line and the working line do, even when the
/// packet's count lags behind the sessions' own reports.
#[test]
fn the_merge_card_counts_the_spend_the_mode_line_does() {
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
    let card = checkpoint_card(1, &merge(), Some(&run));
    let detail = card.detail.join("\n");
    assert!(detail.contains("$0.26 spent"), "{detail}");
    assert!((run.cost_usd() - 0.26).abs() < f64::EPSILON);
    // Without the run, the packet's count is all there is.
    let card = checkpoint_card(1, &merge(), None);
    assert!(card.detail.join("\n").contains("$0.00 spent"));
}

/// An attempt that ended without a report lists nothing in the packet, so the card counts the
/// card's owner sessions.
#[test]
fn an_escalation_counts_attempts_that_reported_nothing() {
    let mut run = crate::state::RunState::new(argonauts_core::RunId::new("run-01").unwrap());
    for n in 1..=2 {
        let mut session = crate::state::SessionState::new(
            argonauts_core::names::SessionKey::numbered(n),
            worker("opencode"),
            "opencode",
            argonauts_core::names::RoleName::Worker,
            datetime!(2026-09-30 10:00 UTC),
        );
        session.card = Some(card_id(4));
        session.step = Some(argonauts_core::steps::StepName::Build);
        run.sessions.insert(session.key.clone(), session);
    }
    let mut opened = escalation();
    if let CheckpointPacket::Escalation(packet) = &mut opened.packet {
        packet.attempts.clear();
    }
    let detail = checkpoint_card(1, &opened, Some(&run)).detail.join("\n");
    assert!(detail.contains("task-004 Parser · 2 attempts"), "{detail}");
    let detail = checkpoint_card(1, &opened, None).detail.join("\n");
    assert!(detail.contains("· 0 attempts"), "{detail}");
}

/// A plan's author ran its CLI's default, so the card names it by that model, as the
/// transcript named the draft's session.
#[test]
fn a_plans_author_reads_as_its_session_did() {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::events::plan::{JudgingResult, PlanTally};
    use argonauts_core::settings::protocol::JudgingMethod;

    let mut opened = review_plan();
    if let CheckpointPacket::Plan(packet) = &mut opened.packet {
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
    let card = checkpoint_card_local(1, &opened, None, UtcOffset::UTC, &names);
    let detail = card.detail.join("\n");
    assert!(
        detail.contains("Plan 1 by GPT 5.6 Luna on Codex scored best"),
        "{detail}"
    );
}

/// The merge waits on your untracked files: the conversation names them, the card asks only
/// what to do, and its record never says the run merged.
#[test]
fn files_in_the_way_wait_to_merge() {
    let mut waiting = merge();
    waiting.title = "Waiting to merge into main".into();
    waiting.free_text = FreeText::None;
    if let CheckpointPacket::Merge(packet) = &mut waiting.packet {
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

    let card = checkpoint_card(1, &waiting, None);
    assert_eq!(
        card.detail,
        ["textkit/__pycache__/core.cpython-312.pyc and 1 other in the way"]
    );
    assert_eq!(
        card.question.as_deref(),
        Some("Move them aside and merge, or merge later?")
    );
    assert!(!card.allow_note, "a choice, not something to type");

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

    screen.event(checkpoint(CheckpointEvent::Decided(decided(
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
/// conversation says which and why, and the card offers only trying again or merging later.
#[test]
fn edits_and_git_refusals_wait_to_merge_too() {
    let choose = |value: &str| AnswerAction::Choose {
        value: value.into(),
    };
    let waiting = |edited: Vec<String>, refused: Option<String>| {
        let mut waiting = merge();
        waiting.title = "Waiting to merge into main".into();
        waiting.free_text = FreeText::None;
        if let CheckpointPacket::Merge(packet) = &mut waiting.packet {
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
    let card = checkpoint_card(1, &edited, None);
    assert_eq!(card.detail, ["README.md is in the way"]);
    assert_eq!(card.question.as_deref(), Some("Try again, or merge later?"));
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
    let card = checkpoint_card(1, &refused, None);
    assert_eq!(card.detail, [format!("Git said: {lock}")]);
    let mut screen = Screen::new();
    screen.event(opened_event(refused));
    let about = screen.history().join("\n");
    assert!(
        about.contains("Git wouldn't move your checkout of main forward:"),
        "{about}"
    );
    assert!(about.contains("index.lock"), "{about}");
}

fn check(verdict: Option<Verdict>, shadow: bool) -> AppEvent {
    checkpoint(CheckpointEvent::Checked(PolicyCheck {
        id: CheckpointId::numbered(1),
        violations: Vec::new(),
        verdict,
        by: Some(DecideWith::Rules),
        approver: None,
        reasons: vec!["The plan goes over the limit".into()],
        shadow,
    }))
}

#[test]
fn a_card_your_rules_still_decide_takes_the_keys_once_it_needs_you() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    assert_eq!(
        screen.model.bottom.card, None,
        "it asks nothing while your rules decide, so it stays off the screen"
    );
    let out = screen.event(check(Some(Verdict::Escalate), true));
    assert!(
        !out.bell && screen.model.bottom.card.is_none(),
        "an auto preview check is only recorded"
    );
    // The engine's own shape for "ask the user": no verdict, and maybe no reason either.
    let out = screen.event(checkpoint(CheckpointEvent::Checked(PolicyCheck {
        id: CheckpointId::numbered(1),
        violations: Vec::new(),
        verdict: None,
        by: None,
        approver: None,
        reasons: Vec::new(),
        shadow: false,
    })));
    assert!(out.bell);
    assert!(screen.card().needs_you && screen.card().focused);

    // Your rules escalating says the same.
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    screen.event(check(Some(Verdict::Escalate), false));
    assert!(screen.card().needs_you && screen.card().focused);
}

#[test]
fn a_card_that_needs_you_goes_before_a_waiting_spot_check() {
    let mut screen = Screen::new();
    let mut auto = merge();
    auto.mode = ApprovalMode::Auto;
    auto.needs_you = false;
    screen.event(opened_event(auto));
    screen.event(checkpoint(CheckpointEvent::Decided(decided(
        AnswerAction::Merge,
        Party::Rules,
    ))));
    screen.event(checkpoint(CheckpointEvent::SpotCheckQueued {
        id: CheckpointId::numbered(1),
    }));
    assert!(screen.card().title.starts_with("Spot check"));
    assert!(
        screen.card().focused,
        "a spot check asks, so it takes the keys"
    );

    let mut next = handoff();
    next.id = CheckpointId::numbered(2);
    screen.event(opened_event(next));
    assert_eq!(screen.card().title, "Fallback · task-003 Middleware");
    assert!(screen.card().focused);
    screen.keys(&[KeyCode::Esc]);
    assert!(screen.card().title.starts_with("Spot check"));
    assert!(
        screen.card().focused,
        "and it asks again once the fallback is handed on"
    );
}

/// A run's card takes the keys as it shows, even mid-draft: the draft goes aside, and comes
/// back once the card is handed on.
#[test]
fn a_card_that_shows_while_you_type_sets_the_draft_aside() {
    let mut screen = Screen::new();
    screen.model.bottom.composer.set_text("keep the CSV pa");
    screen.event(opened_event(merge()));
    assert!(screen.card().focused, "the card took the keys");
    assert!(screen.model.bottom.composer.is_empty());
    screen.keys(&[KeyCode::Esc]);
    assert!(screen.model.bottom.card.is_none());
    assert!(screen.model.bottom.settle_draft());
    assert_eq!(screen.model.bottom.composer.text(), "keep the CSV pa");
}

/// An answered question's record goes by its title, which names the asking agent's label, so
/// no card id shows around the card.
#[test]
fn an_answered_question_goes_by_its_label() {
    let question = opened(
        CheckpointKind::Question,
        "Question from build:fix-money",
        CheckpointPacket::Question(QuestionPacket {
            from: worker("claude-code/opus"),
            card: card_id(2),
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

/// The goal's own-answer option opens a line inside the card, and only that answer goes out.
#[test]
fn a_goal_question_takes_your_own_answer_inside_the_card() {
    let mut screen = Screen::new();
    let mut question = opened(
        CheckpointKind::Question,
        "Keep a CSV fallback?",
        CheckpointPacket::Question(QuestionPacket {
            from: worker("claude-code/opus"),
            card: card_id(2),
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
    assert!(screen.card().focused);
    let own = screen.card().options[2].clone();
    assert_eq!(own.kind, OptionKind::FreeText);
    screen.keys(&[KeyCode::Char('3')]);
    assert!(screen.card().has_open_note());
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
        Some(SettingScope) => Some(DetectedGates),
        Some(DetectedGates) => Some(TrustProject),
        Some(TrustProject) => Some(ConfirmRevert),
        Some(ConfirmRevert) => Some(ConfirmConnect),
        Some(ConfirmConnect) => Some(KeepInstructions),
        Some(KeepInstructions) => Some(Permission),
        Some(Permission) => Some(RefusedCommands),
        Some(RefusedCommands) => Some(RewindTurn),
        Some(RewindTurn) => Some(GoAhead),
        Some(GoAhead) => Some(HandOff),
        Some(HandOff) => Some(TryWorkflow),
        Some(TryWorkflow) => Some(Setup),
        Some(Setup) => Some(Git),
        Some(Git) => None,
    }
}

/// Every kind of checkpoint, walked the same way.
fn next_checkpoint_kind(kind: Option<CheckpointKind>) -> Option<CheckpointKind> {
    use CheckpointKind::*;
    match kind {
        None => Some(Goal),
        Some(Goal) => Some(Plan),
        Some(Plan) => Some(Merge),
        Some(Merge) => Some(Handoff),
        Some(Handoff) => Some(VendorDown),
        Some(VendorDown) => Some(Escalation),
        Some(Escalation) => Some(Budget),
        Some(Budget) => Some(ChangeRequest),
        Some(ChangeRequest) => Some(GoalChange),
        Some(GoalChange) => Some(WorkflowPromotion),
        Some(WorkflowPromotion) => Some(TakeOverReturn),
        Some(TakeOverReturn) => Some(Question),
        Some(Question) => Some(OriginalReturned),
        Some(OriginalReturned) => None,
    }
}

/// The bottom of the screen with `card` up and an input nobody touched, after a frame.
fn bottom_with(card: CardPrompt) -> (crate::bottom::BottomState, Vec<String>) {
    let mut bottom = crate::bottom::BottomState::new();
    bottom.card = Some(card);
    let now = Instant::now();
    bottom.hand_keys_to_card(now);
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    let view = bottom.render(&RenderCtx::new(100, &theme, Clock::still(now)));
    let rows = plain_text(&view.lines).lines().map(str::to_owned).collect();
    (bottom, rows)
}

/// As a rule rather than a list: every card that asks you something takes the keys
/// once the input is free, and the input box goes while it holds them, whatever its kind. A new
/// kind of choice or checkpoint fails here until it does.
#[test]
fn every_card_that_asks_takes_the_keys_and_hides_the_input() {
    let mut cards = Vec::new();
    let mut kind = next_choice_kind(None);
    while let Some(each) = kind {
        let options = vec![
            option('y', "Yes", AnswerAction::Approve),
            option('n', "No", AnswerAction::Cancel),
        ];
        cards.push(choice::choice_card(1, &choice(each, options)));
        kind = next_choice_kind(Some(each));
    }
    let mut kind = next_checkpoint_kind(None);
    while let Some(each) = kind {
        let mut opened = merge();
        opened.kind = each;
        cards.push(checkpoint::checkpoint_card(1, &opened, None));
        kind = next_checkpoint_kind(Some(each));
    }
    let names = DisplayNames::default();
    cards.push(spot::spot_check_card(1, &merge(), None, None, &names));

    for card in cards {
        let title = card.title.clone();
        assert!(card.asks(), "{title} asks you, so it says so");
        let (bottom, rows) = bottom_with(card);
        assert!(bottom.focused_card().is_some(), "{title} takes the keys");
        assert!(
            !rows.iter().any(|row| row.starts_with('❯')),
            "{title} stands in for the input:\n{}",
            rows.join("\n")
        );
    }
}

/// A go-ahead in full auto, as the peer crew's was: while full auto decides, "Run this
/// workflow?" asks nothing and stays off the screen. Once its approver can't check it and full
/// auto leaves it to you, it asks and holds the keys with the input hidden, never waiting for ↑
/// under a ruled input.
#[test]
fn a_go_ahead_autopilot_leaves_to_you_holds_the_keys() {
    let mut screen = Screen::new();
    let mut go_ahead = merge();
    go_ahead.kind = CheckpointKind::Goal;
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
        screen.model.bottom.card, None,
        "a task autopilot still decides isn't on screen"
    );
    screen.event(checkpoint(CheckpointEvent::Checked(PolicyCheck {
        id: CheckpointId::numbered(1),
        violations: Vec::new(),
        verdict: None,
        by: None,
        approver: None,
        reasons: vec!["the approver's session ended early: auth".into()],
        shadow: false,
    })));
    let card = screen.card();
    assert_eq!(card.title, "Run this workflow?");
    assert!(card.asks());
    assert!(card.focused, "it took the keys as it showed");
    let (bottom, rows) = bottom_with(card.clone());
    assert!(bottom.focused_card().is_some(), "{}", rows.join("\n"));
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("to answer the question") || row.starts_with('❯')),
        "{}",
        rows.join("\n")
    );
}

/// The setup card as it was approved: the settings in plain text with only the notes dim, the
/// four places to keep it listed, and `d` for what would be written.
#[test]
fn a_setup_card_shows_its_settings_plain_its_notes_dim_and_four_choices() {
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
            "reviewers     \tadds to\t roles/reviewers.md".into(),
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
    let card = screen.card();
    let labels: Vec<&str> = card.options.iter().map(|o| o.label.as_str()).collect();
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
    let lines = card.lines(&ctx);
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
    assert_ne!(style_of("roles/reviewers.md"), Some(dim));
    let shown = plain_text(&lines);
    assert!(
        shown.contains("limits        spend_per_day = 5.00   when reached: fallback"),
        "{shown}"
    );
    assert!(!shown.contains("You asked"), "{shown}");

    let effects = screen.keys(&[KeyCode::Char('d')]);
    assert_eq!(effects.len(), 1, "d opens what would be written");
    assert!(!effects[0].views.is_empty(), "a view opens");
    assert!(screen.model.bottom.card.is_some(), "the card stays");
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
