//! Doctor's lines about the Overview's tokens: each CLI's total as the Tokens group counts it,
//! beside a second count by plainer rules and Claude Code's own /stats, with the days where the
//! two counts are different. Claude Code's /stats isn't expected to match: here each answer
//! counts once by its id, its workflows' agents count too, and lines copied into a forked
//! session don't.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::Paths;
use argonauts_core::base::text::and_list;
use argonauts_core::look::display_names::DisplayNames;
use argonauts_tui::base::format::Figures;
use argonauts_tui::base::text::str_width;
use time::{Date, UtcOffset};

use super::Locations;
use super::stats_cache::{self, StatsCache};
use crate::overview::build::{line_tokens, record_tokens};
use crate::overview::records::{Record, Store};
use crate::overview::recount::{self, CLAUDE_CODE, Days, Folders, Recount};
use crate::usage::usage_ledger::{LedgerLine, UsageLedger};

/// Why Claude Code's /stats is a different number, under the lines that give it.
const WHY: &str = "  /stats has its own rules: here an answer counts once by its id, workflows' \
                   agents included, and a forked session's copied lines are left out";

/// The most days named for one CLI; the rest are counted.
const DAYS_NAMED: usize = 4;

/// Doctor's lines for the tokens on this computer. Claude Code's and Codex's folders are read
/// only once you said yes to the import.
pub(crate) fn doctor_lines(paths: &Paths) -> Vec<String> {
    let ledger = UsageLedger::new(paths).read_since(None);
    lines(&Store::new(paths), &Locations::from_env(), &ledger)
}

fn lines(store: &Store, locations: &Locations, ledger: &[LedgerLine]) -> Vec<String> {
    let state = store.state();
    let imported = state.import == Some(true);
    let records = if imported {
        store.records().to_vec()
    } else {
        Vec::new()
    };
    let folders = Folders {
        claude: locations.claude.as_deref().filter(|_| imported),
        codex: locations.codex.as_deref().filter(|_| imported),
        zstd: locations.zstd.as_deref(),
    };
    let counted = counted(&records, ledger);
    let again = recount::recount(&folders, ledger);
    if counted.is_empty() && again.clis.is_empty() {
        return Vec::new();
    }
    let cache = folders.claude.map_or(Ok(None), stats_cache::read);
    let mut lines = vec!["Tokens by CLI · as Overview counts them, then counted again".to_owned()];
    lines.extend(cli_lines(&counted, &again, &cache));
    if matches!(&cache, Ok(Some(cache)) if cache.through.is_some()) {
        lines.push(WHY.to_owned());
    }
    lines.extend(days_apart_lines(&counted, &again));
    let notes = [
        (
            whole_days(&records),
            "day",
            "whose transcripts Claude Code deleted came from its stats-cache.json",
        ),
        (
            state.unreadable as usize,
            "line",
            "of the CLIs' session files couldn't be read",
        ),
        (
            again.unopened as usize,
            "session file",
            "the second count couldn't open",
        ),
    ];
    for (count, what, rest) in notes.into_iter().filter(|(count, ..)| *count > 0) {
        lines.push(format!("  {} {rest}", many(count, what)));
    }
    lines
}

/// One CLI's tokens as the Tokens group counts them.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Counted {
    pub total: u64,
    /// By day, leaving out days taken whole from Claude Code's own totals, which no session file
    /// holds any more.
    pub days: Days,
}

/// Each CLI's tokens as the Tokens group sums them: the import's records for the CLIs it read,
/// the ledger for the rest. Records are dated on UTC's clock, ledger lines on yours.
pub(super) fn counted(records: &[Record], ledger: &[LedgerLine]) -> BTreeMap<String, Counted> {
    let mut out: BTreeMap<String, Counted> = BTreeMap::new();
    for record in records {
        let tokens = record_tokens(record).total();
        let cli = out.entry(record.cli.clone()).or_default();
        cli.total += tokens;
        if !record.whole_day {
            *cli.days
                .entry(record.at.to_offset(UtcOffset::UTC).date())
                .or_default() += tokens;
        }
    }
    let imported: BTreeSet<&str> = records.iter().map(|record| record.cli.as_str()).collect();
    for line in ledger {
        if imported.contains(line.agent.as_str()) {
            continue;
        }
        let tokens = line_tokens(line).total();
        let cli = out.entry(line.agent.clone()).or_default();
        cli.total += tokens;
        *cli.days.entry(line.day).or_default() += tokens;
    }
    out.retain(|_, cli| cli.total > 0);
    out
}

