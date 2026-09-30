//! Goal alignment events and the goal file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::run::Party;
use crate::RunId;
use crate::names::{MessageId, WorkerRef};
use crate::steps::goal::{DiffTag, Difference, Restatement};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum GoalEvent {
    /// One restatement, under its anonymous label.
    Restated {
        /// `A`, `B`, `C`.
        label: String,
        /// Shown to you, never to the differs.
        worker: WorkerRef,
        restatement: Restatement,
    },
    Compared {
        worker: WorkerRef,
        differences: Vec<Difference>,
    },
    /// The materiality rules' result, computed by code.
    Settled {
        /// What every restatement shares.
        agreements: Vec<String>,
        /// Majority readings adopted as stated assumptions.
        assumptions: Vec<Assumption>,
        /// At most `goal.max_questions`.
        questions: Vec<GoalQuestion>,
        /// Differences dropped as wording.
        dropped: u32,
    },
    Drafted(GoalFile),
    /// Approved and frozen. Build sessions need its hash.
    Approved {
        file: GoalFile,
        by: Party,
    },
    ChangeRequested {
        id: MessageId,
        from: Party,
        text: String,
        /// True when it only clarifies and leaves the success criteria unchanged.
        clarification: bool,
    },
    /// What `approvals.goal_changes` did with it.
    ChangeResolved {
        id: MessageId,
        resolution: GoalChangeResolution,
        new_sha256: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assumption {
    pub text: String,
    /// Such as "a detail, and 2 of 3 read it that way".
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalQuestion {
    /// 1-based, out of `of`.
    pub n: u8,
    pub of: u8,
    pub text: String,
    pub options: Vec<ChoiceOption>,
    /// Scope, security, data, and cost always reach you.
    pub tag: DiffTag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub key: char,
    pub label: String,
    pub recommended: bool,
    /// Such as "2 of 3 restatements read it this way".
    pub basis: Option<String>,
}

/// The goal file, `.argonauts/goal/<run>.md`, frozen once approved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalFile {
    pub run: RunId,
    /// 1, then +1 for each approved change.
    pub version: u32,
    pub summary: String,
    /// Each becomes at least one acceptance test.
    pub criteria: Vec<Criterion>,
    pub in_scope: Vec<String>,
    pub out_of_scope: Vec<String>,
    pub assumptions: Vec<Assumption>,
    pub answers: Vec<String>,
    pub source: GoalSource,
    pub path: PathBuf,
    /// SHA-256 of the file's bytes.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Criterion {
    /// `G1`, `G2`, ...
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalSource {
    /// Your prompt recorded as the goal (solo without `restate_first`).
    Prompt,
    Restatements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalChangeResolution {
    /// A clarification recorded as a decision (`allow_clarifications`).
    Recorded,
    /// `deny`. The card continues against the approved goal, or escalates.
    Refused,
    AskedYou,
    Changed,
}
