//! Where a script's calls meet the engine: an `agent()` becomes a session on the CLI and model
//! its role names, a `check()` goes to whatever keeps goals, each finished call is saved, and
//! each report is recorded for the views.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use argonauts_core::events::RunEventKind;
use argonauts_core::events::session::{CliWorkflowRun, EndReason, InputPartKind};
use argonauts_core::events::workflow::{AgentSession, RunNote, WorkflowEvent, WorkflowReport};
use argonauts_core::log::{Note, NoteLevel};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::Access;
use argonauts_core::settings::crew::CrewRole;
use argonauts_runner::{AgentCall, AgentEnd, Check, Checked, JournalEntry, Kept, Script};
use futures::future::BoxFuture;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{goal, journal, tasks};
use crate::engine::host::{Fallbacks, Host, Ran, SessionPlan};
use crate::error::{EngineError, EngineResult};
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

    async fn run_agent(&self, call: &AgentCall) -> EngineResult<AgentEnd> {
        let worker = self.pick(call)?;
        if call.writes {
            return tasks::work(self, call, &worker).await;
        }
        let (workdir, change) = match tasks::change_of(self, call).await? {
            Some((tree, change)) => (tree, Some(change)),
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
        };
        // The session this call started before the run stopped carries on.
        let known = lock(&self.sessions).get(&call.index).cloned();
        let keep = |key: &SessionKey| self.ran(call, key);
        let ran = self
            .host
            .run_session_in_turn(plan, known.as_ref(), Fallbacks::Allowed, &keep)
            .await?;
        // It may have fallen back to another agent, under a session of its own.
        self.ran(call, &ran.key)?;
        let end = answer(call, &ran);
        if let Ok(verdict) = &end.result {
            tasks::judged(self, call, &ran, verdict)?;
        }
        Ok(end)
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
            if let Some(phase) = &call.phase {
                lock(&self.ran).insert((phase.clone(), call.role));
            }
            let end = match self.run_agent(call).await {
                Ok(end) => end,
                Err(err) => self.failed(err),
            };
            // The script only sees a missing answer, so the reason is said here.
            if let Err(why) = &end.result {
                let note = Note {
                    level: NoteLevel::Warn,
                    text: format!("{} gave no answer: {why}", call.label),
                };
                let _ = self.host.record(None, None, RunEventKind::Note(note));
            }
            end
        })
    }

    fn check<'b>(&'b self, check: &'b Check) -> BoxFuture<'b, Checked> {
        Box::pin(async move {
            let workdir = match tasks::tree_of(self, check.task.as_deref()).await {
                Ok(Some(tree)) => tree,
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
            match checked.await {
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
        lock(&self.rows).insert(call.row, call.index);
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

/// A panic elsewhere while the lock was held leaves the data as it was, which is still right
/// to read.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
