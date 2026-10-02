//! How cards, their records, and what they ask about name things: agents, diffs, money, budget
//! limits, and who decided.

use argonauts_core::budget::BudgetLimit;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::GateKind;
use argonauts_core::events::checkpoint::MergePacket;
use argonauts_core::events::run::Party;
use argonauts_core::limits::{Candidate, CostEstimate, TrackRecord};
use argonauts_core::settings::budget::{BudgetAmount, BudgetUnit};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{BudgetDimension, BudgetStatus, DiffStat};
use time::{OffsetDateTime, UtcOffset};

use crate::format::{self, ClockTime};

/// `4 files · +96 -12`.
pub fn diffstat(stat: &DiffStat) -> String {
    format!("{} · {}", files(stat.files_changed), lines(stat))
}

pub fn files(count: u32) -> String {
    plural(count, "file", "files")
}

/// `+96 -12`.
pub fn lines(stat: &DiffStat) -> String {
    format!("+{} -{}", stat.insertions, stat.deletions)
}

/// How the suite fared on the latest main, for a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    Passed,
    Failed,
    /// No command gate is set, so nothing ran.
    NotRun,
}

impl Suite {
    pub fn of(merge: &MergePacket) -> Self {
        let ran = merge
            .gates
            .iter()
            .any(|gate| gate.kind == GateKind::Command && gate.command.is_some());
        match (merge.suite_passed_on_latest_main, ran) {
            (true, _) => Self::Passed,
            (false, true) => Self::Failed,
            (false, false) => Self::NotRun,
        }
    }
}

pub fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

pub fn amount(amount: &BudgetAmount) -> String {
    match amount.unit {
        BudgetUnit::Usd => format::usd(amount.amount),
        BudgetUnit::Tokens => format!("{} tokens", format::tokens(amount.amount.max(0.0) as u64)),
        BudgetUnit::Quota => format!("{} of quota", percent(amount.amount)),
        BudgetUnit::Minutes => minutes(amount.amount),
    }
}

/// A share from 0 to 1 as `62%`.
pub fn percent(share: f64) -> String {
    format!("{:.0}%", (share * 100.0).clamp(0.0, 999.0))
}

pub fn minutes(minutes: f64) -> String {
    format::duration_compact(std::time::Duration::from_secs_f64(minutes.max(0.0) * 60.0))
}

/// What the meter read when a limit stopped the work, such as `$2.10 of $2.00`.
pub fn budget_reading(status: &BudgetStatus, dimension: BudgetDimension) -> String {
    match dimension {
        BudgetDimension::Spend => match status.limit {
            BudgetLimit::Usd(limit) => {
                format!("{} of {}", format::spend(&status.spend), format::usd(limit))
            }
            BudgetLimit::Tokens(limit) => format!(
                "{} of {} tokens",
                format::tokens(status.spend.input_tokens + status.spend.output_tokens),
                format::tokens(limit)
            ),
            BudgetLimit::None => format!("{} spent", format::spend(&status.spend)),
        },
        BudgetDimension::Minutes => match status.work_limits.max_minutes {
            Some(limit) => format!("{} of {}", minutes(status.work.minutes), minutes(limit)),
            None => minutes(status.work.minutes),
        },
        BudgetDimension::ToolCalls => match status.work_limits.max_tool_calls {
            Some(limit) => format!("{} of {limit} tool calls", status.work.tool_calls),
            None => plural(
                u32::try_from(status.work.tool_calls).unwrap_or(u32::MAX),
                "tool call",
                "tool calls",
            ),
        },
    }
}

pub fn dimension(dimension: BudgetDimension) -> &'static str {
    match dimension {
        BudgetDimension::Spend => "spend",
        BudgetDimension::Minutes => "time",
        BudgetDimension::ToolCalls => "tool call",
    }
}

/// Who decided, for a record line: `you`, `your rules`, `the approver model`.
pub fn party(party: &Party, names: &DisplayNames) -> String {
    match party {
        Party::User => "you".to_owned(),
        Party::Rules => "your rules".to_owned(),
        Party::Approver { .. } => "the approver model".to_owned(),
        Party::Config => "a setting".to_owned(),
        Party::Conductor => "the conductor".to_owned(),
        Party::Agent { worker, .. } => names.worker(worker),
        Party::FrontEnd { client } => client.clone(),
    }
}

pub fn review(verdict: Option<&ReviewVerdict>) -> &'static str {
    match verdict {
        Some(ReviewVerdict::Approve) => "approved",
        Some(ReviewVerdict::RequestChanges) => "changes asked",
        Some(ReviewVerdict::Reject) => "rejected",
        None => "not reviewed",
    }
}

pub fn track_record(record: Option<&TrackRecord>) -> String {
    match record {
        Some(record) => format!("{} of {} passed", record.passed, record.of),
        None => "no record yet".to_owned(),
    }
}

/// A candidate's estimated cost, with what you should know before picking it.
pub fn cost(candidate: &Candidate) -> String {
    let mut text = match candidate.cost {
        CostEstimate::Usd(usd) => format!("~{}", format::usd(usd)),
        CostEstimate::Quota(share) => format!("~{} of quota", percent(share)),
        CostEstimate::Free => "free".to_owned(),
        CostEstimate::Unknown => "unknown".to_owned(),
    };
    if candidate.may_run_out {
        text.push_str(", may run out");
    }
    if candidate.needs_confirm {
        text.push_str(", you confirm");
    }
    text
}

/// A clock time in the terminal's zone, `10:58 AM`, written as the status line writes
/// `out until 3:45 PM`. Events carry UTC, so `offset` is the one read at startup.
pub fn clock(at: OffsetDateTime, offset: UtcOffset) -> String {
    ClockTime::from(at.to_offset(offset)).to_string()
}

/// `and`-joined, such as `codex, opencode and antigravity-cli`.
pub fn joined(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
