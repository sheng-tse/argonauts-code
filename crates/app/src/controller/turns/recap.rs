//! Recaps and titles, built by code.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::TaskId;
use argonauts_core::app::TurnReply;
use argonauts_core::runs::api::StatusSummary;
use argonauts_core::runs::events::run::{
    ClosingAnswer, KEPT_ON_ITS_BRANCH, RunLifecycle, RunMode, RunOutcome, says_merged,
};
use argonauts_core::runs::events::task::{CheckKind, TaskEvent, TaskKind, TaskState};
use argonauts_core::runs::events::{RunEvent, RunEventKind};
use argonauts_crew::merge::final_check::CHECK_SCOPE;
use argonauts_crew::task::checks::{held_back_file, is_no_command_checks};

/// Longest title, in characters. The terminal cuts the rest anyway.
const TITLE_MAX: usize = 60;

/// Such as "Export now takes --json. All 4 tasks merged, and the checks pass: tests ran on each
/// task and on the merge. Ask for another change, or /resume an older run." A run that ended
/// starts with what changed, from the closing answer in its `events`, rather than its goal,
/// which only repeats what you asked. A run that didn't merge never says it did, and one kept
/// on its branch says so and why, as its answer does. A merged run says the checks pass only
/// when its events show they all ran and passed, and otherwise what didn't run.
pub(crate) fn recap_from_summary(
    summary: &StatusSummary,
    outcome: Option<RunOutcome>,
    events: &[RunEvent],
) -> String {
    let closing = closing_in(events);
    let main_agent = match outcome {
        Some(outcome) => closing.and_then(|closing| what_changed(closing, outcome)),
        None => Some(goal_line(summary)),
    };
    let tasks = Tasks::count(summary);
    let rest = match outcome {
        Some(RunOutcome::Merged) => {
            let merged = capitalized(&tasks.merged_phrase().unwrap_or("it merged".to_owned()));
            let checked = RanChecks::read(events).said(&merged, tasks.merged);
            format!("{checked} Ask for another change, or /resume an older run.")
        }
        // A run that didn't merge only merged its tasks into its own branch, so they're done,
        // never "merged" beside an ending that says nothing merged.
        Some(RunOutcome::PullRequest) => {
            with_done(&tasks, "The work waits in a pull request for review")
        }
        // The recap is of the run on screen, which `/resume` alone carries on.
        Some(RunOutcome::BranchLeft) => {
            let kept = closing
                .and_then(ClosingAnswer::kept)
                .unwrap_or(KEPT_ON_ITS_BRANCH);
            format!("{} /resume picks it up.", with_done(&tasks, kept))
        }
        Some(RunOutcome::Abandoned) => "You abandoned the run, so nothing merged.".to_owned(),
        Some(RunOutcome::Stopped) => format!(
            "{} /resume goes on from there.",
            with_done(&tasks, "The run stopped")
        ),
        Some(RunOutcome::Failed) => {
            format!(
                "{} /status shows where.",
                with_done(&tasks, "The run failed")
            )
        }
        None => in_progress(summary, &tasks),
    };
    match main_agent {
        Some(main_agent) => format!("{}. {rest}", main_agent.trim_end_matches('.')),
        None => rest,
    }
}

/// What a run's own events say of its command checks: which ran, each on how many of its tasks
/// and whether on the merge, which failed at their newest run, and, when none ran, the file
/// that holds the checks it left out.
#[derive(Debug, Default)]
struct RanChecks {
    /// In the order they first ran.
    names: Vec<String>,
    /// Where each check ran, by its name.
    ran_on: BTreeMap<String, RanOn>,
    /// Each check's newest outcome, by where it ran and its name.
    newest: BTreeMap<(TaskId, String), bool>,
    held_back_in: Option<String>,
}

/// Where one check ran: the run's tasks it ran on, and whether it ran on the merge.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RanOn {
    tasks: BTreeSet<TaskId>,
    merge: bool,
}

