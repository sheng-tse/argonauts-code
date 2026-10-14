//! Claude Code's own workflow for a crew. When the orchestrator and
//! the workers are on Claude Code, Claude Code plans the workflow and runs it with its own
//! subagents, exactly as it does by itself, in one task's worktree. Argonauts gives that
//! session a tool for the reviewers and one for the judge, answers each call by running that
//! role on its own model against the change in the worktree, and before anything merges checks
//! that the final change went through them. When it didn't, Argonauts asks them itself.
//! Steps you require and a task list you fixed go with the request; Argonauts can't change
//! Claude Code's workflow, so what it reported is checked against them and what it lacked is
//! said.

use std::sync::{Mutex, MutexGuard, PoisonError};

use argonauts_core::CardId;
use argonauts_core::asking::{Ask, AskLine, AskReply};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardBudget, CardKind, CardSpec, CardState, ReviewEvent};
use argonauts_core::events::session::InputPartKind;
use argonauts_core::events::workflow::{RunNote, WorkflowEvent};
use argonauts_core::labels;
use argonauts_core::names::WorkerRef;
use argonauts_core::required::{Required, brief, check};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::sha256_hex;
use argonauts_core::steps::judgment::{ReviewComment, ReviewOutput, ReviewVerdict};
use argonauts_core::steps::plan::Severity;
use argonauts_core::worker::{Access, Asking, SessionSpec};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::card::machine::Trigger;
use crate::card::pipeline::{self, Until};
use crate::card::support::part;
use crate::card::{self, worktree};
use crate::engine::host::{Host, SessionPlan, StandIns};
use crate::error::{EngineError, EngineResult};
use crate::roles::display_name;
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
}

/// One task's asks while its session works.
struct Asks<'a> {
    host: &'a Host,
    at: &'a StepAt,
    card: &'a CardId,
    crew: &'a OwnCrew,
    request: &'a str,
    token: String,
    reviewed: Mutex<Vec<Reviewed>>,
}

/// Runs the request as Claude Code's own workflow, then merges what the reviewers accepted.
pub async fn run(
    host: &Host,
    call: &StepCall,
    request: &str,
    crew: &OwnCrew,
) -> EngineResult<StepDone> {
    let at = &call.at;
    let name = labels::id_slug(request, 4);
    let card =
        CardId::new(format!("task-{name}")).map_err(|err| EngineError::Refused(err.to_string()))?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let asks = Asks {
        host,
        at,
        card: &card,
        crew,
        request,
        token: format!("{:032x}", rand::random::<u128>()),
        reviewed: Mutex::default(),
    };
    note(host, call, format!("{name} · {}", engine_line(crew)))?;
    let required =
        crate::script::required::shared(&crate::script::required::for_run(host, request));
    if host.read(|state| state.cards.get(&card).is_none()) {
        card::create(host, task(host, &card, &name, request, crew, &required))?;
    }
    let roles = if crew.judge.is_empty() {
        vec![CrewRole::Reviewers]
    } else {
        vec![CrewRole::Reviewers, CrewRole::Judge]
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
        subagent_model: crew.workers.model.clone(),
    };
    host.shared().work_as_claude_codes_own(&card, own);
    let built = tokio::select! {
        built = pipeline::drive(host, &card, at, Until::GatesPassed) => built,
        () = serve(&listener, &asks) => unreachable!("serving asks never ends by itself"),
    };
    host.shared().done_as_claude_codes_own(&card);
    let state = built?;
    if state != CardState::InReview {
        let state = format!("{state:?}").to_lowercase();
        let said = argonauts_core::text::state_words(&state);
        return Ok(StepDone::done(format!(
            "{name} stopped before its checks passed: it is {said}"
        )));
    }
    let lacked = lacked(host, &card, &required);
    if !lacked.is_empty() {
        note_left(host, call, lacked.join(" "))?;
    }
    let lacked: String = lacked.iter().map(|line| format!(" · {line}")).collect();
    let accepted = gone_through(&asks).await?;
    let (trigger, why) = if accepted {
        (
            Trigger::Approved,
            "the crew's reviewers accepted the final change",
        )
    } else {
        (
            Trigger::ChangesRequestedAgain,
            "the crew's reviewers didn't accept the final change",
        )
    };
    pipeline::transition(host, &card, trigger, why)?;
    let merged = pipeline::run_card(host, &card, at).await? == CardState::Integrated;
    Ok(StepDone::done(if merged {
        format!("{name} ran as Claude Code's own workflow · merged into the run{lacked}")
    } else {
        format!("{name} ran as Claude Code's own workflow · {why}{lacked}")
    }))
}

