//! Days taken from Claude Code's `stats-cache.json`, which outlives the transcripts Claude Code
//! deletes after a while. A day whose transcripts are gone is taken from there. Its tokens are
//! split into input, output, cache read and cache write the way that model's tokens split
//! overall, and shared evenly by the day's sessions; it has no hour and no folder.

use std::collections::BTreeMap;

pub(super) use argonauts_adapter_claude_code::tokens::stats_cache::{Day, StatsCache, read};
use time::OffsetDateTime;
use time::macros::time;

use crate::overview::records::Record;

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
