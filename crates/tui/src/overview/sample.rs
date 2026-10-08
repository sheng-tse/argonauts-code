//! One day's usage, for the tests of what shows it.

use argonauts_core::api::{
    CardBrief, DaySpend, PlanUsage, RunProgress, RunState, RunSummary, StatusSummary, UsagePeriod,
    UsageShare, UsageSummary,
};
use argonauts_core::app::SettingRow;
use argonauts_core::crew_stats::{CrewRole, CrewStats, Fallback, ModelDay, TaskRecord, WorkRow};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::CardState;
use argonauts_core::events::context::{ContextCategory, ContextItem, ContextPart};
use argonauts_core::events::run::RunMode;
use argonauts_core::events::vendor::RunBudget;
use argonauts_core::limits::{QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::{SessionKey, VendorId, WorkerRef};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::settings::stats::StatsSettings;
use argonauts_core::views::{ContextRow, ContextView};
use argonauts_core::{CardId, RunId, Spend};
use time::macros::datetime;
use time::{Duration, OffsetDateTime};

use crate::state::ContextBreakdown;

/// Monday, early afternoon.
pub fn now() -> OffsetDateTime {
    datetime!(2026-10-05 13:00 UTC)
}

pub fn names() -> DisplayNames {
    let mut names = DisplayNames::default();
    for (id, name) in [
        ("claude-code", "Claude Code"),
        ("codex", "Codex"),
        ("antigravity-cli", "agy"),
    ] {
        names.harnesses.insert(id.to_owned(), name.to_owned());
    }
    names
}

fn reading(
    agent: &str,
    window: &str,
    used: Option<f64>,
    resets: OffsetDateTime,
    read_ago: Duration,
) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(agent.to_owned()),
        status: match used {
            Some(_) => QuotaStatus::Allowed,
            None => QuotaStatus::Rejected,
        },
        used,
        resets_at: Some(resets),
        window: Some(window.to_owned()),
        source: QuotaSource::AppServer,
        observed_at: now() - read_ago,
        detail: None,
        wrap_up: false,
    }
}

fn spent(cost: f64, estimated: bool) -> Spend {
    Spend {
        cost_usd: cost,
        cost_estimated: estimated,
        ..Spend::default()
    }
}

fn run(n: u32, name: &str, state: RunState, cost: f64) -> RunSummary {
    RunSummary {
        run: RunId::new(format!("run-{n:02}")).expect("a run id"),
        title: name.replace('-', " "),
        name: Some(name.to_owned()),
        mode: RunMode::Crew,
        workflow: "council".to_owned(),
        state,
        stopped_at: String::new(),
        updated: now(),
        spent: spent(cost, true),
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: state == RunState::Running,
        remind: false,
    }
}

pub fn usage() -> UsageSummary {
    let minute = Duration::minutes(1);
    let plans = vec![
        PlanUsage {
            agent: "claude-code".to_owned(),
            name: "Claude Code".to_owned(),
            plan: Some("Max".to_owned()),
            in_use: true,
            windows: vec![
                reading(
                    "claude-code",
                    "five_hour",
                    Some(0.71),
                    datetime!(2026-10-05 16:40 UTC),
                    minute,
                ),
                reading(
                    "claude-code",
                    "seven_day",
                    Some(0.38),
                    datetime!(2026-10-06 22:00 UTC),
                    minute,
                ),
            ],
            ..PlanUsage::default()
        },
        PlanUsage {
            agent: "codex".to_owned(),
            name: "Codex".to_owned(),
            plan: Some("Plus".to_owned()),
            windows: vec![
                reading(
                    "codex",
                    "five_hour",
                    Some(0.92),
                    datetime!(2026-10-05 14:15 UTC),
                    minute * 3,
                ),
                reading(
                    "codex",
                    "weekly",
                    Some(0.41),
                    datetime!(2026-10-09 09:00 UTC),
                    minute * 3,
                ),
            ],
            ..PlanUsage::default()
        },
        PlanUsage {
            agent: "antigravity-cli".to_owned(),
            name: "Antigravity CLI".to_owned(),
            plan: Some("Pro".to_owned()),
            windows: vec![reading(
                "antigravity-cli",
                "day",
                None,
                datetime!(2026-10-06 15:45 UTC),
                minute,
            )],
            moved_to: Some("GPT 5.6 on Codex".to_owned()),
            ..PlanUsage::default()
        },
        PlanUsage {
            agent: "openrouter".to_owned(),
            name: "OpenRouter".to_owned(),
            pays_per_token: true,
            ..PlanUsage::default()
        },
    ];
    let share = |agent: &str, model: &str, cost: f64, estimated: bool| UsageShare {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        vendor: agent.to_owned(),
        spent: spent(cost, estimated),
    };
    let day_costs = [
        1.9, 3.1, 0.0, 0.9, 1.7, 0.6, 0.0, 0.0, 2.2, 1.5, 0.5, 0.8, 1.3, 1.86,
    ];
    let history = day_costs
        .iter()
        .enumerate()
        .map(|(k, cost)| DaySpend {
            day: now().date() - Duration::days(13 - k as i64),
            spent: spent(*cost, true),
        })
        .collect();
    UsageSummary {
        period: UsagePeriod::Today,
        total: Spend {
            input_tokens: 1_342_000,
            cached_tokens: 950_000,
            output_tokens: 58_000,
            cost_usd: 1.86,
            cost_estimated: true,
            cost_partial: false,
        },
        by_agent: vec![
            share("claude-code", "claude-opus-5-5", 1.12, true),
            share("codex", "gpt-5.6", 0.62, false),
            share("antigravity-cli", "gemini-3.6-flash", 0.12, true),
        ],
        history,
        runs: vec![
            run(3, "export-reports-csv", RunState::Running, 1.20),
            run(2, "fix-flaky-export-test", RunState::Merged, 0.39),
            run(1, "streaming-writer", RunState::Stopped, 0.06),
        ],
        quotas: Vec::new(),
        plans,
        conversation: Some(spent(0.21, true)),
        spend_limit: None,
        plan_limit: None,
        progress: vec![
            progress(3, 1, 4, Duration::minutes(51)),
            progress(2, 2, 2, Duration::seconds(760)),
            progress(1, 0, 1, Duration::seconds(190)),
        ],
        task_limit: None,
        limits: limits(),
        stats: None,
    }
}