/// Days where the two counts are different: the day, the Tokens group's count and the second
/// one.
pub(super) fn days_apart(counted: &Days, again: &Days) -> Vec<(Date, u64, u64)> {
    let days: BTreeSet<&Date> = counted.keys().chain(again.keys()).collect();
    days.into_iter()
        .map(|day| {
            let here = counted.get(day).copied().unwrap_or(0);
            (*day, here, again.get(day).copied().unwrap_or(0))
        })
        .filter(|(_, here, again)| here != again)
        .collect()
}

/// `  Claude Code   60.80b · counted again 60.80b · its /stats 60.60b through Oct 8`, the most
/// first.
fn cli_lines(
    counted: &BTreeMap<String, Counted>,
    again: &Recount,
    cache: &Result<Option<StatsCache>, String>,
) -> Vec<String> {
    let names = DisplayNames::default();
    let mut clis: Vec<(&str, u64, u64)> = counted
        .keys()
        .chain(again.clis.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|cli| {
            let here = counted.get(cli).map_or(0, |counted| counted.total);
            (cli.as_str(), here, again.total(cli))
        })
        .collect();
    clis.sort_by_key(|(cli, here, again)| (std::cmp::Reverse((*here, *again)), *cli));
    let width = clis
        .iter()
        .map(|(cli, ..)| str_width(&names.cli(cli)))
        .max()
        .unwrap_or(0);
    clis.into_iter()
        .map(|(cli, here, again)| {
            let name = names.cli(cli);
            let pad = " ".repeat(width - str_width(&name));
            let mut line = format!(
                "  {name}{pad}   {} · counted again {}",
                Figures::Table.tokens(here),
                Figures::Table.tokens(again)
            );
            if cli == CLAUDE_CODE {
                line.push_str(&stats_words(cache));
            }
            line
        })
        .collect()
}

/// What Claude Code's own /stats says, after its line.
fn stats_words(cache: &Result<Option<StatsCache>, String>) -> String {
    match cache {
        Ok(Some(cache)) => match cache.through {
            Some(through) => format!(
                " · its /stats {} through {}",
                Figures::Table.tokens(cache.total()),
                day_words(through)
            ),
            None => " · its /stats hasn't counted a day yet".to_owned(),
        },
        Ok(None) => " · its /stats total isn't kept until /stats runs".to_owned(),
        Err(why) => format!(" · its stats-cache.json couldn't be read: {why}"),
    }
}

