//! How full something is, in three steps that color badges, meters, and numbers alike.

use crate::theme::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Level {
    #[default]
    Ok,
    Warn,
    Danger,
}

impl Level {
    /// A badge's fill: green, amber, red.
    pub fn badge_role(self) -> Role {
        match self {
            Level::Ok => Role::Success,
            Level::Warn => Role::Warning,
            Level::Danger => Role::Error,
        }
    }

    /// A meter's fill: lavender, amber, red.
    pub fn fill_role(self) -> Role {
        match self {
            Level::Ok => Role::BarFill,
            Level::Warn => Role::Warning,
            Level::Danger => Role::Error,
        }
    }

    /// A number in a row: dim until it means something.
    pub fn number_role(self) -> Role {
        match self {
            Level::Ok => Role::Dim,
            Level::Warn => Role::Warning,
            Level::Danger => Role::Error,
        }
    }

    /// A number beside its own meter, bold either way.
    pub fn strong_role(self) -> Role {
        match self {
            Level::Ok => Role::Bright,
            other => other.number_role(),
        }
    }

    /// What a screen reader or a screen without color hears in place of the color.
    pub fn word(self) -> Option<&'static str> {
        match self {
            Level::Ok => None,
            Level::Warn => Some("warning"),
            Level::Danger => Some("danger"),
        }
    }

    /// The name `ui.levels.notify` uses.
    pub fn name(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Danger => "danger",
        }
    }
}

/// A widget's warn and danger lines, in percent of what it's measured against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Levels {
    pub warn: f64,
    pub danger: f64,
}

impl Levels {
    /// `ui.levels`, for every widget without lines of its own.
    pub const GENERAL: Levels = Levels::new(70.0, 90.0);
    /// Against the window, the mode line's rule.
    pub const QUOTA: Levels = Levels::new(90.0, 100.0);
    /// Against the tick, where the session compacts or rotates.
    pub const CONTEXT: Levels = Levels::new(80.0, 95.0);
    /// Against each limit, so a meter turns amber where Argonauts starts asking.
    pub const BUDGET: Levels = Levels::new(80.0, 100.0);
    /// Against `narrator.spend_cap`.
    pub const NARRATOR: Levels = Levels::new(70.0, 90.0);

    pub const fn new(warn: f64, danger: f64) -> Self {
        Self { warn, danger }
    }

    pub fn level(&self, percent: f64) -> Level {
        if percent >= self.danger {
            Level::Danger
        } else if percent >= self.warn {
            Level::Warn
        } else {
            Level::Ok
        }
    }

    /// The level a value holds once it's there: it stays until the value falls `slack`
    /// points under the line.
    fn held(&self, percent: f64, slack: f64) -> Level {
        if percent > self.danger - slack {
            Level::Danger
        } else if percent > self.warn - slack {
            Level::Warn
        } else {
            Level::Ok
        }
    }
}

/// A level that rises as soon as a value crosses a line, and drops back only when the value
/// falls `hysteresis` points under it, so a value sitting on a line doesn't flicker.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelWatch {
    levels: Levels,
    hysteresis: f64,
    notify: Vec<Level>,
    level: Level,
}

impl LevelWatch {
    /// `ui.levels.hysteresis`.
    pub const HYSTERESIS: f64 = 2.0;

    /// Watches against `levels`, notifying on crossings into danger, `ui.levels.notify`'s
    /// default.
    pub fn new(levels: Levels) -> Self {
        Self {
            levels,
            hysteresis: Self::HYSTERESIS,
            notify: vec![Level::Danger],
            level: Level::Ok,
        }
    }

    pub fn with_hysteresis(mut self, points: f64) -> Self {
        self.hysteresis = points.max(0.0);
        self
    }

    /// The levels whose crossing rings the bell or sends a notification.
    pub fn with_notify(mut self, levels: &[Level]) -> Self {
        self.notify = levels.to_vec();
        self
    }

    pub fn level(&self) -> Level {
        self.level
    }

    /// Takes a new reading. Returns the level it just rose into when that level notifies,
    /// once per crossing.
    pub fn update(&mut self, percent: f64) -> Option<Level> {
        let now = self.levels.level(percent);
        let held = self.levels.held(percent, self.hysteresis);
        let next = if now >= self.level {
            now
        } else {
            held.min(self.level).max(now)
        };
        let rose = next > self.level;
        self.level = next;
        (rose && self.notify.contains(&next)).then_some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_on_a_line_stays_until_it_falls_two_points_under() {
        let mut watch = LevelWatch::new(Levels::QUOTA);
        let mut seen = Vec::new();
        for value in [89.0, 90.0, 89.0, 88.0] {
            watch.update(value);
            seen.push(watch.level());
        }
        assert_eq!(seen, [Level::Ok, Level::Warn, Level::Warn, Level::Ok]);
    }

    #[test]
    fn a_fall_through_two_lines_lands_where_the_value_is() {
        let mut watch = LevelWatch::new(Levels::GENERAL);
        watch.update(95.0);
        assert_eq!(watch.level(), Level::Danger);
        watch.update(89.0);
        assert_eq!(watch.level(), Level::Danger);
        watch.update(80.0);
        assert_eq!(watch.level(), Level::Warn);
        watch.update(10.0);
        assert_eq!(watch.level(), Level::Ok);
    }

    #[test]
    fn notify_fires_once_per_crossing() {
        let mut watch = LevelWatch::new(Levels::CONTEXT);
        let fired: Vec<Option<Level>> = [50.0, 85.0, 96.0, 97.0, 99.0, 94.0, 96.0, 60.0, 96.0]
            .into_iter()
            .map(|value| watch.update(value))
            .collect();
        let danger = Some(Level::Danger);
        assert_eq!(
            fired,
            [None, None, danger, None, None, None, None, None, danger]
        );
        let mut both = LevelWatch::new(Levels::CONTEXT).with_notify(&[Level::Warn, Level::Danger]);
        assert_eq!(both.update(85.0), Some(Level::Warn));
        assert_eq!(both.update(85.0), None);
    }

    #[test]
    fn every_widget_has_its_lines() {
        assert_eq!(Levels::GENERAL.level(69.9), Level::Ok);
        assert_eq!(Levels::GENERAL.level(70.0), Level::Warn);
        assert_eq!(Levels::QUOTA.level(92.0), Level::Warn);
        assert_eq!(Levels::QUOTA.level(100.0), Level::Danger);
        assert_eq!(Levels::CONTEXT.level(95.0), Level::Danger);
        assert_eq!(Levels::BUDGET.level(85.0), Level::Warn);
        assert_eq!(Level::Warn.badge_role(), Role::Warning);
        assert_eq!(Level::Ok.fill_role(), Role::BarFill);
        assert_eq!(Level::Ok.number_role(), Role::Dim);
    }
}
