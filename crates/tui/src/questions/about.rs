//! What a question asks about goes into the conversation just before it opens: a merge's per-task
//! table, a plan's summary, a fallback's candidates, an agent outage's evidence. The same items,
//! whole, are what Show the details opens in the pager.

use argonauts_core::base::names::{ModelRef, PlanLabel, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::runs::events::agent_status::FallbackPlan;
use argonauts_core::runs::events::approval::{
    AgentDownPacket, ApprovalOpened, ApprovalPacket, ChangeRequestPacket, GoalChangePacket,
    GoalPacket, LimitPacket, MergePacket, MergeQuestion, PlanPacket, StuckPacket,
};
use argonauts_core::runs::events::goal::GoalFile;
use argonauts_core::runs::events::task::TaskSpec;
use argonauts_core::runs::hidden_git::MergedInto;
use argonauts_core::settings::limit::LimitLineKind;
use argonauts_core::workflows::steps::judgment::ReviewVerdict;
use argonauts_core::{DiffStat, TaskId};
use time::UtcOffset;

use super::item::AboutCell;
use super::plan_workflow;
use super::table::{Column, Table, TableCell};
use super::words;
use crate::base::format;
use crate::base::theme::Role;
use crate::bottom::picker::one_line;
use crate::cells::cell::Cell;
use crate::frame::state::RunState;

/// An agent's name in a table keeps this many cells, `Haiku 4.5 on Cl…`, before columns go.
const AGENT_FLOOR: usize = 16;

/// The approval with each merge row's worker as the run knows the agent: by the model its
/// session runs, which the transcript names it by, else by its CLI's settings.
pub fn with_run_workers(
    opened: &ApprovalOpened,
    run: Option<&RunState>,
    names: &DisplayNames,
) -> ApprovalOpened {
    let mut opened = opened.clone();
    if let ApprovalPacket::Merge(merge) = &mut opened.packet {
        for row in &mut merge.rows {
            if let Some(worker) = &mut row.worker {
                *worker =
                    run_worker(run, &row.task, worker).unwrap_or_else(|| names.running(worker));
            }
        }
    }
    opened
}

/// The worker of the task's newest worker session on the same CLI.
fn run_worker(run: Option<&RunState>, task: &TaskId, worker: &ModelRef) -> Option<ModelRef> {
    run?.sessions
        .values()
        .filter(|session| session.task.as_ref() == Some(task) && session.role == RoleName::Worker)
        .filter(|session| session.worker.agent == worker.agent)
        .max_by_key(|session| session.started)
        .map(|session| session.worker.clone())
}

/// What the question is about, with its clock times in UTC and agents by the names their ids
/// give, for callers that know neither the terminal's zone nor the app's names.
pub fn about_cells(opened: &ApprovalOpened, run: Option<&RunState>) -> Vec<Cell> {
    about_cells_local(opened, run, UtcOffset::UTC, &DisplayNames::default())
}

/// What the question is about, with its clock times at `offset`, the terminal's zone.
pub fn about_cells_local(
    opened: &ApprovalOpened,
    run: Option<&RunState>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Vec<Cell> {
    about_item(opened, run, offset, names)
        .map(|item| vec![Cell::Custom(Box::new(item))])
        .unwrap_or_default()
}

/// The item [`about_cells`] draws, for reading it some other way, with clock times at
/// `offset`. `None` when the question has nothing to say before it opens. With the run, a merge's
/// rows name their tasks by label, as the conversation does; without it, by id.
pub fn about_item(
    opened: &ApprovalOpened,
    run: Option<&RunState>,
    offset: UtcOffset,
    names: &DisplayNames,
) -> Option<AboutCell> {
    let item = match &opened.packet {
        ApprovalPacket::Understand(goal) if goal.questions.is_empty() => {
            Some(goal_file(&goal.goal))
        }
        ApprovalPacket::Understand(goal) => readings(goal),
        ApprovalPacket::Plan(plan) => Some(plan_item(plan, names)),
        // The merge question it follows showed the same evidence just above.
        ApprovalPacket::Merge(merge) if merge.follows_the_merge_question() => None,
        ApprovalPacket::Merge(merge) if merge.waits_on_you() => {
            Some(in_the_way_item(merge, Some(IN_THE_WAY_SHOWN)))
        }
        ApprovalPacket::Merge(merge) => Some(merge_item(merge, run, names)),
        ApprovalPacket::Fallback(plan) => Some(fallback_item(plan, names)),
        ApprovalPacket::AgentDown(down) => Some(agent_down_item(down, offset, names)),
        ApprovalPacket::Stuck(stuck) => Some(stuck_item(stuck, false)),
        _ => None,
    };
    item.filter(|item| !item.is_empty())
}

/// Everything the question is about, for Show the details, with clock times at `offset`.
pub fn details(opened: &ApprovalOpened, offset: UtcOffset, names: &DisplayNames) -> AboutCell {
    let mut item = match &opened.packet {
        ApprovalPacket::Understand(goal) => goal_file(&goal.goal),
        ApprovalPacket::Plan(plan) => plan_item(plan, names),
        ApprovalPacket::Merge(merge)
            if merge.waits_on_you() && !merge.follows_the_merge_question() =>
        {
            in_the_way_item(merge, None)
        }
        ApprovalPacket::Merge(merge) => merge_item(merge, None, names),
        ApprovalPacket::Fallback(plan) => fallback_item(plan, names),
        ApprovalPacket::AgentDown(down) => agent_down_item(down, offset, names),
        ApprovalPacket::Stuck(stuck) => stuck_item(stuck, true),
        ApprovalPacket::Limit(limit) => limit_item(limit),
        ApprovalPacket::ChangeRequest(request) => change_request_item(request, names),
        ApprovalPacket::GoalChange(change) => goal_change_item(change, names),
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

/// Before the first question: what every reading shares and what was assumed.
fn readings(goal: &GoalPacket) -> Option<AboutCell> {
    let first = goal.questions.iter().map(|question| question.n).min()?;
    if first > 1 || (goal.agreements.is_empty() && goal.assumptions.is_empty()) {
        return None;
    }
    let mut item = AboutCell::new("What the readings agree on");
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
            Role::Warn,
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

    if !plan.graph.tasks.is_empty() {
        item.blank();
        item.dim("Tasks");
        for (task, depth) in by_depth(&plan.graph.tasks) {
            item.push(task_line(task, names), Role::Text, depth + 1);
        }
    }

    list(
        &mut item,
        "Acceptance tests",
        plan.acceptance_tests.iter().cloned(),
    );

    item.blank();
    if plan.limit.is_unlimited() {
        item.dim("No limit set");
    } else {
        let mut limit = Table::new(vec![
            Column::new("limit", 0).flex(),
            Column::new("", 0).right(),
        ]);
        for line in &plan.limit.lines {
            limit.row(vec![
                TableCell::text(limit_line(line.line)),
                TableCell::text(words::amount(&line.amount)),
            ]);
        }
        limit.row(vec![
            TableCell::dim("total"),
            TableCell::text(words::amount(&plan.limit.total)),
        ]);
        item.table(limit);
    }
    if plan.limit.estimate_high > 0.0 {
        item.dim(format!(
            "Estimated {} to {}",
            format::usd(plan.limit.estimate_low),
            format::usd(plan.limit.estimate_high)
        ));
    }

    list(
        &mut item,
        "New dependencies",
        plan.new_dependencies.iter().cloned(),
    );
    if !plan.flags.is_empty() {
        item.blank();
        item.push("Flags", Role::Warn, 0);
        for flag in &plan.flags {
            item.push(flag, Role::Warn, 1);
        }
    }
    item
}

/// Files of yours the conversation names before the rest are left to Show the details.
const IN_THE_WAY_SHOWN: usize = 8;

/// The merge waiting on your files, which ones and why git stops there, or on git's own reason.
fn in_the_way_item(merge: &MergePacket, shown: Option<usize>) -> AboutCell {
    let into = merge.target.as_deref().map(MergedInto::of);
    let mut item =
        AboutCell::new(into.map_or_else(|| "Waiting to merge".to_owned(), MergedInto::waiting));
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
        let wouldnt = match into {
            Some(MergedInto::Branch(branch)) => {
                format!("Git wouldn't move your checkout of {branch} forward:")
            }
            Some(MergedInto::YourFolder) => "The run couldn't be written into your folder:".into(),
            None => "Git wouldn't move your checkout forward:".to_owned(),
        };
        item.push(wouldnt, Role::Text, 0);
        item.push(refused.as_str(), Role::Text, 1);
    }
    item
}

fn merge_item(merge: &MergePacket, run: Option<&RunState>, names: &DisplayNames) -> AboutCell {
    // The branch's name is plumbing, so the heading names the run. A run that started on no
    // branch has none to name until you pick one.
    let into = merge.target.as_deref().map(MergedInto::of);
    let mut item =
        AboutCell::new(into.map_or_else(|| "Merge the run".to_owned(), MergedInto::asked));
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
        let checks = if row.checks_passed {
            TableCell::new("passed", Role::Success)
        } else {
            TableCell::new("failed", Role::Danger)
        };
        // Work a weaker fallback did waits for a strong enough review.
        let review = match row.debt.first() {
            Some(debt) => {
                TableCell::new(format!("needs {}", names.worker(&debt.needs)), Role::Warn)
            }
            None => TableCell::new(
                words::review(row.review.as_ref()),
                match row.review {
                    Some(ReviewVerdict::Approve) => Role::Text,
                    None => Role::Dim,
                    Some(_) => Role::Warn,
                },
            ),
        };
        let task = run.map_or_else(|| row.task.to_string(), |run| run.task_label(&row.task));
        // A task a script's call made is titled with the label its worker goes by, so the
        // title would only say the task's name again.
        let title = if row.title == task {
            ""
        } else {
            row.title.as_str()
        };
        table.row(vec![
            TableCell::text(task),
            TableCell::text(title),
            TableCell::dim(row.stat.files_changed.to_string()),
            TableCell::text(words::lines(&row.stat)),
            TableCell::dim(
                row.worker
                    .as_ref()
                    .map(|worker| names.worker(worker))
                    .unwrap_or_default(),
            ),
            checks,
            review,
        ]);
    }
    // What Argonauts Code committed itself, such as the goal record, so the rows add up to the
    // totals.
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
    for row in merge.rows.iter().filter(|row| !row.questions.is_empty()) {
        let task = run.map_or_else(|| row.task.to_string(), |run| run.task_label(&row.task));
        for question in &row.questions {
            asked_you(&mut item, &task, question, names);
        }
    }
    // Only a branch of yours has a latest main for the suite to have run on.
    let on_main = matches!(into, Some(MergedInto::Branch(_)));
    let (suite, role) = match (words::Suite::of(merge), on_main) {
        (words::Suite::Passed, true) => ("Suite passed on the latest main", Role::Success),
        (words::Suite::Passed, false) => ("Suite passed", Role::Success),
        (words::Suite::Failed, true) => ("Suite didn't pass on the latest main", Role::Danger),
        (words::Suite::Failed, false) => ("Suite didn't pass", Role::Danger),
        (words::Suite::NotRun, true) => ("No test suite ran on the latest main", Role::Dim),
        (words::Suite::NotRun, false) => ("No test suite ran", Role::Dim),
    };
    item.push(suite, role, 0);
    for check in merge.checks.iter().filter(|check| !check.passed) {
        item.push(
            format!("{} failed: {}", check.name, check.summary),
            Role::Danger,
            1,
        );
    }
    if merge.partial {
        item.push(
            "An agent was stopped early, so the work may be partial",
            Role::Warn,
            0,
        );
    }
    item
}

/// A question a merged task's agent asked you that nothing waited on, or that was answered in
/// your place: merging takes the work as that answer left it.
fn asked_you(item: &mut AboutCell, task: &str, question: &MergeQuestion, names: &DisplayNames) {
    let text = one_line(&question.text);
    match (&question.answer, &question.answered_by) {
        (None, _) => {
            item.push(format!("{task} asked you: {text}"), Role::Warn, 0);
            item.push(
                "Nothing waited for your answer, so the task went on to its reviews",
                Role::Dim,
                1,
            );
        }
        (Some(answer), by) => {
            item.push(format!("{task} asked you: {text}"), Role::Text, 0);
            let by = by
                .as_ref()
                .map(|by| format!("{} answered in your place", names.short(by)))
                .unwrap_or_else(|| "Answered in your place".to_owned());
            item.push(format!("{by}: {}", one_line(answer)), Role::Dim, 1);
        }
    }
}

fn fallback_item(plan: &FallbackPlan, names: &DisplayNames) -> AboutCell {
    let mut item = AboutCell::new(format!("Who can continue {} {}", plan.task, plan.title));
    let mut table = Table::new(vec![
        Column::new("candidate", 0).flex(),
        Column::new("plan usage left", 0),
        Column::new("tasks like this", 1),
        Column::new("est. cost", 2),
    ]);
    for candidate in &plan.candidates {
        table.row(vec![
            TableCell::text(names.worker(&candidate.worker)),
            TableCell::text(&candidate.plan_usage_text),
            TableCell::dim(words::track_record(candidate.track_record.as_ref())),
            TableCell::new(
                words::cost(candidate),
                if candidate.may_run_out || candidate.needs_confirm {
                    Role::Warn
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
            plan.task
        ));
    }
    item.table(table);
    item
}

fn agent_down_item(down: &AgentDownPacket, offset: UtcOffset, names: &DisplayNames) -> AboutCell {
    // The CLI by the name people read, as the question under it says it.
    let mut item = AboutCell::new(super::cli_down::title(down, names));
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
    for paused in &down.paused {
        table.row(vec![
            TableCell::text(paused.task.as_str()),
            TableCell::text(&paused.title),
            TableCell::dim(paused.role.as_str()),
        ]);
    }
    item.table(table);
    item
}

/// The question names the last failure itself, so the conversation gets it only in `full`.
fn stuck_item(stuck: &StuckPacket, full: bool) -> AboutCell {
    let mut item = AboutCell::new(format!("{} · tries", stuck.title));
    for (n, a_try) in stuck.tries.iter().enumerate() {
        item.push(format!("{}. {a_try}", n + 1), Role::Text, 0);
    }
    if full {
        item.blank();
        item.labeled("Last failure", &stuck.last_failure);
    }
    item
}

fn limit_item(limit: &LimitPacket) -> AboutCell {
    let whose = limit
        .task
        .as_ref()
        .map_or_else(|| "The run".to_owned(), ToString::to_string);
    let mut item = AboutCell::new(format!(
        "{whose} hit its {} limit",
        words::dimension(limit.dimension)
    ));
    item.line(words::limit_reading(&limit.status, limit.dimension));
    if let Some(reserve) = &limit.reserve_left {
        item.dim(format!("Reserve left: {}", words::amount(reserve)));
    }
    item
}

fn change_request_item(request: &ChangeRequestPacket, names: &DisplayNames) -> AboutCell {
    let mut item = AboutCell::new(format!(
        "{} asks to change {}",
        words::party(&request.from, names),
        request.task
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

fn limit_line(line: LimitLineKind) -> &'static str {
    match line {
        LimitLineKind::GoalAndPlan => "goal and plan",
        LimitLineKind::Experiments => "experiments",
        LimitLineKind::Contracts => "contracts",
        LimitLineKind::Tasks => "tasks",
        LimitLineKind::Review => "review",
        LimitLineKind::Merge => "merge",
        LimitLineKind::Reserve => "reserve",
    }
}

/// `task-002 Redis store · GPT-6.1 Sol on Codex · reviewed by Opus 5.5 on Claude Code`.
fn task_line(task: &TaskSpec, names: &DisplayNames) -> String {
    let mut line = format!("{} {}", task.id, task.title);
    // Named as their sessions will be, by the CLI's default where the plan names no model.
    if let Some(worker) = &task.worker {
        line.push_str(&format!(" · {}", names.worker(&names.running(worker))));
    }
    if !task.reviewers.is_empty() {
        let reviewers: Vec<String> = task
            .reviewers
            .iter()
            .map(|reviewer| names.worker(&names.running(reviewer)))
            .collect();
        line.push_str(&format!(" · reviewed by {}", words::joined(&reviewers)));
    }
    line
}

/// Tasks by how long a chain of dependencies each waits on, in graph order within a level.
fn by_depth(tasks: &[TaskSpec]) -> Vec<(&TaskSpec, usize)> {
    let mut depth: Vec<Option<usize>> = vec![None; tasks.len()];
    // Each pass settles the tasks whose dependencies are settled; a cycle or a missing task
    // leaves the rest at depth 0.
    for _ in 0..tasks.len() {
        let mut changed = false;
        for (index, task) in tasks.iter().enumerate() {
            if depth[index].is_some() {
                continue;
            }
            let parents: Option<Vec<usize>> = task
                .depends_on
                .iter()
                .filter_map(|id| tasks.iter().position(|other| &other.id == id))
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
    let mut order: Vec<(&TaskSpec, usize)> = tasks
        .iter()
        .zip(depth)
        .map(|(task, depth)| (task, depth.unwrap_or(0)))
        .collect();
    order.sort_by_key(|(_, depth)| *depth);
    order
}

fn totals(tasks: usize, combined: &DiffStat) -> String {
    format!(
        "{} · {}",
        words::plural(u32::try_from(tasks).unwrap_or(u32::MAX), "task", "tasks"),
        words::diffstat(combined)
    )
}

fn ids(tasks: &[TaskId]) -> String {
    tasks
        .iter()
        .map(TaskId::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
