//! A crew shaped like a real machine's, two weeks after it started working through Argonauts
//! Code: ten models over four CLIs, a history in the CLIs' own records that outranks most of
//! the crew, sessions that never said which model they ran, names too long for a narrow
//! column, models that never used a token, and a week that has spent nothing yet.
//!
//! The Stats and Limits figures match the approved drawings of those tabs, so a panel drawn
//! from this crew can be held to them. Every other part (the days, the tasks, the
//! Overview's history) is made to agree with them: one table of members feeds all of it.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::Arc;

use argonauts_core::api::{
    DaySpend, PlanUsage, RunProgress, RunState, RunSummary, UsagePeriod, UsageShare, UsageSummary,
};
use argonauts_core::crew_stats::StatsRole::{Judge, Main, Merger, Orchestrator, Reviewer, Worker};
use argonauts_core::crew_stats::{
    CrewStats, Fallback, ModelDay, Outcomes, StatsRole, TaskRecord, WorkRow,
};
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::run::RunMode;
use argonauts_core::limits::PlanUsageSource;
use argonauts_core::model_catalog::CatalogEntry;
use argonauts_core::names::{AgentId, ModelRef, SessionKey};
use argonauts_core::overview::{
    CliSource, ImportStatus, ModelDay as OverviewModelDay, ModelTokens, OverviewData, OverviewDay,
    OverviewModel, OverviewMonth, OverviewPeriod, OverviewWeek, ThroughArgonauts, Tokens,
};
use argonauts_core::plan_usage::{LiveUsage, LiveWindow};
use argonauts_core::settings::stats::StatsSettings;
use argonauts_core::views::{ContextRow, ContextView, EndedHow, RunFinishedAt, SessionLeft};
use argonauts_core::{RunId, Spend};
use time::macros::{date, datetime};
use time::{Date, Duration, OffsetDateTime, Weekday};

use crate::state::AppState;

pub const CLAUDE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const AGY: &str = "antigravity-cli";
pub const OPENCODE: &str = "opencode";

/// The model Codex runs when a session names none, as its model list marks it. Codex records
/// such a session with no model at all.
pub const CODEX_DEFAULT: &str = "gpt-6.1-sol";

/// Everything the panels read on such a machine.
#[derive(Debug, Clone)]
pub struct CrewLike {
    /// Tuesday afternoon, in a week that has spent nothing yet.
    pub now: OffsetDateTime,
    pub names: DisplayNames,
    /// Everything so far, with the crew's stats and what ran through Argonauts Code.
    pub usage: UsageSummary,
    /// The Overview's numbers: the CLIs' own history with the crew's two weeks.
    pub overview: OverviewData,
    /// The last workflow's sessions on every CLI, each as it left its window. The main
    /// conversation has had no message yet, so nothing has counted it.
    pub context: ContextView,
}

impl CrewLike {
    /// The app's state as the panels read it, before the first message.
    pub fn state(&self) -> AppState {
        let mut state = AppState::default();
        state.usage = Some(Arc::new(self.usage.clone()));
        state.names = self.names.clone();
        state
    }
}

pub fn crew_like() -> CrewLike {
    let names = names();
    let stats = crew_stats();
    let usage = usage(stats);
    CrewLike {
        now: now(),
        overview: overview(&usage),
        context: context(),
        names,
        usage,
    }
}

fn now() -> OffsetDateTime {
    datetime!(2026-10-27 16:15 UTC)
}

/// The first day the crew worked. Its history begins here, so the last 14 days cover it all.
fn first_day() -> Date {
    date!(2026 - 10 - 14)
}

/// How the crew's work fell over the 14 days of history, oldest first: busy midweek, a
/// Saturday off, and nothing since the week began on Sunday the 25th.
const DAY_WEIGHTS: [u64; 14] = [2, 4, 3, 0, 1, 5, 6, 4, 3, 2, 1, 0, 0, 0];

/// One model of the crew as its records spell it, with what it did through Argonauts Code and
/// what this computer has seen of it in all.
struct Member {
    agent: &'static str,
    /// `None` for the sessions that ran their CLI's default and never said which model.
    model: Option<&'static str>,
    /// Tokens that ran through Argonauts Code.
    here: u64,
    /// Tokens this computer has seen in all: the CLI's own records, which the import reads
    /// for Claude Code and Codex, with what ran here.
    in_all: u64,
    /// What `here` cost in dollars and whether at list prices; `None` for a model served on
    /// this machine.
    cost: Option<(f64, bool)>,
    /// The time its sessions took, together.
    took_secs: u64,
    /// What it did in each role. A zero is a session that ended before it finished anything.
    work: &'static [(StatsRole, u32)],
    /// How many of its tasks review sent back once, and how many twice.
    sent_back: [u32; 2],
}

