//! A run's approval as a question, for every `ApprovalPacket` kind: goal questions, goal, plan,
//! merge (with the request-changes checklist), fallback (with its strategy line and the remember
//! toggle), agent down, limit, stuck task, change request, goal change, take-over return,
//! question, and original returned. What a question asks about at length has gone into the
//! conversation already, so the question itself stays short.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::agent_status::FallbackPlan;
use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalOpened, ApprovalPacket, ChangeRequestPacket,
    CouncilPacket, CouncilQuestion, FreeText, GoalChangePacket, GoalPacket, LimitPacket,
    MergePacket, OriginalReturnedPacket, PlanPacket, QuestionPacket, StuckPacket,
    TakeOverReturnPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::names::ModelRef;
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::settings::fallback::FallbackStrategy;
use argonauts_core::steps::StepName;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::steps::understand::DiffTag;
use argonauts_core::{DiffStat, TaskId};
use time::UtcOffset;

use super::options::{first_recommended, question_options};
use super::words;
use crate::bottom::{
    BACK_HINT, CARRY_ON, OptionKind, QuestionAgent, QuestionChip, QuestionOption, QuestionPrompt,
};
use crate::format;
use crate::state::RunState;

/// A goal question shows at most this many chips.
const MAX_CHIPS: u8 = 3;

pub const REMEMBER_LABEL: &str = "Remember this pick";

/// The question with its clock times in UTC and agents by the names their ids give, for callers
/// that know neither the terminal's zone nor the app's names.
pub fn approval_question(
    id: u64,
    opened: &ApprovalOpened,
    run: Option<&RunState>,
) -> QuestionPrompt {
    approval_question_local(id, opened, run, UtcOffset::UTC, &DisplayNames::default())
}