/// The limits as `/limits` lists them before you set one.
pub fn limits() -> Vec<SettingRow> {
    let row = |key: &str, value: &str| SettingRow {
        key: format!("limits.{key}"),
        value: value.to_owned(),
        layer: "built-in".to_owned(),
        preset_from: None,
        default_scope: SettingScope::Global,
        safety: false,
        allowed: Vec::new(),
        description: String::new(),
    };
    vec![
        row("spend_per_run", ""),
        row("spend_per_day", ""),
        row("plan_usage", ""),
        row("per_task_usd", ""),
        row("run_tokens", ""),
        row("run_minutes", ""),
        row("when_reached", "\"fallback\""),
        row("soft_stop", "0.85"),
        row("from_history", "true"),
        row("lines.review", "0.15"),
    ]
}

/// One limit of [`limits`] set to `value` in the layer `layer`.
pub fn with_limit(
    mut rows: Vec<SettingRow>,
    key: &str,
    value: &str,
    layer: &str,
) -> Vec<SettingRow> {
    for row in rows.iter_mut().filter(|row| row.key == key) {
        row.value = value.to_owned();
        row.layer = layer.to_owned();
    }
    rows
}

fn progress(n: u32, tasks_done: u32, tasks: u32, took: Duration) -> RunProgress {
    RunProgress {
        run: RunId::new(format!("run-{n:02}")).expect("a run id"),
        tasks_done,
        tasks,
        started: Some(now() - took),
    }
}

/// A sample run: four tasks, one merged, one running, one waiting, and
/// one that asks you.
pub fn status() -> StatusSummary {
    let task = |n: u32, title: &str, state: CardState, worker: &str, note: &str| CardBrief {
        card: CardId::numbered(n),
        title: title.to_owned(),
        state,
        owner: Some(worker.parse().expect("a worker")),
        note: note.to_owned(),
    };
    let (opus, gpt) = ("claude-code/claude-opus-5-5", "codex/gpt-5.6");
    StatusSummary {
        run: Some(RunId::new("run-03").expect("a run id")),
        goal: vec![
            "Reports export as CSV from the API and the web app".to_owned(),
            "The columns match the screen".to_owned(),
        ],
        plan: Some("4 tasks: GPT 5.6 writes the CSV, Opus builds the endpoint".to_owned()),
        decisions: Vec::new(),
        cards: vec![
            task(
                1,
                "CSV writer",
                CardState::Integrated,
                gpt,
                "6m12s · 38k tokens",
            ),
            task(
                2,
                "Report endpoint",
                CardState::Running,
                opus,
                "14m · 402k tokens",
            ),
            task(
                3,
                "Docs and examples",
                CardState::Waiting,
                gpt,
                "needs task-002",
            ),
            task(
                4,
                "Review",
                CardState::Blocked,
                opus,
                "which date format to use",
            ),
        ],
        recent: vec![
            "task-003 moved from agy to Codex 20m ago".to_owned(),
            "task-001 merged 12m ago".to_owned(),
        ],
        budget: Some(RunBudget {
            spent: Spend {
                input_tokens: 1_100_000,
                output_tokens: 100_000,
                ..spent(1.20, false)
            },
            limit: None,
            lines: Vec::new(),
            reserve_left: None,
        }),
        quotas: Vec::new(),
        waiting: vec!["task-004 Review asks which date format the CSV uses".to_owned()],
    }
}

