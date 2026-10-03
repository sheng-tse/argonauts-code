mod common;

use std::time::Duration;

use argonauts_tui::HistoryCell;
use argonauts_tui::model::{
    AgentReady, AssistantMessage, ClockTime, CollapsedGroup, DiffCell, DiffView, Notice, Recap,
    SessionHeader, Todo, TodoState, Tone, ToolCall, ToolItem, ToolKind, ToolState, TurnEnd,
    UserMessage,
};
use argonauts_tui::theme::{ColorLevel, Role};
use common::{
    assert_fits, bg, fg, row_styled, row_text, row_with, still, style_of, styled, text, theme,
    theme_at,
};
use ratatui::style::{Color, Modifier};
use ratatui::text::Line;

const WIDTHS: [u16; 2] = [80, 120];

/// A changed doc comment, so its changed words get highlighted.
const CREW_DIFF: &str = "\
@@ -40,3 +40,4 @@
-    /// Writes the rows as CSV.
+    /// Writes the rows as CSV or JSON.
     fn write(&self, out: &mut dyn Write) -> Result<()> {
-        self.write_csv(out)
+        match self.format {
+            Format::Json => self.write_json(out),
";

const FULL_DIFF: &str = "\
@@ -40,4 +40,7 @@
-    /// Writes the rows as CSV.
+    /// Writes the rows as CSV or JSON.
     fn write(&self, out: &mut dyn Write) -> Result<()> {
-        self.write_csv(out)
+        match self.format {
+            Format::Json => self.write_json(out),
+            Format::Csv => self.write_csv(out),
+        }
     }
";

const ANSWER: &str = "\
Here's the plan, with `--json` as the new flag:

- **Parse** the flag in `src/cli.rs`.
- Pass the format to the exporter, then write each row as one JSON object per line, so long exports stream instead of building one huge array.

```bash
cd /tmp/argonauts-try && mkdir -p out
echo \"export ready\" > out/status.txt
```

```rust
fn write(&self, out: &mut dyn Write) -> Result<()> {
    match self.format {
        Format::Json => self.write_json(out),
        Format::Csv => self.write_csv(out),
    }
}
```

Run `/tree` to watch the crew.";

fn snapshot_widths(name: &str, cell: &dyn HistoryCell) {
    let theme = theme();
    for width in WIDTHS {
        let lines = cell.lines(&still(&theme, width));
        assert!(!lines.is_empty(), "{name} rendered nothing at {width}");
        assert_fits(&lines, width);
        insta::assert_snapshot!(format!("{name}_{width}"), text(&lines));
    }
}

fn lines_of(cell: &dyn HistoryCell, width: u16) -> Vec<Line<'static>> {
    cell.lines(&still(&theme(), width))
}

fn diff(unified: &str) -> DiffView {
    DiffView::from_unified(unified).expect("the test diff parses")
}

fn header() -> SessionHeader {
    SessionHeader {
        app: "Argonauts".into(),
        version: "0.0.1".into(),
        default_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        preset: "peer-crew".into(),
        folder: "~/github/argonauts".into(),
        agents: vec![
            ready("Claude Code", true, "2.1.284"),
            ready("Codex", true, "0.158.0"),
            ready("Antigravity CLI", false, "not signed in"),
        ],
        runs_going: 0,
    }
}

fn ready(name: &str, ready: bool, note: &str) -> AgentReady {
    AgentReady {
        name: name.into(),
        ready,
        note: Some(note.into()),
    }
}

fn crew_block() -> ToolCall {
    let mut block = ToolCall::agent(
        "Opus 5.5 on Claude Code",
        None,
        "owner · card-002",
        "Add the JSON writer",
    )
    .with_key("builder");
    block.push(ToolItem::text("Read src/export.rs (212 lines)"));
    let mut update = ToolCall::call(ToolKind::Edit, "Update", "src/export.rs").with_key("update");
    update.push(ToolItem::Diff(diff(CREW_DIFF)));
    update.finish(true);
    block.push_call(update);
    for (key, args) in [("build", "cargo build"), ("test", "cargo test export")] {
        block.push_call(
            ToolCall::call(ToolKind::Shell, "Bash", args)
                .with_key(key)
                .with_state(ToolState::Done),
        );
    }
    block
}

fn group() -> CollapsedGroup {
    let mut group = CollapsedGroup::new();
    for path in ["src/export.rs", "src/cli.rs", "tests/export.rs"] {
        group.push(
            ToolCall::call(ToolKind::Read, "Read", path)
                .with_state(ToolState::Done)
                .with_item(ToolItem::text("Read 212 lines")),
        );
    }
    for command in ["cargo build", "cargo test export"] {
        group.push(
            ToolCall::call(ToolKind::Shell, "Bash", command)
                .with_state(ToolState::Done)
                .with_item(ToolItem::Command {
                    command: command.into(),
                    output: vec!["Finished `dev` profile in 2.41s".into()],
                    hidden: 0,
                }),
        );
    }
    group
}

#[test]
fn session_header() {
    snapshot_widths("session_header", &header());
    let lines = lines_of(&header(), 80);
    let all = text(&lines);
    for word in [
        "Argonauts",
        "v0.0.1",
        "Claude Code · Opus 5.5 with xhigh effort · peer-crew",
        "~/github/argonauts",
        // Only the agent CLI that isn't ready gets a note.
        "Antigravity CLI: not signed in",
    ] {
        assert!(all.contains(word), "the header shows {word}:\n{all}");
    }
    assert!(!all.contains("Codex"), "{all}");
    assert_eq!(style_of(&lines, "▟▙").and_then(|s| s.fg), fg(Role::Accent));
    assert_eq!(
        style_of(&lines, "Antigravity CLI: not signed in").and_then(|s| s.fg),
        fg(Role::Dim)
    );
}

#[test]
fn user_message() {
    snapshot_widths("user_message_short", &UserMessage::new("hi"));
    snapshot_widths(
        "user_message_wrapping",
        &UserMessage::new(
            "Add a --json flag to the export command, keep the CSV output exactly as it is, and \
             make sure the new flag shows up in --help with a one-line description.",
        ),
    );
}

#[test]
fn your_message_sits_on_a_full_width_band() {
    let message = UserMessage::new(
        "Add a --json flag to the export command and keep the CSV output exactly as it is today.",
    );
    let lines = lines_of(&message, 40);
    assert!(lines.len() > 1, "the message wraps at 40 columns");
    for line in &lines {
        assert_eq!(line.style.bg, bg(Role::UserBand), "{}", row_styled(line));
    }
    assert_eq!(text(&lines[..1]).chars().next(), Some('❯'));
    assert_eq!(style_of(&lines, "❯").and_then(|s| s.fg), fg(Role::Faint));
    assert_eq!(
        style_of(&lines, "Add a").and_then(|s| s.fg),
        fg(Role::Bright)
    );
    assert!(
        text(&lines[1..2]).starts_with("  "),
        "wrapped rows hang at column 2"
    );
}

#[test]
fn assistant_message() {
    snapshot_widths("assistant_message", &AssistantMessage::new(ANSWER));
}

#[test]
fn assistant_markdown_is_drawn_not_shown() {
    let lines = lines_of(&AssistantMessage::new(ANSWER), 100);
    let all = text(&lines);
    assert!(all.starts_with("● Here's the plan"), "{all}");
    assert!(!all.contains('`'), "no backticks or fences:\n{all}");
    assert!(!all.contains("**"), "bold markers are gone:\n{all}");
    assert_eq!(
        style_of(&lines, "--json").and_then(|s| s.fg),
        fg(Role::Code)
    );
    let parse = style_of(&lines, "Parse").expect("the bold word");
    assert!(parse.add_modifier.contains(Modifier::BOLD));
    for command in ["cd", "mkdir", "echo"] {
        let row = row_with(&lines, &format!("{command} ")).expect("a bash row");
        let span = row
            .spans
            .iter()
            .find(|s| s.content.trim() == command)
            .unwrap_or_else(|| panic!("{command} is its own span: {}", row_styled(row)));
        assert_eq!(span.style.fg, fg(Role::SyntaxCommand), "{command}");
    }
    assert_eq!(
        style_of(&lines, "\"export ready\"").and_then(|s| s.fg),
        fg(Role::SyntaxString)
    );
    assert!(
        row_with(&lines, "fn write").is_some_and(|row| row_text(row).starts_with("  fn write"))
    );
    let pass = lines
        .iter()
        .position(|l| row_text(l).contains("Pass the format"))
        .unwrap();
    assert!(row_text(&lines[pass + 1]).starts_with("    "), "{all}");
}

#[test]
fn tool_calls() {
    let running = ToolCall::call(ToolKind::Read, "Read", "src/export.rs");
    snapshot_widths("tool_running", &running);

    let done = ToolCall::call(ToolKind::Shell, "Bash", "cargo test -p ui-kit")
        .with_item(ToolItem::Command {
            command: "cargo test -p ui-kit".into(),
            output: vec![
                "running 26 tests".into(),
                "test text::wrap::tests::hangs ... ok".into(),
                "test format::tests::tokens ... ok".into(),
            ],
            hidden: 24,
        })
        .with_state(ToolState::Done);
    snapshot_widths("tool_done", &done);

    let failed = ToolCall::call(ToolKind::Shell, "Bash", "cargo build --release")
        .with_item(ToolItem::Text {
            text: "error[E0308]: mismatched types in src/export.rs:42".into(),
            tone: Tone::Error,
        })
        .with_state(ToolState::Failed);
    snapshot_widths("tool_failed", &failed);

    let long = ToolCall::call(
        ToolKind::Search,
        "Search",
        "rg --files-with-matches \"fn write_json\" crates/argonauts-core/src \
         crates/argonauts-tui/src crates/argonauts-cli/src --glob '!target' --hidden",
    )
    .with_item(ToolItem::text("Found 3 files · /workflows to monitor"))
    .with_state(ToolState::Done);
    snapshot_widths("tool_long_args", &long);

    let described = ToolCall::described(ToolKind::Shell, "Updating build plan status").with_item(
        ToolItem::Command {
            command: "python3 - <<'EOF'".into(),
            output: vec![
                "p = \".github/docs/ROUTE_PLAN.md\"".into(),
                "s = open(p).read()".into(),
                "old = s[s.index(\"## Status\"):]".into(),
            ],
            hidden: 9,
        },
    );
    snapshot_widths("tool_described_command", &described);
}

#[test]
fn a_long_call_hangs_its_args_under_the_open_paren() {
    let call = ToolCall::call(
        ToolKind::Search,
        "Search",
        "rg --files-with-matches \"fn write_json\" crates/argonauts-core/src crates/argonauts-tui/src",
    );
    let lines = lines_of(&call, 60);
    assert!(lines.len() > 1, "{}", text(&lines));
    assert!(text(&lines[..1]).starts_with("● Search(rg"));
    // Args wrap at the column of the paren, as Claude Code wraps them.
    let paren = "● Search".chars().count();
    let next = text(&lines[1..2]);
    assert!(
        next.starts_with(&" ".repeat(paren)) && !next[paren..].starts_with(' '),
        "args continue under `(`:\n{}",
        text(&lines)
    );
    let name = style_of(&lines, "Search").unwrap();
    assert!(
        name.add_modifier.contains(Modifier::BOLD),
        "the name is bold"
    );
}

#[test]
fn the_bullet_color_carries_the_state() {
    let done = ToolCall::call(ToolKind::Read, "Read", "a").with_state(ToolState::Done);
    let failed = ToolCall::call(ToolKind::Read, "Read", "a").with_state(ToolState::Failed);
    let running = ToolCall::call(ToolKind::Read, "Read", "a");
    let bullet = |call: &ToolCall| style_of(&lines_of(call, 80), "●").and_then(|s| s.fg);
    assert_eq!(bullet(&done), fg(Role::Success));
    assert_eq!(bullet(&failed), fg(Role::Error));
    let pending = bullet(&running);
    assert!(
        pending != fg(Role::Success) && pending != fg(Role::Error),
        "a running call is neither green nor red: {pending:?}"
    );

    // Without color, the word says it.
    let plain = theme_at(ColorLevel::None);
    let lines = failed.lines(&still(&plain, 80));
    assert!(text(&lines).contains("failed"), "{}", text(&lines));
    assert!(lines.iter().flat_map(|l| &l.spans).all(|s| {
        !matches!(s.style.fg, Some(Color::Rgb(..))) && !matches!(s.style.bg, Some(Color::Rgb(..)))
    }));
}

#[test]
fn crew_agent_block() {
    snapshot_widths("crew_block", &crew_block());
    let theme = theme();
    for width in WIDTHS {
        let lines = crew_block().lines_within(&still(&theme, width), 6);
        assert!(lines.len() <= 6, "{}", text(&lines));
        assert_fits(&lines, width);
        insta::assert_snapshot!(format!("crew_block_within_6_{width}"), text(&lines));
    }
}

#[test]
fn a_squeezed_block_keeps_its_header() {
    let lines = crew_block().lines_within(&still(&theme(), 80), 6);
    assert!(text(&lines).starts_with("● Opus 5.5 on Claude Code · owner · card-002"));
    let lines = crew_block().lines_within(&still(&theme(), 80), 2);
    assert!(lines.len() <= 2);
    assert!(text(&lines).starts_with("● Opus 5.5 on Claude Code"));
}

#[test]
fn diff_cell() {
    let cell = DiffCell {
        title: "Update(src/export.rs)".into(),
        diff: diff(FULL_DIFF),
    };
    snapshot_widths("diff_cell", &cell);
}

#[test]
fn diff_rows_fill_short_of_the_edge_but_not_their_indent() {
    let view = diff(CREW_DIFF);
    assert_eq!(view.stats(), (3, 2));
    let lines = view.render(&still(&theme(), 80), 8);
    let removed = row_with(&lines, "write_csv").expect("the removed row");
    let added = row_with(&lines, "match self.format").expect("the added row");
    let context = row_with(&lines, "fn write").expect("the context row");
    // The fill runs from the gutter to seven cells short of the edge, as in Claude Code.
    for (row, role) in [(removed, Role::RemovedRow), (added, Role::AddedRow)] {
        assert!(
            row.spans[1..].iter().all(|span| span.style.bg == bg(role)),
            "{}",
            row_styled(row)
        );
        assert_eq!(row.width(), 73, "{}", row_styled(row));
    }
    assert!(
        context
            .spans
            .iter()
            .all(|span| span.style.bg.is_none() || span.style.bg == Some(Color::Reset)),
        "{}",
        styled(&lines)
    );
    let edge = view.render_to_edge(&still(&theme(), 80), 8);
    assert_eq!(row_with(&edge, "write_csv").unwrap().width(), 80);
    for row in [removed, added] {
        let first = &row.spans[0];
        assert!(
            first.content.chars().all(|c| c == ' '),
            "{}",
            row_styled(row)
        );
        assert_eq!(first.style.bg, Some(Color::Reset), "{}", row_styled(row));
    }
    assert!(row_text(removed).trim_start().starts_with("42 -"));
    assert!(row_text(added).trim_start().starts_with("42 +"));
    assert!(row_text(context).trim_start().starts_with("41"));
}

#[test]
fn changed_words_are_brighter() {
    let view = diff(CREW_DIFF);
    let lines = view.render(&still(&theme(), 80), 0);
    let marked = |needle: &str, role: Role| -> String {
        let row = row_with(&lines, needle).unwrap();
        row.spans
            .iter()
            .filter(|span| span.style.bg == bg(role))
            .map(|span| span.content.as_ref())
            .collect()
    };
    assert_eq!(
        marked("as CSV.", Role::RemovedWord).trim(),
        "CSV.",
        "{}",
        styled(&lines)
    );
    assert_eq!(
        marked("CSV or JSON", Role::AddedWord).trim(),
        "CSV or JSON.",
        "{}",
        styled(&lines)
    );
}

#[test]
fn collapsed_group() {
    snapshot_widths("group_collapsed", &group());
    let theme = theme();
    for width in WIDTHS {
        let lines = group().lines(&still(&theme, width).with_expanded(true));
        assert_fits(&lines, width);
        insta::assert_snapshot!(format!("group_expanded_{width}"), text(&lines));
    }
    assert_eq!(group().summary(), "Read 3 files, ran 2 shell commands");
    let lines = lines_of(&group(), 80);
    assert_eq!(
        text(&lines),
        "  Read 3 files, ran 2 shell commands (ctrl+o to expand)"
    );
    assert_eq!(style_of(&lines, "Read").and_then(|s| s.fg), fg(Role::Dim));
}

#[test]
fn a_running_group_speaks_in_the_present() {
    let mut group = CollapsedGroup::new();
    group.push(ToolCall::call(ToolKind::Read, "Read", "a").with_state(ToolState::Done));
    group.push(ToolCall::call(ToolKind::Read, "Read", "b"));
    group.push(ToolCall::call(ToolKind::Search, "Search", "fn write"));
    assert!(group.is_running());
    assert_eq!(group.summary(), "Reading 2 files, searching for 1 pattern…");
}

#[test]
fn to_dos() {
    let call = ToolCall::agent(
        "GPT-5.6 on Codex",
        None,
        "owner · card-003",
        "Wire --json into the CLI",
    )
    .with_item(ToolItem::Todos(vec![
        todo("Read the CLI flags", TodoState::Done),
        todo("Add the --json flag", TodoState::InProgress),
        todo("Pass the format to the exporter", TodoState::Pending),
    ]));
    snapshot_widths("todos", &call);
    let lines = lines_of(&call, 80);
    assert!(
        style_of(&lines, "Add the --json flag")
            .unwrap()
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

fn todo(text: &str, state: TodoState) -> Todo {
    Todo {
        text: text.into(),
        state,
    }
}

#[test]
fn turn_end_recap_and_notices() {
    let turn = TurnEnd::new(
        "Sailed for",
        Duration::from_secs(663),
        ClockTime::new(10, 58),
    );
    snapshot_widths("turn_end", &turn);
    assert_eq!(
        text(&lines_of(&turn, 80)),
        "✻ Sailed for 11m 3s · done 10:58 AM"
    );
    assert!(
        lines_of(&turn, 80)
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| !s.content.trim().is_empty())
            .all(|s| s.style.fg == fg(Role::Dim))
    );

    let recap = Recap::new(
        "Export now takes --json, all 4 cards merged, and the tests pass. Ask for another \
         change, or /resume an older run.",
    );
    snapshot_widths("recap", &recap);
    let lines = lines_of(&recap, 80);
    assert!(text(&lines).starts_with("※ recap: Export now takes --json"));
    assert!(
        text(&lines)
            .trim_end()
            .ends_with("(disable recaps in /config)")
    );
    let body = style_of(&lines, "Export").unwrap();
    assert!(
        body.add_modifier.contains(Modifier::ITALIC),
        "{}",
        styled(&lines)
    );

    let answered = Notice::new(Tone::Warning, "Merge card-002 into main?")
        .with_glyph("◆")
        .with_detail("Merge · you answered");
    snapshot_widths("notice_answered", &answered);
    assert_eq!(
        text(&lines_of(&answered, 80)),
        "◆ Merge card-002 into main?\n  ⎿  Merge · you answered"
    );

    let warning = Notice::new(
        Tone::Warning,
        "codex hit its rate limit, so card-003 waits until 11:20 AM",
    )
    .with_detail("claude-code and agy keep going");
    snapshot_widths("notice_warning", &warning);
    let error = Notice::new(Tone::Error, "agy stopped: not signed in")
        .with_detail("Run `agy login`, then /resume");
    snapshot_widths("notice_error", &error);
    assert_eq!(
        style_of(&lines_of(&error, 80), "●").and_then(|s| s.fg),
        fg(Role::Error)
    );
}

/// A settings file that needs a fix before you can trust it: the key keeps its spelling, the
/// file is named from the repo, and the shape to write sits on lines of its own.
#[test]
fn a_settings_file_to_fix_shows_the_shape_to_write() {
    let notice = Notice::new(
        Tone::Warning,
        ".argonauts/config.local.toml needs a fix before you can trust it: gates.run can't be a \
         list of strings; it takes a list of checks, each a [[gates.run]] table with a name and \
         a command. Write it like this:\n[[gates.run]]\nname = \"python3\"\n\
         command = [\"python3\", \"-m\", \"unittest\", \"-q\"]",
    );
    snapshot_widths("notice_settings_fix", &notice);
    let lines = text(&lines_of(&notice, 80));
    assert!(
        lines.starts_with("● .argonauts/config.local.toml needs a fix"),
        "{lines}"
    );
    let shape: Vec<&str> = lines.lines().rev().take(3).collect();
    assert_eq!(
        shape,
        [
            "  command = [\"python3\", \"-m\", \"unittest\", \"-q\"]",
            "  name = \"python3\"",
            "  [[gates.run]]",
        ]
    );
}

#[test]
fn untrusted_text_is_neutralized() {
    let evil = "\u{1b}]0;pwned\u{7}rm -rf \u{1b}[2J\u{1b}[31mred\u{1b}[0m\u{202e}txt.exe";
    let cells: Vec<Box<dyn HistoryCell>> = vec![
        Box::new(UserMessage::new(evil)),
        Box::new(AssistantMessage::new(evil)),
        Box::new(ToolCall::call(ToolKind::Shell, "Bash", evil).with_item(ToolItem::text(evil))),
        Box::new(Notice::new(Tone::Error, evil).with_detail(evil)),
        Box::new(Recap::new(evil)),
    ];
    for cell in &cells {
        let lines = lines_of(cell.as_ref(), 80);
        assert_fits(&lines, 80);
        let all = text(&lines);
        assert!(
            !all.contains('\u{202e}') && !all.contains("pwned"),
            "{all:?}"
        );
        assert!(all.contains("rm -rf red"), "{all:?}");
    }
}

/// A `!` command as Claude Code shows bash mode: the command on your band behind a pink `!`,
/// `Running… (3s)` until it ends, then its output, folded past a screen.
#[test]
fn shell_command_running_finished_and_folded() {
    use argonauts_tui::model::{ShellEnd, ShellRun};

    let theme = theme();
    let running = ShellRun::new("sleep 25", common::t0());
    for width in WIDTHS {
        let lines = running.lines(&common::ctx(&theme, width, 3.0));
        assert_fits(&lines, width);
        insta::assert_snapshot!(format!("shell_command_running_{width}"), text(&lines));
    }

    let mut done = ShellRun::new("git status --short && echo done", common::t0());
    done.push_output(" M crates/tui/src/cell/shell.rs\n?? notes.txt\ndone\n");
    done.finish(ShellEnd::Exit(0));
    snapshot_widths("shell_command_finished", &done);
    let lines = lines_of(&done, 80);
    assert_eq!(
        style_of(&lines, "!").and_then(|style| style.fg),
        fg(Role::Shell)
    );
    assert_eq!(
        lines[0].style.bg,
        bg(Role::UserBand),
        "the command sits on your band"
    );

    let mut failed = ShellRun::new("cargo test -q", common::t0());
    failed.push_output("error: could not compile `argonauts-tui`\n");
    failed.finish(ShellEnd::Exit(101));
    snapshot_widths("shell_command_failed", &failed);

    let mut long = ShellRun::new("seq 100", common::t0());
    for n in 1..=100 {
        long.push_output(&format!("{n}\n"));
    }
    long.finish(ShellEnd::Exit(0));
    let screen = still(&theme, 80).with_rows(24);
    let folded = long.lines(&screen);
    assert_eq!(folded.len(), 1 + 16 + 1, "{}", text(&folded));
    insta::assert_snapshot!("shell_command_folded_80", text(&folded));
    let all = long.lines(&screen.with_expanded(true));
    assert_eq!(all.len(), 101, "Ctrl+O shows every line");
}
