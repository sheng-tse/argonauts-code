//! The Understand approval. The open questions go to whoever answers the crew's questions: you, or
//! the main agent, whose answers become assumptions and leave you only the questions that turn on a
//! fact no agent can find out. Once you've answered them all, that approves the goal. Otherwise
//! the approval asks: approve, approve with edits (rewrite and re-hash), send back (read and
//! compare once more with your note) while the goal's loop has a round left, abandon, show.

use argonauts_core::NoteLevel;
use argonauts_core::PhaseStatus;
use argonauts_core::RunId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalDecided, ApprovalKind, ApprovalPacket, FreeText, GoalPacket,
};
use argonauts_core::events::goal::{
    ChoiceOption, Decider, GoalEvent, GoalFile, GoalQuestion, GoalSource,
    LET_THE_MAIN_AGENT_ANSWER, LET_THE_MAIN_AGENT_DECIDE,
};
use argonauts_core::events::run::{Party, RunMode, RunOutcome};
use argonauts_core::events::workflow::StepEnd;
use argonauts_core::names::ModelRef;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::Answerer;
use argonauts_core::settings::workflows::RunStyle;
use argonauts_core::steps::understand::{CompareOutput, DiffTag};
use argonauts_core::workflow::StepRef;

use crate::approvals::{self, ApprovalRequest, autopilot, options};
use crate::captain;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::state::RunState;
use crate::state::goal::SettledGoal;
use crate::workflow::{Flow, StepCall, StepDone, brief, kinds, shape};

use super::main_agent::{self, Pick};
use super::materiality::{self, one_line};
use super::{draft, file, round};

pub async fn run(host: &Host, call: &StepCall) -> EngineResult<StepDone> {
    loop {
        // `None` when the step looks again: your prompt just became the draft, or autopilot came
        // on while a question waited for you and the main agent answers it.
        if let Some(done) = settle(host, call).await? {
            return Ok(done);
        }
    }
}

