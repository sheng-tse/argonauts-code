//! Claude Code's own workflow for a crew. When the orchestrator and
//! the workers are on Claude Code, Claude Code plans the workflow and runs it with its own
//! subagents, exactly as it does by itself, in one task's worktree. Argonauts Code gives that
//! session a tool for the reviewers and one for the judge, answers each call by running that
//! role on its own model against the change in the worktree, and before anything merges checks
//! that the final change went through them. When it didn't, Argonauts Code asks them itself.
//! Steps you require and a task list you fixed go with the request; Argonauts Code can't change
//! Claude Code's workflow, so what it reported is checked against them and what it lacked is
//! said.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Mutex, MutexGuard, PoisonError};

use argonauts_core::TaskId;
use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::run::account_in;
use argonauts_core::events::session::InputPartKind;
use argonauts_core::events::task::{
    CheckOutcome, ReviewEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
};
use argonauts_core::events::workflow::{RunNote, WorkflowEvent};
use argonauts_core::labels;
use argonauts_core::names::ModelRef;
use argonauts_core::required::{Required, brief, check};
use argonauts_core::session_runner::{Access, Asking, SessionSpec};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::sha256_hex;
use argonauts_core::steps::judgment::{ReviewComment, ReviewOutput, ReviewVerdict};
use argonauts_core::steps::plan::Severity;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::engine::host::{Fallbacks, Host, SessionPlan};
use crate::error::{EngineError, EngineResult};
use crate::roles::{display_name, provider};
use crate::task::machine::Trigger;
use crate::task::pipeline::{self, Until};
use crate::task::support::part;
use crate::task::{self, checks, worktree};
use crate::ultra::OwnCrew;
use crate::workflow::{StepAt, StepCall, StepDone};

/// What the task's Claude Code session gets: ultracode for this one request, the tools that
/// ask the crew's other roles, and its subagents on the workers' model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnSession {
    pub asking: Asking,
    pub subagent_model: Option<String>,
}

impl OwnSession {
    pub fn apply(&self, spec: &mut SessionSpec) {
        spec.cli_workflow.ultracode = true;
        spec.asking = Some(self.asking.clone());
        if let Some(model) = &self.subagent_model {
            spec.env.push((SUBAGENT_MODEL.to_owned(), model.clone()));
        }
    }
}

/// Claude Code's default model for the agents it starts, workflows' included (2.1.293).
const SUBAGENT_MODEL: &str = "CLAUDE_CODE_SUBAGENT_MODEL";

/// A review asked through the tools, and the change it read.
#[derive(Debug, Clone)]
struct Reviewed {
    role: CrewRole,
    change: String,
    accepted: bool,
    /// The answers of the role's models, as the asking agent read them.
    said: String,
}

/// One task's asks while its session works.
struct Asks<'a> {
    host: &'a Host,
    at: &'a StepAt,
    task: &'a TaskId,
    /// The task's name, which its labels carry.
    name: &'a str,
    crew: &'a OwnCrew,
    request: &'a str,
    token: String,
    reviewed: Mutex<Vec<Reviewed>>,
    /// How many times each label was asked, for its rounds.
    rounds: Mutex<BTreeMap<String, u32>>,
}

/// What the reviewers made of the final change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Verdict {
    accepted: bool,
    /// What they said of it when they didn't accept it.
    reasons: Vec<String>,
}

