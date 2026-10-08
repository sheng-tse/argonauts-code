//! A run's checkpoint as a card, for every `CheckpointPacket` kind: goal questions, goal, plan,
//! merge (with the request-changes checklist), handoff (with its strategy line and the remember
//! toggle), vendor down, budget, escalation, change request, goal change, take-over return,
//! question, and original returned. What a card asks about at length has gone into the
//! conversation already, so the card itself stays short.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::checkpoint::{
    AnswerAction, AnswerOption, BudgetPacket, ChangeRequestPacket, CheckpointKind,
    CheckpointOpened, CheckpointPacket, EscalationPacket, FreeText, GoalChangePacket, GoalPacket,
    MergePacket, OriginalReturnedPacket, PlanPacket, QuestionPacket, TakeOverReturnPacket,
    VendorDownPacket,
};
use argonauts_core::events::run::Party;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::names::WorkerRef;
use argonauts_core::settings::handoff::HandoffStrategy;
use argonauts_core::steps::StepName;
use argonauts_core::steps::goal::DiffTag;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{CardId, DiffStat};
use time::UtcOffset;

use super::options::{card_options, first_recommended};
use super::words;
use crate::bottom::{CardAgent, CardChip, CardOption, CardPrompt, OptionKind};
use crate::format;
use crate::state::RunState;

/// A goal question card shows at most this many chips.
const MAX_CHIPS: u8 = 3;

pub const REMEMBER_LABEL: &str = "Remember this pick";

/// The card with its clock times in UTC and agents by the names their ids give, for callers
/// that know neither the terminal's zone nor the app's names.
pub fn checkpoint_card(id: u64, opened: &CheckpointOpened, run: Option<&RunState>) -> CardPrompt {
    checkpoint_card_local(id, opened, run, UtcOffset::UTC, &DisplayNames::default())
}

