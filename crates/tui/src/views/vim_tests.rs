//! Vim keys in the list-shaped views that have no tests of their own: `hjkl`, `g`, and `G`
//! with `ui.keys = "vim"`, `/` for the filter in `/config`, and plain letters without.

use std::path::PathBuf;
use std::time::Instant;

use argonauts_core::api::{RawView, RunState as RunPhase, RunSummary};
use argonauts_core::app::{SettingRow, UiCommand};
use argonauts_core::events::run::RunMode;
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::settings::SettingScope;
use argonauts_core::views::{CheckpointRow, WorkflowFile, WorkflowOrigin};
use argonauts_core::{RunId, Spend, TaskId};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use time::OffsetDateTime;

use super::help::HelpView;
use super::library::LibraryView;
use super::list::keys_state;
use super::pager::Pager;
use super::resume::ResumePicker;
use super::rewind::RewindPicker;
use super::settings::approvals::ApprovalsView;
use super::settings::config::ConfigView;
use super::{View, ViewCtx, ViewOutcome};
use crate::keymap::Keymap;
use crate::theme::{Role, Theme};

/// Drives one view with vim keys on or off, drawing it first so it knows its page.
struct Driver {
    theme: Theme,
    state: crate::state::AppState,
}

impl Driver {
    fn new(vim: bool) -> Self {
        Self {
            theme: Theme::dark(),
            state: keys_state(vim),
        }
    }

    fn ctx(&self) -> ViewCtx<'_> {
        ViewCtx {
            theme: &self.theme,
            state: &self.state,
            now: Instant::now(),
        }
    }

    fn draw(&self, view: &mut dyn View) {
        self.text(view);
    }

    fn text(&self, view: &mut dyn View) -> String {
        crate::stability::buffer_text(&self.rendered(view))
    }

    fn rendered(&self, view: &mut dyn View) -> Buffer {
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf, &self.ctx());
        buf
    }

    fn key(&self, view: &mut dyn View, code: KeyCode) -> ViewOutcome {
        view.handle_key(KeyEvent::from(code), &self.ctx())
    }

    fn press(&self, view: &mut dyn View, c: char) -> ViewOutcome {
        self.key(view, KeyCode::Char(c))
    }
}

fn run(n: u32) -> RunSummary {
    RunSummary {
        run: RunId::new(format!("run-{n:02}")).unwrap(),
        title: format!("Run {n}"),
        mode: RunMode::Crew,
        workflow: "council".into(),
        state: RunPhase::Waiting,
        stopped_at: "merge approval".into(),
        updated: OffsetDateTime::UNIX_EPOCH,
        spent: Spend::default(),
        sessions_to_resume: 1,
        waiting: Vec::new(),
        live: false,
        remind: false,
        name: None,
    }
}

fn setting(key: &str) -> SettingRow {
    SettingRow {
        key: key.into(),
        value: "\"x\"".into(),
        layer: "global".into(),
        workflow_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: vec!["\"x\"".into(), "\"y\"".into()],
        description: format!("Sets {key}"),
    }
}

fn is_ignored(outcome: &ViewOutcome) -> bool {
    matches!(outcome, ViewOutcome::Ignored)
}

#[test]
fn resume_picker() {
    let runs = || vec![run(3), run(2), run(1)];
    let off = Driver::new(false);
    let mut view = ResumePicker::new(runs());
    off.draw(&mut view);
    for c in ['j', 'k', 'g', 'G', 'h', 'l'] {
        assert!(is_ignored(&off.press(&mut view, c)), "{c}");
    }

    let vim = Driver::new(true);
    let mut view = ResumePicker::new(runs());
    vim.draw(&mut view);
    vim.press(&mut view, 'G');
    vim.press(&mut view, 'k');
    let resumed = |outcome: ViewOutcome| match outcome {
        ViewOutcome::SendToConversation(UiCommand::Resume { run: Some(run) }) => run.to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(resumed(vim.press(&mut view, 'l')), "run-02");
    vim.press(&mut view, 'g');
    vim.press(&mut view, 'j');
    vim.press(&mut view, 'j');
    assert_eq!(resumed(vim.press(&mut view, 'l')), "run-01");
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Close));
}