/// The question with its clock times, such as when an agent's test task runs, at `offset`, the
/// terminal's zone.
pub fn approval_question_local(
    id: u64,
    opened: &ApprovalOpened,
    run: Option<&RunState>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> QuestionPrompt {
    let cancel_is_final = opened.kind == ApprovalKind::Stuck;
    let options = question_options(&opened.options, cancel_is_final);
    let title = title(opened, run, names);
    let mut question = QuestionPrompt::new(id, title.clone(), title, options);
    question.selected = first_recommended(&opened.options);
    question.allow_note = opened.free_text != FreeText::None;
    // While your rules or autopilot still decide, it asks nothing and stays off the screen;
    // once it's yours it asks, whatever its kind, and takes the keys.
    question.needs_you = opened.needs_you;
    question.offers_shift_tab = true;
    for (option, answer) in question.options.iter_mut().zip(&opened.options) {
        option.asks_note = carries_words(&answer.action);
    }

    match &opened.packet {
        ApprovalPacket::Understand(goal) => goal_question(&mut question, goal, &opened.options),
        ApprovalPacket::Plan(plan) => plan_question(&mut question, plan, names),
        ApprovalPacket::Merge(merge) => merge_question(&mut question, merge, run, &opened.options),
        ApprovalPacket::Fallback(plan) => fallback_question(&mut question, plan, run, names),
        ApprovalPacket::AgentDown(down) => {
            super::cli_down::question(&mut question, down, &opened.options, offset, names);
        }
        ApprovalPacket::Stuck(stuck) => {
            stuck_question(&mut question, stuck, run, names);
        }
        ApprovalPacket::Limit(limit) => limit_question(&mut question, limit, run, names),
        ApprovalPacket::ChangeRequest(request) => {
            change_request_question(&mut question, request, run, names);
        }
        ApprovalPacket::GoalChange(change) => goal_change_question(&mut question, change, names),
        ApprovalPacket::TakeOverReturn(back) => {
            take_over_return_question(&mut question, back, run, names);
        }
        ApprovalPacket::Question(packet) => asked(&mut question, packet, run, names),
        ApprovalPacket::OriginalReturned(back) => {
            original_returned_question(&mut question, back, names)
        }
        ApprovalPacket::SecondAgent(second) => question.question = Some(second.text.clone()),
        ApprovalPacket::Council(council) => {
            question.question = Some(council_text(council, offset));
            backed_sides(&mut question, council, run, names);
        }
        ApprovalPacket::WorkflowPromotion(_) => {}
    }
    // A title that asks the question already needn't ask it again.
    if question.question.as_deref() == Some(opened.title.trim()) {
        question.question = None;
    }
    // Esc goes back to the list and the question waits; whoever asked decides in your place
    // only when you say so, on the last row.
    question.esc_hint = BACK_HINT.to_owned();
    if carries_on(opened) {
        let asker = question.agent.as_ref().map(|agent| agent.name.clone());
        question.options.push(
            QuestionOption::new(CARRY_ON)
                .with_description(carry_on_words(opened, asker.as_deref())),
        );
    }
    crate::views::issues::shape_approval(&mut question, opened);
    question
}

/// The council's question, with the time you wrote the private words a held message repeats
/// in your zone, which the engine leaves out.
fn council_text(council: &CouncilPacket, offset: UtcOffset) -> String {
    match &council.about {
        CouncilQuestion::Held {
            why: Some(why),
            wrote_at: Some(at),
            ..
        } => why.question_text(Some(&words::clock(*at, offset))),
        CouncilQuestion::HeldPlan {
            why: Some(why),
            wrote_at: Some(at),
            ..
        } => why.plan_question_text(Some(&words::clock(*at, offset))),
        _ => council.text.clone(),
    }
}

/// The seats that back each side of a point left open, after the side's label, as the run's
/// talk last listed them.
fn backed_sides(
    question: &mut QuestionPrompt,
    council: &CouncilPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    let CouncilQuestion::Point { point } = council.about else {
        return;
    };
    let Some(talk) = run
        .and_then(|run| run.talk.as_ref())
        .filter(|talk| talk.step == council.talk)
    else {
        return;
    };
    let sides = talk
        .sides(point)
        .iter()
        .filter(|side| !side.words.is_empty());
    for (option, side) in question.options.iter_mut().zip(sides) {
        option.seats = side
            .backers
            .iter()
            .filter_map(|seat| talk.seat(seat))
            .map(|seat| crate::council::Face::of_seat(seat, names))
            .collect();
    }
}

/// Whether a run's question offers Carry on without me. The draft pull request's doesn't,
/// since its yes posts on GitHub as you and waits for your own answer.
pub fn carries_on(opened: &ApprovalOpened) -> bool {
    !crate::views::issues::waits_for_you(opened)
}

/// The answer Carry on without me sends: the engine takes an option in your place, as
/// `approvals.when_declined` says, and records it as yours to review.
pub fn carry_on_answer() -> AnswerOption {
    AnswerOption {
        key: '0',
        label: CARRY_ON.to_owned(),
        action: AnswerAction::CarryOn,
        recommended: false,
        detail: None,
    }
}

/// What carrying on without you does, on its own row under its option, so it starts with a
/// capital unless it starts with the asking agent's name, which keeps its own spelling: as
/// `approvals.when_declined` said when the question opened. With `decide`, an agent's question
/// goes back to the agent, a CLI that is down or a goal's questions to the main agent, a plan
/// or a goal goes ahead as written, and anything else ends that part safely with its work
/// kept. With `stop`, the part ends: an agent's question tells its agent to stop and report.
fn carry_on_words(opened: &ApprovalOpened, asker: Option<&str>) -> String {
    const KEPT: &str = "kept as an assumption to review";
    let asker = asker.unwrap_or("The agent");
    if opened.when_declined == WhenDeclined::Stop {
        return match &opened.packet {
            ApprovalPacket::Question(_) => {
                format!("{asker} stops and reports what is done and what is left")
            }
            _ => "This part ends here with its work kept; the closing answer says why".to_owned(),
        };
    }
    match &opened.packet {
        ApprovalPacket::Question(_) => format!("{asker} decides and goes on; its pick is {KEPT}"),
        ApprovalPacket::AgentDown(_) => {
            format!("The main agent picks for each task and goes on; {KEPT}")
        }
        ApprovalPacket::Understand(_) => format!("The main agent decides and goes on; {KEPT}"),
        ApprovalPacket::Plan(_) => {
            "The plan goes ahead as written; the merge still asks you".to_owned()
        }
        // A council's own question, as its other options word theirs.
        ApprovalPacket::Council(council) => match council.about {
            CouncilQuestion::Held { .. } => "The seat says it again without your words".to_owned(),
            CouncilQuestion::HeldPlan { .. } => {
                "The note keeper writes the plan again without your words".to_owned()
            }
            CouncilQuestion::Point { .. } => {
                "The point goes the side most of the seats back".to_owned()
            }
        },
        _ => "This part ends safely with its work kept, so nothing merges without you".to_owned(),
    }
}

/// The event's title, except that a question names who asks it as the rest of the app names
/// the agent, `Question from build:fix-money`, or by its model when the run doesn't know the
/// task yet, and never by the task's id.
fn title(opened: &ApprovalOpened, run: Option<&RunState>, names: &DisplayNames) -> String {
    match &opened.packet {
        ApprovalPacket::Question(question) => {
            let asker = agent_line(&question.from, None, &question.task, run, names);
            format!("Question from {}", asker.name)
        }
        ApprovalPacket::AgentDown(down) => super::cli_down::title(down, names),
        _ => opened.title.clone(),
    }
}

/// Answers whose note is what the agents act on, such as the changes to make, so the question asks
/// for it before sending.
fn carries_words(action: &AnswerAction) -> bool {
    matches!(
        action,
        AnswerAction::ApproveWithEdits
            | AnswerAction::SendBack
            | AnswerAction::RequestChanges
            | AnswerAction::Retry
            | AnswerAction::Split
    )
}

/// The merge rows Request changes ticks, in the checklist's order. Empty when the question has no
/// checklist, and then Request changes sends every task back.
pub fn checklist_questions(opened: &ApprovalOpened) -> Vec<TaskId> {
    match &opened.packet {
        ApprovalPacket::Merge(merge) if merge.rows.len() > 1 => {
            merge.rows.iter().map(|row| row.task.clone()).collect()
        }
        _ => Vec::new(),
    }
}

/// Where the fallback question's remember toggle sits, after the event's own options. `None` on any
/// other question, or when there's no candidate to remember.
pub fn remember_toggle(opened: &ApprovalOpened) -> Option<usize> {
    let fallback = matches!(opened.packet, ApprovalPacket::Fallback(_));
    let candidates = opened
        .options
        .iter()
        .any(|option| matches!(option.action, AnswerAction::Candidate { .. }));
    (fallback && candidates).then_some(opened.options.len())
}

fn goal_question(question: &mut QuestionPrompt, goal: &GoalPacket, options: &[AnswerOption]) {
    let Some(current) = goal.questions.iter().min_by_key(|question| question.n) else {
        question.detail.push(goal.goal.summary.clone());
        // A go-ahead's title asks already, as "Run this workflow?" does, so it isn't asked twice.
        if !question.title.trim_end().ends_with('?') {
            question.question = Some("Approve this goal?".into());
        }
        return;
    };
    let of = current.of.max(current.n);
    let tag_of = |n: u8| {
        goal.questions
            .iter()
            .find(|question| question.n == n)
            .map(|question| question.tag)
    };
    let shown = 1..=of.min(MAX_CHIPS);
    question.chips = shown
        .clone()
        .map(|n| {
            let label = match tag_of(n) {
                // Three scope questions read `Scope 1`, `Scope 2`, `Scope 3`, not the same
                // chip three times.
                Some(this) if shown.clone().filter(|&m| tag_of(m) == Some(this)).count() > 1 => {
                    let nth = (1..=n).filter(|&m| tag_of(m) == Some(this)).count();
                    format!("{} {nth}", tag(this))
                }
                Some(this) => tag(this).to_owned(),
                None => format!("Question {n}"),
            };
            QuestionChip {
                label,
                answered: n < current.n,
            }
        })
        .collect();
    if of > MAX_CHIPS {
        question
            .detail
            .push(format!("Question {} of {of}", current.n));
    }
    question.question = Some(current.text.clone());
    // An answer's basis goes on its dim second line, from the event or the question itself.
    // The answers are lettered again once the recommended one moves first, so they find their
    // choice by its words, not its letter.
    for (option, answer) in question.options.iter_mut().zip(options) {
        if option.description.is_some() || option.kind == OptionKind::FreeText {
            continue;
        }
        option.description = current
            .options
            .iter()
            .find(|choice| choice.label == answer.label)
            .and_then(|choice| choice.basis.clone());
    }
}

fn tag(tag: DiffTag) -> &'static str {
    match tag {
        DiffTag::Wording => "Wording",
        DiffTag::Detail => "Detail",
        DiffTag::Scope => "Scope",
        DiffTag::Security => "Security",
        DiffTag::Data => "Data",
        DiffTag::Cost => "Cost",
    }
}

