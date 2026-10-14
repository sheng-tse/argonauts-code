//! The check that decides whether a goal holds. One entry point, [`goal`]: a goal's lines and
//! a place to run, in; each line met or not and what still fails, out. A line is checked by a
//! command (its exit code), by the judge (a fresh read-only agent from another provider than
//! the work, reading the result against the line), or both, as `goal.check` says. Only code
//! decides from what they found: a line the judge can't tell about isn't met.
//!
//! [`judge`] is the same check for the run's own goal, on the landing branch, after the
//! project's checks.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::goal::{
    CheckRun, CheckedBy, Criterion, CriterionState, CriterionVerdict, GoalChecked, GoalEvent,
    GoalVerdict,
};
use argonauts_core::events::task::CheckOutcome;
use argonauts_core::goal::{CheckWith, GoalSpec, THE_JUDGE, YOUR_CHECKS, argv};
use argonauts_core::limits::CheckRequest;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::protocol::CheckCommand;
use argonauts_core::steps::StepName;
use argonauts_core::steps::pursue::{CheckGoal, CheckGoalInput, GoalCheck};

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::merge::final_check::{FinalCheck, run_base};
use crate::merge::{diff, merge_packet, step};
use crate::prompts::goal::CHECK;
use crate::roles;
use crate::state::RunState;
use crate::workflow::{StepAt, StepCall, change};

use super::pursue;

/// What the check reads of the diff: enough for a few files, and the judge can read the rest.
const DIFF_BYTES: usize = 60_000;
const CHECK_BYTES: usize = 4_000;
/// What a check's run keeps of a command's output, where what failed is named.
const OUTPUT_BYTES: usize = 2_000;
/// Used when `checks.timeout` doesn't parse or says `none`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The name a goal's own commands run under when the goal belongs to no task.
const GOAL_SCOPE: &str = "goal";

/// Where a goal is checked, and what the check may take as given.
#[derive(Debug, Clone)]
pub struct Place {
    /// Where the work is: the commands run here, and the judge reads here.
    pub dir: PathBuf,
    /// The commit the work started from, for the diff the judge reads. `None` gives it none,
    /// and it reads the files.
    pub base: Option<String>,
    /// The task whose goal it is. `None` for a run's or a workflow's.
    pub task: Option<TaskId>,
    /// The step that asked, which places the judge's session in the workflow view.
    pub at: Option<StepAt>,
    /// The try this check follows, from 1.
    pub try_number: u32,
    /// Who wrote the work, so the judge is someone else.
    pub authors: Vec<ModelRef>,
    /// The project's own checks, when they already ran here: they aren't run twice, and the
    /// goal doesn't hold while they fail.
    pub project: Option<FinalCheck>,
}

impl Place {
    /// A check of try `try_number` in `dir`, with nothing taken as given.
    pub fn new(dir: impl Into<PathBuf>, try_number: u32) -> Self {
        Self {
            dir: dir.into(),
            base: None,
            task: None,
            at: None,
            try_number,
            authors: Vec::new(),
            project: None,
        }
    }
}

/// What a check needs from wherever it runs: the engine inside a run, or the conversation when the
/// main agent works alone.
pub trait Means: Sync {
    /// The project's own check commands, `checks.run`.
    fn project_commands(&self) -> Vec<CheckCommand>;
    /// Runs one command in the place and says how it ended.
    fn run(
        &self,
        command: &CheckCommand,
    ) -> impl Future<Output = EngineResult<CheckOutcome>> + Send;
    /// What the work changed, for the judge. Empty when nothing can say.
    fn diff(&self) -> impl Future<Output = String> + Send;
    /// Asks the judge about the lines in `input`, and says who judged.
    fn judge(
        &self,
        input: CheckGoalInput,
    ) -> impl Future<Output = EngineResult<(GoalCheck, ModelRef)>> + Send;
}

/// Checks `goal` in `place`, inside a run: every command that checks a line runs once, then the
/// judge reads the lines left to it. Records `Goal::Checked` and returns it, each line met or
/// not with what still fails.
pub async fn goal(host: &Host, goal: &GoalSpec, place: &Place) -> EngineResult<GoalChecked> {
    let checked = unrecorded(host, goal, place).await?;
    host.record(
        place.task.as_ref(),
        None,
        RunEventKind::Goal(GoalEvent::Checked(Box::new(checked.clone()))),
    )?;
    Ok(checked)
}

