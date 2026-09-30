//! Short human-readable forms for durations, token counts, money, paths, and diff stats.

use std::path::Path;

use argonauts_core::{BudgetDimension, BudgetLimit, BudgetStatus, DiffStat, Spend, StopCause};

/// Elapsed time in the live block style, like `0m 38s`, or `1h 02m` past an hour.
pub(crate) fn elapsed(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// A short duration for gates and commands, like `0.4 s`, `12.0 s`, or `1m 05s`.
pub(crate) fn seconds(ms: u64) -> String {
    if ms >= 60_000 {
        elapsed(ms)
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

pub(crate) fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1_000.0),
        10_000..1_000_000 => format!("{}k", (n as f64 / 1_000.0).round() as u64),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

pub(crate) fn usd(amount: f64, lower_bound: bool) -> String {
    let prefix = if lower_bound { "≥ " } else { "" };
    format!("{prefix}${amount:.2}")
}

/// Spend against a limit in the limit's unit, like `$0.06 of $3.00` or `12k of 500k tok`.
pub(crate) fn spend(spend: &Spend, limit: BudgetLimit) -> String {
    let dollars = || {
        let amount = usd(spend.cost_usd, spend.cost_partial);
        if spend.cost_estimated && !spend.cost_partial {
            format!("≈ {amount}")
        } else {
            amount
        }
    };
    match limit {
        BudgetLimit::Usd(max) => format!("{} of {}", dollars(), usd(max, false)),
        BudgetLimit::Tokens(max) => {
            format!("{} of {} tok", tokens(spend.billable_tokens()), tokens(max))
        }
        BudgetLimit::None if spend.cost_usd > 0.0 || spend.cost_partial => dollars(),
        BudgetLimit::None => format!("{} tok", tokens(spend.billable_tokens())),
    }
}

/// The limit a budget status is closest to, like `$3.00`, `60 min`, or `300 tool calls`.
pub(crate) fn budget_limit(status: &BudgetStatus) -> String {
    let limits = status.work_limits;
    match status.binding {
        Some(BudgetDimension::Minutes) => limits
            .max_minutes
            .map_or_else(|| "the time limit".to_owned(), |m| format!("{m} min")),
        Some(BudgetDimension::ToolCalls) => limits.max_tool_calls.map_or_else(
            || "the tool-call limit".to_owned(),
            |n| count(n, "tool call", "tool calls"),
        ),
        Some(BudgetDimension::Spend) | None => status.limit.to_string(),
    }
}

pub(crate) fn stopped_card(cause: StopCause, has_report: bool) -> String {
    let why = match cause {
        StopCause::Budget => "budget hit",
        StopCause::User => "stopped by you",
        StopCause::Timeout => "time limit hit",
    };
    let report = if has_report { "" } else { " · no report" };
    format!("{why}{report} · partial work")
}

pub(crate) fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

pub(crate) fn display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok()) {
        if rest.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

pub(crate) fn diff_stat(stat: &DiffStat) -> String {
    format!(
        "{} · +{} −{}",
        count(stat.files_changed as u64, "file", "files"),
        stat.insertions,
        stat.deletions
    )
}

pub(crate) fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn count(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn elapsed_uses_minutes_then_hours() {
        assert_eq!(elapsed(38_400), "0m 38s");
        assert_eq!(elapsed(372_000), "6m 12s");
        assert_eq!(elapsed(3_720_000), "1h 02m");
    }

    #[test]
    fn seconds_is_short() {
        assert_eq!(seconds(400), "0.4 s");
        assert_eq!(seconds(8_240), "8.2 s");
        assert_eq!(seconds(65_000), "1m 05s");
    }

    #[test]
    fn tokens_are_compact() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1_100), "1.1k");
        assert_eq!(tokens(38_400), "38k");
        assert_eq!(tokens(1_230_000), "1.2M");
    }

    #[test]
    fn spend_follows_the_limit_unit() {
        let s = Spend {
            input_tokens: 12_000,
            cached_tokens: 2_000,
            output_tokens: 500,
            cost_usd: 0.061,
            ..Spend::default()
        };
        assert_eq!(spend(&s, BudgetLimit::Usd(3.0)), "$0.06 of $3.00");
        assert_eq!(spend(&s, BudgetLimit::Tokens(500_000)), "11k of 500k tok");
        assert_eq!(spend(&s, BudgetLimit::None), "$0.06");
        let partial = Spend {
            cost_partial: true,
            ..s
        };
        assert_eq!(spend(&partial, BudgetLimit::Usd(3.0)), "≥ $0.06 of $3.00");
        let tokens_only = Spend { cost_usd: 0.0, ..s };
        assert_eq!(spend(&tokens_only, BudgetLimit::None), "11k tok");
        let estimated = Spend {
            cost_estimated: true,
            ..s
        };
        assert_eq!(spend(&estimated, BudgetLimit::Usd(3.0)), "≈ $0.06 of $3.00");
    }

    #[test]
    fn budget_limits_name_what_binds() {
        let mut status = BudgetStatus {
            level: argonauts_core::BudgetAction::Stop,
            limit: BudgetLimit::Usd(3.0),
            spend: Spend::default(),
            fraction_used: Some(1.0),
            work: argonauts_core::WorkUsed::default(),
            work_limits: argonauts_core::WorkLimits {
                max_minutes: Some(60.0),
                max_tool_calls: Some(300),
            },
            binding: Some(BudgetDimension::Spend),
        };
        assert_eq!(budget_limit(&status), "$3.00");
        status.binding = Some(BudgetDimension::Minutes);
        assert_eq!(budget_limit(&status), "60 min");
        status.binding = Some(BudgetDimension::ToolCalls);
        assert_eq!(budget_limit(&status), "300 tool calls");
        assert_eq!(
            stopped_card(StopCause::Budget, false),
            "budget hit · no report · partial work"
        );
        assert_eq!(
            stopped_card(StopCause::User, true),
            "stopped by you · partial work"
        );
    }

    #[test]
    fn paths_use_tilde_for_home() {
        let home = PathBuf::from("/home/you");
        assert_eq!(
            display_path(Path::new("/home/you/code/api"), Some(&home)),
            "~/code/api"
        );
        assert_eq!(display_path(Path::new("/home/you"), Some(&home)), "~");
        assert_eq!(display_path(Path::new("/srv/api"), Some(&home)), "/srv/api");
        assert_eq!(display_path(Path::new("/srv/api"), None), "/srv/api");
    }

    #[test]
    fn small_helpers() {
        assert_eq!(short_sha("3f9c2e1aa"), "3f9c2e1");
        assert_eq!(short_sha("abc"), "abc");
        let stat = DiffStat {
            files_changed: 1,
            insertions: 1,
            deletions: 0,
        };
        assert_eq!(diff_stat(&stat), "1 file · +1 −0");
        assert_eq!(count(2, "file", "files"), "2 files");
    }
}
