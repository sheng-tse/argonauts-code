//! The usage ledger, `~/.argonauts/usage/<yyyy-mm>.jsonl`: one line when a conversation turn ends
//! and one for each agent and model when a run ends, with what it spent, so /usage shows spend by
//! day and period after the runs are gone. Reading it costs nothing.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use argonauts_core::api::{
    DaySpend, RunSummary, UsagePeriod, UsageShare, UsageSummary, day_format,
};
use argonauts_core::events::RunEvent;
use argonauts_core::limits::PlanUsageReading;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::periods::WeekStart;
use argonauts_core::{Paths, RunId, Spend, create_private_dir_all};
use argonauts_tui::format::WallClock;
use serde::{Deserialize, Serialize};
use time::{Date, Duration, OffsetDateTime};

use crate::views::sessions::{RunFacts, add_spend};
use crate::work_ledger::{self, WorkLedger};

/// The history card's days.
const HISTORY_DAYS: i64 = 14;

/// What a share of the spend is summed by: the agent, its model and the agent's id, each as
/// the ledger's line has it.
type ShareKey = (String, Option<String>, String);

/// One turn's or one run agent's spend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct LedgerLine {
    /// The local day it ended.
    #[serde(with = "day_format")]
    pub day: Date,
    /// The repository's root.
    pub repo: String,
    /// `None` for a conversation turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub agent_id: String,
    /// All input tokens, cached ones included, as `Spend` counts them.
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub cost: f64,
    #[serde(default)]
    pub estimated: bool,
    /// Some usage came without a price, so `cost` is a lower bound.
    #[serde(default)]
    pub partial: bool,
    /// The local hour it ended, 0 to 23, for the Overview's hours of the day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hour: Option<u8>,
    /// For a conversation turn: the conversation it was a turn of, when it was one, and the
    /// seconds it took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub took: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// What the ledger keeps of a conversation turn beside what it spent.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TurnNote<'a> {
    /// The conversation it was a turn of. A question put to another model beside the
    /// conversation has none.
    pub conversation: Option<&'a str>,
    /// Seconds from the message to the end of its answer.
    pub took: u64,
}

impl LedgerLine {
    fn new(day: Date, repo: &Path, run: Option<&RunId>, worker: &ModelRef, spent: &Spend) -> Self {
        Self {
            day,
            repo: repo.display().to_string(),
            run: run.cloned(),
            agent: worker.agent.clone(),
            model: worker.model.clone(),
            agent_id: worker.agent_id().0,
            input: spent.input_tokens,
            cached: spent.cached_tokens,
            output: spent.output_tokens,
            cost: spent.cost_usd,
            estimated: spent.cost_estimated,
            partial: spent.cost_partial,
            hour: None,
            conversation: None,
            took: 0,
        }
    }

    /// Notes the local hour of `at`.
    fn at_hour(mut self, at: OffsetDateTime) -> Self {
        self.hour = Some(WallClock::default().to_local(at).hour());
        self
    }

    pub fn spent(&self) -> Spend {
        Spend {
            input_tokens: self.input,
            output_tokens: self.output,
            cached_tokens: self.cached,
            cost_usd: self.cost,
            cost_partial: self.partial,
            cost_estimated: self.estimated,
        }
    }
}

pub(crate) struct UsageLedger {
    dir: PathBuf,
}

impl UsageLedger {
    pub fn new(paths: &Paths) -> Self {
        Self {
            dir: paths.usage_dir(),
        }
    }

    /// A conversation turn ended. Nothing spent, nothing written.
    pub fn turn_ended(
        &self,
        repo: &Path,
        worker: &ModelRef,
        spent: &Spend,
        turn: TurnNote<'_>,
        at: OffsetDateTime,
    ) {
        if spent.total_tokens() == 0 && spent.cost_usd == 0.0 {
            return;
        }
        let mut line = LedgerLine::new(local_day(at), repo, None, worker, spent).at_hour(at);
        line.conversation = turn.conversation.map(str::to_owned);
        line.took = turn.took;
        if let Err(err) = self.append(&[line]) {
            tracing::warn!("couldn't add the turn to the usage ledger: {err}");
        }
    }