/// The same check with nothing recorded, for one that decides nothing: a check you asked for
/// while a try is still at work.
pub async fn unrecorded(host: &Host, goal: &GoalSpec, place: &Place) -> EngineResult<GoalChecked> {
    with(&InRun { host, place }, goal, place).await
}

/// The check itself, with whatever `means` runs the commands and asks the judge.
pub async fn with<M: Means>(
    means: &M,
    goal: &GoalSpec,
    place: &Place,
) -> EngineResult<GoalChecked> {
    let mut found = Found::default();
    let project_ran = project_checks(means, goal, place, &mut found).await?;
    own_commands(means, goal, &mut found).await?;
    let to_judge = left_to_the_judge(goal, project_ran, &found);
    let mut impossible = None;
    if !to_judge.is_empty() {
        impossible = ask_the_judge(means, goal, place, &to_judge, &mut found).await?;
    }
    let verdicts: Vec<CriterionVerdict> = goal
        .lines
        .iter()
        .map(|line| {
            found
                .lines
                .remove(&line.id)
                .unwrap_or_else(|| CriterionVerdict {
                    id: line.id.clone(),
                    verdict: CriterionState::CantTell,
                    evidence: String::new(),
                    missing: "nothing checked it".to_owned(),
                })
        })
        .collect();
    let project_passed = place.project.as_ref().is_none_or(|project| project.passed);
    let met = project_passed
        && verdicts
            .iter()
            .all(|verdict| verdict.verdict == CriterionState::Met);
    let overall = match (met, &impossible) {
        (true, _) => GoalVerdict::Met,
        (false, Some(_)) => GoalVerdict::Impossible,
        (false, None) => GoalVerdict::NotYet,
    };
    let failed_project = place.project.as_ref().filter(|project| !project.passed);
    let reason = match (&impossible, failed_project) {
        (Some(why), _) => why.clone(),
        (None, Some(project)) => failed_checks(project),
        (None, None) => still_fails(&goal.lines, &verdicts),
    };
    Ok(GoalChecked {
        try_number: place.try_number,
        verdicts,
        overall,
        reason,
        task: place.task.clone(),
        checks: found.runs,
    })
}

/// What a check has found so far.
#[derive(Default)]
struct Found {
    lines: BTreeMap<String, CriterionVerdict>,
    runs: Vec<CheckRun>,
}

impl Found {
    fn decide(&mut self, id: &str, met: bool, proof: &str, missing: &str) {
        let verdict = CriterionVerdict {
            id: id.to_owned(),
            verdict: if met {
                CriterionState::Met
            } else {
                CriterionState::NotYet
            },
            evidence: if met { proof.to_owned() } else { String::new() },
            missing: if met {
                String::new()
            } else {
                missing.to_owned()
            },
        };
        // A line a command already failed stays failed, whatever else passes.
        match self.lines.get(id) {
            Some(earlier) if earlier.verdict != CriterionState::Met => {}
            _ => {
                self.lines.insert(id.to_owned(), verdict);
            }
        }
    }

    fn failed(&self, id: &str) -> bool {
        self.lines
            .get(id)
            .is_some_and(|verdict| verdict.verdict != CriterionState::Met)
    }
}

/// The project's own checks, for the lines that say "the tests pass". Returns whether any ran
/// or were given, so a project with none leaves those lines to the judge.
async fn project_checks<M: Means>(
    means: &M,
    goal: &GoalSpec,
    place: &Place,
    found: &mut Found,
) -> EngineResult<bool> {
    let lines: Vec<String> = goal
        .lines
        .iter()
        .filter(|line| goal.check != CheckWith::Judge && line.check.by_project_checks())
        .map(|line| line.id.clone())
        .collect();
    let outcomes = match &place.project {
        Some(project) => project.checks.clone(),
        None if lines.is_empty() => Vec::new(),
        None => {
            let mut outcomes = Vec::new();
            for command in &means.project_commands() {
                outcomes.push(means.run(command).await?);
            }
            outcomes
        }
    };
    if outcomes.is_empty() {
        return Ok(false);
    }
    let passed = outcomes.iter().all(|outcome| outcome.passed);
    let missing = outcomes
        .iter()
        .filter(|outcome| !outcome.passed)
        .map(|outcome| what_failed(&format!("{YOUR_CHECKS}: {}", outcome.name), outcome))
        .collect::<Vec<_>>()
        .join("; ");
    for id in &lines {
        found.decide(id, passed, &format!("{YOUR_CHECKS} pass"), &missing);
    }
    for outcome in &outcomes {
        let name = format!("{YOUR_CHECKS}: {}", outcome.name);
        found.runs.push(check_run(name, &lines, outcome));
    }
    Ok(true)
}

