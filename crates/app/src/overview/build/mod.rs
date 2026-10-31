//! The Overview's numbers, summed from what's recorded: the import's records for each CLI it
//! read, and the usage ledger for every other CLI (whose work is what ran through Argonauts Code),
//! with the work ledger for what Argonauts Code itself did. A CLI the import read is counted from
//! its own files only, since those already hold the sessions Argonauts Code ran on it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::app::overview::{
    CliSource, ImportStatus, ModelDay, ModelTokens, OverviewData, OverviewDay, OverviewModel,
    OverviewMonth, OverviewPeriod, OverviewWeek, ThroughArgonauts, Tokens,
};
use argonauts_core::look::display_names::{DisplayNames, Spellings};
use argonauts_core::models::meter::WhoPaid;
use argonauts_core::models::pricing::PriceList;
use argonauts_core::runs::events::session::Billing;
use argonauts_core::settings::overview::YearColors;
use argonauts_tui::base::format::WallClock;
use time::{Date, Duration, OffsetDateTime};

use super::records::Record;
use crate::usage::usage_ledger::LedgerLine;
use crate::usage::work_ledger::{Work, WorkLine};

/// Weeks the year of days covers.
const YEAR_WEEKS: i64 = 53;
/// Weeks the weekly bars cover.
const BAR_WEEKS: i64 = 26;
/// Models the Models chart draws a line for, as Claude Code's /stats does.
const CHART_MODELS: usize = 3;

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
    /// Who paid and what it cost, where a ledger line's session kept who paid.
    paid: Option<Paid>,
}

/// What a ledger line says its tokens cost and who paid for them.
#[derive(Debug, Clone, Copy)]
struct Paid {
    billing: Billing,
    cost: f64,
    estimated: bool,
    partial: bool,
}

/// What the numbers are summed from.
pub(crate) struct Sources<'a> {
    pub records: &'a [Record],
    pub lines: &'a [LedgerLine],
    pub work: &'a [WorkLine],
    pub clock: WallClock,
    /// Every model's list price, for its API value.
    pub prices: &'a PriceList,
    /// The names that say which spellings are one model.
    pub names: &'a DisplayNames,
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
    let spelled = spellings(sources);
    let entries = entries(sources, &spelled);
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
            api_value: Some(sources.prices.price(agent, model).value(&tokens)),
            tokens,
            sessions: sessions_of.get(&(agent, model)).copied().unwrap_or(0),
        })
        .collect();
    data.models
        .sort_by_key(|model| Reverse(model.tokens.total()));
    data.api_value = data.models.iter().filter_map(|model| model.api_value).sum();
    data.api_partial = data.models.iter().any(|model| model.api_value.is_none());
    (data.peak_day, data.model_days) = days_by_model(&counted, &data.models);

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
    data.all_time_models = ranked_models(sources.records, sources.lines, &sources.clock, today);
    data
}