impl Member {
    fn local(&self) -> bool {
        self.cost.is_none()
    }

    fn cost_usd(&self) -> f64 {
        self.cost.map_or(0.0, |(usd, _)| usd)
    }

    fn estimated(&self) -> bool {
        self.cost.is_some_and(|(_, estimated)| estimated)
    }

    /// `tokens` and `usd` as this member's records hold them: three in a hundred tokens are
    /// output, and most of the input is read from cache.
    fn spend(&self, tokens: u64, usd: f64) -> Spend {
        let output = tokens * 3 / 100;
        Spend {
            input_tokens: tokens - output,
            cached_tokens: (tokens - output) * 7 / 10,
            output_tokens: output,
            cost_usd: usd,
            cost_estimated: tokens > 0 && self.estimated(),
            cost_partial: false,
        }
    }
}

/// The crew, then the models only the CLIs' own records hold. Within a CLI, the model that
/// ran most here is rarely the one this computer has seen most, which is what a ranking
/// over the whole history gets wrong.
const MEMBERS: [Member; 15] = [
    Member {
        agent: CLAUDE,
        model: Some("claude-haiku-5-5"),
        here: 6_503_000,
        in_all: 41_200_000,
        cost: Some((0.379, true)),
        took_secs: 1_289,
        work: &[
            (Orchestrator, 7),
            (Worker, 9),
            (Reviewer, 12),
            (Merger, 5),
            (Main, 8),
        ],
        sent_back: [2, 0],
    },
    Member {
        agent: CLAUDE,
        model: Some("claude-opus-5-5"),
        here: 1_972_000,
        in_all: 612_400_000,
        cost: Some((5.351, false)),
        took_secs: 905,
        work: &[(Orchestrator, 1), (Worker, 1), (Main, 14)],
        sent_back: [0, 0],
    },
    // Two models whose sessions ran but answered nothing: rows of no tokens, no spend and no
    // time, with a dated id and one that outranks the crew's Haiku in all.
    Member {
        agent: CLAUDE,
        model: Some("claude-haiku-4-5-20251001"),
        here: 0,
        in_all: 9_600_000,
        cost: Some((0.0, false)),
        took_secs: 0,
        work: &[(Worker, 0)],
        sent_back: [0, 0],
    },
    Member {
        agent: CLAUDE,
        model: Some("claude-opus-5"),
        here: 0,
        in_all: 96_300_000,
        cost: Some((0.0, false)),
        took_secs: 0,
        work: &[(Reviewer, 0)],
        sent_back: [0, 0],
    },
    Member {
        agent: CODEX,
        model: Some("gpt-6-luna"),
        here: 1_468_000,
        in_all: 22_400_000,
        cost: Some((0.074, true)),
        took_secs: 269,
        work: &[(Worker, 4), (Reviewer, 16), (Judge, 7)],
        sent_back: [0, 1],
    },
    Member {
        agent: CODEX,
        model: Some(CODEX_DEFAULT),
        here: 423_000,
        in_all: 150_290_000,
        cost: Some((0.250, true)),
        took_secs: 180,
        work: &[(Reviewer, 9), (Judge, 3)],
        sent_back: [0, 0],
    },
    // Codex sessions that ran its default and recorded no model. They are GPT 6.1 Sol's:
    // with the row above they make its figures on Stats and Limits.
    Member {
        agent: CODEX,
        model: None,
        here: 210_000,
        in_all: 210_000,
        cost: Some((0.124, true)),
        took_secs: 89,
        work: &[(Worker, 1), (Reviewer, 5)],
        sent_back: [0, 0],
    },
    Member {
        agent: CODEX,
        model: Some("gpt-6-astra"),
        here: 391_000,
        in_all: 310_200_000,
        cost: Some((1.551, true)),
        took_secs: 149,
        work: &[(Reviewer, 3)],
        sent_back: [0, 0],
    },
    Member {
        agent: OPENCODE,
        model: Some("ollama/qwen3-coder:30b-64k"),
        here: 1_431_000,
        in_all: 1_431_000,
        cost: None,
        took_secs: 1_949,
        work: &[(Worker, 2), (Main, 4)],
        sent_back: [1, 0],
    },
    Member {
        agent: AGY,
        model: Some("Gemini 3.8 Flash (Low)"),
        here: 198_000,
        in_all: 198_000,
        cost: Some((0.198, true)),
        took_secs: 149,
        work: &[(Judge, 2)],
        sent_back: [0, 0],
    },
    Member {
        agent: OPENCODE,
        model: Some("ollama/gemma4:e2b"),
        here: 94_000,
        in_all: 94_000,
        cost: None,
        took_secs: 209,
        work: &[(Worker, 0)],
        sent_back: [0, 0],
    },
    // Only the CLIs' own records hold these. Each outranks a crew model of its CLI in all.
    Member {
        agent: CLAUDE,
        model: Some("claude-opus-4-8"),
        here: 0,
        in_all: 140_000_000,
        cost: Some((0.0, false)),
        took_secs: 0,
        work: &[],
        sent_back: [0, 0],
    },
    Member {
        agent: CLAUDE,
        model: Some("claude-sonnet-4-5"),
        here: 0,
        in_all: 188_000_000,
        cost: Some((0.0, false)),
        took_secs: 0,
        work: &[],
        sent_back: [0, 0],
    },
    Member {
        agent: CODEX,
        model: Some("gpt-5.6-sol"),
        here: 0,
        in_all: 97_000_000,
        cost: Some((0.0, false)),
        took_secs: 0,
        work: &[],
        sent_back: [0, 0],
    },
    Member {
        agent: CODEX,
        model: Some("gpt-5.6-terra"),
        here: 0,
        in_all: 18_000_000,
        cost: Some((0.0, false)),
        took_secs: 0,
        work: &[],
        sent_back: [0, 0],
    },
];