/// Every command the goal names, once each. A line's own command decides that line; the
/// goal's commands decide every line with no command of its own.
async fn own_commands<M: Means>(means: &M, goal: &GoalSpec, found: &mut Found) -> EngineResult<()> {
    for command in goal.named_commands() {
        let own: Vec<String> = goal
            .lines
            .iter()
            .filter(|line| line.check.command() == Some(command))
            .map(|line| line.id.clone())
            .collect();
        let lines: Vec<String> = if own.is_empty() {
            goal.lines
                .iter()
                .filter(|line| line.check.command().is_none() && !line.check.by_project_checks())
                .map(|line| line.id.clone())
                .collect()
        } else {
            own
        };
        let outcome = match argv(command) {
            Ok(words) => {
                let named = CheckCommand {
                    name: command.to_owned(),
                    command: words,
                };
                means.run(&named).await?
            }
            Err(problem) => unrunnable(command, &problem),
        };
        let missing = what_failed(command, &outcome);
        for id in &lines {
            found.decide(id, outcome.passed, &format!("{command} passes"), &missing);
        }
        found
            .runs
            .push(check_run(command.to_owned(), &lines, &outcome));
    }
    Ok(())
}

/// The lines the judge reads: every one under `judge`, every one no command failed under
/// `both`, and under `auto` the ones nothing else decides.
fn left_to_the_judge(goal: &GoalSpec, project_ran: bool, found: &Found) -> Vec<Criterion> {
    let whole_goal_commands = !goal.commands.is_empty();
    goal.lines
        .iter()
        .filter(|line| match goal.check {
            CheckWith::Judge => true,
            CheckWith::Both => !found.failed(&line.id),
            CheckWith::Command => false,
            CheckWith::Auto => {
                !found.lines.contains_key(&line.id)
                    && !whole_goal_commands
                    && (!line.check.by_project_checks() || !project_ran)
            }
        })
        .cloned()
        .collect()
}

/// Asks the judge about `lines`. Returns why the goal can't be met, when it says so.
async fn ask_the_judge<M: Means>(
    means: &M,
    goal: &GoalSpec,
    place: &Place,
    lines: &[Criterion],
    found: &mut Found,
) -> EngineResult<Option<String>> {
    let ids: Vec<String> = lines.iter().map(|line| line.id.clone()).collect();
    let input = CheckGoalInput {
        goal: goal.condition.clone(),
        criteria: lines.to_vec(),
        diff: means.diff().await,
        check_output: commands_output(&found.runs, place.project.as_ref()),
    };
    let started = std::time::Instant::now();
    let answered = means.judge(input).await;
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (decided, agent) = match answered {
        Ok((answer, judge)) => (decide(place.try_number, lines, &answer, true), Some(judge)),
        Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
        // A judge that couldn't read proves nothing, so nothing it was asked is met yet.
        Err(err) => (
            unchecked(
                place.try_number,
                lines,
                &format!("the judge couldn't read it: {err}"),
            ),
            None,
        ),
    };
    let met = met_count(&decided.verdicts);
    let total = decided.verdicts.len();
    for verdict in &decided.verdicts {
        // Under `both` a command already passed the line; the judge has the last word.
        found.lines.insert(verdict.id.clone(), verdict.clone());
    }
    found.runs.push(CheckRun {
        name: THE_JUDGE.to_owned(),
        by: CheckedBy::Judge,
        lines: ids,
        passed: met == total,
        summary: format!(
            "{met} of {total} {} met",
            if total == 1 { "line" } else { "lines" }
        ),
        failed: None,
        total: None,
        ms,
        output: decided
            .verdicts
            .iter()
            .filter(|verdict| verdict.verdict != CriterionState::Met)
            .map(|verdict| format!("{}: {}", verdict.id, verdict.missing))
            .collect::<Vec<_>>()
            .join("\n"),
        agent,
    });
    Ok((decided.overall == GoalVerdict::Impossible).then_some(decided.reason))
}