/// Runs the request as Claude Code's own workflow, then merges what the reviewers accepted.
/// A final change they don't accept goes back to the session with their reasons, for as many
/// rounds of fixes as `review.max_change_rounds` allows, before it comes to you.
pub async fn run(
    host: &Host,
    call: &StepCall,
    request: &str,
    crew: &OwnCrew,
) -> EngineResult<StepDone> {
    let at = &call.at;
    let crew = &asked(host, crew);
    // Named for the work, as the run is: a request the main session handed over opens with
    // your words, which may say how to run it and not what it is.
    let name = labels::id_slug(account_in(request).unwrap_or(request), 4);
    let task =
        TaskId::new(format!("task-{name}")).map_err(|err| EngineError::Refused(err.to_string()))?;
    // What its labels carry: "build:add-due-dates", "review:add-due-dates".
    let item = labels::slug(&name, 3);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let asks = Asks {
        host,
        at,
        task: &task,
        name: &item,
        crew,
        request,
        token: format!("{:032x}", rand::random::<u128>()),
        reviewed: Mutex::default(),
        rounds: Mutex::default(),
    };
    note(host, call, format!("{name} · {}", engine_line(crew)))?;
    let required =
        crate::script::required::shared(&crate::script::required::for_run(host, request));
    if host.read(|state| state.tasks.get(&task).is_none()) {
        task::create(host, spec_for(host, &task, &name, request, crew, &required))?;
    }
    let roles = if crew.judge.is_empty() {
        vec![CrewRole::Reviewer]
    } else {
        vec![CrewRole::Reviewer, CrewRole::Judge]
    };
    let own = OwnSession {
        asking: Asking {
            asks: argonauts_core::Asks::Never,
            endpoint: listener.local_addr()?.to_string(),
            token: asks.token.clone(),
            tools: Vec::new(),
            standing: None,
            roles,
        },
        subagent_model: crew.worker.model.clone(),
    };
    host.shared().work_as_claude_codes_own(&task, own);
    let reviewed = review_rounds(host, &task, at, &listener, &asks).await;
    host.shared().done_as_claude_codes_own(&task);
    let (state, verdict, rounds) = reviewed?;
    if state != TaskState::InReview {
        let state = format!("{state:?}").to_lowercase();
        let said = argonauts_core::text::state_words(&state);
        return Ok(StepDone::done(format!(
            "{name} stopped before its checks passed: it is {said}"
        )));
    }
    let lacked = lacked(host, &task, &required);
    if !lacked.is_empty() {
        note_left(host, call, lacked.join(" "))?;
    }
    let lacked: String = lacked.iter().map(|line| format!(" · {line}")).collect();
    let (trigger, why) = if verdict.accepted {
        (
            Trigger::Approved,
            "the crew's reviewers accepted the final change".to_owned(),
        )
    } else {
        (
            Trigger::ChangesRequestedAgain,
            not_accepted(rounds, &verdict),
        )
    };
    pipeline::transition(host, &task, trigger, &why)?;
    let merged = pipeline::run_task(host, &task, at).await? == TaskState::Merged;
    Ok(StepDone::done(if merged {
        format!("{name} ran as Claude Code's own workflow · merged into the run{lacked}")
    } else {
        format!("{name} ran as Claude Code's own workflow · {why}{lacked}")
    }))
}

/// Drives the task as far as its checks, then has the reviewers read its final change; one
/// they don't accept goes back for a round of fixes while rounds are left. Returns where the
/// task stopped, the last verdict, and how many rounds of fixes it had.
async fn review_rounds(
    host: &Host,
    task: &TaskId,
    at: &StepAt,
    listener: &TcpListener,
    asks: &Asks<'_>,
) -> EngineResult<(TaskState, Verdict, u32)> {
    let most = host.settings().review.max_change_rounds;
    let mut state = serving(
        listener,
        asks,
        pipeline::drive(host, task, at, Until::ChecksPassed),
    )
    .await?;
    let mut rounds = 0;
    loop {
        if state != TaskState::InReview {
            return Ok((state, Verdict::default(), rounds));
        }
        let verdict = serving(listener, asks, gone_through(asks)).await?;
        if verdict.accepted || rounds >= most {
            return Ok((state, verdict, rounds));
        }
        rounds += 1;
        let note = fixes_asked(rounds, &verdict.reasons);
        state = serving(listener, asks, pipeline::revise(host, task, at, &note)).await?;
    }
}

/// Answers the session's asks while `work` runs.
async fn serving<T>(
    listener: &TcpListener,
    asks: &Asks<'_>,
    work: impl std::future::Future<Output = T>,
) -> T {
    tokio::select! {
        done = work => done,
        never = serve(listener, asks) => match never {},
    }
}

