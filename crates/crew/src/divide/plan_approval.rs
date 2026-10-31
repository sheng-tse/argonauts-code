//! The plan approval: plan, graph, workers, reviewers, test list, and limit together. Plans judged
//! without a winner go to the pick question first. "Run it as Captain" switches to the Captain
//! workflow your `captain` settings pick, whose `build` makes the Captain task; "send back"
//! re-plans with your note; approve with edits records your note as a decision and adds it to every
//! task prompt, and a note that changes a worker, a task's paths, a way, or a limit line goes
//! through the graph validator.
//!
//! Before the plan is shown each task gets its way: the one your request names for it, else the
//! plan's, else the rule of thumb's. Work starts only once the plan holds every way you asked
//! for; approved again unchanged, it goes ahead with a warning that says what it lacks. A plan
//! that breaks a way you asked for asks you in supervised and auto mode, and on autopilot is
//! approved as it stands in your place, with one line that says what it lacks.

use argonauts_core::PhaseStatus;
use argonauts_core::base::names::{ApprovalId, DecisionId, ModelRef, SeatId};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalPacket, FreeText, PlanPacket, QuestionPacket,
    WayObjection, WaysOffer,
};
use argonauts_core::runs::events::council::{CouncilMessage, PointState, Speaker, TextPart};
use argonauts_core::runs::events::plan::{DivideEvent, LimitPlan, TaskGraph};
use argonauts_core::runs::events::run::{PhaseChange, RunOutcome, RunPhase};
use argonauts_core::runs::events::task::{TaskEvent, TaskKind, TaskSpec, TaskState, Way};
use argonauts_core::runs::events::workflow::StepEnd;
use argonauts_core::safety::policy::{EvidenceDocument, EvidenceFacts, EvidenceSubject};
use argonauts_core::workflows::workflow::WorkflowRef;
use argonauts_core::{Note, NoteLevel, TaskId};