fn plan_question(question: &mut QuestionPrompt, plan: &PlanPacket, names: &DisplayNames) {
    let tasks = u32::try_from(plan.graph.tasks.len()).unwrap_or(u32::MAX);
    // A workflow that drafts no plan has no tasks to count; its phases are the plan.
    let mut summary = match &plan.workflow {
        Some(shown) if tasks == 0 => vec![super::plan_workflow::summary(shown)],
        _ => vec![words::plural(tasks, "task", "tasks")],
    };
    if plan.limit.estimate_high > 0.0 {
        summary.push(format!(
            "estimated {} to {}",
            format::usd(plan.limit.estimate_low),
            format::usd(plan.limit.estimate_high)
        ));
    }
    if plan.limit.is_unlimited() {
        summary.push("no limit set".to_owned());
    } else {
        summary.push(format!("limit {}", words::amount(&plan.limit.total)));
    }
    question.detail.push(summary.join(" · "));
    if let Some(judging) = &plan.judging {
        let winner = judging
            .winner
            .and_then(|label| judging.plans.iter().find(|tally| tally.label == label));
        if let Some(winner) = winner {
            question.detail.push(format!(
                "Plan {} by {} scored best, {:.1} ahead",
                winner.label,
                names.worker(&names.running(&winner.author)),
                judging.margin
            ));
        }
    }
    if plan.near_tie
        || plan
            .judging
            .as_ref()
            .is_some_and(|judging| judging.near_tie)
    {
        question.detail.push("A near tie: yours to break".into());
    }
    match plan.flags.len() {
        0 => {}
        1 => question
            .detail
            .push("A flag above keeps it from auto-approval".into()),
        n => question
            .detail
            .push(format!("{n} flags above keep it from auto-approval")),
    }
    question.question = Some("Approve this plan?".into());
}

