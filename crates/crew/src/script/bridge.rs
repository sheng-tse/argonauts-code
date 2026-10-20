//! Where a script's calls meet the engine: an `agent()` becomes a session on the CLI and model
//! its role names, a `check()` goes to whatever keeps goals, each finished call is saved, and
//! each report is recorded for the views.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::{CliWorkflowRun, EndReason, InputPartKind};
use argonauts_core::events::workflow::{
    AgentLeftOut, AgentSession, RunNote, WorkflowEvent, WorkflowReport,
};
use argonauts_core::log::{GAVE_NO_ANSWER, Note, NoteLevel};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{AgentCall, AgentEnd, Check, Checked, JournalEntry, Kept, Script};
use futures::future::BoxFuture;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

mod beside;

use super::{goal, journal, tasks};
use crate::engine::host::{Fallbacks, Host, Ran, SessionPlan};
use crate::engine::job_fallback;
use crate::error::{EngineError, EngineResult, Stall};
use crate::prompts::instructions::{self, CallAuthor};
use crate::roles;
use crate::task::support::part;
use crate::workflow::StepCall;

/// As much of a task's change as a reviewer is handed in its prompt. It reads the rest in the
/// task's worktree.
const DIFF_SHOWN: usize = 60_000;

pub(super) struct Bridge<'a> {
    pub host: &'a Host,
    pub call: &'a StepCall,
    /// Cancelled when the engine itself fails or the run is interrupted, which ends the script.
    stop: CancellationToken,
    error: Mutex<Option<EngineError>>,
    pub tasks: tasks::Book,
    /// The session behind each call, by the call's number, which a run that picks its script
    /// up again asks by.
    sessions: Mutex<BTreeMap<u32, SessionKey>>,
    /// The call behind each row of the report in this stretch of the run.
    rows: Mutex<BTreeMap<u32, u32>>,
    /// The size the request asked for, when the orchestrator wrote the steps to keep to it.
    pub size_asked: Option<String>,
    /// The most tries a file of steps allows: the whole file under `None`, and each step by
    /// its name.
    pub max_rounds: BTreeMap<Option<String>, u32>,
    /// The workflow whose own words the calls are, by its name. `None` while the orchestrator
    /// wrote the script for this request.
    pub workflow_words: Option<String>,
    /// The steps of the file in use whose workers may run a workflow of their own. A script
    /// the orchestrator wrote can't ask for that itself, so the file says it.
    pub own_workflow_in: BTreeSet<String>,
    /// Each phase that started an agent, with the role it went to.
    ran: Mutex<BTreeSet<(String, CrewRole)>>,
    /// The agents of each phase and role: who works now, who answered, and who was left out.
    phases: Mutex<BTreeMap<(Option<String>, CrewRole), PhaseAgents>>,
    /// The jobs that went to a second agent of a CLI in their step, or were asked to, by label.
    second_tried: Mutex<BTreeSet<String>>,
    /// An agent that writes has started, so the reading before the building is over.
    wrote: AtomicBool,
}

/// One phase's agents of one role, by label.
#[derive(Debug, Default)]
struct PhaseAgents {
    working: BTreeSet<String>,
    answered: BTreeSet<String>,
    left_out: Vec<LeftOut>,
}

/// A job nothing could take, with what its CLI said and that CLI's name.
#[derive(Debug, Clone)]
struct LeftOut {
    label: String,
    said: String,
    cli: String,
    tried: Vec<String>,
}

/// What a job that can be left out is in its phase, and what it gives: a proposer a
/// proposal, a reviewer a review.
fn job_words(call: &AgentCall) -> (&'static str, &'static str) {
    match call.role {
        CrewRole::Reviewer => ("reviewer", "review"),
        _ => ("proposer", "proposal"),
    }
}