use super::{Division, assign, dependencies, edits, limit_lines, paths, pick, plan_facts};
use crate::approvals::{self, ApprovalRequest, autopilot, options};
use crate::council::ways;
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::host::Host;
use crate::state::ids::IdKind;
use crate::understand::round;
use crate::workflow::{Flow, StepCall, StepDone, runner};
use crate::{plan, roles};

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    if host.read(|state| state.run.phase) != Some(RunPhase::Dividing) {
        phase(host, PhaseStatus::Started, None)?;
    }
    pick::pick_if_tied(host, call).await?;
    let division = super::division(host, call).await?;
    let mut graph = division.graph.clone();
    let asked = asked_ways(host);
    let way_lines = settle_ways(host, &mut graph, &asked);
    // What the plan lacked of your ways when you were last told, so the same answer twice goes
    // ahead instead of reopening forever.
    let mut warned: Vec<String> = Vec::new();
    let limit = division.limit.clone();
    let new_dependencies = &division.plan.new_dependencies;
    let dependency_problem = dependencies::problem(&host.repo().root, new_dependencies)?;
    let subject = subject(host).await;
    let mut reopened: Vec<String> = Vec::new();
    let mut last: Option<ApprovalId> = None;
    loop {
        let settings = host.settings();
        let unlisted =
            dependencies::unlisted(new_dependencies, &settings.limits.dependency_allowlist);
        let blocking = blocking(&division.broken, &unlisted);
        let owed = ways_owed(host, &graph, &asked, &[]);
        let ways_flags: Vec<String> = way_lines
            .iter()
            .chain(&owed)
            .cloned()
            .chain(same_maker_ask(host, &graph))
            .chain(last_round(host, call))
            .collect();
        let shown = Shown {
            division: &division,
            graph: &graph,
            limit: &limit,
            blocking: &blocking,
            ways: &ways_flags,
            reopened: &reopened,
            dependency_problem: dependency_problem.as_deref(),
            subject: subject.as_ref(),
        };
        let request = request(host, call, &shown);
        // A way you asked for that the plan breaks is yours to settle, so neither your rules nor
        // the judge do. On autopilot nothing waits for you: the plan is shown and approved as it
        // stands at once, unless something blocks approving it, which autopilot settles as it
        // settles any plan.
        let on_autopilot = autopilot::applies_now(host, ApprovalKind::Plan)
            && !autopilot::kept_for_you(settings, ApprovalKind::Plan);
        let because = "the plan breaks a way you asked for".to_owned();
        let decided = if owed.is_empty() || (on_autopilot && !blocking.is_empty()) {
            approvals::open_and_wait(host, request).await?
        } else if on_autopilot {
            autopilot::open_and_settle(host, request, AnswerAction::Approve, because)?;
            went_ahead_on_autopilot(host, &owed)?;
            approve(host, call, &graph, &division, dependency_problem.as_deref()).await?;
            return finish(host, &graph, "Plan approved on autopilot");
        } else {
            approvals::open_and_wait_yours(host, request, because).await?
        };
        if last.as_ref() == Some(&decided.id) {
            return Err(EngineError::Refused(format!(
                "The plan approval {} couldn't be reopened after: {}",
                decided.id,
                reopened.last().map_or("", String::as_str)
            )));
        }
        last = Some(decided.id.clone());
        match verdict(&decided.action, decided.text.as_deref(), &blocking) {
            Verdict::Approve => {
                if !owed.is_empty() && owed != warned {
                    reopened.push(format!("Before work starts: {}", owed.join(" ")));
                    warned = owed;
                    continue;
                }
                went_ahead_without(host, &owed)?;
                approve(host, call, &graph, &division, dependency_problem.as_deref()).await?;
                return finish(host, &graph, "Plan approved");
            }
            Verdict::ApproveWithEdits(note) => {
                let provider = |worker: &ModelRef| roles::provider(host, worker);
                let offer = ways::offer_for(host);
                let edited = edits::parse(&note).and_then(|parsed| {
                    edits::apply(&graph, &limit, &parsed, settings, &provider, &offer)
                });
                match edited {
                    Ok(edited) => {
                        let set_by_you: Vec<TaskId> =
                            edited.ways.iter().map(|(task, _)| task.clone()).collect();
                        let owed = ways_owed(host, &edited.graph, &asked, &set_by_you);
                        if !owed.is_empty() && owed != warned {
                            reopened
                                .push(format!("Your edits weren't applied: {}", owed.join(" ")));
                            warned = owed;
                            continue;
                        }
                        went_ahead_without(host, &owed)?;
                        record_edits(host, &note, &edited).await?;
                        graph = edited.graph;
                        approve(host, call, &graph, &division, dependency_problem.as_deref())
                            .await?;
                        return finish(host, &graph, "Plan approved with your edits");
                    }
                    Err(reason) => reopened.push(format!("Your edits weren't applied: {reason}")),
                }
            }
            Verdict::Reopen(reason) => reopened.push(reason),
            Verdict::SendBack => {
                phase(
                    host,
                    PhaseStatus::Finished,
                    Some("Sent back for a new plan"),
                )?;
                return Ok(StepDone::done("Sent back for a new plan with your note"));
            }
            Verdict::Captain => {
                phase(host, PhaseStatus::Finished, Some("Switched to Captain"))?;
                return Ok(StepDone {
                    end: StepEnd::Done,
                    summary: Some("You chose to run it as Captain".to_owned()),
                    flow: Flow::Switch(WorkflowRef::new(runner::captain_workflow(settings))),
                });
            }
            Verdict::Abandon => {
                phase(host, PhaseStatus::Finished, Some("Abandoned"))?;
                return Ok(StepDone {
                    end: StepEnd::Done,
                    summary: Some("You abandoned the run".to_owned()),
                    flow: Flow::Finish(RunOutcome::Abandoned),
                });
            }
        }
    }
}

/// What the run does with an answer to the plan approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    ApproveWithEdits(String),
    SendBack,
    Captain,
    Abandon,
    /// Open the approval again, saying why.
    Reopen(String),
}