/// What the workflow Claude Code reported lacked of what you required, in plain lines: its
/// phases' titles against the steps, and its agents' labels against a task list you fixed.
/// One that reported no workflow lacked all of it.
fn lacked(host: &Host, card: &CardId, required: &Required) -> Vec<String> {
    if required.is_empty() {
        return Vec::new();
    }
    let reported = host.read(|state| {
        state
            .sessions
            .in_order()
            .into_iter()
            .filter(|session| session.card.as_ref() == Some(card))
            .filter_map(|session| session.workflow.clone())
            .next_back()
    });
    let Some(workflow) = reported else {
        let tasks = check::tasks(required, &[]);
        return check::said(&check::steps(required, &[]), tasks.as_ref());
    };
    let titles: Vec<&str> = workflow
        .phases
        .iter()
        .map(|phase| phase.title.as_str())
        .collect();
    let found = check::steps(required, &titles);
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
        let task = task_of(&agent.label);
        if !in_a_required_step(agent.phase) && !labels.iter().any(|known| task_of(known) == task) {
            labels.push(&agent.label);
        }
    }
    let tasks = check::tasks(required, &labels);
    check::said(&found, tasks.as_ref())
}

/// "Claude Code's own workflow · workers on Opus 5.5 · reviews by Astra", which says which
/// engine runs it.
pub fn engine_line(crew: &OwnCrew) -> String {
    let mut line = "Claude Code's own workflow".to_owned();
    if crew.workers.model.is_some() {
        line.push_str(&format!(" · workers on {}", display_name(&crew.workers)));
    }
    if let Some(reviewer) = crew.reviewers.first() {
        line.push_str(&format!(" · reviews by {}", display_name(reviewer)));
    }
    line
}

fn note(host: &Host, call: &StepCall, text: String) -> EngineResult<()> {
    record_note(host, call, text, false)
}

/// What it lacked of what you required: said now, and listed again in the run's closing
/// answer, since Argonauts can't add it to Claude Code's workflow.
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

/// The one task the workflow works in: the request, owned by the orchestrator's seat. Its
/// review comes through the tools, so the engine's own stays out.
fn task(
    host: &Host,
    card: &CardId,
    name: &str,
    request: &str,
    crew: &OwnCrew,
    required: &Required,
) -> CardSpec {
    let criteria = host.read(|state| {
        state
            .goal
            .approved
            .as_ref()
            .map(|goal| goal.criteria.iter().map(|c| c.id.clone()).collect())
            .unwrap_or_default()
    });
    CardSpec {
        id: card.clone(),
        title: format!("build:{name}"),
        task: instructions(request, crew, required),
        kind: CardKind::Build,
        owned_paths: Vec::new(),
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria,
        owner: Some(crew.orchestrator.clone()),
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: Some(name.to_owned()),
    }
}

