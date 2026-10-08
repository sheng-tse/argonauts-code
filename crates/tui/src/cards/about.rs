//! What a card asks about goes into the conversation just before it opens: a merge's per-card
//! table, a plan's summary, a handoff's candidates, a vendor outage's evidence. The same items,
//! whole, are what Show the details opens in the pager.

use argonauts_core::display_names::DisplayNames;
use argonauts_core::events::card::CardSpec;
use argonauts_core::events::checkpoint::{
    BudgetPacket, ChangeRequestPacket, CheckpointOpened, CheckpointPacket, EscalationPacket,
    GoalChangePacket, GoalPacket, MergePacket, PlanPacket, VendorDownPacket,
};
use argonauts_core::events::goal::GoalFile;
use argonauts_core::events::vendor::HandoffPlan;
use argonauts_core::names::{PlanLabel, RoleName, WorkerRef};
use argonauts_core::settings::budget::BudgetLineKind;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{CardId, DiffStat};
use time::UtcOffset;

use super::item::AboutCell;
use super::plan_workflow;
use super::table::{Column, Table, TableCell};
use super::words;
use crate::cell::Cell;
use crate::format;
use crate::state::RunState;
use crate::theme::Role;

/// An agent's name in a table keeps this many cells, `Haiku 4.5 on Cl…`, before columns go.
const AGENT_FLOOR: usize = 16;

