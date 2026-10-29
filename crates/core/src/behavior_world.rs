//! What the Behaviors world draws besides the dots: facts the app works out from its own records,
//! for each behavior, for the crew as a whole and for each model, and the wonders that have
//! risen. The screen holds the world's rules and applies them to these facts. Day counts go back
//! from the end of the period, 0 being its last day, so a past period's world never changes.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Everything the world reads, sent with the map it is drawn under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldFacts {
    /// The end of the period, which every day count goes back from: now, for the current one.
    #[serde(with = "time::serde::rfc3339")]
    pub end: OffsetDateTime,
    /// One for each behavior used in the period or the one before it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub behaviors: Vec<BehaviorFacts>,
    /// How often agents went from one behavior straight to another in the same session: the
    /// rivers, currents and basins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moves: Vec<Move>,
    #[serde(default)]
    pub crew: CrewFacts,
    /// One for each model used in the period: the stars and their systems.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelFacts>,
    /// Models that worked in the same runs: the bridges between stars.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<ModelPair>,
    /// Runs in the last 24 hours with models of two makers or more: the comets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixed_runs: Vec<MixedRun>,
    /// The wonders risen so far. One stays risen when the run it rose from is deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wonders: Vec<RisenWonder>,
}

impl Default for WorldFacts {
    fn default() -> Self {
        Self {
            end: OffsetDateTime::UNIX_EPOCH,
            behaviors: Vec::new(),
            moves: Vec::new(),
            crew: CrewFacts::default(),
            models: Vec::new(),
            pairs: Vec::new(),
            mixed_runs: Vec::new(),
            wonders: Vec::new(),
        }
    }
}

impl WorldFacts {
    pub fn behavior(&self, behavior: &str) -> Option<&BehaviorFacts> {
        self.behaviors
            .iter()
            .find(|facts| facts.behavior == behavior)
    }
}

/// One behavior's facts. Its place and its height come from the map's dots.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BehaviorFacts {
    /// The behavior as [`MapAction::behavior`](crate::behavior_map::MapAction::behavior) names it.
    pub behavior: String,
    /// Its actions in the period.
    pub count: u32,
    /// Its actions in the period before this one: an atoll when it has none in this one.
    pub last_period: u32,
    /// Days back to its last action, read past the period's start, which sets its climate.
    /// `None` when it was never used.
    pub last_used: Option<u32>,
    /// How many of the last 7 days it was used on.
    pub days_in_last_7: u8,
    /// Its actions in each full week of the period, counted back from the end, latest first.
    pub weeks: Vec<u32>,
    /// Its actions that came 10 minutes or more after the one before in the same session.
    pub after_a_wait: u32,
    /// Sessions that ended on it.
    pub session_ends: u32,
    /// Sessions stopped on it: by you, a limit, or a full context.
    pub stopped: u32,
    /// Sessions that ended on it because its CLI failed, crashes included.
    pub cli_failed: u32,
    /// The ones of those whose CLI crashed, newest first.
    pub crashes: Vec<Crash>,
    /// Its actions that failed, and days back to the last one that did.
    pub failed: u32,
    pub last_failed: Option<u32>,
    /// Its actions in work a review sent back.
    pub sent_back: u32,
    /// Its actions in work that never merged, and days back to the last drop.
    pub dropped: u32,
    pub last_dropped: Option<u32>,
    /// The most of its actions in any one hour.
    pub busiest_hour: Option<Burst>,
    /// The most agents that did it within 5 minutes.
    pub most_agents_in_5_minutes: Option<Burst>,
    /// Times one agent did it 5 or more times in a row, and the longest such run.
    pub runs_in_a_row: u32,
    pub longest_run: u32,
}

/// The most of something within a stretch of time, and when that stretch began.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Burst {
    pub count: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// A session that ended because its CLI crashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crash {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The CLI or endpoint, and the model it ran.
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Agents went from `from` straight to `to` this many times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub from: String,
    pub to: String,
    pub count: u32,
}

