//! When each turn of a conversation ran, which its actions are dated by. A turn's start line
//! carries its time; older journals have one only where a turn ended with its context kept.
//! A turn with neither takes the time of the nearest turn that has one, and a journal with none
//! at all the time it was created. Nothing here reads when the journal last changed, so a
//! later write never moves an action to another day.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use time::OffsetDateTime;

/// The key a turn's start line carries its time under.
pub(crate) const STARTED_AT: &str = "at";

/// The fields of a journal line that say when a turn ran.
#[derive(Deserialize)]
struct Stamp {
    conversation: String,
    #[serde(default)]
    turn: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    at: Option<OffsetDateTime>,
}

/// Each turn's time, by its id.
#[derive(Debug, Clone, Default)]
pub(crate) struct TurnTimes {
    by_turn: HashMap<String, OffsetDateTime>,
    /// For a turn the journal never started.
    fallback: Option<OffsetDateTime>,
}

impl TurnTimes {
    /// The times of the turns in `text`, a journal's lines, with `born` for a journal that
    /// says none.
    pub fn of(text: &str, born: Option<OffsetDateTime>) -> TurnTimes {
        let mut order: Vec<String> = Vec::new();
        let mut started: HashMap<String, OffsetDateTime> = HashMap::new();
        let mut ended: HashMap<String, OffsetDateTime> = HashMap::new();
        for line in text.lines() {
            if !line.contains("\"turn_started\"") && !line.contains("\"context_left\"") {
                continue;
            }
            let Ok(stamp) = serde_json::from_str::<Stamp>(line) else {
                continue;
            };
            let Some(turn) = stamp.turn else { continue };
            match stamp.conversation.as_str() {
                "turn_started" => {
                    if !order.contains(&turn) {
                        order.push(turn.clone());
                    }
                    if let Some(at) = stamp.at {
                        started.insert(turn, at);
                    }
                }
                "context_left" => {
                    if let Some(at) = stamp.at {
                        ended.entry(turn).or_insert(at);
                    }
                }
                _ => {}
            }
        }
        let own: Vec<Option<OffsetDateTime>> = order
            .iter()
            .map(|turn| started.get(turn).or_else(|| ended.get(turn)).copied())
            .collect();
        let mut by_turn = HashMap::new();
        for (at, turn) in order.iter().enumerate() {
            let before = own[..=at].iter().rev().flatten().next();
            let after = || own[at..].iter().flatten().next();
            if let Some(time) = before.or_else(after).copied().or(born) {
                by_turn.insert(turn.clone(), time);
            }
        }
        let fallback = own.iter().flatten().next_back().copied().or(born);
        TurnTimes { by_turn, fallback }
    }

    /// Every turn at `at`, as a test gives them.
    #[cfg(test)]
    pub fn all_at(at: OffsetDateTime) -> TurnTimes {
        TurnTimes {
            by_turn: HashMap::new(),
            fallback: Some(at),
        }
    }

    /// The times of the turns in the journal at `path`, read whole.
    pub fn read(path: &Path, text: &str) -> TurnTimes {
        TurnTimes::of(text, born(path))
    }

    /// When `turn` ran, as well as the journal says.
    pub fn at(&self, turn: &str) -> OffsetDateTime {
        self.by_turn
            .get(turn)
            .copied()
            .or(self.fallback)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

/// When the journal was created, which later writes leave alone. A file system that keeps no
/// creation time gives its last change instead.
fn born(path: &Path) -> Option<OffsetDateTime> {
    let meta = fs::metadata(path).ok()?;
    let at = meta.created().or_else(|_| meta.modified()).ok()?;
    Some(OffsetDateTime::from(at))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    fn journal(lines: &[serde_json::Value]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    fn started(turn: &str, at: Option<&str>) -> serde_json::Value {
        let mut line = json!({"conversation": "turn_started", "turn": turn, "text": "hi",
            "worker": "codex", "read_only": true, "commit": "abc", "uncommitted": []});
        if let Some(at) = at {
            line[STARTED_AT] = json!(at);
        }
        line
    }

    fn left(turn: &str, at: &str) -> serde_json::Value {
        json!({"conversation": "context_left", "turn": turn, "at": at, "used": 10})
    }

    #[test]
    fn a_turn_is_dated_by_its_start_else_by_its_end() {
        let text = journal(&[
            started("t-1", Some("2026-10-18T09:00:00Z")),
            left("t-1", "2026-10-18T09:05:00Z"),
            started("t-2", None),
            left("t-2", "2026-10-19T10:00:00Z"),
        ]);
        let times = TurnTimes::of(&text, Some(datetime!(2026-10-01 0:00 UTC)));
        assert_eq!(times.at("t-1"), datetime!(2026-10-18 9:00 UTC));
        assert_eq!(times.at("t-2"), datetime!(2026-10-19 10:00 UTC));
    }

    #[test]
    fn a_turn_with_no_time_takes_its_nearest_neighbours() {
        let text = journal(&[
            started("t-1", None),
            started("t-2", None),
            left("t-2", "2026-10-18T09:05:00Z"),
            started("t-3", None),
        ]);
        let times = TurnTimes::of(&text, Some(datetime!(2026-10-01 0:00 UTC)));
        assert_eq!(times.at("t-1"), datetime!(2026-10-18 9:05 UTC));
        assert_eq!(times.at("t-3"), datetime!(2026-10-18 9:05 UTC));
    }

    #[test]
    fn a_journal_with_no_time_is_dated_by_its_creation() {
        let text = journal(&[started("t-1", None)]);
        let born = datetime!(2026-09-20 8:00 UTC);
        assert_eq!(TurnTimes::of(&text, Some(born)).at("t-1"), born);
        assert_eq!(TurnTimes::of(&text, Some(born)).at("t-9"), born);
    }

    #[test]
    fn touching_a_journal_moves_none_of_its_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation-001.jsonl");
        let text = journal(&[started("t-1", None)]);
        fs::write(&path, &text).unwrap();
        if fs::metadata(&path).unwrap().created().is_err() {
            return;
        }
        let before = TurnTimes::read(&path, &text).at("t-1");
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3 * 86_400);
        fs::File::options()
            .append(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(TurnTimes::read(&path, &text).at("t-1"), before);
    }
}
