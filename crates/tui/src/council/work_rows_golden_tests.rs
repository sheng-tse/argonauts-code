//! The council's work in the run's rows under the bottom line, cell for cell against the work
//! screens' last rows: the request, each task with how it is done, and the steps or agents
//! that do a task's work, each with the logo of whoever serves it and its seat's number where
//! its model sits more than once.

use std::time::Instant;

use argonauts_core::logos::logo_for;

use super::golden_tests::assert_rows_from;
use crate::bottom::{
    AgentPanel, RunRowKind, WorkflowPanel, WorkflowRow, WorkflowRowState, panel_lines,
};
use crate::kit::logo::golden::{council_text, pictured};
use crate::render::{Clock, RenderCtx};
use crate::theme::Theme;

const OPUS: (&str, &str) = ("claude-code", "claude-opus-5-5");
const LUNA: (&str, &str) = ("codex", "gpt-5.6-luna");
const FLASH: (&str, &str) = ("antigravity-cli", "gemini-3.8-flash");
const QWEN: (&str, &str) = ("opencode", "ollama/qwen3-coder:30b");

use WorkflowRowState::{Done, Running, Waiting};

/// One row: its state, whose model serves it and the seat's number, its name, how it is done,
/// its count or what it does, its seconds and its tokens out.
struct Row<'a> {
    state: WorkflowRowState,
    by: (&'a str, &'a str),
    number: Option<u8>,
    name: &'a str,
    way: Option<&'a str>,
    count: Option<(u32, u32)>,
    doing: Option<&'a str>,
    seconds: u64,
    tokens: u64,
}

impl Row<'_> {
    fn at(&self, kind: RunRowKind, depth: usize) -> WorkflowRow {
        let mut row = WorkflowRow::new(format!("run-limit#{}", self.name), self.name, self.state);
        row.kind = kind;
        row.depth = depth;
        row.logo = Some(logo_for(self.by.0, Some(self.by.1)));
        row.number = self.number;
        row.way = self.way.map(str::to_owned);
        row.count = self.count;
        row.doing = self.doing.map(str::to_owned);
        row.elapsed_ms = self.seconds * 1000;
        row.timed = true;
        row.tokens = self.tokens;
        row
    }

    /// A task of the request.
    fn task(&self) -> WorkflowRow {
        self.at(RunRowKind::Task, 1)
    }

    /// A step or an agent doing a task's work.
    fn work(&self) -> WorkflowRow {
        self.at(RunRowKind::Work, 2)
    }
}

/// The request's own row: its tasks done of all, its seconds and the tokens of all its work.
fn request(done: u32, total: u32, seconds: u64, tokens: u64) -> WorkflowRow {
    let mut row = WorkflowRow::new("run-limit", "Rate limit the API", Running);
    row.count = Some((done, total));
    row.elapsed_ms = seconds * 1000;
    row.timed = true;
    row.tokens = tokens;
    row
}

/// The rows as the panel draws them `width` columns wide in `theme`, the row `selected` picked,
/// logos as the screens write them.
fn drawn(
    rows: Vec<WorkflowRow>,
    selected: Option<usize>,
    theme: &Theme,
    width: u16,
) -> Vec<String> {
    let panel = WorkflowPanel {
        rows,
        max_rows: 10,
        selected,
        ..WorkflowPanel::default()
    };
    let ctx = RenderCtx::new(width, theme, Clock::still(Instant::now()));
    panel_lines(&panel, &AgentPanel::default(), &ctx)
        .iter()
        .map(|line| council_text(line).trim_end().to_owned())
        .collect()
}

/// The work as it starts: Redis bucket's workflow, Plan limits and Memory store each in its
/// seat's session, and Route tests' three agents not yet under way.
pub(super) fn starts() -> Vec<WorkflowRow> {
    vec![
        request(0, 4, 662, 1_200_000),
        Row {
            state: Running,
            by: OPUS,
            number: None,
            name: "Redis bucket",
            way: Some("Maker-checker"),
            count: Some((1, 2)),
            doing: None,
            seconds: 216,
            tokens: 402_000,
        }
        .task(),
        Row {
            state: Running,
            by: LUNA,
            number: None,
            name: "Plan limits",
            way: Some("Own session"),
            count: Some((1, 5)),
            doing: None,
            seconds: 216,
            tokens: 171_000,
        }
        .task(),
        Row {
            state: Running,
            by: QWEN,
            number: None,
            name: "Memory store",
            way: Some("Own session"),
            count: None,
            doing: Some("Write(src/rate/memory-store.ts)"),
            seconds: 216,
            tokens: 120_000,
        }
        .task(),
        Row {
            state: Running,
            by: FLASH,
            number: None,
            name: "Route tests",
            way: Some("3 agents"),
            count: Some((0, 3)),
            doing: None,
            seconds: 64,
            tokens: 52_000,
        }
        .task(),
    ]
}

/// As the work starts the request's row counts its tasks done, and each task says how it is
/// done, with its bar or what it does; a terminal without pictures shows each logo's dot.
#[test]
fn the_work_starts_with_each_task_and_how_it_is_done() {
    let pictures = pictured(&Theme::dark());
    for screen in ["work_starts", "task_picked"] {
        assert_rows_from(screen, &drawn(starts(), None, &pictures, 120), 33);
    }
    let rows = drawn(starts(), None, &Theme::dark(), 120);
    assert_rows_from("transcript_task_picked", &rows, 33);
}

