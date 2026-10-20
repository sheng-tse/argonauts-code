//! The sessions the Context tab's pictures draw: a 1M Opus 5.5 main session at 75% and at
//! 94%, with two crew agents at work on "Add due dates", and a Haiku 5.5 main session after an
//! orchestrator run whose nine sessions have all ended. The main session's counts are the
//! Claude Code adapter's made-up replies, as Argonauts Code keeps them.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::context::{ContextCategory, ContextKind, ContextUsage};
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::views::{ContextRow, ContextView, EndedHow, RunFinishedAt, SessionLeft};
use argonauts_core::{RunId, Spend};
use time::macros::{datetime, offset};
use time::{Duration, OffsetDateTime};

use crate::bridge::tools::Places;
use crate::state::ContextCount;

const AT_THREE_QUARTERS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../adapters/claude-code/tests/data/context-usage-full.usage.json"
));
const NEAR_FULL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../adapters/claude-code/tests/data/context-usage-near-full.usage.json"
));
/// The main session's count as its turn ended, after the run: the lists of the count above,
/// with the numbers of a 27.8k Haiku 5.5 session.
const AFTER_RUN: &str = include_str!("after-run.usage.json");

/// What a turn of the pictures' conversation adds.
pub const PER_TURN: u64 = 9_400;
pub const RUN_TITLE: &str = "Add due dates";

/// One state of the pictures' session, with everything the tab draws from.
pub struct Example {
    pub count: ContextCount,
    pub view: ContextView,
    pub names: DisplayNames,
    pub places: Places,
    pub now: OffsetDateTime,
}

pub fn now() -> OffsetDateTime {
    datetime!(2026-10-10 14:00 UTC)
}

/// The run the pictures' crew works in.
pub fn run() -> RunId {
    RunId::new("run-07").expect("a run id")
}

/// The main session at 75%, counted six seconds ago, its crew just started.
pub fn at_three_quarters() -> Example {
    example(
        AT_THREE_QUARTERS,
        6,
        [flag(36_200, 17_500, 14, 0.14), tests(13_400, 6, 0.03)],
    )
}

/// The same session at 94%, counted two seconds ago, 22.4k short of compacting, with one
/// crew agent close to starting afresh.
pub fn near_full() -> Example {
    example(
        NEAR_FULL,
        2,
        [flag(171_400, 152_700, 52, 0.61), tests(41_000, 19, 0.09)],
    )
}

fn example(kept: &str, counted_ago: i64, crew: [ContextRow; 2]) -> Example {
    let usage: ContextUsage = serde_json::from_str(kept).expect("the kept count");
    let main = ContextRow {
        run: None,
        session: SessionKey::numbered(1),
        label: "main \u{b7} conversation".to_owned(),
        worker: model("claude-code/claude-opus-5-5"),
        used: usage.total_tokens,
        window: Some(usage.window),
        cap: None,
        tool_calls: 0,
        spent: Spend::default(),
        rotations: 0,
        estimated: false,
        counts: true,
        categories: Vec::new(),
        recorded: None,
        ended: None,
    };
    let mut rows = vec![main];
    rows.extend(crew);
    Example {
        count: ContextCount {
            usage,
            counted: now() - Duration::seconds(counted_ago),
        },
        view: ContextView::of(rows),
        names: crate::overview::sample::names(),
        places: Places {
            folder: Some("/home/you/todo".to_owned()),
            home: Some("/home/you".to_owned()),
        },
        now: now(),
    }
}

pub const AFTER_RUN_TITLE: &str = "Tasks due dates";

/// The orchestrator run the main session started, which has finished.
pub fn finished_run() -> RunId {
    RunId::new("run-20261017-070202-84f5").expect("a run id")
}

/// Two minutes after the main session's last answer ended, at 3:04 AM where you are.
pub fn after_run_now() -> OffsetDateTime {
    datetime!(2026-10-17 3:06:20 -4)
}