    /// A run ended: one line for each agent and model it ran, from its log. A run already in
    /// this month's file isn't written again.
    pub fn run_ended(&self, repo: &Path, run: &RunId, events: &[RunEvent], at: OffsetDateTime) {
        let day = local_day(at);
        if self
            .read_month(day)
            .iter()
            .any(|line| line.run.as_ref() == Some(run))
        {
            return;
        }
        let mut by_worker: BTreeMap<(String, Option<String>), (ModelRef, Spend)> = BTreeMap::new();
        for session in RunFacts::of(events).sessions {
            let key = (session.worker.agent.clone(), session.worker.model.clone());
            let (_, spent) = by_worker
                .entry(key)
                .or_insert_with(|| (session.worker.clone(), Spend::default()));
            add_spend(spent, &session.spent());
        }
        let lines: Vec<LedgerLine> = by_worker
            .values()
            .filter(|(_, spent)| spent.total_tokens() > 0 || spent.cost_usd > 0.0)
            .map(|(worker, spent)| LedgerLine::new(day, repo, Some(run), worker, spent).at_hour(at))
            .collect();
        if let Err(err) = self.append(&lines) {
            tracing::warn!("couldn't add run {run} to the usage ledger: {err}");
        }
        WorkLedger::beside(&self.dir).run_ended(repo, run, events, at);
    }

    /// Lines go to the file of their day's month, each in one write, so two processes
    /// appending at once never interleave within a line.
    pub fn append(&self, lines: &[LedgerLine]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        create_private_dir_all(&self.dir)?;
        for line in lines {
            let mut text = serde_json::to_string(line).map_err(io::Error::other)?;
            text.push('\n');
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(self.month_file(line.day))?
                .write_all(text.as_bytes())?;
        }
        Ok(())
    }

    /// Every line from `first` on, or every line at all. Lines that don't read are skipped.
    pub fn read_since(&self, first: Option<Date>) -> Vec<LedgerLine> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let first_month = first.map(month_name);
        let mut months: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                let Some(month) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".jsonl"))
                else {
                    return false;
                };
                first_month.as_deref().is_none_or(|first| month >= first)
            })
            .collect();
        months.sort();
        months
            .iter()
            .flat_map(|path| read_lines(path))
            .filter(|line| first.is_none_or(|first| line.day >= first))
            .collect()
    }

    fn read_month(&self, day: Date) -> Vec<LedgerLine> {
        read_lines(&self.month_file(day))
    }

    fn month_file(&self, day: Date) -> PathBuf {
        self.dir.join(format!("{}.jsonl", month_name(day)))
    }
}

fn read_lines(path: &Path) -> Vec<LedgerLine> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// `2026-10`.
fn month_name(day: Date) -> String {
    format!("{:04}-{:02}", day.year(), u8::from(day.month()))
}

/// The day on your clock when `at` happened.
pub(crate) fn local_day(at: OffsetDateTime) -> Date {
    WallClock::default().to_local(at).date()
}

/// Spend for each day that has any.
pub(crate) fn by_day(lines: &[LedgerLine]) -> BTreeMap<Date, Spend> {
    let mut days: BTreeMap<Date, Spend> = BTreeMap::new();
    for line in lines {
        add_spend(days.entry(line.day).or_default(), &line.spent());
    }
    days
}