/// The cells between the longest choice and the explanations beside the choices, where you
/// and a run changed the same lines.
const BOTH_CHANGED_GAP: usize = 3;

fn merge_question(
    question: &mut QuestionPrompt,
    merge: &MergePacket,
    run: Option<&RunState>,
    options: &[AnswerOption],
) {
    // You and the run changed the same lines, in a folder without git: the title asks which
    // version goes in, and the lines under it say what each side changed.
    if !merge.both_changed.is_empty() {
        question.descriptions_beside = Some(BOTH_CHANGED_GAP);
        question.detail.push(both_changed(merge));
        let checks = match words::Suite::of(merge) {
            words::Suite::Passed => "checks passed on the run's version",
            words::Suite::Failed => "checks failed on the run's version",
            words::Suite::NotRun => "no checks ran on the run's version",
        };
        question.detail.push(format!(
            "{checks} · nothing is written into your folder until you choose"
        ));
        return;
    }
    // Which branch takes the run: its title asks, and each choice says what it does.
    if merge.asks_which_branch {
        return;
    }
    // Waiting on your files: the conversation lists them just above, so the question asks.
    if merge.waits_on_you() {
        let files: Vec<&String> = merge.in_the_way.iter().chain(&merge.edited).collect();
        if let [first, rest @ ..] = files.as_slice() {
            question.detail.push(match rest.len() {
                0 => format!("{first} is in the way"),
                n => format!(
                    "{first} and {} in the way",
                    words::plural(u32::try_from(n).unwrap_or(u32::MAX), "other", "others")
                ),
            });
        } else if let Some(refused) = &merge.refused {
            question.detail.push(format!("Git said: {refused}"));
        }
        question.question = Some(
            if merge.in_the_way.is_empty() {
                "Try again, or merge later?"
            } else {
                "Move them aside and merge, or merge later?"
            }
            .into(),
        );
        return;
    }
    for (option, answer) in question.options.iter_mut().zip(options) {
        if answer.action == AnswerAction::Abandon {
            option.warning = Some("its work is removed".to_owned());
        }
    }
    let tasks = u32::try_from(merge.rows.len()).unwrap_or(u32::MAX);
    // A folder without git has no main for the suite to have run on, and neither has a run
    // that started on no branch.
    let folder = merge.into_your_folder();
    let no_main = folder || merge.target.is_none();
    let mut summary = vec![
        words::plural(tasks, "task", "tasks"),
        if folder {
            words::diffstat_signed(&merge.combined)
        } else {
            words::diffstat(&merge.combined)
        },
    ];
    summary.push(
        match (words::Suite::of(merge), no_main) {
            (words::Suite::Passed, true) => "suite passed",
            (words::Suite::Failed, true) => "suite failed",
            (words::Suite::Passed, false) => "suite passed on the latest main",
            (words::Suite::Failed, false) => "suite failed on the latest main",
            (words::Suite::NotRun, _) => "no test suite set",
        }
        .into(),
    );
    if !merge.rows.is_empty() {
        let approved = merge
            .rows
            .iter()
            .all(|row| row.review == Some(ReviewVerdict::Approve));
        if approved {
            summary.push("every review approved".into());
        }
    }
    let debt = merge.rows.iter().filter(|row| !row.debt.is_empty()).count();
    if debt > 0 {
        let debt = u32::try_from(debt).unwrap_or(u32::MAX);
        summary.push(format!(
            "{} needs a stronger review",
            words::plural(debt, "task", "tasks")
        ));
    }
    // The run's spend as the bottom line counts it; the packet's count can lag behind it.
    let spent = run
        .map(RunState::spend)
        .filter(|spent| spent.cost_usd > 0.0)
        .unwrap_or(merge.spent);
    // Nothing spent says nothing where the run is written into a folder.
    let nothing_spent = spent.cost_usd == 0.0 && spent.total_tokens() == 0;
    if !(folder && nothing_spent && merge.limit.is_none()) {
        summary.push(match &merge.limit {
            Some(limit) => format!("{} of {}", format::spend(&spent), words::amount(limit)),
            None => format!("{} spent", format::spend(&spent)),
        });
    }
    question.detail.push(summary.join(" · "));
    if let Some(again) = &merge.again {
        question
            .detail
            .push(format!("Asked again: {}", again.trim()));
    }
    if merge.partial {
        question
            .detail
            .push("An agent was stopped early, so the work may be partial.".into());
    }
    // Request changes on some tasks ticks the merge rows.
    if merge.rows.len() > 1 {
        let items: Vec<String> = merge
            .rows
            .iter()
            .map(|row| format!("{} {}", row.task, row.title))
            .collect();
        for (index, option) in options.iter().enumerate() {
            if option.action == AnswerAction::RequestChanges
                && let Some(option) = question.options.get_mut(index)
            {
                option.kind = OptionKind::Checklist;
                option.items = items.clone();
            }
        }
    }
}