/// What the session reads when its final change goes back: the reviewers' own reasons.
fn fixes_asked(round: u32, reasons: &[String]) -> String {
    format!(
        "Round {round} of fixes: the crew's reviewers didn't accept the final change.\n\n{}\n\n\
         Fix what they found, ask them again about the change, and when that is done report \
         again with argonauts_report.",
        reasons.join("\n\n")
    )
}

/// Why the change comes to you, in the reviewers' words.
fn not_accepted(rounds: u32, verdict: &Verdict) -> String {
    let after = match rounds {
        0 => String::new(),
        1 => " after a round of fixes".to_owned(),
        n => format!(" after {n} rounds of fixes"),
    };
    let first = verdict
        .reasons
        .first()
        .and_then(|said| said.lines().next())
        .map(|line| format!(": {line}"))
        .unwrap_or_default();
    format!("the crew's reviewers didn't accept the final change{after}{first}")
}

/// What the workflow Claude Code reported lacked of what you required, in plain lines: its
/// phases' names against the steps, and its agents' labels against a task list you fixed.
/// One that reported no workflow lacked all of it.
fn lacked(host: &Host, task: &TaskId, required: &Required) -> Vec<String> {
    if required.is_empty() {
        return Vec::new();
    }
    let reported = host.read(|state| {
        state
            .sessions
            .in_order()
            .into_iter()
            .filter(|session| session.task.as_ref() == Some(task))
            .filter_map(|session| session.workflow.clone())
            .next_back()
    });
    let Some(workflow) = reported else {
        let listed = check::tasks(required, &[]);
        return check::said(&check::steps(required, &[]), listed.as_ref());
    };
    let phases: Vec<&str> = workflow
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    let found = check::steps(required, &phases);
    // A required step's agents work on that step, not on a task of the list.
    let in_a_required_step = |phase: Option<u32>| {
        workflow
            .phases
            .iter()
            .find(|known| Some(known.index) == phase)
            .is_some_and(|known| required.step(&known.title).is_some())
    };
    // A task's builder and its reviewer share what follows the colon of their labels.
    let task_of = |label: &str| label.rsplit(':').next().unwrap_or(label).to_owned();
    let mut labels: Vec<&str> = Vec::new();
    for agent in &workflow.agents {
        let listed_task = task_of(&agent.label);
        if !in_a_required_step(agent.phase)
            && !labels.iter().any(|known| task_of(known) == listed_task)
        {
            labels.push(&agent.label);
        }
    }
    let listed = check::tasks(required, &labels);
    check::said(&found, listed.as_ref())
}

/// "Claude Code's own workflow · workers on Opus 5.5 · reviews by Astra", which says which
/// engine runs it.
pub fn engine_line(crew: &OwnCrew) -> String {
    let mut line = "Claude Code's own workflow".to_owned();
    if crew.worker.model.is_some() {
        line.push_str(&format!(" · workers on {}", display_name(&crew.worker)));
    }
    if !crew.reviewer.is_empty() {
        line.push_str(&format!(" · reviews by {}", names(&crew.reviewer)));
    }
    line
}