/// The checkpoint with each merge row's owner as the run knows the agent: by the model its
/// session runs, which the transcript names it by, else by its CLI's settings.
pub fn with_run_owners(
    opened: &CheckpointOpened,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> CheckpointOpened {
    let mut opened = opened.clone();
    if let CheckpointPacket::Merge(merge) = &mut opened.packet {
        for row in &mut merge.rows {
            if let Some(owner) = &mut row.owner {
                *owner = run_owner(run, &row.card, owner).unwrap_or_else(|| names.running(owner));
            }
        }
    }
    opened
}

/// The worker of the card's newest owner session on the same CLI.
fn run_owner(run: Option<&RunState>, card: &CardId, owner: &WorkerRef) -> Option<WorkerRef> {
    run?.sessions
        .values()
        .filter(|session| session.card.as_ref() == Some(card) && session.role == RoleName::Worker)
        .filter(|session| session.worker.worker == owner.worker)
        .max_by_key(|session| session.started)
        .map(|session| session.worker.clone())
}

/// What the card is about, with its clock times in UTC and agents by the names their ids
/// give, for callers that know neither the terminal's zone nor the app's names.
pub fn about_cells(opened: &CheckpointOpened, run: Option<&RunState>) -> Vec<Cell> {
    about_cells_local(opened, run, UtcOffset::UTC, &DisplayNames::default())
}

/// What the card is about, with its clock times at `offset`, the terminal's zone.
pub fn about_cells_local(
    opened: &CheckpointOpened,
    run: Option<&RunState>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Vec<Cell> {
    about_item(opened, run, offset, names)
        .map(|item| vec![Cell::Custom(Box::new(item))])
        .unwrap_or_default()
}

/// The item [`about_cells`] draws, for reading it some other way, with clock times at
/// `offset`. `None` when the card has nothing to say before it opens. With the run, a merge's
/// rows name their cards by label, as the chat does; without it, by id.
pub fn about_item(
    opened: &CheckpointOpened,
    run: Option<&RunState>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Option<AboutCell> {
    let item = match &opened.packet {
        CheckpointPacket::Goal(goal) if goal.questions.is_empty() => Some(goal_file(&goal.goal)),
        CheckpointPacket::Goal(goal) => readings(goal),
        CheckpointPacket::Plan(plan) => Some(plan_item(plan, names)),
        CheckpointPacket::Merge(merge) if merge.waits_on_you() => {
            Some(in_the_way_item(merge, Some(IN_THE_WAY_SHOWN)))
        }
        CheckpointPacket::Merge(merge) => Some(merge_item(merge, run, names)),
        CheckpointPacket::Handoff(plan) => Some(handoff_item(plan, names)),
        CheckpointPacket::VendorDown(down) => Some(vendor_down_item(down, offset)),
        CheckpointPacket::Escalation(escalation) => Some(escalation_item(escalation, false)),
        _ => None,
    };
    item.filter(|item| !item.is_empty())
}

/// Everything the card is about, for Show the details, with clock times at `offset`.
pub fn details(opened: &CheckpointOpened, offset: UtcOffset, names: &DisplayNames) -> AboutCell {
    let mut item = match &opened.packet {
        CheckpointPacket::Goal(goal) => goal_file(&goal.goal),
        CheckpointPacket::Plan(plan) => plan_item(plan, names),
        CheckpointPacket::Merge(merge) if merge.waits_on_you() => in_the_way_item(merge, None),
        CheckpointPacket::Merge(merge) => merge_item(merge, None, names),
        CheckpointPacket::Handoff(plan) => handoff_item(plan, names),
        CheckpointPacket::VendorDown(down) => vendor_down_item(down, offset),
        CheckpointPacket::Escalation(escalation) => escalation_item(escalation, true),
        CheckpointPacket::Budget(budget) => budget_item(budget),
        CheckpointPacket::ChangeRequest(request) => change_request_item(request, names),
        CheckpointPacket::GoalChange(change) => goal_change_item(change, names),
        _ => AboutCell::new(opened.title.clone()),
    };
    if item.heading.trim().is_empty() {
        item.heading = opened.title.clone();
    }
    item
}

fn goal_file(goal: &GoalFile) -> AboutCell {
    let mut item = AboutCell::new(format!("Goal · version {}", goal.version));
    item.line(&goal.summary);
    list(
        &mut item,
        "Done when",
        goal.criteria.iter().map(|c| format!("{} {}", c.id, c.text)),
    );
    list(&mut item, "In scope", goal.in_scope.iter().cloned());
    list(&mut item, "Out of scope", goal.out_of_scope.iter().cloned());
    if !goal.assumptions.is_empty() {
        item.blank();
        item.dim("Assumed");
        for assumption in &goal.assumptions {
            item.push(&assumption.text, Role::Text, 1);
            item.push(&assumption.basis, Role::Dim, 2);
        }
    }
    list(&mut item, "Your answers", goal.answers.iter().cloned());
    item.blank();
    item.dim(goal.path.display().to_string());
    item
}

/// Before the first question: what every restatement shares and what was assumed.
fn readings(goal: &GoalPacket) -> Option<AboutCell> {
    let first = goal.questions.iter().map(|question| question.n).min()?;
    if first > 1 || (goal.agreements.is_empty() && goal.assumptions.is_empty()) {
        return None;
    }
    let mut item = AboutCell::new("What the restatements agree on");
    for agreement in &goal.agreements {
        item.push(agreement, Role::Text, 0);
    }
    if !goal.assumptions.is_empty() {
        item.blank();
        item.dim("Assumed");
        for assumption in &goal.assumptions {
            item.push(&assumption.text, Role::Text, 1);
            item.push(&assumption.basis, Role::Dim, 2);
        }
    }
    Some(item)
}

fn plan_item(plan: &PlanPacket, names: &DisplayNames) -> AboutCell {
    let mut item = AboutCell::new("The plan");
    item.line(&plan.summary);
    if let Some(shown) = &plan.workflow {
        plan_workflow::push(&mut item, shown, &plan.summary, names);
    }

    if let Some(judging) = &plan.judging {
        item.blank();
        let mut table = Table::new(vec![
            Column::new("plan", 0),
            Column::new("author", 0).flex(),
            Column::new("score", 0).right(),
            Column::new("spread", 2).right(),
            Column::new("judges", 1),
        ]);
        for tally in &judging.plans {
            let won = judging.winner == Some(tally.label);
            let judges = if tally.dropped > 0 {
                format!("{}, {} dropped", tally.counted, tally.dropped)
            } else {
                tally.counted.to_string()
            };
            table.row(vec![
                TableCell::new(
                    plan_name(tally.label),
                    if won { Role::Success } else { Role::Text },
                ),
                // Named as its draft's session was, by its CLI's default where it named none.
                TableCell::text(names.worker(&names.running(&tally.author))),
                TableCell::text(format!("{:.1}", tally.mean)),
                TableCell::dim(format!("{:.1}", tally.spread)),
                TableCell::dim(judges),
            ]);
        }
        item.table(table);
    }
    let near_tie = plan.near_tie || plan.judging.as_ref().is_some_and(|j| j.near_tie);
    if near_tie {
        let margin = plan.judging.as_ref().map_or(0.0, |judging| judging.margin);
        item.push(
            format!("A near tie, {margin:.1} apart: yours to break"),
            Role::Warning,
            0,
        );
    }

    if !plan.decisions.is_empty() {
        item.blank();
        item.dim("Decisions");
        for decision in &plan.decisions {
            item.push(
                format!("{}: {}", decision.title, decision.choice),
                Role::Text,
                1,
            );
            item.push(&decision.rationale, Role::Dim, 2);
        }
    }

    if !plan.graph.cards.is_empty() {
        item.blank();
        item.dim("Tasks");
        for (card, depth) in by_depth(&plan.graph.cards) {
            item.push(card_line(card, names), Role::Text, depth + 1);
        }
    }

    list(
        &mut item,
        "Acceptance tests",
        plan.acceptance_tests.iter().cloned(),
    );

    item.blank();
    if plan.budget.is_unlimited() {
        item.dim("No limit set");
    } else {
        let mut budget = Table::new(vec![
            Column::new("limit", 0).flex(),
            Column::new("", 0).right(),
        ]);
        for line in &plan.budget.lines {
            budget.row(vec![
                TableCell::text(budget_line(line.line)),
                TableCell::text(words::amount(&line.amount)),
            ]);
        }
        budget.row(vec![
            TableCell::dim("total"),
            TableCell::text(words::amount(&plan.budget.total)),
        ]);
        item.table(budget);
    }
    if plan.budget.estimate_high > 0.0 {
        item.dim(format!(
            "Estimated {} to {}",
            format::usd(plan.budget.estimate_low),
            format::usd(plan.budget.estimate_high)
        ));
    }

    list(
        &mut item,
        "New dependencies",
        plan.new_dependencies.iter().cloned(),
    );
    if !plan.flags.is_empty() {
        item.blank();
        item.push("Flags", Role::Warning, 0);
        for flag in &plan.flags {
            item.push(flag, Role::Warning, 1);
        }
    }
    item
}

/// Files of yours the conversation names before the rest are left to Show the details.
const IN_THE_WAY_SHOWN: usize = 8;

/// The merge waiting on your files, which ones and why git stops there, or on git's own reason.
fn in_the_way_item(merge: &MergePacket, shown: Option<usize>) -> AboutCell {
    let into = merge.target.as_deref().unwrap_or("main");
    let mut item = AboutCell::new(format!("Waiting to merge into {into}"));
    let mut list = |files: &[String], one: &str, many: String| {
        if files.is_empty() {
            return;
        }
        let why = if files.len() == 1 {
            one.to_owned()
        } else {
            many
        };
        item.push(why, Role::Text, 0);
        let shown = shown.unwrap_or(files.len()).min(files.len());
        for file in &files[..shown] {
            item.push(file.as_str(), Role::Text, 1);
        }
        if files.len() > shown {
            item.push(format!("and {} more", files.len() - shown), Role::Dim, 1);
        }
    };
    list(
        &merge.in_the_way,
        "An untracked file in your checkout is where the run's file goes, so git won't merge \
         over it:",
        format!(
            "{} untracked files in your checkout are where the run's files go, so git won't \
             merge over them:",
            merge.in_the_way.len()
        ),
    );
    list(
        &merge.edited,
        "You have uncommitted edits to a file the run changes, so git won't merge over them:",
        format!(
            "You have uncommitted edits to {} files the run changes, so git won't merge over \
             them:",
            merge.edited.len()
        ),
    );
    if let Some(refused) = &merge.refused {
        item.push(
            format!("Git wouldn't move your checkout of {into} forward:"),
            Role::Text,
            0,
        );
        item.push(refused.as_str(), Role::Text, 1);
    }
    item
}

fn merge_item(merge: &MergePacket, run: Option<&RunState>, names: &DisplayNames) -> AboutCell {
    let into = merge.target.as_deref().unwrap_or("main");
    // The branch's name is plumbing, so the heading names the run.
    let mut item = AboutCell::new(format!("Merge the run into {into}"));
    let mut table = Table::new(vec![
        Column::new("task", 0),
        Column::new("title", 0).flex(),
        Column::new("files", 4).right(),
        Column::new("lines", 0),
        Column::new("worker", 3).cut_to(AGENT_FLOOR),
        Column::new("checks", 0),
        Column::new("review", 1),
    ]);
    for row in &merge.rows {
        let gates = if row.gates_passed {
            TableCell::new("passed", Role::Success)
        } else {
            TableCell::new("failed", Role::Error)
        };
        // Work a weaker replacement did waits for a strong enough review.
        let review = match row.debt.first() {
            Some(debt) => TableCell::new(
                format!("needs {}", names.worker(&debt.needs)),
                Role::Warning,
            ),
            None => TableCell::new(
                words::review(row.review.as_ref()),
                match row.review {
                    Some(ReviewVerdict::Approve) => Role::Text,
                    None => Role::Dim,
                    Some(_) => Role::Warning,
                },
            ),
        };
        let card = run.map_or_else(|| row.card.to_string(), |run| run.card_label(&row.card));
        table.row(vec![
            TableCell::text(card),
            TableCell::text(&row.title),
            TableCell::dim(row.stat.files_changed.to_string()),
            TableCell::text(words::lines(&row.stat)),
            TableCell::dim(
                row.owner
                    .as_ref()
                    .map(|owner| names.worker(owner))
                    .unwrap_or_default(),
            ),
            gates,
            review,
        ]);
    }
    // What Argonauts committed itself, such as the goal record, so the rows add up to the totals.
    if !merge.conductor.is_empty() {
        table.row(vec![
            TableCell::text(""),
            TableCell::text("Run records"),
            TableCell::dim(merge.conductor.files_changed.to_string()),
            TableCell::text(words::lines(&merge.conductor)),
            TableCell::dim("Argonauts Code"),
            TableCell::text(""),
            TableCell::text(""),
        ]);
    }
    item.table(table);
    item.dim(totals(merge.rows.len(), &merge.combined));
    match words::Suite::of(merge) {
        words::Suite::Passed => item.push("Suite passed on the latest main", Role::Success, 0),
        words::Suite::Failed => item.push("Suite didn't pass on the latest main", Role::Error, 0),
        words::Suite::NotRun => item.push("No test suite ran on the latest main", Role::Dim, 0),
    }
    for gate in merge.gates.iter().filter(|gate| !gate.passed) {
        item.push(
            format!("{} failed: {}", gate.name, gate.summary),
            Role::Error,
            1,
        );
    }
    if merge.partial {
        item.push(
            "An agent was stopped early, so the work may be partial",
            Role::Warning,
            0,
        );
    }
    item
}

fn handoff_item(plan: &HandoffPlan, names: &DisplayNames) -> AboutCell {
    let mut item = AboutCell::new(format!("Who can continue {} {}", plan.card, plan.title));
    let mut table = Table::new(vec![
        Column::new("candidate", 0).flex(),
        Column::new("plan usage left", 0),
        Column::new("tasks like this", 1),
        Column::new("est. cost", 2),
    ]);
    for candidate in &plan.candidates {
        table.row(vec![
            TableCell::text(names.worker(&candidate.worker)),
            TableCell::text(&candidate.quota_text),
            TableCell::dim(words::track_record(candidate.track_record.as_ref())),
            TableCell::new(
                words::cost(candidate),
                if candidate.may_run_out || candidate.needs_confirm {
                    Role::Warning
                } else {
                    Role::Dim
                },
            ),
        ]);
    }
    if table.rows.is_empty() {
        item.dim("No other agent can take it under this role's rules.");
        item.dim(format!(
            "To relax the rule for this task, /reassign {} <agent>.",
            plan.card
        ));
    }
    item.table(table);
    item
}

fn vendor_down_item(down: &VendorDownPacket, offset: UtcOffset) -> AboutCell {
    let mut item = AboutCell::new(format!("{} is down", down.vendor));
    item.line(&down.evidence);
    if let Some(at) = down.test_at {
        item.dim(format!(
            "A test task tries it again at {}",
            words::clock(at, offset)
        ));
    }
    let mut table = Table::new(vec![
        Column::new("paused", 0),
        Column::new("title", 0).flex(),
        Column::new("role", 1),
    ]);
    for card in &down.paused {
        table.row(vec![
            TableCell::text(card.card.as_str()),
            TableCell::text(&card.title),
            TableCell::dim(card.role.as_str()),
        ]);
    }
    item.table(table);
    item
}

/// The card names the last failure itself, so the conversation gets it only in `full`.
fn escalation_item(escalation: &EscalationPacket, full: bool) -> AboutCell {
    let mut item = AboutCell::new(format!(
        "{} {} · attempts",
        escalation.card, escalation.title
    ));
    for (n, attempt) in escalation.attempts.iter().enumerate() {
        item.push(format!("{}. {attempt}", n + 1), Role::Text, 0);
    }
    if full {
        item.blank();
        item.labeled("Last failure", &escalation.last_failure);
    }
    item
}

fn budget_item(budget: &BudgetPacket) -> AboutCell {
    let whose = budget
        .card
        .as_ref()
        .map_or_else(|| "The run".to_owned(), ToString::to_string);
    let mut item = AboutCell::new(format!(
        "{whose} hit its {} limit",
        words::dimension(budget.dimension)
    ));
    item.line(words::budget_reading(&budget.status, budget.dimension));
    if let Some(reserve) = &budget.reserve_left {
        item.dim(format!("Reserve left: {}", words::amount(reserve)));
    }
    item
}

fn change_request_item(request: &ChangeRequestPacket, names: &DisplayNames) -> AboutCell {
    let mut item = AboutCell::new(format!(
        "{} asks to change {}",
        words::party(&request.from, names),
        request.card
    ));
    item.line(&request.text);
    list(&mut item, "Paths", request.paths.iter().cloned());
    item.blank();
    item.labeled("Why it's yours", &request.why_you);
    if !request.paused.is_empty() {
        item.labeled("Paused", ids(&request.paused));
    }
    item
}

fn goal_change_item(change: &GoalChangePacket, names: &DisplayNames) -> AboutCell {
    let mut item = AboutCell::new(format!(
        "{} asks to change the goal",
        words::party(&change.from, names)
    ));
    item.line(&change.text);
    item.blank();
    let goal = goal_file(&change.goal);
    item.dim(goal.heading.clone());
    item.blocks.extend(goal.blocks);
    item
}

fn list(item: &mut AboutCell, label: &str, rows: impl Iterator<Item = String>) {
    let rows: Vec<String> = rows.collect();
    if rows.is_empty() {
        return;
    }
    item.blank();
    item.dim(label);
    for row in rows {
        item.push(row, Role::Text, 1);
    }
}

/// `Plan 1`, as the judges saw it.
fn plan_name(label: PlanLabel) -> String {
    format!("Plan {label}")
}

fn budget_line(line: BudgetLineKind) -> &'static str {
    match line {
        BudgetLineKind::GoalAndPlan => "goal and plan",
        BudgetLineKind::Spikes => "experiments",
        BudgetLineKind::Contracts => "contracts",
        BudgetLineKind::Cards => "tasks",
        BudgetLineKind::Review => "review",
        BudgetLineKind::Integration => "integration",
        BudgetLineKind::Reserve => "reserve",
    }
}

/// `card-002 Redis store · GPT-6.1 Sol on Codex · reviewed by Opus 5.5 on Claude Code`.
fn card_line(card: &CardSpec, names: &DisplayNames) -> String {
    let mut line = format!("{} {}", card.id, card.title);
    // Named as their sessions will be, by the CLI's default where the plan names no model.
    if let Some(owner) = &card.owner {
        line.push_str(&format!(" · {}", names.worker(&names.running(owner))));
    }
    if !card.reviewers.is_empty() {
        let reviewers: Vec<String> = card
            .reviewers
            .iter()
            .map(|reviewer| names.worker(&names.running(reviewer)))
            .collect();
        line.push_str(&format!(" · reviewed by {}", words::joined(&reviewers)));
    }
    line
}

/// Cards by how long a chain of dependencies each waits on, in graph order within a level.
fn by_depth(cards: &[CardSpec]) -> Vec<(&CardSpec, usize)> {
    let mut depth: Vec<Option<usize>> = vec![None; cards.len()];
    // Each pass settles the cards whose dependencies are settled; a cycle or a missing card
    // leaves the rest at depth 0.
    for _ in 0..cards.len() {
        let mut changed = false;
        for (index, card) in cards.iter().enumerate() {
            if depth[index].is_some() {
                continue;
            }
            let parents: Option<Vec<usize>> = card
                .depends_on
                .iter()
                .filter_map(|id| cards.iter().position(|other| &other.id == id))
                .map(|parent| depth[parent])
                .collect();
            if let Some(parents) = parents {
                depth[index] = Some(parents.into_iter().max().map_or(0, |d| d + 1));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut order: Vec<(&CardSpec, usize)> = cards
        .iter()
        .zip(depth)
        .map(|(card, depth)| (card, depth.unwrap_or(0)))
        .collect();
    order.sort_by_key(|(_, depth)| *depth);
    order
}

fn totals(cards: usize, combined: &DiffStat) -> String {
    format!(
        "{} · {}",
        words::plural(u32::try_from(cards).unwrap_or(u32::MAX), "task", "tasks"),
        words::diffstat(combined)
    )
}

fn ids(cards: &[CardId]) -> String {
    cards
        .iter()
        .map(CardId::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
