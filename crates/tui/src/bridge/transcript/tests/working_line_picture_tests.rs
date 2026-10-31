//! The working line drawn from its picture's own example, at the picture's width of 118
//! columns: the task in hand with the next one under it, the same line after twenty seconds
//! without a token, and a command shown while it runs.

use std::time::{Duration, Instant};

use argonauts_core::Verbosity;
use ratatui::text::Line;
use serde_json::json;

use super::live_rows_tests::{Screen, pending, started};
use crate::base::draw::render::{Clock, RenderCtx};
use crate::bottom::{Activity, WorkingLine};
use crate::testing::pictures::picture_check::assert_rows_match;

const WIDTH: u16 = 118;
const TASK: &str = "Add the --json flag to the export command";
const NEXT: &str = "Write the tests, then run them all";

/// How far into the spinner's sweep the picture caught it, at `✶`.
const SPINNER_AT: Duration = Duration::from_millis(750);

/// The line `ran` into its turn, drawn at the moment the picture shows.
fn drawn(ran: Duration, set: impl FnOnce(&mut WorkingLine, Instant)) -> Vec<Line<'static>> {
    let theme = Screen::theme();
    let clock_start = Instant::now();
    let now = clock_start + Duration::from_secs(600) + SPINNER_AT;
    // The spinner's sweep takes 1.86 seconds, so a whole number of them leaves it at `✶`.
    let clock_start = now - (SPINNER_AT + Duration::from_millis(1_860 * 300));
    let mut line = WorkingLine::new("Rowing", now - ran);
    line.task = Some(TASK.to_owned());
    line.tokens_down = 470;
    line.heard_at = Some(now);
    set(&mut line, now);
    line.lines(&RenderCtx::new(WIDTH, &theme, Clock::new(clock_start, now)))
}

fn running_command() -> Vec<Line<'static>> {
    let mut screen = Screen::new();
    screen.settings.verbosity = Verbosity::Normal;
    screen.ask("What changed on main?");
    screen.agent(pending("b1", "Bash"));
    screen.agent(started(
        "b1",
        "Bash",
        json!({"command": "git log --oneline -3 main && git status --short"}),
    ));
    let lines = screen.lines(WIDTH);
    lines[lines.len() - 2..].to_vec()
}

#[test]
fn working_line_the_task_in_hand_the_next_one_and_going_quiet() {
    let mut lines = drawn(Duration::from_secs(44), |line, now| {
        line.next = Some(NEXT.to_owned());
        line.effort = Some("xhigh".to_owned());
        line.activity = Activity::Thinking {
            since: now - Duration::from_secs(3),
        };
    });
    lines.push(Line::default());
    lines.extend(drawn(Duration::from_secs(72), |line, now| {
        line.heard_at = Some(now - Duration::from_secs(20));
    }));
    let picture = (
        include_str!("../../../../tests/golden/transcript/working_line.txt"),
        include_str!("../../../../tests/golden/transcript/working_line.html"),
    );
    // The picture's note about the red line, its row 4, isn't screen.
    assert_rows_match("working_line", &lines, picture, 0..4, &Screen::theme(), &[]);
    let mut command = vec![Line::default()];
    command.extend(running_command());
    assert_rows_match(
        "working_line",
        &command,
        picture,
        5..8,
        &Screen::theme(),
        &[],
    );
}