/// `src/cli.js: yours +3 −1 since the run began, the run's +24 −4 · its other 3 files go in
/// either way`.
fn both_changed(merge: &MergePacket) -> String {
    let files = &merge.both_changed;
    let sum = |stats: &mut dyn Iterator<Item = &DiffStat>| {
        stats.fold(DiffStat::default(), |sum, stat| DiffStat {
            files_changed: sum.files_changed + stat.files_changed,
            insertions: sum.insertions + stat.insertions,
            deletions: sum.deletions + stat.deletions,
        })
    };
    let yours = sum(&mut files.iter().map(|file| &file.yours));
    let runs = sum(&mut files.iter().map(|file| &file.runs));
    let count = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let named = match files.as_slice() {
        [one] => one.path.clone(),
        [first, rest @ ..] => format!(
            "{} and {}",
            first.path,
            words::plural(
                u32::try_from(rest.len()).unwrap_or(u32::MAX),
                "other",
                "others"
            )
        ),
        [] => String::new(),
    };
    let mut line = format!(
        "{named}: yours {} since the run began, the run's {}",
        words::lines_signed(&yours),
        words::lines_signed(&runs)
    );
    match merge.combined.files_changed.saturating_sub(count) {
        0 => {}
        1 => line.push_str(" · its other 1 file goes in either way"),
        others => line.push_str(&format!(" · its other {others} files go in either way")),
    }
    line
}

fn fallback_question(
    question: &mut QuestionPrompt,
    plan: &FallbackPlan,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    question.agent = Some(agent_line(
        &plan.stopped,
        Some("worker"),
        &plan.task,
        run,
        names,
    ));
    question.detail.push(format!(
        "{} stopped: {}",
        names.worker(&plan.stopped),
        plan.reason
    ));
    let checks = if plan.checks_ran {
        "Checks ran on it."
    } else {
        "Checks haven't run on it yet."
    };
    question.detail.push(format!(
        "Work so far is committed: {}, {}. {checks}",
        words::files(plan.work.files_changed),
        words::lines(&plan.work)
    ));
    if let Some(line) = strategy_line(plan, names) {
        question.detail.push(line);
    }
    if !plan.candidates.is_empty() {
        question
            .options
            .push(QuestionOption::toggle(REMEMBER_LABEL, false));
    }
}

