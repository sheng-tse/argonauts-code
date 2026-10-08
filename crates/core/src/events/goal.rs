//! Understand's events (once goal alignment), the goal file, and a goal's own events: the goal
//! as it's set, its tries, the checks, and the answers given for you. A goal is a condition
//! around a run's workflow or around one task; the events of a task's goal carry its id.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::run::Party;
use crate::goal::{CheckWith, GoalFrom, GoalScope, KeptBy};
use crate::names::{MessageId, WorkerRef};
use crate::steps::goal::{DiffTag, Difference, Restatement};
use crate::{CardId, RunId};

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
    /// A goal is set: the run's, when it starts with one or takes one on the way, or a task's,
    /// when its role works each task as a goal.
    Set(Box<GoalRun>),
    /// A try began: for the run's goal a round, as the phase of the same title, and for a
    /// task's goal the session that works it.
    AttemptStarted {
        n: u32,
        title: String,
        /// The workflow phase it runs as, from 0.
        phase: u32,
        /// The task whose goal it is. `None` for the run's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
    },
    /// The check's verdicts on a try, and what code decided from them.
    Checked(Box<GoalChecked>),
    StatusChanged {
        status: GoalStatus,
        by: Party,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        /// Why, when it's stuck or stopped short: "the same failure 3 rounds running".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        why: Option<String>,
    },
    /// The goal failed the same way `goal.stuck_after` rounds running, so the next try takes
    /// another approach. It happens once; stuck again, the goal stops and says why.
    ApproachChanged {
        /// The try whose check found it stuck.
        after: u32,
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
    },
    /// What the CLI that keeps a goal says about it: its own account, never the check.
    CliSaid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        said: CliGoalSaid,
    },
    /// You asked for a goal's checks now, while its try is still at work.
    CheckingNow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        /// The try under way, from 1.
        attempt: u32,
    },
    /// What a check you asked for found. It shows as the last check and decides nothing: only
    /// a try's own check sends the work round again or ends the goal.
    CheckedNow(Box<GoalChecked>),
    /// A check you asked for ended with nothing found, as when the run stopped under it.
    CheckNowDropped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        why: String,
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
    /// Hold a goal: the try in progress finishes and is checked, and nothing is sent round
    /// again until it's resumed. `task` names one task's goal; `None` is the run's own.
    Pause {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
    },
    /// Pick a paused goal up again.
    Resume {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
    },
    /// Rewrite a goal's lines, one thing that must hold a line. The tries so far stay, and the
    /// next check reads the new lines.
    EditLines {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
        lines: Vec<String>,
    },
    /// Run a task's goal's checks now, beside the try under way. What they find shows as the
    /// last check; the try's own check still decides what follows.
    CheckNow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task: Option<CardId>,
    },
    /// The running workflow becomes round 1 of a goal with its done-when list, checked when
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
pub const LEAD_ANSWER: &str = "the main agent's answer";

/// The go-ahead's second answer: yes, and from now on the lead answers the crew's questions in
/// this project, as `approvals.answers = "lead"` saved for you.
pub const LET_THE_LEAD_ANSWER: &str = "Yes, and let the main agent answer";

/// Every question's last answer: the lead answers this question and the ones after it.
pub const LET_THE_LEAD_DECIDE: &str = "Let the main agent decide this and the rest";

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

/// How a criterion is checked: a test by name, a command by its exit code, the project's own
/// checks, or the judge's reading when nothing a command can check changed (a refactor, the
/// docs).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CriterionCheck {
    Test(String),
    Command(String),
    /// The project's own checks, `checks.run`: "its tests pass".
    Checks,
    #[default]
    Review,
}

/// A goal being kept: the condition, its lines, how they're checked, and who keeps it going.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRun {
    /// The condition in the words it was set with: yours verbatim from `/goal <condition>`.
    pub objective: String,
    /// The goal's lines, each with how it's checked.
    pub criteria: Vec<Criterion>,
    pub attempts: Vec<Attempt>,
    pub status: GoalStatus,
    /// The most rounds, when you set one. No fixed number otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    /// The task it sits around. `None` for the run's goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<CardId>,
    #[serde(default)]
    pub scope: GoalScope,
    #[serde(default)]
    pub kept_by: KeptBy,
    /// Why Argonauts keeps a goal that was asked of the CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_note: Option<String>,
    #[serde(default)]
    pub check: CheckWith,
    /// Commands for the goal as a whole; every one must exit 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
    /// Where it came from. `None` in journals older than goals as conditions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<GoalFrom>,
}

impl GoalRun {
    /// A goal just set from `spec`, with no try yet.
    pub fn new(spec: &crate::goal::GoalSpec, scope: GoalScope, kept_by: KeptBy) -> Self {
        Self {
            objective: spec.condition.clone(),
            criteria: spec.lines.clone(),
            attempts: Vec::new(),
            status: GoalStatus::Pursuing,
            max_rounds: spec.max_rounds,
            task: None,
            scope,
            kept_by,
            kept_note: None,
            check: spec.check,
            commands: spec.commands.clone(),
            from: Some(spec.from),
        }
    }

    /// The goal as it was set, for the check.
    pub fn spec(&self) -> crate::goal::GoalSpec {
        crate::goal::GoalSpec {
            condition: self.objective.clone(),
            lines: self.criteria.clone(),
            check: self.check,
            commands: self.commands.clone(),
            max_rounds: self.max_rounds,
            from: self.from.unwrap_or(GoalFrom::Command),
        }
    }
}

/// What a CLI reports about its own goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliGoalSaid {
    /// The CLI's agent id, such as `codex`.
    pub cli: String,
    /// `active`, `complete`, `blocked`, `paused`, as the CLI words it.
    pub status: String,
    /// Its own note on where it stands. Empty when it gave none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns: Option<u32>,
}

/// One try at a goal: a round of the workflow, shown as its phases, or one session on a task.
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
    /// The same failure `goal.stuck_after` rounds running after it changed approach once,
    /// `max_rounds` used up, or the check says it can't be met.
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
    /// The task whose goal was checked. `None` for the run's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<CardId>,
    /// Everything that ran, in order: each command, then the judge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckRun>,
}

/// One thing a goal's check ran, and the lines it decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    /// The command as written, `your checks: tests`, or `the judge`.
    pub name: String,
    pub by: CheckedBy,
    /// The ids of the lines it decided.
    pub lines: Vec<String>,
    pub passed: bool,
    /// One line: `1 failed · 11 passed`.
    pub summary: String,
    /// How many of its tests failed and ran, when its output says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    pub ms: u64,
    /// The end of its output, where what failed is named.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// The judge's agent, such as `claude-code/opus`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<WorkerRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckedBy {
    /// A command's exit code.
    Command,
    /// A model from another provider than the work, reading the result against each line.
    Judge,
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
