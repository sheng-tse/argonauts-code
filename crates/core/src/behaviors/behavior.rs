//! An action's behavior: one label, two for a shell command that did two kinds of thing, or
//! none when no rule fits.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::list::Label;

/// How far a pair sits from its first label toward its second, so a pair stays beside the
/// kind of thing it did first.
const PAIR_PULL: f32 = 0.3;

/// Serialized as its id: `git_status`, `git_status+git_history`, or `unlabelled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Behavior {
    One(Label),
    /// A shell command's first two kinds, in the order it ran them.
    Two(Label, Label),
    Unlabelled,
}

impl Behavior {
    /// The behavior of up to the first two distinct `labels`.
    pub fn of(labels: impl IntoIterator<Item = Label>) -> Self {
        let mut labels = labels.into_iter();
        let Some(first) = labels.next() else {
            return Self::Unlabelled;
        };
        match labels.find(|label| *label != first) {
            Some(second) => Self::Two(first, second),
            None => Self::One(first),
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::One(label) => label.id().to_owned(),
            Self::Two(first, second) => format!("{}+{}", first.id(), second.id()),
            Self::Unlabelled => UNLABELLED.to_owned(),
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        if id == UNLABELLED {
            return Some(Self::Unlabelled);
        }
        match id.split_once('+') {
            Some((first, second)) => {
                let (first, second) = (Label::from_id(first)?, Label::from_id(second)?);
                (first != second).then_some(Self::Two(first, second))
            }
            None => Label::from_id(id).map(Self::One),
        }
    }

    /// As the list and the examples say it: "checked git status or diff and read git history".
    pub fn words(&self) -> String {
        match self {
            Self::One(label) => label.words().to_owned(),
            Self::Two(first, second) => format!("{} and {}", first.words(), second.words()),
            Self::Unlabelled => "no label fits".to_owned(),
        }
    }

    /// Where it sits on the map, each in 0..1 from the top left. `None` for an unlabelled
    /// action, which has no place.
    pub fn place(&self) -> Option<(f32, f32)> {
        match self {
            Self::One(label) => Some(label.place()),
            Self::Two(first, second) => {
                let (a, b) = (first.place(), second.place());
                Some((a.0 + (b.0 - a.0) * PAIR_PULL, a.1 + (b.1 - a.1) * PAIR_PULL))
            }
            Self::Unlabelled => None,
        }
    }

    /// Its labels, one or two.
    pub fn labels(&self) -> Vec<Label> {
        match self {
            Self::One(label) => vec![*label],
            Self::Two(first, second) => vec![*first, *second],
            Self::Unlabelled => Vec::new(),
        }
    }

    pub fn has(&self, label: Label) -> bool {
        self.labels().contains(&label)
    }

    pub fn is_labelled(&self) -> bool {
        !matches!(self, Self::Unlabelled)
    }
}

const UNLABELLED: &str = "unlabelled";

impl fmt::Display for Behavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.words())
    }
}

impl Serialize for Behavior {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.id())
    }
}

impl<'de> Deserialize<'de> for Behavior {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::from_id(&id)
            .ok_or_else(|| serde::de::Error::custom(format!("no behavior is called {id}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pair_reads_and_keys_in_order() {
        let pair = Behavior::of([Label::GitStatus, Label::GitHistory]);
        assert_eq!(
            pair.words(),
            "checked git status or diff and read git history"
        );
        assert_eq!(pair.id(), "git_status+git_history");
        assert_eq!(Behavior::from_id(&pair.id()), Some(pair));
        let json = serde_json::to_string(&pair).unwrap();
        assert_eq!(json, "\"git_status+git_history\"");
        assert_eq!(serde_json::from_str::<Behavior>(&json).unwrap(), pair);
    }

    #[test]
    fn repeats_count_once_and_only_two_are_kept() {
        let one = Behavior::of([Label::GitHistory, Label::GitHistory]);
        assert_eq!(one, Behavior::One(Label::GitHistory));
        let two = Behavior::of([
            Label::GitStatus,
            Label::GitStatus,
            Label::RunTests,
            Label::Shell,
        ]);
        assert_eq!(two, Behavior::Two(Label::GitStatus, Label::RunTests));
        assert_eq!(Behavior::of([]), Behavior::Unlabelled);
    }

    #[test]
    fn a_pair_sits_beside_its_first_label() {
        let pair = Behavior::Two(Label::GitStatus, Label::GitHistory)
            .place()
            .unwrap();
        let (status, history) = (Label::GitStatus.place(), Label::GitHistory.place());
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).hypot(a.1 - b.1);
        assert!(near(pair, status) < near(pair, history));
        assert_eq!(Behavior::Unlabelled.place(), None);
    }

    #[test]
    fn unknown_ids_are_refused() {
        assert_eq!(Behavior::from_id("git_status+git_status"), None);
        assert_eq!(Behavior::from_id("made_coffee"), None);
        assert!(serde_json::from_str::<Behavior>("\"made_coffee\"").is_err());
        assert_eq!(Behavior::from_id("unlabelled"), Some(Behavior::Unlabelled));
    }
}