/// The same run with a three-dollar limit set.
pub fn status_with_limit() -> StatusSummary {
    let mut status = status();
    if let Some(budget) = &mut status.budget {
        budget.limit = Some(BudgetAmount {
            unit: BudgetUnit::Usd,
            amount: 3.0,
        });
    }
    status
}

/// Three live sessions: the chat with the main agent and two of the run's.
pub fn context() -> ContextView {
    let row =
        |n: u32, label: &str, worker: &str, used: u64, window: u64, calls: u32, cost| ContextRow {
            run: (n > 1).then(|| RunId::new("run-03").expect("a run id")),
            session: SessionKey::numbered(n),
            label: label.to_owned(),
            worker: worker.parse::<WorkerRef>().expect("a worker"),
            used,
            window: Some(window),
            cap: Some(window / 100 * 95),
            tool_calls: calls,
            spent: spent(cost, true),
            rotations: 0,
        };
    let (opus, gpt) = ("claude-code/claude-opus-5-5", "codex/gpt-5.6");
    ContextView {
        rows: vec![
            row(1, "main · chat", opus, 212_000, 1_000_000, 41, 0.21),
            row(2, "build:report-endpoint", opus, 48_200, 200_000, 37, 0.62),
            row(3, "build:docs", gpt, 31_000, 272_000, 12, 0.09),
        ],
    }
}

/// The main session's context as Claude Code's /context counts it.
pub fn breakdown() -> ContextBreakdown {
    let category = |part, tokens| ContextCategory { part, tokens };
    let item = |name: &str, tokens| ContextItem {
        name: name.to_owned(),
        tokens,
        source: None,
    };
    ContextBreakdown {
        categories: vec![
            category(ContextPart::SystemPrompt, 3_400),
            category(ContextPart::Tools, 12_000),
            category(ContextPart::McpTools, 2_600),
            category(ContextPart::Agents, 1_100),
            category(ContextPart::MemoryFiles, 4_900),
            category(ContextPart::Skills, 2_200),
            category(ContextPart::Messages, 186_000),
            category(ContextPart::Free, 738_000),
            category(ContextPart::Buffer, 50_000),
        ],
        auto_compact_at: Some(950_000),
        window: 1_000_000,
        memory_files: vec![item("CLAUDE.md", 2_100), item("AGENTS.md", 2_800)],
        mcp_tools: vec![item("github", 1_900), item("linear", 700)],
        agents: vec![item("reviewer", 1_100)],
        skills: vec![
            item("export-csv", 1_100),
            item("review", 600),
            item("release-notes", 500),
        ],
    }
}

