//! Doctor's lines about the import: Claude Code's tokens as counted here beside the total its
//! own /stats shows, and why the two differ. The two can't match: /stats adds up every line
//! Claude Code writes as an answer streams, so an answer written over several lines counts
//! more than once, and it leaves out the agents its workflows start. Here each answer counts
//! once and those agents count too.

use std::path::Path;

use argonauts_core::Paths;
use time::{Date, UtcOffset};

use super::stats_cache::{self, StatsCache};
use super::{Locations, Source};
use crate::overview::records::{Record, Store};

/// Why the two totals differ, under the line that gives them.
const WHY: &str = "  /stats counts an answer once per line Claude Code writes for it and leaves out its \
                   workflows' agents; here each answer counts once, agents included";

/// Doctor's lines for the import on this computer; none until you said yes to it.
pub(crate) fn doctor_lines(paths: &Paths) -> Vec<String> {
    lines(&Store::new(paths), Locations::from_env().claude.as_deref())
}

fn lines(store: &Store, claude: Option<&Path>) -> Vec<String> {
    let state = store.state();
    if state.import != Some(true) {
        return Vec::new();
    }
    let records = store.records();
    let cache = claude.map(stats_cache::read).unwrap_or(Ok(None));
    let mut lines = vec![totals_line(&records, &cache)];
    if matches!(&cache, Ok(Some(cache)) if cache.through.is_some()) {
        lines.push(WHY.to_owned());
    }
    let days = whole_days(&records);
    if days > 0 {
        let s = if days == 1 { "" } else { "s" };
        lines.push(format!(
            "  {days} day{s} whose transcripts Claude Code deleted came from its stats-cache.json"
        ));
    }
    if state.unreadable > 0 {
        let s = if state.unreadable == 1 { "" } else { "s" };
        lines.push(format!(
            "  {} line{s} of the CLIs' session files couldn't be read",
            state.unreadable
        ));
    }
    lines
}

/// `Claude Code tokens · 73.5b counted here, 60.2b in its /stats through Oct 8`.
fn totals_line(records: &[Record], cache: &Result<Option<StatsCache>, String>) -> String {
    let ours = |through: Option<Date>| -> u64 {
        records
            .iter()
            .filter(|record| record.cli == Source::Claude.cli())
            .filter(|record| {
                through.is_none_or(|through| record.at.to_offset(UtcOffset::UTC).date() <= through)
            })
            .map(|record| record.input + record.output + record.cache_read + record.cache_write)
            .sum()
    };
    match cache {
        Ok(Some(cache)) => match cache.through {
            Some(through) => format!(
                "Claude Code tokens · {} counted here, {} in its /stats through {}",
                short(ours(Some(through))),
                short(cache.total()),
                day_words(through),
            ),
            None => format!(
                "Claude Code tokens · {} counted here · its /stats hasn't counted a day yet",
                short(ours(None)),
            ),
        },
        Ok(None) => format!(
            "Claude Code tokens · {} counted here · its /stats total isn't kept until /stats runs",
            short(ours(None)),
        ),
        Err(why) => format!(
            "Claude Code tokens · {} counted here · its stats-cache.json couldn't be read: {why}",
            short(ours(None)),
        ),
    }
}

/// Days taken whole from Claude Code's own totals.
fn whole_days(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| record.whole_day)
        .map(|record| record.at.to_offset(UtcOffset::UTC).date())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// `950`, `48.2k`, `3.4m`, `118.4b`.
fn short(count: u64) -> String {
    let count = count as f64;
    match count {
        n if n >= 1e9 => format!("{:.1}b", n / 1e9),
        n if n >= 1e6 => format!("{:.1}m", n / 1e6),
        n if n >= 1e3 => format!("{:.1}k", n / 1e3),
        n => format!("{n}"),
    }
}

/// `Oct 8`.
fn day_words(day: Date) -> String {
    let month = day.month().to_string();
    format!("{} {}", &month[..3], day.day())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::records::ImportState;
    use time::macros::datetime;

    fn record(at: time::OffsetDateTime, tokens: u64, whole_day: bool) -> Record {
        Record {
            at,
            cli: "claude-code".into(),
            model: "claude-opus-5-5".into(),
            folder: String::new(),
            input: tokens,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            session: 1,
            subagent: false,
            whole_day,
        }
    }

    #[test]
    fn doctor_puts_the_total_here_beside_claude_codes_own_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(&dir.path().join("imported"));
        let claude = dir.path().join("claude");
        assert!(lines(&store, Some(&claude)).is_empty(), "not imported yet");
        store
            .save_state(&ImportState {
                import: Some(true),
                unreadable: 3,
                ..ImportState::default()
            })
            .unwrap();
        store
            .append(&[
                record(datetime!(2026-08-01 12:00 UTC), 1_500_000_000, true),
                record(datetime!(2026-10-08 21:00 UTC), 2_000_000_000, false),
                // After the day Claude Code last counted, so left out of the comparison.
                record(datetime!(2026-10-09 09:00 UTC), 9_000_000_000, false),
            ])
            .unwrap();
        assert_eq!(
            lines(&store, Some(&claude))[0],
            "Claude Code tokens · 12.5b counted here · its /stats total isn't kept until /stats runs"
        );
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("stats-cache.json"),
            serde_json::json!({
                "version": 5,
                "lastComputedDate": "2026-10-08",
                "dailyActivity": [],
                "dailyModelTokens": [],
                "modelUsage": {"claude-opus-5-5": {
                    "inputTokens": 100_000_000, "outputTokens": 400_000_000,
                    "cacheReadInputTokens": 2_000_000_000, "cacheCreationInputTokens": 300_000_000
                }},
                "totalSessions": 1,
                "totalMessages": 1
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            lines(&store, Some(&claude)),
            vec![
                "Claude Code tokens · 3.5b counted here, 2.8b in its /stats through Oct 8"
                    .to_owned(),
                WHY.to_owned(),
                "  1 day whose transcripts Claude Code deleted came from its stats-cache.json"
                    .to_owned(),
                "  3 lines of the CLIs' session files couldn't be read".to_owned(),
            ]
        );
    }
}