/// Which way the stopped agent's tasks move, such as `Claude Code's 3 tasks will be spread
/// across Codex and agy`.
fn strategy_line(plan: &FallbackPlan, names: &DisplayNames) -> Option<String> {
    let stopped = names.cli(&plan.stopped.agent);
    let moving = u32::try_from(plan.also_moving.len() + 1).unwrap_or(u32::MAX);
    let mut others: Vec<String> = Vec::new();
    for candidate in &plan.candidates {
        let name = names.cli(&candidate.worker.agent);
        if !others.contains(&name) {
            others.push(name);
        }
    }
    match plan.strategy {
        FallbackStrategy::Spread if !others.is_empty() => Some(format!(
            "{stopped}'s {} will be spread across {}",
            words::plural(moving, "task", "tasks"),
            words::joined(&others)
        )),
        FallbackStrategy::Race if plan.race_count > 1 => Some(format!(
            "{} fallbacks will take {} at once, and fallback.pick says whose work is kept",
            plan.race_count, plan.task
        )),
        FallbackStrategy::First if moving > 1 => Some(format!(
            "{stopped}'s {} will each go to the first agent that can take them",
            words::plural(moving, "task", "tasks")
        )),
        _ => None,
    }
}

fn stuck_question(
    question: &mut QuestionPrompt,
    stuck: &StuckPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    question.agent = worker_line(run, &stuck.task, names);
    // The packet lists what the tries reported, and one that ended without a report lists
    // nothing, so its workers' sessions count them.
    let workers = run.map_or(0, |run| {
        run.sessions
            .values()
            .filter(|session| {
                session.task.as_ref() == Some(&stuck.task) && session.step == Some(StepName::Build)
            })
            .count()
    });
    let tries = u32::try_from(workers.max(stuck.tries.len())).unwrap_or(u32::MAX);
    // The task goes by its title here, as in the question's own title, never by its id.
    question.detail.push(format!(
        "{} · {}",
        stuck.title,
        words::plural(tries, "try", "tries")
    ));
    question
        .detail
        .push(format!("Last failure: {}", stuck.last_failure));
}

fn limit_question(
    question: &mut QuestionPrompt,
    limit: &LimitPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    let whose = match &limit.task {
        Some(id) => {
            question.agent = worker_line(run, id, names);
            id.to_string()
        }
        None => "The run".to_owned(),
    };
    let mut line = format!(
        "{whose} hit its {} limit: {}",
        words::dimension(limit.dimension),
        words::limit_reading(&limit.status, limit.dimension)
    );
    if let Some(reserve) = &limit.reserve_left {
        line.push_str(&format!(" · reserve left {}", words::amount(reserve)));
    }
    question.detail.push(line);
}

fn change_request_question(
    question: &mut QuestionPrompt,
    request: &ChangeRequestPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    question.agent = party_line(&request.from, &request.task, run, names);
    question.detail.push(request.text.clone());
    if !request.paths.is_empty() {
        question
            .detail
            .push(format!("Paths: {}", request.paths.join(", ")));
    }
    question
        .detail
        .push(format!("Why it's yours: {}", request.why_you));
    if !request.paused.is_empty() {
        let paused: Vec<String> = request.paused.iter().map(ToString::to_string).collect();
        question
            .detail
            .push(format!("Paused until you decide: {}", paused.join(", ")));
    }
    question.question = Some("Accept this change?".into());
}

fn goal_change_question(
    question: &mut QuestionPrompt,
    change: &GoalChangePacket,
    names: &DisplayNames,
) {
    question.detail.push(format!(
        "{} asks: {}",
        words::party(&change.from, names),
        change.text
    ));
    question.detail.push(if change.clarification {
        "A clarification: the success criteria stay the same.".into()
    } else {
        "This changes the success criteria.".into()
    });
    question.question = Some("Accept the change?".into());
}