/// The members the crew ran, whether or not they used a token.
fn crew() -> impl Iterator<Item = &'static Member> {
    MEMBERS.iter().filter(|member| !member.work.is_empty())
}

/// `total` split by `weights`, what rounding leaves going to the last part that has weight,
/// so the parts always add up to `total`.
fn split(total: u64, weights: &[u64]) -> Vec<u64> {
    let sum: u64 = weights.iter().sum();
    let mut parts: Vec<u64> = weights
        .iter()
        .map(|weight| (total * weight).checked_div(sum).unwrap_or(0))
        .collect();
    if let Some(last) = weights.iter().rposition(|weight| *weight > 0) {
        parts[last] += total - parts.iter().sum::<u64>();
    }
    parts
}

/// The part of `usd` that `weight` takes of `weights`.
fn cost_part(usd: f64, weight: u64, weights: &[u64]) -> f64 {
    let sum: u64 = weights.iter().sum();
    if sum == 0 {
        0.0
    } else {
        usd * weight as f64 / sum as f64
    }
}

/// The day `k` of the 14 days of history.
fn day(k: usize) -> Date {
    first_day() + Duration::days(k as i64)
}

/// Names as the CLIs' model lists give them. The local models' names come from OpenCode's
/// list; nothing in their ids says "(64k)".
fn names() -> DisplayNames {
    let mut names = DisplayNames::default();
    for (id, name) in [
        (CLAUDE, "Claude Code"),
        (CODEX, "Codex"),
        (AGY, "agy"),
        (OPENCODE, "OpenCode"),
    ] {
        names.clis.insert(id.to_owned(), name.to_owned());
    }
    for (agent, model, name) in [
        (
            OPENCODE,
            "ollama/qwen3-coder:30b-64k",
            "Qwen3 Coder 30B (64k)",
        ),
        (OPENCODE, "ollama/gemma4:e2b", "Gemma4 E2B"),
        (AGY, "Gemini 3.8 Flash (Low)", "Gemini 3.8 Flash (Low)"),
    ] {
        let mut entry = CatalogEntry::new(ModelRef::new(agent, Some(model.to_owned())));
        entry.name = Some(name.to_owned());
        names.catalog.models.push(entry);
    }
    for (agent, model) in [
        (CLAUDE, "claude-opus-5-5"),
        (CODEX, CODEX_DEFAULT),
        (AGY, "Gemini 3.8 Flash (Low)"),
        (OPENCODE, "ollama/qwen3-coder:30b-64k"),
    ] {
        names.defaults.insert(agent.to_owned(), model.to_owned());
    }
    names
}

