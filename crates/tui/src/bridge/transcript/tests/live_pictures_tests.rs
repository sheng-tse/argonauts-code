//! The live rows drawn from the pictures' own example: the money fix in `/work/captain`, at the
//! pictures' width of 118 columns.

use std::time::{Duration, Instant};

use argonauts_core::{AgentEvent, Verbosity};
use ratatui::text::Line;
use serde_json::json;

use super::live_rows_tests::{Screen, finished, pending, started};
use crate::base::draw::render::{Clock, RenderCtx};
use crate::bottom::{Activity, WorkingLine};
use crate::frame::state::Welcome;
use crate::testing::pictures::picture_check::assert_matches;

const WIDTH: u16 = 118;
const FOLDER: &str = "/work/captain";
const PROMPT: &str = "Fix the money rounding so the three failing tests in tests/test_money.py pass. Don't change the tests.";

fn in_folder(screen: &mut Screen) {
    screen.state.welcome = Some(Welcome {
        version: "0.0.1".into(),
        repo: FOLDER.into(),
        workflow: "captain".into(),
        main_agent: None,
        agents: Vec::new(),
        runs_going: 0,
        runs_need_you: 0,
        example_prompt: None,
    });
}

fn file(name: &str) -> String {
    format!("{FOLDER}/{name}")
}

/// The reads and test runs before the edit, folded into one line. Each call's row starts
/// as the model starts writing it, as Claude Code streams it.
fn looked_around(screen: &mut Screen) {
    screen.agent(pending("r1", "Read"));
    screen.agent(started(
        "r1",
        "Read",
        json!({"file_path": file("invoice/money.py")}),
    ));
    screen.agent(finished(
        "r1",
        "1\t\"\"\"Money amounts kept as whole cents.\"\"\"",
    ));
    // Commands that only look around fold with the read; a test run would keep its own row.
    let ls = json!({"command": "ls invoice tests"});
    screen.agent(pending("b1", "Bash"));
    screen.agent(started("b1", "Bash", ls));
    screen.agent(finished(
        "b1",
        "invoice:\nmoney.py\n\ntests:\ntest_money.py",
    ));
    let cat = json!({"command": "cat tests/test_money.py | head -40"});
    screen.agent(pending("b2", "Bash"));
    screen.agent(started("b2", "Bash", cat));
    screen.agent(finished("b2", "import unittest"));
}

fn edit_input() -> serde_json::Value {
    json!({
        "file_path": file("invoice/money.py"),
        "old_string": "    \"\"\"'12.50' or '$12.50' to 1250 cents.\"\"\"\n",
        "new_string": "    \"\"\"'12.50', '12.5', '$12.50' or '-3.25' to cents.\"\"\"\n",
    })
}

/// The turn's working line as the bottom draws it 9 seconds in, the edit a second into its run.
fn working_line() -> Line<'static> {
    let theme = Screen::theme();
    let start = Instant::now();
    let now = start + Duration::from_secs(9);
    let mut line = WorkingLine::new("Sailing", start);
    line.tokens_down = 2_100;
    line.activity = Activity::RunningTool {
        since: now - Duration::from_secs(1),
    };
    line.hide_while_text_streams = true;
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::new(start, now));
    line.line(&ctx)
}

#[test]
fn live_moment_the_edit_s_row_is_there_while_it_runs() {
    let mut screen = Screen::new();
    screen.settings.verbosity = Verbosity::Normal;
    in_folder(&mut screen);
    screen.ask(PROMPT);
    looked_around(&mut screen);
    screen.agent(pending("e1", "Edit"));
    screen.agent(started("e1", "Edit", edit_input()));
    let mut lines = screen.lines(WIDTH);
    lines.push(Line::default());
    lines.push(working_line());
    assert_matches(
        "live_moment",
        &lines,
        include_str!("../../../../tests/golden/transcript/live_moment.txt"),
        include_str!("../../../../tests/golden/transcript/live_moment.html"),
        &Screen::theme(),
        &[],
    );
}