/// A check inside a run: commands go through the run's sandboxed runner, and the judge is one
/// of the crew.
struct InRun<'a> {
    host: &'a Host,
    place: &'a Place,
}

impl Means for InRun<'_> {
    fn project_commands(&self) -> Vec<CheckCommand> {
        self.host.settings().checks.run.clone()
    }

    /// One command in the place, inside the sandbox a build has there. Its outcome stays out
    /// of the task's own checks: a goal's line failing is no failed check of the task.
    async fn run(&self, command: &CheckCommand) -> EngineResult<CheckOutcome> {
        let (host, place) = (self.host, self.place);
        let settings = host.settings();
        let scope = match &place.task {
            Some(task) => task.clone(),
            None => TaskId::new(GOAL_SCOPE).map_err(|err| EngineError::Refused(err.to_string()))?,
        };
        let temp = host.repo().run_dir.join("tmp").join(scope.as_str());
        std::fs::create_dir_all(&temp)?;
        let request = CheckRequest {
            run: host.run_id().clone(),
            task: scope,
            name: command.name.clone(),
            command: command.command.clone(),
            workdir: place.dir.clone(),
            sandbox: crate::spec::sandbox(settings, Access::Build, &place.dir, &temp)?,
            timeout: settings
                .checks
                .timeout
                .parse()
                .ok()
                .flatten()
                .unwrap_or(DEFAULT_TIMEOUT),
            setup: false,
        };
        Ok(host.services().checks.run(request).await)
    }

    async fn diff(&self) -> String {
        let Some(base) = &self.place.base else {
            return String::new();
        };
        self.host
            .git()
            .review_diff(&self.place.dir, base, "HEAD")
            .await
            .map(|text| diff::cut(text, DIFF_BYTES))
            .unwrap_or_default()
    }

    async fn judge(&self, input: CheckGoalInput) -> EngineResult<(GoalCheck, ModelRef)> {
        let place = self.place;
        let answered = self
            .host
            .ask::<CheckGoal>(Ask {
                workdir: Some(place.dir.clone()),
                blind: true,
                task: place.task.clone(),
                slot: self
                    .host
                    .read(|state| fresh_slot(state, place.task.as_ref())),
                independent_of: place.authors.clone(),
                ..Ask::new(
                    place.at.clone(),
                    RoleName::Reviewer,
                    checker(self.host, &place.authors),
                    CHECK,
                    input,
                )
            })
            .await?;
        Ok((answered.output, answered.worker))
    }
}

/// A slot no judge of this goal has sat in. The work changes between two checks of one goal,
/// whether they follow two tries or you asked for one in between, so a judge always reads it
/// afresh and never answers with what an earlier one found.
fn fresh_slot(state: &RunState, task: Option<&TaskId>) -> u32 {
    let judged = state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| {
            session.task.as_ref() == task && session.started.step == Some(StepName::CheckGoal)
        })
        .count();
    u32::try_from(judged).unwrap_or(u32::MAX)
}

/// A command that can't be run, as a failed check that says why.
fn unrunnable(command: &str, problem: &str) -> CheckOutcome {
    CheckOutcome {
        name: command.to_owned(),
        kind: argonauts_core::events::task::CheckKind::Command,
        passed: false,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary: problem.to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: false,
        skipped: false,
    }
}

fn check_run(name: String, lines: &[String], outcome: &CheckOutcome) -> CheckRun {
    let (failed, total) = counts(&outcome.summary);
    CheckRun {
        name,
        by: CheckedBy::Command,
        lines: lines.to_vec(),
        passed: outcome.passed,
        summary: outcome.summary.trim().to_owned(),
        failed,
        total,
        ms: outcome.duration_ms,
        output: if outcome.passed {
            String::new()
        } else {
            tail(&outcome.output_tail, OUTPUT_BYTES)
        },
        agent: None,
    }
}