async fn settle(host: &Host, call: &StepCall) -> EngineResult<Option<StepDone>> {
    let view = host.read(|state| View::of(state, host.settings(), host.run_id(), &call.at.step));
    if view.approved {
        return Ok(Some(StepDone::done("The goal is already approved")));
    }
    if view.sent_back {
        return Ok(Some(StepDone::done("Sent back for another round")));
    }
    // Nothing read the request, as when Understand is off or the main agent left it out. Your
    // prompt is the draft, and the main agent's own open questions, when it wrote some, come to
    // this question. The go-ahead is this question's, unless a plan question follows to give it.
    if view.drafted.is_none() && host.read(|state| state.goal.round == 0) {
        let questions = host.read(brief_questions);
        let plan_task_follows = host.read(|state| {
            state
                .workflows
                .by_id
                .get(&call.at.step.workflow)
                .is_some_and(|record| shape::plan_approval_runs(&record.workflow))
        });
        if questions.is_empty() && plan_task_follows {
            captain::prompt_is_the_goal(host).await?;
            return Ok(Some(StepDone::skipped(
                "Your prompt is the goal, and the plan approval gives the go-ahead",
            )));
        }
        draft_from_the_prompt(host, questions).await?;
        return Ok(None);
    }
    let drafted = view
        .drafted
        .clone()
        .ok_or_else(|| EngineError::Refused("there is no goal draft to check yet".to_owned()))?;

    let by_main_agent = approvals::answerer(host, ApprovalKind::Understand).answers_for_you();
    let autopilot = approvals::autopilot::applies_now(host, ApprovalKind::Understand);
    let mut drafted = drafted;
    let mut main_agent_answered = None;
    let questions = &view.settled.questions;
    // A question you answered yourself keeps your answer, whoever answers the rest.
    let yours_answered = |question: &GoalQuestion| {
        view.answered
            .iter()
            .find(|(asked, _)| *asked == question.text)
            .map(|(_, decided)| decided)
    };
    if !questions.is_empty() {
        let for_main_agent: Vec<bool> = questions
            .iter()
            .map(|question| {
                by_main_agent
                    && materiality::main_agent_may_answer(question, host.settings(), autopilot)
                    && yours_answered(question).is_none()
            })
            .collect();
        let main_agent_questions: Vec<GoalQuestion> = questions
            .iter()
            .zip(&for_main_agent)
            .filter(|(_, for_main_agent)| **for_main_agent)
            .map(|(question, _)| question.clone())
            .collect();
        let answered = if main_agent_questions.is_empty() {
            if let Some(yours) = main_agent::without_answers(&drafted) {
                drafted = round::write_draft(host, yours).await?;
            }
            None
        } else {
            Some(main_agent::answer(host, call, &drafted, &main_agent_questions).await?)
        };
        // Yours are the questions the main agent may not answer, and the ones it couldn't.
        let mut picks = answered.iter().flat_map(|answered| answered.picks.iter());
        let yours: Vec<(GoalQuestion, Option<String>)> = questions
            .iter()
            .zip(&for_main_agent)
            .filter_map(|(question, for_main_agent)| {
                if !*for_main_agent {
                    let why = materiality::asked_about(question, host.settings())
                        .filter(|_| by_main_agent)
                        .map(|word| approvals::yours_because(&format!("{word} questions")));
                    return Some((question.clone(), why));
                }
                match picks.next() {
                    Some(Pick::Answered(_)) => None,
                    Some(Pick::Yours(why)) => Some((question.clone(), Some(why.clone()))),
                    None => Some((question.clone(), None)),
                }
            })
            .collect();
        let assumed = answered
            .as_ref()
            .map(main_agent::MainAgentAnswers::assumptions)
            .unwrap_or_default();
        if let Some(answered) = &answered
            && !assumed.is_empty()
        {
            // Written before you're asked anything, so a re-entered step finds them and the
            // transcript says it once.
            let first_time = !drafted
                .assumptions
                .iter()
                .any(|assumption| assumption.by_main_agent());
            drafted =
                round::write_draft(host, main_agent::with_assumptions(&drafted, &assumed)).await?;
            if first_time {
                round::note(
                    host,
                    NoteLevel::Info,
                    main_agent::answered_line(
                        host.settings()
                            .approvals
                            .answers
                            .as_ref()
                            .and_then(Answerer::model)
                            .filter(|_| autopilot),
                        assumed.len(),
                        yours.len(),
                    ),
                )?;
                for (question, pick) in main_agent_questions.iter().zip(&answered.picks) {
                    if let Pick::Answered(assumption) = pick {
                        answered_by(host, &question.text, &assumption.text, Decider::MainAgent)?;
                    }
                }
            }
            main_agent_answered = Some((answered.main_agent.clone(), assumed.len()));
        }

        let renumbered = renumber(yours.iter().map(|(question, _)| question.clone()).collect());
        let mut answers = Vec::new();
        let mut by = Party::User;
        // From here on the main agent decides, once you chose "Let the main agent decide this and
        // the rest". A question you answered before that keeps your answer.
        let mut to_main_agent = None;
        for (index, question) in renumbered.iter().enumerate() {
            let (decided, fresh) = match yours_answered(question) {
                Some(decided) => (decided.clone(), false),
                None => {
                    let request = question_request(call, &drafted, &view, &renumbered[index..]);
                    let because = yours[index].1.clone();
                    let opening = approvals::open_because(host, request, because)?;
                    match opening.decided_or_withdrawn(host).await? {
                        Ok(decided) => (decided, true),
                        Err(why) if why == autopilot::LEFT_TO_THE_MAIN_AGENT => return Ok(None),
                        Err(why) => {
                            return Err(EngineError::Refused(format!(
                                "the goal's question was withdrawn: {why}"
                            )));
                        }
                    }
                }
            };
            by = decided.by.clone();
            if lets_the_main_agent_decide(&decided) {
                to_main_agent = Some(index);
                break;
            }
            let answer = answer_text(question, &decided)?;
            if fresh {
                answered_by(host, &question.text, &answer, decider(&decided.by))?;
            }
            answers.push(answer);
        }
        if !answers.is_empty() {
            drafted = round::write_draft(host, draft::with_answers(&drafted, &answers)).await?;
        }
        if let Some(from) = to_main_agent {
            let rest = &renumbered[from..];
            let decided = main_agent::answer_in_slot(host, call, &drafted, rest, 1).await?;
            // A question the main agent can't settle takes its recommended answer: you handed it
            // over.
            let assumed: Vec<_> = rest
                .iter()
                .zip(&decided.picks)
                .map(|(question, pick)| match pick {
                    Pick::Answered(assumption) => assumption.clone(),
                    Pick::Yours(_) => main_agent::recommended(question),
                })
                .collect();
            if !drafted
                .assumptions
                .iter()
                .any(|kept| assumed.contains(kept))
            {
                for (question, assumption) in rest.iter().zip(&assumed) {
                    answered_by(host, &question.text, &assumption.text, Decider::MainAgent)?;
                }
            }
            drafted =
                round::write_draft(host, main_agent::with_assumptions(&drafted, &assumed)).await?;
            main_agent_answered = Some((decided.main_agent.clone(), assumed.len()));
        }
        if main_agent_answered.is_none() {
            // Answering the last question approves the goal, with every answer written in.
            approve(host, drafted, by)?;
            return Ok(Some(StepDone::done(match answers.len() {
                1 => "You answered 1 question and approved the goal".to_owned(),
                n => format!("You answered {n} questions and approved the goal"),
            })));
        }
        // The main agent's answers still need the approval, which in auto the judge gives.
    }

    let decided = match view.decided.clone() {
        Some(decided) => decided,
        None => {
            let style = shape::run_style(host);
            let request = approval_request(
                call,
                &drafted,
                &view,
                by_main_agent,
                main_agent_answered.clone(),
                style,
            );
            if approvals::keeps_yours(host, ApprovalKind::Understand, &[approvals::GO_AHEAD_TOPIC])
            {
                let because = approvals::yours_because("the go-ahead");
                approvals::open_and_wait_yours(host, request, because).await?
            } else {
                approvals::open_and_wait(host, request).await?
            }
        }
    };
    let done = match decided.action {
        AnswerAction::Choose { value } if value == LET_THE_MAIN_AGENT_ANSWER => {
            // The main agent answers what comes next in this run; the app saves it for the project.
            if !host.read(main_agent_answers_for_you) {
                answered_by(
                    host,
                    GO_AHEAD,
                    LET_THE_MAIN_AGENT_ANSWER,
                    decider(&decided.by),
                )?;
            }
            approve(host, drafted, decided.by)?;
            StepDone::done("The goal is approved, and the main agent answers from here")
        }
        AnswerAction::Approve => {
            approve(host, drafted, decided.by)?;
            StepDone::done(match main_agent_answered {
                Some((_, 1)) => {
                    "The main agent answered 1 question, and the goal is approved".to_owned()
                }
                Some((_, n)) => {
                    format!("The main agent answered {n} questions, and the goal is approved")
                }
                None => "The goal is approved".to_owned(),
            })
        }
        AnswerAction::ApproveWithEdits => {
            let note = decided.text.unwrap_or_default();
            let goal = round::write_draft(host, file::with_edits(&drafted, &note)).await?;
            approve(host, goal, decided.by)?;
            StepDone::done("The goal is approved with your edits")
        }
        AnswerAction::SendBack => StepDone::done("Sent back for another round"),
        AnswerAction::Abandon => StepDone {
            end: StepEnd::Done,
            summary: Some("You abandoned the goal".to_owned()),
            flow: Flow::Finish(RunOutcome::Abandoned),
        },
        other => {
            return Err(EngineError::Refused(format!(
                "the Understand approval doesn't offer {other:?}"
            )));
        }
    };
    Ok(Some(done))
}