impl RanOn {
    /// Whether it ran over all the work of a run with `merged` tasks merged: on every one, or
    /// on the merge alone, which holds them all.
    fn covers(&self, merged: usize) -> bool {
        self.tasks.len() >= merged || self.tasks.is_empty()
    }

    /// `each task and on the merge`, `2 of 3 tasks`, `the merge`.
    fn said(&self, merged: usize) -> String {
        let and_merge = if self.merge { " and on the merge" } else { "" };
        match self.tasks.len() {
            0 => "the merge".to_owned(),
            checked if checked >= merged => {
                let tasks = if merged == 1 { "the task" } else { "each task" };
                format!("{tasks}{and_merge}")
            }
            checked => format!("{checked} of {merged} tasks{and_merge}"),
        }
    }
}

impl RanChecks {
    fn read(events: &[RunEvent]) -> Self {
        // A test task's commands fail against the stubs by design, and its stub check stands
        // for them, so only the other tasks' commands count.
        let tasks: BTreeSet<&TaskId> = events
            .iter()
            .filter_map(|event| match &event.kind {
                RunEventKind::Task(TaskEvent::Created(spec))
                    if !matches!(spec.kind, TaskKind::AcceptanceTests { .. }) =>
                {
                    event.task_id.as_ref()
                }
                _ => None,
            })
            .collect();
        let mut ran = Self::default();
        for event in events {
            let (RunEventKind::Check(outcome), Some(task)) = (&event.kind, &event.task_id) else {
                continue;
            };
            if is_no_command_checks(outcome) {
                if let Some(file) = held_back_file(outcome) {
                    ran.held_back_in = Some(file.to_owned());
                }
                continue;
            }
            let command = matches!(
                outcome.kind,
                CheckKind::Command | CheckKind::AffectedTests | CheckKind::FullSuite
            ) && outcome.command.is_some();
            let on_merge = task.as_str() == CHECK_SCOPE;
            if !command || !(on_merge || tasks.contains(task)) {
                continue;
            }
            let name = outcome.name.trim().to_owned();
            if !ran.names.contains(&name) {
                ran.names.push(name.clone());
            }
            let ran_on = ran.ran_on.entry(name.clone()).or_default();
            if on_merge {
                ran_on.merge = true;
            } else {
                ran_on.tasks.insert(task.clone());
            }
            ran.newest.insert((task.clone(), name), outcome.passed);
        }
        ran
    }

    /// The recap's sentence about the checks after `merged`, "All 3 tasks merged", with
    /// `merged_tasks` of them merged.
    fn said(&self, merged: &str, merged_tasks: usize) -> String {
        let failed = |on_merge: bool| -> Vec<&str> {
            let mut names: Vec<&str> = Vec::new();
            for ((task, name), passed) in &self.newest {
                if !passed
                    && (task.as_str() == CHECK_SCOPE) == on_merge
                    && !names.contains(&name.as_str())
                {
                    names.push(name);
                }
            }
            names
        };
        let on_merge = failed(true);
        if !on_merge.is_empty() {
            return format!(
                "{merged}. The final check failed ({}) and you merged anyway.",
                listed(&on_merge)
            );
        }
        let on_tasks = failed(false);
        if !on_tasks.is_empty() {
            return format!(
                "{merged}. A task's checks failed ({}) and you merged anyway.",
                listed(&on_tasks)
            );
        }
        if self.names.is_empty() {
            return match &self.held_back_in {
                Some(file) => format!(
                    "{merged}. No checks ran, since checks.run in {file} waits until you trust \
                     that file: /setup."
                ),
                None => {
                    format!(
                        "{merged}. No checks ran, since none are set: /config checks.run sets them."
                    )
                }
            };
        }
        // The checks that ran in the same places go together, in the order they first ran.
        let mut places: Vec<(&RanOn, Vec<&str>)> = Vec::new();
        for name in &self.names {
            let ran_on = &self.ran_on[name];
            match places.iter_mut().find(|(place, _)| *place == ran_on) {
                Some((_, names)) => names.push(name),
                None => places.push((ran_on, vec![name])),
            }
        }
        let said: Vec<String> = places
            .iter()
            .enumerate()
            .map(|(at, (place, names))| {
                let ran = if at == 0 { " ran" } else { "" };
                format!("{}{ran} on {}", listed(names), place.said(merged_tasks))
            })
            .collect();
        let said = match said.as_slice() {
            [one] => one.clone(),
            [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
            [] => String::new(),
        };
        if places.iter().all(|(place, _)| place.covers(merged_tasks)) {
            format!("{merged}, and the checks pass: {said}.")
        } else {
            format!("{merged}. The checks pass where they ran: {said}.")
        }
    }
}

/// "tests", "tests and lint", "tests, lint and types".
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The newest closing answer in a run's events.
fn closing_in(events: &[RunEvent]) -> Option<&ClosingAnswer> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) => Some(answer.as_ref()),
        _ => None,
    })
}