fn names(models: &[ModelRef]) -> String {
    models
        .iter()
        .map(display_name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The crew with only the models its asks go to: `review.per_task` reviewers and one judge,
/// chosen as the runner chooses a task's.
fn asked(host: &Host, crew: &OwnCrew) -> OwnCrew {
    let available = host.services().workers.available();
    let theirs = provider(host, &crew.worker);
    let the_authors = |model: &ModelRef| provider(host, model) == theirs;
    let per_task = host.settings().review.per_task as usize;
    OwnCrew {
        reviewer: asked_models(&crew.reviewer, &available, &the_authors, per_task),
        judge: asked_models(&crew.judge, &available, &the_authors, 1),
        ..crew.clone()
    }
}

/// `wanted` of `models`: the agents that can work now, those of another provider than the
/// work's author first, and the list's own order among the rest.
fn asked_models(
    models: &[ModelRef],
    available: &[ModelRef],
    the_authors: &dyn Fn(&ModelRef) -> bool,
    wanted: usize,
) -> Vec<ModelRef> {
    let mut models = models.to_vec();
    if models.iter().any(|model| available.contains(model)) {
        models.retain(|model| available.contains(model));
    }
    // Stable, so the list's order still decides among each kind.
    models.sort_by_key(|model| the_authors(model));
    models.truncate(wanted.max(1));
    models
}

fn note(host: &Host, call: &StepCall, text: String) -> EngineResult<()> {
    record_note(host, call, text, false)
}

/// What it lacked of what you required: said now, and listed again in the run's closing
/// answer, since Argonauts Code can't add it to Claude Code's workflow.
pub(crate) fn note_left(host: &Host, call: &StepCall, text: String) -> EngineResult<()> {
    record_note(host, call, text, true)
}

fn record_note(host: &Host, call: &StepCall, text: String, left: bool) -> EngineResult<()> {
    let note = RunNote {
        text,
        workflow: Some(call.workflow().clone()),
        left,
    };
    host.record(
        None,
        None,
        RunEventKind::Workflow(WorkflowEvent::Note(note)),
    )?;
    Ok(())
}

/// The one task the workflow works in: the request, owned by the orchestrator. Its
/// review comes through the tools, so the engine's own stays out.
fn spec_for(
    host: &Host,
    task: &TaskId,
    name: &str,
    request: &str,
    crew: &OwnCrew,
    required: &Required,
) -> TaskSpec {
    let criteria = host.read(|state| {
        state
            .goal
            .approved
            .as_ref()
            .map(|goal| goal.criteria.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default()
    });
    TaskSpec {
        id: task.clone(),
        title: format!("build:{}", labels::slug(name, 3)),
        task: instructions(request, crew, required),
        kind: TaskKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria,
        worker: Some(crew.orchestrator.clone()),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: Some(labels::slug(name, 3)),
    }
}

/// What the orchestrator's session reads besides the request: run it as a workflow of its own,
/// with the steps you require and the tasks you fixed, and send each review and the sign-off
/// to the crew's roles through the tools.
fn instructions(request: &str, crew: &OwnCrew, required: &Required) -> String {
    let mut text = format!(
        "{request}\n\nRun this as a workflow of your own with the Workflow tool: plan its \
         phases and let its agents do the work."
    );
    if crew.worker.model.is_some() {
        text.push_str(&format!(
            " Your agents run on {}.",
            display_name(&crew.worker)
        ));
    }
    if let Some(yours) = brief::for_claude_code(required) {
        text.push_str(&format!("\n\n{}", yours.trim_end()));
    }
    text.push_str(&format!(
        "\n\nThe crew's reviewers are {}. At each review step, the agent that reviews calls \
         mcp__argonauts_status__argonauts_ask_reviewer with what the change is meant to do, \
         instead of reviewing it itself, and what they find is fixed. Before you finish, ask \
         them once more about the final change: nothing merges that didn't go through them.",
        names(&crew.reviewer)
    ));
    if !crew.judge.is_empty() {
        text.push_str(&format!(
            " Then ask the judge, {}, to sign off with \
             mcp__argonauts_status__argonauts_ask_judge.",
            names(&crew.judge)
        ));
    }
    text.push_str(
        "\nAn agent that doesn't see one of those tools loads it with ToolSearch, such as \
         select:mcp__argonauts_status__argonauts_ask_reviewer.",
    );
    text.push_str(REPORT_WHEN_DONE);
    text
}

/// The workflow runs in the background past the session's first turn, so its report waits.
const REPORT_WHEN_DONE: &str = "\n\nYour workflow runs in the background. When it has ended, and \
not before, call mcp__argonauts_status__argonauts_report once with your report of the whole task. \
A report sent while the workflow still runs isn't taken.";

/// Answers asks until the task's session is over, several at once.
async fn serve(listener: &TcpListener, asks: &Asks<'_>) -> Infallible {
    let mut answering = FuturesUnordered::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    answering.push(answer(stream, asks));
                }
            }
            Some(()) = answering.next(), if !answering.is_empty() => {}
        }
    }
}