/// The open questions the main agent's brief listed, when it decided Understand wasn't needed.
fn brief_questions(state: &RunState) -> Vec<GoalQuestion> {
    let Some(brief) = brief::of_run(state) else {
        return Vec::new();
    };
    let of = u8::try_from(brief.questions.len()).unwrap_or(u8::MAX);
    brief
        .questions
        .iter()
        .enumerate()
        .map(|(index, question)| GoalQuestion {
            n: u8::try_from(index + 1).unwrap_or(u8::MAX),
            of,
            text: one_line(&question.text),
            options: question
                .options
                .iter()
                .enumerate()
                .map(|(choice, label)| ChoiceOption {
                    key: char::from(b'a' + u8::try_from(choice).unwrap_or(25)),
                    label: one_line(label),
                    recommended: choice == 0,
                    basis: (choice == 0)
                        .then(|| format!("the main agent recommends it · {}", question.topic)),
                })
                .collect(),
            tag: DiffTag::Detail,
            topic: Some(one_line(&question.topic)),
        })
        .collect()
}

/// Your prompt as this round's draft, with the main agent's questions, if any, to shape it.
async fn draft_from_the_prompt(host: &Host, questions: Vec<GoalQuestion>) -> EngineResult<()> {
    round::begin(host)?;
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Settled {
            agreements: Vec::new(),
            assumptions: Vec::new(),
            questions,
            dropped: 0,
        }),
    )?;
    let request = host.read(round::request)?;
    round::write_draft(host, draft::from_prompt(host.run_id(), &request)).await?;
    Ok(())
}

