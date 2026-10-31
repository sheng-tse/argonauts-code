//! The turns made in a CLI's own screen, drawn under one line when the conversation comes back
//! to its session, at the picture's width of 118 columns.

use argonauts_core::agents::cli_sessions::{CliCall, CliStep};
use argonauts_core::app::{AppEvent, ReplayTurn};
use argonauts_core::base::names::{ModelRef, SessionKey};
use argonauts_core::runs::events::run::Party;
use serde_json::json;

use super::live_rows_tests::Screen;
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 118;

fn you(text: &str) -> ReplayTurn {
    ReplayTurn::said(Party::User, text)
}

fn agent(steps: Vec<CliStep>) -> ReplayTurn {
    let text = steps
        .iter()
        .filter_map(|step| match step {
            CliStep::Wrote { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    ReplayTurn {
        from: Party::Agent {
            session: SessionKey::numbered(0),
            worker: ModelRef::new("claude-code", None),
        },
        text,
        steps,
    }
}

fn wrote(text: &str) -> CliStep {
    CliStep::Wrote {
        text: text.to_owned(),
    }
}

#[test]
fn coming_back_from_the_cli_draws_the_turns_made_there_under_one_line() {
    let mut tests = CliCall::new("toolu_1", "Bash", json!({"command": "cargo test"}));
    tests.ok = Some(true);
    tests.output = Some("48 passed, 0 failed".to_owned());
    let mut screen = Screen::new();
    screen.app(AppEvent::TurnsMadeInCli {
        said: "Back from Claude Code · 2 turns made there".to_owned(),
        turns: vec![
            you("Make the due date optional"),
            agent(vec![wrote(
                "The --due flag is optional now, and list puts undated tasks last.",
            )]),
            you("Run the tests"),
            agent(vec![CliStep::Ran(tests), wrote("All 48 tests pass.")]),
        ],
    });
    let lines = screen.lines(WIDTH);
    assert_matches(
        "back_from_cli",
        &lines,
        include_str!("../../../../tests/golden/transcript/back_from_cli.txt"),
        include_str!("../../../../tests/golden/transcript/back_from_cli.html"),
        &Screen::theme(),
    );
}

#[test]
fn resuming_draws_the_turns_made_in_the_cli_since_you_left() {
    let mut screen = Screen::new();
    screen.app(AppEvent::TurnsMadeInCli {
        said: "2 turns made in Claude Code since you left".to_owned(),
        turns: vec![
            you("Make the due date optional"),
            agent(vec![wrote(
                "The --due flag is optional now, and list puts undated tasks last.",
            )]),
            you("Run the tests"),
            agent(vec![wrote("All 48 tests pass.")]),
        ],
    });
    let lines = screen.lines(WIDTH);
    assert_matches(
        "made_since_you_left",
        &lines,
        include_str!("../../../../tests/golden/transcript/made_since_you_left.txt"),
        include_str!("../../../../tests/golden/transcript/made_since_you_left.html"),
        &Screen::theme(),
    );
}
