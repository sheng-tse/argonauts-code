use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

use super::*;
use crate::cell::{CollapsedGroup, DiffView, HistoryCell, Tone};
use crate::render::{Clock, RenderCtx};
use crate::text::{plain_text, spans_width};
use crate::theme::{ColorLevel, Glyphs, Role, Theme};

fn theme() -> Theme {
    let mut theme = Theme::dark();
    theme.glyphs = Glyphs::UNICODE;
    theme
}

fn ctx(theme: &Theme, width: u16) -> RenderCtx<'_> {
    RenderCtx::new(width, theme, Clock::still(Instant::now()))
}

fn text(lines: &[Line<'_>]) -> String {
    plain_text(lines)
}

fn column_of(lines: &[Line<'_>], needle: &str) -> usize {
    for line in lines {
        let row: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        if let Some(at) = row.find(needle) {
            return crate::text::str_width(&row[..at]);
        }
    }
    panic!("{needle:?} not found in\n{}", plain_text(lines));
}

fn session() -> ToolCall {
    let mut session = ToolCall::agent(
        "Opus 5.5 on Claude Code",
        None,
        "owner · card-002",
        "Add the JSON writer",
    );
    session.push_call(
        ToolCall::call(ToolKind::Read, "Read", "src/export.rs").with_state(ToolState::Done),
    );
    let diff = DiffView::from_unified(
        "@@ -41,2 +41,3 @@\n     fn write(&self) -> Result<()> {\n-        self.write_csv(out)\n+        match self.format {\n+            Format::Json => self.write_json(out),\n",
    )
    .expect("diff");
    session.push_call(
        ToolCall::call(ToolKind::Edit, "Update", "src/export.rs")
            .with_key("edit-1")
            .with_state(ToolState::Done)
            .with_item(ToolItem::Diff(diff)),
    );
    session.push_call(ToolCall::call(ToolKind::Shell, "Bash", "cargo test").with_key("sh-1"));
    session.push_call(ToolCall::call(ToolKind::Shell, "Bash", "cargo clippy").with_key("sh-2"));
    session
}

#[test]
fn a_call_reads_like_claude_code() {
    let theme = theme();
    let call = ToolCall::call(
        ToolKind::Agent,
        "Workflow",
        "Add a --json flag to the export command, then cover it with tests",
    )
    .with_state(ToolState::Done)
    .with_item(ToolItem::text(
        "Running in background · /workflows to monitor (/config)",
    ));
    let lines = call.lines(&ctx(&theme, 60));
    assert_eq!(
        text(&lines),
        "● Workflow(Add a --json flag to the export command, then\n          cover it with tests)\n  ⎿  Running in background · /workflows to monitor (/config)"
    );
    assert_eq!(
        lines[0].spans[0].style,
        crate::text::marks::decoration(theme.style(Role::Success))
    );
    assert!(
        lines[0].spans[2]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(column_of(&lines, "Add"), 11);
    assert_eq!(column_of(&lines, "cover it"), 10);
    assert_eq!(column_of(&lines, "Running"), 5);
    assert_eq!(column_of(&lines, "⎿"), 2);
    let result = &lines[2];
    let styled = |needle: &str| {
        result
            .spans
            .iter()
            .find(|span| span.content.contains(needle))
            .map(|span| span.style)
    };
    assert_eq!(styled("/workflows"), Some(theme.style(Role::Code)));
    assert_eq!(styled("/config"), Some(theme.style(Role::Code)));
    assert_eq!(styled("Running"), Some(theme.style(Role::Dim)));
}

#[test]
fn the_bullet_carries_the_state() {
    let theme = theme();
    let still = ctx(&theme, 40);
    let bullet = |call: &ToolCall, ctx: &RenderCtx<'_>| {
        let span = call.lines(ctx)[0].spans[0].clone();
        assert_eq!(
            span.style,
            crate::text::marks::decoration(span.style),
            "the bullet only decorates"
        );
        (
            span.content.into_owned(),
            crate::text::marks::unmarked(span.style),
        )
    };
    let dim = theme.style(Role::Dim);
    let running = ToolCall::call(ToolKind::Shell, "Bash", "ls");
    assert_eq!(bullet(&running, &still), ("●".into(), dim));
    fn at(theme: &Theme, ms: u64) -> RenderCtx<'_> {
        let start = Instant::now();
        RenderCtx::new(
            40,
            theme,
            Clock::new(start, start + Duration::from_millis(ms)),
        )
    }
    assert_eq!(bullet(&running, &at(&theme, 700)), (" ".into(), dim));
    assert_eq!(bullet(&running, &at(&theme, 1300)), ("●".into(), dim));
    let mut calm = theme.clone();
    calm.reduced_motion = true;
    assert_eq!(bullet(&running, &at(&calm, 700)), ("●".into(), dim));
    assert!(running.is_animating());
    let failed = running.clone().with_state(ToolState::Failed);
    assert_eq!(
        bullet(&failed, &at(&theme, 700)),
        ("●".into(), theme.style(Role::Error))
    );
    assert!(!failed.is_animating());
    let stopped = running.with_state(ToolState::Stopped);
    assert_eq!(
        bullet(&stopped, &at(&theme, 700)),
        ("●".into(), theme.style(Role::Warning))
    );
    assert!(!stopped.is_animating());
}

#[test]
fn stopping_a_call_stops_what_runs_inside_it() {
    let theme = theme();
    let mut session = ToolCall::agent(
        "GPT-5.6 on Codex",
        None,
        "owner · card-003",
        "Wire --json into the CLI",
    );
    session.push_call(ToolCall::call(ToolKind::Shell, "Bash", "cargo test"));
    let mut done = ToolCall::call(ToolKind::Edit, "Update", "src/cli.rs");
    done.finish(true);
    session.push_call(done);
    session.stop();
    assert_eq!(session.state, ToolState::Stopped);
    let states: Vec<ToolState> = session
        .body
        .iter()
        .flat_map(|item| match item {
            ToolItem::Call(call) => vec![call.state],
            ToolItem::Group(group) => group.calls.iter().map(|call| call.state).collect(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(states, [ToolState::Stopped, ToolState::Done]);
    let plain = theme.with_level(ColorLevel::None);
    let header = text(&session.lines(&ctx(&plain, 80)));
    assert!(
        header.starts_with(
            "● GPT-5.6 on Codex · owner · card-003 (Wire --json into the CLI) stopped"
        ),
        "{header}"
    );
}

#[test]
fn a_waiting_call_counts_up_until_output_arrives() {
    let theme = theme();
    let start = Instant::now();
    let mut call = ToolCall::call(ToolKind::Shell, "Bash", "cargo build --release");
    call.push(ToolItem::Running {
        since: start,
        background: true,
    });
    let at = |call: &ToolCall, secs| {
        let ctx = RenderCtx::new(
            60,
            &theme,
            Clock::new(start, start + Duration::from_secs(secs)),
        );
        text(&call.lines(&ctx))
    };
    assert_eq!(
        at(&call, 4),
        "● Bash(cargo build --release)\n  ⎿  Running… (4s)\n     (ctrl+b to run in background)"
    );
    assert!(at(&call, 65).contains("Running… (1m 5s)"));
    call.push(ToolItem::text("Compiling ui-kit"));
    assert_eq!(
        at(&call, 10),
        "● Bash(cargo build --release)\n  ⎿  Compiling ui-kit"
    );
}

#[test]
fn without_color_a_failure_says_so() {
    let theme = theme().with_level(ColorLevel::None);
    let failed =
        ToolCall::call(ToolKind::Shell, "Bash", "cargo test").with_state(ToolState::Failed);
    assert_eq!(
        text(&failed.lines(&ctx(&theme, 40))),
        "● Bash(cargo test) failed"
    );
    let colored = self::theme();
    assert_eq!(
        text(&failed.lines(&ctx(&colored, 40))),
        "● Bash(cargo test)"
    );
}

#[test]
fn a_shell_command_shows_its_first_lines() {
    let theme = theme();
    let call =
        ToolCall::described(ToolKind::Shell, "Running the tests").with_item(ToolItem::Command {
            command: "cargo test".into(),
            output: (1..=8).map(|i| format!("test case_{i} ... ok")).collect(),
            hidden: 2,
        });
    let lines = call.lines(&ctx(&theme, 60));
    assert_eq!(
        text(&lines),
        "● Running the tests\n  ⎿  $ cargo test\n     test case_1 ... ok\n     test case_2 ... ok\n     test case_3 ... ok\n     test case_4 ... ok\n     test case_5 ... ok\n     … +5 lines (ctrl+o to expand)"
    );
    assert_eq!(lines[1].spans.last().unwrap().style, theme.style(Role::Dim));
    assert_eq!(lines[2].spans[1].style, theme.style(Role::Dim));
    assert_eq!(lines[7].spans[1].style, theme.style(Role::Dim));
    let expanded = call.lines(&ctx(&theme, 60).with_expanded(true));
    assert_eq!(expanded.len(), 2 + 8 + 1);
    assert!(text(&expanded).ends_with("test case_8 ... ok\n     … +2 lines"));
}

#[test]
fn long_commands_are_cut() {
    let theme = theme();
    let script: Vec<String> = (1..=14).map(|i| format!("line {i}")).collect();
    let call = ToolCall::described(ToolKind::Shell, "Script").with_item(ToolItem::Command {
        command: format!("python3 - <<'EOF'\n{}", script.join("\n")),
        output: Vec::new(),
        hidden: 0,
    });
    let lines = call.lines(&ctx(&theme, 60));
    assert_eq!(lines.len(), 1 + 10);
    assert_eq!(text(&lines[10..]), "     line 9…");
}

#[test]
fn nested_calls_sit_three_columns_further_in() {
    let theme = theme();
    let lines = session().lines(&ctx(&theme, 80));
    assert_eq!(
        text(&lines),
        "● Opus 5.5 on Claude Code · owner · card-002 (Add the JSON writer)\n  ⎿  Read 1 file (ctrl+o to expand)\n     Update(src/export.rs)\n        Added 2 lines, removed 1 line\n         41      fn write(&self) -> Result<()> {\n         42 -        self.write_csv(out)\n         42 +        match self.format {\n         43 +            Format::Json => self.write_json(out),\n     Running 2 shell commands…\n        $ cargo clippy"
    );
    assert_eq!(column_of(&lines, "Update("), 5);
    assert_eq!(column_of(&lines, "Added 2 lines"), 8);
    assert_eq!(column_of(&lines, "41"), 9);
    // One hook per block, on its first body row.
    assert_eq!(text(&lines).matches('⎿').count(), 1);
    let update = lines
        .iter()
        .find(|line| text(std::slice::from_ref(line)).contains("Update("))
        .expect("row");
    let name = update
        .spans
        .iter()
        .find(|span| span.content == "Update")
        .expect("name");
    assert!(name.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn nested_names_show_their_state() {
    let theme = theme();
    let mut parent = ToolCall::described(ToolKind::Agent, "agent");
    parent.push_call(ToolCall::call(ToolKind::Edit, "Update", "a.rs"));
    parent
        .push_call(ToolCall::call(ToolKind::Edit, "Update", "b.rs").with_state(ToolState::Failed));
    parent.push_call(ToolCall::call(ToolKind::Edit, "Update", "c.rs").with_state(ToolState::Done));
    let lines = parent.lines(&ctx(&theme, 40));
    let name = |row: usize| {
        lines[row]
            .spans
            .iter()
            .find(|s| s.content == "Update")
            .map(|s| s.style)
    };
    let bold = |role| Some(theme.style(role).add_modifier(Modifier::BOLD));
    assert_eq!(name(1), bold(Role::Dim));
    assert_eq!(name(2), bold(Role::Error));
    assert_eq!(name(3), bold(Role::Text));
}

#[test]
fn quiet_calls_fold_into_a_trailing_group() {
    let session = session();
    let kinds: Vec<&str> = session
        .body
        .iter()
        .map(|item| match item {
            ToolItem::Group(_) => "group",
            ToolItem::Call(_) => "call",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["group", "call", "group"]);
    let ToolItem::Group(group) = &session.body[2] else {
        panic!("no group")
    };
    assert_eq!(group.calls.len(), 2);
}

#[test]
fn children_are_found_by_key_inside_groups() {
    let mut session = session();
    session.child_mut("sh-2").expect("in a group").finish(true);
    session.child_mut("edit-1").expect("nested").finish(false);
    assert!(session.child_mut("nope").is_none());
    let ToolItem::Group(group) = &session.body[2] else {
        panic!("no group")
    };
    assert_eq!(group.calls[1].state, ToolState::Done);
    let ToolItem::Call(edit) = &session.body[1] else {
        panic!("no call")
    };
    assert_eq!(edit.state, ToolState::Failed);
    let mut outer = ToolCall::described(ToolKind::Agent, "outer");
    outer.push_call(session);
    assert!(outer.child_mut("sh-1").is_some());
}

#[test]
fn a_finished_session_folds_into_one_line() {
    let theme = theme();
    let mut session = session();
    session.finish(true);
    let summary = session.summarized(Some("18.1k tokens · 4m 12s"));
    assert_eq!(
        text(&summary.lines(&ctx(&theme, 80))),
        "● Opus 5.5 on Claude Code · owner · card-002 (Add the JSON writer)\n  ⎿  Done · 4 tool uses · 18.1k tokens · 4m 12s"
    );
    let failed = ToolCall::described(ToolKind::Agent, "x")
        .with_state(ToolState::Failed)
        .with_item(ToolItem::Call(ToolCall::call(ToolKind::Other, "A", "")))
        .summarized(None);
    assert_eq!(failed.body, vec![ToolItem::text("Failed · 1 tool use")]);
}

#[test]
fn todos_show_their_state() {
    let theme = theme();
    let call = ToolCall::described(ToolKind::Agent, "agent").with_item(ToolItem::Todos(vec![
        Todo {
            text: "Read".into(),
            state: TodoState::Done,
        },
        Todo {
            text: "Write the JSON writer".into(),
            state: TodoState::InProgress,
        },
        Todo {
            text: "Test".into(),
            state: TodoState::Pending,
        },
    ]));
    let lines = call.lines(&ctx(&theme, 40));
    assert_eq!(
        text(&lines),
        "● agent\n  ⎿  ☒ Read\n     ◧ Write the JSON writer\n     ☐ Test"
    );
    let done = lines[1]
        .spans
        .iter()
        .find(|s| s.content == "Read")
        .expect("span")
        .style;
    assert!(done.add_modifier.contains(Modifier::CROSSED_OUT));
    let active = lines[2]
        .spans
        .iter()
        .find(|s| s.content.contains("Write"))
        .expect("span")
        .style;
    assert!(active.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn a_diff_first_in_the_body_hangs_under_the_hook_outside_the_fill() {
    let theme = theme();
    let call = ToolCall::call(ToolKind::Edit, "Update", "a.rs")
        .with_state(ToolState::Done)
        .with_item(ToolItem::Diff(DiffView::between(
            "one\ntwo\n",
            "one\nthree\n",
            1,
        )));
    let lines = call.lines(&ctx(&theme, 40));
    assert_eq!(
        text(&lines),
        "● Update(a.rs)\n  ⎿  Added 1 line, removed 1 line\n      1  one\n      2 -two\n      2 +three"
    );
    assert!(
        lines[1]
            .spans
            .iter()
            .take(3)
            .all(|span| span.style.bg == Some(Color::Reset))
    );
    assert!(
        lines[2]
            .spans
            .iter()
            .all(|span| span.style.bg != theme.style(Role::RemovedRow).bg)
    );
    assert_eq!(lines[3].spans[1].style.bg, theme.style(Role::RemovedRow).bg);
    let removed_first = &lines[3].spans[0];
    assert_eq!(removed_first.style.bg, Some(Color::Reset));
}

#[test]
fn escapes_in_tool_output_never_reach_a_line() {
    let theme = theme();
    let call = ToolCall::call(ToolKind::Shell, "Bash\x1b[31m", "ls\x1b]0;x\x07")
        .with_item(ToolItem::Command {
            command: "ls \x1b[2J".into(),
            output: vec!["\x1b[1;32mgreen\x1b[0m\r\nnext\x07".into()],
            hidden: 0,
        })
        .with_item(ToolItem::Text {
            text: "done\x1b[H".into(),
            tone: Tone::Success,
        })
        .with_item(ToolItem::Markdown("**bold**\x1b[5m".into()));
    let lines = call.lines(&ctx(&theme, 60));
    for line in &lines {
        for span in &line.spans {
            assert!(!span.content.chars().any(|c| c.is_control()), "{span:?}");
        }
    }
    assert_eq!(
        text(&lines),
        "● Bash(ls)\n  ⎿  $ ls\n     green\n     next\n     done\n     bold"
    );
}

#[test]
fn squeezing_keeps_what_matters_and_never_exceeds_the_budget() {
    let theme = theme();
    let ctx = ctx(&theme, 80);
    let mut call = session();
    call.push(ToolItem::Markdown(
        "I'll add the writer now.\n\nThen the flag, and then the tests that cover both.".into(),
    ));
    call.push(ToolItem::Todos(vec![
        Todo {
            text: "Read".into(),
            state: TodoState::Done,
        },
        Todo {
            text: "Writing the JSON writer".into(),
            state: TodoState::InProgress,
        },
        Todo {
            text: "Tests".into(),
            state: TodoState::Pending,
        },
    ]));
    let full = call.lines(&ctx);
    assert_eq!(call.lines_within(&ctx, full.len()), full);
    for budget in 0..full.len() {
        let squeezed = call.lines_within(&ctx, budget);
        assert!(squeezed.len() <= budget, "{budget}: {}", text(&squeezed));
        for line in &squeezed {
            assert!(spans_width(&line.spans) <= 80);
        }
    }
    let six = call.lines_within(&ctx, 6);
    assert_eq!(
        text(&six),
        "● Opus 5.5 on Claude Code · owner · card-002 (Add the JSON writer)\n  ⎿  … +9 lines (ctrl+o to expand)\n     Running 2 shell commands…\n        $ cargo clippy\n     Then the flag, and then the tests that cover both.\n     ◧ Writing the JSON writer · 1/3 done"
    );
    let two = call.lines_within(&ctx, 2);
    assert_eq!(
        text(&two),
        "● Opus 5.5 on Claude Code · owner · card-002 (Add the JSON writer)\n  ⎿  ◧ Writing the JSON writer · 1/3 done"
    );
    assert_eq!(call.lines_within(&ctx, 1).len(), 1);
}

#[test]
fn the_crew_at_80_columns() {
    let theme = theme();
    let mut session = session();
    session.child_mut("sh-1").expect("sh").finish(true);
    session.child_mut("sh-2").expect("sh").finish(true);
    let goal = ToolCall::call(ToolKind::Other, "Goal", "Add a --json flag to export")
        .with_state(ToolState::Done)
        .with_item(ToolItem::text("Agreed by 3 agents · 1 question answered"));
    let mut rows = goal.lines(&ctx(&theme, 80));
    rows.push(Line::default());
    rows.extend(session.lines(&ctx(&theme, 80)));
    insta::assert_snapshot!(text(&rows));
}

#[test]
fn groups_expand_in_the_overlay() {
    let theme = theme();
    let mut group = CollapsedGroup::new();
    group.push(
        ToolCall::call(ToolKind::Read, "Read", "a.rs")
            .with_state(ToolState::Done)
            .with_item(ToolItem::text("Read 12 lines")),
    );
    let call = ToolCall::described(ToolKind::Agent, "agent").with_item(ToolItem::Group(group));
    assert_eq!(
        text(&call.lines(&ctx(&theme, 40))),
        "● agent\n  ⎿  Read 1 file (ctrl+o to expand)"
    );
    assert_eq!(
        text(&call.lines(&ctx(&theme, 40).with_expanded(true))),
        "● agent\n  ⎿  Read(a.rs)\n        Read 12 lines"
    );
}

#[test]
fn a_summarized_session_prints_one_line_and_shows_whole_in_the_overlay() {
    let theme = theme();
    let session = ToolCall::agent(
        "GPT-5.6 on Codex",
        None,
        "owner · card-003",
        "Wire --json into the CLI",
    )
    .with_stamp("10:58 AM codex/gpt-5.6")
    .with_item(ToolItem::text("Read src/cli.rs (96 lines)"))
    .with_state(ToolState::Done);
    let summary = session.summarized(Some("12.9k tokens · 3m 41s"));
    assert_eq!(
        text(&summary.lines(&ctx(&theme, 80))),
        "● GPT-5.6 on Codex · owner · card-003 (Wire --json into the CLI)\n  ⎿  Done · 0 tool uses · 12.9k tokens · 3m 41s"
    );
    let overlay = ctx(&theme, 80).with_expanded(true);
    let whole = text(&summary.lines(&overlay));
    assert_eq!(whole, text(&session.lines(&overlay)));
    assert!(whole.contains("Read src/cli.rs (96 lines)"), "{whole}");
    let stamp = whole.lines().next().unwrap();
    assert!(stamp.ends_with("10:58 AM codex/gpt-5.6"), "{stamp:?}");
    assert_eq!(stamp.chars().count(), 73);
}

#[test]
fn agent_headers_side_by_side_cut_their_tasks_alike() {
    let theme = theme();
    let start = Instant::now();
    let header = |name: &str, task: &str, width: u16| {
        let call = ToolCall::agent(name, None, "owner · card-002", task).with_stats(ToolStats {
            started_at: Some(start),
            ended_at: None,
            tokens_down: 18_600,
        });
        let ctx = RenderCtx::new(
            width,
            &theme,
            Clock::new(start, start + Duration::from_secs(258)),
        );
        text(&call.lines(&ctx))
    };
    let both = |width| {
        [
            header("Opus 5.5 on Claude Code", "Add the JSON writer", width),
            header("GPT-5.6 on Codex", "Wire --json into the CLI", width),
        ]
    };
    for row in both(62) {
        assert!(!row.contains('('), "{row}");
        assert!(row.ends_with("· 4m18s · ↓ 18.6k"), "{row}");
    }
    for row in both(78) {
        assert!(row.contains('(') && row.contains('…'), "{row}");
        assert!(row.ends_with("· 4m18s · ↓ 18.6k"), "{row}");
    }
}

#[test]
fn a_retry_counts_down_and_gives_way_before_the_stats() {
    let mut theme = theme();
    theme.reduced_motion = true;
    let start = Instant::now();
    let mut call = ToolCall::agent(
        "Opus 5.5 on Claude Code",
        None,
        "owner · card-003",
        "Middleware",
    )
    .with_stats(ToolStats {
        started_at: Some(start),
        ended_at: None,
        tokens_down: 18_100,
    });
    call.retrying(Some(start + Duration::from_secs(264)), 2);
    let row = |width, seconds| {
        let clock = Clock::new(start, start + Duration::from_secs(seconds));
        plain_text(&call.header(&RenderCtx::new(width, &theme, clock)))
    };
    assert_eq!(
        row(120, 252),
        "● Opus 5.5 on Claude Code · owner · card-003 (Middleware) · 4m12s · ↓ 18.1k · retrying in 12s (attempt 2)"
    );
    assert_eq!(
        row(120, 260),
        "● Opus 5.5 on Claude Code · owner · card-003 (Middleware) · 4m20s · ↓ 18.1k · retrying in 4s (attempt 2)"
    );
    assert!(row(120, 300).ends_with("· 5m0s · ↓ 18.1k · retrying (attempt 2)"));
    assert_eq!(
        row(82, 252),
        "● Opus 5.5 on Claude Code · owner · card-003 · 4m12s · ↓ 18.1k · retrying in 12s"
    );
    // The warning outlasts the time and tokens.
    assert_eq!(
        row(62, 252),
        "● Opus 5.5 on Claude Code · owner · card-003 · retrying in 12s"
    );
}

#[test]
fn a_quiet_spell_shows_after_a_while_and_warns_later() {
    let mut theme = theme();
    theme.reduced_motion = true;
    let start = Instant::now();
    let mut call = ToolCall::agent("Antigravity CLI", None, "owner", "");
    call.quiet_since(start);
    let at = |seconds| {
        RenderCtx::new(
            100,
            &theme,
            Clock::new(start, start + Duration::from_secs(seconds)),
        )
    };
    assert_eq!(
        plain_text(&call.header(&at(20))),
        "● Antigravity CLI · owner"
    );
    let lines = call.header(&at(45));
    assert_eq!(
        plain_text(&lines),
        "● Antigravity CLI · owner · quiet 45s · may be waiting on a limit"
    );
    assert_eq!(lines[0].spans.last().unwrap().style, theme.style(Role::Dim));
    let lines = call.header(&at(150));
    assert_eq!(
        plain_text(&lines),
        "● Antigravity CLI · owner · quiet 2m · may be waiting on a limit"
    );
    assert_eq!(
        lines[0].spans.last().unwrap().style,
        theme.style(Role::Warning)
    );
}

#[test]
fn the_todo_line_keeps_the_item_in_progress_in_view() {
    let theme = theme();
    let todo = |text: &str, state| Todo {
        text: text.into(),
        state,
    };
    let todos = vec![
        todo("read the router", TodoState::Done),
        todo("wire middleware", TodoState::Done),
        todo("tests", TodoState::InProgress),
        todo("docs", TodoState::Pending),
    ];
    let call = ToolCall::described(ToolKind::Agent, "agent").with_item(ToolItem::TodoLine(todos));
    let lines = call.lines(&ctx(&theme, 80));
    assert_eq!(
        text(&lines),
        "● agent\n  ⎿  1 done · ☒ wire middleware  ☐ tests  ☐ docs"
    );
    let tests = lines[1]
        .spans
        .iter()
        .find(|span| span.content == "tests")
        .unwrap();
    assert!(tests.style.add_modifier.contains(Modifier::BOLD));
    let narrow = call.lines(&ctx(&theme, 30));
    assert_eq!(text(&narrow), "● agent\n  ⎿  1 done · ☒ wire middlewa…");
}

#[test]
fn streaming_text_shows_its_newest_row_with_a_cursor() {
    let theme = theme();
    let call = ToolCall::described(ToolKind::Agent, "agent").with_item(ToolItem::Streaming(
        "First the router. Then the middleware that checks the token on every request".into(),
    ));
    assert_eq!(
        text(&call.lines(&ctx(&theme, 40))),
        "● agent\n  ⎿  every request▎"
    );
    let empty =
        ToolCall::described(ToolKind::Agent, "agent").with_item(ToolItem::Streaming(String::new()));
    assert_eq!(text(&empty.lines(&ctx(&theme, 40))), "● agent\n  ⎿  ▎");
}