/// Records `Goal::Approved` with the file's SHA-256. The goal is frozen from here: approving
/// again returns the approved hash, and only a goal change with a higher version replaces it.
pub fn approve(host: &Host, file: GoalFile, by: Party) -> EngineResult<String> {
    if file.sha256 != file::sha256(&file) {
        return Err(EngineError::Refused(
            "the goal file's hash doesn't match what it says".to_owned(),
        ));
    }
    let approved = host.read(|state| state.goal.approved.clone());
    if let Some(approved) = &approved
        && approved.version >= file.version
    {
        return Ok(approved.sha256.clone());
    }
    let summary = match file.source {
        GoalSource::Prompt => "Your prompt is the goal".to_owned(),
        GoalSource::Readings => match file.criteria.len() {
            1 => "Goal approved with 1 criterion".to_owned(),
            n => format!("Goal approved with {n} criteria"),
        },
    };
    let sha256 = file.sha256.clone();
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Approved { file, by }),
    )?;
    if approved.is_none() {
        round::phase(host, PhaseStatus::Finished, Some(summary))?;
    }
    // A goal run whose first try began before this go-ahead pursues what was approved.
    let pursuit = host.read(|state| {
        let mut pursuit = state.goal.pursuit.clone()?;
        if !pursuit.criteria.is_empty() {
            return None;
        }
        pursuit.criteria = crate::goal::pursue::planned_criteria(state);
        (!pursuit.criteria.is_empty()).then_some(pursuit)
    });
    if let Some(pursuit) = pursuit {
        host.record(
            None,
            None,
            RunEventKind::Goal(GoalEvent::Set(Box::new(pursuit))),
        )?;
    }
    Ok(sha256)
}

/// One answer as the goal file records it.
pub fn answer_text(question: &GoalQuestion, decided: &ApprovalDecided) -> EngineResult<String> {
    let answer = match &decided.action {
        AnswerAction::Choose { value } => question
            .options
            .iter()
            .find(|option| option.label == *value || option.key.to_string() == *value)
            .map_or_else(|| one_line(value), |option| option.label.clone()),
        AnswerAction::Answer => decided
            .text
            .as_deref()
            .map(one_line)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| EngineError::Refused("your own answer is empty".to_owned()))?,
        other => {
            return Err(EngineError::Refused(format!(
                "a goal question takes an answer, not {other:?}"
            )));
        }
    };
    let asked = one_line(&question.text);
    Ok(format!(
        "{}: {answer}",
        asked.trim_end_matches(['?', '.', ':'])
    ))
}

/// What the approval needs from the state, read once.
#[derive(Debug, Clone)]
pub struct View {
    pub run: RunId,
    pub approved: bool,
    pub sent_back: bool,
    pub drafted: Option<GoalFile>,
    pub settled: SettledGoal,
    /// This round's decided question approvals, oldest first, each with the question it
    /// asked.
    pub answered: Vec<(String, ApprovalDecided)>,
    /// This round's decided approval, if it was decided.
    pub decided: Option<ApprovalDecided>,
    /// Reasons the approval question must wait for you even with no question open: differences
    /// in `always_ask` over the question limit, and readings no comparer compared.
    pub waits: u32,
    /// Differences in `always_ask` over the question limit, adopted with their recommended
    /// answers. No judge may settle those in your place when you answer the crew's questions.
    pub held_back: u32,
    /// Differences over the question limit that `approvals.ask_you_about` keeps yours, which
    /// wait for you even when the main agent answers.
    pub held_back_yours: u32,
    /// Sending the draft back runs another round, since the goal's loop has one left.
    pub send_back: bool,
    /// Readings no comparer compared.
    pub unchecked: bool,
    /// The readers and comparers, who must not approve their own goal.
    pub authors: Vec<ModelRef>,
    /// Each difference the comparers tagged, one line each.
    pub tags: Vec<String>,
}