/// `blocking` holds what stops approval: unlisted dependencies and rules the graph breaks.
pub fn verdict(action: &AnswerAction, text: Option<&str>, blocking: &[String]) -> Verdict {
    let note = text.map(str::trim).filter(|note| !note.is_empty());
    match action {
        AnswerAction::Approve | AnswerAction::ApproveWithEdits if !blocking.is_empty() => {
            Verdict::Reopen(format!(
                "The plan can't be approved yet. {}",
                blocking.join(" ")
            ))
        }
        AnswerAction::Approve => Verdict::Approve,
        AnswerAction::ApproveWithEdits => note.map_or(Verdict::Approve, |note| {
            Verdict::ApproveWithEdits(note.to_owned())
        }),
        AnswerAction::SendBack => Verdict::SendBack,
        AnswerAction::SwitchToCaptain => Verdict::Captain,
        AnswerAction::Abandon => Verdict::Abandon,
        other => Verdict::Reopen(format!(
            "{other:?} isn't an answer the plan approval takes."
        )),
    }
}

/// The question's options: approval is left off while anything blocks it.
pub fn answer_options(blocked: bool) -> Option<Vec<AnswerOption>> {
    blocked.then(|| {
        options::standard(ApprovalKind::Plan)
            .into_iter()
            .filter(|option| {
                !matches!(
                    option.action,
                    AnswerAction::Approve | AnswerAction::ApproveWithEdits
                )
            })
            .collect()
    })
}

pub fn blocking(broken: &[String], unlisted: &[String]) -> Vec<String> {
    let mut reasons = broken.to_vec();
    for package in unlisted {
        reasons.push(format!(
            "The plan adds {package}, which isn't on your dependency allowlist. Add it with \
             /permissions, or send the plan back."
        ));
    }
    reasons
}

/// One line per acceptance-test task, for the test list.
pub fn test_list(graph: &TaskGraph) -> Vec<String> {
    graph
        .tasks
        .iter()
        .filter_map(|task| {
            let TaskKind::AcceptanceTests { for_task } = &task.kind else {
                return None;
            };
            let criteria = if task.criteria.is_empty() {
                String::new()
            } else {
                format!(", covering {}", task.criteria.join(", "))
            };
            let by = task
                .worker
                .as_ref()
                .map_or_else(String::new, |worker| format!(", written by {worker}"));
            Some(format!("{} tests {for_task}{criteria}{by}", task.id))
        })
        .collect()
}

/// What one opening of the plan approval shows.
struct Shown<'a> {
    division: &'a Division,
    graph: &'a TaskGraph,
    limit: &'a LimitPlan,
    blocking: &'a [String],
    /// What code changed of the plan's ways, and what the plan lacks of yours.
    ways: &'a [String],
    /// Why it opened again, one reason per reopening. They're part of the evidence, so each
    /// reopening is an approval of its own.
    reopened: &'a [String],
    /// Why Argonauts Code can't add the new dependencies.
    dependency_problem: Option<&'a str>,
    subject: Option<&'a EvidenceSubject>,
}

/// The commit the run started from, which the plan was made against. `None`, which asks you,
/// when it can't be read.
pub(super) async fn subject(host: &Host) -> Option<EvidenceSubject> {
    let base = host.read(|state| {
        state
            .run
            .started
            .as_ref()
            .map(|started| started.base_commit.clone())
    })?;
    let tree_sha = host.git().tree_of(&host.repo().root, &base).await.ok()?;
    Some(EvidenceSubject {
        base_commit: base.clone(),
        head_commit: base,
        tree_sha,
    })
}

