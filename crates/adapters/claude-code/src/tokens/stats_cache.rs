//! Claude Code's own totals: `stats-cache.json` in its folder, which its /stats keeps and which
//! outlives the transcripts Claude Code deletes after a while. It holds each day's tokens by
//! model (`dailyModelTokens`, the four kinds summed, days in UTC), each day's sessions
//! (`dailyActivity`), each model's tokens split into the four kinds (`modelUsage`), and the
//! last day it has counted (`lastComputedDate`).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;
use time::Date;
use time::macros::format_description;

use super::NOT_A_MODEL;

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

/// What the file says.
#[derive(Debug, Default, PartialEq)]
pub struct StatsCache {
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
pub struct Day {
    pub date: Date,
    pub tokens: BTreeMap<String, u64>,
    pub sessions: u32,
}

/// The file in Claude Code's folder `dir`: `Ok(None)` when it isn't there, an error when it
/// can't be read.
pub fn read(dir: &Path) -> Result<Option<StatsCache>, String> {
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

/// A count as a whole number, zero when it isn't one.
fn number(value: &Value) -> u64 {
    match value {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|n| *n > 0.0).map(|n| n as u64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// A count that is missing, `null` or not a whole number is read as zero.
fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let value = Option::<Value>::deserialize(d)?;
    Ok(value.as_ref().map_or(0, number))
}