/// The work under way: a workflow's steps and a task's agents under it, a task done in its
/// own session with its seat's last words, and the picked row marked.
fn nests() -> Vec<WorkflowRow> {
    let step = |state, by, name, doing, seconds, tokens| Row {
        state,
        by,
        number: None,
        name,
        way: None,
        count: None,
        doing: Some(doing),
        seconds,
        tokens,
    };
    vec![
        request(1, 4, 846, 1_600_000),
        Row {
            state: Running,
            by: OPUS,
            number: None,
            name: "Redis bucket",
            way: Some("Maker-checker"),
            count: Some((1, 2)),
            doing: None,
            seconds: 400,
            tokens: 612_000,
        }
        .task(),
        step(Done, OPUS, "Build", "Done, 6 files changed", 200, 380_000).work(),
        step(Waiting, FLASH, "Review", "Reading the diff", 200, 232_000).work(),
        Row {
            state: Running,
            by: LUNA,
            number: None,
            name: "Plan limits",
            way: Some("Own session"),
            count: Some((2, 5)),
            doing: None,
            seconds: 400,
            tokens: 288_000,
        }
        .task(),
        Row {
            state: Done,
            by: QWEN,
            number: None,
            name: "Memory store",
            way: Some("Own session"),
            count: None,
            doing: Some("Memory store is done: 9 tests pass."),
            seconds: 252,
            tokens: 194_000,
        }
        .task(),
        Row {
            state: Running,
            by: FLASH,
            number: None,
            name: "Route tests",
            way: Some("3 agents"),
            count: Some((1, 3)),
            doing: None,
            seconds: 248,
            tokens: 164_000,
        }
        .task(),
        step(Done, FLASH, "Orders", "14 tests pass", 112, 58_000).work(),
        step(
            Waiting,
            FLASH,
            "Users",
            "Write(test/routes/users.test.ts)",
            248,
            61_000,
        )
        .work(),
        step(
            Waiting,
            FLASH,
            "Import",
            "Bash(npm test -- import)",
            248,
            45_000,
        )
        .work(),
    ]
}

/// Under the request, a workflow's steps and a task's agents nest under their task, each
/// saying what it does; at 80 columns each row keeps its name, bar and numbers, without how
/// its task is done.
#[test]
fn the_work_nests_under_each_task() {
    let pictures = pictured(&Theme::dark());
    assert_rows_from("work_nests", &drawn(nests(), Some(6), &pictures, 120), 28);
    assert_rows_from(
        "work_at_80_columns",
        &drawn(nests(), Some(6), &pictures, 80),
        28,
    );
}

/// A blind check's work: the checker's review step accepted the workflow's build, and the
/// picked step's row says so; where only one maker is set up, the review says Same maker.
#[test]
fn a_checks_step_says_it_was_blind_and_whose_maker_it_was() {
    let checked = |review: (&str, &str), accepted: &str, tokens: [u64; 6], seconds: u64| {
        let (planner, planned_number) = match review {
            OPUS => (OPUS, Some(3)),
            _ => (LUNA, None),
        };
        vec![
            request(1, 3, seconds, tokens[0]),
            Row {
                state: Done,
                by: OPUS,
                number: Some(1),
                name: "Redis bucket",
                way: Some("Maker-checker"),
                count: Some((2, 2)),
                doing: None,
                seconds: 400,
                tokens: tokens[1],
            }
            .task(),
            Row {
                state: Done,
                by: OPUS,
                number: Some(1),
                name: "Build",
                way: None,
                count: None,
                doing: Some("Done, 6 files changed"),
                seconds: 200,
                tokens: tokens[2],
            }
            .work(),
            Row {
                state: Done,
                by: review,
                number: None,
                name: "Review",
                way: None,
                count: None,
                doing: Some(accepted),
                seconds: 200,
                tokens: tokens[3],
            }
            .work(),
            Row {
                state: Running,
                by: planner,
                number: planned_number,
                name: "Plan limits",
                way: Some("Own session"),
                count: Some((3, 5)),
                doing: None,
                seconds,
                tokens: tokens[4],
            }
            .task(),
            Row {
                state: Running,
                by: OPUS,
                number: Some(2),
                name: "Route tests",
                way: Some("3 agents"),
                count: Some((1, 3)),
                doing: None,
                seconds: 330,
                tokens: tokens[5],
            }
            .task(),
        ]
    };
    let pictures = pictured(&Theme::dark());
    let blind = checked(
        FLASH,
        "Accepted, blind",
        [1_100_000, 612_000, 380_000, 232_000, 288_000, 164_000],
        552,
    );
    assert_rows_from("blind_check", &drawn(blind, Some(3), &pictures, 120), 32);
    let same = checked(
        OPUS,
        "Accepted · Same maker",
        [1_100_000, 640_000, 402_000, 238_000, 301_000, 171_000],
        545,
    );
    assert_rows_from(
        "same_maker_check",
        &drawn(same, Some(3), &pictures, 120),
        32,
    );
}