/// The main session counted as its answer ended, its run finished, and each of the run's
/// sessions as it left its window, in the order they started.
pub fn after_run() -> Example {
    let usage: ContextUsage = serde_json::from_str(AFTER_RUN).expect("the kept count");
    let now = after_run_now();
    let main = ContextRow {
        run: None,
        session: SessionKey::numbered(1),
        label: "main \u{b7} conversation".to_owned(),
        worker: model("claude-code/claude-haiku-5-5"),
        used: usage.total_tokens,
        window: Some(usage.window),
        cap: None,
        tool_calls: 1,
        spent: Spend::default(),
        rotations: 0,
        estimated: false,
        counts: true,
        categories: Vec::new(),
        recorded: None,
        ended: None,
    };
    let at = |hour, minute| {
        time::Date::from_calendar_date(2026, time::Month::October, 17)
            .expect("a day")
            .with_hms(hour, minute, 30)
            .expect("a time")
            .assume_offset(offset!(-4))
    };
    let done = |hour, minute| SessionLeft {
        at: at(hour, minute),
        how: EndedHow::Done,
        fallback_for: None,
    };
    let haiku = "claude-code/claude-haiku-5-5";
    let mut fallback = done(3, 3);
    fallback.fallback_for = Some(model(haiku));
    let failed = SessionLeft {
        how: EndedHow::Failed {
            why: "no valid report in 2 tries".to_owned(),
        },
        ..done(3, 3)
    };
    let rows = [
        ended_claude(
            1,
            "plan:orchestrator",
            12_021,
            [4_407, 153, 273, 4_921],
            1,
            done(3, 2),
        ),
        ended_claude(
            2,
            "plan:due-dates",
            12_965,
            [4_218, 153, 273, 3_605],
            6,
            done(3, 2),
        ),
        ContextRow {
            spent: Spend {
                cost_usd: 0.02,
                cost_estimated: true,
                ..Spend::default()
            },
            ..ended_claude(
                3,
                "build:due-dates",
                24_924,
                [9_868, 153, 273, 5_989],
                16,
                failed,
            )
        },
        ended_codex(4, "build:due-dates", 19_965, 2, fallback),
        ended_codex(5, "review:due-dates", 17_856, 1, done(3, 3)),
        ended_codex(6, "signoff:due-dates", 3_029, 1, done(3, 3)),
        ended_claude(
            7,
            "review:due-dates-2",
            12_992,
            [6_131, 167, 0, 5_291],
            1,
            done(3, 4),
        ),
        ended_codex(8, "run-review", 16_231, 1, done(3, 4)),
        ended_claude(9, "main", 11_666, [4_472, 153, 273, 4_187], 1, done(3, 4)),
    ];
    let mut view = vec![main];
    view.extend(rows);
    Example {
        count: ContextCount {
            usage,
            counted: now - Duration::minutes(2),
        },
        view: ContextView {
            rows: view,
            finished: vec![RunFinishedAt {
                run: finished_run(),
                at: at(3, 4),
            }],
            ..ContextView::default()
        },
        names: crate::overview::sample::names(),
        places: Places {
            folder: Some("/home/you/todo".to_owned()),
            home: Some("/home/you".to_owned()),
        },
        now,
    }
}