async fn answer(stream: TcpStream, asks: &Asks<'_>) {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    if BufReader::new(read).read_line(&mut line).await.is_err() {
        return;
    }
    let reply = match serde_json::from_str::<AskLine>(line.trim()) {
        Ok(asked) if asked.token == asks.token => match asked.ask {
            Ask::Role { role, what, .. } => match asks.ask(role, &what).await {
                Ok(text) => AskReply::text(text),
                Err(err) => AskReply {
                    text: format!("Argonauts Code couldn't ask them: {err}"),
                    is_error: true,
                },
            },
            _ => AskReply {
                text: "This session can only ask the crew's roles.".to_owned(),
                is_error: true,
            },
        },
        _ => AskReply {
            text: "Argonauts Code didn't take that ask.".to_owned(),
            is_error: true,
        },
    };
    if let Ok(text) = serde_json::to_string(&reply) {
        let _ = write.write_all(format!("{text}\n").as_bytes()).await;
    }
}

impl Asks<'_> {
    /// The checks run on the change as it is now, then the role's models each read it with
    /// their results, and their answers come back as one text. Each answer is recorded as a
    /// review of the task, under the reviewer's name.
    async fn ask(&self, role: CrewRole, what: &str) -> EngineResult<String> {
        let host = self.host;
        let models = match role {
            CrewRole::Judge if !self.crew.judge.is_empty() => &self.crew.judge,
            CrewRole::Judge | CrewRole::Reviewer => &self.crew.reviewer,
            other => {
                return Err(EngineError::Refused(format!(
                    "the {other} aren't asked this way"
                )));
            }
        };
        let change = change_of(host, self.task).await?;
        if change.trim().is_empty() {
            return Ok("There is no change in the folder yet, so nothing was reviewed.".to_owned());
        }
        let tree = worktree::ensure(host, self.task).await?;
        let checks = checks_now(host, self.task).await?;
        let (label, round) = self.round_of(role);
        let asked = Asked {
            role,
            label: &label,
            round,
            what,
            change: &change,
            checks: checks.as_deref(),
        };
        let mut answers = Vec::new();
        let mut accepted = true;
        for model in models {
            let (yes, said) = self.one(model, &asked, &tree.path).await?;
            accepted &= yes;
            answers.push(said);
        }
        let said = answers.join("\n\n");
        lock(&self.reviewed).push(Reviewed {
            role,
            change: sha256_hex(change.as_bytes()),
            accepted,
            said: said.clone(),
        });
        Ok(said)
    }

    /// The ask's label, from the role and the task's name whatever the session called it, and
    /// which round of it this is: "review:add-due-dates", then "review:add-due-dates · round 2".
    /// So every ask about the task, the session's own and the last one Argonauts Code makes, folds
    /// into one row per role.
    fn round_of(&self, role: CrewRole) -> (String, u32) {
        let label = format!("{}:{}", role_word(role), self.name);
        let mut rounds = lock(&self.rounds);
        let round = rounds.entry(label.clone()).or_default();
        *round += 1;
        let shown = match *round {
            1 => label,
            n => format!("{label} · round {n}"),
        };
        (shown, *round)
    }

    async fn one(
        &self,
        model: &ModelRef,
        asked: &Asked<'_>,
        workdir: &std::path::Path,
    ) -> EngineResult<(bool, String)> {
        let host = self.host;
        let role = asked.role;
        let mut parts = vec![
            part(InputPartKind::Prompt, role_word(role), asked_of(role), None),
            part(InputPartKind::Goal, "the request", self.request, None),
        ];
        if !asked.what.trim().is_empty() {
            parts.push(part(
                InputPartKind::Message,
                "what it does",
                asked.what,
                None,
            ));
        }
        if let Some(checks) = asked.checks {
            parts.push(part(InputPartKind::CheckOutput, "the checks", checks, None));
        }
        parts.push(part(
            InputPartKind::Evidence,
            "the change",
            asked.change,
            None,
        ));
        let plan = SessionPlan {
            at: Some(self.at.clone()),
            role: role.fine_role(),
            worker: model.clone(),
            task: None,
            access: Access::ReadOnly,
            step: None,
            workdir: workdir.to_path_buf(),
            parts,
            json_schema: Some(verdict_schema()),
            resume: None,
            slot: 0,
            label: Some(asked.label.to_owned()),
            reviews: false,
            fresh: false,
        };
        let ran = host
            .run_session_in_turn(plan, None, Fallbacks::Allowed, &|_| Ok(()))
            .await?;
        let answer = ran.end.structured.clone().unwrap_or(Value::Null);
        let review = review_of(&answer);
        let accepted = review.verdict == ReviewVerdict::Approve;
        let event = ReviewEvent::Reviewed {
            reviewer: ran.worker.clone(),
            blind: false,
            round: asked.round,
            review: review.clone(),
        };
        host.record(Some(self.task), Some(&ran.key), RunEventKind::Review(event))?;
        Ok((accepted, said(&ran.worker, &review)))
    }
}

