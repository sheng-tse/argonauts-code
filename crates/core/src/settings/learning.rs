//! What the crew keeps between runs besides recipes: notes and their safety limits, exported
//! skills, and the shared outcome log.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Notes reach every later session, so a bad one keeps acting long after the run that wrote it.
/// The defaults are deliberately tight, and they're safety keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MemorySettings {
    /// Notes one task may propose.
    pub max_per_task: u32,
    /// Characters in one note.
    pub max_chars: u32,
    /// Whether a note may hold a URL.
    pub allow_urls: bool,
    /// Whether a note may hold a shell command.
    pub allow_commands: bool,
    /// What happens to a note the scan flags. A flagged note never applies by itself.
    pub flagged: FlaggedNote,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            max_per_task: 3,
            max_chars: 300,
            allow_urls: false,
            allow_commands: false,
            flagged: FlaggedNote::Ask,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlaggedNote {
    /// It waits for you with the scan's findings.
    #[default]
    Ask,
    /// It's dropped and logged.
    Drop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SkillSettings {
    /// Link each exported skill into .claude/skills, the only place Claude Code reads project
    /// skills from.
    pub claude_link: bool,
    /// Once a promoted learned workflow passes its canary, propose exporting it as a skill, as a branch
    /// that waits for you.
    pub export_after_canary: bool,
}

impl Default for SkillSettings {
    fn default() -> Self {
        Self {
            claude_link: true,
            export_after_canary: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OutcomeSettings {
    /// Also put each finished run's outcome log in .argonauts/outcomes, so your team's track
    /// records add up.
    pub share: bool,
}