/// A run session on Haiku 5.5 as it ended: its system tools, MCP tools, MCP server
/// instructions and messages, after Claude Code's system prompt.
fn ended_claude(
    n: u32,
    label: &str,
    used: u64,
    [tools, mcp, instructions, messages]: [u64; 4],
    calls: u32,
    left: SessionLeft,
) -> ContextRow {
    let row = |name: &str, tokens: u64, color: &str| ContextCategory {
        name: name.to_owned(),
        tokens,
        color: color.to_owned(),
        kind: ContextKind::Used,
    };
    let categories = vec![
        row("System prompt", 2_497, "promptBorder"),
        row("System tools", tools, "inactive"),
        row("MCP tools", mcp, "cyan_FOR_SUBAGENTS_ONLY"),
        row(
            "MCP server instructions",
            instructions,
            "green_FOR_SUBAGENTS_ONLY",
        ),
        row("Messages", messages, "purple_FOR_SUBAGENTS_ONLY"),
    ];
    ContextRow {
        run: Some(finished_run()),
        session: SessionKey::numbered(n),
        label: label.to_owned(),
        worker: model("claude-code/claude-haiku-5-5"),
        used,
        window: Some(1_000_000),
        cap: Some(900_000),
        tool_calls: calls,
        spent: Spend {
            cost_usd: 0.004,
            ..Spend::default()
        },
        rotations: 0,
        estimated: false,
        counts: true,
        categories,
        recorded: None,
        ended: Some(left),
    }
}

/// A run session on GPT 6 Luna as it ended, which Codex says only how full it was.
fn ended_codex(n: u32, label: &str, used: u64, calls: u32, left: SessionLeft) -> ContextRow {
    ContextRow {
        run: Some(finished_run()),
        session: SessionKey::numbered(n),
        label: label.to_owned(),
        worker: model("codex/gpt-6-luna"),
        used,
        window: Some(872_000),
        cap: Some(784_800),
        tool_calls: calls,
        spent: Spend {
            cost_usd: 0.000_8,
            cost_estimated: true,
            ..Spend::default()
        },
        rotations: 0,
        estimated: false,
        counts: false,
        categories: Vec::new(),
        recorded: None,
        ended: Some(left),
    }
}

fn model(worker: &str) -> ModelRef {
    worker.parse().expect("a model")
}

/// The agent adding the flag, on Claude Code, which says what fills its window.
fn flag(used: u64, messages: u64, calls: u32, cost: f64) -> ContextRow {
    let row = |name: &str, tokens: u64, color: &str, kind| ContextCategory {
        name: name.to_owned(),
        tokens,
        color: color.to_owned(),
        kind,
    };
    let categories = vec![
        row("System prompt", 2_900, "promptBorder", ContextKind::Used),
        row("System tools", 12_100, "inactive", ContextKind::Used),
        row("Memory files", 3_700, "claude", ContextKind::Used),
        row(
            "Messages",
            messages,
            "purple_FOR_SUBAGENTS_ONLY",
            ContextKind::Used,
        ),
        row(
            "Autocompact buffer",
            20_000,
            "inactive",
            ContextKind::Buffer,
        ),
        row(
            "Free space",
            180_000 - used,
            "promptBorder",
            ContextKind::Free,
        ),
    ];
    assert_eq!(2_900 + 12_100 + 3_700 + messages, used);
    ContextRow {
        run: Some(run()),
        session: SessionKey::numbered(2),
        label: "build:due-flag".to_owned(),
        worker: model("claude-code/claude-sonnet-5-5"),
        used,
        window: Some(200_000),
        cap: Some(180_000),
        tool_calls: calls,
        spent: Spend {
            cost_usd: cost,
            ..Spend::default()
        },
        rotations: 0,
        estimated: false,
        counts: true,
        categories,
        recorded: None,
        ended: None,
    }
}

/// The agent writing the tests, on Codex, which says how full its window is and not with what.
fn tests(used: u64, calls: u32, cost: f64) -> ContextRow {
    ContextRow {
        run: Some(run()),
        session: SessionKey::numbered(3),
        label: "build:due-tests".to_owned(),
        worker: model("codex/gpt-5.6"),
        used,
        window: Some(272_000),
        cap: Some(244_800),
        tool_calls: calls,
        spent: Spend {
            cost_usd: cost,
            cost_estimated: true,
            ..Spend::default()
        },
        rotations: 0,
        estimated: true,
        counts: false,
        categories: Vec::new(),
        recorded: None,
        ended: None,
    }
}