/// Every model in the order its colours rank, the one list every tab is given. It is settled
/// once a day from what was recorded before `today`, so a colour never changes while you look:
/// first the models that ran through Argonauts Code, the most tokens first; then the ones only
/// the import has seen, by their count on this computer, which also breaks the first ones'
/// ties; then the models new today, until tomorrow. Each carries the tokens that ran through
/// Argonauts Code before today, which a screen adds up for a model recorded under two names.
pub(crate) fn ranked_models(
    records: &[Record],
    lines: &[LedgerLine],
    clock: &WallClock,
    today: Date,
) -> Vec<ModelTokens> {
    let imported: BTreeSet<&str> = records.iter().map(|r| r.cli.as_str()).collect();
    let mut counts: BTreeMap<ModelKey<'_>, Ranking> = BTreeMap::new();
    for record in records {
        let model = Some(record.model.as_str()).filter(|model| !model.is_empty());
        let count = counts.entry((&record.cli, model)).or_default();
        if clock.to_local(record.at).date() < today {
            count.met_before_today = true;
            count.on_this_computer +=
                record.input + record.output + record.cache_read + record.cache_write;
        }
    }
    for line in lines {
        let count = counts
            .entry((&line.agent, line.model.as_deref()))
            .or_default();
        if line.day < today {
            count.met_before_today = true;
            count.through_argonauts += line.input + line.output;
            // A line of a CLI the import read is in its records already.
            if !imported.contains(line.agent.as_str()) {
                count.on_this_computer += line.input + line.output;
            }
        }
    }
    let mut ranked: Vec<(ModelKey<'_>, Ranking)> = counts.into_iter().collect();
    ranked.sort_by_key(|(_, count)| {
        (
            Reverse(count.through_argonauts),
            !count.met_before_today,
            Reverse(count.on_this_computer),
        )
    });
    ranked
        .into_iter()
        .map(|((agent, model), count)| ModelTokens {
            agent: agent.to_owned(),
            model: model.map(str::to_owned),
            tokens: count.through_argonauts,
        })
        .collect()
}

/// What places a model among its CLI's models, as recorded before today.
#[derive(Debug, Default, Clone, Copy)]
struct Ranking {
    through_argonauts: u64,
    on_this_computer: u64,
    met_before_today: bool,
}

/// Each model as the records and the ledger spell it, to the spelling it's summed under, so
/// the alias `haiku`, `claude-haiku-5-5` and a dated id of it make one model.
fn spellings<'a>(sources: &'a Sources<'_>) -> BTreeMap<ModelKey<'a>, Option<String>> {
    let records = sources.records.iter().map(|record| {
        let model = Some(record.model.as_str()).filter(|model| !model.is_empty());
        (record.cli.as_str(), model)
    });
    let lines = (sources.lines.iter()).map(|line| (line.agent.as_str(), line.model.as_deref()));
    let met: BTreeSet<ModelKey<'a>> = records.chain(lines).collect();
    let mut spellings = Spellings::new(sources.names);
    for (agent, model) in &met {
        spellings.meet(agent, *model);
    }
    met.into_iter()
        .map(|(agent, model)| ((agent, model), spellings.of(agent, model)))
        .collect()
}

/// A record's tokens as the Overview sums them.
pub(crate) fn record_tokens(record: &Record) -> Tokens {
    Tokens {
        input: record.input,
        output: record.output,
        cache_read: record.cache_read,
        cache_write: record.cache_write,
    }
}

/// A ledger line's tokens as the Overview sums them: its input counts the cached part too.
pub(crate) fn line_tokens(line: &LedgerLine) -> Tokens {
    Tokens {
        input: line.input.saturating_sub(line.cached),
        output: line.output,
        cache_read: line.cached,
        cache_write: 0,
    }
}

/// The records of the CLIs the import read, and the ledger's lines for every other CLI, each
/// model under the spelling `spelled` gives it.
fn entries<'a>(
    sources: &'a Sources<'_>,
    spelled: &'a BTreeMap<ModelKey<'a>, Option<String>>,
) -> Vec<Entry<'a>> {
    let spelled = |agent: &'a str, model: Option<&'a str>| -> Option<&'a str> {
        spelled
            .get(&(agent, model))
            .map_or(model, |one| one.as_deref())
    };
    let imported: BTreeSet<&str> = sources.records.iter().map(|r| r.cli.as_str()).collect();
    let mut entries: Vec<Entry<'a>> = sources
        .records
        .iter()
        .map(|record| {
            let local = sources.clock.to_local(record.at);
            Entry {
                day: local.date(),
                hour: (!record.whole_day).then(|| local.hour()),
                at: (!record.whole_day).then_some(record.at),
                agent: &record.cli,
                model: spelled(
                    &record.cli,
                    Some(record.model.as_str()).filter(|model| !model.is_empty()),
                ),
                folder: &record.folder,
                tokens: record_tokens(record),
                session: format!("{}:{}", record.cli, record.session),
                subagent: record.subagent,
                paid: None,
            }
        })
        .collect();
    for line in sources.lines {
        if imported.contains(line.agent.as_str()) {
            continue;
        }
        // A conversation's turns on one day count as one session; a run's agent as one of its own.
        let session = match &line.run {
            Some(run) => format!("{run}:{}:{:?}", line.agent, line.model),
            None => format!("conversation:{}:{}", line.day, line.agent),
        };
        entries.push(Entry {
            day: line.day,
            hour: line.hour,
            at: None,
            agent: &line.agent,
            model: spelled(&line.agent, line.model.as_deref()),
            folder: &line.repo,
            tokens: line_tokens(line),
            session,
            subagent: false,
            paid: line.billing.map(|billing| Paid {
                billing,
                cost: line.cost,
                estimated: line.estimated,
                partial: line.partial,
            }),
        });
    }
    entries
}

