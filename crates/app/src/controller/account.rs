//! You and your agents' accounts, whatever CLI the main agent is: `/usage` shows plan usage
//! and spend, `/login` and `/logout` hand the terminal to an agent CLI's own login, and
//! `/release-notes` and `/feedback` are about Argonauts itself.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context as _, bail};
use argonauts_core::api::{
    CardBrief, PlanUsage, RunProgress, RunSummary, UsagePeriod, UsageSummary,
};
use argonauts_core::app::{AppEvent, QuotaChip, SettingRow, ViewData};
use argonauts_core::events::card::CardState;
use argonauts_core::limits::{HealthBoard, HealthState, QuotaBoard, QuotaReading, QuotaStatus};
use argonauts_core::names::VendorId;
use argonauts_core::names::WorkerRef;
use argonauts_core::plan_usage::LiveUsage;
use argonauts_core::settings::Settings;
use argonauts_core::settings::budget::{BudgetAmount, BudgetSettings, BudgetUnit};
use argonauts_core::settings::handoff::FallbackEntry;
use argonauts_core::settings::stats::{STATS_FILE, StatsPeriod, StatsSettings};
use argonauts_core::worker::WorkerDirectory;
use argonauts_core::{AuthMode, NoteLevel, RunId, Spend};
use time::{Duration, OffsetDateTime};

use super::settings::setting_rows;
use super::status_line::quota_chips;
use super::{Ctx, Effect};
use crate::usage_ledger::{self, UsageLedger, local_day};

const FEEDBACK_FILE: &str = "feedback.jsonl";

/// What's new, newest release first. Each entry's first line is its heading.
const RELEASE_NOTES: &[&[&str]] = &[&[
    "Argonauts Code 0.0.1",
    "Runs Claude Code, Codex, agy, OpenCode, and model endpoints as one crew, in a screen that \
     works like Claude Code's.",
    "Chat with the main agent; /solo and /crew hand work to one agent or the crew.",
    "The commands the agent CLIs share work whatever CLI the main agent is: /compact, /rename, \
     /branch, /fork, /btw, /recap, /copy, /export, /cd, /add-dir, /plan, /goal, /fast, \
     /output-style, /diff, /review, /security-review, /usage, /login, and /logout.",
    "Reviews go to an agent from another provider than the main agent's when the crew has one.",
]];

/// The span `/usage` adds spend and runs up over. Quota is always now.
pub(crate) fn parse_period(word: Option<&str>) -> anyhow::Result<UsagePeriod> {
    Ok(match word.map(str::to_ascii_lowercase).as_deref() {
        None | Some("day" | "daily" | "today") => UsagePeriod::Today,
        Some("week" | "weekly") => UsagePeriod::Week,
        Some("month" | "monthly") => UsagePeriod::Month,
        Some("all" | "cumulative" | "total") => UsagePeriod::All,
        Some(other) => bail!("/usage takes day, week, month, or all, not {other:?}."),
    })
}

fn title(period: UsagePeriod) -> &'static str {
    match period {
        UsagePeriod::Today => "Usage, today",
        UsagePeriod::Week => "Usage, this week",
        UsagePeriod::Month => "Usage, this month",
        UsagePeriod::All => "Usage, all time",
    }
}

/// `/usage [day|week|month|all]`: each agent's quota now, what the conversation spent, then
/// what the period spent by the usage ledger and this repo's runs in it. Reads only what's
/// recorded, so it costs nothing.
pub(crate) async fn usage(
    ctx: &mut Ctx,
    conversation: Option<Spend>,
    period: Option<String>,
) -> anyhow::Result<Vec<Effect>> {
    let period = parse_period(period.as_deref())?;
    let now = OffsetDateTime::now_utc();
    let settings = ctx.settings();
    let mut crew = vec![settings.main_agent.clone()];
    crew.extend(settings.crew.members());
    let chips = quota_chips(
        &crew,
        ctx.services.quota.as_ref(),
        ctx.services.health.as_ref(),
    );
    let mut lines: Vec<String> = chips.iter().map(|chip| quota_line(chip, now)).collect();
    if let Some(spent) = conversation {
        lines.push(format!("This conversation · {}", usd(&spent)));
    }
    let quotas = crew
        .iter()
        .flat_map(|worker| ctx.services.quota.windows(&worker.vendor()))
        .collect();
    let listed = ctx.services.conductor.list().await;
    let ledger = UsageLedger::new(&ctx.env.paths);
    let runs = listed.as_deref().unwrap_or_default();
    let span = (period, ctx.settings().periods.week_starts);
    let summary = usage_ledger::summary(&ledger, span, local_day(now), runs, quotas);
    lines.push(format!(
        "Spent · {} · {} tokens",
        usd(&summary.total),
        argonauts_tui::format::tokens(summary.total.total_tokens())
    ));
    lines.push(match listed {
        Ok(_) => {
            let total = summary
                .runs
                .iter()
                .fold(Spend::default(), |sum, run| sum.plus(&run.spent));
            let runs = match summary.runs.len() {
                1 => "1 run".to_owned(),
                n => format!("{n} runs"),
            };
            format!("Runs in this repo · {runs} · {}", usd(&total))
        }
        Err(err) => format!("Runs in this repo · couldn't list them: {err}"),
    });
    ctx.send(AppEvent::Report {
        title: title(period).to_owned(),
        lines,
    });
    Ok(Vec::new())
}

/// The period `/stats` shows: the one it was given, else the one `mods/stats.toml` sets.
pub(crate) fn stats_period(ctx: &Ctx, given: Option<&str>) -> UsagePeriod {
    let given = given.and_then(StatsPeriod::parse);
    given
        .unwrap_or_else(|| {
            let home = ctx.env.paths.home();
            StatsSettings::read(home, Some(&ctx.env.repo_root)).0.period
        })
        .usage()
}

/// `/limits print`: what counts toward the limits you set, once, as lines in the
/// conversation: each CLI's plan usage now, and what today spent, each with its limit or
/// the word that none is set.
pub(crate) async fn limits_report(
    ctx: &mut Ctx,
    conversation: Option<Spend>,
) -> anyhow::Result<Vec<Effect>> {
    let now = OffsetDateTime::now_utc();
    let summary = usage_summary(ctx, conversation, UsagePeriod::Today).await;
    let settings = ctx.settings();
    let mut crew = vec![settings.main_agent.clone()];
    crew.extend(settings.crew.members());
    let chips = quota_chips(
        &crew,
        ctx.services.quota.as_ref(),
        ctx.services.health.as_ref(),
    );
    let mut lines: Vec<String> = chips.iter().map(|chip| quota_line(chip, now)).collect();
    lines.push(match summary.plan_limit {
        Some(limit) => format!("Plan usage limit · {:.0}% of a window", limit * 100.0),
        None => "Plan usage · no limit set".to_owned(),
    });
    let spent = usd(&summary.total);
    lines.push(match summary.spend_limit {
        Some(limit) => format!("Spent today · {spent} of ${limit:.2}"),
        None => format!("Spent today · {spent} · no limit set"),
    });
    ctx.send(AppEvent::Report {
        title: "Limits".to_owned(),
        lines,
    });
    Ok(Vec::new())
}

