//! Run lifecycle and phases.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;

use crate::names::{AgentId, ApprovalId, ModelRef, SessionKey, TurnId};
use crate::required::RequiredStep;
use crate::settings::approvals::ApprovalMode;
use crate::settings::protocol::CaptainKind;
use crate::settings::workflows::RunStyle;
use crate::steps::intake::{ChangeEstimate, InstructionKind};
use crate::{PhaseStatus, RunId, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RunLifecycle {
    /// The first event of every run.
    Started(Box<RunStarted>),
    /// The run's name and description, from the main agent's brief, or from intake for a Captain
    /// run or a template that runs as written. Every view shows these, never a template's name.
    Named(RunNamed),
    /// A new run host rebuilt the run from the log and carried on.
    Resumed(RunResumed),
    /// Esc at the top level. Cancelled by Enter until the safe point is reached. A second Esc
    /// sends `now`: sessions stop at once and their work is committed.
    PauseRequested {
        by: Party,
        /// Stop now instead of at the next safe point.
        now: bool,
    },
    /// Shift+Tab changed the approval mode for every decision point of this run.
    ApprovalModeChanged {
        mode: ApprovalMode,
        /// Always you.
        by: Party,
    },
    /// You pressed Enter before the safe point.
    PauseCancelled,
    /// The safe point was reached: no new sessions, running ones stopped after their tool call,
    /// worktrees committed. A `Waiting` event follows.
    Paused {
        sessions_stopped: u32,
        committed: Vec<TaskId>,
    },
    /// `/stop`: agents wrap up and report, work is committed, the run ends as stopped.
    StopRequested { by: Party },
    /// The run host exited with nothing running and nothing spending.
    Waiting {
        reason: WaitReason,
        /// Approvals waiting for you, if that's the reason.
        waiting_on: Vec<ApprovalId>,
    },
    /// Everything a resume or a reattach needs, so it can start here instead of at `seq` 0.
    /// The engine writes one every `run_host.snapshot_every` events.
    Snapshot(Box<RunSnapshot>),
    /// How the run ends in the conversation, as an agent's own CLI ends a task: what changed,
    /// how it was checked, and what's left. Just before `Finished`.
    ClosingAnswer(Box<ClosingAnswer>),
    /// The last event of a run, unless it ended `Stopped` and you reopened it with `/resume`,
    /// when a `Resumed` follows.
    Finished {
        outcome: RunOutcome,
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunNamed {
    /// Two to four task words joined with `-`, such as `fix-slugify-median`.
    pub name: String,
    /// One line, the task in plain words.
    pub description: String,
    /// Always `workflow` now: a goal is a condition around the run, in `Goal::Set`.
    pub style: RunStyle,
    /// The example the workflow started from, shown once at the end of the start item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    /// The main agent's done-when list, which the final check judges. Empty when the main agent
    /// wrote none, and then the approved goal's criteria are judged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done_when: Vec<crate::events::goal::Criterion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSnapshot {
    /// The last event this snapshot covers.
    pub upto: u64,
    /// The engine's own state, versioned and read only by the engine. It holds everything
    /// replay would rebuild, including the id counters, so ids never repeat.
    pub engine: Value,
    /// For views that redraw without replaying.
    pub summary: crate::api::StatusSummary,
    pub open_approvals: Vec<crate::events::approval::ApprovalOpened>,
    pub running: Vec<SessionKey>,
}

/// What comes between your message and the main session's own account of the work, in a
/// request the session handed over.
const ACCOUNT_FOLLOWS: &str = "\n\nFrom the conversation, as the main agent put it: ";

/// A request the main session hands over: your message as you typed it, and after it the
/// session's own account of the work when it adds something, marked as the session's. Your
/// last message may be no more than "yes, do that", and the agents that take the work haven't
/// read the conversation.
pub fn handed_over(typed: &str, account: &str) -> String {
    let (typed, account) = (typed.trim(), account.trim());
    if account.is_empty() || account == typed {
        return typed.to_owned();
    }
    format!("{typed}{ACCOUNT_FOLLOWS}{account}")
}

/// The session's account of the work in a request that [`handed_over`] wrote. It says what
/// the work is where your own words say how to run it, or only agree to it.
pub fn account_in(request: &str) -> Option<&str> {
    let (_, account) = request.split_once(ACCOUNT_FOLLOWS)?;
    Some(account.trim()).filter(|account| !account.is_empty())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunStarted {
    /// What you typed, and after it the main session's account of the work when it handed
    /// the request over ([`handed_over`]).
    pub request: String,
    pub title: String,
    pub mode: RunMode,
    /// Whether `/captain`, `/crew`, `--captain`, or `--crew` forced the mode.
    pub forced: bool,
    pub workflow: String,
    /// The main agent's estimate, unless the mode was forced.
    pub estimate: Option<ChangeEstimate>,
    pub captain_or_crew: Option<CaptainOrCrewDecision>,
    pub conversation_turn: Option<TurnId>,
    /// The merged run this one continues (`/follow-up`).
    pub continues: Option<RunId>,
    /// Absolute path of the repository.
    pub repo_root: PathBuf,
    pub base_branch: Option<String>,
    pub base_commit: String,
    /// Uncommitted paths left out (the default), or included when you asked.
    pub uncommitted: Vec<String>,
    pub uncommitted_included: bool,
    pub settings_sha256: String,
    pub settings: Value,
    /// Every `--set` and where each key came from, for the ablation record.
    pub overrides: Vec<OverrideRecord>,
    /// Keyed by adapter id.
    pub cli_versions: BTreeMap<String, String>,
    /// Argonauts Code version.
    pub version: String,
    /// The MCP client that asked for the run, by the name it reported for itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_agent: Option<String>,
    /// What the request said about how to run it, and what came of each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instructions: Vec<CheckedInstruction>,
    /// Steps a setup kept for the conversation or the request that started the run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_steps: Vec<RequiredStep>,
    /// What the orchestrator begins from, when your words in a conversation started the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_from: Option<StartsFrom>,
}

/// What the orchestrator of a run your words started begins from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartsFrom {
    /// A copy of the main session, made by the CLI they share: the whole conversation, with
    /// the brief as its first message.
    Fork(ForkFrom),
    /// The brief alone, as every worker starts.
    Brief(BriefFrom),
}

/// The main session an orchestrator's fork copies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkFrom {
    pub worker: ModelRef,
    /// The CLI's own id for it.
    pub cli_session: String,
    /// Where it ran, which is where its CLI resumes it: Claude Code keeps sessions by folder.
    pub folder: PathBuf,
    /// How many tokens the conversation held when the run started, when its CLI said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
}

/// The brief an orchestrator starts from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefFrom {
    /// The main agent, which wrote it from the conversation.
    pub written_by: ModelRef,
    /// Why the orchestrator can't carry the conversation, when that's why: "agy can't carry a
    /// Claude Code conversation".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// One instruction from a request, after code checked it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckedInstruction {
    /// The request's own words for it.
    pub says: String,
    pub kind: InstructionKind,
    #[serde(flatten)]
    pub outcome: InstructionOutcome,
}

impl CheckedInstruction {
    pub fn followed(&self) -> bool {
        matches!(self.outcome, InstructionOutcome::Followed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum InstructionOutcome {
    /// Applied, as the start line says it: "Codex orchestrates". `note` says what of it couldn't
    /// be, such as "Codex has no model named opus".
    Followed {
        said: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// Not followed, and why, as the start line says it.
    NotFollowed { why: String },
    /// Its words aren't in your message, so the model wrote it, not you. Only Ctrl+O lists it.
    Dropped,
}

/// A run's last word to you, in the main agent's voice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosingAnswer {
    /// The agent's own words: Captain's answer, or the main agent's from the tasks' reports.
    /// Code writes it when no agent could.
    pub text: String,
    /// Who wrote `text`. `None` when code did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<ModelRef>,
    /// What changed, with lines added and removed. The run's own records are left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ChangedFile>,
    /// How it was checked: the checks Argonauts Code ran, each with its result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckLine>,
    /// The tests the agents say they ran, in their words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<String>,
    /// Anything left or worth knowing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<String>,
    /// What the run decided in your place on autopilot, for you to review.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decided: Vec<DecidedAlone>,
}

/// One thing a run on autopilot decided where it would have asked you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecidedAlone {
    /// What was decided, in one line.
    pub what: String,
    /// Whose call it was, such as "the worker's call".
    pub whose: String,
}

impl DecidedAlone {
    /// The line that follows what was decided: whose call it was, and what you can do.
    pub fn review_note(&self) -> String {
        format!(
            "({}; ask for a change if you want otherwise)",
            self.whose.trim()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLine {
    /// The check's name.
    pub what: String,
    pub passed: bool,
    /// Such as "6 passed". Empty when there's nothing more to say.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub result: String,
}

/// One setting that is different from the built-in default, and the layer that set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverrideRecord {
    /// Dotted key.
    pub key: String,
    /// Value as TOML text.
    pub value: String,
    /// `global`, `project`, `workflow`, or `flag`.
    pub from: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// One task for the main agent.
    Captain,
    /// Goal check, planning, dividing, task pipelines.
    Crew,
}

/// The Captain-or-crew rules' answer, computed by code from the estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptainOrCrewDecision {
    pub mode: RunMode,
    /// Captain first, then crew if Captain's task needs you.
    pub move_to_crew: bool,
    /// The rule that fired, such as `captain_max_files` or `learned:api-feature`.
    pub rule: String,
    /// One line for you, such as "at most 3 files, and tests exist". Empty when your words
    /// chose the mode.
    pub reason: String,
    /// Where a Captain run works: in your checkout, or checked in a worktree. `None` for the crew.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captain: Option<CaptainKind>,
}

impl CaptainOrCrewDecision {
    /// The rule that fires when your words chose the mode.
    pub const AS_YOU_ASKED: &'static str = "request";

    /// Your words chose the mode, not the rules.
    pub fn as_you_asked(&self) -> bool {
        self.rule == Self::AS_YOU_ASKED
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResumed {
    pub cause: ResumeCause,
    pub settings_changed: bool,
    pub cli_versions_changed: Vec<String>,
    /// Commits `main` moved by, if it moved. The final check runs again before the merge.
    pub main_moved_by: Option<u32>,
    pub sessions: Vec<ResumedSession>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeCause {
    /// Enter or `/resume`.
    User,
    /// Your answer to an approval.
    Answer,
    /// An agent the run waited for came back, as its health says.
    AgentBack,
    /// The time a waiting run waited for passed, so it tried again on its own.
    Timer,
    /// A fallback approval's `ask_timeout` passed, so the role's fallback applies.
    AskTimeout,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumedSession {
    /// The new session key.
    pub session: SessionKey,
    pub task: Option<TaskId>,
    /// The CLI session id, when resumed by id.
    pub cli_session: Option<String>,
    /// Resumed with every launch flag passed again, or restarted fresh with a fallback note.
    pub resumed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum WaitReason {
    /// An approval waits for you and no session is running.
    Approval,
    Paused,
    /// Every agent is down or out of plan usage. It resumes when one returns.
    AllAgentsOut {
        /// The earliest known reset.
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
    },
    /// Its remaining tasks wait for one agent (fallback mode `wait`).
    WaitingForAgent {
        agent_id: AgentId,
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
    },
    /// A step's agent stopped for an agent reason, and nothing could take a job the run
    /// needs, so the run stopped fully, its work kept. Waiting can't bring a model or a login
    /// back, so it never tries again on its own: only `/resume` carries it on.
    NoFallback {
        /// Which role nobody could fill, in words, as the run's row says it.
        detail: String,
        /// When an older run said it would try again. A run stops with none now.
        #[serde(with = "time::serde::rfc3339::option")]
        until: Option<OffsetDateTime>,
        /// Why it stopped, what was tried, and how to carry it on. Older runs have none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop: Option<Box<RunStop>>,
    },
}

/// What a run that stopped fully says in its one item: why, each job nothing could take
/// with what its own CLI said, what else was tried, and how to carry it on.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunStop {
    /// Such as `a council needs two proposals, and only propose:p1 can make one`.
    pub why: String,
    /// Each job nothing could take: `propose:p3 · agy is signed out`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// Why nothing else could take them: `OpenCode has no model set`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tried: Vec<String>,
    /// The agents whose work so far is kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<String>,
    /// The CLIs whose working again lets `/resume` carry the run on, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waits_for: Vec<String>,
}

impl RunStop {
    /// The item's lines under its title: the agents, what else was tried, and how to carry on.
    pub fn lines(&self) -> Vec<String> {
        let mut lines = self.agents.clone();
        // Each is a clause of its own, so the last joins with a comma before its `and`.
        let tried = match self.tried.as_slice() {
            [] => None,
            [only] => Some(only.clone()),
            [rest @ .., last] => Some(format!("{}, and {last}", rest.join(", "))),
        };
        if let Some(tried) = tried {
            let them = if self.agents.len() > 1 { "them" } else { "it" };
            lines.push(format!("Nothing else can take {them}: {tried}"));
        }
        let kept = match self.kept.as_slice() {
            [] => "Its work so far is kept".to_owned(),
            kept => format!("{}'s work so far is kept", crate::text::and_list(kept)),
        };
        let carry_on = match self.waits_for.as_slice() {
            [] => "/resume carries the run on".to_owned(),
            clis => format!(
                "once {} works, /resume carries the run on",
                clis.join(" or ")
            ),
        };
        lines.push(format!("{kept} · {carry_on}"));
        lines
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Merged,
    /// A pull request was opened or suggested.
    PullRequest,
    /// Branches kept for later.
    BranchLeft,
    /// You abandoned it at the merge approval.
    Abandoned,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "party", rename_all = "snake_case")]
pub enum Party {
    /// You, interactively.
    User,
    /// Your approval rules.
    Rules,
    /// The judge of approvals, a model that decides in your place.
    Judge { model: ModelRef },
    /// A setting, such as `merge.mode = "leave"`.
    Config,
    /// Autopilot, in your place: nothing waits for you there, so what your rules and the
    /// judge couldn't settle takes its safe option.
    Autopilot,
    /// You pressed Esc on it, which means "carry on without me": the option was
    /// taken in your place, as `approvals.when_declined` says.
    Declined,
    /// Argonauts Code itself, by a fixed rule.
    ArgonautsCode,
    /// The internet, gone for `network.hold_minutes`: the run pauses rather than hold its
    /// sessions open.
    Network,
    Agent {
        session: SessionKey,
        worker: ModelRef,
    },
    /// An MCP client such as an outside agent, by the name it reported for itself. It can guide
    /// and pause, never approve.
    OutsideAgent { client: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseChange {
    pub phase: RunPhase,
    pub status: PhaseStatus,
    /// The one-line summary the collapsed step shows, such as "3 agents read the request back".
    pub summary: Option<String>,
}

/// Phases of a run. Captain runs use Understand, Build, FinalCheck, Merge, Run review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Readings, comparisons, questions, the Understand approval.
    Understand,
    /// Drafts, map, critique, revision, experiments, judging.
    Planning,
    /// Task graph, workers, review ring, predictions, limit lines, plan approval.
    Dividing,
    /// Interface task and acceptance-test tasks.
    Contracts,
    /// Task pipelines.
    Build,
    /// Bring in main, full suite, end-to-end checks.
    FinalCheck,
    /// The merge approval and the merge.
    Merge,
    /// Predictions scored, memory, learned workflow candidate.
    RunReview,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handed_over_request_is_your_words_first_then_the_sessions_account() {
        assert_eq!(handed_over(" yes, do that ", ""), "yes, do that");
        assert_eq!(
            handed_over("add due dates", "add due dates"),
            "add due dates"
        );
        assert_eq!(
            handed_over(
                "yes, do that",
                "Add due dates to tasks, in the cli and the store."
            ),
            "yes, do that\n\nFrom the conversation, as the main agent put it: Add due dates to \
             tasks, in the cli and the store."
        );
    }

    #[test]
    fn the_sessions_account_is_read_back_from_a_handed_over_request() {
        let account = "Add due dates to tasks, in the cli and the store.";
        assert_eq!(
            account_in(&handed_over("yes, do that", account)),
            Some(account)
        );
        // Your words alone carry no account, whatever they quote.
        assert_eq!(account_in("add due dates"), None);
        assert_eq!(account_in("as the main agent put it: add due dates"), None);
    }
}