/// One row of work for each role a member had, its tokens, time and cost split by what it
/// did in each.
fn work_of(member: &Member) -> Vec<WorkRow> {
    let weights: Vec<u64> = member
        .work
        .iter()
        .map(|(_, done)| u64::from(*done).max(1))
        .collect();
    let tokens = split(member.here, &weights);
    let secs = split(member.took_secs, &weights);
    member
        .work
        .iter()
        .enumerate()
        .map(|(k, (role, done))| WorkRow {
            agent: member.agent.to_owned(),
            model: member.model.map(str::to_owned),
            role: *role,
            kind: (*role == Worker).then(|| "build".to_owned()),
            spent: member.spend(
                tokens[k],
                cost_part(member.cost_usd(), weights[k], &weights),
            ),
            done: *done,
            sent_back: 0,
            took_secs: secs[k],
            sessions: (*done).max(1),
            tool_calls: done * 6,
            turns: if *role == Main { done * 3 } else { 0 },
            review: Default::default(),
        })
        .collect()
}

/// The tasks a member built, every one reviewed and merged, the first few sent back.
fn tasks_of(member: &Member) -> Vec<TaskRecord> {
    let built: u32 = member
        .work
        .iter()
        .filter(|(role, _)| *role == Worker)
        .map(|(_, done)| *done)
        .sum();
    let [once, twice] = member.sent_back;
    (0..built)
        .map(|n| {
            let sent_back = match n {
                n if n < twice => 2,
                n if n < twice + once => 1,
                _ => 0,
            };
            TaskRecord {
                agent: member.agent.to_owned(),
                model: member.model.map(str::to_owned),
                kind: Some("build".to_owned()),
                sent_back,
                reasons: vec!["a test missing".to_owned(); sent_back as usize],
                merged: true,
                reviewed: true,
            }
        })
        .collect()
}

/// A member's tokens and cost on each day it worked.
fn days_of(member: &Member) -> Vec<ModelDay> {
    split(member.here, &DAY_WEIGHTS)
        .into_iter()
        .enumerate()
        .filter(|(_, tokens)| *tokens > 0)
        .map(|(k, tokens)| ModelDay {
            day: day(k),
            agent: member.agent.to_owned(),
            model: member.model.map(str::to_owned),
            role: None,
            tokens,
            cost_usd: cost_part(member.cost_usd(), DAY_WEIGHTS[k], &DAY_WEIGHTS),
            done: 0,
        })
        .collect()
}

fn crew_stats() -> CrewStats {
    let mut work: Vec<WorkRow> = crew().flat_map(work_of).collect();
    let tasks: Vec<TaskRecord> = crew().flat_map(tasks_of).collect();
    let days: Vec<ModelDay> = crew().flat_map(days_of).collect();
    // The reviews that sent work back are the tasks' send-backs; the busiest reviewer gave them.
    let sent_back: u32 = tasks.iter().map(|task| task.sent_back).sum();
    if let Some(review) = work
        .iter_mut()
        .filter(|row| row.role == Reviewer)
        .max_by_key(|row| row.done)
    {
        review.sent_back = sent_back;
    }
    let fallback = |model: &str, from: &str, why: &str, window: &str| Fallback {
        agent: CODEX.to_owned(),
        model: Some(model.to_owned()),
        from_agent: CLAUDE.to_owned(),
        from_model: Some(from.to_owned()),
        why: why.to_owned(),
        window: Some(window.to_owned()),
    };
    CrewStats {
        work,
        tasks,
        days,
        fallbacks: vec![
            fallback(
                "gpt-6-luna",
                "claude-haiku-5-5",
                "usage limit, resets 3:45pm",
                "five_hour",
            ),
            fallback(
                "gpt-6-astra",
                "claude-opus-5-5",
                "usage limit, resets Thursday",
                "seven_day",
            ),
        ],
        workflows: 22,
        took_secs: 3600 + 39 * 60,
        outcomes: Outcomes {
            runs: 22,
            runs_merged: 19,
            runs_replanned: 2,
            sided_with_worker: 2,
            sided_with_reviewer: 1,
            merges_clean: 4,
            merges_settled: 1,
            ..Outcomes::default()
        },
        settings: StatsSettings::default(),
        sources: Default::default(),
        problems: Vec::new(),
    }
}

