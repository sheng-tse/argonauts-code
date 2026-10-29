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
use argonauts_core::events::session::{
    AskedBy, CliAgent, CliAgentState, CliWorkflowRun, InputPartKind,
};
use argonauts_core::events::task::{
    CheckOutcome, ReviewEvent, TaskKind, TaskLimit, TaskSpec, TaskState,
};
use argonauts_core::events::workflow::{RunNote, WorkflowEvent};
use argonauts_core::labels;
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::required::{Required, brief, check};
use argonauts_core::session_runner::{Access, Asking, SessionSpec};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::sha256_hex;
use argonauts_core::steps::judgment::{ReviewComment, ReviewOutput, ReviewVerdict};
use argonauts_core::steps::plan::Severity;
use argonauts_core::views::cli_workflow_run_id;
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
            spec.env
                .push((WORKFLOW_SUBAGENT_MODEL.to_owned(), model.clone()));
        }
    }
}

/// Claude Code's default model for the agents it starts, workflows' included (2.1.293).
const SUBAGENT_MODEL: &str = "CLAUDE_CODE_SUBAGENT_MODEL";

/// The model a workflow's agents run on even where its script names another. Without it a
/// script's `model: 'haiku'` can put the workers on the reviewer's model when reviews stay
/// within one provider.
const WORKFLOW_SUBAGENT_MODEL: &str = "CLAUDE_CODE_WORKFLOW_SUBAGENT_MODEL";

/// A review asked through the tools, and the change it read.
#[derive(Debug, Clone)]
struct Reviewed {
    role: CrewRole,
    label: String,
    change: String,
    accepted: bool,
    /// The answers of the role's models, as the asking agent read them.
    said: String,
    /// What each model made of it, under the model's name.
    reviews: Vec<(String, ReviewOutput)>,
    /// What the asking agent said the change does, which answers the round before it.
    what: String,
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
    /// How many times the session's workflow asked each role, which its rounds cap.
    asked: Mutex<BTreeMap<CrewRole, u32>>,
}

/// The asks a task's log holds, as an earlier process kept them while it answered them.
#[derive(Debug, Default)]
struct RecordedAsks {
    reviewed: Vec<Reviewed>,
    rounds: BTreeMap<String, u32>,
    asked: BTreeMap<CrewRole, u32>,
}

/// Every ask recorded for the task: each label's reviews, in the order they came, by the
/// session that gave them. What each one read of the change isn't recorded, so the final change
/// is gone through again before it merges.
fn recorded_asks(state: &crate::state::RunState, task: &TaskId) -> RecordedAsks {
    let mut recorded = RecordedAsks::default();
    let Some(record) = state.tasks.get(task) else {
        return recorded;
    };
    let sessions = &state.tasks.progress(task).review_sessions;
    for (event, key) in record.reviews.iter().zip(sessions) {
        let (
            ReviewEvent::Reviewed {
                reviewer, review, ..
            },
            Some(key),
        ) = (event, key)
        else {
            continue;
        };
        let Some(session) = state.sessions.get(key) else {
            continue;
        };
        let role = match session.started.role {
            RoleName::Reviewer => CrewRole::Reviewer,
            RoleName::Judge => CrewRole::Judge,
            _ => continue,
        };
        let Some(label) = session.started.label.clone() else {
            continue;
        };
        let asked = session
            .asked
            .as_ref()
            .or(session.started.asked.as_ref())
            .unwrap_or(reviewer);
        let who = in_place_of(state, key, asked, reviewer);
        let said = said_by(&who, review);
        let same_ask = recorded
            .reviewed
            .last()
            .is_some_and(|last| last.label == label && last.role == role);
        if same_ask && let Some(last) = recorded.reviewed.last_mut() {
            last.accepted &= review.verdict == ReviewVerdict::Approve;
            last.said = format!("{}\n\n{said}", last.said);
            last.reviews.push((who, review.clone()));
            continue;
        }
        let (base, round) = round_in(&label);
        let rounds = recorded.rounds.entry(base.to_owned()).or_default();
        *rounds = (*rounds).max(round);
        *recorded.asked.entry(role).or_default() += 1;
        recorded.reviewed.push(Reviewed {
            role,
            label,
            change: String::new(),
            accepted: review.verdict == ReviewVerdict::Approve,
            said,
            reviews: vec![(who, review.clone())],
            what: String::new(),
        });
    }
    recorded
}

