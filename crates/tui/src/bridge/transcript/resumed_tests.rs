//! A resumed conversation and a command's report, as they print.

use std::time::Instant;

use argonauts_core::Verbosity;
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::events::run::Party;
use argonauts_core::names::{ChatId, SessionKey, WorkerRef};
use argonauts_core::settings::ui::UiSettings;

use super::Transcript;
use crate::bridge::{Cx, Effects};
use crate::model::ViewModel;
use crate::render::{Clock, RenderCtx};
use crate::state::AppState;
use crate::text::plain_text;
use crate::theme::{Glyphs, Theme};

#[derive(Default)]
struct Screen {
    model: ViewModel,
    state: AppState,
    settings: UiSettings,
    transcript: Transcript,
    closed_views: bool,
}

impl Screen {
    fn send(&mut self, event: AppEvent) {
        let mut out = Effects::default();
        let mut cx = Cx {
            model: &mut self.model,
            state: &self.state,
            settings: &self.settings,
            now: Instant::now(),
            out: &mut out,
        };
        self.transcript.on_event(&event, &mut cx);
        self.closed_views |= out.close_views;
    }

    fn printed(&mut self) -> String {
        let mut theme = Theme::dark();
        theme.glyphs = Glyphs::UNICODE;
        let ctx = RenderCtx::new(80, &theme, Clock::still(Instant::now()));
        plain_text(&self.model.history.drain_printable(&ctx))
    }
}

fn you(text: &str) -> ReplayTurn {
    ReplayTurn {
        from: Party::User,
        text: text.to_owned(),
    }
}

fn agent(text: &str) -> ReplayTurn {
    ReplayTurn {
        from: Party::Agent {
            session: SessionKey::parse("t-001").unwrap(),
            worker: WorkerRef::new("claude-code", None),
        },
        text: text.to_owned(),
    }
}

#[test]
fn a_resumed_conversation_prints_its_last_turns_and_keeps_the_rest_for_ctrl_o() {
    let mut screen = Screen::default();
    screen.printed();
    screen.send(AppEvent::ChatResumed {
        chat: ChatId::numbered(3),
        title: "hi".to_owned(),
        worker: Some(WorkerRef::new("claude-code", None)),
        earlier: vec![you("hi"), agent("Hi. Ask me anything.")],
        turns: vec![you("what does export do?"), agent("It writes CSV.")],
        artifacts: Vec::new(),
    });
    let printed = screen.printed();
    assert_eq!(
        printed,
        "● Resumed chat-003 · hi\n  ⎿  with Claude Code · 1 earlier turn folded\n     ctrl+o shows the whole history\n\n❯ what does export do?\n\n● It writes CSV."
    );
    assert!(!printed.contains("Ask me anything"));
    assert!(
        screen.closed_views,
        "the picker gives the screen to the conversation"
    );
    let kept: String = screen
        .model
        .history
        .cells()
        .iter()
        .map(|cell| format!("{cell:?}"))
        .collect();
    assert!(kept.contains("Ask me anything"), "{kept}");
}

#[test]
fn a_report_prints_even_when_quiet() {
    let mut screen = Screen::default();
    screen.settings.verbosity = Verbosity::Quiet;
    screen.printed();
    screen.send(AppEvent::Report {
        title: "Doctor".to_owned(),
        lines: vec![
            "claude-code  2.1.300  tested  logged in".to_owned(),
            "git 2.50.1".to_owned(),
        ],
    });
    let printed = screen.printed();
    assert!(
        printed.starts_with("● Doctor\n  ⎿  claude-code"),
        "{printed}"
    );
    assert!(printed.contains("git 2.50.1"), "{printed}");
}