#[test]
fn live_done_the_edit_s_row_is_filled_in_place() {
    let mut screen = Screen::new();
    screen.settings.verbosity = Verbosity::Normal;
    in_folder(&mut screen);
    screen.ask(PROMPT);
    looked_around(&mut screen);
    screen.agent(pending("e1", "Edit"));
    screen.agent(started("e1", "Edit", edit_input()));
    let before = screen.lines(WIDTH).len() - 2;
    screen.agent(AgentEvent::FilePatch {
        id: "e1".into(),
        path: file("invoice/money.py"),
        hunks: PATCH.into(),
        parent: None,
    });
    screen.agent(finished("e1", "The file has been updated successfully."));
    screen.agent(pending("b3", "Bash"));
    screen.agent(started(
        "b3",
        "Bash",
        json!({"command": "python3 -m unittest -q"}),
    ));
    screen.agent(finished("b3", "Ran 8 tests in 0.002s · OK"));
    let mut shown: Vec<Line<'static>> = screen.lines(WIDTH).split_off(before);
    // The picture stops at the gap after the first change; the screen goes on with the next
    // one, as Claude Code draws every change of a patch.
    let next_change = shown
        .drain(6..8)
        .map(|line| line.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        next_change,
        [
            "      8 -    return int(dollars) * 100 + int(cents or 0)",
            "      8 +    return sign * (int(dollars) * 100 + int(cents))",
        ]
        .map(|row| format!("{row:<111}"))
    );
    assert_matches(
        "live_done",
        &shown,
        include_str!("../../../../tests/golden/transcript/live_done.txt"),
        include_str!("../../../../tests/golden/transcript/live_done.html"),
        &Screen::theme(),
        &[],
    );
}

/// The edit's patch as Claude Code reports it: two changes, each with the line before it.
const PATCH: &str = "@@ -4,2 +4,2 @@\n def parse(text):\n-    \"\"\"'12.50' or '$12.50' to 1250 cents.\"\"\"\n+    \"\"\"'12.50', '12.5', '$12.50' or '-3.25' to cents.\"\"\"\n@@ -8,1 +8,1 @@\n-    return int(dollars) * 100 + int(cents or 0)\n+    return sign * (int(dollars) * 100 + int(cents))\n";

/// Your prompt as the sticky_after picture starts it; the rest of it is this test's own.
const RATE_PROMPT: &str = "Make the rate limiter per API key: each X-Api-Key gets its own bucket, requests \
     without it share one. Cap the burst at 20 requests a key.\n\nKeep the tests green.";

/// The full screen scrolled up past that prompt into a reviewer's reply, as the picture
/// shows it: the rows between the prompt and the hint are the reply, drawn here as plain rows.
fn scrolled_past_a_prompt() -> (crate::app::Scrollback, crate::base::theme::Theme) {
    use crate::cells::cell::UserMessage;
    use crate::cells::cell::history::History;
    use ratatui::style::Modifier;
    use ratatui::text::Span;

    let theme = Screen::theme();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let mut scrollback = crate::app::Scrollback::new();
    let mut history = History::default();
    history.push(UserMessage::new(RATE_PROMPT));
    let rows = history.drain_printable(&ctx);
    scrollback.mark_prompts(history.take_prompt_rows());
    scrollback.push(rows, None);

    let dim = theme.style(crate::base::theme::Role::Dim);
    let hook = || Span::styled("  ⎿  ", dim);
    let mut reply: Vec<Line<'static>> = (1..=12)
        .map(|n| Line::from(Span::styled(format!("  row {n} of the work"), dim)))
        .collect();
    reply.extend([
        Line::from(vec![
            hook(),
            Span::styled("Done (3 tool uses · 46.2k tokens · 13s) · report ✓", dim),
        ]),
        Line::default(),
        Line::from(vec![
            Span::styled("● ", theme.style(crate::base::theme::Role::AgentPurple)),
            Span::styled(
                "final:per-key",
                theme
                    .style(crate::base::theme::Role::Text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · Haiku 4.5 on Claude Code", dim),
        ]),
        Line::from(vec![
            hook(),
            Span::styled(
                "Searched for 1 pattern, read 1 file (ctrl+o to expand)",
                dim,
            ),
        ]),
        Line::from(
            "     Per-key buckets, the shared no-key bucket and the refill clamp all check out; \
             the tests passed in the checks.",
        ),
    ]);
    reply.extend((1..=6).map(|n| Line::from(format!("  more {n}"))));
    scrollback.push(reply, None);
    (scrollback, theme)
}

#[test]
fn sticky_after_the_prompt_being_answered_stays_at_the_top() {
    let (mut scrollback, theme) = scrolled_past_a_prompt();
    let ctx = RenderCtx::new(WIDTH, &theme, Clock::still(Instant::now()));
    let rows = 9;
    assert!(
        !scrollback
            .window(&[], rows, &ctx)
            .iter()
            .any(|line| line.to_string().contains("Make the rate")),
        "nothing stays at the top at the bottom"
    );
    // Up to where the view starts with the reply's last rows, the prompt above it.
    scrollback.scroll(4);
    let lines = scrollback.window(&[], rows, &ctx);
    assert_matches(
        "sticky_after",
        &lines,
        include_str!("../../../../tests/golden/transcript/sticky_after.txt"),
        include_str!("../../../../tests/golden/transcript/sticky_after.html"),
        &theme,
        &[],
    );
}