/// `  Codex isn't the same on Oct 3 (1.20M, counted again 1.10M), Oct 5 (…) and 2 more days`.
fn days_apart_lines(counted: &BTreeMap<String, Counted>, again: &Recount) -> Vec<String> {
    let names = DisplayNames::default();
    let none = Days::new();
    let clis: BTreeSet<&String> = counted.keys().chain(again.clis.keys()).collect();
    clis.into_iter()
        .filter_map(|cli| {
            let here = counted.get(cli).map_or(&none, |counted| &counted.days);
            let days = days_apart(here, again.clis.get(cli).unwrap_or(&none));
            if days.is_empty() {
                return None;
            }
            let mut named: Vec<String> = days
                .iter()
                .take(DAYS_NAMED)
                .map(|(day, here, again)| {
                    format!(
                        "{} ({}, counted again {})",
                        day_words(*day),
                        Figures::Table.tokens(*here),
                        Figures::Table.tokens(*again)
                    )
                })
                .collect();
            let more = days.len().saturating_sub(DAYS_NAMED);
            if more > 0 {
                named.push(many(more, "more day"));
            }
            Some(format!(
                "  {} isn't the same on {}",
                names.cli(cli),
                and_list(&named)
            ))
        })
        .collect()
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

/// `1 day`, `3 days`.
fn many(count: usize, what: &str) -> String {
    let s = if count == 1 { "" } else { "s" };
    format!("{count} {what}{s}")
}

/// `Oct 8`.
fn day_words(day: Date) -> String {
    let month = day.month().to_string();
    format!("{} {}", &month[..3], day.day())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

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

    fn ledger_line(agent: &str, day: &str, input: u64, cached: u64, output: u64) -> LedgerLine {
        serde_json::from_value(serde_json::json!({
            "day": day, "repo": "/work/app", "agent": agent, "agent_id": "a1",
            "input": input, "cached": cached, "output": output, "cost": 0.0
        }))
        .unwrap()
    }

    fn locations(claude: &Path) -> Locations {
        Locations {
            claude: Some(claude.to_path_buf()),
            codex: None,
            zstd: None,
        }
    }

    #[test]
    fn doctor_gives_each_clis_tokens_beside_a_second_count_and_names_the_days_they_part() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(&dir.path().join("imported"));
        let claude = dir.path().join("claude");
        let ledger = [ledger_line(
            "antigravity-cli",
            "2026-10-08",
            300_000_000,
            100_000_000,
            5_000_000,
        )];
        assert_eq!(
            lines(&store, &locations(&claude), &ledger),
            vec![
                "Tokens by CLI · as Overview counts them, then counted again".to_owned(),
                "  agy   305.00M · counted again 305.00M".to_owned(),
            ],
            "before the import only the ledger is counted"
        );
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
                // No session file holds this one, so the second count is different on its day.
                record(datetime!(2026-10-09 09:00 UTC), 9_000_000_000, false),
            ])
            .unwrap();
        let session = claude.join("projects/-work-app/s1.jsonl");
        std::fs::create_dir_all(session.parent().unwrap()).unwrap();
        std::fs::write(
            &session,
            serde_json::json!({
                "type": "assistant", "timestamp": "2026-10-08T21:10:00.000Z",
                "message": {"id": "m1", "model": "claude-opus-5-5",
                    "usage": {"input_tokens": 2_000_000_000u64, "output_tokens": 0}}
            })
            .to_string(),
        )
        .unwrap();
        let apart = "  Claude Code isn't the same on Oct 9 (9.00b, counted again 0.00M)".to_owned();
        let rest = [
            "  1 day whose transcripts Claude Code deleted came from its stats-cache.json"
                .to_owned(),
            "  3 lines of the CLIs' session files couldn't be read".to_owned(),
        ];
        let mut expected = vec![
            "Tokens by CLI · as Overview counts them, then counted again".to_owned(),
            "  Claude Code   12.50b · counted again 2.00b · its /stats total isn't kept until /stats \
             runs"
                .to_owned(),
            "  agy           305.00M · counted again 305.00M".to_owned(),
            apart.clone(),
        ];
        expected.extend(rest.clone());
        assert_eq!(lines(&store, &locations(&claude), &ledger), expected);
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
        let lines = lines(&store, &locations(&claude), &ledger);
        assert_eq!(
            lines[1],
            "  Claude Code   12.50b · counted again 2.00b · its /stats 2.80b through Oct 8"
        );
        assert_eq!(
            lines[3..],
            [vec![WHY.to_owned(), apart], rest.to_vec()].concat()
        );
    }

    #[test]
    fn many_days_apart_are_named_four_at_most() {
        let counted: Days = (1..=6)
            .map(|day| {
                (
                    Date::from_calendar_date(2026, time::Month::October, day).unwrap(),
                    10,
                )
            })
            .collect();
        let again = Recount::default();
        let here = BTreeMap::from([(
            "codex".to_owned(),
            Counted {
                total: 60,
                days: counted,
            },
        )]);
        assert_eq!(
            days_apart_lines(&here, &again),
            vec![
                "  Codex isn't the same on Oct 1 (<0.01M, counted again 0.00M), \
                 Oct 2 (<0.01M, counted again 0.00M), Oct 3 (<0.01M, counted again 0.00M), \
                 Oct 4 (<0.01M, counted again 0.00M) and 2 more days"
                    .to_owned()
            ]
        );
    }
}
