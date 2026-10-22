//! How questions, their records, and what they ask about name things: agents, diffs, money, limit
//! limits, and who decided.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::approval::MergePacket;
use argonauts_core::events::run::Party;
use argonauts_core::events::task::CheckKind;
use argonauts_core::limits::{Candidate, CostEstimate, TrackRecord};
use argonauts_core::meter::MeterLimit;
use argonauts_core::settings::limit::{LimitAmount, LimitUnit};
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{DiffStat, LimitDimension, SpendStatus};
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

/// `4 files · +52 −6`, with a minus sign, as the questions about a folder without git write
/// it.
pub fn diffstat_signed(stat: &DiffStat) -> String {
    format!("{} · {}", files(stat.files_changed), lines_signed(stat))
}

/// `+52 −6`.
pub fn lines_signed(stat: &DiffStat) -> String {
    format!("+{} \u{2212}{}", stat.insertions, stat.deletions)
}

/// How the suite fared on the latest main, for a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    Passed,
    Failed,
    /// No command check is set, so nothing ran.
    NotRun,
}

impl Suite {
    pub fn of(merge: &MergePacket) -> Self {
        let ran = merge
            .checks
            .iter()
            .any(|check| check.kind == CheckKind::Command && check.command.is_some());
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

pub fn amount(amount: &LimitAmount) -> String {
    match amount.unit {
        LimitUnit::Usd => format::usd(amount.amount),
        LimitUnit::Tokens => format!("{} tokens", format::tokens(amount.amount.max(0.0) as u64)),
        LimitUnit::PlanUsage => format!("{} of plan usage", percent(amount.amount)),
        LimitUnit::Minutes => minutes(amount.amount),
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
pub fn limit_reading(status: &SpendStatus, dimension: LimitDimension) -> String {
    match dimension {
        LimitDimension::Spend => match status.limit {
            MeterLimit::Usd(limit) => {
                format!("{} of {}", format::spend(&status.spend), format::usd(limit))
            }
            MeterLimit::Tokens(limit) => format!(
                "{} of {} tokens",
                format::tokens(status.spend.input_tokens + status.spend.output_tokens),
                format::tokens(limit)
            ),
            MeterLimit::None => format!("{} spent", format::spend(&status.spend)),
        },
        LimitDimension::Minutes => match status.work_limits.max_minutes {
            Some(limit) => format!("{} of {}", minutes(status.work.minutes), minutes(limit)),
            None => minutes(status.work.minutes),
        },
        LimitDimension::ToolCalls => match status.work_limits.max_tool_calls {
            Some(limit) => format!("{} of {limit} tool calls", status.work.tool_calls),
            None => plural(
                u32::try_from(status.work.tool_calls).unwrap_or(u32::MAX),
                "tool call",
                "tool calls",
            ),
        },
    }
}

pub fn dimension(dimension: LimitDimension) -> &'static str {
    match dimension {
        LimitDimension::Spend => "spend",
        LimitDimension::Minutes => "time",
        LimitDimension::ToolCalls => "tool call",
    }
}

/// Who decided, for a record line: `you`, `your rules`, `the judge`.
pub fn party(party: &Party, names: &DisplayNames) -> String {
    match party {
        Party::User | Party::Declined => "you".to_owned(),
        Party::Rules => "your rules".to_owned(),
        Party::Judge { .. } => "the judge".to_owned(),
        Party::Config => "a setting".to_owned(),
        Party::Autopilot => "autopilot".to_owned(),
        Party::ArgonautsCode => "Argonauts Code".to_owned(),
        Party::Network => "the network".to_owned(),
        Party::Agent { worker, .. } => names.worker(worker),
        Party::OutsideAgent { client } => client.clone(),
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
        CostEstimate::PlanUsage(share) => format!("~{} of plan usage", percent(share)),
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

/// A clock time in the terminal's zone, `10:58 AM`, written as the bottom line writes
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