fn usage(stats: CrewStats) -> UsageSummary {
    // What each model spent is what its rows of work spent, so Limits and Stats agree.
    let mut by_agent: Vec<UsageShare> = Vec::new();
    for member in crew().filter(|member| member.here > 0) {
        let spent = stats
            .work
            .iter()
            .filter(|row| row.agent == member.agent && row.model.as_deref() == member.model)
            .fold(Spend::default(), |sum, row| sum.plus(&row.spent));
        by_agent.push(UsageShare {
            agent: member.agent.to_owned(),
            model: member.model.map(str::to_owned),
            agent_id: member.agent.to_owned(),
            spent,
            pays_per_token: false,
            local: member.local(),
            billing: None,
            // Nothing spent since Sunday.
            this_week: Vec::new(),
        });
    }
    // The ledger keeps its shares by CLI and model, so equal spends stay in that order.
    by_agent.sort_by(|a, b| (&a.agent, &a.model).cmp(&(&b.agent, &b.model)));
    by_agent.sort_by(|a, b| b.spent.cost_usd.total_cmp(&a.spent.cost_usd));
    let total = by_agent
        .iter()
        .fold(Spend::default(), |sum, share| sum.plus(&share.spent));
    let history = (0..DAY_WEIGHTS.len())
        .map(|k| DaySpend {
            day: day(k),
            spent: stats.days.iter().filter(|part| part.day == day(k)).fold(
                Spend::default(),
                |sum, part| {
                    let member = member_of(&part.agent, part.model.as_deref());
                    sum.plus(&member.spend(part.tokens, part.cost_usd))
                },
            ),
        })
        .collect();
    let argonauts = through_argonauts(&stats);
    UsageSummary {
        period: UsagePeriod::All,
        week_starts: Default::default(),
        total,
        by_agent,
        history,
        runs: runs(),
        plan_windows: Vec::new(),
        plans: plans(),
        conversation: None,
        spend_limit: None,
        plan_limit: None,
        progress: progress(),
        task_limit: None,
        limits: super::limits(),
        stats: Some(stats),
        argonauts: Some(argonauts),
        all_time_models: all_time_models(),
    }
}

fn member_of(agent: &str, model: Option<&str>) -> &'static Member {
    MEMBERS
        .iter()
        .find(|member| member.agent == agent && member.model == model)
        .expect("a member of the crew")
}

fn through_argonauts(stats: &CrewStats) -> ThroughArgonauts {
    let reviewed = stats.tasks.iter().filter(|task| task.reviewed).count();
    let first_time = stats
        .tasks
        .iter()
        .filter(|task| task.reviewed && task.sent_back == 0)
        .count();
    ThroughArgonauts {
        since: Some(first_day()),
        workflows: stats.workflows,
        by_workflow: vec![
            ("council".to_owned(), 14),
            ("maker-checker".to_owned(), 5),
            ("best-of-n".to_owned(), 3),
        ],
        merged: stats.tasks.iter().filter(|task| task.merged).count() as u32,
        first_pass: Some(first_time as f64 / reviewed as f64),
        fallbacks: stats.fallbacks.len() as u32,
        for_limit: stats.fallbacks.len() as u32,
        answered: 3,
    }
}

/// Every model in the order the app ranks their colours: those that ran through Argonauts
/// Code by their tokens here, then those only the CLIs' records hold by their count on this
/// computer. Each carries its tokens here. Nothing ran today, so no model is new today.
fn all_time_models() -> Vec<ModelTokens> {
    let mut members: Vec<&Member> = MEMBERS.iter().collect();
    members.sort_by_key(|member| (Reverse(member.here), Reverse(member.in_all)));
    members
        .into_iter()
        .map(|member| ModelTokens {
            agent: member.agent.to_owned(),
            model: member.model.map(str::to_owned),
            tokens: member.here,
        })
        .collect()
}

/// What a CLI's own usage screen said two minutes ago. A plan counts the CLI's use outside
/// Argonauts Code too, so its windows fill on days the crew spent nothing.
fn plan(
    agent: &str,
    name: &str,
    plan: &str,
    windows: &[(&str, &str, &str, f64, OffsetDateTime)],
) -> PlanUsage {
    let mut live = LiveUsage::new(
        AgentId(agent.to_owned()),
        PlanUsageSource::AppServer,
        now() - Duration::minutes(2),
    );
    live.plan = Some(plan.to_owned());
    live.windows = windows
        .iter()
        .map(|(key, label, title, used, resets)| {
            LiveWindow::new(*key, *label, *title)
                .used(Some(*used))
                .resets_at(Some(*resets))
        })
        .collect();
    PlanUsage {
        agent: agent.to_owned(),
        name: name.to_owned(),
        plan: live.plan.clone(),
        in_use: agent == CLAUDE,
        windows: live.plan_usage_readings(),
        live: Some(live),
        ..PlanUsage::default()
    }
}