impl View {
    pub fn of(state: &RunState, settings: &Settings, run: &RunId, at: &StepRef) -> Self {
        let goal = &state.goal;
        let mut this_round: Vec<_> = state
            .approvals
            .by_id
            .values()
            .filter(|record| {
                record.opened.kind == ApprovalKind::Understand
                    && record.withdrawn.is_none()
                    && goal.settled_at.is_none_or(|at| record.since > at)
            })
            .collect();
        this_round.sort_by_key(|record| record.since);
        let decisions = this_round.iter().filter_map(|record| {
            let asked = match &record.opened.packet {
                ApprovalPacket::Understand(packet) => packet.questions.first(),
                _ => None,
            };
            let asked = asked.map(|question| question.text.clone());
            Some((asked.unwrap_or_default(), record.decided.clone()?))
        });
        let (answered, approvals): (Vec<_>, Vec<_>) = decisions.partition(|(_, decided)| {
            matches!(
                decided.action,
                AnswerAction::Choose { .. } | AnswerAction::Answer
            )
        });
        let last_approval = approvals
            .into_iter()
            .next_back()
            .map(|(_, decided)| decided);

        let listings: Vec<CompareOutput> = goal
            .compared
            .iter()
            .map(|(_, differences)| CompareOutput {
                agreements: Vec::new(),
                differences: differences.clone(),
            })
            .collect();
        let autopilot = approvals::autopilot::applies(state, settings, ApprovalKind::Understand);
        let understand = materiality::understand_settings(settings, autopilot);
        let settled = materiality::settle(&listings, goal.readings.len(), &understand);
        let held_back = settled.held_back;
        // Nothing waits for you in autopilot, whatever `approvals.ask_you_about` names.
        let held_back_yours = settled
            .over_limit
            .iter()
            .filter(|(tag, topic)| {
                !autopilot
                    && approvals::asks_you_about(settings, &[materiality::tag_name(*tag), topic])
            })
            .count();
        let unchecked = round::mode(state) == RunMode::Crew
            && goal.readings.len() >= 2
            && goal.compared.is_empty();

        let mut authors: Vec<ModelRef> = Vec::new();
        let workers = goal
            .readings
            .iter()
            .map(|(_, worker, _)| worker)
            .chain(goal.compared.iter().map(|(worker, _)| worker));
        for worker in workers {
            if !authors.contains(worker) {
                authors.push(worker.clone());
            }
        }
        let mut tags: Vec<String> = Vec::new();
        for (_, differences) in &goal.compared {
            for difference in differences {
                let line = format!(
                    "{}: {}",
                    materiality::tag_name(difference.tag),
                    one_line(&difference.topic)
                );
                if !tags.contains(&line) {
                    tags.push(line);
                }
            }
        }

        Self {
            run: run.clone(),
            approved: goal.approved.is_some(),
            sent_back: round::sent_back(state),
            drafted: goal.drafted.clone(),
            settled: goal.settled.clone().unwrap_or_default(),
            answered,
            decided: last_approval,
            waits: held_back + u32::from(unchecked),
            held_back,
            held_back_yours: u32::try_from(held_back_yours).unwrap_or(u32::MAX),
            send_back: kinds::another_round(state, at),
            unchecked,
            authors,
            tags,
        }
    }
}

fn packet(drafted: &GoalFile, view: &View, questions: Vec<GoalQuestion>) -> ApprovalPacket {
    ApprovalPacket::Understand(GoalPacket {
        goal: drafted.clone(),
        agreements: view.settled.agreements.clone(),
        assumptions: drafted.assumptions.clone(),
        questions,
    })
}

/// Inline, since `path` names a document stored beside the packet, where the judge reads
/// it, and the goal file lives in the repo.
fn document(drafted: &GoalFile) -> EvidenceDocument {
    EvidenceDocument {
        label: "goal file".to_owned(),
        text: file::render(drafted),
        path: None,
        sha256: Some(drafted.sha256.clone()),
    }
}

/// One approval per question, the remaining ones in its packet. A question always waits for you:
/// the recommended answer is a default you accept with one key, never your rules' or your
/// judge's pick.
pub fn question_request(
    call: &StepCall,
    drafted: &GoalFile,
    view: &View,
    remaining: &[GoalQuestion],
) -> ApprovalRequest {
    let remaining = remaining.to_vec();
    let question = &remaining[0];
    ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Understand,
        title: question_title(question.n, question.of),
        task: None,
        options: Some(options::goal_question(question)),
        facts: EvidenceFacts {
            open_questions: Some(u32::try_from(remaining.len()).unwrap_or(u32::MAX)),
            ..EvidenceFacts::default()
        },
        packet: packet(drafted, view, remaining),
        agent_outputs: view.tags.clone(),
        documents: vec![document(drafted)],
        free_text: FreeText::None,
        authors: view.authors.clone(),
    }
}

/// The question asks, as the merge question does, and counts only when there's more than one.
fn question_title(n: u8, of: u8) -> String {
    const ASKS: &str = "What should the goal say?";
    if of > 1 {
        format!("{ASKS} · {n} of {of}")
    } else {
        ASKS.to_owned()
    }
}

