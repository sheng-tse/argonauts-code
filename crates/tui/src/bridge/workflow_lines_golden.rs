//! The line a workflow's command leaves under itself as typed: where the switch saved, or that
//! the workflow is in use already and who does what in it.

use std::time::Instant;

use argonauts_core::app::{AppEvent, AppMode, CommandReply};
use argonauts_core::base::names::TurnId;
use argonauts_core::settings::SettingScope;
use ratatui::text::Line;

use super::super::testing::pictures::picture_check::assert_rows_match;
use super::Bridge;
use crate::UiOptions;
use crate::app::UiAction;
use crate::app::model::ViewModel;
use crate::base::draw::render::{Clock, RenderCtx};
use crate::base::text::plain_text;
use crate::base::theme::{Glyphs, Theme};

const WIDTH: u16 = 120;
const PICTURE: (&str, &str) = (
    include_str!("../../tests/golden/workflow_switch_lines.txt"),
    include_str!("../../tests/golden/workflow_switch_lines.html"),
);

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

struct Screen {
    bridge: Bridge,
    model: ViewModel,
}

impl Screen {
    fn new() -> Self {
        Self {
            bridge: Bridge::new(&UiOptions::default()),
            model: ViewModel::default(),
        }
    }

    fn typed(&mut self, text: &str) {
        let submit = UiAction::Submit {
            text: text.into(),
            shown: text.into(),
        };
        self.bridge
            .on_action(&mut self.model, &submit, Instant::now());
    }

    fn send(&mut self, event: AppEvent) {
        self.bridge
            .on_event(&mut self.model, &event, Instant::now());
    }

    /// What printed since last asked, without the blank row a new item starts with.
    fn printed(&mut self) -> Vec<Line<'static>> {
        let theme = theme();
        let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
        self.model
            .history
            .drain_printable(&ctx)
            .into_iter()
            .filter(|line| !plain_text(std::slice::from_ref(line)).trim().is_empty())
            .collect()
    }
}

fn switched_to_council(previous: &str) -> AppEvent {
    switched_to_council_in(Some(previous), SettingScope::Global, None)
}

fn switched_to_council_in(
    previous: Option<&str>,
    scope: SettingScope,
    why: Option<&str>,
) -> AppEvent {
    AppEvent::SettingChanged {
        key: "workflow".into(),
        value: "\"council\"".into(),
        scope,
        previous: previous.map(|previous| format!("\"{previous}\"")),
        reset: false,
        why: why.map(Into::into),
        kept: false,
    }
}

#[test]
fn a_ready_workflows_command_says_where_it_switched_under_itself() {
    let theme = theme();
    let mut screen = Screen::new();
    screen.typed("/council");
    screen.send(switched_to_council("captain"));
    assert_rows_match(
        "workflow_switch_lines",
        &screen.printed(),
        PICTURE,
        0..2,
        &theme,
    );

    // Where the team's file sets the workflow, the switch saves for you alone and says why.
    screen.typed("/council");
    screen.send(switched_to_council_in(
        None,
        SettingScope::ProjectLocal,
        Some("since the team's file sets the workflow"),
    ));
    assert_rows_match(
        "workflow_switch_lines",
        &screen.printed(),
        PICTURE,
        3..5,
        &theme,
    );

    // Typed while an answer runs, the switch waits for the next request and says so.
    screen.send(AppEvent::Mode {
        mode: AppMode::Answering {
            turn: TurnId::numbered(1),
        },
    });
    screen.typed("/council");
    screen.send(switched_to_council("captain"));
    let lines = screen.printed();
    let text = plain_text(&lines);
    assert_eq!(
        text.trim_end(),
        "❯ /council\n  ⎿  Switched to Council · everywhere (~/.argonauts/config.toml) · was \
         Captain · applies at the next request",
        "one echo over the line, as any command typed mid-answer gets"
    );
    assert_rows_match("workflow_switch_lines", &lines, PICTURE, 6..8, &theme);

    // A flag saves it where it says, here for this session only, which goes when you quit.
    screen.send(AppEvent::Mode {
        mode: AppMode::Idle,
    });
    screen.typed("/council --session");
    screen.send(switched_to_council_in(
        Some("captain"),
        SettingScope::Session,
        None,
    ));
    assert_rows_match(
        "workflow_switch_lines",
        &screen.printed(),
        PICTURE,
        9..11,
        &theme,
    );
}

#[test]
fn the_workflow_in_use_says_who_does_what_under_its_command() {
    let theme = theme();
    let mut screen = Screen::new();
    screen.typed("/council");
    screen.send(AppEvent::CommandReplied {
        reply: CommandReply {
            text: "Council is the workflow already · Opus 5.5, GPT 5.6 and Gemini 3.6 propose"
                .into(),
            value: None,
            after: None,
            dim: false,
            error: false,
        },
    });
    assert_rows_match(
        "workflow_switch_lines",
        &screen.printed(),
        PICTURE,
        12..14,
        &theme,
    );
}

#[test]
fn a_switch_from_a_picker_prints_no_echo() {
    let mut screen = Screen::new();
    screen.send(switched_to_council("captain"));
    assert_eq!(
        plain_text(&screen.printed()).trim(),
        "⎿  Switched to Council · everywhere (~/.argonauts/config.toml) · was Captain"
    );
    // A workflow's command typed and then left, as Esc leaves its sheet, echoes nothing later.
    screen.typed("/council");
    screen.typed("/status");
    screen.send(switched_to_council("captain"));
    assert!(!plain_text(&screen.printed()).contains("❯ /council"));
}