fn plans() -> Vec<PlanUsage> {
    vec![
        plan(
            CLAUDE,
            "Claude Code",
            "Max",
            &[
                (
                    "five_hour",
                    "session",
                    "Current session",
                    0.12,
                    datetime!(2026-10-27 19:00 UTC),
                ),
                (
                    "seven_day",
                    "week",
                    "Current week (all models)",
                    0.44,
                    datetime!(2026-10-29 22:00 UTC),
                ),
            ],
        ),
        plan(
            CODEX,
            "Codex",
            "Plus",
            &[
                (
                    "five_hour",
                    "5h",
                    "5h limit",
                    0.0,
                    datetime!(2026-10-27 21:15 UTC),
                ),
                (
                    "weekly",
                    "week",
                    "Weekly limit",
                    0.31,
                    datetime!(2026-10-30 09:00 UTC),
                ),
            ],
        ),
        plan(
            AGY,
            "agy",
            "Pro",
            &[(
                "day",
                "day",
                "Daily limit",
                0.06,
                datetime!(2026-10-28 00:00 UTC),
            )],
        ),
        PlanUsage {
            agent: OPENCODE.to_owned(),
            name: "OpenCode".to_owned(),
            local: true,
            ..PlanUsage::default()
        },
    ]
}

/// The newest of the period's workflows, the last of them with a name too long for any row.
fn runs() -> Vec<RunSummary> {
    let run = |n: u32, name: &str, workflow: &str, state, updated, cost| RunSummary {
        run: RunId::new(format!("run-{n:02}")).expect("a run id"),
        title: name.replace('-', " "),
        name: Some(name.to_owned()),
        mode: RunMode::Crew,
        workflow: workflow.to_owned(),
        state,
        stopped_at: String::new(),
        updated,
        spent: Spend {
            cost_usd: cost,
            cost_estimated: true,
            ..Spend::default()
        },
        sessions_to_resume: 0,
        waiting: Vec::new(),
        live: false,
        remind: false,
        shape_known: true,
    };
    vec![
        run(
            22,
            "keep-the-retry-queue-in-order-when-a-worker-restarts-early",
            "council",
            RunState::Merged,
            last_run_ended(),
            0.61,
        ),
        run(
            21,
            "daily-column",
            "maker-checker",
            RunState::Merged,
            datetime!(2026-10-24 15:10 UTC),
            0.34,
        ),
        run(
            20,
            "cache-the-price-table-lookup",
            "best-of-n",
            RunState::Stopped,
            datetime!(2026-10-23 11:05 UTC),
            0.12,
        ),
    ]
}

fn last_run_ended() -> OffsetDateTime {
    datetime!(2026-10-24 18:40 UTC)
}

fn progress() -> Vec<RunProgress> {
    let progress = |n: u32, tasks_done, tasks, ended: OffsetDateTime, took: Duration| RunProgress {
        run: RunId::new(format!("run-{n:02}")).expect("a run id"),
        tasks_done,
        tasks,
        started: Some(ended - took),
    };
    vec![
        progress(22, 3, 3, last_run_ended(), Duration::minutes(38)),
        progress(
            21,
            2,
            2,
            datetime!(2026-10-24 15:10 UTC),
            Duration::minutes(21),
        ),
        progress(
            20,
            0,
            1,
            datetime!(2026-10-23 11:05 UTC),
            Duration::minutes(6),
        ),
    ]
}

/// The last workflow's sessions, one or more on each CLI, as each left its window when the
/// workflow ended. Only Claude Code says what fills a window, and this one never said.
fn context() -> ContextView {
    let run = RunId::new("run-22").expect("a run id");
    let row =
        |n: u32, label: &str, agent: &str, model: Option<&str>, used: u64, window: Option<u64>| {
            ContextRow {
                run: Some(run.clone()),
                session: SessionKey::numbered(n),
                label: label.to_owned(),
                worker: ModelRef::new(agent, model.map(str::to_owned)),
                used,
                window,
                cap: window.map(|window| window / 100 * 95),
                tool_calls: (used / 4_000) as u32,
                spent: Spend::default(),
                rotations: 0,
                // agy says only what its sessions used in all, so how full one was is worked out.
                estimated: agent == AGY,
                counts: agent == CLAUDE,
                categories: Vec::new(),
                recorded: None,
                ended: Some(SessionLeft {
                    at: last_run_ended(),
                    how: EndedHow::Done,
                    fallback_for: None,
                }),
                answered_for: None,
            }
        };
    let rows = vec![
        row(
            1,
            "orchestrator",
            CLAUDE,
            Some("claude-haiku-5-5"),
            61_400,
            Some(200_000),
        ),
        row(2, "build:limits-list", CODEX, None, 41_000, Some(272_000)),
        row(
            3,
            "build:daily-column",
            CODEX,
            Some("gpt-6-luna"),
            88_400,
            Some(272_000),
        ),
        row(
            4,
            "review:limits-list",
            AGY,
            Some("Gemini 3.8 Flash (Low)"),
            12_800,
            None,
        ),
        row(
            5,
            "build:spend-figures-under-the-spend-card",
            OPENCODE,
            Some("ollama/qwen3-coder:30b-64k"),
            61_900,
            Some(65_536),
        ),
        row(
            6,
            "build:legend",
            OPENCODE,
            Some("ollama/gemma4:e2b"),
            0,
            Some(32_768),
        ),
    ];
    ContextView {
        rows,
        finished: vec![RunFinishedAt {
            run,
            at: last_run_ended(),
        }],
        local: vec!["ollama".to_owned()],
        main_window: None,
        main_start: None,
    }
}