/// Asks this far apart mean the panel was closed in between, so the next ask opens it again.
const OPENED_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// A read started this long ago that hasn't landed isn't coming.
const READ_GIVES_UP: std::time::Duration = std::time::Duration::from_secs(45);

/// After a reset passes, the CLI is read again at most this often, so one whose read fails
/// isn't asked every second.
const RESET_REREAD_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// `/usage` and its tab of the Overview: answers with what the tab draws, and reads again
/// any plan usage that has grown old, for the next time it's asked.
pub(crate) async fn usage_view(
    ctx: &mut Ctx,
    conversation: Option<Spend>,
    period: UsagePeriod,
) -> anyhow::Result<Vec<Effect>> {
    let summary = usage_summary(ctx, conversation, period).await;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Usage {
            summary: Box::new(summary),
        }),
    });
    Ok(Vec::new())
}

/// Plan usage for every CLI in the crew, spend by model, tokens, 14 days of history and the
/// period's workflows. Reads only what's recorded.
async fn usage_summary(
    ctx: &Ctx,
    conversation: Option<Spend>,
    period: UsagePeriod,
) -> UsageSummary {
    let now = OffsetDateTime::now_utc();
    let settings = ctx.settings();
    let mut crew = vec![settings.main_agent.clone()];
    crew.extend(settings.crew.members());
    let quota = &ctx.services.quota;
    let limits = setting_rows(&ctx.settings, Some("limits"), None);
    let plan_limit = limit_of(&limits, PLAN_USAGE_LIMIT);
    let plans = {
        let named = Named {
            cli: &|id| ctx.names.harness(id),
            worker: &|worker| ctx.names.worker(worker),
        };
        let boards = Boards {
            quota: quota.as_ref(),
            health: ctx.services.health.as_ref(),
        };
        plan_usage(&crew, settings, &boards, &named, plan_limit, now)
    };
    let opened = panel_opened(std::time::Instant::now());
    let plans = read_again(ctx, &crew, plans, opened, now);
    let quotas = plans.iter().flat_map(|plan| plan.windows.clone()).collect();
    let listed = ctx.services.conductor.list().await;
    let ledger = UsageLedger::new(&ctx.env.paths);
    let runs = listed.as_deref().unwrap_or_default();
    let span = (period, ctx.settings().periods.week_starts);
    let mut summary = usage_ledger::summary(&ledger, span, local_day(now), runs, quotas);
    if let Some(stats) = &mut summary.stats {
        let clis: Vec<String> = stats.work.iter().map(|row| row.agent.clone()).collect();
        let defaults = super::names::cli_defaults(ctx, clis.iter().map(String::as_str));
        crate::work_ledger::name_unnamed_models(stats, &defaults);
        let home = ctx.env.paths.home();
        let (settings, sources, problems) =
            StatsSettings::read_with_sources(home, Some(&ctx.env.repo_root));
        // The stats page names the file the way the rest of the screen does.
        let files = [
            home.join("mods").join(STATS_FILE),
            ctx.env.repo_root.join(".argonauts/mods").join(STATS_FILE),
        ];
        stats.problems = problems
            .into_iter()
            .map(|problem| {
                files.iter().fold(problem, |problem, file| {
                    problem.replace(&file.display().to_string(), &ctx.env.shown(file))
                })
            })
            .collect();
        stats.settings = settings;
        stats.sources = sources;
    }
    UsageSummary {
        plans,
        conversation,
        progress: run_progress(ctx, &summary.runs).await,
        task_limit: task_limit(&settings.limits.budget),
        spend_limit: limit_of(&limits, SPEND_LIMIT),
        plan_limit,
        limits,
        ..summary
    }
}

/// The limits the cards count toward, by the keys `/limits` lists them under: dollars a day,
/// and the share of a plan window from 0 to 1.
const SPEND_LIMIT: &str = "limits.spend_per_day";
const PLAN_USAGE_LIMIT: &str = "limits.plan_usage";

/// A limit's value in effect, from the rows `/limits` lists. Unset or zero is no limit.
fn limit_of(limits: &[SettingRow], key: &str) -> Option<f64> {
    let row = limits.iter().find(|row| row.key == key)?;
    let limit = row.value.trim().parse::<f64>().ok()?;
    (limit > 0.0).then_some(limit)
}

/// The limit of a task that has none of its own, in the unit you set it in. The keys are
/// one choice, so the first one set is the only one.
fn task_limit(limits: &BudgetSettings) -> Option<BudgetAmount> {
    let amount = |unit, amount| BudgetAmount { unit, amount };
    let usd = limits.per_task_usd.map(|usd| amount(BudgetUnit::Usd, usd));
    usd.or_else(|| {
        let tokens = limits.per_task_tokens;
        tokens.map(|tokens| amount(BudgetUnit::Tokens, tokens as f64))
    })
    .or_else(|| {
        let share = limits.per_task_plan_usage;
        share.map(|share| amount(BudgetUnit::Quota, share))
    })
    .or_else(|| {
        let minutes = limits.per_task_minutes;
        minutes.map(|minutes| amount(BudgetUnit::Minutes, minutes))
    })
    .filter(|limit| limit.amount > 0.0)
}

/// The workflows card has rows for this many runs, so only they are read.
const PROGRESS_RUNS: usize = 8;

/// How far the newest runs got. A run's tasks are read again only when it has moved since
/// the last ask, so a finished run's log is read once however long the Usage tab stays open.
async fn run_progress(ctx: &Ctx, runs: &[RunSummary]) -> Vec<RunProgress> {
    static READ: Mutex<BTreeMap<RunId, (OffsetDateTime, RunProgress)>> =
        Mutex::new(BTreeMap::new());
    let known = |run: &RunSummary| {
        let read = READ.lock().unwrap_or_else(PoisonError::into_inner);
        read.get(&run.run)
            .filter(|(updated, _)| *updated == run.updated)
            .map(|(_, progress)| progress.clone())
    };
    let mut progress = Vec::new();
    for run in runs.iter().take(PROGRESS_RUNS) {
        if let Some(known) = known(run) {
            progress.push(known);
            continue;
        }
        let Ok(status) = ctx.services.conductor.status(&run.run).await else {
            continue;
        };
        let started = crate::run_files::read_start(&ctx.env.run_dir(&run.run))
            .ok()
            .map(|start| start.created);
        let read = tasks_of(&run.run, &status.cards, started);
        READ.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run.run.clone(), (run.updated, read.clone()));
        progress.push(read);
    }
    progress
}