fn request(host: &Host, call: &StepCall, shown: &Shown) -> ApprovalRequest {
    let Shown {
        division,
        graph,
        limit,
        blocking,
        ways: way_flags,
        reopened,
        dependency_problem,
        subject,
    } = shown;
    let settled = roles::tasks::settled(host);
    let settings = &*settled;
    let review = plan_facts::review(host);
    let (decisions, mut workers) = host.read(|state| {
        let workers: Vec<String> = state
            .divide
            .workers
            .iter()
            .map(|(task, pick)| format!("{task} goes to {}: {}", pick.worker, pick.reason))
            .collect();
        (state.plan.decisions.clone(), workers)
    });
    workers.extend(how_each_is_done(host, graph));
    let mut flags = blocking.to_vec();
    flags.extend(way_flags.iter().cloned());
    flags.extend(review.flags());
    if let Some(problem) = dependency_problem {
        flags.push(dependency_flag(&division.plan.new_dependencies, problem));
    }
    flags.extend(assign::shortfalls(graph, settings));
    flags.extend(reopened.iter().cloned());
    let owned: Vec<String> = graph
        .tasks
        .iter()
        .flat_map(|task| task.owned_paths.iter().cloned())
        .collect();
    let protected: Vec<String> = owned
        .iter()
        .filter(|path| {
            settings
                .limits
                .protected_paths
                .iter()
                .any(|area| paths::inside(path, area) || paths::contains(path, area))
        })
        .cloned()
        .collect();
    let planning = review.facts();
    let facts = EvidenceFacts {
        open_questions: planning
            .open_questions
            .map(|open| open + u32::from(dependency_problem.is_some())),
        estimate_usd: limit_lines::estimate_usd(&graph.tasks, limit),
        new_dependencies: division.plan.new_dependencies.clone(),
        agent_paths: owned,
        protected_paths: protected,
        subject: subject.cloned(),
        ..planning
    };
    let mut documents = vec![EvidenceDocument {
        label: "plan".to_owned(),
        text: division.plan_text.clone(),
        path: None,
        sha256: None,
    }];
    if !reopened.is_empty() {
        documents.push(EvidenceDocument {
            label: "why it opened again".to_owned(),
            text: reopened.join("\n"),
            path: None,
            sha256: None,
        });
    }
    let packet = PlanPacket {
        summary: division.plan.summary.clone(),
        near_tie: review
            .judging
            .as_ref()
            .is_some_and(|result| result.near_tie),
        judging: review.judging,
        decisions,
        graph: (*graph).clone(),
        acceptance_tests: test_list(graph),
        limit: (*limit).clone(),
        new_dependencies: division.plan.new_dependencies.clone(),
        flags,
        workflow: host.read(super::workflow_plan::plan_workflow),
        seats: crate::council::talk::folder::planned(host, &graph.tasks),
        objections: objections(host, &graph.tasks),
        offer: offer(host, &graph.tasks),
    };
    ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Plan,
        title: "Approve this plan?".to_owned(),
        task: None,
        packet: ApprovalPacket::Plan(Box::new(packet)),
        facts,
        agent_outputs: workers,
        documents,
        options: answer_options(!blocking.is_empty()),
        free_text: FreeText::Edits,
        authors: vec![division.plan_author.clone(), division.divided_by.clone()],
    }
}

/// What Approve with edits offers the tasks, once any of them has a way.
fn offer(host: &Host, tasks: &[TaskSpec]) -> Option<WaysOffer> {
    if tasks.iter().all(|task| task.way.is_none()) {
        return None;
    }
    let offer = ways::offer_for(host);
    Some(WaysOffer {
        ways: offer.ways().to_vec(),
        not_offered: offer.not_offered_line(),
    })
}