fn take_over_return_question(
    question: &mut QuestionPrompt,
    back: &TakeOverReturnPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    question.agent = Some(agent_line(
        &back.worker,
        role_of(run, &back.task, &back.worker),
        &back.task,
        run,
        names,
    ));
    let mut line = format!("You changed {} on {}", changed(&back.changed), back.task);
    if let Some(commit) = &back.commit {
        let short: String = commit.chars().take(7).collect();
        line.push_str(&format!(" · committed {short}"));
    }
    question.detail.push(line);
    question.question = Some(format!("Let {} continue?", names.worker(&back.worker)));
}

fn asked(
    question: &mut QuestionPrompt,
    packet: &QuestionPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    question.agent = Some(agent_line(
        &packet.from,
        role_of(run, &packet.task, &packet.from),
        &packet.task,
        run,
        names,
    ));
    question.question = Some(packet.text.clone());
}

fn original_returned_question(
    question: &mut QuestionPrompt,
    back: &OriginalReturnedPacket,
    names: &DisplayNames,
) {
    let mut line = format!(
        "{} since fallback-{} · {}",
        words::plural(back.commits, "commit", "commits"),
        back.since,
        words::lines(&back.changed)
    );
    match back.tests_passed {
        Some(true) => line.push_str(" · tests passed"),
        Some(false) => line.push_str(" · tests failed"),
        None => {}
    }
    question.detail.push(line);
    for debt in &back.debt {
        question
            .detail
            .push(format!("Needs review by {}", names.worker(&debt.needs)));
    }
}

fn changed(stat: &DiffStat) -> String {
    format!(
        "{}, {}",
        words::files(stat.files_changed),
        words::lines(stat)
    )
}

/// `Agent: build:fix-money · Haiku 4.5 on Claude Code`: the agent by its label and model, as
/// its panel row names it. An agent other than the task's worker goes by its own label, such as
/// `review:fix-money`, else its role, and one on a task the run doesn't know yet by its model
/// alone. The task id stays in the agent view and Ctrl+O.
fn agent_line(
    worker: &ModelRef,
    role: Option<&'static str>,
    task: &TaskId,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> QuestionAgent {
    let model = names.worker(&running(worker, task, run, names));
    let label = match role {
        None | Some("worker") => task_label(run, task),
        Some(role) => own_label(worker, task, run).or_else(|| Some(role.to_owned())),
    };
    let (name, detail) = match label {
        Some(label) => (label, model),
        None => (model, String::new()),
    };
    QuestionAgent {
        name,
        worker: worker.to_string(),
        color: None,
        detail,
    }
}

/// What a task goes by: its label, `build:fix-money`. `None` until the run knows the task,
/// since its id belongs to Ctrl+O and the agent view.
fn task_label(run: Option<&RunState>, task: &TaskId) -> Option<String> {
    let label = run?.task_label(task);
    (label != task.to_string()).then_some(label)
}

/// The label of `worker`'s newest session on `task`, as the panel shows it.
fn own_label(worker: &ModelRef, task: &TaskId, run: Option<&RunState>) -> Option<String> {
    run?.sessions_in_order()
        .filter(|session| session.task.as_ref() == Some(task) && session.is_worker(worker))
        .last()?
        .label
        .clone()
}

/// The worker with the model its session on `task` runs, else the one its CLI's settings
/// give, so a question from a CLI on its default model still names the model.
fn running(
    worker: &ModelRef,
    task: &TaskId,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> ModelRef {
    let session = run.and_then(|run| {
        run.sessions_in_order()
            .filter(|session| session.task.as_ref() == Some(task) && session.is_worker(worker))
            .last()
    });
    names.running(session.map_or(worker, |session| &session.worker))
}

fn worker_line(
    run: Option<&RunState>,
    task: &TaskId,
    names: &DisplayNames,
) -> Option<QuestionAgent> {
    let worker = run?.tasks.get(task)?.worker.as_ref()?;
    Some(agent_line(worker, Some("worker"), task, run, names))
}

fn party_line(
    party: &Party,
    task: &TaskId,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> Option<QuestionAgent> {
    match party {
        Party::Agent { worker, .. } => Some(agent_line(
            worker,
            role_of(run, task, worker),
            task,
            run,
            names,
        )),
        _ => None,
    }
}

/// The role `worker` plays on `task`, from the run's sessions.
fn role_of(run: Option<&RunState>, task: &TaskId, worker: &ModelRef) -> Option<&'static str> {
    run?.sessions_in_order()
        .filter(|session| session.task.as_ref() == Some(task) && session.is_worker(worker))
        .last()
        .map(|session| session.role.as_str())
}