/// One ask as each of its role's models reads it.
struct Asked<'a> {
    role: CrewRole,
    label: &'a str,
    round: u32,
    what: &'a str,
    change: &'a str,
    checks: Option<&'a str>,
}

/// The task's change as its reviewers read it: what its commit will hold, as a diff, empty
/// when nothing changed yet.
async fn change_of(host: &Host, task: &TaskId) -> EngineResult<String> {
    Ok(crate::change::of_task(host, task).await?.diff)
}

/// Runs each check on a clean copy of the task's change as it is now, committed or not, so a
/// review reads what they found rather than guessing, and nothing they write reaches the
/// folder the agents work in. `None` when no check is set.
async fn checks_now(host: &Host, task: &TaskId) -> EngineResult<Option<String>> {
    let commands = &host.settings().checks.run;
    if commands.is_empty() {
        return Ok(None);
    }
    let (copy, label) = checks::clean_copy_of_change(host, task).await?;
    if let Some(output) = checks::setup(host, task, &copy, &label).await? {
        return Ok(Some(format!("checks.setup failed: {output}")));
    }
    let mut lines = Vec::new();
    for command in commands {
        let request = checks::request(host, task, command, &copy, false)?;
        let mut outcome = host.services().checks.run(request).await;
        outcome
            .command
            .get_or_insert_with(|| command.command.clone());
        lines.push(checked(&outcome));
    }
    Ok(Some(lines.join("\n\n")))
}

/// "tests (python3 -m unittest -q) passed: Ran 8 tests · OK", as the runner words a check for
/// its reviewers, with the output's end when it failed.
fn checked(outcome: &CheckOutcome) -> String {
    let mut line = checks::result_line(outcome);
    let tail = outcome.output_tail.trim();
    if !outcome.passed && !tail.is_empty() {
        line.push_str(&format!("\n{tail}"));
    }
    line
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Once the session is over: when the final change went through the reviewers (and the judge, when
/// it has models of its own) and they accepted it, it may merge. When it didn't, Argonauts Code
/// asks them now, so nothing merges unreviewed.
async fn gone_through(asks: &Asks<'_>) -> EngineResult<Verdict> {
    let last = sha256_hex(change_of(asks.host, asks.task).await?.as_bytes());
    let mut roles = vec![CrewRole::Reviewer];
    if !asks.crew.judge.is_empty() {
        roles.push(CrewRole::Judge);
    }
    let mut verdict = Verdict {
        accepted: true,
        reasons: Vec::new(),
    };
    for role in roles {
        let read_last = |reviewed: &Reviewed| reviewed.role == role && reviewed.change == last;
        let mut reviewed = lock(&asks.reviewed)
            .iter()
            .rev()
            .find(|r| read_last(r))
            .cloned();
        if reviewed.is_none() {
            asks.ask(role, "The final change, before it merges.")
                .await?;
            reviewed = lock(&asks.reviewed).last().cloned();
        }
        match reviewed {
            Some(reviewed) if reviewed.accepted => {}
            Some(reviewed) => {
                verdict.accepted = false;
                verdict.reasons.push(reviewed.said);
            }
            None => verdict.accepted = false,
        }
    }
    Ok(verdict)
}

fn role_word(role: CrewRole) -> &'static str {
    match role {
        CrewRole::Judge => "sign-off",
        _ => "review",
    }
}

/// What a reviewer or the judge is asked about a change, here and from the main session.
pub fn asked_of(role: CrewRole) -> &'static str {
    match role {
        CrewRole::Judge => {
            "Decide whether the change below is ready to merge for the request. Set accept to \
             true to sign it off. In findings, list what must change first, if anything."
        }
        _ => {
            "Review the change below for the request. Set accept to true when it does what \
             was asked and nothing in it is wrong. In findings, list each thing that must be \
             fixed, one per item. Summary is one line."
        }
    }
}

