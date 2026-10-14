//! The Overview's numbers, summed from what's recorded: the import's records for each CLI it
//! read, and the usage ledger for every other CLI (whose work is what ran through Argonauts),
//! with the work ledger for what Argonauts itself did. A CLI the import read is counted from
//! its own files only, since those already hold the sessions Argonauts ran on it.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::builtin_price;
use argonauts_core::overview::{
    CliSource, ImportStatus, OverviewData, OverviewDay, OverviewModel, OverviewMonth,
    OverviewPeriod, OverviewWeek, ThroughArgonauts, Tokens,
};
use argonauts_core::settings::overview::YearColors;
use argonauts_tui::format::WallClock;
use time::{Date, Duration, OffsetDateTime};

use super::records::Record;
use crate::usage_ledger::LedgerLine;
use crate::work_ledger::{Work, WorkLine};

/// Weeks the year of days covers.
const YEAR_WEEKS: i64 = 53;
/// Weeks the weekly bars cover.
const BAR_WEEKS: i64 = 26;

/// A CLI and one of its models.
type ModelKey<'a> = (&'a str, Option<&'a str>);

/// One answer or a few, from either source, in the terms the Overview counts.
struct Entry<'a> {
    day: Date,
    hour: Option<u8>,
    /// When it happened, where known to the hour, for timing sessions.
    at: Option<OffsetDateTime>,
    agent: &'a str,
    model: Option<&'a str>,
    folder: &'a str,
    tokens: Tokens,
    session: String,
    subagent: bool,
}

/// What the numbers are summed from.
pub(crate) struct Sources<'a> {
    pub records: &'a [Record],
    pub lines: &'a [LedgerLine],
    pub work: &'a [WorkLine],
    pub clock: WallClock,
}