/// The period's tokens by who paid for them. A line whose session kept who paid counts under
/// that, with what the line says it cost. Tokens nothing kept a payer for count under `pays`,
/// who your settings say pays for the model now, and what the paid ones cost is their worth
/// at list prices.
pub(crate) fn who_paid(
    sources: &Sources<'_>,
    period: OverviewPeriod,
    today: Date,
    pays: &dyn Fn(&str, Option<&str>) -> Billing,
) -> WhoPaid {
    let spelled = spellings(sources);
    let entries = entries(sources, &spelled);
    let first = period.first_day(today);
    let mut paid = WhoPaid::default();
    let mut unrecorded: BTreeMap<ModelKey<'_>, Tokens> = BTreeMap::new();
    for entry in entries
        .iter()
        .filter(|entry| first.is_none_or(|first| entry.day >= first))
    {
        match entry.paid {
            // By output tokens, which every CLI counts the same way.
            Some(kept) => {
                let tokens = entry.tokens.output;
                paid.count(kept.billing, tokens, Some(kept.cost), kept.estimated);
                paid.cost_partial |= kept.partial && kept.billing == Billing::Paid && tokens > 0;
            }
            None => unrecorded
                .entry((entry.agent, entry.model))
                .or_default()
                .add(&entry.tokens),
        }
    }
    for ((agent, model), tokens) in unrecorded {
        let worth = sources.prices.price(agent, model).value(&tokens);
        paid.count(pays(agent, model), tokens.output, Some(worth), true);
    }
    paid
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

/// The period's busiest day, and each day with work with the tokens of the top models, which
/// `models` lists first.
fn days_by_model(
    entries: &[&Entry<'_>],
    models: &[OverviewModel],
) -> (Option<Date>, Vec<ModelDay>) {
    let top = &models[..models.len().min(CHART_MODELS)];
    let mut days: BTreeMap<Date, (u64, Vec<u64>)> = BTreeMap::new();
    for entry in entries {
        let tokens = entry.tokens.total();
        if tokens == 0 {
            continue;
        }
        let (total, by_model) = days
            .entry(entry.day)
            .or_insert_with(|| (0, vec![0; top.len()]));
        *total += tokens;
        let slot = top
            .iter()
            .position(|model| model.agent == entry.agent && model.model.as_deref() == entry.model);
        if let Some(slot) = slot {
            by_model[slot] += tokens;
        }
    }
    let mut peak: Option<(Date, u64)> = None;
    for (day, (total, _)) in &days {
        if peak.is_none_or(|(_, most)| *total > most) {
            peak = Some((*day, *total));
        }
    }
    let chart = days
        .into_iter()
        .map(|(day, (_, tokens))| ModelDay { day, tokens })
        .collect();
    (peak.map(|(day, _)| day), chart)
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

/// What ran through Argonauts Code over the work ledger's `work`.
pub(crate) fn through_argonauts(work: &[WorkLine]) -> Option<ThroughArgonauts> {
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
                fallback_for,
                window,
                ..
            } if fallback_for.is_some() => {
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
mod tests;