/// The agent's first sentence, or without an agent's words, the files that changed. Of a run
/// that didn't merge, words that say it did give way to the files.
fn what_changed(closing: &ClosingAnswer, outcome: RunOutcome) -> Option<String> {
    if closing.by.is_some() {
        let text = closing.text.trim();
        let first = match text.find(". ") {
            Some(end) => &text[..end],
            None => text,
        };
        let contradicts = outcome != RunOutcome::Merged && says_merged(first);
        if !first.is_empty() && !contradicts {
            return Some(first.to_owned());
        }
    }
    let paths: Vec<&str> = closing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    match paths.as_slice() {
        [] => None,
        [one] => Some(format!("Changed {one}")),
        [first, second] => Some(format!("Changed {first} and {second}")),
        [first, second, third] => Some(format!("Changed {first}, {second}, and {third}")),
        more => Some(format!("Changed {} files", more.len())),
    }
}

/// After a long conversation turn.
pub(crate) fn recap_from_turn(request: &str, reply: &TurnReply) -> String {
    let title = title_from_request(request);
    match reply {
        TurnReply::Answer { .. } => format!("Answered \"{title}\"."),
        TurnReply::Change {
            captain_or_crew, ..
        } => {
            format!(
                "Started a {} run for \"{title}\": {}.",
                mode_name(captain_or_crew.mode),
                captain_or_crew.reason
            )
        }
        TurnReply::Unclear { .. } => {
            format!("Answered \"{title}\", and a question asks whether to start work on it.")
        }
    }
}

/// The terminal title for a conversation: its first request, shortened.
pub(crate) fn title_from_request(request: &str) -> String {
    let line = request
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let words: Vec<&str> = line.split_whitespace().collect();
    let joined = words.join(" ");
    if joined.chars().count() <= TITLE_MAX {
        return joined;
    }
    // Cut at a word when one ends close enough, so the title doesn't end mid-word.
    let mut title = String::new();
    for word in &words {
        let next = title.chars().count() + usize::from(!title.is_empty()) + word.chars().count();
        if next > TITLE_MAX - 1 {
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
    }
    if title.chars().count() < TITLE_MAX / 2 {
        title = joined.chars().take(TITLE_MAX - 1).collect();
    }
    title.push('…');
    title
}

pub(crate) fn mode_name(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Captain => "captain",
        RunMode::Crew => "crew",
    }
}

fn goal_line(summary: &StatusSummary) -> String {
    let goal = summary
        .goal
        .iter()
        .chain(summary.plan.as_ref())
        .map(|line| line.trim().trim_end_matches('.'))
        .find(|line| !line.is_empty());
    match goal {
        Some(goal) => capitalized(goal),
        None => match &summary.run {
            Some(run) => format!("Run {run}"),
            None => "The run".to_owned(),
        },
    }
}

struct Tasks {
    merged: usize,
    working: usize,
    /// Cancelled tasks don't count.
    total: usize,
}

