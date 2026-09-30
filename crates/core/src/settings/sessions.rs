//! Sessions, compaction, fan-out, concurrency, the circuit breaker, and the supervisor.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Span;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SessionSettings {
    /// For every role unless the role overrides it.
    pub lifetime: SessionLifetime,
    /// When a session rotates to a fresh one with a handoff note.
    pub rotate_at: RotateAt,
    /// Chat with the default agent resumes the same vendor session each turn.
    pub chat_resumes: bool,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            lifetime: SessionLifetime::Card,
            rotate_at: RotateAt::Text("200k".to_owned()),
            chat_resumes: true,
        }
    }
}

/// A token cap such as `"200k"` or `150000`, which holds whatever the window, or a share of
/// the context window such as `0.9`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RotateAt {
    Tokens(u64),
    /// A count with an optional `k` or `m`.
    Text(String),
    Share(f64),
}

impl RotateAt {
    /// The token count at which a session rotates. A share needs the window's size, and is
    /// `None` without it.
    pub fn tokens(&self, window: Option<u64>) -> Result<Option<u64>, String> {
        match self {
            Self::Tokens(0) => Err("rotate_at 0 would rotate at once".to_owned()),
            Self::Tokens(count) => Ok(Some(*count)),
            Self::Text(text) => parse_token_count(text).map(Some),
            Self::Share(share) if *share > 0.0 && *share <= 1.0 => {
                Ok(window.map(|window| (window as f64 * share) as u64))
            }
            Self::Share(share) => Err(format!("rotate_at {share} isn't a share from 0 to 1")),
        }
    }
}

fn parse_token_count(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let bad = || format!("rotate_at {text:?}: use a count such as \"200k\" or \"1m\"");
    let (digits, scale) = match text.char_indices().last() {
        Some((at, 'k' | 'K')) => (&text[..at], 1_000),
        Some((at, 'm' | 'M')) => (&text[..at], 1_000_000),
        _ => (text, 1),
    };
    let count: u64 = digits.trim().parse().map_err(|_| bad())?;
    count.checked_mul(scale).filter(|n| *n > 0).ok_or_else(bad)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionLifetime {
    /// A fresh session per card, resumed for that card's retries.
    Card,
    /// One session per role for the whole run.
    Run,
    /// A long-lived main session per role, reused across runs.
    Project,
}

/// Compaction per launched session. Your harness settings files are never edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Compaction {
    /// Pass nothing; the harness uses your settings.
    Inherit,
    Auto,
    /// No compaction where the harness allows it (Claude Code); a full session rotates.
    Off,
}

/// A harness's own subagent cap per card: `"on"`, `"off"`, or `{ max = N }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NativeFanout {
    Switch(FanoutSwitch),
    /// At most N at once.
    Limit {
        max: u32,
    },
}

impl Default for NativeFanout {
    fn default() -> Self {
        Self::Limit { max: 3 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FanoutSwitch {
    /// The harness decides.
    On,
    Off,
}

/// Global concurrency caps, shared by every run through the per-user registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ConcurrencySettings {
    pub max_sessions: u32,
    pub max_per_vendor: u32,
}

impl Default for ConcurrencySettings {
    fn default() -> Self {
        Self {
            max_sessions: 6,
            max_per_vendor: 3,
        }
    }
}

/// The circuit breaker per vendor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HealthSettings {
    /// Errors or retries within `window` that mark a vendor degraded.
    pub degraded_after: u32,
    /// Overload or server errors within `window` that mark it down...
    pub down_after: u32,
    /// ...seen across at least this many sessions.
    pub down_sessions: u32,
    pub window: Span,
    /// How long a down vendor rests before a test card, unless it reported a reset time.
    pub cooldown: Span,
}

impl Default for HealthSettings {
    fn default() -> Self {
        Self {
            degraded_after: 2,
            down_after: 3,
            down_sessions: 2,
            window: Span::new("2m"),
            cooldown: Span::new("5m"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct SupervisorSettings {
    /// How long a supervisor with nothing to do waits for an answer before it parks and exits.
    pub park_grace: Span,
    /// The engine writes a `Run::Snapshot` after this many events, so a resume or a reattach
    /// replays from the latest snapshot instead of from the start.
    pub snapshot_every: u32,
    /// Most text and reasoning deltas per second per session that go into the journal. Finer
    /// deltas reach live subscribers only.
    pub journal_deltas_per_second: u32,
}

impl Default for SupervisorSettings {
    fn default() -> Self {
        Self {
            park_grace: Span::new("10s"),
            snapshot_every: 5000,
            journal_deltas_per_second: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct RunsSettings {
    /// A run parked longer than this many days gets a reminder in `/resume` and
    /// `argonauts doctor`. Parked runs never expire.
    pub remind_parked_days: u32,
}

impl Default for RunsSettings {
    fn default() -> Self {
        Self {
            remind_parked_days: 7,
        }
    }
}