/// The ways you asked for, read with the workflows a task could run: in your request, and in
/// your messages and points to the council. A seat you name for the work does it.
fn asked_ways(host: &Host) -> Vec<ways::Asked> {
    // A run always starts with its request; without one, nothing was asked.
    let request = host.read(round::request).unwrap_or_default();
    let workflows = ways::workflows_for(host);
    let mut asked = ways::asked_in(&request, &workflows);
    let Some(council) = host.read(|state| state.councils.newest().cloned()) else {
        return asked;
    };
    let Ok(relay) = crate::council::deliver::relay_for(host, &council) else {
        return asked;
    };
    let names = |seat: &SeatId| {
        crate::council::seats::find(relay.seating(), seat)
            .map(|seat| crate::council::seats::name(seat, &Default::default()))
            .unwrap_or_default()
    };
    // Your points from the request bind already, as the request's own ways.
    let from_request: Vec<String> = asked
        .iter()
        .map(|asked| relay.yours_held(&asked.clause))
        .collect();
    let mut yours: Vec<String> = council
        .points
        .iter()
        .filter(|point| point.state == PointState::Yours && !from_request.contains(&point.words))
        .map(|point| crate::council::relay::draw_markers(&point.words, names))
        .collect();
    yours.extend(
        council
            .messages
            .iter()
            .filter(|said| {
                said.message.from == Speaker::You
                    && !council.private_words.contains_key(&said.message.id)
            })
            .map(|said| said.message.words(names)),
    );
    for text in yours {
        for more in ways::asked_in(&text, &workflows) {
            if !asked
                .iter()
                .any(|known| known.way == more.way && known.about == more.about)
            {
                asked.push(more);
            }
        }
    }
    for wanted in &mut asked {
        let mut named: Vec<SeatId> = Vec::new();
        for part in relay.from_you(&wanted.clause).unwrap_or_default() {
            if let TextPart::Seat(seat) = part
                && !named.contains(&seat)
            {
                named.push(seat);
            }
        }
        if let [seat] = named.as_slice() {
            wanted.who =
                crate::council::seats::find(relay.seating(), seat).map(|seat| seat.model.clone());
        }
    }
    asked
}

/// Gives each task its way before the plan is shown: the one your request names for it, else
/// the plan's when the setup allows it, else the rule of thumb's. Returns what code changed.
fn settle_ways(host: &Host, graph: &mut TaskGraph, asked: &[ways::Asked]) -> Vec<String> {
    ways::attach(&mut graph.tasks, asked);
    let mut lines = ways::unmark(&mut graph.tasks, asked);
    lines.extend(ways::settle(&mut graph.tasks, &ways::offer_for(host)));
    lines
}

/// What the plan lacks before work starts: a way you asked for that no task is done by, a
/// task whose agents its paths can't split between, a workflow one task can't follow, and a
/// council's checker of the worker's own maker while another maker could check.
fn ways_owed(
    host: &Host,
    graph: &TaskGraph,
    asked: &[ways::Asked],
    set_by_you: &[TaskId],
) -> Vec<String> {
    let provider = |worker: &ModelRef| roles::provider(host, worker);
    let mut owed = ways::check(&graph.tasks, asked, set_by_you);
    owed.extend(graph.tasks.iter().filter_map(ways::agents_misfit));
    for task in &graph.tasks {
        if let Some(Way::Workflow { name }) = task.way.as_ref().map(|way| &way.how)
            && let Err(why) = ways::steps_of(host, name.as_str())
        {
            owed.push(format!("{}: {why}", task.title));
        }
    }
    owed.extend(edits::same_maker_checks(
        &graph.tasks,
        host.settings(),
        &provider,
    ));
    owed
}

/// In the planning loop's last round the question has no send back, so it says why.
fn last_round(host: &Host, call: &StepCall) -> Option<String> {
    host.read(|state| crate::workflow::kinds::in_last_loop_round(state, &call.at))
        .then(|| {
            "This is the last round of planning, so the plan can't be sent back again: approve it \
             or stop."
                .to_owned()
        })
}

/// With `council.same_model_check = "ask"`, the plan's approval asks you once whether a fresh
/// session of the model that made a task may check it, for every task no other maker can.
fn same_maker_ask(host: &Host, graph: &TaskGraph) -> Option<String> {
    if !ways::asks_first(host.settings()) {
        return None;
    }
    let titles: Vec<&str> = graph
        .tasks
        .iter()
        .filter(|task| ways::checked_by_its_own_model(host, task))
        .map(|task| task.title.as_str())
        .collect();
    let (last, rest) = titles.split_last()?;
    let tasks = match rest {
        [] => (*last).to_owned(),
        rest => format!("{} and {last}", rest.join(", ")),
    };
    Some(format!(
        "Same maker: no other maker is set up, so a fresh session of the model that made it \
         checks {tasks}. Approving the plan lets it."
    ))
}

/// You approved a plan that lacks some of what you asked for, after being told: the run says
/// so once more as it goes ahead.
fn went_ahead_without(host: &Host, owed: &[String]) -> EngineResult<()> {
    if owed.is_empty() {
        return Ok(());
    }
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: format!("The plan went ahead as approved: {}", owed.join(" ")),
        }),
    )?;
    Ok(())
}