/// `two`, as a count reads in a sentence. Past twelve, digits.
fn count_in_words(count: usize) -> String {
    const WORDS: [&str; 13] = [
        "none", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    WORDS
        .get(count)
        .map_or_else(|| count.to_string(), |word| (*word).to_owned())
}

impl<'a> Bridge<'a> {
    /// The steps you require that started no agent of their role in their phase.
    pub fn never_ran(&self, required: &[argonauts_core::required::RequiredStep]) -> Vec<String> {
        let ran = lock(&self.ran);
        required
            .iter()
            .filter(|step| {
                !ran.iter().any(|(phase, role)| {
                    *role == step.role && argonauts_core::required::same_name(phase, &step.name)
                })
            })
            .map(|step| step.name.clone())
            .collect()
    }

    pub fn new(host: &'a Host, call: &'a StepCall, stop: CancellationToken) -> Self {
        Self {
            host,
            call,
            stop,
            error: Mutex::default(),
            tasks: tasks::Book::open(journal::place(host, call, "tasks.json")),
            sessions: Mutex::new(journal::sessions(host, call)),
            rows: Mutex::default(),
            size_asked: None,
            max_rounds: BTreeMap::new(),
            workflow_words: None,
            own_workflow_in: BTreeSet::new(),
            ran: Mutex::default(),
            phases: Mutex::default(),
            second_tried: Mutex::default(),
            wrote: AtomicBool::new(false),
        }
    }

    /// Who wrote what `call` asks: the orchestrator, or the workflow at the call's step.
    pub fn author<'c>(&'c self, call: &'c AgentCall) -> CallAuthor<'c> {
        match &self.workflow_words {
            Some(name) => CallAuthor::Workflow {
                name,
                step: call.phase.as_deref(),
            },
            None => CallAuthor::Orchestrator,
        }
    }

    /// The engine error that ended the script, when one did.
    pub fn error(&self) -> Option<EngineError> {
        lock(&self.error).take()
    }

    pub fn take_error(&self) -> EngineError {
        self.error().unwrap_or(EngineError::Interrupted)
    }

    /// Notes the session behind an agent, for the report now and after a restart.
    pub fn ran(&self, call: &AgentCall, session: &SessionKey) -> EngineResult<()> {
        lock(&self.rows).insert(call.row, call.index);
        let mut sessions = lock(&self.sessions);
        sessions.insert(call.index, session.clone());
        journal::keep_sessions(self.host, self.call, &sessions)
    }

    /// The session behind each row that has one yet, in the rows' order.
    fn row_sessions(&self) -> Vec<AgentSession> {
        let sessions = lock(&self.sessions);
        lock(&self.rows)
            .iter()
            .filter_map(|(row, call)| {
                Some(AgentSession {
                    agent: *row,
                    session: sessions.get(call)?.clone(),
                })
            })
            .collect()
    }

    /// An error of the engine's own stops the script, since nothing after it can be trusted:
    /// the step ends with it and runs again when the run resumes. Anything else is that one
    /// agent failing, which the script is told as a missing answer.
    fn failed(&self, err: EngineError) -> AgentEnd {
        let ends_the_step = matches!(
            err,
            EngineError::Interrupted
                | EngineError::Stalled(_)
                | EngineError::Journal(_)
                | EngineError::Io(_)
                | EngineError::Git(_)
        );
        let why = err.to_string();
        if ends_the_step {
            let mut first = lock(&self.error);
            if first.is_none() {
                *first = Some(err);
            }
            self.stop.cancel();
        }
        AgentEnd::failed(why)
    }

    /// Leaves `call`'s job out when nothing can take it and enough of its phase's others
    /// answered or still work for the step to go on, saying so once in the run and under the
    /// workflow's header. `None` when the run needs the job.
    fn leave_out(&self, call: &AgentCall, stall: &Stall) -> Option<AgentEnd> {
        let fewest = self.fewest_answers(call)?;
        let others = {
            let mut phases = lock(&self.phases);
            let agents = phases.entry((call.phase.clone(), call.role)).or_default();
            let others = agents.working.len() + agents.answered.len();
            if others < fewest {
                return None;
            }
            agents.left_out.push(LeftOut {
                label: call.label.clone(),
                said: stall.job.said.clone(),
                cli: cli_of(stall),
                tried: stall.tried.clone(),
            });
            others
        };
        let goes_on = if self.is_council() {
            format!("the council goes on with {others}")
        } else {
            format!("the workflow goes on with {others}")
        };
        let line = format!(
            "{} is left out: {} and nothing else can take it · {goes_on}",
            call.label, stall.job.said
        );
        let (acts_as, gives) = job_words(call);
        let whole = if self.is_council() {
            "a council"
        } else {
            "the workflow"
        };
        let again = "the next run asks it again";
        let means = vec![
            format!(
                "{whole} can do without one {gives}, so it goes on with {}",
                count_in_words(others)
            ),
            if stall.job.signed_out {
                format!("sign in to {} and {again}", cli_of(stall))
            } else {
                again.to_owned()
            },
        ];
        let host = self.host;
        let left = AgentLeftOut {
            step: self.call.at.step.clone(),
            label: call.label.clone(),
            meant_for: stall
                .job
                .asked
                .clone()
                .unwrap_or_else(|| ModelRef::new("", None)),
            why: stall.job.short.clone(),
            said: stall.job.said.clone(),
            acts_as: acts_as.to_owned(),
            means,
        };
        let recorded = host
            .record(
                None,
                None,
                RunEventKind::Workflow(WorkflowEvent::LeftOut(left)),
            )
            .and_then(|_| {
                let note = RunNote {
                    text: line.clone(),
                    workflow: Some(self.call.at.step.workflow.clone()),
                    left: false,
                };
                host.record(
                    None,
                    None,
                    RunEventKind::Workflow(WorkflowEvent::Note(note)),
                )
            })
            .and_then(|_| {
                let note = Note {
                    level: NoteLevel::Warn,
                    text: line.clone(),
                };
                host.record(None, None, RunEventKind::Note(note))
            });
        if let Err(err) = recorded {
            return Some(self.failed(err));
        }
        Some(AgentEnd::failed(line))
    }

    /// How many of its phase's agents of the same role must answer for `call`'s job to be
    /// left out when nothing can take it: two proposals in a council's Propose step, as two make
    /// a choice, and one review beside a reviewer's. Any other job, such as a planner's or a
    /// cross-review a worker reads, the run needs. `None` when the job can't be left out.
    fn fewest_answers(&self, call: &AgentCall) -> Option<usize> {
        match call.role {
            CrewRole::Reviewer => Some(1),
            CrewRole::Worker if self.is_council() && self.proposes(call) => Some(2),
            _ => None,
        }
    }

    /// Whether `call` proposes: a worker that only reads, works on no task, and starts before any
    /// agent writes. A council's phases are named for each request, so this is told by what
    /// the agent does.
    fn proposes(&self, call: &AgentCall) -> bool {
        call.role == CrewRole::Worker
            && !call.writes
            && call.task.is_none()
            && !self.wrote.load(Ordering::Relaxed)
    }

    /// The workflow in use is a council, whose proposals the stop and its lines name.
    fn is_council(&self) -> bool {
        self.host.settings().workflow == "council"
    }

    /// The run's stop when `call`'s job can't be left out: why, in the step's own words where
    /// it can do with fewer, and with every job of its phase left out before.
    fn stop_with_left_out(&self, call: &AgentCall, mut stall: Box<Stall>) -> Box<Stall> {
        let phases = lock(&self.phases);
        let Some(agents) = phases.get(&(call.phase.clone(), call.role)) else {
            return stall;
        };
        let mut stop = stall.run_stop();
        let this = LeftOut {
            label: call.label.clone(),
            said: stall.job.said.clone(),
            cli: cli_of(&stall),
            tried: stall.tried.clone(),
        };
        let mut out: Vec<&LeftOut> = agents.left_out.iter().chain([&this]).collect();
        out.sort_by(|a, b| a.label.cmp(&b.label));
        stop.agents = out
            .iter()
            .map(|left| format!("{} · {}", left.label, left.said))
            .collect();
        stop.tried = Vec::new();
        let in_step = stall.in_step.clone();
        stop.waits_for = Vec::new();
        for left in &out {
            for tried in &left.tried {
                if !stop.tried.contains(tried) {
                    stop.tried.push(tried.clone());
                }
            }
            if !stop.waits_for.contains(&left.cli) {
                stop.waits_for.push(left.cli.clone());
            }
        }
        stop.tried = crate::error::with_step(&stop.tried, &in_step);
        let mut kept: Vec<String> = agents.answered.union(&agents.working).cloned().collect();
        kept.sort();
        if let Some(fewest) = self.fewest_answers(call) {
            let only = argonauts_core::text::and_list(&kept);
            let makes = if kept.len() == 1 {
                "can make one"
            } else {
                "can make them"
            };
            stop.why = match fewest {
                2 if self.is_council() => {
                    format!("a council needs two proposals, and only {only} {makes}")
                }
                _ => format!(
                    "{} needs {fewest} answers, and only {only} {makes}",
                    call.phase.as_deref().unwrap_or("the step")
                ),
            };
        } else {
            stop.why = format!("nothing else can take {}", call.label);
            stop.agents = out
                .iter()
                .map(|left| format!("{} · {}", left.label, left.said))
                .collect();
        }
        stop.kept = kept;
        stall.stop = Some(Box::new(stop));
        stall
    }

    async fn run_agent(&self, call: &AgentCall) -> EngineResult<AgentEnd> {
        let worker = self.pick(call)?;
        if call.writes {
            return tasks::work(self, call, &worker).await;
        }
        let mut whole = None;
        let (workdir, change) = match tasks::change_of(self, call).await? {
            Some((tree, change)) => (tree, Some(change)),
            // A reviewer or a judge about no one task reads the run's whole change, as text and
            // in its folder, so the files it opens and the tests it runs are the ones it judges.
            None if call.task.is_none() && !self.builders(call).is_empty() => {
                let workdir = match tasks::whole_tree(self, call.index).await? {
                    tasks::Whole::Tree(tree) => {
                        let path = tree.path.clone();
                        whole = Some(tree);
                        path
                    }
                    tasks::Whole::Nothing => self.host.repo().base_worktree.clone(),
                    // Judging the base in place of a change it can't hold would judge the wrong
                    // files, so the call fails, saying why.
                    tasks::Whole::Conflict(why) => return Ok(AgentEnd::failed(why)),
                };
                (workdir, tasks::combined_change(self).await?)
            }
            None => (self.host.repo().base_worktree.clone(), None),
        };
        let reviews = change.is_some();
        let mut parts = vec![instructions::of_call(
            self.host,
            &call.prompt,
            self.author(call),
        )];
        if let Some(change) = change {
            parts.push(part(InputPartKind::Evidence, "the change", change, None));
        }
        // What the checks said after the build, so a reviewer or a judge reads the results
        // rather than guessing at them. One about no task, as a sign-off is, reads them all.
        let checks = match call.task.as_deref() {
            Some(task) => tasks::checks_of(self, Some(task)),
            None if matches!(call.role.fine_role(), RoleName::Judge | RoleName::Reviewer) => {
                tasks::all_checks(self)
            }
            None => None,
        };
        if let Some(checks) = checks {
            parts.push(part(InputPartKind::CheckOutput, "the checks", checks, None));
        }
        let plan = SessionPlan {
            at: Some(self.call.at.clone()),
            role: call.role.fine_role(),
            worker,
            task: None,
            access: Access::ReadOnly,
            step: None,
            workdir,
            parts,
            json_schema: call.schema.clone(),
            resume: None,
            slot: 0,
            label: Some(call.label.clone()),
            reviews,
            fresh: false,
            independent_of: self.builders(call),
        };
        // The session this call started before the run stopped carries on.
        let known = lock(&self.sessions).get(&call.index).cloned();
        let keep = |key: &SessionKey| self.ran(call, key);
        let ran = self
            .host
            .run_session_in_turn(plan, known.as_ref(), Fallbacks::Allowed, &keep)
            .await;
        if let Some(whole) = whole {
            whole.done(self).await;
        }
        let ran = ran?;
        // It may have fallen back to another agent, under a session of its own.
        self.ran(call, &ran.key)?;
        let end = answer(call, &ran);
        if let Ok(verdict) = &end.result {
            tasks::judged(self, call, &ran, verdict)?;
        }
        Ok(end)
    }

    /// Who built what a reviewer or a judge reads, so a fallback for it never comes from their
    /// provider: the task's worker, or every writer's for a call about no one task.
    fn builders(&self, call: &AgentCall) -> Vec<ModelRef> {
        if !matches!(call.role, CrewRole::Reviewer | CrewRole::Judge) {
            return Vec::new();
        }
        if call.task.is_some() {
            return tasks::author(self, call).into_iter().collect();
        }
        tasks::authors(self)
    }

    /// Who takes the call: the model it names, else the one of its role's models it numbers.
    /// A reviewer or a judge of a task comes from another provider than its author where the
    /// role has one, and a call that mustn't be the author never is.
    fn pick(&self, call: &AgentCall) -> EngineResult<ModelRef> {
        if let Some(model) = &call.model {
            return model
                .parse()
                .map_err(|err| EngineError::Refused(format!("{}: {err}", call.label)));
        }
        let host = self.host;
        let mut models = roles::candidates(host.settings(), call.role.fine_role());
        let available = host.services().workers.available();
        if models.iter().any(|model| available.contains(model)) {
            models.retain(|model| available.contains(model));
        }
        if let Some(author) = tasks::author(self, call) {
            if call.not_the_author && models.iter().any(|model| *model != author) {
                models.retain(|model| *model != author);
            }
            if matches!(call.role, CrewRole::Reviewer | CrewRole::Judge) || call.not_the_author {
                let theirs = roles::provider(host, &author);
                // Stable, so the list's own order still decides among the others.
                models.sort_by_key(|model| roles::provider(host, model) == theirs);
            }
        }
        let nth = call.model_index as usize % models.len().max(1);
        models.into_iter().nth(nth).ok_or_else(|| {
            EngineError::Refused(format!(
                "no model is set for the {} to take {}",
                call.role, call.label
            ))
        })
    }
}

impl argonauts_runner::Host for Bridge<'_> {
    fn agent<'b>(&'b self, call: &'b AgentCall) -> BoxFuture<'b, AgentEnd> {
        Box::pin(async move {
            if call.writes {
                self.wrote.store(true, Ordering::Relaxed);
            }
            if let Some(phase) = &call.phase {
                lock(&self.ran).insert((phase.clone(), call.role));
            }
            let place = (call.phase.clone(), call.role);
            lock(&self.phases)
                .entry(place.clone())
                .or_default()
                .working
                .insert(call.label.clone());
            let ran = self.run_agent(call).await;
            {
                let mut phases = lock(&self.phases);
                let agents = phases.entry(place).or_default();
                agents.working.remove(&call.label);
                if matches!(&ran, Ok(end) if end.result.is_ok()) {
                    agents.answered.insert(call.label.clone());
                }
            }
            let end = match ran {
                Ok(end) => end,
                Err(EngineError::Stalled(mut stall)) => {
                    if let Ok(own) = self.pick(call) {
                        job_fallback::named_by_its_own(self.host, &mut stall, &own);
                    }
                    match self.leave_out(call, &stall) {
                        Some(left) => return left,
                        None => match self.beside(call, stall).await {
                            Ok(end) => end,
                            Err(stall) => self
                                .failed(EngineError::Stalled(self.stop_with_left_out(call, stall))),
                        },
                    }
                }
                Err(err) => self.failed(err),
            };
            // The script only sees a missing answer, so the reason is said here.
            if let Err(why) = &end.result {
                let note = Note {
                    level: NoteLevel::Warn,
                    text: format!("{}{GAVE_NO_ANSWER}{why}", call.label),
                };
                let _ = self.host.record(None, None, RunEventKind::Note(note));
            }
            end
        })
    }

    fn check<'b>(&'b self, check: &'b Check) -> BoxFuture<'b, Checked> {
        Box::pin(async move {
            // A check about no one task, as a whole run's goal is, runs on the whole change.
            let mut whole = None;
            let workdir = match tasks::tree_of(self, check.task.as_deref()).await {
                Ok(Some(tree)) => tree,
                _ if check.task.is_none() => match tasks::whole_tree(self, check.index).await {
                    Ok(tasks::Whole::Tree(tree)) => {
                        let path = tree.path.clone();
                        whole = Some(tree);
                        path
                    }
                    Ok(tasks::Whole::Nothing) => self.host.repo().base_worktree.clone(),
                    Ok(tasks::Whole::Conflict(why)) => {
                        return Checked {
                            holds: false,
                            failed: format!("the check couldn't run: {why}"),
                            stop: true,
                        };
                    }
                    Err(err) => {
                        let why = self.failed(err).result.err().unwrap_or_default();
                        return Checked {
                            holds: false,
                            failed: format!("the check couldn't run: {why}"),
                            stop: true,
                        };
                    }
                },
                _ => self.host.repo().base_worktree.clone(),
            };
            let task = tasks::task_of(self, check.task.as_deref());
            let max_rounds = self.max_rounds.get(&check.step).copied();
            let checked = goal::check(
                self.host,
                self.call,
                check,
                task.as_ref(),
                max_rounds,
                &workdir,
            );
            let checked = checked.await;
            if let Some(whole) = whole {
                whole.done(self).await;
            }
            match checked {
                Ok(checked) => checked,
                Err(err) => {
                    let why = self.failed(err).result.err().unwrap_or_default();
                    // A check that couldn't run settles nothing, so the try isn't repeated on it.
                    Checked {
                        holds: false,
                        failed: format!("the check couldn't run: {why}"),
                        stop: true,
                    }
                }
            }
        })
    }

    fn workflow(&self, name: &str) -> Result<Script, String> {
        super::workflow_named(self.host, name)
    }

    fn replayed(&self, call: &AgentCall) {
        if call.writes {
            self.wrote.store(true, Ordering::Relaxed);
        }
        lock(&self.rows).insert(call.row, call.index);
        // It answered before the run stopped, which counts as its phase goes on without one.
        lock(&self.phases)
            .entry((call.phase.clone(), call.role))
            .or_default()
            .answered
            .insert(call.label.clone());
    }

    fn report(&self, workflow: &CliWorkflowRun) {
        // A row's prompt and answer are its session's to show, and would make every report
        // as long as the whole run's talk.
        let mut workflow = workflow.clone();
        for agent in &mut workflow.agents {
            agent.prompt = None;
            agent.result = None;
        }
        let report = WorkflowReport {
            step: self.call.at.step.clone(),
            workflow,
            sessions: self.row_sessions(),
            size_asked: self.size_asked.clone(),
        };
        let event = RunEventKind::Workflow(WorkflowEvent::Reported(Box::new(report)));
        if let Err(err) = self.host.record(None, None, event) {
            self.failed(err);
        }
    }

    fn log(&self, message: &str) {
        let note = RunNote {
            text: message.to_owned(),
            workflow: Some(self.call.workflow().clone()),
            left: false,
        };
        let event = RunEventKind::Workflow(WorkflowEvent::Note(note));
        if let Err(err) = self.host.record(None, None, event) {
            self.failed(err);
        }
    }

    fn keep(&self, kept: &Kept) {
        if let Err(err) = self.tasks.keep(kept) {
            self.failed(err);
        }
    }

    fn journal(&self, entry: &JournalEntry) {
        if let Err(err) = journal::append(self.host, self.call, entry) {
            self.failed(err);
        }
    }

    fn tokens_spent(&self) -> u64 {
        self.host.read(|state| {
            state
                .sessions
                .by_key
                .values()
                .filter_map(|session| session.end.as_ref())
                .map(|end| end.spend.input_tokens + end.spend.output_tokens)
                .sum()
        })
    }

    fn now_ms(&self) -> u64 {
        let since = self.host.now() - time::OffsetDateTime::UNIX_EPOCH;
        u64::try_from(since.whole_milliseconds()).unwrap_or_default()
    }
}