/// `(failed, ran)` from a runner's one-line summary such as `11 passed, 1 failed`.
pub fn counts(summary: &str) -> (Option<u32>, Option<u32>) {
    let number_before = |word: &str| {
        let words: Vec<&str> = summary
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
            .filter(|part| !part.is_empty())
            .collect();
        words
            .windows(2)
            .find(|pair| pair[1].eq_ignore_ascii_case(word))
            .and_then(|pair| pair[0].parse::<u32>().ok())
    };
    let failed = number_before("failed");
    let passed = number_before("passed");
    let total = match (failed, passed) {
        (None, None) => None,
        (failed, passed) => Some(failed.unwrap_or(0) + passed.unwrap_or(0)),
    };
    (failed.or(passed.map(|_| 0)), total)
}

/// `cargo test overdue: 1 of 12 fails`, or the command's own summary when it counts nothing.
fn what_failed(name: &str, outcome: &CheckOutcome) -> String {
    if outcome.passed {
        return String::new();
    }
    match counts(&outcome.summary) {
        (Some(failed), Some(total)) if failed > 0 => format!(
            "{name}: {failed} of {total} {}",
            if failed == 1 { "fails" } else { "fail" }
        ),
        _ => match outcome.summary.trim() {
            "" => format!("{name} failed"),
            summary => format!("{name}: {summary}"),
        },
    }
}

/// What still fails, a line each, for the next try and for the screen.
pub fn still_fails(lines: &[Criterion], verdicts: &[CriterionVerdict]) -> String {
    let failing: Vec<String> = verdicts
        .iter()
        .filter(|verdict| verdict.verdict != CriterionState::Met)
        .map(|verdict| {
            let text = lines
                .iter()
                .find(|line| line.id == verdict.id)
                .map_or(verdict.id.as_str(), |line| line.text.as_str());
            match verdict.missing.trim() {
                "" => text.to_owned(),
                missing => format!("{text}: {missing}"),
            }
        })
        .collect();
    if failing.is_empty() {
        summary(verdicts)
    } else {
        failing.join("\n")
    }
}

/// What the commands printed, for the judge: each one passed or failed, and the end of a failed
/// one's output.
fn commands_output(runs: &[CheckRun], project: Option<&FinalCheck>) -> String {
    let mut text = String::new();
    if let Some(problem) = project.and_then(|project| project.problem.as_ref()) {
        text.push_str(problem);
        text.push('\n');
    }
    for run in runs.iter().filter(|run| run.by == CheckedBy::Command) {
        let mark = if run.passed { "passed" } else { "failed" };
        text.push_str(&format!("{} {mark}: {}\n", run.name, run.summary));
        if !run.output.trim().is_empty() {
            text.push_str(run.output.trim_end());
            text.push('\n');
        }
    }
    if text.is_empty() {
        text.push_str("No check commands ran.\n");
    }
    diff::cut(text, CHECK_BYTES)
}

/// The end of `text`, cut at a line start.
fn tail(text: &str, bytes: usize) -> String {
    let text = text.trim_end();
    if text.len() <= bytes {
        return text.to_owned();
    }
    let mut from = text.len() - bytes;
    while !text.is_char_boundary(from) {
        from += 1;
    }
    match text[from..].find('\n') {
        Some(line) => text[from + line + 1..].to_owned(),
        None => text[from..].to_owned(),
    }
}

/// The run's own goal, checked on the landing branch: the project's checks first, as the merge
/// needs them, then the goal's own commands and the judge.
pub async fn judge(
    host: &Host,
    call: &StepCall,
    try_number: u32,
    goal: &GoalSpec,
) -> EngineResult<GoalChecked> {
    let project = step::checked(host).await?;
    let (_, worktree) = merge_packet::landing_branch(host).await?;
    let place = Place {
        base: run_base(host),
        at: Some(call.at.clone()),
        authors: host.read(|state| authors(state, try_number)),
        project: Some(project),
        ..Place::new(worktree, try_number)
    };
    self::goal(host, goal, &place).await
}