/// The Overview for `period` as of `today`.
pub(crate) fn overview(
    sources: &Sources<'_>,
    period: OverviewPeriod,
    today: Date,
    import: ImportStatus,
    year_colors: YearColors,
    cli_name: &dyn Fn(&str) -> String,
) -> OverviewData {
    let entries = entries(sources);
    let imported: BTreeSet<&str> = sources.records.iter().map(|r| r.cli.as_str()).collect();
    let first = period.first_day(today);
    let in_period = |entry: &&Entry<'_>| first.is_none_or(|first| entry.day >= first);
    let counted: Vec<&Entry<'_>> = entries.iter().filter(in_period).collect();

    let mut data = OverviewData::empty(period, today);
    data.import = import;
    data.year_colors = year_colors;

    let mut models: BTreeMap<ModelKey<'_>, Tokens> = BTreeMap::new();
    let mut session_tokens: BTreeMap<(&str, &str), BTreeMap<Option<&str>, u64>> = BTreeMap::new();
    let mut folders: BTreeMap<&str, u64> = BTreeMap::new();
    let mut active: BTreeSet<Date> = BTreeSet::new();
    let mut months: BTreeMap<Date, BTreeMap<&str, u64>> = BTreeMap::new();
    for entry in &counted {
        let tokens = entry.tokens.total();
        data.total.add(&entry.tokens);
        models
            .entry((entry.agent, entry.model))
            .or_default()
            .add(&entry.tokens);
        *session_tokens
            .entry((entry.agent, entry.session.as_str()))
            .or_default()
            .entry(entry.model)
            .or_default() += tokens;
        *folders.entry(entry.folder).or_default() += tokens;
        if let Some(hour) = entry.hour {
            data.hours[usize::from(hour).min(23)] += tokens;
        }
        if entry.subagent {
            data.subagent_tokens += tokens;
        }
        if tokens > 0 {
            active.insert(entry.day);
        }
        let month = entry.day.replace_day(1).unwrap_or(entry.day);
        *months
            .entry(month)
            .or_default()
            .entry(entry.agent)
            .or_default() += tokens;
    }

    // A session counts once, under the model that did most of its work, as when its subagents
    // ran a smaller model, so the column adds up to the Sessions tile.
    let mut sessions_of: BTreeMap<ModelKey<'_>, u32> = BTreeMap::new();
    for ((agent, _), by_model) in &session_tokens {
        let most = by_model
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)));
        if let Some((model, _)) = most {
            *sessions_of.entry((agent, *model)).or_default() += 1;
        }
    }
    data.models = models
        .into_iter()
        .map(|((agent, model), tokens)| OverviewModel {
            agent: agent.to_owned(),
            model: model.map(str::to_owned),
            api_value: model.and_then(|model| api_value(agent, model, &tokens)),
            tokens,
            sessions: sessions_of.get(&(agent, model)).copied().unwrap_or(0),
        })
        .collect();
    data.models
        .sort_by_key(|model| std::cmp::Reverse(model.tokens.total()));
    data.api_value = data.models.iter().filter_map(|model| model.api_value).sum();
    data.api_partial = data.models.iter().any(|model| model.api_value.is_none());

    data.sources = sources_of(&entries, &counted, &imported, &data);
    data.found = found(&entries, &imported, today, cli_name);

    let mut folders: Vec<(String, u64)> = folders
        .into_iter()
        .filter(|(folder, _)| !folder.is_empty())
        .map(|(folder, tokens)| (folder.to_owned(), tokens))
        .collect();
    folders.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    data.folders = folders;

    data.active_days = active.len() as u32;
    let earliest = entries.iter().map(|entry| entry.day).min().unwrap_or(today);
    let span_from = first.unwrap_or(earliest).max(earliest.min(today));
    data.span_days = ((today - span_from).whole_days() + 1).max(1) as u32;

    let every_day: BTreeSet<Date> = entries
        .iter()
        .filter(|entry| entry.tokens.total() > 0)
        .map(|entry| entry.day)
        .collect();
    (data.streak, data.longest_streak) = streaks(&every_day, today);

    let span = if period == OverviewPeriod::Week {
        7
    } else {
        30
    };
    let recent_from = today - Duration::days(span - 1);
    let before_from = recent_from - Duration::days(span);
    for entry in &entries {
        if entry.day >= recent_from && entry.day <= today {
            data.recent += entry.tokens.total();
        } else if entry.day >= before_from && entry.day < recent_from {
            data.before += entry.tokens.total();
        }
    }

    data.days = year(&entries, today);
    data.weeks = weeks(&entries, today);
    data.months = months
        .into_iter()
        .map(|(month, by_cli)| OverviewMonth {
            month,
            by_cli: by_cli
                .into_iter()
                .map(|(agent, tokens)| (agent.to_owned(), tokens))
                .collect(),
        })
        .collect();
    data.longest_session_secs = longest_session(&counted);
    data.argonauts = through_argonauts(sources.work);
    data
}

/// The records of the CLIs the import read, and the ledger's lines for every other CLI.
fn entries<'a>(sources: &'a Sources<'_>) -> Vec<Entry<'a>> {
    let imported: BTreeSet<&str> = sources.records.iter().map(|r| r.cli.as_str()).collect();
    let mut entries: Vec<Entry<'a>> = sources
        .records
        .iter()
        .map(|record| {
            let local = sources.clock.to_local(record.at);
            Entry {
                day: local.date(),
                hour: Some(local.hour()),
                at: Some(record.at),
                agent: &record.cli,
                model: Some(record.model.as_str()).filter(|model| !model.is_empty()),
                folder: &record.folder,
                tokens: Tokens {
                    input: record.input,
                    output: record.output,
                    cache_read: record.cache_read,
                    cache_write: record.cache_write,
                },
                session: format!("{}:{}", record.cli, record.session),
                subagent: record.subagent,
            }
        })
        .collect();
    for line in sources.lines {
        if imported.contains(line.agent.as_str()) {
            continue;
        }
        // A chat's turns on one day count as one session; a run's agent as one of its own.
        let session = match &line.run {
            Some(run) => format!("{run}:{}:{:?}", line.agent, line.model),
            None => format!("chat:{}:{}", line.day, line.agent),
        };
        entries.push(Entry {
            day: line.day,
            hour: line.hour,
            at: None,
            agent: &line.agent,
            model: line.model.as_deref(),
            folder: &line.repo,
            tokens: Tokens {
                input: line.input.saturating_sub(line.cached),
                output: line.output,
                cache_read: line.cached,
                cache_write: 0,
            },
            session,
            subagent: false,
        });
    }
    entries
}