#[test]
fn help_and_pagers_scroll() {
    let raw = || {
        Pager::raw(RawView {
            session: SessionKey::numbered(1),
            lines: (0..100).map(|n| format!("line {n}")).collect(),
            truncated: false,
            path: PathBuf::from("raw.jsonl"),
            cli_transcript: None,
        })
    };
    let off = Driver::new(false);
    let vim = Driver::new(true);

    let mut pager = raw();
    off.draw(&mut pager);
    for c in ['j', 'G', 'h'] {
        assert!(is_ignored(&off.press(&mut pager, c)), "{c}");
    }
    assert_eq!(pager.offset, 0);
    // ← goes back, with or without vim keys.
    assert!(matches!(
        off.key(&mut pager, KeyCode::Left),
        ViewOutcome::Close
    ));

    // The raw log moves its cursor by event, and the rows follow it at the next frame.
    let mut pager = raw();
    vim.draw(&mut pager);
    assert!(!is_ignored(&vim.press(&mut pager, 'j')));
    vim.press(&mut pager, 'G');
    vim.draw(&mut pager);
    let bottom = pager.top();
    assert!(bottom > 50, "{bottom}");
    // One up from the last event is still in view, so nothing scrolls.
    vim.press(&mut pager, 'k');
    vim.draw(&mut pager);
    assert_eq!(pager.top(), bottom);
    vim.press(&mut pager, 'g');
    vim.draw(&mut pager);
    assert_eq!(pager.top(), 0);
    assert!(matches!(vim.press(&mut pager, 'h'), ViewOutcome::Close));

    let mut help = HelpView::new(Keymap::default());
    off.draw(&mut help);
    assert!(is_ignored(&off.press(&mut help, 'j')));
    assert!(is_ignored(&off.press(&mut help, 'h')));
    let mut help = HelpView::new(Keymap::default());
    vim.draw(&mut help);
    vim.press(&mut help, 'j');
    assert_eq!(help.offset, 1);
    vim.press(&mut help, 'g');
    assert_eq!(help.offset, 0);
    assert!(matches!(vim.press(&mut help, 'h'), ViewOutcome::Close));
}