/// A run's tasks as `done of all`. A task that was cancelled or that another took over is
/// no longer one the run means to do.
fn tasks_of(run: &RunId, tasks: &[CardBrief], started: Option<OffsetDateTime>) -> RunProgress {
    let count = |keep: &dyn Fn(CardState) -> bool| {
        let kept = tasks.iter().filter(|task| keep(task.state)).count();
        u32::try_from(kept).unwrap_or(u32::MAX)
    };
    RunProgress {
        run: run.clone(),
        tasks_done: count(&|state| state == CardState::Integrated),
        tasks: count(&|state| !matches!(state, CardState::Cancelled | CardState::TakenOver)),
        started,
    }
}

/// How the screen names a CLI and an agent.
struct Named<'a> {
    cli: &'a dyn Fn(&str) -> String,
    worker: &'a dyn Fn(&WorkerRef) -> String,
}

/// Where each CLI's plan usage and health are read from.
struct Boards<'a> {
    quota: &'a dyn QuotaBoard,
    health: &'a dyn HealthBoard,
}

/// One entry per CLI in `crew` order, with the windows that haven't reset since they were
/// read, the shortest first. A reading of a window that's over says nothing about the one
/// that followed it, so it's dropped rather than shown as old. A CLI whose plan ran out,
/// that can't be reached, or that has a window as full as `limit` lets it get, names who
/// takes its tasks.
fn plan_usage(
    crew: &[WorkerRef],
    settings: &Settings,
    boards: &Boards<'_>,
    named: &Named<'_>,
    limit: Option<f64>,
    now: OffsetDateTime,
) -> Vec<PlanUsage> {
    let Boards { quota, health } = *boards;
    let mut plans: Vec<PlanUsage> = Vec::new();
    for worker in crew {
        let vendor = worker.vendor();
        if plans.iter().any(|plan| plan.agent == vendor.0) {
            continue;
        }
        let pays_per_token =
            settings.endpoints.contains_key(&vendor.0) || bills_api_key(settings, &vendor);
        // Only the CLI's own live reading of the account signed in now. One a failed read or
        // a sign-in has cast doubt on is kept apart, to be drawn as from before.
        let kept = quota
            .live(&vendor)
            .filter(|live| live.shown_at(now) && !pays_per_token);
        let (live, last) = match kept {
            Some(kept) if kept.current_at(now, quota.doubted_at(&vendor)) => (Some(kept), None),
            kept => (None, kept),
        };
        let mut windows: Vec<QuotaReading> = live
            .iter()
            .flat_map(LiveUsage::quota_readings)
            .filter(|reading| reading.resets_at.is_none_or(|resets| resets > now))
            .collect();
        windows.sort_by_key(|reading| window_rank(reading.window.as_deref()));
        let down = health.state(&vendor).state == HealthState::Down;
        let out = windows
            .iter()
            .any(|reading| reading.status == QuotaStatus::Rejected && !reading.wrap_up);
        let at_limit = limit.is_some_and(|limit| {
            let reached = |reading: &QuotaReading| reading.used.is_some_and(|used| used >= limit);
            windows.iter().any(reached)
        });
        plans.push(PlanUsage {
            name: (named.cli)(&vendor.0),
            plan: live.as_ref().and_then(|live| live.plan.clone()),
            in_use: plans.is_empty(),
            down,
            pays_per_token,
            moved_to: (down || out || at_limit)
                .then(|| takes_over(settings, &vendor))
                .flatten()
                .map(named.worker),
            windows,
            live,
            reading: false,
            failed: quota.failed(&vendor),
            last,
            agent: vendor.0,
        });
    }
    plans
}

/// The CLI bills an API key rather than a plan, so there's no plan to read.
fn bills_api_key(settings: &Settings, vendor: &VendorId) -> bool {
    settings
        .agents
        .get(&vendor.0)
        .is_some_and(|agent| agent.auth == AuthMode::ApiKey)
}

/// Shorter windows first: the five-hour one, a day's, the week's, then anything else.
fn window_rank(window: Option<&str>) -> u8 {
    let window = window.unwrap_or_default();
    if window.contains("hour") {
        0
    } else if window.contains("seven") || window.contains("week") {
        2
    } else if window.contains("day") {
        1
    } else {
        3
    }
}

/// Who the fallback order gives `vendor`'s work to: the first entry on another CLI.
fn takes_over<'a>(settings: &'a Settings, vendor: &VendorId) -> Option<&'a WorkerRef> {
    settings
        .fallback
        .order
        .iter()
        .map(|entry| match entry {
            FallbackEntry::Worker(worker) => worker,
            FallbackEntry::WithRules(rules) => &rules.worker,
        })
        .find(|worker| worker.vendor() != *vendor)
}

/// The CLIs being read live now, with when each read started.
static READING: Mutex<BTreeMap<String, std::time::Instant>> = Mutex::new(BTreeMap::new());

/// Whether this ask opens the panel: the first, or the first after a pause. Each ask counts.
fn panel_opened(now: std::time::Instant) -> bool {
    static LAST_ASK: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST_ASK.lock().unwrap_or_else(PoisonError::into_inner);
    let opened = last.is_none_or(|at| now.saturating_duration_since(at) >= OPENED_AFTER);
    *last = Some(now);
    opened
}

/// Reads each CLI live again when the panel opens, and when a window it read has reset
/// since, at most once a minute for that. A CLI being read already isn't asked twice. Each
/// plan says whether a read is on its way; the answer lands on the board for the next ask.
fn read_again(
    ctx: &Ctx,
    crew: &[WorkerRef],
    mut plans: Vec<PlanUsage>,
    opened: bool,
    now: OffsetDateTime,
) -> Vec<PlanUsage> {
    static RESET_ASKED: Mutex<BTreeMap<String, std::time::Instant>> = Mutex::new(BTreeMap::new());
    let started = std::time::Instant::now();
    for plan in plans.iter_mut().filter(|plan| !plan.pays_per_token) {
        let reset = plan
            .live
            .as_ref()
            .is_some_and(|live| live.reset_passed(now) || live.fading_at(now));
        let due = opened || {
            let mut asked = RESET_ASKED.lock().unwrap_or_else(PoisonError::into_inner);
            let lately = asked
                .get(&plan.agent)
                .is_some_and(|at| started.saturating_duration_since(*at) < RESET_REREAD_EVERY);
            let due = reset && !lately;
            if due {
                asked.insert(plan.agent.clone(), started);
            }
            due
        };
        let worker = crew.iter().find(|worker| worker.vendor().0 == plan.agent);
        if let Some(worker) = worker.filter(|_| due) {
            read_live_for(ctx, worker);
        }
        plan.reading = reading_now(&plan.agent);
    }
    plans
}