/// Each CLI with work: since when, whether its history came from the import, and its
/// sessions in the period, the most tokens first.
fn sources_of(
    entries: &[Entry<'_>],
    counted: &[&Entry<'_>],
    imported: &BTreeSet<&str>,
    data: &OverviewData,
) -> Vec<CliSource> {
    let mut since: BTreeMap<&str, Date> = BTreeMap::new();
    for entry in entries {
        let day = since.entry(entry.agent).or_insert(entry.day);
        *day = (*day).min(entry.day);
    }
    let mut sessions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for entry in counted {
        sessions
            .entry(entry.agent)
            .or_default()
            .insert(&entry.session);
    }
    let order = data.by_cli();
    let mut out: Vec<CliSource> = since
        .into_iter()
        .map(|(agent, day)| CliSource {
            agent: agent.to_owned(),
            since: Some(day),
            imported: imported.contains(agent),
            sessions: sessions.get(agent).map_or(0, |set| set.len() as u32),
        })
        .collect();
    out.sort_by_key(|source| {
        order
            .iter()
            .position(|(agent, _)| *agent == source.agent)
            .unwrap_or(usize::MAX)
    });
    out
}

/// `Claude Code since Oct 2025, 812 sessions; Codex since Feb, 221 sessions`: what the import
/// found, every day counted.
fn found(
    entries: &[Entry<'_>],
    imported: &BTreeSet<&str>,
    today: Date,
    cli_name: &dyn Fn(&str) -> String,
) -> Option<String> {
    let mut parts = Vec::new();
    for cli in imported {
        let mine: Vec<&Entry<'_>> = entries.iter().filter(|e| e.agent == *cli).collect();
        let first = mine.iter().map(|entry| entry.day).min()?;
        let sessions: BTreeSet<&str> = mine.iter().map(|entry| entry.session.as_str()).collect();
        let since = if first.year() == today.year() {
            month_word(first)
        } else {
            format!("{} {}", month_word(first), first.year())
        };
        let count = match sessions.len() {
            1 => "1 session".to_owned(),
            n => format!("{n} sessions"),
        };
        parts.push(format!("{} since {since}, {count}", cli_name(cli)));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn month_word(day: Date) -> String {
    let name = day.month().to_string();
    name[..3].to_owned()
}

/// What `tokens` of `model` cost at list prices. Claude's cache writes cost a quarter more
/// than its input, as Anthropic lists them.
fn api_value(agent: &str, model: &str, tokens: &Tokens) -> Option<f64> {
    let price = builtin_price(model)?;
    let cached = price.cached_input_per_mtok.unwrap_or(price.input_per_mtok);
    let write = if agent == "claude-code" { 1.25 } else { 1.0 };
    let cost = tokens.input as f64 * price.input_per_mtok
        + tokens.cache_write as f64 * price.input_per_mtok * write
        + tokens.cache_read as f64 * cached
        + tokens.output as f64 * price.output_per_mtok;
    Some(cost / 1e6)
}

/// Days in a row with work up to today, or up to yesterday when today has none yet, and the
/// longest such run.
fn streaks(days: &BTreeSet<Date>, today: Date) -> (u32, u32) {
    let mut longest = 0;
    let mut run = 0;
    let mut last: Option<Date> = None;
    for day in days {
        run = match last {
            Some(last) if *day - last == Duration::days(1) => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        last = Some(*day);
    }
    let mut current = 0;
    let mut day = if days.contains(&today) {
        today
    } else {
        today - Duration::days(1)
    };
    while days.contains(&day) {
        current += 1;
        day -= Duration::days(1);
    }
    (current, longest)
}

/// The last 53 weeks' days with work, each with the CLI that ran most and its main model.
fn year(entries: &[Entry<'_>], today: Date) -> Vec<OverviewDay> {
    let first = today - Duration::weeks(YEAR_WEEKS);
    let mut days: BTreeMap<Date, BTreeMap<(&str, Option<&str>), u64>> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.day > first && e.day <= today) {
        *days
            .entry(entry.day)
            .or_default()
            .entry((entry.agent, entry.model))
            .or_default() += entry.tokens.total();
    }
    days.into_iter()
        .filter_map(|(day, by_model)| {
            let mut by_cli: BTreeMap<&str, u64> = BTreeMap::new();
            for ((agent, _), tokens) in &by_model {
                *by_cli.entry(agent).or_default() += tokens;
            }
            let (agent, _) = by_cli.iter().max_by_key(|(_, tokens)| **tokens)?;
            let model = by_model
                .iter()
                .filter(|((cli, _), _)| cli == agent)
                .max_by_key(|(_, tokens)| **tokens)
                .and_then(|((_, model), _)| *model);
            let tokens: u64 = by_cli.values().sum();
            (tokens > 0).then(|| OverviewDay {
                day,
                tokens,
                agent: (*agent).to_owned(),
                model: model.map(str::to_owned),
            })
        })
        .collect()
}

/// The last 26 weeks, Monday first, each with its tokens by CLI.
fn weeks(entries: &[Entry<'_>], today: Date) -> Vec<OverviewWeek> {
    let this_monday = today - Duration::days(i64::from(today.weekday().number_days_from_monday()));
    let first = this_monday - Duration::weeks(BAR_WEEKS - 1);
    let mut weeks: Vec<OverviewWeek> = (0..BAR_WEEKS)
        .map(|k| OverviewWeek {
            start: first + Duration::weeks(k),
            by_cli: Vec::new(),
        })
        .collect();
    for entry in entries.iter().filter(|e| e.day >= first && e.day <= today) {
        let last = weeks.len() - 1;
        let at = ((entry.day - first).whole_days() / 7) as usize;
        let week = &mut weeks[at.min(last)];
        match week
            .by_cli
            .iter_mut()
            .find(|(agent, _)| agent == entry.agent)
        {
            Some((_, tokens)) => *tokens += entry.tokens.total(),
            None => week
                .by_cli
                .push((entry.agent.to_owned(), entry.tokens.total())),
        }
    }
    weeks
}

/// The longest session, first answer to last, counting its last hour whole.
fn longest_session(entries: &[&Entry<'_>]) -> u64 {
    let mut spans: BTreeMap<&str, (OffsetDateTime, OffsetDateTime)> = BTreeMap::new();
    for entry in entries {
        let Some(at) = entry.at else { continue };
        let span = spans.entry(&entry.session).or_insert((at, at));
        span.0 = span.0.min(at);
        span.1 = span.1.max(at);
    }
    spans
        .values()
        .map(|(first, last)| (*last - *first).whole_seconds().max(0) as u64 + 3600)
        .max()
        .unwrap_or(0)
}

/// What ran through Argonauts since it was installed, from the work ledger.
fn through_argonauts(work: &[WorkLine]) -> Option<ThroughArgonauts> {
    let mut through = ThroughArgonauts {
        since: work.iter().map(|line| line.day).min(),
        ..ThroughArgonauts::default()
    };
    let mut by_workflow: BTreeMap<String, u32> = BTreeMap::new();
    let (mut reviewed, mut first_time) = (0u32, 0u32);
    for line in work {
        match &line.what {
            Work::Run {
                workflow, answered, ..
            } => {
                through.workflows += 1;
                through.answered += answered;
                if let Some(workflow) = workflow {
                    *by_workflow.entry(workflow.clone()).or_default() += 1;
                }
            }
            Work::Task {
                merged,
                reviewed: was_reviewed,
                sent_back,
                ..
            } => {
                through.merged += u32::from(*merged);
                if *was_reviewed {
                    reviewed += 1;
                    first_time += u32::from(*sent_back == 0);
                }
            }
            Work::Session {
                stood_in_for,
                window,
                ..
            } if stood_in_for.is_some() => {
                through.fallbacks += 1;
                through.for_limit += u32::from(window.is_some());
            }
            _ => {}
        }
    }
    let mut by_workflow: Vec<(String, u32)> = by_workflow.into_iter().collect();
    by_workflow.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    through.by_workflow = by_workflow;
    through.first_pass = (reviewed > 0).then(|| f64::from(first_time) / f64::from(reviewed));
    (!work.is_empty()).then_some(through)
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;