/// The approval question, with the kind's standard options, less "Send back" when no round is left
/// to send the draft back to. Auto may approve it only when nothing waits for you. When the
/// main agent answers the crew's questions nothing does: the judge checks the goal, and the
/// main agent's answers in it, in your place.
pub fn approval_request(
    call: &StepCall,
    drafted: &GoalFile,
    view: &View,
    by_main_agent: bool,
    main_agent_answered: Option<(ModelRef, usize)>,
    style: RunStyle,
) -> ApprovalRequest {
    let mut agent_outputs = view.tags.clone();
    let mut authors = view.authors.clone();
    if by_main_agent && view.unchecked {
        agent_outputs.push("No comparer compared the readings".to_owned());
    }
    if let Some((main_agent, answered)) = main_agent_answered {
        agent_outputs.push(format!(
            "{main_agent} answered {answered} of the open questions for the user. Its \
             answers are the assumptions whose basis starts with \"{}\"; check them against the \
             request",
            argonauts_core::events::goal::MAIN_AGENT_ANSWER
        ));
        if !authors.contains(&main_agent) {
            authors.push(main_agent);
        }
    }
    // When the main agent answers, the judge weighs readings nobody compared, from the line
    // above, and what's over the question limit, unless `ask_you_about` keeps it yours.
    let waits = if by_main_agent {
        view.held_back_yours
    } else {
        view.waits
    };
    ApprovalRequest {
        at: Some(call.at.clone()),
        kind: ApprovalKind::Understand,
        title: match style {
            RunStyle::Workflow => "Run this workflow?",
            RunStyle::Goal => "Pursue this goal?",
        }
        .to_owned(),
        task: None,
        packet: packet(drafted, view, Vec::new()),
        facts: EvidenceFacts {
            open_questions: Some(waits),
            ..EvidenceFacts::default()
        },
        agent_outputs,
        documents: vec![document(drafted)],
        options: go_ahead_options(view.send_back, style),
        free_text: FreeText::None,
        authors,
    }
}

/// The go-ahead's answers, choose-only: in a goal its first one starts rather than runs, and
/// without a round left there's no sending it back.
fn go_ahead_options(send_back: bool, style: RunStyle) -> Option<Vec<AnswerOption>> {
    if send_back && style == RunStyle::Workflow {
        return None;
    }
    Some(
        options::standard(ApprovalKind::Understand)
            .into_iter()
            .filter(|option| send_back || option.action != AnswerAction::SendBack)
            .map(|mut option| {
                if style == RunStyle::Goal && option.action == AnswerAction::Approve {
                    option.label = "Yes, start".to_owned();
                }
                option
            })
            .collect(),
    )
}

/// What the go-ahead question is called in the record of its answers.
pub const GO_AHEAD: &str = "The go-ahead";

/// You chose "Let the main agent decide this and the rest" on one of the goal's questions.
fn lets_the_main_agent_decide(decided: &ApprovalDecided) -> bool {
    matches!(&decided.action, AnswerAction::Choose { value } if value == LET_THE_MAIN_AGENT_DECIDE)
}

/// You chose "Yes, and let the main agent answer" earlier in this run.
pub fn main_agent_answers_for_you(state: &RunState) -> bool {
    state
        .goal
        .answers
        .iter()
        .any(|(_, answer, by)| answer == LET_THE_MAIN_AGENT_ANSWER && *by == Decider::You)
}

/// Who a decision's party is, as a record of an answer names them.
pub fn decider(party: &Party) -> Decider {
    match party {
        // Leaving it was your call.
        Party::User | Party::Declined => Decider::You,
        Party::Rules | Party::Config | Party::ArgonautsCode => Decider::Rules,
        _ => Decider::MainAgent,
    }
}

fn answered_by(host: &Host, question: &str, answer: &str, by: Decider) -> EngineResult<()> {
    host.record(
        None,
        None,
        RunEventKind::Goal(GoalEvent::Answered {
            question: one_line(question),
            answer: one_line(answer),
            by,
        }),
    )?;
    Ok(())
}

/// The questions that go to you, numbered among themselves, so the question counts only those.
fn renumber(questions: Vec<GoalQuestion>) -> Vec<GoalQuestion> {
    let of = u8::try_from(questions.len()).unwrap_or(u8::MAX);
    questions
        .into_iter()
        .enumerate()
        .map(|(index, mut question)| {
            question.n = u8::try_from(index + 1).unwrap_or(u8::MAX);
            question.of = of;
            question
        })
        .collect()
}
