//! Prints sample transcript cells with ANSI colors, without the inline terminal.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use argonauts_tui::cell::{
    AgentReady, AssistantMessage, CollapsedGroup, DiffView, Notice, Recap, SessionHeader, Todo,
    TodoState, Tone, ToolCall, ToolItem, ToolKind, ToolState, ToolStats, TurnEnd, UserMessage,
};
use argonauts_tui::format::ClockTime;
use argonauts_tui::render::Clock;
use argonauts_tui::theme::{ColorLevel, Glyphs};
use argonauts_tui::{History, RenderCtx, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let width = args
        .iter()
        .position(|arg| arg == "--width")
        .and_then(|i| args.get(i + 1))
        .and_then(|value| value.parse().ok())
        .or_else(|| crossterm::terminal::size().ok().map(|(cols, _)| cols))
        .unwrap_or(100);
    let mut theme = if flag("--light") {
        Theme::light()
    } else {
        Theme::dark()
    };
    if flag("--no-color") {
        theme = theme.with_level(ColorLevel::None);
    }
    if flag("--ascii") {
        theme.glyphs = Glyphs::ASCII;
    }

    let now = Instant::now();
    let ctx = RenderCtx::new(width, &theme, Clock::still(now));
    let mut history = History::new();
    for cell in samples(now) {
        history.push(cell);
    }
    let stream = history.start_stream();
    history.push_stream(stream, STREAMED);
    history.finish_stream(stream);

    let mut out = io::stdout().lock();
    for line in history.drain_printable(&ctx) {
        write_line(&mut out, &line, width)?;
    }
    out.flush()
}

const STREAMED: &str = "## Where it stands\nIt's installed and working:\n\n```console\n$ argonauts --version\nargonauts 0.0.1\n```\n\nIt lives at `~/.cargo/bin/argonauts`. If your current shell still says \"command not found\", run `rehash` once.\n\nTo try it without touching a real project:\n\n```bash\nmkdir -p /tmp/argonauts-try && cd /tmp/argonauts-try\ngit init -q && echo \"# try\" > README.md && git add . && git commit -qm init\nargonauts run --cheapest \"Create a file named hello.txt containing the word hi\"\n```\n\nThen pick one:\n1. Merge now\n   1. run the tests\n   2. merge the branch\n2. Wait for the review\n\n```\nargonauts run --cheapest\n```\n";

