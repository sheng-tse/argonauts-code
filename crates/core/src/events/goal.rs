//! Understand's events (once goal alignment), the goal file, and the goal style's events:
//! the goal, its attempts, the checks, and the answers given for you.

use std::path::PathBuf;

use schemars::JsonSchema;
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
    /// A goal-style run's goal, set when the start card or your rules approve it.
    Set(Box<GoalRun>),
    /// An attempt began, as the phase of the same title.
    AttemptStarted {
        n: u32,
        title: String,
        /// The workflow phase it runs as, from 0.
        phase: u32,
    },
    /// The check's verdicts on an attempt, and what code decided from them.
    Checked(Box<GoalChecked>),
    StatusChanged {
        status: GoalStatus,
        by: Party,
    },
    /// A question answered, by you, the lead, your rules, or a majority of readings.
    Answered {
        question: String,
        answer: String,
        by: Decider,
    },
    /// An assumption changed to another reading (the goal view, or `/status`).
    AssumptionChanged {
        /// Its place in the goal file's assumptions, from 1.
        id: u32,
        to: String,
        by: Decider,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assumption {
    pub text: String,
    /// Such as "a detail, and 2 of 3 read it that way".
    pub basis: String,
    /// Who decided it, when that was recorded. Older goal files only say so in `basis`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Decider>,
}

/// What you do to a run's goal, from `/goal` and the goal view. Only you do these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GoalAction {
    /// Stop pursuing it. With `land`, Merge lands what holds once every check passes; without,
    /// the run stops and keeps its branch.
    Clear { land: bool },
    /// The running workflow becomes attempt 1 of a goal with its done-when list, checked when
    /// it ends.
    TurnIntoGoal,
    /// Assumption `id` (from 1) takes another reading, from the next attempt or phase end.
    ChangeAssumption { id: u32, to: String },
}

/// Who settled a question or an assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decider {
    /// Two of three readings shared it, outside `understand.always_ask`.
    Majority,
    Lead,
    Rules,
    You,
}

/// How the basis of an answer the lead gave for you starts, when the lead answers the crew's
/// questions. The goal file shows the basis, so this marks it there too.
pub const LEAD_ANSWER: &str = "the lead's answer";

/// The go-ahead's second answer: yes, and from now on the lead answers the crew's questions in
/// this project, as `approvals.answered_by = "lead"` saved for you.
pub const LET_THE_LEAD_ANSWER: &str = "Yes, and let the lead answer";

/// Every question's last answer: the lead answers this question and the ones after it.
pub const LET_THE_LEAD_DECIDE: &str = "Let the lead decide this and the rest";

impl Assumption {
    /// The lead answered it for you.
    pub fn by_lead(&self) -> bool {
        self.basis.starts_with(LEAD_ANSWER)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalQuestion {
    /// 1-based, out of `of`.
    pub n: u8,
    pub of: u8,
    pub text: String,
    pub options: Vec<ChoiceOption>,
    /// Scope, security, data, cost, or a detail. Understand's `always_ask` makes the first four
    /// questions; who answers follows `approvals`.
    pub tag: DiffTag,
    /// What it's about in a word or two, such as "Log level", which `approvals.ask_you_about`
    /// may name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
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

/// One item of a done-when list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Criterion {
    /// `G1`, `G2`, ...
    pub id: String,
    /// One sentence a check can prove, such as `median([]) raises ValueError`.
    pub text: String,
    /// A few words for narrow places, such as `Empty list`. Empty in older goal files.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub short: String,
    /// How it's checked. Older goal files leave it to a reading of the diff.
    #[serde(default)]
    pub check: CriterionCheck,
}

impl Criterion {
    /// A criterion checked by reading the diff, with no short name.
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            short: String::new(),
            check: CriterionCheck::Review,
        }
    }
}

/// How a criterion is checked: a test by name, a command, or a reading of the diff when nothing
/// a test can check changed (a refactor, the docs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CriterionCheck {
    Test(String),
    Command(String),
    #[default]
    Review,
}

/// A goal-style run's goal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRun {
    /// One line, the run's description. Your words verbatim from `/goal <objective>`.
    pub objective: String,
    pub criteria: Vec<Criterion>,
    pub attempts: Vec<Attempt>,
    pub status: GoalStatus,
    pub max_attempts: u32,
}

/// One pass at a goal, shown as one phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// From 1.
    pub n: u32,
    pub title: String,
    /// The phase it runs as, from 0.
    pub phase: u32,
    /// `None` until its check returns.
    pub verdict: Option<GoalVerdict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Pursuing,
    /// By you, or a usage limit.
    Paused,
    /// Only the check, through code, sets it.
    Met,
    /// The same blocker `goal.stuck_after` attempts running, no progress for
    /// `goal.idle_after`, no attempts left, or the check says it can't be met.
    Stuck,
    OverBudget,
    /// You stopped pursuing it.
    Cleared,
}

/// The check's answer for one criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CriterionVerdict {
    /// `G1`, ...
    pub id: String,
    pub verdict: CriterionState,
    /// A passing test, or a file and line. Empty unless met.
    pub evidence: String,
    /// What's missing. Empty when met.
    pub missing: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CriterionState {
    Met,
    NotYet,
    /// Counts as not yet: uncertain or indirect evidence isn't proof.
    CantTell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GoalVerdict {
    Met,
    NotYet,
    /// The goal can't be met as written.
    Impossible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalChecked {
    pub attempt: u32,
    pub verdicts: Vec<CriterionVerdict>,
    /// What code decided from the verdicts and the gates.
    pub overall: GoalVerdict,
    pub reason: String,
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