impl Tasks {
    fn count(summary: &StatusSummary) -> Self {
        let counted = || {
            summary
                .tasks
                .iter()
                .filter(|task| task.state != TaskState::Cancelled)
        };
        Self {
            merged: counted()
                .filter(|task| task.state == TaskState::Merged)
                .count(),
            working: counted()
                .filter(|task| {
                    matches!(
                        task.state,
                        TaskState::Running
                            | TaskState::Checking
                            | TaskState::InReview
                            | TaskState::Merging
                    )
                })
                .count(),
            total: counted().count(),
        }
    }

    /// "all 4 tasks merged", "the task merged", "2 of 5 tasks merged", or `None` with no tasks.
    /// "all 4 tasks done", "the task done", "1 of 2 tasks done", or `None` with no tasks.
    fn done_phrase(&self) -> Option<String> {
        match (self.merged, self.total) {
            (_, 0) => None,
            (1, 1) => Some("the task done".to_owned()),
            (0, 1) => Some("the task not done".to_owned()),
            (done, total) if done == total => Some(format!("all {total} tasks done")),
            (done, total) => Some(format!("{done} of {total} tasks done")),
        }
    }

    fn merged_phrase(&self) -> Option<String> {
        match (self.merged, self.total) {
            (_, 0) => None,
            (1, 1) => Some("the task merged".to_owned()),
            (0, 1) => Some("the task isn't merged".to_owned()),
            (merged, total) if merged == total => Some(format!("all {total} tasks merged")),
            (merged, total) => Some(format!("{merged} of {total} tasks merged")),
        }
    }
}

/// `sentence`, then how many tasks finished, as "The run failed, with 1 of 2 tasks done."
fn with_done(tasks: &Tasks, sentence: &str) -> String {
    match tasks.done_phrase() {
        Some(done) => format!("{sentence}, with {done}."),
        None => format!("{sentence}."),
    }
}

