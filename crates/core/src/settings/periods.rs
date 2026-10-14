//! The periods every tab counts over (`[periods]`): Today, Week, Month and All. A week is
//! the calendar week from the day `week_starts` names, and a month runs from its 1st.
//!
//! ```toml
//! [periods]
//! week_starts = "sunday"   # or monday, … saturday
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use time::Weekday;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PeriodSettings {
    /// The day a week starts on, for Week and for the days of the week the charts show.
    pub week_starts: WeekStart,
}

/// A day a week can start on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WeekStart {
    #[default]
    Sunday,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
}

impl WeekStart {
    pub fn weekday(self) -> Weekday {
        match self {
            Self::Sunday => Weekday::Sunday,
            Self::Monday => Weekday::Monday,
            Self::Tuesday => Weekday::Tuesday,
            Self::Wednesday => Weekday::Wednesday,
            Self::Thursday => Weekday::Thursday,
            Self::Friday => Weekday::Friday,
            Self::Saturday => Weekday::Saturday,
        }
    }

    pub fn is_sunday(&self) -> bool {
        *self == Self::Sunday
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_start_on_sunday_unless_the_file_says_otherwise() {
        assert_eq!(
            PeriodSettings::default().week_starts.weekday(),
            Weekday::Sunday
        );
        let read: PeriodSettings = toml::from_str("week_starts = \"monday\"").unwrap();
        assert_eq!(read.week_starts.weekday(), Weekday::Monday);
    }
}