/// What the crew did as a whole: the ice, the aurora and the sky.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CrewFacts {
    /// Days back to the crew's last action: the northern ice cap.
    pub idle_days: u32,
    /// Days in the period with no action at all: the southern ice.
    pub quiet_days: u32,
    /// Each stretch of 2 or more quiet days in a row: an iceberg each.
    pub quiet_stretches: Vec<QuietStretch>,
    /// Actions between 10:00 PM and 5:00 AM where you are: the aurora.
    pub night_actions: u32,
    /// The actions of each month before the period, the month just before it first: the sky's
    /// star clusters.
    pub earlier_months: Vec<MonthActions>,
    pub storm: StormWord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietStretch {
    /// Days back to its latest quiet day.
    pub ends: u32,
    pub days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthActions {
    /// Such as `2026-08`.
    pub month: String,
    pub actions: u32,
}

/// What a storm of 25 or more actions in an hour is called where you are, from the time zone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StormWord {
    /// The Atlantic and the east and central Pacific, and wherever the zone is unknown.
    #[default]
    Hurricane,
    /// The north-west Pacific.
    Typhoon,
    /// Round the Indian Ocean and in the South Pacific.
    Cyclone,
}

impl StormWord {
    pub fn word(self) -> &'static str {
        match self {
            Self::Hurricane => "hurricane",
            Self::Typhoon => "typhoon",
            Self::Cyclone => "cyclone",
        }
    }
}

/// One model's facts, for its star and its system.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelFacts {
    /// The model's id as [`MapGroup::model`](crate::behavior_map::MapGroup::model) names it.
    pub model: String,
    /// The CLI or endpoint that ran it.
    pub agent: String,
    /// Its maker as the registry names providers, such as `anthropic`: the galaxy it sits in.
    pub maker: String,
    pub actions: u32,
    pub failed: u32,
    /// Its actions in the last 7 days, 24 hours, hour and 10 minutes before the period's end.
    pub last_7_days: u32,
    pub last_24_hours: u32,
    pub last_hour: u32,
    pub last_10_minutes: u32,
    /// Days back to its first and its last action in the period.
    pub first_used: u32,
    pub last_used: u32,
    pub days_used: u32,
    pub sessions: u32,
    /// Its actions in work that merged, that a review sent back, and that never merged.
    pub merged: u32,
    pub sent_back: u32,
    pub dropped: u32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_action: Option<OffsetDateTime>,
    /// When a session of it ended because its CLI crashed or failed, newest first.
    pub cli_failures: Vec<CliFailure>,
    /// A newer model of its line, where one came.
    pub newer: Option<NewerModel>,
    /// Its behaviors, most used first: the planets, then the asteroid belt.
    pub behaviors: Vec<ModelBehavior>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliFailure {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewerModel {
    pub model: String,
    /// Days back to when it came.
    pub came: u32,
}

/// What one model did of one behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelBehavior {
    pub behavior: String,
    pub count: u32,
    pub failed: u32,
    /// Days back to its last action of it.
    pub last_used: u32,
    pub days_in_last_7: u8,
    /// Times an agent of this model did it 4 or more times in a row.
    pub runs_in_a_row: u32,
}

/// Two models that shared runs, written in the order the models list them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPair {
    pub models: [String; 2],
    pub runs: u32,
    /// How many times more often they shared a run than chance would give.
    pub lift: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MixedRun {
    pub run: String,
    /// When its first action was.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub actions: u32,
    /// Its makers in the order they first acted, and the busiest model of each in the same
    /// order.
    pub makers: Vec<String>,
    pub models: Vec<String>,
}

/// A wonder that has risen, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RisenWonder {
    pub wonder: Wonder,
    /// The behavior it rises beside.
    pub behavior: String,
    #[serde(with = "time::serde::rfc3339")]
    pub risen: OffsetDateTime,
    /// What its line says after its name, in your own records' words.
    pub why: String,
    /// The models the line names, so each gets its logo before its name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

/// The sixteen wonders. Each rises from one rare thing in your own records, and nothing lists
/// the ones still to come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wonder {
    Argo,
    Fleece,
    Symplegades,
    Talos,
    Sirens,
    Dragon,
    Harpies,
    Dodona,
    Pyramid,
    Gardens,
    Colossus,
    Pharos,
    Artemis,
    Zeus,
    Mausoleum,
    Atlantis,
}