/// `r` on the Usage tab: every CLI of the crew read live again now.
pub(crate) fn read_plan_usage(ctx: &Ctx) {
    let settings = ctx.settings();
    let mut crew = vec![settings.main_agent.clone()];
    crew.extend(settings.crew.members());
    for worker in &crew {
        let vendor = worker.vendor();
        if !settings.endpoints.contains_key(&vendor.0) && !bills_api_key(settings, &vendor) {
            read_live_for(ctx, worker);
        }
    }
}

/// The crew's workers whose CLI is read live for its account, by the CLI's id: one each, for
/// CLIs a plan read can name the account of.
pub(crate) fn live_readers(ctx: &Ctx) -> Vec<WorkerRef> {
    let settings = ctx.settings();
    let mut crew = vec![settings.main_agent.clone()];
    crew.extend(settings.crew.members());
    let mut readers: Vec<WorkerRef> = Vec::new();
    for worker in crew {
        let vendor = worker.vendor();
        let paid = settings.endpoints.contains_key(&vendor.0) || bills_api_key(settings, &vendor);
        if !paid && !readers.iter().any(|known| known.vendor() == vendor) {
            readers.push(worker);
        }
    }
    readers
}

/// Reads `worker`'s CLI live, as the Usage tab does, then calls `then`: for a CLI whose
/// login check doesn't name the account, which its plan read does, as Codex's.
pub(crate) fn read_account_live(
    workers: &dyn WorkerDirectory,
    quota: &Arc<dyn QuotaBoard>,
    internal: tokio::sync::mpsc::UnboundedSender<super::ctx::Internal>,
    worker: &WorkerRef,
    then: impl FnOnce() + Send + 'static,
) {
    read_live(workers, quota, worker, move || {
        let _ = internal.send(super::ctx::Internal::PlanUsageRead);
        then();
    });
}

/// `/login` and `/logout` commands handed to the terminal, with the CLI each signs in or out.
static SIGNING: Mutex<Vec<(Vec<String>, WorkerRef)>> = Mutex::new(Vec::new());

/// The screen is back from a command it ran in the terminal. After a CLI's `/login` or
/// `/logout`, the account may have changed, so the reading kept from before stops standing as
/// the CLI's own and the CLI is read again at once.
pub(crate) fn terminal_returned(ctx: &Ctx, argv: &[String]) {
    let worker = {
        let mut signing = SIGNING.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(at) = signing.iter().position(|(sent, _)| sent == argv) else {
            return;
        };
        signing.remove(at).1
    };
    ctx.services.quota.doubt(&worker.vendor());
    let _ = ctx.internal.send(super::ctx::Internal::PlanUsageRead);
    read_live_for(ctx, &worker);
}

/// A live read whose answer brings a new status line, so the bottom line shows it too.
fn read_live_for(ctx: &Ctx, worker: &WorkerRef) {
    let internal = ctx.internal.clone();
    let read = move || {
        let _ = internal.send(super::ctx::Internal::PlanUsageRead);
    };
    read_live(
        ctx.services.workers.as_ref(),
        &ctx.services.quota,
        worker,
        read,
    );
}

/// Starts a live read of `worker`'s CLI unless one is on its way. A CLI that can't be read for
/// free answers nothing, and its card says it has no live reading. Its probe is registered
/// when its worker is built, which no session may have done yet, so the worker is built first.
fn read_live(
    workers: &dyn WorkerDirectory,
    quota: &Arc<dyn QuotaBoard>,
    worker: &WorkerRef,
    read: impl FnOnce() + Send + 'static,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if workers.get(worker).is_err() {
        return;
    }
    let vendor = worker.vendor();
    let now = std::time::Instant::now();
    {
        let mut reading = READING.lock().unwrap_or_else(PoisonError::into_inner);
        let busy = reading
            .get(&vendor.0)
            .is_some_and(|at| now.saturating_duration_since(*at) < READ_GIVES_UP);
        if busy {
            return;
        }
        reading.insert(vendor.0.clone(), now);
    }
    let quota = Arc::clone(quota);
    runtime.spawn(async move {
        quota.probe(&vendor).await;
        READING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&vendor.0);
        read();
    });
}

fn reading_now(agent: &str) -> bool {
    let reading = READING.lock().unwrap_or_else(PoisonError::into_inner);
    reading
        .get(agent)
        .is_some_and(|at| at.elapsed() < READ_GIVES_UP)
}

/// `Claude · 42% of the five_hour window · resets in 2h 10m`.
fn quota_line(chip: &QuotaChip, now: OffsetDateTime) -> String {
    let mut parts = vec![chip.label.clone()];
    if chip.health == HealthState::Down {
        parts.push("down".to_owned());
    }
    let Some(reading) = &chip.reading else {
        parts.push("no plan usage reading yet".to_owned());
        return parts.join(" · ");
    };
    let window = reading
        .window
        .as_deref()
        .map(|window| window.replace('_', "-"))
        .unwrap_or_else(|| "current".to_owned());
    match (reading.status, reading.used) {
        (QuotaStatus::Rejected, _) => parts.push(format!("out of the {window} window")),
        (_, Some(used)) => parts.push(format!(
            "{:.0}% of the {window} window used",
            (used * 100.0).clamp(0.0, 100.0)
        )),
        (QuotaStatus::Warning, None) => parts.push(format!("near the {window} limit")),
        (QuotaStatus::Allowed | QuotaStatus::Unknown, None) => {
            parts.push(format!("{window} window open"));
        }
    }
    if let Some(resets) = reading.resets_at.filter(|resets| *resets > now) {
        parts.push(format!("resets in {}", span(resets - now)));
    }
    parts.join(" · ")
}

fn span(left: Duration) -> String {
    let minutes = left.whole_minutes().max(1);
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) if h % 24 == 0 => format!("{}d", h / 24),
        (h, 0) => format!("{h}h"),
        (h, m) if h < 24 => format!("{h}h {m}m"),
        (h, _) => format!("{}d {}h", h / 24, h % 24),
    }
}

fn usd(spend: &Spend) -> String {
    argonauts_tui::format::spend(spend)
}