/// The Usage tab's numbers for `period`. The ledger's lines and the runs are filtered by it;
/// the plan usage windows, which are always now, and the 14 days of history are not.
pub(crate) fn summary(
    ledger: &UsageLedger,
    (period, week_starts): (UsagePeriod, WeekStart),
    today: Date,
    runs: &[RunSummary],
    plan_windows: Vec<PlanUsageReading>,
) -> UsageSummary {
    let first = period.first_day(today, week_starts.weekday());
    // Every line, whatever the period: a model's color goes by its tokens in all.
    let lines = ledger.read_since(None);
    let counted: Vec<&LedgerLine> = lines
        .iter()
        .filter(|line| first.is_none_or(|first| line.day >= first))
        .collect();

    let key = |line: &LedgerLine| {
        (
            line.agent.clone(),
            line.model.clone(),
            line.agent_id.clone(),
        )
    };
    let mut total = Spend::default();
    let mut shares: BTreeMap<ShareKey, Spend> = BTreeMap::new();
    for line in &counted {
        let spent = line.spent();
        add_spend(&mut total, &spent);
        add_spend(shares.entry(key(line)).or_default(), &spent);
    }
    // Each share's days of this week, whatever the period, for the spend card's small bars.
    let week_from = UsagePeriod::Week
        .first_day(today, week_starts.weekday())
        .unwrap_or(today);
    let mut week: BTreeMap<ShareKey, BTreeMap<Date, Spend>> = BTreeMap::new();
    for line in lines.iter().filter(|line| line.day >= week_from) {
        let days = week.entry(key(line)).or_default();
        add_spend(days.entry(line.day).or_default(), &line.spent());
    }
    let mut by_agent: Vec<UsageShare> = shares
        .into_iter()
        .map(|(key, spent)| {
            let days = week.remove(&key).unwrap_or_default().into_iter();
            let (agent, model, agent_id) = key;
            UsageShare {
                agent,
                model,
                agent_id,
                spent,
                pays_per_token: false,
                this_week: days.map(|(day, spent)| DaySpend { day, spent }).collect(),
            }
        })
        .collect();
    by_agent.sort_by(|a, b| b.spent.cost_usd.total_cmp(&a.spent.cost_usd));

    let days = by_day(&lines);
    let history = (0..HISTORY_DAYS)
        .rev()
        .map(|back| today.saturating_sub(Duration::days(back)))
        .map(|day| DaySpend {
            day,
            spent: days.get(&day).copied().unwrap_or_default(),
        })
        .collect();

    let mut runs: Vec<RunSummary> = runs
        .iter()
        .filter(|run| first.is_none_or(|first| local_day(run.updated) >= first))
        .cloned()
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.updated));

    let work = WorkLedger::beside(&ledger.dir);
    let stats = work_ledger::stats(&work, first, &lines);
    let argonauts = crate::overview::through_argonauts(&work.read_since(first));
    UsageSummary {
        period,
        week_starts,
        total,
        by_agent,
        history,
        runs,
        plan_windows,
        stats: Some(stats),
        argonauts,
        all_time_models: crate::overview::all_time_models(&ledger.dir, &lines),
        ..UsageSummary::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::test_support::{run_id, session_started, usage};
    use argonauts_core::api::RunState;
    use argonauts_core::events::run::RunMode;
    use argonauts_core::limits::{PlanUsageSource, PlanUsageStatus};
    use argonauts_core::names::{AgentId, RoleName, SessionKey};
    use time::macros::{date, datetime};

    fn ledger() -> (tempfile::TempDir, UsageLedger) {
        let home = tempfile::tempdir().unwrap();
        let ledger = UsageLedger::new(&Paths::new(home.path()));
        (home, ledger)
    }

    fn spend(cost: f64, input: u64, cached: u64, output: u64) -> Spend {
        Spend {
            input_tokens: input,
            output_tokens: output,
            cached_tokens: cached,
            cost_usd: cost,
            ..Spend::default()
        }
    }

    fn line(day: Date, agent: &str, run: Option<&str>, cost: f64) -> LedgerLine {
        let worker = ModelRef {
            agent: agent.into(),
            model: None,
        };
        let run = run.map(|run| RunId::new(run).unwrap());
        LedgerLine::new(
            day,
            Path::new("/repo"),
            run.as_ref(),
            &worker,
            &spend(cost, 1000, 600, 100),
        )
    }

    fn run_summary(run: &str, updated: OffsetDateTime) -> RunSummary {
        RunSummary {
            run: RunId::new(run).unwrap(),
            title: run.into(),
            name: None,
            mode: RunMode::Crew,
            workflow: "balanced".into(),
            state: RunState::Merged,
            stopped_at: String::new(),
            updated,
            spent: Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: false,
            remind: false,
        }
    }

    #[test]
    fn a_turn_end_appends_a_line_to_its_months_file() {
        let (home, ledger) = ledger();
        let worker = ModelRef {
            agent: "claude-code".into(),
            model: Some("claude-opus-5-5".into()),
        };
        let at = datetime!(2026-10-04 12:00 UTC);
        let turn = TurnNote {
            conversation: Some("conversation-007"),
            took: 42,
        };
        ledger.turn_ended(
            Path::new("/repo"),
            &worker,
            &spend(0.12, 5000, 4000, 300),
            turn,
            at,
        );
        // Nothing spent, nothing written.
        ledger.turn_ended(Path::new("/repo"), &worker, &Spend::default(), turn, at);
        let month = month_name(local_day(at));
        let text =
            fs::read_to_string(home.path().join("usage").join(format!("{month}.jsonl"))).unwrap();
        assert_eq!(text.lines().count(), 1);
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["agent"], "claude-code");
        assert_eq!(line["model"], "claude-opus-5-5");
        assert_eq!(line["agent_id"], "claude-code");
        assert_eq!(
            (line["input"].clone(), line["cached"].clone()),
            (5000.into(), 4000.into())
        );
        assert_eq!(line["cost"], 0.12);
        assert!(line.get("run").is_none());
        assert_eq!(line["day"].as_str().unwrap().len(), "2026-10-04".len());
        assert_eq!(line["conversation"], "conversation-007");
        assert_eq!(line["took"], 42);
    }

    #[test]
    fn a_run_end_appends_a_line_for_each_agent_and_model_once() {
        let (_home, ledger) = ledger();
        let run = run_id();
        let task = argonauts_core::TaskId::new("task-001").unwrap();
        let (first, second, third) = (
            SessionKey::numbered(1),
            SessionKey::numbered(2),
            SessionKey::numbered(3),
        );
        let events = vec![
            session_started(0, &first, Some(&task), RoleName::Worker, "claude-code"),
            usage(1, &first, 1000, 100, 0.10),
            session_started(2, &second, Some(&task), RoleName::Worker, "claude-code"),
            usage(3, &second, 2000, 200, 0.20),
            session_started(4, &third, Some(&task), RoleName::Reviewer, "codex"),
            usage(5, &third, 500, 50, 0.05),
        ];
        let at = datetime!(2026-10-04 12:00 UTC);
        ledger.run_ended(Path::new("/repo"), &run, &events, at);
        ledger.run_ended(Path::new("/repo"), &run, &events, at);
        let lines = ledger.read_since(None);
        let costs: Vec<(&str, f64)> = lines
            .iter()
            .map(|line| (line.agent.as_str(), (line.cost * 100.0).round() / 100.0))
            .collect();
        assert_eq!(costs, [("claude-code", 0.30), ("codex", 0.05)]);
        assert!(lines.iter().all(|line| line.run.as_ref() == Some(&run)));
    }

    #[test]
    fn lines_sum_by_day_across_months() {
        let (_home, ledger) = ledger();
        ledger
            .append(&[
                line(date!(2026 - 09 - 30), "codex", None, 0.50),
                line(date!(2026 - 10 - 01), "claude-code", None, 1.00),
                line(date!(2026 - 10 - 01), "codex", Some("run-01"), 0.25),
            ])
            .unwrap();
        let days = by_day(&ledger.read_since(None));
        let costs: Vec<(Date, f64)> = days.iter().map(|(d, s)| (*d, s.cost_usd)).collect();
        assert_eq!(
            costs,
            [(date!(2026 - 09 - 30), 0.50), (date!(2026 - 10 - 01), 1.25)]
        );
        assert_eq!(ledger.read_since(Some(date!(2026 - 10 - 01))).len(), 2);
    }

    #[test]
    fn the_period_filters_spend_tokens_and_runs_but_not_plan_usage_or_history() {
        let (_home, ledger) = ledger();
        let today = date!(2026 - 10 - 04);
        ledger
            .append(&[
                line(date!(2026 - 09 - 20), "codex", None, 4.00),
                line(date!(2026 - 10 - 01), "codex", None, 2.00),
                line(today, "claude-code", None, 1.00),
                line(today, "codex", Some("run-02"), 0.50),
            ])
            .unwrap();
        let runs = [
            run_summary("run-01", datetime!(2026-09-30 12:00 UTC)),
            run_summary("run-02", datetime!(2026-10-04 12:00 UTC)),
        ];
        let plan_usage = PlanUsageReading {
            agent_id: AgentId("codex".into()),
            status: PlanUsageStatus::Allowed,
            used: Some(0.92),
            resets_at: None,
            window: Some("five_hour".into()),
            source: PlanUsageSource::AppServer,
            observed_at: datetime!(2026-10-04 11:00 UTC),
            detail: None,
            wrap_up: false,
        };
        // Weeks start on Monday here, so this week reaches back past the month's start.
        let sum = |period| {
            let period = (period, WeekStart::Monday);
            summary(&ledger, period, today, &runs, vec![plan_usage.clone()])
        };

        let day = sum(UsagePeriod::Today);
        assert!((day.total.cost_usd - 1.50).abs() < 1e-9);
        assert_eq!(day.total.input_tokens, 2000);
        assert_eq!(day.total.cached_tokens, 1200);
        assert_eq!(day.total.output_tokens, 200);
        let shares: Vec<(&str, f64)> = day
            .by_agent
            .iter()
            .map(|share| (share.agent.as_str(), share.spent.cost_usd))
            .collect();
        assert_eq!(shares, [("claude-code", 1.00), ("codex", 0.50)]);
        let runs_of = |summary: &UsageSummary| -> Vec<String> {
            summary.runs.iter().map(|run| run.run.to_string()).collect()
        };
        assert_eq!(runs_of(&day), ["run-02"]);
        assert_eq!(day.plan_windows, std::slice::from_ref(&plan_usage));

        let week = sum(UsagePeriod::Week);
        assert!((week.total.cost_usd - 3.50).abs() < 1e-9);
        assert_eq!(runs_of(&week), ["run-02", "run-01"]);
        assert_eq!(week.plan_windows, day.plan_windows);

        let all = sum(UsagePeriod::All);
        assert!((all.total.cost_usd - 7.50).abs() < 1e-9);

        // A month from its 1st, and a week from Sunday when that's the day weeks start on.
        let month = sum(UsagePeriod::Month);
        assert!((month.total.cost_usd - 3.50).abs() < 1e-9);
        let from_sunday = (UsagePeriod::Week, WeekStart::Sunday);
        let week = summary(&ledger, from_sunday, today, &runs, Vec::new());
        assert!((week.total.cost_usd - 1.50).abs() < 1e-9);
        assert_eq!(runs_of(&week), ["run-02"]);

        // A share carries its days of this week whatever the period: here from Monday.
        let codex = day.by_agent.iter().find(|share| share.agent == "codex");
        let days: Vec<(Date, f64)> = codex
            .unwrap()
            .this_week
            .iter()
            .map(|spent| (spent.day, spent.spent.cost_usd))
            .collect();
        assert_eq!(days, [(date!(2026 - 10 - 01), 2.00), (today, 0.50)]);

        // A model's color goes by its tokens in all, so every period carries the same list.
        for summary in [&day, &week, &all] {
            let listed: Vec<(&str, u64)> = summary
                .all_time_models
                .iter()
                .map(|model| (model.agent.as_str(), model.tokens))
                .collect();
            assert_eq!(listed, [("codex", 3300), ("claude-code", 1100)]);
        }

        // History is 14 days whatever the period, today last.
        for summary in [&day, &week, &all] {
            assert_eq!(summary.history.len(), 14);
            assert_eq!(summary.history.last().unwrap().day, today);
            assert!((summary.history[13].spent.cost_usd - 1.50).abs() < 1e-9);
            assert!((summary.history[10].spent.cost_usd - 2.00).abs() < 1e-9);
            assert_eq!(summary.history[0].day, date!(2026 - 09 - 21));
        }
    }
}