/// When each CLI's own records begin. agy's and OpenCode's aren't imported, so theirs begin
/// with the crew.
fn since(agent: &str) -> Date {
    match agent {
        CLAUDE => date!(2025 - 10 - 09),
        CODEX => date!(2026 - 02 - 03),
        _ => first_day(),
    }
}

/// How a CLI counts a model's tokens: nearly all of them read from cache.
fn tokens(total: u64) -> Tokens {
    let input = total * 4 / 100;
    let output = total / 100;
    let cache_write = total * 5 / 100;
    Tokens {
        input,
        output,
        cache_write,
        cache_read: total - input - output - cache_write,
    }
}

/// Tokens by the hour the answers came, midnight first.
const HOUR_WEIGHTS: [u64; 24] = [
    1, 0, 0, 0, 0, 0, 1, 2, 4, 6, 7, 7, 5, 6, 8, 8, 7, 6, 5, 5, 6, 7, 5, 2,
];

fn overview(usage: &UsageSummary) -> OverviewData {
    let today = now().date();
    let mut data = OverviewData::empty(OverviewPeriod::All, today);
    data.import = ImportStatus::Done;
    // A model's days: what its CLI recorded before the crew began, more each month and less
    // at weekends, then the crew's days.
    for member in &MEMBERS {
        let before: Vec<Date> = days_between(since(member.agent), first_day()).collect();
        let weights: Vec<u64> = before
            .iter()
            .enumerate()
            .map(|(k, day)| match day.weekday() {
                Weekday::Saturday | Weekday::Sunday => 1,
                _ => 3 + k as u64 / 30,
            })
            .collect();
        let history = split(member.in_all - member.here, &weights);
        let crew = split(member.here, &DAY_WEIGHTS);
        let dated = before
            .into_iter()
            .zip(history)
            .chain((0..DAY_WEIGHTS.len()).map(day).zip(crew));
        for (day, tokens) in dated.filter(|(_, tokens)| *tokens > 0) {
            data.days.push(OverviewDay {
                day,
                tokens,
                agent: member.agent.to_owned(),
                model: member.model.map(str::to_owned),
            });
        }
    }
    data.days.sort_by_key(|part| part.day);
    data.models = MEMBERS
        .iter()
        .map(|member| OverviewModel {
            agent: member.agent.to_owned(),
            model: member.model.map(str::to_owned),
            tokens: tokens(member.in_all),
            api_value: (!member.local()).then(|| member.in_all as f64 / 1e6 * list_price(member)),
            sessions: (member.in_all / 1_500_000).max(1) as u32,
        })
        .collect();
    data.models
        .sort_by_key(|model| Reverse(model.tokens.total()));
    for model in &data.models {
        data.total.add(&model.tokens);
    }
    data.api_value = data.models.iter().filter_map(|model| model.api_value).sum();
    data.sources = sources(&data);
    let by_day = tokens_by_day(&data.days);
    data.active_days = by_day.len() as u32;
    data.span_days = ((today - since(CLAUDE)).whole_days() + 1) as u32;
    // Nothing since Saturday the 24th, so no days in a row reach today.
    data.streak = 0;
    data.longest_streak = 5;
    data.peak_day = by_day
        .iter()
        .max_by_key(|(_, tokens)| **tokens)
        .map(|(day, _)| *day);
    data.hours = split(data.total.total(), &HOUR_WEIGHTS);
    data.weeks = weeks(&data.days, today);
    data.months = months(&data.days);
    data.model_days = model_days(&data);
    data.folders = vec![
        (
            "/Users/you/code/argonauts-code".to_owned(),
            data.total.total() * 61 / 100,
        ),
        (
            "/Users/you/code/a-folder-whose-name-is-longer-than-any-column-gives-it".to_owned(),
            data.total.total() * 27 / 100,
        ),
        (
            "/Users/you/code/minitorch".to_owned(),
            data.total.total() * 12 / 100,
        ),
    ];
    data.argonauts = usage.argonauts.clone();
    data.subagent_tokens = data.total.total() / 5;
    data.longest_session_secs = 9 * 3600 + 12 * 60;
    data.all_time_models = usage.all_time_models.clone();
    data
}

