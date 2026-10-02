//! The built-in presets. Each file sets only the keys its preset changes on top of the
//! defaults, which are `peer-crew`, so `peer-crew.toml` is empty.

pub(crate) const DEFAULT_PRESET: &str = "peer-crew";

pub(crate) struct BuiltinPreset {
    pub name: &'static str,
    pub description: &'static str,
    /// The keys it sets, as TOML.
    pub toml: &'static str,
}

macro_rules! preset {
    ($name:literal, $description:literal) => {
        BuiltinPreset {
            name: $name,
            description: $description,
            toml: include_str!(concat!("../../presets/", $name, ".toml")),
        }
    };
}

/// In the order the picker shows them, the default first.
pub(crate) const BUILTIN: &[BuiltinPreset] = &[
    preset!(
        "peer-crew",
        "Peers, blind drafts, cross-family review (the default)"
    ),
    preset!(
        "coordinated-crew",
        "One coordinator plans and delegates; blind reviewers and an adjudicator"
    ),
    preset!("solo", "One agent does every task"),
    preset!(
        "solo-reviewed",
        "One agent, reviewed by another model family"
    ),
    preset!(
        "best-of-n",
        "Several solo attempts; keeps one that passes the visible tests"
    ),
    preset!(
        "draft-and-vote",
        "Drafts, then a vote; no critique or spikes"
    ),
    preset!("ablate-no-blind", "Drafts see each other"),
    preset!("ablate-no-anonymize", "Judges see authors"),
    preset!("ablate-no-critique", "No critique round"),
    preset!("ablate-fixed-roles", "Fixed owners, no router"),
    preset!(
        "ablate-same-family",
        "Reviewers may share the owner's family"
    ),
    preset!("ablate-no-spikes", "No spikes"),
];

pub(crate) fn builtin(name: &str) -> Option<&'static BuiltinPreset> {
    BUILTIN.iter().find(|preset| preset.name == name)
}
