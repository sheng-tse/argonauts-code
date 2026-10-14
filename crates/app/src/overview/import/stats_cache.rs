//! Claude Code's own totals: `stats-cache.json` in its folder, which its /stats keeps and which
//! outlives the transcripts Claude Code deletes after a while. It holds each day's tokens by
//! model (`dailyModelTokens`, the four kinds summed, days in UTC), each day's sessions
//! (`dailyActivity`), each model's tokens split into the four kinds (`modelUsage`), and the
//! last day it has counted (`lastComputedDate`).
//!
//! A day whose transcripts are gone is taken from here. Its tokens are split into input,
//! output, cache read and cache write the way that model's tokens split overall, and shared
//! evenly by the day's sessions; it has no hour and no folder.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use time::macros::{format_description, time};
use time::{Date, OffsetDateTime};

use super::{count, number};
use crate::overview::records::Record;

/// The model Claude Code names for lines it wrote itself rather than a model's answer.
const NOT_A_MODEL: &str = "<synthetic>";

#[derive(Deserialize)]
struct File {
    #[serde(rename = "lastComputedDate")]
    last: Option<String>,
    #[serde(rename = "dailyModelTokens", default)]
    tokens: Vec<DayTokens>,
    #[serde(rename = "dailyActivity", default)]
    activity: Vec<DayActivity>,
    #[serde(rename = "modelUsage", default)]
    models: BTreeMap<String, ModelUsage>,
}

#[derive(Deserialize)]
struct DayTokens {
    date: Option<String>,
    #[serde(rename = "tokensByModel", default)]
    by_model: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct DayActivity {
    date: Option<String>,
    #[serde(rename = "sessionCount", default, deserialize_with = "count")]
    sessions: u64,
}

#[derive(Deserialize)]
struct ModelUsage {
    #[serde(rename = "inputTokens", default, deserialize_with = "count")]
    input: u64,
    #[serde(rename = "outputTokens", default, deserialize_with = "count")]
    output: u64,
    #[serde(rename = "cacheReadInputTokens", default, deserialize_with = "count")]
    cache_read: u64,
    #[serde(
        rename = "cacheCreationInputTokens",
        default,
        deserialize_with = "count"
    )]
    cache_write: u64,
}

impl ModelUsage {
    fn counts(&self) -> [u64; 4] {
        [self.input, self.output, self.cache_read, self.cache_write]
    }
}

/// What the file says, in the import's terms.
#[derive(Debug, Default, PartialEq)]
pub(super) struct StatsCache {
    /// Each day it counted, oldest first, with its tokens by model and its sessions.
    pub days: Vec<Day>,
    /// Each model's tokens in the four kinds: input, output, cache read, cache write.
    pub models: BTreeMap<String, [u64; 4]>,
    /// The last day it has counted.
    pub through: Option<Date>,
}

impl StatsCache {
    /// Every token it holds, as Claude Code's /stats adds them up.
    pub fn total(&self) -> u64 {
        self.models.values().flatten().sum()
    }

    /// The tokens of the days from `first` on, or of every day, as its /stats adds them up.
    pub fn since(&self, first: Option<Date>) -> u64 {
        let Some(first) = first else {
            return self.total();
        };
        let days = self.days.iter().filter(|day| day.date >= first);
        days.flat_map(|day| day.tokens.values()).sum()
    }
}

#[derive(Debug, PartialEq)]
pub(super) struct Day {
    pub date: Date,
    pub tokens: BTreeMap<String, u64>,
    pub sessions: u32,
}