/// Autopilot went ahead with a plan that lacks some of what you asked for: the one line that
/// says so, in place of the question.
fn went_ahead_on_autopilot(host: &Host, owed: &[String]) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: on_autopilot_line(owed),
        }),
    )?;
    Ok(())
}

fn on_autopilot_line(owed: &[String]) -> String {
    format!(
        "Autopilot approved the plan instead of asking you, though it breaks a way you asked \
         for. {}",
        owed.join(" ")
    )
}

/// One line per task on how it is done and why: "Redis bucket is done by Maker-checker: You
/// asked".
fn how_each_is_done(host: &Host, graph: &TaskGraph) -> Vec<String> {
    let offer = ways::offer_for(host);
    graph
        .tasks
        .iter()
        .filter_map(|task| {
            let way = task.way.as_ref()?;
            Some(format!(
                "{} is done {}: {}",
                task.title,
                ways::done_by(&way.how),
                ways::why(task, &offer, None)
            ))
        })
        .collect()
}

fn dependency_flag(packages: &[String], problem: &str) -> String {
    format!(
        "Argonauts Code can't add {}: {problem} Add them yourself on the run branch, \
         then approve.",
        packages.join(", ")
    )
}

/// Every task is created Waiting, then Argonauts Code adds the new dependencies, so each task
/// branches from a tree that already has them. When the approval said Argonauts Code can't add
/// them, they're yours to add; when adding fails anyway, a question asks you.
async fn approve(
    host: &Host,
    call: &StepCall,
    graph: &TaskGraph,
    division: &Division,
    dependency_problem: Option<&str>,
) -> EngineResult<()> {
    for task in &graph.tasks {
        if host.read(|state| state.tasks.by_id.contains_key(&task.id)) {
            continue;
        }
        // A task for agents tells its own session which part is its, and one done by a workflow
        // says how many steps its row counts.
        let mut task = task.clone();
        if let Some(way) = task.way.as_mut()
            && let Way::Workflow { name } = &way.how
        {
            way.steps = ways::steps_of(host, name.as_str())
                .ok()
                .map(|steps| u32::try_from(steps.len()).unwrap_or(u32::MAX));
        }
        let id = task.id.clone();
        let created = TaskEvent::Created(Box::new(ways::briefed(task)));
        host.record(Some(&id), None, RunEventKind::Task(created))?;
        let waiting = TaskEvent::State {
            from: None,
            to: TaskState::Waiting,
            reason: "created from the approved plan".to_owned(),
        };
        host.record(Some(&id), None, RunEventKind::Task(waiting))?;
    }
    let packages = &division.plan.new_dependencies;
    if packages.is_empty() || dependencies_added(host, packages) {
        return Ok(());
    }
    if let Some(problem) = dependency_problem {
        // The approval told you, and you approved: they're yours to add.
        host.record(
            None,
            None,
            RunEventKind::Note(Note {
                level: NoteLevel::Warn,
                text: dependency_flag(packages, problem),
            }),
        )?;
        return Ok(());
    }
    match dependencies::add(host, packages).await {
        Ok(_) => Ok(()),
        Err(EngineError::Interrupted) => Err(EngineError::Interrupted),
        Err(error) => ask_to_add(host, call, graph, division, &error.to_string()).await,
    }
}

fn dependencies_added(host: &Host, packages: &[String]) -> bool {
    host.read(|state| {
        packages
            .iter()
            .all(|package| state.merge.dependencies.contains(package))
    })
}