/// Every criterion can't-tell, for `reason`.
fn unchecked(try_number: u32, criteria: &[Criterion], reason: &str) -> GoalChecked {
    GoalChecked {
        try_number,
        verdicts: criteria
            .iter()
            .map(|criterion| CriterionVerdict {
                id: criterion.id.clone(),
                verdict: CriterionState::CantTell,
                evidence: String::new(),
                missing: reason.to_owned(),
            })
            .collect(),
        overall: GoalVerdict::NotYet,
        reason: reason.to_owned(),
        task: None,
        checks: Vec::new(),
    }
}

/// What code decides from the check's answers. A criterion the check left out can't be told,
/// so it isn't met; only every criterion met, with the checks passing, is "met".
pub fn decide(
    try_number: u32,
    criteria: &[Criterion],
    check: &GoalCheck,
    checks_passed: bool,
) -> GoalChecked {
    let verdicts: Vec<CriterionVerdict> = criteria
        .iter()
        .map(|criterion| {
            check
                .verdicts
                .iter()
                .find(|verdict| verdict.id == criterion.id)
                .cloned()
                .unwrap_or_else(|| CriterionVerdict {
                    id: criterion.id.clone(),
                    verdict: CriterionState::CantTell,
                    evidence: String::new(),
                    missing: "the check said nothing about it".to_owned(),
                })
        })
        .collect();
    let met = checks_passed
        && verdicts
            .iter()
            .all(|verdict| verdict.verdict == CriterionState::Met);
    let overall = if met {
        GoalVerdict::Met
    } else if check.overall == GoalVerdict::Impossible {
        GoalVerdict::Impossible
    } else {
        GoalVerdict::NotYet
    };
    let reason = match check.reason.trim() {
        "" => summary(&verdicts),
        reason => reason.to_owned(),
    };
    GoalChecked {
        try_number,
        verdicts,
        overall,
        reason,
        task: None,
        checks: Vec::new(),
    }
}

/// `3 of 4 met; not yet: G4` for a check that gave no reason.
pub fn summary(verdicts: &[CriterionVerdict]) -> String {
    let met = met_count(verdicts);
    let unmet: Vec<&str> = unmet(verdicts).collect();
    if unmet.is_empty() {
        format!("{met} of {} met", verdicts.len())
    } else {
        format!(
            "{met} of {} met; not yet: {}",
            verdicts.len(),
            unmet.join(", ")
        )
    }
}

pub fn met_count(verdicts: &[CriterionVerdict]) -> usize {
    verdicts
        .iter()
        .filter(|verdict| verdict.verdict == CriterionState::Met)
        .count()
}

/// The ids of the criteria not met, can't tell included.
pub fn unmet(verdicts: &[CriterionVerdict]) -> impl Iterator<Item = &str> {
    verdicts
        .iter()
        .filter(|verdict| verdict.verdict != CriterionState::Met)
        .map(|verdict| verdict.id.as_str())
}

/// The judge, always in a fresh session: `goal.checker` when it names one of the crew, else a
/// crew member that wrote none of the work, from another provider than its authors and than
/// the phase's tie-breaker where there is one, else from another provider than its authors, else
/// from any. Only when everyone wrote some does the tie-breaker's own CLI judge. Agents outside
/// the crew and the reviewer role are never picked: they may not be set up to run here.
pub fn checker(host: &Host, authors: &[ModelRef]) -> ModelRef {
    let settings = host.settings();
    let mut pool = settings.crew.members();
    for worker in roles::candidates(settings, RoleName::Reviewer) {
        if !pool.contains(&worker) {
            pool.push(worker);
        }
    }
    let available = host.services().workers.available();
    pool.retain(|worker| available.iter().any(|ready| ready.agent == worker.agent));
    if settings.goal.checker != "other_provider"
        && let Some(named) = pool
            .iter()
            .find(|worker| worker.to_string() == settings.goal.checker)
    {
        return named.clone();
    }
    let tie_breaker = change::tie_breaker(host);
    let wrote: Vec<String> = authors
        .iter()
        .map(|author| roles::provider(host, author))
        .collect();
    let its_provider = roles::provider(host, &tie_breaker);
    let fresh = |worker: &&ModelRef| !authors.contains(worker);
    let apart = |worker: &&ModelRef| !wrote.contains(&roles::provider(host, worker));
    let outside = |worker: &&ModelRef| roles::provider(host, worker) != its_provider;
    pool.iter()
        .find(|worker| fresh(worker) && apart(worker) && outside(worker))
        .or_else(|| pool.iter().find(|worker| fresh(worker) && apart(worker)))
        .or_else(|| pool.iter().find(fresh))
        .cloned()
        .unwrap_or(tie_breaker)
}