impl Wonder {
    pub const ALL: [Self; 16] = [
        Self::Argo,
        Self::Fleece,
        Self::Symplegades,
        Self::Talos,
        Self::Sirens,
        Self::Dragon,
        Self::Harpies,
        Self::Dodona,
        Self::Pyramid,
        Self::Gardens,
        Self::Colossus,
        Self::Pharos,
        Self::Artemis,
        Self::Zeus,
        Self::Mausoleum,
        Self::Atlantis,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Argo => "The Argo",
            Self::Fleece => "The Golden Fleece",
            Self::Symplegades => "The Clashing Rocks",
            Self::Talos => "Talos",
            Self::Sirens => "The Sirens",
            Self::Dragon => "The Sleepless Dragon",
            Self::Harpies => "The Harpies",
            Self::Dodona => "The Oak of Dodona",
            Self::Pyramid => "The Great Pyramid",
            Self::Gardens => "The Hanging Gardens",
            Self::Colossus => "The Colossus of Rhodes",
            Self::Pharos => "The Lighthouse of Alexandria",
            Self::Artemis => "The Temple of Artemis",
            Self::Zeus => "The Statue of Zeus",
            Self::Mausoleum => "The Mausoleum",
            Self::Atlantis => "Atlantis",
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::macros::datetime;

    use super::*;

    #[test]
    fn facts_with_nothing_in_them_read_back() {
        let written = json!({ "end": "2026-09-30T19:30:00Z" });
        let facts: WorldFacts = serde_json::from_value(written).unwrap();
        assert_eq!(facts.end, datetime!(2026-09-30 19:30 UTC));
        assert!(facts.behaviors.is_empty() && facts.wonders.is_empty());
        assert_eq!(facts.crew.storm, StormWord::Hurricane);
        assert_eq!(
            serde_json::to_value(&facts).unwrap(),
            json!({ "end": "2026-09-30T19:30:00Z", "crew": {
                "idle_days": 0, "quiet_days": 0, "quiet_stretches": [], "night_actions": 0,
                "earlier_months": [], "storm": "hurricane" } })
        );
    }

    #[test]
    fn a_behavior_reads_with_the_facts_it_has() {
        let facts: BehaviorFacts = serde_json::from_value(json!({
            "behavior": "ran the test suite",
            "count": 31,
            "busiest_hour": { "count": 26, "at": "2026-09-29T14:00:00Z" },
        }))
        .unwrap();
        assert_eq!(facts.count, 31);
        assert_eq!(facts.last_used, None);
        assert_eq!(
            facts.busiest_hour,
            Some(Burst {
                count: 26,
                at: datetime!(2026-09-29 14:00 UTC)
            })
        );
        let world = WorldFacts {
            behaviors: vec![facts.clone()],
            ..WorldFacts::default()
        };
        assert_eq!(world.behavior("ran the test suite"), Some(&facts));
        assert_eq!(world.behavior("edited a file"), None);
    }

    #[test]
    fn a_model_and_a_wonder_read_back_as_written() {
        let model = ModelFacts {
            model: "opus".into(),
            agent: "claude-code".into(),
            maker: "anthropic".into(),
            actions: 120,
            last_action: Some(datetime!(2026-09-30 19:10 UTC)),
            cli_failures: vec![CliFailure {
                at: datetime!(2026-09-28 11:02 UTC),
            }],
            newer: Some(NewerModel {
                model: "opus-next".into(),
                came: 12,
            }),
            behaviors: vec![ModelBehavior {
                behavior: "edited a file".into(),
                count: 40,
                runs_in_a_row: 2,
                ..ModelBehavior::default()
            }],
            ..ModelFacts::default()
        };
        let wonder = RisenWonder {
            wonder: Wonder::Pharos,
            behavior: "built the project".into(),
            risen: datetime!(2026-09-21 09:00 UTC),
            why: "Work a local model made merged after review".into(),
            models: vec!["qwen".into()],
        };
        let facts = WorldFacts {
            end: datetime!(2026-09-30 19:30 UTC),
            models: vec![model],
            wonders: vec![wonder],
            ..WorldFacts::default()
        };
        let written = serde_json::to_string(&facts).unwrap();
        assert_eq!(serde_json::from_str::<WorldFacts>(&written).unwrap(), facts);
    }

    #[test]
    fn every_wonder_has_its_own_name() {
        let mut names: Vec<&str> = Wonder::ALL.iter().map(|wonder| wonder.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 16);
        assert_eq!(StormWord::Typhoon.word(), "typhoon");
    }
}