/// `/login` and `/logout`: the screen steps aside while the agent CLI's own command runs in
/// the terminal, as when you take over a card. No agent means the main agent's CLI.
pub(crate) fn login(
    ctx: &mut Ctx,
    agent: Option<String>,
    log_in: bool,
) -> anyhow::Result<Vec<Effect>> {
    let command = if log_in { "/login" } else { "/logout" };
    let agent = agent.unwrap_or_else(|| ctx.settings().main_agent.worker.clone());
    let adapters = crate::commands::adapters_turning_on(
        Some(&ctx.env),
        ctx.settings.resolved(),
        std::slice::from_ref(&agent),
    )?;
    let Some(adapter) = adapters.iter().find(|adapter| adapter.id() == agent) else {
        bail!("{agent} isn't an agent CLI Argonauts knows. /agents lists them.");
    };
    let Some(login) = adapter.setup_hints().login else {
        bail!(
            "{agent} has no login of its own; its keys live in your settings. /agents shows them."
        );
    };
    let mut argv: Vec<String> = login.split_whitespace().map(str::to_owned).collect();
    if !log_in {
        argv = match logout_of(&argv) {
            Some(argv) => argv,
            None => bail!(
                "{agent} logs out from its own screen: run `{}` and log out there.",
                argv.join(" ")
            ),
        };
    }
    if let Some(binary) = ctx
        .settings()
        .agents
        .get(&agent)
        .and_then(|settings| settings.binary.as_ref())
        && let Some(program) = argv.first_mut()
    {
        *program = binary.display().to_string();
    }
    let shown = argv.join(" ");
    if !ctx.env.interactive {
        bail!(
            "{command} hands the terminal to `{shown}`, and there's no terminal here. Run it in one."
        );
    }
    ctx.notice(
        NoteLevel::Info,
        format!("Running `{shown}`. The screen comes back when it ends."),
    );
    let worker = match ctx.settings().main_agent.clone() {
        main if main.worker == agent => main,
        _ => WorkerRef::new(agent, None),
    };
    SIGNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((argv.clone(), worker));
    ctx.send(AppEvent::RunInTerminal {
        argv,
        cwd: ctx.env.repo_root.clone(),
    });
    Ok(Vec::new())
}

/// The CLIs pair them by name: `codex login` and `codex logout`, `claude auth login` and
/// `claude auth logout`. A CLI that logs in with no such word has no logout command.
fn logout_of(login: &[String]) -> Option<Vec<String>> {
    let (last, rest) = login.split_last()?;
    (last == "login").then(|| {
        let mut argv = rest.to_vec();
        argv.push("logout".to_owned());
        argv
    })
}

/// `/release-notes`.
pub(crate) fn release_notes(ctx: &mut Ctx) -> anyhow::Result<Vec<Effect>> {
    let mut lines = vec![format!(
        "This is Argonauts Code {}.",
        env!("CARGO_PKG_VERSION")
    )];
    for release in RELEASE_NOTES {
        let Some((heading, notes)) = release.split_first() else {
            continue;
        };
        lines.push(String::new());
        lines.push((*heading).to_owned());
        lines.extend(notes.iter().map(|note| format!("- {note}")));
    }
    ctx.send(AppEvent::Report {
        title: "Release notes".to_owned(),
        lines,
    });
    Ok(Vec::new())
}