/// A long session's raw log keeps all of it, draws as fast as a short one, and `/` finds what
/// is near its start.
#[test]
fn a_long_raw_log_draws_quickly_and_search_finds_its_start() {
    let off = Driver::new(false);
    let mut pager = Pager::raw(RawView {
        session: SessionKey::numbered(1),
        lines: (0..5_000)
            .map(|n| format!(r#"{{"type":"event","n":{n},"text":"step {n}"}}"#))
            .collect(),
        truncated: false,
        path: PathBuf::from("raw.jsonl"),
        cli_transcript: None,
    });
    let started = Instant::now();
    for _ in 0..20 {
        off.draw(&mut pager);
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    off.press(&mut pager, '/');
    for c in "step 10\"".chars() {
        off.press(&mut pager, c);
    }
    off.key(&mut pager, KeyCode::Enter);
    let shown = off.text(&mut pager);
    assert!(shown.contains("step 10\""), "{shown}");
    assert!(shown.contains("of 5000"), "{shown}");
}

#[test]
fn config_filters_as_you_type_without_vim_keys() {
    let off = Driver::new(false);
    let mut view = ConfigView::new(vec![setting("ui.keys"), setting("ui.theme")]);
    off.press(&mut view, 'j');
    assert_eq!(view.filter, "j");
    off.key(&mut view, KeyCode::Esc);
    assert_eq!(view.filter, "");
    off.press(&mut view, 't');
    assert_eq!(view.filter, "t");
    assert!(!view.filtering);
}

#[test]
fn config_with_vim_keys_filters_only_after_slash() {
    let vim = Driver::new(true);
    let mut view = ConfigView::new(vec![
        setting("ui.keys"),
        setting("ui.theme"),
        setting("ui.verbs"),
    ]);
    // Plain letters do nothing, and the vim ones move.
    assert!(matches!(vim.press(&mut view, 't'), ViewOutcome::Consumed));
    assert_eq!(view.filter, "");
    vim.press(&mut view, 'j');
    assert_eq!(view.selected, 1);
    vim.press(&mut view, 'G');
    assert_eq!(view.selected, 2);
    vim.press(&mut view, 'g');
    assert_eq!(view.selected, 0);

    // `/` starts the filter, where every letter is text, and Esc leaves it.
    vim.press(&mut view, '/');
    assert!(view.filtering);
    for c in "theme".chars() {
        vim.press(&mut view, c);
    }
    assert_eq!(view.filter, "theme");
    assert!(vim.text(&mut view).contains("1 of 3 match"));
    assert!(matches!(
        vim.key(&mut view, KeyCode::Esc),
        ViewOutcome::Consumed
    ));
    assert!(!view.filtering);
    assert_eq!(view.filter, "");

    // `l` edits the row as Enter does, `h` backs out of the editor, then out of the view.
    vim.press(&mut view, 'j');
    assert!(matches!(vim.press(&mut view, 'l'), ViewOutcome::Consumed));
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Consumed));
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Close));
}

#[test]
fn config_editor_picks_with_vim_keys() {
    let vim = Driver::new(true);
    let mut view = ConfigView::new(vec![setting("ui.keys")]);
    vim.press(&mut view, 'l');
    // The editor's values move with `j` and pick with `l`, then ask where to save it.
    vim.press(&mut view, 'j');
    let outcome = vim.press(&mut view, 'l');
    assert!(matches!(outcome, ViewOutcome::Consumed), "{outcome:?}");
    let text = vim.text(&mut view);
    assert!(text.contains("Where to save it"), "{text}");
}

#[test]
fn approvals_view() {
    let rows = || {
        vec![
            setting("approvals.mode"),
            setting("approvals.decide_with"),
            setting("approvals.spot_check"),
        ]
    };
    let off = Driver::new(false);
    let mut view = ApprovalsView::with_rows(rows());
    for c in ['j', 'G', 'h', 'l'] {
        assert!(is_ignored(&off.press(&mut view, c)), "{c}");
    }
    assert_eq!(view.selected, 0);

    let vim = Driver::new(true);
    let mut view = ApprovalsView::with_rows(rows());
    vim.press(&mut view, 'j');
    assert_eq!(view.selected, 1);
    vim.press(&mut view, 'G');
    assert_eq!(view.selected, 2);
    vim.press(&mut view, 'k');
    vim.press(&mut view, 'g');
    assert_eq!(view.selected, 0);
    // `l` opens the editor, so the first `h` closes it and the next closes the view.
    vim.press(&mut view, 'l');
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Consumed));
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Close));
}

#[test]
fn without_vim_keys_right_opens_a_level_and_left_goes_back() {
    let off = Driver::new(false);
    // In the settings views → opens the editor, ← backs out of it and then out of the view.
    let mut config = ConfigView::new(vec![setting("ui.keys")]);
    let mut approvals = ApprovalsView::with_rows(vec![setting("approvals.mode")]);
    for view in [&mut config as &mut dyn View, &mut approvals] {
        off.draw(view);
        off.key(view, KeyCode::Right);
        assert!(matches!(
            off.key(view, KeyCode::Left),
            ViewOutcome::Consumed
        ));
        assert!(matches!(off.key(view, KeyCode::Left), ViewOutcome::Close));
    }
}