/// A label and the round it names: `review:money · round 2` is round 2 of `review:money`.
fn round_in(label: &str) -> (&str, u32) {
    label
        .rsplit_once(" · round ")
        .and_then(|(base, round)| Some((base, round.parse().ok()?)))
        .unwrap_or((label, 1))
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
    // A relaunched run carries on the asks its earlier process answered, so the cap, the
    // rounds and the earlier findings hold across it.
    let recorded = host.read(|state| recorded_asks(state, &task));
    let asks = Asks {
        host,
        at,
        task: &task,
        name: &item,
        crew,
        request,
        token: format!("{:032x}", rand::random::<u128>()),
        reviewed: Mutex::new(recorded.reviewed),
        rounds: Mutex::new(recorded.rounds),
        asked: Mutex::new(recorded.asked),
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
        let note = fixes_asked(rounds, &verdict.reasons, asks.left(CrewRole::Reviewer));
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

/// What the session reads when its final change goes back: the reviewers' own reasons, and
/// whether its workflow may still ask them.
fn fixes_asked(round: u32, reasons: &[String], asks_left: bool) -> String {
    let then = if asks_left {
        "Fix what they found, ask them again about the change, and when that is done report \
         again with argonauts_report."
    } else {
        "Fix what they found and report again with argonauts_report. They were asked as many \
         times as Argonauts Code allows, so don't ask them again: Argonauts Code goes through \
         the final change itself."
    };
    format!(
        "Round {round} of fixes: the crew's reviewers didn't accept the final change.\n\n{}\n\n\
         {then}",
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
/// chosen as the runner chooses a task's. The judge comes from another provider than the
/// reviewers where one can sign off, so the sign-off is a second look and not the review again.
fn asked(host: &Host, crew: &OwnCrew) -> OwnCrew {
    let available = host.services().workers.available();
    let theirs = provider(host, &crew.worker);
    let the_authors = |model: &ModelRef| provider(host, model) == theirs;
    let per_task = host.settings().review.per_task as usize;
    let reviewer = asked_models(
        &crew.reviewer,
        &available,
        &|model| (the_authors(model), false),
        per_task,
    );
    let reviewers: Vec<String> = reviewer.iter().map(|model| provider(host, model)).collect();
    let after = |model: &ModelRef| {
        (
            the_authors(model),
            reviewers.contains(&provider(host, model)),
        )
    };
    OwnCrew {
        judge: asked_models(&crew.judge, &available, &after, 1),
        reviewer,
        ..crew.clone()
    }
}

/// `wanted` of `models`: the agents that can work now, ordered by `after` (those of another
/// provider than the work's author first), and the list's own order among the rest.
fn asked_models(
    models: &[ModelRef],
    available: &[ModelRef],
    after: &dyn Fn(&ModelRef) -> (bool, bool),
    wanted: usize,
) -> Vec<ModelRef> {
    let mut models = models.to_vec();
    if models.iter().any(|model| available.contains(model)) {
        models.retain(|model| available.contains(model));
    }
    // Stable, so the list's order still decides among each kind.
    models.sort_by_key(|model| after(model));
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
        task: instructions(request, crew, required, asks_allowed(host)),
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
        way: None,
    }
}

/// What the orchestrator's session reads besides the request: run it as a workflow of its own,
/// with the steps you require and the tasks you fixed, and send each review and the sign-off
/// to the crew's roles through the tools.
fn instructions(request: &str, crew: &OwnCrew, required: &Required, asks: u32) -> String {
    let mut text = format!(
        "{request}\n\nRun this as one workflow of your own with the Workflow tool: plan its \
         phases and let its agents do the work. Start one Workflow call for the whole task, \
         and no second one for reviews or a sign-off."
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
        "\n\nThe crew's reviewers are {}. At the review step, the agent that reviews calls \
         mcp__argonauts_status__argonauts_ask_reviewer with its own label and what the change \
         is meant to do, instead of reviewing it itself, and what they mark blocking is fixed.",
        names(&crew.reviewer)
    ));
    if !crew.judge.is_empty() {
        text.push_str(&format!(
            " Then the judge, {}, signs off through \
             mcp__argonauts_status__argonauts_ask_judge.",
            names(&crew.judge)
        ));
    }
    text.push_str(&format!(
        " Each of them is asked at most {} in this task, and then its last answer stands. \
         Argonauts Code goes through the final change with them itself before anything merges, \
         so don't ask them again for that.",
        times(asks)
    ));
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
            Ask::Role { role, label, what } => {
                match asks.asked_in_the_session(role, &label, &what).await {
                    Ok(text) => AskReply::text(text),
                    Err(err) => AskReply {
                        text: format!("Argonauts Code couldn't ask them: {err}"),
                        is_error: true,
                    },
                }
            }
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

/// How many times the session's workflow may ask each role: one review, and one more for each
/// round of fixes `review.max_change_rounds` allows.
fn asks_allowed(host: &Host) -> u32 {
    host.settings().review.max_change_rounds.saturating_add(1)
}

/// The answer to an ask past the cap: the last answer stands, and the workflow reports.
fn report_now(role: CrewRole, allowed: u32) -> String {
    let who = match role {
        CrewRole::Judge => "the judge",
        _ => "the crew's reviewers",
    };
    format!(
        "Argonauts Code has asked {who} {} about this task, as many as it allows, so it doesn't \
         ask again and their last answer stands. Don't start another review or sign-off: end your \
         workflow and report with mcp__argonauts_status__argonauts_report. Argonauts Code goes \
         through the final change itself before anything merges.",
        times(allowed)
    )
}

fn times(n: u32) -> String {
    match n {
        1 => "once".to_owned(),
        2 => "twice".to_owned(),
        n => format!("{n} times"),
    }
}

impl Asks<'_> {
    /// Whether the session's workflow may still ask `role`.
    fn left(&self, role: CrewRole) -> bool {
        lock(&self.asked).get(&role).copied().unwrap_or_default() < asks_allowed(self.host)
    }

    /// An ask from the session's workflow, answered until the role was asked as many times as
    /// the task's rounds allow. Past that, the last answer stands and the session is told to
    /// report.
    async fn asked_in_the_session(
        &self,
        role: CrewRole,
        label: &str,
        what: &str,
    ) -> EngineResult<String> {
        let allowed = asks_allowed(self.host);
        let within = {
            let mut asked = lock(&self.asked);
            let asked = asked.entry(role).or_default();
            let within = *asked < allowed;
            if within {
                *asked += 1;
            }
            within
        };
        if !within {
            return Ok(report_now(role, allowed));
        }
        self.ask(role, label, what).await
    }

    /// The checks run on the change as it is now, then the role's models each read it with
    /// their results and the earlier rounds' findings, and their answers come back as one
    /// text. Each answer is recorded as a review of the task, under the reviewer's name.
    async fn ask(&self, role: CrewRole, label: &str, what: &str) -> EngineResult<String> {
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
        let (label, round) = self.round_of(role, label);
        let earlier = self.earlier_findings(what);
        let asked = Asked {
            role,
            label: &label,
            round,
            what,
            earlier: earlier.as_deref(),
            change: &change,
            checks: checks.as_deref(),
        };
        let mut answers = Vec::new();
        let mut reviews = Vec::new();
        for model in models {
            let (who, review) = self.one(model, &asked, &tree.path).await?;
            answers.push(said_by(&who, &review));
            reviews.push((who, review));
        }
        let said = answers.join("\n\n");
        let accepted = reviews
            .iter()
            .all(|(_, review)| review.verdict == ReviewVerdict::Approve);
        lock(&self.reviewed).push(Reviewed {
            role,
            label,
            change: sha256_hex(change.as_bytes()),
            accepted,
            said: said.clone(),
            reviews,
            what: what.to_owned(),
        });
        Ok(said)
    }

    /// The ask's label: the one the session's workflow gave it, so its answer fills the row
    /// that asked, else the role and the task's name ("review:add-due-dates"). And which round
    /// of that label this is, which the label says from the second on: "review:money · round 2".
    fn round_of(&self, role: CrewRole, given: &str) -> (String, u32) {
        let label = match given.trim() {
            "" => format!("{}:{}", role_word(role), self.name),
            given => given.to_owned(),
        };
        let mut rounds = lock(&self.rounds);
        let round = rounds.entry(label.clone()).or_default();
        *round += 1;
        let shown = match *round {
            1 => label,
            n => format!("{label} · round {n}"),
        };
        (shown, *round)
    }

    /// What the earlier asks about this task found, oldest first, and how the asking agent
    /// answered each: what it said of the change when it asked next. `what` is this ask's,
    /// which answers the latest. `None` on the first ask.
    fn earlier_findings(&self, what: &str) -> Option<String> {
        let reviewed = lock(&self.reviewed);
        if reviewed.is_empty() {
            return None;
        }
        let mut text =
            "Earlier rounds on this change, oldest first. Judge the change as it is now \
                        against the request; what an answer settled isn't raised again."
                .to_owned();
        for (n, round) in reviewed.iter().enumerate() {
            text.push_str(&format!("\n\n{}. {}", n + 1, round.label));
            for (who, review) in &round.reviews {
                let verdict = if review.verdict == ReviewVerdict::Approve {
                    "accepted it"
                } else {
                    "didn't accept it"
                };
                text.push_str(&format!("\n{who} {verdict}: {}", review.summary));
                for comment in &review.comments {
                    text.push_str(&format!("\n- {}: {}", weight(comment), comment.text));
                }
            }
            let answered = reviewed.get(n + 1).map_or(what, |next| next.what.as_str());
            if !answered.trim().is_empty() {
                text.push_str(&format!("\nAnswered: {}", answered.trim()));
            }
        }
        Some(text)
    }

    /// One model's review of the ask, and who gave it: the model asked, or its fallback, named
    /// as one ("Claude Code, in place of Codex (it crashed)"). A fallback never comes from the
    /// provider of whoever built the work.
    async fn one(
        &self,
        model: &ModelRef,
        asked: &Asked<'_>,
        workdir: &std::path::Path,
    ) -> EngineResult<(String, ReviewOutput)> {
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
        if let Some(earlier) = asked.earlier {
            parts.push(part(
                InputPartKind::EarlierFindings,
                "earlier findings",
                earlier,
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
        let mut builders = vec![self.crew.orchestrator.clone()];
        if !builders.contains(&self.crew.worker) {
            builders.push(self.crew.worker.clone());
        }
        let plan = SessionPlan {
            without_status: false,
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
            independent_of: builders,
            in_place_of: None,
            fork_of: None,
        };
        let ran = host
            .run_session_in_turn(plan, None, Fallbacks::Allowed, &|_| Ok(()))
            .await?;
        let answer = ran.end.structured.clone().unwrap_or(Value::Null);
        let review = review_of(&answer);
        let picked_by =
            crate::review::picked_for(host, role.fine_role(), model, &ran.worker, false);
        let event = ReviewEvent::Reviewed {
            reviewer: ran.worker.clone(),
            blind: false,
            round: asked.round,
            review: review.clone(),
            picked_by: Some(picked_by),
        };
        host.record(Some(self.task), Some(&ran.key), RunEventKind::Review(event))?;
        let who = host.read(|state| in_place_of(state, &ran.key, model, &ran.worker));
        Ok((who, review))
    }
}

/// One ask as each of its role's models reads it.
struct Asked<'a> {
    role: CrewRole,
    label: &'a str,
    round: u32,
    what: &'a str,
    earlier: Option<&'a str>,
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
            asks.ask(role, "", "The final change, before it merges.")
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
            "Decide whether the change below is ready to merge for the request, judged against \
             the request alone. List each finding once and mark it blocking when the change \
             can't merge without it (it leaves out part of the request, or breaks something), \
             else a note. Set accept to true to sign it off when nothing blocking is left."
        }
        _ => {
            "Review the change below for the request, judged against the request alone. List \
             each finding once and mark it blocking when the change can't merge without it (it \
             leaves out part of the request, or breaks something), else a note. Set accept to \
             true when nothing blocking is left. Summary is one line."
        }
    }
}

/// The answer a reviewer or the judge gives: accept, a one-line summary, and findings, each
/// blocking or a note.
pub fn verdict_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "accept": {"type": "boolean"},
            "summary": {"type": ["string", "null"]},
            "findings": {
                "type": ["array", "null"],
                "items": {
                    "type": "object",
                    "properties": {
                        "text": {"type": "string"},
                        "blocking": {"type": "boolean"},
                    },
                    "required": ["text", "blocking"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["accept", "summary", "findings"],
        "additionalProperties": false,
    })
}

/// The answer as a review: accepted when nothing blocking is left. A finding given as plain
/// text blocks unless the answer accepts, and an answer that doesn't accept and names nothing
/// blocks on its summary. An unreadable one reads as not accepted.
pub fn review_of(answer: &Value) -> ReviewOutput {
    let readable = answer.get("accept").and_then(Value::as_bool);
    let accept = readable == Some(true);
    let comments: Vec<ReviewComment> = answer
        .get("findings")
        .and_then(Value::as_array)
        .map(|findings| {
            findings
                .iter()
                .filter_map(|finding| finding_of(finding, accept))
                .collect()
        })
        .unwrap_or_default();
    let blocking = comments
        .iter()
        .any(|comment| comment.severity == Severity::High);
    let approved = readable.is_some() && !blocking && (accept || !comments.is_empty());
    let summary = answer
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
        .map_or_else(
            || if approved { "Accepted" } else { "Not accepted" }.to_owned(),
            str::to_owned,
        );
    ReviewOutput {
        verdict: if approved {
            ReviewVerdict::Approve
        } else {
            ReviewVerdict::RequestChanges
        },
        summary,
        comments,
        tamper_suspected: false,
    }
}

/// One finding: `{"text", "blocking"}`, or plain text, which blocks unless the answer accepts.
/// A blocking finding is high severity and a note low.
fn finding_of(finding: &Value, accept: bool) -> Option<ReviewComment> {
    let (text, blocking) = match finding {
        Value::String(text) => (text.as_str(), !accept),
        Value::Object(_) => (
            finding.get("text").and_then(Value::as_str)?,
            finding.get("blocking").and_then(Value::as_bool) != Some(false),
        ),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| ReviewComment {
        path: String::new(),
        line: None,
        severity: if blocking {
            Severity::High
        } else {
            Severity::Low
        },
        text: text.to_owned(),
    })
}

/// "blocking" or "note", as a finding is marked.
fn weight(comment: &ReviewComment) -> &'static str {
    if comment.severity == Severity::High {
        "blocking"
    } else {
        "note"
    }
}

/// Who gave a review: the model asked, or its fallback, with why the asked one
/// couldn't: "Claude Code, in place of Codex (it crashed)".
fn in_place_of(
    state: &crate::state::RunState,
    key: &argonauts_core::names::SessionKey,
    asked: &ModelRef,
    worker: &ModelRef,
) -> String {
    let who = display_name(worker);
    if worker == asked {
        return who;
    }
    let since = state
        .sessions
        .get(key)
        .map_or(u64::MAX, |record| record.since);
    let why = state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .filter(|record| record.since < since && record.started.worker == *asked)
        .find_map(|record| record.end.as_ref())
        .map(|end| format!(" ({})", crate::engine::job_fallback::in_words(&end.reason)))
        .unwrap_or_default();
    format!("{who}, in place of {}{why}", display_name(asked))
}

/// The answer as the asking agent reads it.
pub fn said(reviewer: &ModelRef, review: &ReviewOutput) -> String {
    said_by(&display_name(reviewer), review)
}

/// [`said`] under a name already worded, such as a fallback's.
fn said_by(who: &str, review: &ReviewOutput) -> String {
    let mut text = match review.verdict {
        ReviewVerdict::Approve => format!("{who} accepts it: {}", review.summary),
        _ => format!("{who} asks for changes: {}", review.summary),
    };
    for comment in &review.comments {
        text.push_str(&format!("\n- {}: {}", weight(comment), comment.text));
    }
    text
}

/// Who asked for the session `plan` starts, when it answers an ask from a task's own CLI
/// workflow: the task's session running that workflow, and the agent its latest report had at
/// work. A session starts as its ask comes, so the state now is the state at the ask. `None`
/// for any other session, and before the workflow's first report.
pub fn asked_by(host: &Host, plan: &SessionPlan) -> Option<AskedBy> {
    let asked = matches!(plan.role, RoleName::Reviewer | RoleName::Judge) && plan.task.is_none();
    let (at, label) = (plan.at.as_ref()?, plan.label.as_deref()?);
    if !asked {
        return None;
    }
    host.read(|state| {
        let asking = state
            .sessions
            .in_order()
            .into_iter()
            .rev()
            .find(|session| {
                session.task.is_some()
                    && session.end.is_none()
                    && session.workflow.is_some()
                    && session.started.workflow_step.as_ref() == Some(&at.step)
            })?;
        let workflow = asking.workflow.as_deref()?;
        Some(asker_in(&asking.key, workflow, plan.role, label))
    })
}

/// The agent of `workflow` that asked under `label`: the one at work, else the one at work
/// whose label is the ask's but for its round, else the one at work named for the ask's role.
/// Its `agent` is `None` when that still leaves none or several.
fn asker_in(
    session: &SessionKey,
    workflow: &CliWorkflowRun,
    role: RoleName,
    label: &str,
) -> AskedBy {
    let at_work: Vec<&CliAgent> = workflow
        .agents
        .iter()
        .filter(|agent| matches!(agent.state, CliAgentState::Running | CliAgentState::Other))
        .collect();
    let word = match role {
        RoleName::Judge => "sign-off",
        _ => "review",
    };
    let only = |found: Vec<&&CliAgent>| match found.as_slice() {
        [one] => Some(one.label.clone()),
        _ => None,
    };
    let agent = only(at_work.iter().collect())
        .or_else(|| {
            only(
                at_work
                    .iter()
                    .filter(|agent| label_stem(&agent.label) == label_stem(label))
                    .collect(),
            )
        })
        .or_else(|| {
            only(
                at_work
                    .iter()
                    .filter(|agent| agent.label.starts_with(word))
                    .collect(),
            )
        });
    AskedBy {
        session: session.clone(),
        workflow: Some(cli_workflow_run_id(&workflow.id)),
        agent,
    }
}

/// A label without the round its script or the crew numbered it by: `review:money` for
/// `review:money#2`, and for the crew's `review:money · round 2`.
pub fn label_stem(label: &str) -> &str {
    fn numbered<'a>((stem, n): (&'a str, &str)) -> Option<&'a str> {
        n.trim().parse::<u32>().is_ok().then_some(stem)
    }
    let label = label.trim();
    label
        .rsplit_once(" · round ")
        .and_then(numbered)
        .or_else(|| label.rsplit_once('#').and_then(numbered))
        .unwrap_or(label)
        .trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::events::session::SessionEvent;
    use argonauts_core::events::task::TaskEvent;

    use crate::state::RunState;
    use crate::state::tests::{envelope, session_started};
    use crate::testutil::task_spec;

    /// Claude Code's workflow as its report gives it, each agent by its label and state.
    fn reported(agents: &[(&str, CliAgentState)]) -> CliWorkflowRun {
        CliWorkflowRun {
            id: "worx86tbc".to_owned(),
            call: None,
            name: "fix-money-rounding".to_owned(),
            description: String::new(),
            phases: Vec::new(),
            agents: agents
                .iter()
                .zip(1..)
                .map(|((label, state), index)| CliAgent {
                    index,
                    label: (*label).to_owned(),
                    state: *state,
                    ..CliAgent::default()
                })
                .collect(),
            status: argonauts_core::events::session::SubagentStatus::Running,
            tokens: 0,
            tool_calls: 0,
            duration_ms: 0,
            summary: None,
            started_at_ms: None,
        }
    }

    fn asker(workflow: &CliWorkflowRun, role: RoleName, label: &str) -> Option<String> {
        asker_in(&SessionKey::numbered(1), workflow, role, label).agent
    }

    /// Haiku's script numbers its agents by round and asks under the bare label, so the label
    /// alone never matched: the agent at work is the one that asked.
    #[test]
    fn the_asker_is_the_one_agent_at_work_whatever_its_label() {
        use CliAgentState::{Done, Running};
        let workflow = reported(&[
            ("build:fix-the-money", Done),
            ("review:fix-the-money#1", Done),
            ("sign-off:fix-the-money#1", Running),
        ]);
        assert_eq!(
            asker(&workflow, RoleName::Judge, "sign-off:fix-the-money").as_deref(),
            Some("sign-off:fix-the-money#1")
        );
        let by = asker_in(
            &SessionKey::numbered(1),
            &workflow,
            RoleName::Judge,
            "anything",
        );
        assert_eq!(by.session, SessionKey::numbered(1));
        assert_eq!(by.workflow, Some(cli_workflow_run_id("worx86tbc")));
        assert_eq!(by.agent.as_deref(), Some("sign-off:fix-the-money#1"));
    }

    /// With several at work the label without its round says which, then the role's word, and
    /// when neither does no agent is named.
    #[test]
    fn with_several_at_work_the_label_then_the_role_says_which() {
        use CliAgentState::Running;
        let workflow = reported(&[
            ("build:fix-the-money", Running),
            ("review:fix-the-money#2", Running),
            ("sign-off:fix-the-money#1", Running),
        ]);
        assert_eq!(
            asker(
                &workflow,
                RoleName::Reviewer,
                "review:fix-the-money · round 2"
            )
            .as_deref(),
            Some("review:fix-the-money#2")
        );
        assert_eq!(
            asker(&workflow, RoleName::Judge, "final").as_deref(),
            Some("sign-off:fix-the-money#1")
        );
        let two_reviews = reported(&[("review:a", Running), ("review:b", Running)]);
        assert_eq!(asker(&two_reviews, RoleName::Reviewer, "review:c"), None);
        let none = reported(&[("build:a", CliAgentState::Done)]);
        assert_eq!(asker(&none, RoleName::Reviewer, "review:a"), None);
    }

    #[test]
    fn a_labels_stem_drops_either_rounds_number() {
        assert_eq!(label_stem("review:money#2"), "review:money");
        assert_eq!(label_stem("review:money · round 3"), "review:money");
        assert_eq!(label_stem("review:money"), "review:money");
        assert_eq!(label_stem("review:issue#x"), "review:issue#x");
    }

    fn review(verdict: ReviewVerdict, summary: &str) -> ReviewOutput {
        ReviewOutput {
            verdict,
            summary: summary.to_owned(),
            comments: vec![ReviewComment {
                path: "money.rs".to_owned(),
                line: None,
                severity: Severity::High,
                text: format!("{summary} in full"),
            }],
            tamper_suspected: false,
        }
    }

    /// A run relaunched midway through its review loop (your answer, Shift+Tab, a crash) starts
    /// its asks again from the log: each role's count, each label's round and the earlier
    /// findings go on from where the earlier process left them.
    #[test]
    fn a_relaunched_loop_counts_its_asks_from_the_log() {
        let codex: ModelRef = "codex".parse().unwrap();
        let flash: ModelRef = "antigravity-cli/flash".parse().unwrap();
        let task = "task-fix-the-money";
        let mut events = vec![envelope(
            1,
            Some(task),
            None,
            RunEventKind::Task(TaskEvent::Created(Box::new(task_spec(task, &[], None)))),
        )];
        let asks = [
            (
                "s-001",
                &codex,
                RoleName::Reviewer,
                "review:money",
                ReviewVerdict::RequestChanges,
            ),
            (
                "s-002",
                &codex,
                RoleName::Reviewer,
                "review:money · round 2",
                ReviewVerdict::RequestChanges,
            ),
            (
                "s-003",
                &flash,
                RoleName::Judge,
                "sign-off:money",
                ReviewVerdict::Approve,
            ),
        ];
        for (n, (key, worker, role, label, verdict)) in asks.into_iter().enumerate() {
            let mut started = session_started(worker, role);
            started.label = Some(label.to_owned());
            let seq = 2 + 2 * n as u64;
            events.push(envelope(
                seq,
                None,
                Some(key),
                RunEventKind::Session(SessionEvent::Started(Box::new(started))),
            ));
            let reviewed = ReviewEvent::Reviewed {
                reviewer: worker.clone(),
                blind: false,
                round: 1,
                review: review(verdict, &format!("finding {n}")),
                picked_by: None,
            };
            events.push(envelope(
                seq + 1,
                Some(task),
                Some(key),
                RunEventKind::Review(reviewed),
            ));
        }
        let state = RunState::fold(&events);
        let recorded = recorded_asks(&state, &TaskId::new(task).unwrap());
        assert_eq!(
            recorded.asked,
            BTreeMap::from([(CrewRole::Reviewer, 2), (CrewRole::Judge, 1)])
        );
        assert_eq!(
            recorded.rounds,
            BTreeMap::from([
                ("review:money".to_owned(), 2),
                ("sign-off:money".to_owned(), 1)
            ])
        );
        let labels: Vec<&str> = recorded.reviewed.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            ["review:money", "review:money · round 2", "sign-off:money"]
        );
        assert!(!recorded.reviewed[1].accepted);
        assert!(recorded.reviewed[2].accepted);
        assert!(
            recorded.reviewed[0]
                .said
                .contains("- blocking: finding 0 in full"),
            "{}",
            recorded.reviewed[0].said
        );
        assert_eq!(round_in("review:money · round 12"), ("review:money", 12));
        assert_eq!(round_in("review:money"), ("review:money", 1));
    }

    /// Once its workflow may not ask the reviewers again, a round of fixes doesn't tell the
    /// session to ask them.
    #[test]
    fn a_round_of_fixes_says_whether_the_reviewers_may_be_asked_again() {
        let reasons = ["Codex asks for changes: the rounding".to_owned()];
        assert!(fixes_asked(1, &reasons, true).contains("ask them again about the change"));
        let spent = fixes_asked(2, &reasons, false);
        assert!(spent.contains("don't ask them again"), "{spent}");
        assert!(
            !spent.contains("ask them again about the change"),
            "{spent}"
        );
    }

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
        let text = instructions(
            "add due dates to tasks",
            &crew(&[]),
            &Required::default(),
            3,
        );
        assert!(text.starts_with("add due dates to tasks\n\n"));
        assert!(text.contains("Workflow tool"));
        assert!(text.contains("argonauts_ask_reviewer"));
        assert!(!text.contains("argonauts_ask_judge"));
        assert!(text.ends_with("A report sent while the workflow still runs isn't taken."));
        let judged = instructions(
            "add due dates",
            &crew(&["claude-code/opus-5-5"]),
            &Required::default(),
            3,
        );
        assert!(judged.contains("argonauts_ask_judge"));
    }

    /// One workflow, the ask's own label, and a cap the session knows: nothing tells it to
    /// ask once more, since Argonauts Code goes through the final change itself.
    #[test]
    fn the_orchestrator_is_asked_for_one_workflow_and_told_the_cap() {
        let text = instructions(
            "fix the rounding",
            &crew(&["codex"]),
            &Required::default(),
            3,
        );
        assert!(
            text.contains("Start one Workflow call for the whole task"),
            "{text}"
        );
        assert!(text.contains("with its own label"), "{text}");
        assert!(text.contains("at most 3 times in this task"), "{text}");
        assert!(!text.contains("once more"), "{text}");
        assert!(report_now(CrewRole::Judge, 3).starts_with(
            "Argonauts Code has asked the judge 3 times about this task, as many as it allows"
        ));
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
        let text = instructions("add due dates", &crew(&[]), &required, 3);
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
        let claude = |model: &ModelRef| (model.agent == "claude-code", false);
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
        // The judge after Codex reviews: neither the author's provider nor the reviewer's.
        let after_codex = |model: &ModelRef| (model.agent == "claude-code", model.agent == "codex");
        assert_eq!(
            asked_models(&models, &[], &after_codex, 1),
            [models[2].clone()]
        );
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
        assert!(said(&astra, &review).ends_with("\n- blocking: past dates pass"));
        assert_eq!(
            review_of(&Value::Null).verdict,
            ReviewVerdict::RequestChanges
        );
        assert_eq!(
            review_of(&json!({"accept": false, "summary": "Not ready", "findings": null})).verdict,
            ReviewVerdict::RequestChanges,
            "a no with nothing named blocks on its summary"
        );
    }

    /// Each finding is blocking or a note, and nothing blocking left is an accept, whatever
    /// the answer's own accept says: a round that took its findings back passes.
    #[test]
    fn a_review_accepts_when_nothing_blocking_is_left() {
        let notes = review_of(&json!({
            "accept": false,
            "summary": "Close; nothing must change",
            "findings": [{"text": "a bare $ parses as zero", "blocking": false}],
        }));
        assert_eq!(notes.verdict, ReviewVerdict::Approve);
        assert_eq!(notes.comments[0].severity, Severity::Low);
        let blocking = review_of(&json!({
            "accept": true,
            "summary": "Fine",
            "findings": [
                {"text": "with_tax drops the tax", "blocking": true},
                {"text": "a docstring is stale", "blocking": false},
            ],
        }));
        assert_eq!(blocking.verdict, ReviewVerdict::RequestChanges);
        let codex: ModelRef = "codex".parse().unwrap();
        assert_eq!(
            said(&codex, &blocking),
            "Codex asks for changes: Fine\n- blocking: with_tax drops the tax\n- note: a docstring is stale"
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
        // A model the workflow's script names for an agent doesn't move it off this one.
        assert!(
            spec.env
                .contains(&(WORKFLOW_SUBAGENT_MODEL.to_owned(), "opus-5-5".to_owned()))
        );
    }

    /// Workers that follow the orchestrator leave its agents on the session's own model, so
    /// neither variable is set.
    #[test]
    fn no_worker_model_sets_no_subagent_model() {
        let own = OwnSession {
            asking: Asking {
                asks: argonauts_core::Asks::Never,
                endpoint: "127.0.0.1:9".into(),
                token: "t".into(),
                tools: Vec::new(),
                standing: None,
                roles: vec![CrewRole::Reviewer],
            },
            subagent_model: None,
        };
        let mut spec = argonauts_testkit::runner::session_spec(
            "claude-code",
            Access::Build,
            std::path::Path::new("/tmp/work"),
        );
        own.apply(&mut spec);
        assert!(
            !spec
                .env
                .iter()
                .any(|(name, _)| name == SUBAGENT_MODEL || name == WORKFLOW_SUBAGENT_MODEL),
            "{:?}",
            spec.env
        );
    }
}