/// Seven days of the crew's work, as the Stats tab draws it: Opus 5.5 plans and
/// builds, Sonnet 5.5 and both GPTs build, GPT 5.6 and Gemini 3.6 Pro review, Gemini 3.6 Flash
/// signs off.
pub fn crew_stats() -> CrewStats {
    let tokens = |total: u64, cached: u64, output: u64, cost: f64, estimated: bool| Spend {
        input_tokens: total - output,
        cached_tokens: cached,
        output_tokens: output,
        cost_usd: cost,
        cost_estimated: estimated,
        cost_partial: false,
    };
    let row =
        |agent: &str, model: &str, role: CrewRole, kind: Option<&str>, spent: Spend| WorkRow {
            agent: agent.to_owned(),
            model: Some(model.to_owned()),
            role,
            kind: kind.map(str::to_owned),
            spent,
            done: 0,
            sent_back: 0,
            took_secs: 0,
        };
    let did = |mut row: WorkRow, done: u32, sent_back: u32, each: u64| {
        row.done = done;
        row.sent_back = sent_back;
        row.took_secs = each * u64::from(done);
        row
    };
    use CrewRole::{Judge, Orchestrator, Reviewer, Worker};
    let (claude, codex, agy) = ("claude-code", "codex", "antigravity-cli");
    let opus = "claude-opus-5-5";
    let work = vec![
        did(
            row(
                claude,
                opus,
                Orchestrator,
                None,
                tokens(500_000, 340_000, 20_000, 1.50, true),
            ),
            14,
            0,
            120,
        ),
        did(
            row(
                claude,
                opus,
                Worker,
                Some("build"),
                tokens(1_600_000, 1_090_000, 60_000, 4.90, true),
            ),
            24,
            0,
            280,
        ),
        did(
            row(
                claude,
                "claude-sonnet-5-5",
                Worker,
                Some("build"),
                tokens(800_000, 540_000, 30_000, 1.50, true),
            ),
            11,
            0,
            190,
        ),
        did(
            row(
                codex,
                "gpt-5.6",
                Worker,
                Some("build"),
                tokens(700_000, 480_000, 30_000, 1.30, false),
            ),
            12,
            0,
            200,
        ),
        did(
            row(
                codex,
                "gpt-5.6",
                Worker,
                Some("tests"),
                tokens(250_000, 170_000, 10_000, 0.50, false),
            ),
            6,
            0,
            200,
        ),
        did(
            row(
                codex,
                "gpt-5.6",
                Worker,
                Some("docs"),
                tokens(100_000, 70_000, 5_000, 0.20, false),
            ),
            3,
            0,
            200,
        ),
        did(
            row(
                codex,
                "gpt-5.6",
                Reviewer,
                None,
                tokens(150_000, 100_000, 5_000, 0.30, false),
            ),
            14,
            2,
            52,
        ),
        did(
            row(
                codex,
                "gpt-5.6-luna",
                Worker,
                Some("build"),
                tokens(350_000, 240_000, 15_000, 0.45, false),
            ),
            7,
            0,
            135,
        ),
        did(
            row(
                agy,
                "gemini-3.6-pro",
                Reviewer,
                None,
                tokens(430_000, 290_000, 20_000, 0.47, true),
            ),
            49,
            4,
            44,
        ),
        did(
            row(
                agy,
                "gemini-3.6-flash",
                Judge,
                None,
                tokens(120_000, 80_000, 5_000, 0.08, true),
            ),
            14,
            1,
            30,
        ),
    ];
    let task = |agent: &str, model: &str, kind: &str, sent_back: u32| TaskRecord {
        agent: agent.to_owned(),
        model: Some(model.to_owned()),
        kind: Some(kind.to_owned()),
        sent_back,
        merged: true,
        reviewed: true,
    };
    let mut tasks = Vec::new();
    let mut add = |agent: &str, model: &str, kind: &str, first: u32, once: u32, twice: u32| {
        for sent_back in [(first, 0), (once, 1), (twice, 2)]
            .into_iter()
            .flat_map(|(n, back)| std::iter::repeat_n(back, n as usize))
        {
            tasks.push(task(agent, model, kind, sent_back));
        }
    };
    add(claude, opus, "build", 23, 1, 0);
    add(claude, "claude-sonnet-5-5", "build", 10, 1, 0);
    add(codex, "gpt-5.6", "build", 11, 1, 0);
    add(codex, "gpt-5.6", "tests", 5, 1, 0);
    add(codex, "gpt-5.6", "docs", 3, 0, 0);
    add(codex, "gpt-5.6-luna", "build", 6, 0, 1);
    let daily = [
        (opus, claude, [300, 420, 180, 330, 440, 120, 310]),
        (
            "claude-sonnet-5-5",
            claude,
            [110, 150, 60, 120, 180, 40, 140],
        ),
        ("gpt-5.6", codex, [160, 230, 90, 170, 260, 70, 220]),
        ("gpt-5.6-luna", codex, [50, 60, 30, 50, 80, 20, 60]),
        ("gemini-3.6-pro", agy, [60, 80, 30, 60, 100, 30, 70]),
        ("gemini-3.6-flash", agy, [20, 20, 10, 20, 20, 10, 20]),
    ];
    let days = daily
        .iter()
        .flat_map(|(model, agent, thousands)| {
            thousands
                .iter()
                .enumerate()
                .map(move |(k, tokens)| ModelDay {
                    day: now().date() - Duration::days(6 - k as i64),
                    agent: (*agent).to_owned(),
                    model: Some((*model).to_owned()),
                    tokens: tokens * 1_000,
                    cost_usd: *tokens as f64 * 0.002,
                })
        })
        .collect();
    CrewStats {
        work,
        tasks,
        days,
        fallbacks: vec![
            Fallback {
                agent: codex.to_owned(),
                model: Some("gpt-5.6".to_owned()),
                from_agent: claude.to_owned(),
                from_model: Some(opus.to_owned()),
                why: "usage limit, resets 3:45pm".to_owned(),
                window: Some("five_hour".to_owned()),
            };
            2
        ],
        workflows: 14,
        took_secs: 12 * 3600 + 40 * 60,
        settings: StatsSettings::default(),
        sources: Default::default(),
        problems: Vec::new(),
    }
}

/// The week's usage, with the crew's stats.
pub fn usage_with_stats() -> UsageSummary {
    UsageSummary {
        period: UsagePeriod::Week,
        stats: Some(crew_stats()),
        ..usage()
    }
}
