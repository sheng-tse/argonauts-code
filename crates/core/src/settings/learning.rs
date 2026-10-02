//! What the crew keeps between runs besides recipes: notes and their safety limits, exported
//! skills, the shared outcome log, and the prompt templates a recipe may replace.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Notes reach every later session, so a bad one keeps acting long after the run that wrote it.
/// The defaults are deliberately tight, and they're safety keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MemorySettings {
    /// Notes one card may propose.
    pub max_per_card: u32,
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
            max_per_card: 3,
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
    /// Once a promoted recipe passes its canary, propose exporting it as a skill, as a branch
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

/// Prompt templates a recipe or you may replace. Judge and reviewer prompts aren't here: they
/// produce the evidence approvals read, so nothing replaces them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PromptSettings {
    /// A Markdown file in the repo that replaces the drafting prompt. Empty uses the built-in.
    pub drafting: String,
    /// A Markdown file in the repo that replaces the build prompt. Empty uses the built-in.
    pub build: String,
}
