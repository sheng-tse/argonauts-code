//! The sessions the Context tab's pictures draw: a 1M Opus 5.5 main session at 75% and at
//! 94%, with two crew agents at work on "Add due dates". The main session's counts are the
//! Claude Code adapter's made-up replies, as Argonauts Code keeps them.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::context::{ContextCategory, ContextKind, ContextUsage};
use argonauts_core::names::{ModelRef, SessionKey};
use argonauts_core::views::{ContextRow, ContextView};
use argonauts_core::{RunId, Spend};
use time::macros::datetime;
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
    };
    let mut rows = vec![main];
    rows.extend(crew);
    Example {
        count: ContextCount {
            usage,
            counted: now() - Duration::seconds(counted_ago),
        },
        view: ContextView { rows },
        names: crate::overview::sample::names(),
        places: Places {
            folder: Some("/home/you/todo".to_owned()),
            home: Some("/home/you".to_owned()),
        },
        now: now(),
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
    }
}
