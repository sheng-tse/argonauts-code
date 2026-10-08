//! The built-in presets. Each file sets only the keys its preset changes on top of the
//! defaults, which are `council`, so `council.toml` is empty.

pub(crate) const DEFAULT_PRESET: &str = "council";

pub(crate) struct BuiltinPreset {
    pub name: &'static str,
    pub description: &'static str,
    /// The keys it sets, as TOML.
    pub toml: &'static str,
    /// Its steps, as the file `/setup` writes out shows them. The ablations and solo have none.
    pub steps: Option<&'static str>,
}

macro_rules! preset {
    ($name:literal, $description:literal) => {
        BuiltinPreset {
            name: $name,
            description: $description,
            toml: include_str!(concat!("../../workflows/", $name, ".toml")),
            steps: None,
        }
    };
    // The steps are the runner's own built-in files, so what is shown is what runs.
    ($name:literal, $description:literal, steps) => {
        BuiltinPreset {
            name: $name,
            description: $description,
            toml: include_str!(concat!("../../workflows/", $name, ".toml")),
            steps: Some(include_str!(concat!(
                "../../../runner/workflows/",
                $name,
                ".toml"
            ))),
        }
    };
}

/// In the order the picker shows them, the default first.
pub(crate) const BUILTIN: &[BuiltinPreset] = &[
    preset!(
        "council",
        "No single planner: everyone proposes and challenges, the judge decides (the default)",
        steps
    ),
    preset!(
        "orchestrator",
        "The orchestrator plans and delegates; workers build, reviewers check",
        steps
    ),
    preset!("solo", "One agent does every task"),
    preset!(
        "maker-checker",
        "A worker builds, a reviewer sends it back until it holds",
        steps
    ),
    preset!(
        "best-of-n",
        "Several workers try the same task and the judge picks one",
        steps
    ),
    preset!(
        "draft-and-vote",
        "Drafts, then a vote; no critique or experiments"
    ),
    preset!("ablate-no-blind", "Drafts see each other"),
    preset!("ablate-no-anonymize", "Judges see authors"),
    preset!("ablate-no-critique", "No critique round"),
    preset!("ablate-fixed-roles", "Fixed workers, no router"),
    preset!(
        "ablate-same-provider",
        "Reviewers may share the worker's provider"
    ),
    preset!("ablate-no-experiments", "No experiments"),
];

pub(crate) fn builtin(name: &str) -> Option<&'static BuiltinPreset> {
    BUILTIN.iter().find(|preset| preset.name == name)
}