/// Argonauts Code couldn't add the packages: you add them yourself, and say when.
async fn ask_to_add(
    host: &Host,
    call: &StepCall,
    graph: &TaskGraph,
    division: &Division,
    failure: &str,
) -> EngineResult<()> {
    let Some(first) = graph.tasks.first() else {
        return Ok(());
    };
    let text = dependency_flag(&division.plan.new_dependencies, failure);
    let offered = vec![ADDED_THEM.to_owned(), WITHOUT_THEM.to_owned()];
    let request = ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Question,
        title: "Add the new dependencies yourself?".to_owned(),
        task: Some(first.id.clone()),
        packet: ApprovalPacket::Question(QuestionPacket {
            from: division.divided_by.clone(),
            task: first.id.clone(),
            text: text.clone(),
            options: offered.clone(),
        }),
        facts: EvidenceFacts::default(),
        agent_outputs: Vec::new(),
        documents: vec![EvidenceDocument {
            label: "why".to_owned(),
            text,
            path: None,
            sha256: None,
        }],
        options: Some(options::question(&offered)),
        free_text: FreeText::Answer,
        authors: Vec::new(),
    };
    let decided = approvals::open_and_wait(host, request).await?;
    let answer = match &decided.action {
        AnswerAction::Choose { value } => value.clone(),
        _ => decided.text.clone().unwrap_or_default(),
    };
    host.record(
        None,
        None,
        RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: format!(
                "Argonauts Code didn't add {}. You answered: {answer}",
                division.plan.new_dependencies.join(", ")
            ),
        }),
    )?;
    Ok(())
}

const ADDED_THEM: &str = "I added them, go on";
const WITHOUT_THEM: &str = "Go on without them";

async fn record_edits(host: &Host, note: &str, edited: &edits::Edited) -> EngineResult<()> {
    // The record takes the next number itself; this one only fills the field.
    let next = DecisionId::numbered(host.read(|state| state.ids.last(IdKind::Decision)) + 1);
    let mut ways = edits::way_lines(edited);
    ways.extend(objection_lines(host, &edited.graph.tasks));
    plan::decisions::record(host, edits::decision(next, note, &ways)).await?;
    let algorithm = host.settings().routing.algorithm;
    for task in edited.graph.tasks.iter() {
        if edited.workers.contains(&task.id)
            && let Some(worker) = &task.worker
        {
            let event = DivideEvent::Worker {
                task: task.id.clone(),
                worker: worker.clone(),
                algorithm,
                explored: false,
                reason: "you chose this worker at the plan approval".to_owned(),
            };
            host.record(Some(&task.id), None, RunEventKind::Divide(event))?;
        }
        if edited.reviewers.contains(&task.id) {
            let event = DivideEvent::Reviewers {
                task: task.id.clone(),
                reviewers: task.reviewers.clone(),
                blind: host.settings().review.blind,
            };
            host.record(Some(&task.id), None, RunEventKind::Divide(event))?;
        }
    }
    if edited.limit_changed {
        let event = DivideEvent::Limit(edited.limit.clone());
        host.record(None, None, RunEventKind::Divide(event))?;
    }
    let event = DivideEvent::Graph(edited.graph.clone());
    host.record(None, None, RunEventKind::Divide(event))?;
    Ok(())
}

/// Each seat's objection to a way you asked for that the plan still holds, from the council
/// whose plan this is.
fn objections(host: &Host, tasks: &[TaskSpec]) -> Vec<WayObjection> {
    let workflows = ways::workflows_for(host);
    with_talk(host, |talk| ways::objections(tasks, talk, &workflows))
}

/// The decision record's line for each of those: "… A seat objected: … · yours stands".
fn objection_lines(host: &Host, tasks: &[TaskSpec]) -> Vec<String> {
    let workflows = ways::workflows_for(host);
    with_talk(host, |talk| ways::objection_lines(tasks, talk, &workflows))
}