fn checkpoint(n: u32) -> CheckpointRow {
    CheckpointRow {
        task: TaskId::new("task-001").unwrap(),
        n,
        at: OffsetDateTime::UNIX_EPOCH,
        commit: "0123456789abcdef".to_owned(),
        from: ModelRef::new("claude-code", None),
        to: vec![ModelRef::new("codex", None)],
        reason: "usage limit".to_owned(),
        rewound_here: false,
    }
}

#[test]
fn rewind_picker() {
    let open = || {
        RewindPicker::new(
            RunId::new("run-02").unwrap(),
            None,
            vec![checkpoint(1), checkpoint(2), checkpoint(3)],
        )
    };
    let off = Driver::new(false);
    let mut view = open();
    off.draw(&mut view);
    for c in ['j', 'k', 'g', 'G', 'h', 'l'] {
        assert!(is_ignored(&off.press(&mut view, c)), "{c}");
    }

    let vim = Driver::new(true);
    let mut view = open();
    vim.draw(&mut view);
    let picked = |outcome: ViewOutcome| match outcome {
        ViewOutcome::SendToConversation(UiCommand::Rewind { checkpoint, .. }) => checkpoint,
        other => panic!("{other:?}"),
    };
    // It opens on the newest checkpoint.
    vim.press(&mut view, 'k');
    assert_eq!(picked(vim.press(&mut view, 'l')), 2);
    vim.press(&mut view, 'g');
    assert_eq!(picked(vim.press(&mut view, 'l')), 1);
    vim.press(&mut view, 'G');
    assert_eq!(picked(vim.press(&mut view, 'l')), 3);
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Close));
}

fn workflow_file(name: &str) -> WorkflowFile {
    WorkflowFile {
        name: name.to_owned(),
        origin: WorkflowOrigin::BuiltIn,
        source: "built in".to_owned(),
        description: format!("The {name} workflow"),
        text: format!("name: {name}\n"),
        edited: None,
        in_use: false,
        problem: None,
    }
}

#[test]
fn workflow_library() {
    let open = || LibraryView::new(vec![workflow_file("captain"), workflow_file("council")]);
    let off = Driver::new(false);
    let mut view = open();
    off.draw(&mut view);
    for c in ['j', 'k', 'g', 'G', 'h'] {
        assert!(is_ignored(&off.press(&mut view, c)), "{c}");
    }
    assert_eq!(view.library.list.selected, 0);

    let vim = Driver::new(true);
    let mut view = open();
    vim.draw(&mut view);
    vim.press(&mut view, 'j');
    assert_eq!(view.library.list.selected, 1);
    vim.press(&mut view, 'g');
    assert_eq!(view.library.list.selected, 0);
    vim.press(&mut view, 'G');
    assert_eq!(view.library.list.selected, 1);
    assert!(matches!(vim.press(&mut view, 'l'), ViewOutcome::Open(_)));
    assert!(matches!(vim.press(&mut view, 'h'), ViewOutcome::Close));
}

/// The pointer and the chosen row are in the question colour, as on every list.
#[test]
fn the_chosen_row_is_in_the_question_colour() {
    let driver = Driver::new(false);
    let colour = driver.theme.style(Role::Question).fg;
    let rewind = RewindPicker::new(RunId::new("run-02").unwrap(), None, vec![checkpoint(1)]);
    let views: Vec<(&str, Box<dyn View>)> = vec![
        ("resume", Box::new(ResumePicker::new(vec![run(1)]))),
        ("rewind", Box::new(rewind)),
        (
            "library",
            Box::new(LibraryView::new(vec![workflow_file("captain")])),
        ),
    ];
    for (name, mut view) in views {
        let buf = driver.rendered(view.as_mut());
        let pointer = buf
            .content()
            .iter()
            .find(|cell| cell.symbol() == "❯")
            .unwrap_or_else(|| panic!("{name} draws no pointer"));
        assert_eq!(pointer.fg, colour.unwrap(), "{name}");
    }
}