fn in_progress(summary: &StatusSummary, tasks: &Tasks) -> String {
    let mut parts = Vec::new();
    if let Some(merged) = tasks.merged_phrase() {
        parts.push(capitalized(&merged));
    }
    if tasks.working > 0 {
        parts.push(format!("{} working", tasks.working));
    }
    let mut text = if !parts.is_empty() {
        format!("{}. ", parts.join(", "))
    } else if summary.waiting.is_empty() {
        "It's under way. ".to_owned()
    } else {
        // A run that waits on you isn't under way, so the wait says it alone.
        String::new()
    };
    if !summary.waiting.is_empty() {
        // A task's title can end a sentence of its own, and one stop is enough.
        let waiting = summary.waiting.join(", ");
        text.push_str(&format!(
            "Waiting on {}.",
            waiting.trim().trim_end_matches('.')
        ));
    } else if let Some(last) = summary.recent.last() {
        text.push_str(&format!("Last: {}.", last.trim().trim_end_matches('.')));
    }
    text.trim_end().to_owned()
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunId;
    use argonauts_core::runs::api::TaskBrief;
    use argonauts_core::runs::events::run::{CaptainOrCrewDecision, ChangedFile};
    use argonauts_core::runs::events::task::{CheckOutcome, MergeEvent};

    fn task(n: u32, state: TaskState) -> TaskBrief {
        TaskBrief {
            task: TaskId::numbered(n),
            title: format!("task {n}"),
            state,
            worker: None,
            note: String::new(),
        }
    }

    fn summary(tasks: Vec<TaskBrief>) -> StatusSummary {
        StatusSummary {
            run: Some(RunId::new("run-007").unwrap()),
            goal: vec!["export now takes --json.".to_owned(), "criteria".to_owned()],
            tasks,
            ..StatusSummary::default()
        }
    }

    #[test]
    fn recap_from_status_summary() {
        let merged = summary((1..=4).map(|n| task(n, TaskState::Merged)).collect());
        assert_eq!(
            recap_from_summary(&merged, Some(RunOutcome::Merged), &[]),
            "All 4 tasks merged. No checks ran, since none are set: /config checks.run sets \
             them. Ask for another change, or /resume an older run."
        );
        let stopped = summary(vec![
            task(1, TaskState::Merged),
            task(2, TaskState::Running),
            task(3, TaskState::Cancelled),
        ]);
        assert_eq!(
            recap_from_summary(&stopped, Some(RunOutcome::Stopped), &[]),
            "The run stopped, with 1 of 2 tasks done. /resume goes on from there."
        );
        let mut going = stopped.clone();
        going.waiting = vec!["the merge approval".to_owned()];
        assert_eq!(
            recap_from_summary(&going, None, &[]),
            "Export now takes --json. 1 of 2 tasks merged, 1 working. Waiting on the merge \
             approval."
        );
        let mut needs_you = summary(vec![task(1, TaskState::NeedsYou)]);
        needs_you.waiting = vec!["Fix add so it returns a + b needs you".to_owned()];
        assert_eq!(
            recap_from_summary(&needs_you, None, &[]),
            "Export now takes --json. The task isn't merged. Waiting on Fix add so it returns a + \
             b needs you."
        );
        // Before any question, a run that waits on you says only that, never that it's under way.
        let mut asking = summary(Vec::new());
        asking.waiting = vec!["Question from build:greet".to_owned()];
        assert_eq!(
            recap_from_summary(&asking, None, &[]),
            "Export now takes --json. Waiting on Question from build:greet."
        );
        let mut working = summary(Vec::new());
        working.recent = vec!["Goal approved".to_owned()];
        assert_eq!(
            recap_from_summary(&working, None, &[]),
            "Export now takes --json. It's under way. Last: Goal approved."
        );
        let empty = StatusSummary::default();
        assert_eq!(
            recap_from_summary(&empty, Some(RunOutcome::Abandoned), &[]),
            "You abandoned the run, so nothing merged."
        );
        let one = summary(vec![task(1, TaskState::Merged)]);
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::PullRequest), &[]),
            "The work waits in a pull request for review, with the task done."
        );
        // A run kept on its branch never says its task merged.
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::BranchLeft), &[]),
            "Kept on its branch, not merged, with the task done. /resume picks it up."
        );
        assert_eq!(
            recap_from_summary(&stopped, Some(RunOutcome::Failed), &[]),
            "The run failed, with 1 of 2 tasks done. /status shows where."
        );
    }

    #[test]
    fn a_finished_runs_recap_says_what_changed_not_what_you_asked() {
        let one = summary(vec![task(1, TaskState::Merged)]);
        let mut closing = ClosingAnswer {
            text: "Fixed parse() and format_cents() in invoice/money.py. All six tests pass."
                .to_owned(),
            by: Some("claude-code/haiku".parse().unwrap()),
            files: vec![ChangedFile {
                path: "invoice/money.py".to_owned(),
                insertions: 19,
                deletions: 3,
            }],
            checks: Vec::new(),
            tests: Vec::new(),
            left: Vec::new(),
            decided: Vec::new(),
        };
        let ran = [checked(1, "tests", true)];
        let events = with_closing(&ran, &closing);
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::Merged), &events),
            "Fixed parse() and format_cents() in invoice/money.py. The task merged, and the \
             checks pass: tests ran on the task. Ask for another change, or /resume an older run."
        );
        closing.by = None;
        let events = with_closing(&ran, &closing);
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::Merged), &events),
            "Changed invoice/money.py. The task merged, and the checks pass: tests ran on the \
             task. Ask for another change, or /resume an older run."
        );
    }

    /// The agent called the work merged, but the run kept it on its branch: the recap says what
    /// changed by its files instead, then that it was kept and why, as the answer's line does.
    #[test]
    fn a_run_kept_on_its_branch_never_reads_as_merged() {
        let one = summary(vec![task(1, TaskState::Merged)]);
        let kept = "Kept on its branch, not merged, as autopilot chose, since the review stayed \
                    within Claude";
        let closing = ClosingAnswer {
            text: "The money rounding fix is merged: invoice/money.py now passes the three \
                   tests. The reviewer accepted it."
                .to_owned(),
            by: Some("claude-code/haiku".parse().unwrap()),
            files: vec![ChangedFile {
                path: "invoice/money.py".to_owned(),
                insertions: 12,
                deletions: 4,
            }],
            checks: Vec::new(),
            tests: Vec::new(),
            left: vec![kept.to_owned()],
            decided: Vec::new(),
        };
        let recap = recap_from_summary(
            &one,
            Some(RunOutcome::BranchLeft),
            &with_closing(&[], &closing),
        );
        assert_eq!(
            recap,
            format!("Changed invoice/money.py. {kept}, with the task done. /resume picks it up.")
        );
        assert!(!says_merged(&recap), "{recap}");
    }

    fn event(task: Option<&str>, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq: 0,
            ts: time::macros::datetime!(2026-10-09 10:42 UTC),
            run_id: RunId::new("run-007").unwrap(),
            task_id: task.map(|task| TaskId::new(task).unwrap()),
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    fn outcome(name: &str, passed: bool) -> CheckOutcome {
        CheckOutcome {
            name: name.to_owned(),
            kind: CheckKind::Command,
            passed,
            command: Some(vec![
                "python3".to_owned(),
                "-m".to_owned(),
                "unittest".to_owned(),
            ]),
            exit_code: Some(if passed { 0 } else { 1 }),
            duration_ms: 30,
            summary: String::new(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }
    }

    /// Task `n` made, then its check `name` run.
    fn checked(n: u32, name: &str, passed: bool) -> Vec<RunEvent> {
        let task = TaskId::numbered(n);
        let spec = crate::client::test_support::task_spec(n as usize);
        vec![
            event(
                Some(task.as_str()),
                RunEventKind::Task(TaskEvent::Created(Box::new(spec))),
            ),
            event(
                Some(task.as_str()),
                RunEventKind::Check(outcome(name, passed)),
            ),
        ]
    }

    fn on_the_merge(name: &str, passed: bool) -> Vec<RunEvent> {
        vec![
            event(
                Some(CHECK_SCOPE),
                RunEventKind::Check(outcome(name, passed)),
            ),
            event(None, RunEventKind::Merge(MergeEvent::FinalCheck { passed })),
        ]
    }

    fn with_closing(ran: &[Vec<RunEvent>], closing: &ClosingAnswer) -> Vec<RunEvent> {
        let mut events: Vec<RunEvent> = ran.iter().flatten().cloned().collect();
        events.push(event(
            None,
            RunEventKind::Run(RunLifecycle::ClosingAnswer(Box::new(closing.clone()))),
        ));
        events
    }

    /// The recap says the checks pass only when the run's events show they ran and passed, on
    /// each task and on the merge; otherwise it says what didn't run, or what failed.
    #[test]
    fn a_merged_runs_recap_says_what_its_checks_did() {
        let three = summary((1..=3).map(|n| task(n, TaskState::Merged)).collect());
        let closing = ClosingAnswer {
            text: "Money rounding now uses Decimal. Three tasks changed invoice/.".to_owned(),
            by: Some("claude-code/haiku".parse().unwrap()),
            files: Vec::new(),
            checks: Vec::new(),
            tests: Vec::new(),
            left: Vec::new(),
            decided: Vec::new(),
        };
        let recap = |ran: &[Vec<RunEvent>]| {
            recap_from_summary(
                &three,
                Some(RunOutcome::Merged),
                &with_closing(ran, &closing),
            )
        };
        let tasks = |passed: bool| {
            (1..=3)
                .map(|n| checked(n, "tests", passed))
                .collect::<Vec<_>>()
        };
        let mut all = tasks(true);
        all.push(on_the_merge("tests", true));
        assert_eq!(
            recap(&all),
            "Money rounding now uses Decimal. All 3 tasks merged, and the checks pass: tests ran \
             on each task and on the merge. Ask for another change, or /resume an older run."
        );
        assert_eq!(
            recap(&[]),
            "Money rounding now uses Decimal. All 3 tasks merged. No checks ran, since none are \
             set: /config checks.run sets them. Ask for another change, or /resume an older run."
        );
        let mut failed = tasks(true);
        failed.push(on_the_merge("tests", false));
        assert_eq!(
            recap(&failed),
            "Money rounding now uses Decimal. All 3 tasks merged. The final check failed (tests) \
             and you merged anyway. Ask for another change, or /resume an older run."
        );
        // A failure a task fixed later is no failure; its newest run counts.
        let mut fixed = tasks(false);
        fixed.extend(tasks(true));
        assert!(recap(&fixed).contains("and the checks pass: tests ran on each task."));
        // One task ran no checks.
        let some = vec![checked(1, "tests", true), checked(2, "tests", true)];
        assert!(
            recap(&some).contains("The checks pass where they ran: tests ran on 2 of 3 tasks."),
            "{}",
            recap(&some)
        );
        // Each check says where it ran: one that ran on a task alone isn't said to run on each.
        let mut apart = tasks(true);
        apart.push(checked(2, "lint", true));
        apart.push(on_the_merge("tests", true));
        assert!(
            recap(&apart).contains(
                "All 3 tasks merged. The checks pass where they ran: tests ran on each task and \
                 on the merge, and lint on 1 of 3 tasks."
            ),
            "{}",
            recap(&apart)
        );
        let mut together = tasks(true);
        together.extend((1..=3).map(|n| checked(n, "lint", true)));
        together.push(on_the_merge("types", true));
        assert!(
            recap(&together).contains(
                "and the checks pass: tests and lint ran on each task, and types on the merge."
            ),
            "{}",
            recap(&together)
        );
        // The checks wait in a file you haven't trusted.
        let mut marker = argonauts_crew::task::checks::no_command_checks(CheckKind::Command);
        marker.summary = argonauts_crew::task::checks::held_back_text(".argonauts/config.toml");
        let held = vec![vec![event(Some("task-001"), RunEventKind::Check(marker))]];
        assert!(
            recap(&held).contains(
                "No checks ran, since checks.run in .argonauts/config.toml waits until you trust \
                 that file: /setup."
            ),
            "{}",
            recap(&held)
        );
    }

    #[test]
    fn titles_are_the_first_line_shortened_at_a_word() {
        assert_eq!(title_from_request("  hi  "), "hi");
        assert_eq!(
            title_from_request("\n add   rate limiting\nto the API"),
            "add rate limiting"
        );
        let long = "how should we add rate limiting to the public API without breaking the \
                    clients that retry";
        let title = title_from_request(long);
        assert!(title.chars().count() <= TITLE_MAX, "{title}");
        assert!(title.ends_with("the…") || title.ends_with('…'), "{title}");
        assert!(long.starts_with(title.trim_end_matches('…')), "{title}");
        let one_word = "x".repeat(200);
        assert_eq!(title_from_request(&one_word).chars().count(), TITLE_MAX);
    }

    #[test]
    fn a_turn_recap_says_what_the_turn_became() {
        let captain_or_crew = CaptainOrCrewDecision {
            mode: RunMode::Crew,
            move_to_crew: false,
            rule: "learned:api-feature".to_owned(),
            reason: "the learned workflow for api-feature says crew".to_owned(),
            captain: None,
        };
        let reply = TurnReply::Change {
            markdown: String::new(),
            estimate: serde_json::from_value(serde_json::json!({
                "task_type": "api-feature", "summary": "s", "files_likely": [], "file_count": 9,
                "risk_labels": [], "tests_exist": true, "unknowns": []
            }))
            .unwrap(),
            captain_or_crew,
            instructions: Vec::new(),
        };
        assert_eq!(
            recap_from_turn("add rate limiting", &reply),
            "Started a crew run for \"add rate limiting\": the learned workflow for api-feature says crew."
        );
    }
}