/// The card with its clock times, such as when a vendor's test card runs, at `offset`, the
/// terminal's zone.
pub fn checkpoint_card_local(
    id: u64,
    opened: &CheckpointOpened,
    run: Option<&RunState>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> CardPrompt {
    let cancel_is_final = opened.kind == CheckpointKind::Escalation;
    let options = card_options(&opened.options, cancel_is_final);
    let title = title(opened, run, names);
    let mut card = CardPrompt::new(id, title.clone(), title, options);
    card.selected = first_recommended(&opened.options);
    card.allow_note = opened.free_text != FreeText::None;
    // While your rules or full auto still decide, it asks nothing and stays off the screen;
    // once it's yours it asks, whatever its kind, and takes the keys.
    card.needs_you = opened.needs_you;
    card.offers_shift_tab = true;
    for (option, answer) in card.options.iter_mut().zip(&opened.options) {
        option.asks_note = carries_words(&answer.action);
    }

    match &opened.packet {
        CheckpointPacket::Goal(goal) => goal_card(&mut card, goal, &opened.options),
        CheckpointPacket::Plan(plan) => plan_card(&mut card, plan, names),
        CheckpointPacket::Merge(merge) => merge_card(&mut card, merge, run, &opened.options),
        CheckpointPacket::Handoff(plan) => handoff_card(&mut card, plan, run, names),
        CheckpointPacket::VendorDown(down) => vendor_down_card(&mut card, down, offset),
        CheckpointPacket::Escalation(escalation) => {
            escalation_card(&mut card, escalation, run, names);
        }
        CheckpointPacket::Budget(budget) => budget_card(&mut card, budget, run, names),
        CheckpointPacket::ChangeRequest(request) => {
            change_request_card(&mut card, request, run, names);
        }
        CheckpointPacket::GoalChange(change) => goal_change_card(&mut card, change, names),
        CheckpointPacket::TakeOverReturn(back) => {
            take_over_return_card(&mut card, back, run, names);
        }
        CheckpointPacket::Question(question) => question_card(&mut card, question, run, names),
        CheckpointPacket::OriginalReturned(back) => original_returned_card(&mut card, back, names),
        CheckpointPacket::WorkflowPromotion(_) => {}
    }
    // A title that asks the question already needn't ask it again.
    if card.question.as_deref() == Some(opened.title.trim()) {
        card.question = None;
    }
    card
}

/// The event's title, except that a question names who asks it as the rest of the app names
/// the agent, `Question from build:fix-money`, or by its model when the run doesn't know the
/// card yet, and never by the card's id.
fn title(opened: &CheckpointOpened, run: Option<&RunState>, names: &DisplayNames) -> String {
    match &opened.packet {
        CheckpointPacket::Question(question) => {
            let asker = agent_line(&question.from, None, &question.card, run, names);
            format!("Question from {}", asker.name)
        }
        _ => opened.title.clone(),
    }
}

/// Answers whose note is what the agents act on, such as the changes to make, so the card asks
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

/// The merge rows Request changes ticks, in the checklist's order. Empty when the card has no
/// checklist, and then Request changes sends every card back.
pub fn checklist_cards(opened: &CheckpointOpened) -> Vec<CardId> {
    match &opened.packet {
        CheckpointPacket::Merge(merge) if merge.rows.len() > 1 => {
            merge.rows.iter().map(|row| row.card.clone()).collect()
        }
        _ => Vec::new(),
    }
}

/// Where the handoff card's remember toggle sits, after the event's own options. `None` on any
/// other card, or when there's no candidate to remember.
pub fn remember_toggle(opened: &CheckpointOpened) -> Option<usize> {
    let handoff = matches!(opened.packet, CheckpointPacket::Handoff(_));
    let candidates = opened
        .options
        .iter()
        .any(|option| matches!(option.action, AnswerAction::Candidate { .. }));
    (handoff && candidates).then_some(opened.options.len())
}

fn goal_card(card: &mut CardPrompt, goal: &GoalPacket, options: &[AnswerOption]) {
    let Some(current) = goal.questions.iter().min_by_key(|question| question.n) else {
        card.detail.push(goal.goal.summary.clone());
        // A go-ahead's title asks already, as "Run this workflow?" does, so it isn't asked twice.
        if !card.title.trim_end().ends_with('?') {
            card.question = Some("Approve this goal?".into());
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
    card.chips = shown
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
            CardChip {
                label,
                answered: n < current.n,
            }
        })
        .collect();
    if of > MAX_CHIPS {
        card.detail.push(format!("Question {} of {of}", current.n));
    }
    card.question = Some(current.text.clone());
    // An answer's basis goes on its dim second line, from the event or the question itself.
    // The answers are lettered again once the recommended one moves first, so they find their
    // choice by its words, not its letter.
    for (option, answer) in card.options.iter_mut().zip(options) {
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

fn plan_card(card: &mut CardPrompt, plan: &PlanPacket, names: &DisplayNames) {
    let cards = u32::try_from(plan.graph.cards.len()).unwrap_or(u32::MAX);
    // A workflow that drafts no plan has no cards to count; its phases are the plan.
    let mut summary = match &plan.workflow {
        Some(shown) if cards == 0 => vec![super::plan_workflow::summary(shown)],
        _ => vec![words::plural(cards, "task", "tasks")],
    };
    if plan.budget.estimate_high > 0.0 {
        summary.push(format!(
            "estimated {} to {}",
            format::usd(plan.budget.estimate_low),
            format::usd(plan.budget.estimate_high)
        ));
    }
    if plan.budget.is_unlimited() {
        summary.push("no limit set".to_owned());
    } else {
        summary.push(format!("limit {}", words::amount(&plan.budget.total)));
    }
    card.detail.push(summary.join(" · "));
    if let Some(judging) = &plan.judging {
        let winner = judging
            .winner
            .and_then(|label| judging.plans.iter().find(|tally| tally.label == label));
        if let Some(winner) = winner {
            card.detail.push(format!(
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
        card.detail.push("A near tie: yours to break".into());
    }
    match plan.flags.len() {
        0 => {}
        1 => card
            .detail
            .push("A flag above keeps it from auto-approval".into()),
        n => card
            .detail
            .push(format!("{n} flags above keep it from auto-approval")),
    }
    card.question = Some("Approve this plan?".into());
}

fn merge_card(
    card: &mut CardPrompt,
    merge: &MergePacket,
    run: Option<&RunState>,
    options: &[AnswerOption],
) {
    // Waiting on your files: the conversation lists them just above, so the card asks.
    if merge.waits_on_you() {
        let files: Vec<&String> = merge.in_the_way.iter().chain(&merge.edited).collect();
        if let [first, rest @ ..] = files.as_slice() {
            card.detail.push(match rest.len() {
                0 => format!("{first} is in the way"),
                n => format!(
                    "{first} and {} in the way",
                    words::plural(u32::try_from(n).unwrap_or(u32::MAX), "other", "others")
                ),
            });
        } else if let Some(refused) = &merge.refused {
            card.detail.push(format!("Git said: {refused}"));
        }
        card.question = Some(
            if merge.in_the_way.is_empty() {
                "Try again, or merge later?"
            } else {
                "Move them aside and merge, or merge later?"
            }
            .into(),
        );
        return;
    }
    let cards = u32::try_from(merge.rows.len()).unwrap_or(u32::MAX);
    let mut summary = vec![
        words::plural(cards, "task", "tasks"),
        words::diffstat(&merge.combined),
    ];
    summary.push(
        match words::Suite::of(merge) {
            words::Suite::Passed => "suite passed on the latest main",
            words::Suite::Failed => "suite failed on the latest main",
            words::Suite::NotRun => "no test suite set",
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
    // The run's spend as the mode line counts it; the packet's count can lag behind it.
    let spent = run
        .map(RunState::spend)
        .filter(|spent| spent.cost_usd > 0.0)
        .unwrap_or(merge.spent);
    summary.push(match &merge.limit {
        Some(limit) => format!("{} of {}", format::spend(&spent), words::amount(limit)),
        None => format!("{} spent", format::spend(&spent)),
    });
    card.detail.push(summary.join(" · "));
    if merge.partial {
        card.detail
            .push("An agent was stopped early, so the work may be partial.".into());
    }
    // Request changes on some cards ticks the merge rows.
    if merge.rows.len() > 1 {
        let items: Vec<String> = merge
            .rows
            .iter()
            .map(|row| format!("{} {}", row.card, row.title))
            .collect();
        for (index, option) in options.iter().enumerate() {
            if option.action == AnswerAction::RequestChanges
                && let Some(option) = card.options.get_mut(index)
            {
                option.kind = OptionKind::Checklist;
                option.items = items.clone();
            }
        }
    }
}

fn handoff_card(
    card: &mut CardPrompt,
    plan: &HandoffPlan,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    card.agent = Some(agent_line(
        &plan.stopped,
        Some("worker"),
        &plan.card,
        run,
        names,
    ));
    card.detail.push(format!(
        "{} stopped: {}",
        names.worker(&plan.stopped),
        plan.reason
    ));
    let gates = if plan.gates_ran {
        "Checks ran on it."
    } else {
        "Checks haven't run on it yet."
    };
    card.detail.push(format!(
        "Work so far is committed: {}, {}. {gates}",
        words::files(plan.work.files_changed),
        words::lines(&plan.work)
    ));
    if let Some(line) = strategy_line(plan, names) {
        card.detail.push(line);
    }
    if !plan.candidates.is_empty() {
        card.options.push(CardOption::toggle(REMEMBER_LABEL, false));
    }
}

/// Which way the stopped vendor's cards move, such as `Claude Code's 3 cards will be spread
/// across Codex and Antigravity CLI`.
fn strategy_line(plan: &HandoffPlan, names: &DisplayNames) -> Option<String> {
    let vendor = names.harness(&plan.stopped.worker);
    let moving = u32::try_from(plan.also_moving.len() + 1).unwrap_or(u32::MAX);
    let mut vendors: Vec<String> = Vec::new();
    for candidate in &plan.candidates {
        let name = names.harness(&candidate.worker.worker);
        if !vendors.contains(&name) {
            vendors.push(name);
        }
    }
    match plan.strategy {
        HandoffStrategy::Spread if !vendors.is_empty() => Some(format!(
            "{vendor}'s {} will be spread across {}",
            words::plural(moving, "task", "tasks"),
            words::joined(&vendors)
        )),
        HandoffStrategy::Race if plan.race_count > 1 => Some(format!(
            "{} fallbacks will take {} at once, and fallback.pick says whose work is kept",
            plan.race_count, plan.card
        )),
        HandoffStrategy::First if moving > 1 => Some(format!(
            "{vendor}'s {} will each go to the first agent that can take them",
            words::plural(moving, "task", "tasks")
        )),
        _ => None,
    }
}

fn vendor_down_card(card: &mut CardPrompt, down: &VendorDownPacket, offset: UtcOffset) {
    let mut evidence = down.evidence.clone();
    if let Some(at) = down.test_at {
        evidence.push_str(&format!(" · test task at {}", words::clock(at, offset)));
    }
    card.detail.push(evidence);
    if !down.paused.is_empty() {
        let paused: Vec<String> = down
            .paused
            .iter()
            .map(|paused| {
                format!(
                    "{} {} ({})",
                    paused.card,
                    paused.title,
                    paused.role.as_str()
                )
            })
            .collect();
        card.detail
            .push(format!("Paused with work committed: {}", paused.join(", ")));
    }
}

fn escalation_card(
    card: &mut CardPrompt,
    escalation: &EscalationPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    card.agent = owner_line(run, &escalation.card, names);
    // The packet lists what the attempts reported, and one that ended without a report lists
    // nothing, so its owners' sessions count them.
    let owners = run.map_or(0, |run| {
        run.sessions
            .values()
            .filter(|session| {
                session.card.as_ref() == Some(&escalation.card)
                    && session.step == Some(StepName::Build)
            })
            .count()
    });
    let attempts = u32::try_from(owners.max(escalation.attempts.len())).unwrap_or(u32::MAX);
    card.detail.push(format!(
        "{} {} · {}",
        escalation.card,
        escalation.title,
        words::plural(attempts, "attempt", "attempts")
    ));
    card.detail
        .push(format!("Last failure: {}", escalation.last_failure));
}

fn budget_card(
    card: &mut CardPrompt,
    budget: &BudgetPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    let whose = match &budget.card {
        Some(id) => {
            card.agent = owner_line(run, id, names);
            id.to_string()
        }
        None => "The run".to_owned(),
    };
    let mut line = format!(
        "{whose} hit its {} limit: {}",
        words::dimension(budget.dimension),
        words::budget_reading(&budget.status, budget.dimension)
    );
    if let Some(reserve) = &budget.reserve_left {
        line.push_str(&format!(" · reserve left {}", words::amount(reserve)));
    }
    card.detail.push(line);
}

fn change_request_card(
    card: &mut CardPrompt,
    request: &ChangeRequestPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    card.agent = party_line(&request.from, &request.card, run, names);
    card.detail.push(request.text.clone());
    if !request.paths.is_empty() {
        card.detail
            .push(format!("Paths: {}", request.paths.join(", ")));
    }
    card.detail
        .push(format!("Why it's yours: {}", request.why_you));
    if !request.paused.is_empty() {
        let paused: Vec<String> = request.paused.iter().map(ToString::to_string).collect();
        card.detail
            .push(format!("Paused until you decide: {}", paused.join(", ")));
    }
    card.question = Some("Accept this change?".into());
}

fn goal_change_card(card: &mut CardPrompt, change: &GoalChangePacket, names: &DisplayNames) {
    card.detail.push(format!(
        "{} asks: {}",
        words::party(&change.from, names),
        change.text
    ));
    card.detail.push(if change.clarification {
        "A clarification: the success criteria stay the same.".into()
    } else {
        "This changes the success criteria.".into()
    });
    card.question = Some("Accept the change?".into());
}

fn take_over_return_card(
    card: &mut CardPrompt,
    back: &TakeOverReturnPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    card.agent = Some(agent_line(
        &back.owner,
        role_of(run, &back.card, &back.owner),
        &back.card,
        run,
        names,
    ));
    let mut line = format!("You changed {} on {}", changed(&back.changed), back.card);
    if let Some(commit) = &back.commit {
        let short: String = commit.chars().take(7).collect();
        line.push_str(&format!(" · committed {short}"));
    }
    card.detail.push(line);
    card.question = Some(format!("Let {} continue?", names.worker(&back.owner)));
}

fn question_card(
    card: &mut CardPrompt,
    question: &QuestionPacket,
    run: Option<&RunState>,
    names: &DisplayNames,
) {
    card.agent = Some(agent_line(
        &question.from,
        role_of(run, &question.card, &question.from),
        &question.card,
        run,
        names,
    ));
    card.question = Some(question.text.clone());
}

fn original_returned_card(
    card: &mut CardPrompt,
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
    card.detail.push(line);
    for debt in &back.debt {
        card.detail
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
/// its panel row names it. An agent other than the card's owner goes by its own label, such as
/// `review:fix-money`, else its role, and one on a card the run doesn't know yet by its model
/// alone. The card id stays in the agent view and Ctrl+O.
fn agent_line(
    worker: &WorkerRef,
    role: Option<&'static str>,
    card: &CardId,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> CardAgent {
    let model = names.worker(&running(worker, card, run, names));
    let label = match role {
        None | Some("worker") => card_label(run, card),
        Some(role) => own_label(worker, card, run).or_else(|| Some(role.to_owned())),
    };
    let (name, detail) = match label {
        Some(label) => (label, model),
        None => (model, String::new()),
    };
    CardAgent {
        name,
        worker: worker.to_string(),
        color: None,
        detail,
    }
}

/// What a card goes by: its label, `build:fix-money`. `None` until the run knows the card,
/// since its id belongs to Ctrl+O and the agent view.
fn card_label(run: Option<&RunState>, card: &CardId) -> Option<String> {
    let label = run?.card_label(card);
    (label != card.to_string()).then_some(label)
}

/// The label of `worker`'s newest session on `card`, as the panel shows it.
fn own_label(worker: &WorkerRef, card: &CardId, run: Option<&RunState>) -> Option<String> {
    run?.sessions_in_order()
        .filter(|session| session.card.as_ref() == Some(card) && session.is_worker(worker))
        .last()?
        .label
        .clone()
}

/// The worker with the model its session on `card` runs, else the one its CLI's settings
/// give, so a question from a CLI on its default model still names the model.
fn running(
    worker: &WorkerRef,
    card: &CardId,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> WorkerRef {
    let session = run.and_then(|run| {
        run.sessions_in_order()
            .filter(|session| session.card.as_ref() == Some(card) && session.is_worker(worker))
            .last()
    });
    names.running(session.map_or(worker, |session| &session.worker))
}

fn owner_line(run: Option<&RunState>, card: &CardId, names: &DisplayNames) -> Option<CardAgent> {
    let owner = run?.cards.get(card)?.owner.as_ref()?;
    Some(agent_line(owner, Some("worker"), card, run, names))
}

fn party_line(
    party: &Party,
    card: &CardId,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> Option<CardAgent> {
    match party {
        Party::Agent { worker, .. } => Some(agent_line(
            worker,
            role_of(run, card, worker),
            card,
            run,
            names,
        )),
        _ => None,
    }
}

/// The role `worker` plays on `card`, from the run's sessions.
fn role_of(run: Option<&RunState>, card: &CardId, worker: &WorkerRef) -> Option<&'static str> {
    run?.sessions_in_order()
        .filter(|session| session.card.as_ref() == Some(card) && session.is_worker(worker))
        .last()
        .map(|session| session.role.as_str())
}