/// `/feedback <text>`: kept in `~/.argonauts/feedback.jsonl`. Nothing leaves this machine, so
/// you share that file when you report something.
pub(crate) fn feedback(ctx: &mut Ctx, text: Option<String>) -> anyhow::Result<Vec<Effect>> {
    let text = text.unwrap_or_default().trim().to_owned();
    if text.is_empty() {
        ctx.notice(
            NoteLevel::Info,
            "Put your feedback after /feedback, such as /feedback the menu is slow.",
        );
        return Ok(Vec::new());
    }
    let path = ctx.env.paths.home().join(FEEDBACK_FILE);
    let at = OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let entry = serde_json::json!({
        "at": at,
        "version": env!("CARGO_PKG_VERSION"),
        "text": text,
    });
    let mut line = serde_json::to_vec(&entry).context("encoding the feedback")?;
    line.push(b'\n');
    let shown = ctx.env.display(&path);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(&line))
        .with_context(|| format!("saving your feedback to {shown}"))?;
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Thanks. Saved to {shown}. Nothing leaves this machine, so share that file when you \
             report something."
        ),
    );
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::limits::{QuotaReading, QuotaSource};
    use argonauts_core::names::VendorId;

    fn chip(status: QuotaStatus, used: Option<f64>, resets_in: Option<Duration>) -> QuotaChip {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        QuotaChip {
            label: "Codex".to_owned(),
            reading: Some(QuotaReading {
                vendor: VendorId("codex".to_owned()),
                status,
                used,
                resets_at: resets_in.map(|left| now + left),
                window: Some("five_hour".to_owned()),
                source: QuotaSource::AppServer,
                observed_at: now,
                detail: None,
                wrap_up: false,
            }),
            health: HealthState::Healthy,
            windows: Vec::new(),
        }
    }

    fn window(agent: &str, name: &str, used: f64, resets_in: Duration) -> QuotaReading {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        QuotaReading {
            vendor: VendorId(agent.to_owned()),
            status: QuotaStatus::Allowed,
            used: Some(used),
            resets_at: Some(now + resets_in),
            window: Some(name.to_owned()),
            source: QuotaSource::AppServer,
            observed_at: now - Duration::minutes(2),
            detail: None,
            wrap_up: false,
        }
    }

    /// A live reading of `agent` for `account`, read two minutes before the tests' now, with
    /// windows of `(name, used, resets in)`.
    fn live(
        quota: &dyn QuotaBoard,
        agent: &str,
        account: Option<&str>,
        windows: &[(&str, f64, Duration)],
    ) {
        use argonauts_core::plan_usage::LiveWindow;
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let mut reading = LiveUsage::new(
            VendorId(agent.to_owned()),
            QuotaSource::AppServer,
            now - Duration::minutes(2),
        );
        reading.account = account.map(str::to_owned);
        reading.windows = windows
            .iter()
            .map(|(name, used, resets_in)| {
                LiveWindow::new(*name, *name, *name)
                    .used(Some(*used))
                    .resets_at(Some(now + *resets_in))
            })
            .collect();
        quota.observe_live(reading);
    }

    fn plans_of(
        settings: &Settings,
        quota: &dyn QuotaBoard,
        health: &dyn HealthBoard,
    ) -> Vec<PlanUsage> {
        plans_under(settings, quota, health, None)
    }

    /// The crew's plan usage with a plan usage limit set, or none.
    fn plans_under(
        settings: &Settings,
        quota: &dyn QuotaBoard,
        health: &dyn HealthBoard,
        limit: Option<f64>,
    ) -> Vec<PlanUsage> {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let mut crew = vec![settings.main_agent.clone()];
        crew.extend(settings.crew.members());
        let named = Named {
            cli: &|id| id.to_uppercase(),
            worker: &|worker| format!("<{worker}>"),
        };
        plan_usage(
            &crew,
            settings,
            &Boards { quota, health },
            &named,
            limit,
            now,
        )
    }

    #[test]
    fn each_cli_shows_its_windows_shortest_first_and_none_that_has_reset() {
        use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};

        let settings = Settings {
            main_agent: WorkerRef::new("claude-code", Some("opus".into())),
            crew: argonauts_core::settings::crew::CrewSettings::of(vec![
                WorkerRef::new("codex", None),
                WorkerRef::new("claude-code", Some("haiku".into())),
            ]),
            ..Settings::default()
        };
        let (quota, health) = (MemoryQuotaBoard::default(), MemoryHealthBoard::default());
        live(
            &quota,
            "claude-code",
            Some("me@example.com"),
            &[
                ("seven_day", 0.38, Duration::days(2)),
                ("five_hour", 0.71, Duration::hours(3)),
            ],
        );
        let mut max = quota.live(&VendorId("claude-code".into())).unwrap();
        max.plan = Some("Max".into());
        quota.observe_live(max);
        // Read before its window ended: it says nothing about the window open now.
        live(
            &quota,
            "codex",
            Some("me@example.com"),
            &[
                ("five_hour", 0.93, -Duration::minutes(5)),
                ("weekly", 0.41, Duration::days(4)),
            ],
        );

        let plans = plans_of(&settings, &quota, &health);
        let [claude, codex] = plans.as_slice() else {
            panic!("one entry per CLI, in crew order: {plans:?}");
        };
        assert_eq!((claude.name.as_str(), claude.in_use), ("CLAUDE-CODE", true));
        assert_eq!(claude.plan.as_deref(), Some("Max"));
        let names = |plan: &PlanUsage| -> Vec<String> {
            plan.windows
                .iter()
                .filter_map(|reading| reading.window.clone())
                .collect()
        };
        assert_eq!(names(claude), ["five_hour", "seven_day"]);
        assert!(!codex.in_use);
        assert_eq!(names(codex), ["weekly"], "the reset window is dropped");
        assert_eq!(codex.moved_to, None);
    }

    #[test]
    fn only_a_live_reading_of_the_account_signed_in_shows() {
        use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};

        let settings = Settings {
            main_agent: WorkerRef::new("codex", None),
            crew: argonauts_core::settings::crew::CrewSettings::of(vec![WorkerRef::new(
                "antigravity-cli",
                None,
            )]),
            ..Settings::default()
        };
        let (quota, health) = (MemoryQuotaBoard::default(), MemoryHealthBoard::default());
        // A session's stream reading names no account, so it never stands for one.
        quota.observe(window("codex", "five_hour", 0.5, Duration::hours(2)));
        let plans = plans_of(&settings, &quota, &health);
        assert!(plans[0].windows.is_empty() && plans[0].live.is_none());

        live(
            &quota,
            "codex",
            Some("one@example.com"),
            &[("five_hour", 0.2, Duration::hours(2))],
        );
        live(
            &quota,
            "codex",
            Some("two@example.com"),
            &[("five_hour", 0.9, Duration::hours(1))],
        );
        let plans = plans_of(&settings, &quota, &health);
        let shown = plans[0].live.as_ref().unwrap();
        assert_eq!(shown.account.as_deref(), Some("two@example.com"));
        assert_eq!(plans[0].windows[0].used, Some(0.9));

        // A reading that names no account shows only while it's fresh.
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let mut stale = LiveUsage::new(
            VendorId("antigravity-cli".into()),
            QuotaSource::UsageProbe,
            now - Duration::minutes(30),
        );
        stale.windows =
            vec![argonauts_core::plan_usage::LiveWindow::new("w", "w", "w").used(Some(0.1))];
        quota.observe_live(stale);
        let plans = plans_of(&settings, &quota, &health);
        assert!(plans[1].live.is_none() && plans[1].windows.is_empty());
    }

    #[test]
    fn a_cli_that_is_out_names_who_takes_its_work_and_an_endpoint_has_no_plan() {
        use argonauts_core::settings::agents::EndpointSettings;
        use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};

        let mut settings = Settings {
            main_agent: WorkerRef::new("antigravity-cli", None),
            crew: argonauts_core::settings::crew::CrewSettings::of(vec![WorkerRef::new(
                "openrouter",
                Some("qwen".into()),
            )]),
            ..Settings::default()
        };
        settings.fallback.order = vec![
            FallbackEntry::Worker(WorkerRef::new("antigravity-cli", Some("flash".into()))),
            FallbackEntry::Worker(WorkerRef::new("codex", Some("gpt-5.6".into()))),
        ];
        settings
            .endpoints
            .insert("openrouter".into(), EndpointSettings::default());
        let (quota, health) = (MemoryQuotaBoard::default(), MemoryHealthBoard::default());
        live(
            &quota,
            "antigravity-cli",
            None,
            &[("day", 1.0, Duration::hours(20))],
        );

        let plans = plans_of(&settings, &quota, &health);
        assert_eq!(plans[0].moved_to.as_deref(), Some("<codex/gpt-5.6>"));
        assert!(plans[1].pays_per_token && plans[1].windows.is_empty());
        assert_eq!(plans[1].moved_to, None);
    }

    #[test]
    fn a_cli_at_your_plan_usage_limit_names_who_takes_its_tasks() {
        use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};

        let mut settings = Settings {
            main_agent: WorkerRef::new("codex", Some("gpt-5.6".into())),
            crew: argonauts_core::settings::crew::CrewSettings::of(vec![WorkerRef::new(
                "claude-code",
                Some("opus".into()),
            )]),
            ..Settings::default()
        };
        settings.fallback.order = vec![FallbackEntry::Worker(WorkerRef::new(
            "claude-code",
            Some("sonnet".into()),
        ))];
        let (quota, health) = (MemoryQuotaBoard::default(), MemoryHealthBoard::default());
        live(
            &quota,
            "codex",
            Some("me@example.com"),
            &[
                ("five_hour", 0.8, Duration::hours(2)),
                ("weekly", 0.41, Duration::days(4)),
            ],
        );
        live(
            &quota,
            "claude-code",
            Some("me@example.com"),
            &[("five_hour", 0.71, Duration::hours(3))],
        );

        let plans = plans_under(&settings, &quota, &health, Some(0.8));
        assert_eq!(plans[0].moved_to.as_deref(), Some("<claude-code/sonnet>"));
        assert_eq!(
            plans[1].moved_to, None,
            "under the limit, it keeps its tasks"
        );
        // With no limit a plan is used until its provider says it's out.
        let plans = plans_of(&settings, &quota, &health);
        assert_eq!(plans[0].moved_to, None);
    }

    #[test]
    fn a_limit_is_read_from_the_rows_limits_lists() {
        let row = |key: &str, value: &str| SettingRow {
            key: key.to_owned(),
            value: value.to_owned(),
            layer: "project".to_owned(),
            preset_from: None,
            default_scope: argonauts_core::settings::SettingScope::Project,
            safety: false,
            allowed: Vec::new(),
            description: String::new(),
        };
        let rows = [
            row(SPEND_LIMIT, "5.0"),
            row(PLAN_USAGE_LIMIT, "0.8"),
            row("limits.when_reached", "\"fallback\""),
        ];
        assert_eq!(limit_of(&rows, SPEND_LIMIT), Some(5.0));
        assert_eq!(limit_of(&rows, PLAN_USAGE_LIMIT), Some(0.8));
        assert_eq!(limit_of(&rows, "limits.when_reached"), None, "not a number");
        // Unset reads as empty text, and zero is no limit to count toward.
        let unset = [row(SPEND_LIMIT, ""), row(PLAN_USAGE_LIMIT, "0")];
        assert_eq!(limit_of(&unset, SPEND_LIMIT), None);
        assert_eq!(limit_of(&unset, PLAN_USAGE_LIMIT), None);
        assert_eq!(limit_of(&[], SPEND_LIMIT), None);
    }

    #[test]
    fn a_quota_line_says_how_much_is_used_and_when_it_resets() {
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let line = quota_line(
            &chip(
                QuotaStatus::Allowed,
                Some(0.42),
                Some(Duration::minutes(130)),
            ),
            now,
        );
        assert_eq!(
            line,
            "Codex · 42% of the five-hour window used · resets in 2h 10m"
        );
        let out = quota_line(&chip(QuotaStatus::Rejected, Some(1.0), None), now);
        assert_eq!(out, "Codex · out of the five-hour window");
        let none = QuotaChip {
            reading: None,
            health: HealthState::Down,
            windows: Vec::new(),
            ..chip(QuotaStatus::Unknown, None, None)
        };
        assert_eq!(
            quota_line(&none, now),
            "Codex · down · no plan usage reading yet"
        );
    }

    #[test]
    fn periods_take_the_vendors_words_too() {
        assert_eq!(parse_period(None).unwrap(), UsagePeriod::Today);
        assert_eq!(parse_period(Some("weekly")).unwrap(), UsagePeriod::Week);
        assert_eq!(parse_period(Some("monthly")).unwrap(), UsagePeriod::Month);
        assert_eq!(parse_period(Some("cumulative")).unwrap(), UsagePeriod::All);
        assert!(parse_period(Some("yearly")).is_err());
    }

    #[test]
    fn logout_pairs_with_login_by_name() {
        let argv =
            |text: &str| -> Vec<String> { text.split_whitespace().map(str::to_owned).collect() };
        assert_eq!(logout_of(&argv("codex login")), Some(argv("codex logout")));
        assert_eq!(
            logout_of(&argv("claude auth login")),
            Some(argv("claude auth logout"))
        );
        assert_eq!(logout_of(&argv("agy")), None);
    }

    #[test]
    fn spend_says_when_it_is_a_floor_or_a_guess() {
        let spend = Spend {
            cost_usd: 1.5,
            cost_partial: true,
            ..Spend::default()
        };
        assert_eq!(usd(&spend), "$1.50+");
        let estimated = Spend {
            cost_usd: 0.24,
            cost_estimated: true,
            ..Spend::default()
        };
        assert_eq!(usd(&estimated), "~$0.24");
        assert_eq!(span(Duration::hours(48)), "2d");
        assert_eq!(span(Duration::seconds(10)), "1m");
    }

    #[test]
    fn a_tasks_limit_is_the_one_you_set_in_its_own_unit() {
        let unset = BudgetSettings {
            per_task_usd: None,
            ..BudgetSettings::default()
        };
        assert_eq!(task_limit(&unset), None);
        let dollars = BudgetSettings {
            per_task_usd: Some(1.5),
            ..unset.clone()
        };
        let limit = task_limit(&dollars).expect("a limit");
        assert_eq!((limit.unit, limit.amount), (BudgetUnit::Usd, 1.5));
        let tokens = BudgetSettings {
            per_task_tokens: Some(200_000),
            ..unset.clone()
        };
        let limit = task_limit(&tokens).expect("a limit");
        assert_eq!((limit.unit, limit.amount), (BudgetUnit::Tokens, 200_000.0));
        // Zero dollars is no limit to fill toward.
        let zero = BudgetSettings {
            per_task_usd: Some(0.0),
            ..unset
        };
        assert_eq!(task_limit(&zero), None);
    }

    #[test]
    fn a_run_counts_the_tasks_it_merged_of_those_it_still_means_to_do() {
        let task = |n: u32, state: CardState| CardBrief {
            card: argonauts_core::CardId::numbered(n),
            title: format!("task {n}"),
            state,
            owner: None,
            note: String::new(),
        };
        let tasks = [
            task(1, CardState::Integrated),
            task(2, CardState::Running),
            task(3, CardState::Waiting),
            task(4, CardState::Cancelled),
            task(5, CardState::TakenOver),
        ];
        let run = RunId::new("run-03").unwrap();
        let progress = tasks_of(&run, &tasks, None);
        assert_eq!((progress.tasks_done, progress.tasks), (1, 3));
        assert_eq!(tasks_of(&run, &[], None).tasks, 0, "a solo run has none");
    }

    /// Builds only the workers it was given, and says which it was asked for.
    #[derive(Default)]
    struct Building {
        built: argonauts_testkit::worker::FakeDirectory,
        asked: Mutex<Vec<String>>,
    }

    impl WorkerDirectory for Building {
        fn get(
            &self,
            worker: &WorkerRef,
        ) -> Result<Arc<dyn argonauts_core::worker::Worker>, argonauts_core::worker::WorkerError>
        {
            self.asked.lock().unwrap().push(worker.to_string());
            self.built.get(worker)
        }

        fn available(&self) -> Vec<WorkerRef> {
            self.built.available()
        }
    }

    /// Says which CLIs it was asked to read live, and that each read failed when `failed` is
    /// set.
    #[derive(Default)]
    struct Probed {
        board: argonauts_testkit::services::MemoryQuotaBoard,
        probed: Mutex<Vec<String>>,
        failed: Option<String>,
        /// When the failed read happened.
        failed_at: Option<OffsetDateTime>,
    }

    impl QuotaBoard for Probed {
        fn observe(&self, reading: QuotaReading) {
            self.board.observe(reading);
        }
        fn get(&self, vendor: &VendorId) -> Option<QuotaReading> {
            self.board.get(vendor)
        }
        fn all(&self) -> Vec<QuotaReading> {
            self.board.all()
        }
        fn windows(&self, vendor: &VendorId) -> Vec<QuotaReading> {
            self.board.windows(vendor)
        }
        fn plan(&self, vendor: &VendorId) -> Option<String> {
            self.board.plan(vendor)
        }
        fn observe_plan(&self, vendor: &VendorId, plan: String) {
            self.board.observe_plan(vendor, plan);
        }
        fn observe_live(&self, reading: LiveUsage) {
            self.board.observe_live(reading);
        }
        fn live(&self, vendor: &VendorId) -> Option<LiveUsage> {
            self.board.live(vendor)
        }
        fn failed(&self, _: &VendorId) -> Option<String> {
            self.failed.clone()
        }
        fn doubted_at(&self, _: &VendorId) -> Option<OffsetDateTime> {
            self.failed_at
        }
        fn probe(
            &self,
            vendor: &VendorId,
        ) -> argonauts_core::future::BoxFuture<'_, Option<QuotaReading>> {
            self.probed.lock().unwrap().push(vendor.0.clone());
            Box::pin(async { None })
        }
    }

    #[test]
    fn a_cli_whose_read_failed_says_why() {
        use argonauts_testkit::services::MemoryHealthBoard;

        let settings = Settings {
            main_agent: WorkerRef::new("codex", None),
            ..Settings::default()
        };
        let quota = Probed {
            failed: Some("failed to fetch codex rate limits".into()),
            ..Probed::default()
        };
        let plans = plans_of(&settings, &quota, &MemoryHealthBoard::default());
        assert_eq!(
            plans[0].failed.as_deref(),
            Some("failed to fetch codex rate limits")
        );
        assert!(plans[0].live.is_none());
    }

    /// A read that fails after a reading means the account may have changed since, so the
    /// reading no longer stands as the CLI's own: the card draws it as kept from before.
    #[test]
    fn a_reading_followed_by_a_failed_read_is_no_longer_current() {
        use argonauts_testkit::services::MemoryHealthBoard;

        let settings = Settings {
            main_agent: WorkerRef::new("codex", None),
            ..Settings::default()
        };
        let now = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
        let quota = Probed {
            failed: Some("failed to fetch codex rate limits".into()),
            failed_at: Some(now - Duration::minutes(1)),
            ..Probed::default()
        };
        live(
            &quota,
            "codex",
            Some("one@example.com"),
            &[("five_hour", 0.95, Duration::hours(1))],
        );
        let plans = plans_of(&settings, &quota, &MemoryHealthBoard::default());
        assert!(plans[0].live.is_none());
        assert!(plans[0].windows.is_empty());
        let last = plans[0].last.as_ref().unwrap();
        assert_eq!(last.account.as_deref(), Some("one@example.com"));

        // A reading after the failure stands again.
        let quota = Probed {
            failed_at: Some(now - Duration::minutes(5)),
            ..Probed::default()
        };
        live(
            &quota,
            "codex",
            Some("one@example.com"),
            &[("five_hour", 0.95, Duration::hours(1))],
        );
        let plans = plans_of(&settings, &quota, &MemoryHealthBoard::default());
        assert!(plans[0].live.is_some() && plans[0].last.is_none());
    }

    /// A CLI that bills an API key has no plan: its card says so and shows no windows, even
    /// one a subscription signed in on the same machine left on the board.
    #[test]
    fn a_cli_that_bills_an_api_key_has_no_plan_windows() {
        use argonauts_core::settings::agents::AgentSettings;
        use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};

        let mut settings = Settings::default();
        settings.agents.insert(
            "claude-code".to_owned(),
            AgentSettings {
                auth: AuthMode::ApiKey,
                ..AgentSettings::default()
            },
        );
        let quota = MemoryQuotaBoard::default();
        live(
            &quota,
            "claude-code",
            Some("someone@example.com"),
            &[("five_hour", 0.4, Duration::hours(1))],
        );
        let plans = plans_of(&settings, &quota, &MemoryHealthBoard::default());
        let claude = plans
            .iter()
            .find(|plan| plan.agent == "claude-code")
            .unwrap();
        assert!(claude.pays_per_token);
        assert!(claude.windows.is_empty() && claude.live.is_none() && claude.last.is_none());
    }

    /// A CLI's probe is registered when its worker is built, so a live read on a fresh start,
    /// before any session, builds the worker first. One that can't be built isn't read, and
    /// one that was says so once its answer is in.
    #[tokio::test]
    async fn a_live_read_builds_the_cli_worker_first() {
        let directory = Building::default();
        directory
            .built
            .add_worker(Arc::new(argonauts_testkit::FakeWorker::harness(
                "live-built/m",
                "m",
            )));
        let probed = Arc::new(Probed::default());
        let quota: Arc<dyn QuotaBoard> = probed.clone();
        let built: WorkerRef = "live-built/m".parse().unwrap();
        let missing: WorkerRef = "live-missing/m".parse().unwrap();
        let answered = Arc::new(Mutex::new(Vec::new()));
        let said = |name: &'static str| {
            let answered = Arc::clone(&answered);
            move || answered.lock().unwrap().push(name)
        };
        read_live(&directory, &quota, &built, said("built"));
        read_live(&directory, &quota, &missing, said("missing"));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            *directory.asked.lock().unwrap(),
            ["live-built/m", "live-missing/m"]
        );
        assert_eq!(*probed.probed.lock().unwrap(), ["live-built"]);
        // The answer that landed is said, so the bottom line is drawn again.
        assert_eq!(*answered.lock().unwrap(), ["built"]);
    }

    /// The Usage tab's card and the bottom line read the same live reading: the account signed
    /// in last, each window with the same share and reset, and nothing of the account before.
    #[test]
    fn the_usage_card_and_the_bottom_line_read_the_same_live_reading() {
        use argonauts_core::plan_usage::LiveWindow;
        use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};

        let settings = Settings {
            main_agent: WorkerRef::new("codex", None),
            ..Settings::default()
        };
        let (quota, health) = (MemoryQuotaBoard::default(), MemoryHealthBoard::default());
        let now = OffsetDateTime::now_utc();
        let resets = now + Duration::hours(2);
        let read = |account: &str, used: f64| {
            let mut reading =
                LiveUsage::new(VendorId("codex".to_owned()), QuotaSource::AppServer, now);
            reading.account = Some(account.to_owned());
            reading.windows = vec![
                LiveWindow::new("five_hour", "five_hour", "five_hour")
                    .used(Some(used))
                    .resets_at(Some(resets)),
            ];
            quota.observe_live(reading);
        };
        read("one@example.com", 0.2);
        read("two@example.com", 0.9);

        let crew = vec![settings.main_agent.clone()];
        let named = Named {
            cli: &|id| id.to_owned(),
            worker: &|worker| worker.to_string(),
        };
        let boards = Boards {
            quota: &quota,
            health: &health,
        };
        let cards = plan_usage(&crew, &settings, &boards, &named, None, now);
        let chips = quota_chips(&crew, &quota, &health);
        let shares = |windows: &[QuotaReading]| -> Vec<(Option<String>, Option<f64>, Option<OffsetDateTime>)> {
            windows
                .iter()
                .map(|reading| (reading.window.clone(), reading.used, reading.resets_at))
                .collect()
        };
        assert_eq!(
            shares(&cards[0].windows),
            [(Some("five_hour".to_owned()), Some(0.9), Some(resets))],
            "the card reads the account signed in last"
        );
        assert_eq!(shares(&chips[0].windows), shares(&cards[0].windows));
        assert_eq!(
            chips[0].reading.as_ref().and_then(|reading| reading.used),
            Some(0.9)
        );
    }
}
