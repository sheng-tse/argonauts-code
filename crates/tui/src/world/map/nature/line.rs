//! What each mark of nature says when the pointer finds it, in the app's words and your agents'
//! own numbers.

use time::OffsetDateTime;

use super::{Feature, Kind, Nature, NatureFacts, Shoal};
use crate::format::ClockTime;
use crate::world::map::fields::Ground;
use crate::world::map::land::days_ago;

impl Feature {
    /// Its line: what it is, then what in your agents' actions made it. `cli` words a CLI the
    /// line names by its id, as the line is shown: by its name, after its logo where logos show.
    pub fn line(
        &self,
        ground: &Ground,
        facts: &NatureFacts,
        cli: &mut dyn FnMut(&str) -> String,
    ) -> String {
        let quoted = |behavior: usize| format!("‘{}’", ground.facts.behaviors[behavior].name);
        let behavior = quoted(self.behavior);
        let count = ground.count[self.behavior];
        let end = facts.end;
        let last = |days: Option<u32>| {
            days.map_or(String::new(), |d| format!(", the last {}", days_ago(d)))
        };
        match &self.kind {
            Kind::Hurricane(storm) => {
                let word = capital(facts.storm.word());
                format!(
                    "{word}, category {} · {} {behavior} actions within one hour, {}",
                    storm.category,
                    storm.actions,
                    when(storm.at, end)
                )
            }
            Kind::Tsunami(wave) => format!(
                "Tsunami · {} agents did {behavior} within the same 5 minutes, {}",
                wave.agents,
                when(wave.at, end)
            ),
            Kind::Volcano {
                failed,
                smoking,
                last: failed_last,
            } => format!(
                "Volcano, {} · {behavior} · {failed} of its {count} actions failed{}",
                if *smoking { "smoking" } else { "quiet" },
                last(*failed_last)
            ),
            Kind::Flood { sent_back } => format!(
                "Flood · {sent_back} of its {count} {behavior} actions were in work a review sent \
                 back"
            ),
            Kind::Wildfire {
                dropped,
                burning,
                last: dropped_last,
            } => format!(
                "Wildfire, {} · {dropped} of its {count} {behavior} actions were in work that never \
                 merged{}",
                if *burning { "burning" } else { "burnt out" },
                last(*dropped_last)
            ),
            Kind::Tornado { times, longest } => format!(
                "Tornado · {} one agent did {behavior} 5 or more times in a row (the longest \
                 {longest})",
                plural(*times, "time")
            ),
            Kind::Thunderstorm { sessions } => format!(
                "Thunderstorm · {} stopped on {behavior}, by you, a limit or a full context",
                plural(*sessions, "session")
            ),
            Kind::Rift(rift) => match rift.recent.first() {
                Some(crash) => {
                    let more = match rift.recent.len() {
                        1 => String::new(),
                        n => format!(", and {} more in the last 24 hours", n - 1),
                    };
                    format!(
                        "Earthquake · {} crashed {}, ending a session on {behavior}{more} · {} ended \
                         here because the CLI crashed",
                        cli(&crash.agent),
                        hours_ago(crash.at, end),
                        plural(rift.sessions, "session")
                    )
                }
                None => format!(
                    "Fault line · {} ended on {behavior} because the CLI failed{}",
                    plural(rift.sessions, "session"),
                    last(rift.last.map(|at| days_back(at, end)))
                ),
            },
            Kind::Lake { sessions } => {
                format!(
                    "Lake · {} ended on {behavior}",
                    plural(*sessions, "session")
                )
            }
            Kind::Land(_) => self
                .land_mark()
                .map(|mark| mark.line(ground))
                .unwrap_or_default(),
            Kind::River(stream) | Kind::Current(stream) => format!(
                "{} · {} times an agent went from {behavior} to {}",
                if matches!(self.kind, Kind::River(_)) {
                    "River"
                } else {
                    "Ocean current"
                },
                stream.moves,
                quoted(stream.to)
            ),
            Kind::Atoll => format!("Atoll · {behavior} was used last period, not in this one"),
        }
    }
}

impl Nature {
    /// The line of what the pointer found by its place in nature's list: a feature, or past
    /// them, a shoal. `cli` words a CLI the line names, as [`Feature::line`] says.
    pub fn line(
        &self,
        index: usize,
        ground: &Ground,
        facts: &NatureFacts,
        cli: &mut dyn FnMut(&str) -> String,
    ) -> Option<String> {
        match self.features.get(index) {
            Some(feature) => Some(feature.line(ground, facts, cli)),
            None => self
                .shoals
                .get(index - self.features.len())
                .map(Shoal::line),
        }
    }
}

impl Shoal {
    pub fn line(&self) -> String {
        format!("Shoal · on the list, never used · ‘{}’", self.name)
    }
}

/// When something happened, as days back from the period's end and the time on its clock.
fn when(at: OffsetDateTime, end: OffsetDateTime) -> String {
    let local = at.to_offset(end.offset());
    format!(
        "{} at {}",
        days_ago(days_back(at, end)),
        ClockTime::from(local)
    )
}

/// Whole days from `at` back to `end`, by the calendar where `end` is.
pub(super) fn days_back(at: OffsetDateTime, end: OffsetDateTime) -> u32 {
    let at = at.to_offset(end.offset()).date();
    (end.date() - at).whole_days().max(0) as u32
}

/// How long before the period's end a crash came, to the nearest hour.
fn hours_ago(at: OffsetDateTime, end: OffsetDateTime) -> String {
    let hours = ((end - at).whole_seconds() as f64 / 3600.0 + 0.5).floor();
    if hours < 1.0 {
        "under an hour ago".to_owned()
    } else if hours == 1.0 {
        "an hour ago".to_owned()
    } else {
        format!("{hours} hours ago")
    }
}

fn plural(n: u32, one: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn capital(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
