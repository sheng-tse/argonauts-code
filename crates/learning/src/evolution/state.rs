//! The loop's state as JSON in the repo's evolution folder, outside every sandbox: candidates,
//! canaries, each learned workflow's pointer, and the monthly spend ledger. A candidate's learned
//! workflow files sit beside them in `candidates/<id>/` until a promotion commits them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use argonauts_core::events::run::Party;

use super::Candidate;
use crate::error::Result;
use crate::types::RunRecord;

pub const CANDIDATES: &str = "candidates.json";
pub const CANARIES: &str = "canaries.json";
pub const LEARNED_WORKFLOWS: &str = "learned_workflows.json";
pub const SPEND: &str = "spend.json";
/// Each candidate's learned workflow folder, by id.
pub const CANDIDATE_DIRS: &str = "candidates";

/// A promoted version running its canary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Canary {
    pub learned_workflow: String,
    pub candidate: String,
    pub version: u32,
    pub parent: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
    /// Who promoted it.
    pub by: Party,
    pub runs: Vec<RunRecord>,
}

/// What Argonauts Code knows about one learned workflow beyond the checkout.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LearnedWorkflowState {
    /// The version that runs, when a promotion or rollback set it ahead of the checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<Pointer>,
    /// The last promotion or canary start, for `min_days_between`.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_promoted: Option<OffsetDateTime>,
}

/// A version set in Argonauts Code's state. It holds only while the checkout's `current` is
/// still what it was then: once you merge or change the pointer yourself, the checkout wins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pointer {
    pub version: u32,
    pub checkout_was: Option<u32>,
}

/// Money a replay spent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub candidate: String,
    pub usd: f64,
}

pub type Candidates = Vec<Candidate>;
pub type Canaries = BTreeMap<String, Canary>;
pub type LearnedWorkflows = BTreeMap<String, LearnedWorkflowState>;
pub type Ledger = Vec<Spend>;

pub fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if !path.is_file() {
        return Ok(T::default());
    }
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// Writes through a temporary file and a rename, so a crash leaves the old state or the new,
/// never half of one.
pub fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = temporary_beside(path);
    std::fs::write(&temporary, serde_json::to_string_pretty(value)?)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn temporary_beside(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}