/// What the orchestrator's session reads besides the request: run it as a workflow of its own,
/// with the steps you require and the tasks you fixed, and send each review and the sign-off
/// to the crew's roles through the tools.
fn instructions(request: &str, crew: &OwnCrew, required: &Required) -> String {
    let names = |seats: &[WorkerRef]| {
        seats
            .iter()
            .map(display_name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = format!(
        "{request}\n\nRun this as a workflow of your own with the Workflow tool: plan its \
         phases and let its agents do the work."
    );
    if crew.workers.model.is_some() {
        text.push_str(&format!(
            " Your agents run on {}.",
            display_name(&crew.workers)
        ));
    }
    if let Some(yours) = brief::for_claude_code(required) {
        text.push_str(&format!("\n\n{}", yours.trim_end()));
    }
    text.push_str(&format!(
        "\n\nThe crew's reviewers are {}. At each review step, the agent that reviews calls \
         mcp__argonauts_status__argonauts_ask_reviewers with what the change is meant to do, \
         instead of reviewing it itself, and what they find is fixed. Before you finish, ask \
         them once more about the final change: nothing merges that didn't go through them.",
        names(&crew.reviewers)
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
         select:mcp__argonauts_status__argonauts_ask_reviewers.",
    );
    text
}

/// Answers asks until the task's session is over, several at once.
async fn serve(listener: &TcpListener, asks: &Asks<'_>) {
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
            Ask::Role { role, label, what } => match asks.ask(role, &label, &what).await {
                Ok(text) => AskReply::text(text),
                Err(err) => AskReply {
                    text: format!("Argonauts couldn't ask them: {err}"),
                    is_error: true,
                },
            },
            _ => AskReply {
                text: "This session can only ask the crew's roles.".to_owned(),
                is_error: true,
            },
        },
        _ => AskReply {
            text: "Argonauts didn't take that ask.".to_owned(),
            is_error: true,
        },
    };
    if let Ok(text) = serde_json::to_string(&reply) {
        let _ = write.write_all(format!("{text}\n").as_bytes()).await;
    }
}

impl Asks<'_> {
    /// The role's seats each read the change as it is now, and their answers come back as
    /// one text. Each answer is recorded as a review of the task, under the reviewer's name.
    async fn ask(&self, role: CrewRole, label: &str, what: &str) -> EngineResult<String> {
        let host = self.host;
        let seats = match role {
            CrewRole::Judge if !self.crew.judge.is_empty() => &self.crew.judge,
            CrewRole::Judge | CrewRole::Reviewers => &self.crew.reviewers,
            other => {
                return Err(EngineError::Refused(format!(
                    "the {other} aren't asked this way"
                )));
            }
        };
        let tree = worktree::ensure(host, self.card).await?;
        let git = worktree::tree_git(host, &tree.path).await?;
        let index = index_file(host, self.card, "index")?;
        let change = git.working_diff(&tree.path, &tree.base, &index).await?;
        if change.trim().is_empty() {
            return Ok("There is no change in the folder yet, so nothing was reviewed.".to_owned());
        }
        let wanted = (host.settings().review.per_task.max(1) as usize).min(seats.len().max(1));
        let label = if label.trim().is_empty() {
            format!("{}:{}", role_word(role), self.card)
        } else {
            label.trim().to_owned()
        };
        let mut answers = Vec::new();
        let mut accepted = true;
        for seat in seats.iter().take(wanted) {
            let (yes, said) = self
                .one(role, seat, &label, what, &tree.path, &change)
                .await?;
            accepted &= yes;
            answers.push(said);
        }
        lock(&self.reviewed).push(Reviewed {
            role,
            change: sha256_hex(change.as_bytes()),
            accepted,
        });
        Ok(answers.join("\n\n"))
    }

    async fn one(
        &self,
        role: CrewRole,
        seat: &WorkerRef,
        label: &str,
        what: &str,
        workdir: &std::path::Path,
        change: &str,
    ) -> EngineResult<(bool, String)> {
        let host = self.host;
        let mut parts = vec![
            part(InputPartKind::Prompt, role_word(role), asked_of(role), None),
            part(InputPartKind::Goal, "the request", self.request, None),
        ];
        if !what.trim().is_empty() {
            parts.push(part(InputPartKind::Message, "what it does", what, None));
        }
        parts.push(part(InputPartKind::Evidence, "the change", change, None));
        let plan = SessionPlan {
            at: Some(self.at.clone()),
            role: role.fine_role(),
            worker: seat.clone(),
            card: None,
            access: Access::ReadOnly,
            step: None,
            workdir: workdir.to_path_buf(),
            parts,
            json_schema: Some(verdict_schema()),
            resume: None,
            slot: 0,
            label: Some(label.to_owned()),
            reviews: false,
        };
        let ran = host
            .run_session_in_turn(plan, None, StandIns::Allowed, &|_| Ok(()))
            .await?;
        let answer = ran.end.structured.clone().unwrap_or(Value::Null);
        let review = review_of(&answer);
        let accepted = review.verdict == ReviewVerdict::Approve;
        let event = ReviewEvent::Reviewed {
            reviewer: ran.worker.clone(),
            blind: false,
            round: 1,
            review: review.clone(),
        };
        host.record(Some(self.card), Some(&ran.key), RunEventKind::Review(event))?;
        Ok((accepted, said(&ran.worker, &review)))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Once the session is over: when the final change went through the reviewers (and the judge,
/// when it has seats of its own) and they accepted it, it may merge. When it didn't, Argonauts
/// asks them now, so nothing merges unreviewed.
async fn gone_through(asks: &Asks<'_>) -> EngineResult<bool> {
    let host = asks.host;
    let tree = worktree::ensure(host, asks.card).await?;
    let git = worktree::tree_git(host, &tree.path).await?;
    let index = index_file(host, asks.card, "final-index")?;
    let last = git.working_diff(&tree.path, &tree.base, &index).await?;
    let last = sha256_hex(last.as_bytes());
    let mut roles = vec![CrewRole::Reviewers];
    if !asks.crew.judge.is_empty() {
        roles.push(CrewRole::Judge);
    }
    let mut accepted = true;
    for role in roles {
        let read_last = lock(&asks.reviewed)
            .iter()
            .rev()
            .find(|reviewed| reviewed.role == role && reviewed.change == last)
            .cloned();
        match read_last {
            Some(reviewed) => accepted &= reviewed.accepted,
            None => {
                asks.ask(role, "", "The final change, before it merges.")
                    .await?;
                let reviewed = lock(&asks.reviewed).last().cloned();
                accepted &= reviewed.is_some_and(|reviewed| reviewed.accepted);
            }
        }
    }
    Ok(accepted)
}

/// A throwaway git index beside the run's workflow, for reading a worktree's whole change.
fn index_file(host: &Host, card: &CardId, name: &str) -> EngineResult<std::path::PathBuf> {
    let dir = host.repo().run_dir.join("workflow");
    argonauts_core::paths::create_private_dir_all(&dir)?;
    Ok(dir.join(format!("{card}.{name}")))
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
pub fn said(reviewer: &WorkerRef, review: &ReviewOutput) -> String {
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
            workers: "claude-code/opus-5-5".parse().unwrap(),
            reviewers: vec!["astra".parse().unwrap()],
            judge: judge.iter().map(|seat| seat.parse().unwrap()).collect(),
        }
    }

    #[test]
    fn the_orchestrator_is_told_to_send_reviews_through_argonauts() {
        let text = instructions("add due dates to tasks", &crew(&[]), &Required::default());
        assert!(text.starts_with("add due dates to tasks\n\n"));
        assert!(text.contains("Workflow tool"));
        assert!(text.contains("argonauts_ask_reviewers"));
        assert!(!text.contains("argonauts_ask_judge"));
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
                role: CrewRole::Workers,
                does: Some("Run the benchmarks.".into()),
                when: Some("last".into()),
            }],
            tasks: vec!["migrate the schema".into()],
        };
        let text = instructions("add due dates", &crew(&[]), &required);
        assert!(
            text.contains(
                "Your workflow must have a phase for each of these steps, titled with this \
                 name, where it says:\n- Benchmark, last, by the workers: Run the benchmarks."
            ),
            "{text}"
        );
        assert!(
            text.contains("1. migrate the schema (label: migrate-schema)"),
            "{text}"
        );
        // Before the tools, which every agent must read.
        assert!(text.find("Benchmark") < text.find("argonauts_ask_reviewers"));
    }

    #[test]
    fn an_answer_reads_as_a_review() {
        let review = review_of(&json!({"accept": false, "findings": ["past dates pass"]}));
        assert_eq!(review.verdict, ReviewVerdict::RequestChanges);
        assert_eq!(review.summary, "Not accepted");
        let astra: WorkerRef = "astra".parse().unwrap();
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
                roles: vec![CrewRole::Reviewers],
            },
            subagent_model: Some("opus-5-5".into()),
        };
        let mut spec = argonauts_testkit::worker::session_spec(
            "claude-code/fable-5-1",
            Access::Build,
            std::path::Path::new("/tmp/work"),
        );
        own.apply(&mut spec);
        assert!(spec.cli_workflow.ultracode);
        assert_eq!(spec.asking.as_ref().unwrap().roles, [CrewRole::Reviewers]);
        assert!(
            spec.env
                .contains(&(SUBAGENT_MODEL.to_owned(), "opus-5-5".to_owned()))
        );
    }
}