/// Who wrote the code a check reads: a goal try's task and its best-of-n tries, or in a
/// workflow every task. Each worker counts, the first and any it fell back to, and every
/// session that built it.
pub(super) fn authors(state: &RunState, try_number: u32) -> Vec<ModelRef> {
    let try_task = pursue::try_task_id(try_number);
    let its_tries = format!("{try_task}-");
    let of_try = |task: &TaskId| task == &try_task || task.as_str().starts_with(&its_tries);
    let checks_a_try = state.tasks.by_id.keys().any(&of_try);
    let checked = |task: &TaskId| !checks_a_try || of_try(task);
    let mut authors: Vec<ModelRef> = Vec::new();
    let workers = state
        .tasks
        .by_id
        .values()
        .filter(|record| checked(&record.spec.id))
        .flat_map(|record| [record.worker.clone(), record.spec.worker.clone()])
        .flatten();
    let builders = state
        .sessions
        .in_order()
        .into_iter()
        .filter(|session| {
            session.started.step == Some(StepName::Build)
                && session.task.as_ref().is_some_and(&checked)
        })
        .map(|session| session.started.worker.clone());
    for model in workers.chain(builders) {
        if !authors.contains(&model) {
            authors.push(model);
        }
    }
    authors
}

fn failed_checks(checks: &FinalCheck) -> String {
    if let Some(problem) = &checks.problem {
        return problem.clone();
    }
    let failed: Vec<String> = checks
        .checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| match check.summary.trim() {
            "" => format!("{} failed", check.name),
            summary => format!("{} failed: {summary}", check.name),
        })
        .collect();
    match failed.as_slice() {
        [] => "the checks failed".to_owned(),
        _ => failed.join("; "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn criteria() -> Vec<Criterion> {
        vec![
            Criterion::new("G1", "slugify collapses dashes"),
            Criterion::new("G2", "median([]) raises ValueError"),
        ]
    }

    fn verdict(id: &str, verdict: CriterionState) -> CriterionVerdict {
        CriterionVerdict {
            id: id.to_owned(),
            verdict,
            evidence: String::new(),
            missing: String::new(),
        }
    }

    fn check(verdicts: Vec<CriterionVerdict>, overall: GoalVerdict) -> GoalCheck {
        GoalCheck {
            verdicts,
            overall,
            reason: String::new(),
        }
    }

    #[test]
    fn only_every_criterion_met_with_the_checks_passing_is_met() {
        let all = vec![
            verdict("G1", CriterionState::Met),
            verdict("G2", CriterionState::Met),
        ];
        let met = decide(
            1,
            &criteria(),
            &check(all.clone(), GoalVerdict::NotYet),
            true,
        );
        assert_eq!(met.overall, GoalVerdict::Met);
        let checks_failed = decide(1, &criteria(), &check(all, GoalVerdict::Met), false);
        assert_eq!(checks_failed.overall, GoalVerdict::NotYet);
    }

    #[test]
    fn cant_tell_and_silence_are_not_yet() {
        let unsure = vec![
            verdict("G1", CriterionState::Met),
            verdict("G2", CriterionState::CantTell),
        ];
        let checked = decide(2, &criteria(), &check(unsure, GoalVerdict::Met), true);
        assert_eq!(checked.overall, GoalVerdict::NotYet);
        assert_eq!(checked.reason, "1 of 2 met; not yet: G2");

        let silent = decide(
            2,
            &criteria(),
            &check(vec![verdict("G1", CriterionState::Met)], GoalVerdict::Met),
            true,
        );
        assert_eq!(silent.verdicts[1].verdict, CriterionState::CantTell);
        assert_eq!(silent.overall, GoalVerdict::NotYet);
    }

    #[test]
    fn the_check_may_say_it_cant_be_met() {
        let checked = decide(
            1,
            &criteria(),
            &check(
                vec![verdict("G1", CriterionState::Met)],
                GoalVerdict::Impossible,
            ),
            true,
        );
        assert_eq!(checked.overall, GoalVerdict::Impossible);
    }
}