/// The file in Claude Code's folder `dir`: `Ok(None)` when it isn't there, an error when it
/// can't be read.
pub(super) fn read(dir: &Path) -> Result<Option<StatsCache>, String> {
    let path = dir.join("stats-cache.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let file: File = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    let mut sessions: BTreeMap<Date, u32> = BTreeMap::new();
    for day in &file.activity {
        if let Some(date) = day.date.as_deref().and_then(date) {
            *sessions.entry(date).or_default() += u32::try_from(day.sessions).unwrap_or(u32::MAX);
        }
    }
    let mut days: BTreeMap<Date, BTreeMap<String, u64>> = BTreeMap::new();
    for day in file.tokens {
        let Some(date) = day.date.as_deref().and_then(date) else {
            continue;
        };
        let by_model = days.entry(date).or_default();
        for (model, tokens) in day.by_model {
            let tokens = number(&tokens);
            if model != NOT_A_MODEL && tokens > 0 {
                *by_model.entry(model).or_default() += tokens;
            }
        }
    }
    Ok(Some(StatsCache {
        days: days
            .into_iter()
            .filter(|(_, tokens)| !tokens.is_empty())
            .map(|(date, tokens)| Day {
                sessions: sessions.get(&date).copied().unwrap_or(0),
                date,
                tokens,
            })
            .collect(),
        models: file
            .models
            .iter()
            .filter(|(model, _)| *model != NOT_A_MODEL)
            .map(|(model, usage)| (model.clone(), usage.counts()))
            .collect(),
        through: file.last.as_deref().and_then(date),
    }))
}

fn date(text: &str) -> Option<Date> {
    Date::parse(text, format_description!("[year]-[month]-[day]")).ok()
}

/// One day's records: for each model its tokens split the way its tokens split overall,
/// shared by the day's sessions, numbered from `first_session`.
pub(super) fn records(
    day: &Day,
    models: &BTreeMap<String, [u64; 4]>,
    first_session: u32,
) -> Vec<Record> {
    let at = OffsetDateTime::new_utc(day.date, time!(12:00));
    let sessions = day.sessions.max(1);
    let everyone = models.values().fold([0u64; 4], |sum, counts| {
        [0, 1, 2, 3].map(|k| sum[k] + counts[k])
    });
    let mut out = Vec::new();
    for (model, &tokens) in &day.tokens {
        let weights = models
            .get(model)
            .filter(|counts| counts.iter().any(|n| *n > 0))
            .copied()
            .unwrap_or(everyone);
        let kinds = split(tokens, weights);
        for n in 0..sessions {
            let share = kinds.map(|kind| shared(kind, sessions, n));
            if share.iter().all(|n| *n == 0) {
                continue;
            }
            out.push(Record {
                at,
                cli: "claude-code".to_owned(),
                model: model.clone(),
                folder: String::new(),
                input: share[0],
                output: share[1],
                cache_read: share[2],
                cache_write: share[3],
                session: first_session + n,
                subagent: false,
                whole_day: true,
            });
        }
    }
    out
}

/// `tokens` in four parts in proportion to `weights`, adding up to `tokens` exactly; all of it
/// input when nothing says how it splits.
fn split(tokens: u64, weights: [u64; 4]) -> [u64; 4] {
    let whole: u128 = weights.iter().map(|w| u128::from(*w)).sum();
    if whole == 0 {
        return [tokens, 0, 0, 0];
    }
    let mut parts = weights.map(|w| (u128::from(tokens) * u128::from(w) / whole) as u64);
    let left = tokens - parts.iter().sum::<u64>();
    let largest = (0..4).max_by_key(|k| weights[*k]).unwrap_or(0);
    parts[largest] += left;
    parts
}

/// Session `n`'s share of `amount` shared by `of`, the remainder going to the first.
fn shared(amount: u64, of: u32, n: u32) -> u64 {
    let each = amount / u64::from(of);
    if n == 0 {
        each + amount % u64::from(of)
    } else {
        each
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    #[test]
    fn a_day_splits_like_its_model_and_adds_up_exactly() {
        let parts = split(1_001, [10, 30, 900, 60]);
        assert_eq!(parts.iter().sum::<u64>(), 1_001);
        assert_eq!(parts, [10, 30, 901, 60]);
        assert_eq!(split(77, [0; 4]), [77, 0, 0, 0]);
        let day = Day {
            date: date!(2025 - 08 - 01),
            tokens: BTreeMap::from([("claude-opus-5-5".to_owned(), 1_001)]),
            sessions: 2,
        };
        let models = BTreeMap::from([("claude-opus-5-5".to_owned(), [10, 30, 900, 60])]);
        let records = records(&day, &models, 7);
        assert_eq!(records.len(), 2);
        assert_eq!(
            records.iter().map(|r| r.session).collect::<Vec<_>>(),
            [7, 8]
        );
        let total: u64 = records
            .iter()
            .map(|r| r.input + r.output + r.cache_read + r.cache_write)
            .sum();
        assert_eq!(total, 1_001);
        assert!(records.iter().all(|r| r.whole_day && r.folder.is_empty()));
    }
}