/// What `read` finds in the talk of the council whose plan this is; nothing when no council
/// talked.
fn with_talk<T: Default>(host: &Host, read: impl FnOnce(&ways::Talk<'_>) -> T) -> T {
    host.read(|state| {
        let Some(council) = state.councils.newest() else {
            return T::default();
        };
        let said: Vec<&CouncilMessage> =
            council.messages.iter().map(|said| &said.message).collect();
        let seats: Vec<SeatId> = council.seats().iter().map(|seat| seat.id.clone()).collect();
        let stance_of = |seat: &SeatId, point: u32| council.stances.of(seat, point);
        let talk = ways::Talk {
            points: &council.points,
            said: &said,
            seats: &seats,
            stance_of: &stance_of,
        };
        read(&talk)
    })
}

fn finish(host: &Host, graph: &TaskGraph, what: &str) -> EngineResult<StepDone> {
    let workers: std::collections::BTreeSet<&ModelRef> = graph
        .tasks
        .iter()
        .filter_map(|task| task.worker.as_ref())
        .collect();
    let summary = format!(
        "{what}: {} tasks for {} agents",
        graph.tasks.len(),
        workers.len()
    );
    phase(host, PhaseStatus::Finished, Some(&summary))?;
    Ok(StepDone::done(summary))
}

fn phase(host: &Host, status: PhaseStatus, summary: Option<&str>) -> EngineResult<()> {
    let change = PhaseChange {
        phase: RunPhase::Dividing,
        status,
        summary: summary.map(str::to_owned),
    };
    host.record(None, None, RunEventKind::Phase(change))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlisted_dependency_blocks_plan_approval() {
        let new = vec!["ioredis@5".to_owned(), "zod".to_owned()];
        let allowlist = vec!["zod".to_owned()];
        let unlisted = dependencies::unlisted(&new, &allowlist);
        assert_eq!(unlisted, ["ioredis@5"]);
        let blocked = blocking(&[], &unlisted);
        assert_eq!(blocked.len(), 1);
        assert!(blocked[0].contains("adds ioredis@5, which isn't on your dependency allowlist"));

        // Approval isn't offered, and an approval that arrives anyway reopens the question.
        let offered = answer_options(true).unwrap();
        assert!(offered.iter().all(|option| !matches!(
            option.action,
            AnswerAction::Approve | AnswerAction::ApproveWithEdits
        )));
        assert!(matches!(
            verdict(&AnswerAction::Approve, None, &blocked),
            Verdict::Reopen(reason) if reason.contains("ioredis@5")
        ));
        assert!(matches!(
            verdict(
                &AnswerAction::ApproveWithEdits,
                Some("task-001 worker codex"),
                &blocked
            ),
            Verdict::Reopen(_)
        ));
        // Sending it back still works while blocked.
        assert_eq!(
            verdict(&AnswerAction::SendBack, Some("no redis"), &blocked),
            Verdict::SendBack
        );

        // Once allowlisted, it goes through.
        let allowlist = vec!["zod".to_owned(), "ioredis".to_owned()];
        let unlisted = dependencies::unlisted(&new, &allowlist);
        let blocked = blocking(&[], &unlisted);
        assert!(blocked.is_empty());
        assert_eq!(answer_options(false), None);
        assert_eq!(
            verdict(&AnswerAction::Approve, None, &blocked),
            Verdict::Approve
        );
    }

    #[test]
    fn the_autopilot_line_says_what_the_plan_lacks() {
        let owed = vec![
            "store: Plan-first can't do one task: its Plan step is the orchestrator's.".to_owned(),
        ];
        assert_eq!(
            on_autopilot_line(&owed),
            "Autopilot approved the plan instead of asking you, though it breaks a way you asked \
             for. store: Plan-first can't do one task: its Plan step is the orchestrator's."
        );
    }

    #[test]
    fn each_answer_has_its_verdict() {
        assert_eq!(
            verdict(
                &AnswerAction::ApproveWithEdits,
                Some(" task-001 worker agy "),
                &[]
            ),
            Verdict::ApproveWithEdits("task-001 worker agy".to_owned())
        );
        assert_eq!(
            verdict(&AnswerAction::ApproveWithEdits, Some("  "), &[]),
            Verdict::Approve
        );
        assert_eq!(
            verdict(&AnswerAction::SwitchToCaptain, None, &[]),
            Verdict::Captain
        );
        assert_eq!(verdict(&AnswerAction::Abandon, None, &[]), Verdict::Abandon);
        assert!(matches!(
            verdict(&AnswerAction::Merge, None, &[]),
            Verdict::Reopen(_)
        ));
        let broken = vec!["task-001 owns no paths.".to_owned()];
        assert!(matches!(
            verdict(&AnswerAction::Approve, None, &broken),
            Verdict::Reopen(_)
        ));
    }
}