/// The answer a reviewer or the judge gives: accept, a one-line summary, and findings.
pub fn verdict_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "accept": {"type": "boolean"},
            "summary": {"type": ["string", "null"]},
            "findings": {"type": ["array", "null"], "items": {"type": "string"}},
        },
        "required": ["accept", "summary", "findings"],
        "additionalProperties": false,
    })
}

/// The answer as a review, an unreadable one read as not accepted.
pub fn review_of(answer: &Value) -> ReviewOutput {
    let accept = answer.get("accept").and_then(Value::as_bool) == Some(true);
    let findings: Vec<String> = answer
        .get("findings")
        .and_then(Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let summary = answer
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map_or_else(
            || if accept { "Accepted" } else { "Not accepted" }.to_owned(),
            str::to_owned,
        );
    ReviewOutput {
        verdict: if accept {
            ReviewVerdict::Approve
        } else {
            ReviewVerdict::RequestChanges
        },
        summary,
        comments: findings
            .into_iter()
            .map(|text| ReviewComment {
                path: String::new(),
                line: None,
                severity: if accept {
                    Severity::Low
                } else {
                    Severity::High
                },
                text,
            })
            .collect(),
        tamper_suspected: false,
    }
}

/// The answer as the asking agent reads it.
pub fn said(reviewer: &ModelRef, review: &ReviewOutput) -> String {
    let who = display_name(reviewer);
    let mut text = match review.verdict {
        ReviewVerdict::Approve => format!("{who} accepts it: {}", review.summary),
        _ => format!("{who} asks for changes: {}", review.summary),
    };
    for comment in &review.comments {
        text.push_str(&format!("\n- {}", comment.text));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crew(judge: &[&str]) -> OwnCrew {
        OwnCrew {
            orchestrator: "claude-code/fable-5-1".parse().unwrap(),
            worker: "claude-code/opus-5-5".parse().unwrap(),
            reviewer: vec!["astra".parse().unwrap()],
            judge: judge.iter().map(|model| model.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn the_orchestrator_is_told_to_send_reviews_through_argonauts() {
        let text = instructions("add due dates to tasks", &crew(&[]), &Required::default());
        assert!(text.starts_with("add due dates to tasks\n\n"));
        assert!(text.contains("Workflow tool"));
        assert!(text.contains("argonauts_ask_reviewer"));
        assert!(!text.contains("argonauts_ask_judge"));
        assert!(text.ends_with("A report sent while the workflow still runs isn't taken."));
        let judged = instructions(
            "add due dates",
            &crew(&["claude-code/opus-5-5"]),
            &Required::default(),
        );
        assert!(judged.contains("argonauts_ask_judge"));
    }

    #[test]
    fn the_steps_you_require_and_your_tasks_go_with_the_request() {
        let required = Required {
            steps: vec![argonauts_core::required::RequiredStep {
                name: "Benchmark".into(),
                role: CrewRole::Worker,
                does: Some("Run the benchmarks.".into()),
                when: Some("last".into()),
            }],
            tasks: vec!["migrate the schema".into()],
        };
        let text = instructions("add due dates", &crew(&[]), &required);
        assert!(
            text.contains(
                "Your workflow must have a phase for each of these steps, by this name, \
                 where it says:\n- Benchmark, last, by the workers: Run the benchmarks."
            ),
            "{text}"
        );
        assert!(
            text.contains("1. migrate the schema (label: migrate-schema)"),
            "{text}"
        );
        // Before the tools, which every agent must read.
        assert!(text.find("Benchmark") < text.find("argonauts_ask_reviewer"));
    }

    /// As the runner picks a task's reviewers: another provider than the author first, the
    /// list's order among the rest, only agents that can work when any can.
    #[test]
    fn asked_models_put_other_providers_first() {
        let models: Vec<ModelRef> = ["claude-code/haiku-4-5", "codex", "antigravity-cli"]
            .iter()
            .map(|model| model.parse().unwrap())
            .collect();
        let claude = |model: &ModelRef| model.agent == "claude-code";
        let picked = asked_models(&models, &[], &claude, 1);
        assert_eq!(picked, [models[1].clone()]);
        let picked = asked_models(&models, &[], &claude, 3);
        assert_eq!(
            picked,
            [models[1].clone(), models[2].clone(), models[0].clone()]
        );
        let picked = asked_models(&models, &[models[0].clone(), models[2].clone()], &claude, 1);
        assert_eq!(picked, [models[2].clone()]);
        assert!(asked_models(&[], &[], &claude, 1).is_empty());
    }

    #[test]
    fn a_change_that_comes_to_you_says_why_in_the_reviewers_words() {
        let verdict = Verdict {
            accepted: false,
            reasons: vec!["Codex asks for changes: Past dates pass\n- a past date".into()],
        };
        assert_eq!(
            not_accepted(2, &verdict),
            "the crew's reviewers didn't accept the final change after 2 rounds of fixes: \
             Codex asks for changes: Past dates pass"
        );
        assert_eq!(
            not_accepted(0, &Verdict::default()),
            "the crew's reviewers didn't accept the final change"
        );
    }

    #[test]
    fn a_check_reads_as_the_runner_words_it() {
        let outcome = |passed: bool, summary: &str| CheckOutcome {
            name: "tests".into(),
            kind: argonauts_core::events::task::CheckKind::Command,
            passed,
            command: Some(vec!["python3".to_owned(), "-m".into(), "unittest".into()]),
            exit_code: Some(i32::from(!passed)),
            duration_ms: 3,
            summary: summary.into(),
            output_tail: "FAIL: test_past_due".into(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        };
        assert_eq!(
            checked(&outcome(true, "Ran 8 tests · OK")),
            "tests (python3 -m unittest) passed: Ran 8 tests · OK"
        );
        assert_eq!(
            checked(&outcome(false, "1 failed")),
            "tests (python3 -m unittest) failed: 1 failed\nFAIL: test_past_due"
        );
        assert_eq!(
            checked(&outcome(true, "passed")),
            "tests (python3 -m unittest) passed",
            "a summary that only says passed isn't said twice"
        );
    }

    #[test]
    fn an_answer_reads_as_a_review() {
        let review = review_of(&json!({"accept": false, "findings": ["past dates pass"]}));
        assert_eq!(review.verdict, ReviewVerdict::RequestChanges);
        assert_eq!(review.summary, "Not accepted");
        let astra: ModelRef = "astra".parse().unwrap();
        assert!(said(&astra, &review).ends_with("\n- past dates pass"));
        assert_eq!(
            review_of(&Value::Null).verdict,
            ReviewVerdict::RequestChanges
        );
    }

    #[test]
    fn a_subagent_model_and_the_tools_reach_the_session() {
        let own = OwnSession {
            asking: Asking {
                asks: argonauts_core::Asks::Never,
                endpoint: "127.0.0.1:9".into(),
                token: "t".into(),
                tools: Vec::new(),
                standing: None,
                roles: vec![CrewRole::Reviewer],
            },
            subagent_model: Some("opus-5-5".into()),
        };
        let mut spec = argonauts_testkit::runner::session_spec(
            "claude-code/fable-5-1",
            Access::Build,
            std::path::Path::new("/tmp/work"),
        );
        own.apply(&mut spec);
        assert!(spec.cli_workflow.ultracode);
        assert_eq!(spec.asking.as_ref().unwrap().roles, [CrewRole::Reviewer]);
        assert!(
            spec.env
                .contains(&(SUBAGENT_MODEL.to_owned(), "opus-5-5".to_owned()))
        );
    }
}