/// What the session said, as the script's `agent()` returns it.
pub(super) fn answer(call: &AgentCall, ran: &Ran) -> AgentEnd {
    let end = &ran.end;
    let said = end
        .result_text
        .as_deref()
        .or(end.last_message.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let result = if end.reason != EndReason::Completed {
        // The same plain cause the agent's own row gives, such as "Codex refused the answer
        // format Argonauts Code asked for".
        let cli = argonauts_core::display_names::DisplayNames::default().cli(&ran.worker.agent);
        Err(match end.reason.plain_line(&cli) {
            Some(why) => format!("{}: {why}", call.label),
            None => format!(
                "{} ended early: {}",
                call.label,
                crate::status::folder::words(&end.reason)
            ),
        })
    } else if let Some(schema) = &call.schema {
        end.structured
            .clone()
            .or_else(|| said.and_then(|text| serde_json::from_str(text).ok()))
            .ok_or_else(|| {
                format!(
                    "{} ended without the answer its schema asks for",
                    call.label
                )
            })
            .and_then(|value| fits(schema, value, &call.label))
    } else {
        Ok(Value::String(said.unwrap_or_default().to_owned()))
    };
    AgentEnd {
        result,
        model: Some(roles::display_name(&ran.worker)),
        tokens: end.spend.input_tokens + end.spend.output_tokens,
        tool_calls: u32::try_from(end.tool_calls).unwrap_or(u32::MAX),
        duration_ms: end.duration_ms,
    }
}

/// An answer that lacks something its schema requires is no answer. The CLIs that enforce a
/// schema never send one, and the others are held to this much.
fn fits(schema: &Value, value: Value, label: &str) -> Result<Value, String> {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let missing = required
        .iter()
        .filter_map(Value::as_str)
        .find(|key| value.get(key).is_none());
    match missing {
        Some(key) => Err(format!("{label}'s answer has no {key}")),
        None => Ok(value),
    }
}

pub(super) fn shown_change(diff: String) -> String {
    if diff.len() <= DIFF_SHOWN {
        return diff;
    }
    let mut cut = DIFF_SHOWN;
    while !diff.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n… the change goes on. Read the rest in this folder, which is the task's worktree.",
        &diff[..cut]
    )
}

/// The name of the CLI a stalled job was asked of.
fn cli_of(stall: &Stall) -> String {
    let names = argonauts_core::display_names::DisplayNames::default();
    stall
        .job
        .asked
        .as_ref()
        .map(|asked| names.cli(asked.agent.as_str()))
        .unwrap_or_default()
}

/// A panic elsewhere while the lock was held leaves the data as it was, which is still right
/// to read.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