fn samples(now: Instant) -> Vec<argonauts_tui::Cell> {
    let header = SessionHeader {
        app: "Argonauts Code".into(),
        version: "0.0.1".into(),
        main_agent: Some("Claude Code".into()),
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        workflow: "council".into(),
        ultra: None,
        folder: "~/github/argonauts".into(),
        agents: vec![
            AgentReady {
                name: "Claude Code".into(),
                ready: true,
                note: Some("2.1.284".into()),
            },
            AgentReady {
                name: "Codex".into(),
                ready: true,
                note: None,
            },
            AgentReady {
                name: "agy".into(),
                ready: false,
                note: Some("not installed".into()),
            },
        ],
        runs_going: 0,
        runs_need_you: 0,
        way_back: None,
    };

    let workflow = ToolCall::call(
        ToolKind::Agent,
        "Workflow",
        "Add a --json flag to the export command: plan the writer, the flag and their tests, build them in parallel, and review each part",
    )
    .with_state(ToolState::Done)
    .with_item(ToolItem::text(
        "Running in background · /workflows to monitor and save · medium size (/config)",
    ));

    let shell = ToolCall::described(ToolKind::Shell, "Updating the roadmap's status table")
        .with_item(ToolItem::Command {
            command: "python3 - <<'EOF'\np = \".github/docs/ROUTE_PLAN.md\"\ns = open(p).read()\nold = s[s.index(\"## Status\"):]\nnew = \"\"\"## Status\n| Area | Status |\n|---|---|\n| Parser | Running |\n| Docs | Waiting |\nEOF".into(),
            output: Vec::new(),
            hidden: 0,
        });

    let old = "## Status\n\n| Area | Status |\n|---|---|\n| Parser | In progress |\n| Everything else | Not started |\n";
    let new = "\n## Status\n\n| Area | Status |\n|---|---|\n| Parser, Docs | Starting now |\n| Everything else | Waiting |\n";
    let update = ToolCall::call(ToolKind::Edit, "Update", ".github/docs/ROUTE_PLAN.md")
        .with_state(ToolState::Done)
        .with_item(ToolItem::text(
            "Updated .github/docs/ROUTE_PLAN.md with 3 additions and 2 removals",
        ))
        .with_item(ToolItem::Diff(DiffView::between(old, new, 3)));

    let mut group = CollapsedGroup::new();
    group.push(
        ToolCall::call(ToolKind::Shell, "Bash", "argonauts doctor").with_state(ToolState::Done),
    );

    let mut session = ToolCall::agent(
        "Opus 5.5 on Claude Code",
        None,
        "worker · task-002",
        "Add the JSON writer",
    )
    .with_stats(ToolStats {
        started_at: now.checked_sub(Duration::from_secs(252)),
        ended_at: None,
        tokens_down: 18_100,
    });
    session.push_call(
        ToolCall::call(ToolKind::Read, "Read", "src/export.rs (212 lines)")
            .with_state(ToolState::Done),
    );
    let diff = DiffView::from_unified(
        "@@ -41,2 +41,4 @@\n     fn write(&self, out: &mut dyn Write) -> Result<()> {\n-        self.write_csv(out)\n+        match self.format {\n+            Format::Json => self.write_json(out),\n",
    )
    .unwrap_or_default();
    session.push_call(
        ToolCall::call(ToolKind::Edit, "Update", "src/export.rs")
            .with_state(ToolState::Done)
            .with_item(ToolItem::Diff(diff)),
    );
    session.push_call(
        ToolCall::call(ToolKind::Shell, "Bash", "cargo test").with_state(ToolState::Done),
    );
    session.push_call(
        ToolCall::call(ToolKind::Shell, "Bash", "cargo clippy").with_state(ToolState::Done),
    );
    session.push(ToolItem::Todos(vec![
        Todo {
            text: "Read the export command".into(),
            state: TodoState::Done,
        },
        Todo {
            text: "Write the JSON writer".into(),
            state: TodoState::InProgress,
        },
        Todo {
            text: "Add the --json flag".into(),
            state: TodoState::Pending,
        },
    ]));

    let failed = ToolCall::call(ToolKind::Shell, "Bash", "cargo test -p argonauts-tui")
        .with_state(ToolState::Failed)
        .with_item(ToolItem::Command {
            command: "cargo test -p argonauts-tui".into(),
            output: (1..=9)
                .map(|i| format!("test app::case_{i} ... ok"))
                .collect(),
            hidden: 3,
        });

    vec![
        header.into(),
        UserMessage::new("hi").into(),
        AssistantMessage::new("The plan is written. Before the workers start, a quick check that the agent CLIs are ready, with `argonauts doctor`:").into(),
        group.into(),
        workflow.into(),
        shell.into(),
        update.into(),
        session.into(),
        failed.into(),
        AssistantMessage::new("To try it:\n\n```bash\ncargo build --release && ./target/release/argonauts doctor\ncd <any git repo> && argonauts run --cheapest \"Create a file named hello.txt containing the word hi\"\n```\n\nStill to do:\n- The `--json` flag itself, once the writer is reviewed.\n- The review's two notes on error messages.\n- A line in the README. It's ready to write once the flag's name is settled.\n\n> A quote, **bold** and *italic*, with [a link](https://example.com).\n\n| Area | Status |\n|---|---|\n| Parser | Starting now |\n| Everything else | Waiting |").into(),
        TurnEnd::new("Sautéed for", Duration::from_secs(2 * 3600 + 53 * 60 + 6), ClockTime::new(6, 41)).into(),
        Recap::new("We're adding a --json flag to the export command. The JSON writer and its tests are in, and the review is going. Next, the flag itself and a line in the README.").into(),
        UserMessage::new("zsh: command not found: argonauts").into(),
        Notice::new(Tone::Warning, "Merge task-002 into main? · answered: Merge")
            .with_glyph("◆")
            .with_detail("3 files · +48 -6 · tests passed")
            .into(),
    ]
}

/// A line's own background fills the rest of the row, as the history writer does.
fn write_line(out: &mut impl Write, line: &Line<'_>, width: u16) -> io::Result<()> {
    let mut used = 0usize;
    for span in &line.spans {
        let style = line.style.patch(span.style);
        write!(out, "\x1b[0m{}{}", sgr(style), span.content)?;
        used += span.width();
    }
    if let Some(bg) = line.style.bg
        && used < usize::from(width)
    {
        write!(out, "\x1b[0m{}\x1b[K", sgr(Style::new().bg(bg)))?;
    }
    write!(out, "\x1b[0m\r\n")
}

fn sgr(style: Style) -> String {
    let mut codes: Vec<String> = Vec::new();
    let modifiers = [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ];
    for (modifier, code) in modifiers {
        if style.add_modifier.contains(modifier) {
            codes.push(code.to_owned());
        }
    }
    if let Some(fg) = style.fg {
        codes.push(color(fg, false));
    }
    if let Some(bg) = style.bg {
        codes.push(color(bg, true));
    }
    if codes.is_empty() {
        String::new()
    } else {
        format!("\x1b[{}m", codes.join(";"))
    }
}

fn color(color: Color, bg: bool) -> String {
    let base = if bg { 40 } else { 30 };
    let named = |n: u8| (base + n).to_string();
    let bright = |n: u8| (base + 60 + n).to_string();
    match color {
        Color::Reset => (base + 9).to_string(),
        Color::Black => named(0),
        Color::Red => named(1),
        Color::Green => named(2),
        Color::Yellow => named(3),
        Color::Blue => named(4),
        Color::Magenta => named(5),
        Color::Cyan => named(6),
        Color::Gray => named(7),
        Color::DarkGray => bright(0),
        Color::LightRed => bright(1),
        Color::LightGreen => bright(2),
        Color::LightYellow => bright(3),
        Color::LightBlue => bright(4),
        Color::LightMagenta => bright(5),
        Color::LightCyan => bright(6),
        Color::White => bright(7),
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", base + 8),
        Color::Indexed(i) => format!("{};5;{i}", base + 8),
    }
}