/// Dollars a million tokens comes to at list prices, cache reads counted at theirs.
fn list_price(member: &Member) -> f64 {
    match member.agent {
        CLAUDE => 1.2,
        CODEX => 0.9,
        _ => 0.3,
    }
}

fn days_between(first: Date, before: Date) -> impl Iterator<Item = Date> {
    std::iter::successors(Some(first), |day| day.next_day()).take_while(move |day| *day < before)
}

fn tokens_by_day(days: &[OverviewDay]) -> BTreeMap<Date, u64> {
    let mut by_day = BTreeMap::new();
    for part in days {
        *by_day.entry(part.day).or_default() += part.tokens;
    }
    by_day
}

fn sources(data: &OverviewData) -> Vec<CliSource> {
    let mut sources: Vec<(u64, CliSource)> = [CLAUDE, CODEX, OPENCODE, AGY]
        .into_iter()
        .map(|agent| {
            let models = data.models.iter().filter(|model| model.agent == agent);
            let tokens = models.clone().map(|model| model.tokens.total()).sum();
            let source = CliSource {
                agent: agent.to_owned(),
                since: Some(since(agent)),
                imported: matches!(agent, CLAUDE | CODEX),
                sessions: models.map(|model| model.sessions).sum(),
            };
            (tokens, source)
        })
        .collect();
    sources.sort_by_key(|(tokens, _)| Reverse(*tokens));
    sources.into_iter().map(|(_, source)| source).collect()
}

/// The last 26 weeks by CLI, oldest first, each from its Sunday.
fn weeks(days: &[OverviewDay], today: Date) -> Vec<OverviewWeek> {
    let this_week = today - Duration::days(i64::from(today.weekday().number_days_from_sunday()));
    (0..26)
        .rev()
        .map(|back| {
            let start = this_week - Duration::weeks(back);
            let end = start + Duration::weeks(1);
            let within = days
                .iter()
                .filter(|part| part.day >= start && part.day < end);
            OverviewWeek {
                start,
                by_cli: by_cli(within),
            }
        })
        .collect()
}

fn months(days: &[OverviewDay]) -> Vec<OverviewMonth> {
    let mut firsts: Vec<Date> = days
        .iter()
        .map(|part| part.day.replace_day(1).expect("a first of the month"))
        .collect();
    firsts.dedup();
    firsts
        .into_iter()
        .map(|month| OverviewMonth {
            month,
            by_cli: by_cli(
                days.iter()
                    .filter(|part| part.day.replace_day(1).ok() == Some(month)),
            ),
        })
        .collect()
}

fn by_cli<'a>(days: impl Iterator<Item = &'a OverviewDay>) -> Vec<(String, u64)> {
    let mut by_cli: Vec<(String, u64)> = Vec::new();
    for part in days {
        match by_cli.iter_mut().find(|(agent, _)| *agent == part.agent) {
            Some((_, tokens)) => *tokens += part.tokens,
            None => by_cli.push((part.agent.clone(), part.tokens)),
        }
    }
    by_cli.sort_by_key(|(_, tokens)| Reverse(*tokens));
    by_cli
}

/// Each day with work, the tokens of the three models the Overview lists first.
fn model_days(data: &OverviewData) -> Vec<OverviewModelDay> {
    let top: Vec<(&str, Option<&str>)> = data
        .models
        .iter()
        .take(3)
        .map(|model| (model.agent.as_str(), model.model.as_deref()))
        .collect();
    tokens_by_day(&data.days)
        .into_keys()
        .map(|day| OverviewModelDay {
            day,
            tokens: top
                .iter()
                .map(|(agent, model)| {
                    data.days
                        .iter()
                        .filter(|part| {
                            part.day == day
                                && part.agent == *agent
                                && part.model.as_deref() == *model
                        })
                        .map(|part| part.tokens)
                        .sum()
                })
                .collect(),
        })
        .collect()
}
