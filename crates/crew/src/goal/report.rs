//! What the screens are told about each goal in a run, built from the journal alone: the lines
//! with each one met or not and what checked it, who keeps it, every try with what it changed
//! and what still failed after it, the last check, and what the CLI itself says. One call,
//! [`reports`], gives one plain [`GoalReport`] for the run's own goal and one for each task
//! worked as a goal, and the views only draw them.

use time::OffsetDateTime;

use argonauts_core::events::card::CardEvent;
use argonauts_core::events::goal::{
    CheckRun, CriterionState, GoalChecked, GoalEvent, GoalRun, GoalStatus,
};
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::session::SessionEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::goal::report::{
    CheckReport, CliGoalReport, GoalReport, Keeper, LineReport, LineState, TryReport,
};
use argonauts_core::goal::{KeptBy, keep};
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::{CardId, DiffStat};

use crate::labels;
use crate::state::RunState;

use super::cli;

/// How many lines of a failed command's output a check's report keeps.
const OUTPUT_LINES: usize = 6;

/// Every goal in the run, the run's own first, then each task's in the order they were set.
pub fn reports(events: &[RunEvent]) -> Vec<GoalReport> {
    let state = RunState::fold(events);
    let mut goals: Vec<Folded> = Vec::new();
    for event in events {
        fold(&mut goals, event);
    }
    goals.into_iter().map(|goal| goal.report(&state)).collect()
}

/// One goal as its events told it so far.
struct Folded {
    task: Option<CardId>,
    goal: GoalRun,
    set_at: OffsetDateTime,
    ended_because: Option<String>,
    tries: Vec<Try>,
    /// The try a change of approach begins with.
    approach_changed_from: Option<u32>,
    cli_says: Option<CliGoalReport>,
    last: Option<GoalChecked>,
    /// A check you asked for is running.
    checking: bool,
    /// The try whose check first found each line met.
    met_on: Vec<(String, u32)>,
    /// Who works it, for the keeper's words.
    worker: Option<WorkerRef>,
    /// The newest session that works it, which keys its agent's row.
    session: Option<SessionKey>,
}

struct Try {
    n: u32,
    title: String,
    started: OffsetDateTime,
    /// When its check came back, which ends it.
    checked_at: Option<OffsetDateTime>,
    what: Option<String>,
    changed: DiffStat,
    committed: bool,
    checked: Option<GoalChecked>,
    /// The newest check you asked for while it was at work, which tells where it stands
    /// until its own check does.
    asked: Option<GoalChecked>,
    work_ms: u64,
    tokens: u64,
}

fn fold(goals: &mut Vec<Folded>, event: &RunEvent) {
    match &event.kind {
        RunEventKind::Goal(goal_event) => fold_goal(goals, goal_event, event),
        // Work between a try's start and its check is that try's: a task's own for its goal,
        // and every task's for the run's.
        RunEventKind::Card(CardEvent::Committed(commit)) if commit.sha.is_some() => {
            for a_try in open_tries(goals, event.card_id.as_ref()) {
                a_try.changed.files_changed += commit.stat.files_changed;
                a_try.changed.insertions += commit.stat.insertions;
                a_try.changed.deletions += commit.stat.deletions;
                a_try.committed = true;
            }
        }
        RunEventKind::Card(CardEvent::Reported(report)) => {
            let Some(task) = event.card_id.as_ref() else {
                return;
            };
            let Some(goal) = goals
                .iter_mut()
                .find(|goal| goal.task.as_ref() == Some(task))
            else {
                return;
            };
            if let Some(a_try) = goal
                .tries
                .last_mut()
                .filter(|a_try| a_try.checked.is_none())
            {
                a_try.what = Some(what_it_did(&report.summary));
            }
        }
        RunEventKind::Session(SessionEvent::Started(started)) => {
            if let Some(goal) = event.card_id.as_ref().and_then(|task| {
                goals
                    .iter_mut()
                    .find(|goal| goal.task.as_ref() == Some(task))
            }) && started.access == argonauts_core::worker::Access::Build
            {
                goal.worker = Some(started.worker.clone());
                goal.session.clone_from(&event.session);
            }
        }
        RunEventKind::Session(SessionEvent::Ended(end)) => {
            for a_try in open_tries(goals, event.card_id.as_ref()) {
                a_try.work_ms += end.duration_ms;
                a_try.tokens += end.spend.input_tokens + end.spend.output_tokens;
            }
        }
        // Nothing runs across a pause, a park, the run's end or a restart, so a check you
        // asked for that never said how it ended isn't running any more.
        RunEventKind::Run(
            RunLifecycle::Paused { .. }
            | RunLifecycle::Parked { .. }
            | RunLifecycle::Finished { .. }
            | RunLifecycle::Resumed(_),
        ) => {
            for goal in goals.iter_mut() {
                goal.checking = false;
            }
        }
        _ => {}
    }
}

/// The tries still at work that an event of `task` belongs to: its own goal's, and the run's.
fn open_tries<'a>(
    goals: &'a mut [Folded],
    task: Option<&'a CardId>,
) -> impl Iterator<Item = &'a mut Try> {
    goals
        .iter_mut()
        .filter(move |goal| goal.task.is_none() || goal.task.as_ref() == task)
        .filter_map(|goal| goal.tries.last_mut())
        .filter(|a_try| a_try.checked.is_none())
}

fn fold_goal(goals: &mut Vec<Folded>, goal_event: &GoalEvent, event: &RunEvent) {
    let of = |goals: &mut Vec<Folded>, task: &Option<CardId>| {
        goals.iter().position(|goal| &goal.task == task)
    };
    match goal_event {
        GoalEvent::Set(set) => match of(goals, &set.task) {
            // Set again, as when its worker changed: the tries so far stay.
            Some(at) => {
                let attempts = std::mem::take(&mut goals[at].goal.attempts);
                goals[at].goal = (**set).clone();
                if goals[at].goal.attempts.is_empty() {
                    goals[at].goal.attempts = attempts;
                }
            }
            None => {
                let folded = Folded {
                    task: set.task.clone(),
                    goal: (**set).clone(),
                    set_at: event.ts,
                    ended_because: None,
                    tries: Vec::new(),
                    approach_changed_from: None,
                    cli_says: None,
                    last: None,
                    checking: false,
                    met_on: Vec::new(),
                    worker: None,
                    session: None,
                };
                // The run's own goal reads first.
                if set.task.is_none() {
                    goals.insert(0, folded);
                } else {
                    goals.push(folded);
                }
            }
        },
        GoalEvent::AttemptStarted { n, title, task, .. } => {
            if let Some(at) = of(goals, task)
                && !goals[at].tries.iter().any(|a_try| a_try.n == *n)
            {
                goals[at].tries.push(Try {
                    n: *n,
                    title: title.clone(),
                    started: event.ts,
                    checked_at: None,
                    what: None,
                    changed: DiffStat::default(),
                    committed: false,
                    checked: None,
                    asked: None,
                    work_ms: 0,
                    tokens: 0,
                });
            }
        }
        GoalEvent::Checked(checked) => {
            let Some(at) = of(goals, &checked.task) else {
                return;
            };
            let goal = &mut goals[at];
            goal.found(checked);
            if let Some(a_try) = goal.try_mut(checked.attempt) {
                a_try.checked = Some((**checked).clone());
                a_try.checked_at = Some(event.ts);
            }
        }
        GoalEvent::CheckingNow { task, .. } => {
            if let Some(at) = of(goals, task) {
                goals[at].checking = true;
            }
        }
        // It says where the lines stand now and leaves its try open: the try's own check
        // ends it.
        GoalEvent::CheckedNow(checked) => {
            let Some(at) = of(goals, &checked.task) else {
                return;
            };
            let goal = &mut goals[at];
            goal.checking = false;
            goal.found(checked);
            if let Some(a_try) = goal.try_mut(checked.attempt) {
                a_try.asked = Some((**checked).clone());
            }
        }
        GoalEvent::CheckNowDropped { task, .. } => {
            if let Some(at) = of(goals, task) {
                goals[at].checking = false;
            }
        }
        GoalEvent::StatusChanged {
            status, task, why, ..
        } => {
            if let Some(at) = of(goals, task) {
                goals[at].goal.status = *status;
                goals[at].ended_because = match status {
                    GoalStatus::Pursuing | GoalStatus::Met => None,
                    _ => why.clone(),
                };
            }
        }
        GoalEvent::ApproachChanged { after, task, .. } => {
            if let Some(at) = of(goals, task) {
                goals[at].approach_changed_from = Some(after + 1);
            }
        }
        GoalEvent::CliSaid { task, said } => {
            if let Some(at) = of(goals, task) {
                let turns = goals[at].cli_says.as_ref().and_then(|says| says.turns);
                goals[at].cli_says = Some(CliGoalReport {
                    cli: argonauts_core::display_names::builtin_harness_name(&said.cli)
                        .map_or_else(|| said.cli.clone(), str::to_owned),
                    status: said.status.clone(),
                    note: said.note.clone(),
                    turns: Some(turns.unwrap_or(0) + 1),
                });
            }
        }
        _ => {}
    }
}

impl Folded {
    /// Takes a check's verdicts as where the lines stand now.
    fn found(&mut self, checked: &GoalChecked) {
        for verdict in &checked.verdicts {
            let met = verdict.verdict == CriterionState::Met;
            let known = self.met_on.iter().position(|(id, _)| id == &verdict.id);
            match (met, known) {
                (true, None) => self.met_on.push((verdict.id.clone(), checked.attempt)),
                // A line that stopped holding is met again on a later try, if at all.
                (false, Some(at)) => {
                    self.met_on.remove(at);
                }
                _ => {}
            }
        }
        self.last = Some(checked.clone());
    }

    fn try_mut(&mut self, n: u32) -> Option<&mut Try> {
        self.tries.iter_mut().find(|a_try| a_try.n == n)
    }

    fn report(self, state: &RunState) -> GoalReport {
        let spec = self.goal.spec();
        let last = self.last.as_ref();
        // Before its first session a task's agent is the one its plan gave it.
        let worker = self.worker.clone().or_else(|| {
            let record = state.cards.get(self.task.as_ref()?)?;
            record.owner.clone().or_else(|| record.spec.owner.clone())
        });
        let lines = self
            .goal
            .criteria
            .iter()
            .map(|line| {
                let verdict = last.and_then(|checked| {
                    checked
                        .verdicts
                        .iter()
                        .find(|verdict| verdict.id == line.id)
                });
                let by = last.and_then(|checked| decided_by(checked, &line.id));
                LineReport {
                    id: line.id.clone(),
                    text: line.text.clone(),
                    state: match verdict.map(|verdict| verdict.verdict) {
                        None => LineState::Unchecked,
                        Some(CriterionState::Met) => LineState::Met,
                        Some(_) => LineState::NotMet,
                    },
                    checked_by: by
                        .map_or_else(|| spec.line_checked_by(line), |run| run.name.clone()),
                    still_fails: match (verdict, by) {
                        (Some(verdict), _) if verdict.verdict == CriterionState::Met => {
                            String::new()
                        }
                        (Some(_), Some(run)) if run.failed.is_some_and(|failed| failed > 0) => {
                            let failed = run.failed.unwrap_or(0);
                            match run.total {
                                Some(total) => format!(
                                    "{failed} of {total} {}",
                                    if failed == 1 { "fails" } else { "fail" }
                                ),
                                None => keep::still_fail_words(failed),
                            }
                        }
                        (Some(verdict), _) => verdict.missing.clone(),
                        (None, _) => String::new(),
                    },
                    met_on_try: self
                        .met_on
                        .iter()
                        .find(|(id, _)| id == &line.id)
                        .map(|(_, n)| *n),
                }
            })
            .collect();
        let pursuing = self.goal.status == GoalStatus::Pursuing;
        let newest = self.tries.last().map(|a_try| a_try.n);
        let run_level = self.task.is_none();
        let tries: Vec<TryReport> = self
            .tries
            .iter()
            .map(|a_try| {
                let wall = a_try
                    .checked_at
                    .map(|end| (end - a_try.started).whole_milliseconds())
                    .and_then(|ms| u64::try_from(ms).ok());
                // Until a try's own check runs, a check you asked for says where it stands.
                let stands = a_try.checked.as_ref().or(a_try.asked.as_ref());
                TryReport {
                    n: a_try.n,
                    what: a_try.what.clone().unwrap_or_else(|| a_try.title.clone()),
                    changed: a_try.committed.then_some(a_try.changed),
                    still_failing: stands.map(keep::failing),
                    lines_met: stands.map_or(0, |checked| {
                        u32::try_from(
                            checked
                                .verdicts
                                .iter()
                                .filter(|verdict| verdict.verdict == CriterionState::Met)
                                .count(),
                        )
                        .unwrap_or(u32::MAX)
                    }),
                    // A round's agents work side by side, so its time is the clock's; one
                    // task's try is its sessions'.
                    ms: match wall {
                        Some(wall) if run_level => wall,
                        _ => a_try.work_ms,
                    },
                    tokens: a_try.tokens,
                    running: pursuing && a_try.checked.is_none() && Some(a_try.n) == newest,
                    changed_approach: self.approach_changed_from == Some(a_try.n),
                }
            })
            .collect();
        let of = match &self.task {
            Some(task) => labels::card_label(state, task),
            None => state
                .run
                .named
                .as_ref()
                .map_or_else(|| "the run".to_owned(), |named| named.name.clone()),
        };
        GoalReport {
            of,
            scope: self.goal.scope,
            task: self.task,
            session: self.session,
            worker: worker.clone(),
            condition: self.goal.objective.clone(),
            status: self.goal.status,
            ended_because: self.ended_because,
            kept: Keeper {
                by: self.goal.kept_by,
                words: match (self.goal.kept_by, &worker) {
                    (KeptBy::Cli, Some(worker)) => format!("{}'s own goal", cli::name(worker)),
                    (KeptBy::Cli, None) => "the CLI's own goal".to_owned(),
                    (KeptBy::Argonauts, _) => "Argonauts".to_owned(),
                },
                cli: match self.goal.kept_by {
                    KeptBy::Cli => worker.as_ref().map(cli::name),
                    KeptBy::Argonauts => None,
                },
                note: self.goal.kept_note.clone(),
            },
            lines,
            work_ms: tries.iter().map(|a_try| a_try.ms).sum(),
            tokens: tries.iter().map(|a_try| a_try.tokens).sum(),
            tries,
            max_rounds: self.goal.max_rounds,
            checks: last
                .map(|checked| checked.checks.iter().map(check_report).collect())
                .unwrap_or_default(),
            checking: self.checking,
            cli_says: self.cli_says,
            set_at: Some(self.set_at),
        }
    }
}

/// The run of the check that decided line `id`: a command that failed it, else the last thing
/// that looked at it.
fn decided_by<'a>(checked: &'a GoalChecked, id: &str) -> Option<&'a CheckRun> {
    let looked: Vec<&CheckRun> = checked
        .checks
        .iter()
        .filter(|run| run.lines.iter().any(|line| line == id))
        .collect();
    looked
        .iter()
        .find(|run| !run.passed)
        .or(looked.last())
        .copied()
}

fn check_report(run: &CheckRun) -> CheckReport {
    let summary = match (run.passed, run.failed, run.total) {
        (false, Some(failed), Some(total)) if failed > 0 => {
            format!("{failed} failed · {} passed", total.saturating_sub(failed))
        }
        (true, _, Some(total)) if total > 0 => format!(
            "passed · {total} {}",
            if total == 1 { "test" } else { "tests" }
        ),
        (true, ..) if run.summary.is_empty() => "passed".to_owned(),
        (false, ..) if run.summary.is_empty() => "failed".to_owned(),
        _ => run.summary.clone(),
    };
    CheckReport {
        name: run.name.clone(),
        passed: run.passed,
        summary,
        output: what_failed(&run.output),
        ms: run.ms,
        agent: run.agent.clone(),
    }
}

/// The lines of a failed command's output that name what failed, else its last few.
fn what_failed(output: &str) -> Vec<String> {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    const TELLING: &[&str] = &[
        "FAILED", "failed", "FAIL", "panicked", "Error", "error:", "expected", "assert",
    ];
    let telling: Vec<&str> = lines
        .iter()
        .filter(|line| TELLING.iter().any(|mark| line.contains(mark)))
        .copied()
        .collect();
    let kept = if telling.is_empty() { lines } else { telling };
    let from = kept.len().saturating_sub(OUTPUT_LINES);
    kept[from..]
        .iter()
        .map(|line| line.trim().to_owned())
        .collect()
}

/// A report's summary as a few words for the tries list: its first sentence, without what the
/// CLI said about its goal.
fn what_it_did(summary: &str) -> String {
    let first = summary
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.to_lowercase().starts_with("goal:"))
        .unwrap_or_default();
    let sentence = first.split(". ").next().unwrap_or(first);
    sentence.trim_end_matches('.').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::RunId;
    use argonauts_core::events::goal::{
        CheckedBy, CliGoalSaid, Criterion, CriterionCheck, CriterionVerdict, GoalVerdict,
    };
    use argonauts_core::events::run::Party;
    use argonauts_core::goal::report::{Failing, FailingWhat};
    use argonauts_core::goal::{CheckWith, GoalFrom, GoalScope, GoalSpec};
    use time::Duration;

    fn task() -> CardId {
        CardId::new("task-002").expect("an id")
    }

    fn at(seq: u64, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts: OffsetDateTime::UNIX_EPOCH + Duration::seconds(i64::try_from(seq).unwrap() * 60),
            run_id: RunId::new("run-01").expect("an id"),
            card_id: Some(task()),
            session: None,
            source: "conductor".to_owned(),
            kind,
        }
    }

    fn goal(seq: u64, event: GoalEvent) -> RunEvent {
        at(seq, RunEventKind::Goal(event))
    }

    fn lines() -> Vec<Criterion> {
        let mut parse = Criterion::new("G2", "tasks add --due rejects a date that isn't ISO");
        parse.check = CriterionCheck::Command("cargo test due::parse".to_owned());
        let mut overdue = Criterion::new("G3", "every test in tests/overdue.rs passes");
        overdue.check = CriterionCheck::Command("cargo test overdue".to_owned());
        vec![
            Criterion::new("G1", "tasks overdue lists tasks due before today"),
            parse,
            overdue,
        ]
    }

    fn set() -> GoalEvent {
        let spec = GoalSpec::from_lines(
            "add the overdue command",
            lines(),
            CheckWith::Auto,
            GoalFrom::Role,
        );
        let mut goal = GoalRun::new(&spec, GoalScope::Task, KeptBy::Cli);
        goal.task = Some(task());
        GoalEvent::Set(Box::new(goal))
    }

    fn started(n: u32) -> GoalEvent {
        GoalEvent::AttemptStarted {
            n,
            title: format!("try {n}"),
            phase: 0,
            task: Some(task()),
        }
    }

    fn verdict(id: &str, met: bool) -> CriterionVerdict {
        CriterionVerdict {
            id: id.to_owned(),
            verdict: if met {
                CriterionState::Met
            } else {
                CriterionState::NotYet
            },
            evidence: String::new(),
            missing: if met {
                String::new()
            } else {
                "it still fails".to_owned()
            },
        }
    }

    fn command(name: &str, line: &str, failed: u32, total: u32) -> CheckRun {
        CheckRun {
            name: name.to_owned(),
            by: CheckedBy::Command,
            lines: vec![line.to_owned()],
            passed: failed == 0,
            summary: format!("{} passed, {failed} failed", total - failed),
            failed: Some(failed),
            total: Some(total),
            ms: 4_100,
            output: if failed == 0 {
                String::new()
            } else {
                "running 12 tests\ntest overdue::skips_tasks_without_a_date ... FAILED\n\
                 expected 2 tasks, got 3\n"
                    .to_owned()
            },
            agent: None,
        }
    }

    fn judge(met: bool) -> CheckRun {
        CheckRun {
            name: "the judge".to_owned(),
            by: CheckedBy::Judge,
            lines: vec!["G1".to_owned()],
            passed: met,
            summary: "1 of 1 line met".to_owned(),
            failed: None,
            total: None,
            ms: 9_000,
            output: String::new(),
            agent: Some("claude-code/astra".parse().expect("a worker")),
        }
    }

    /// A check after try `n`: the judge's line met from try 2, the parse line always, and
    /// `failed` of the 12 overdue tests still failing.
    fn checked(n: u32, failed: u32) -> GoalEvent {
        let first = n >= 2;
        GoalEvent::Checked(Box::new(GoalChecked {
            attempt: n,
            verdicts: vec![
                verdict("G1", first),
                verdict("G2", true),
                verdict("G3", failed == 0),
            ],
            overall: if first && failed == 0 {
                GoalVerdict::Met
            } else {
                GoalVerdict::NotYet
            },
            reason: String::new(),
            task: Some(task()),
            checks: vec![
                command("cargo test overdue", "G3", failed, 12),
                command("cargo test due::parse", "G2", 0, 6),
                judge(first),
            ],
        }))
    }

    fn said(status: &str, note: &str) -> GoalEvent {
        GoalEvent::CliSaid {
            task: Some(task()),
            said: CliGoalSaid {
                cli: "codex".to_owned(),
                status: status.to_owned(),
                note: note.to_owned(),
                turns: None,
            },
        }
    }

    /// The goal panel: build:cli on its third try, two of three lines met.
    #[test]
    fn a_tasks_goal_reports_its_lines_tries_last_check_and_what_codex_says() {
        let events = vec![
            goal(1, set()),
            goal(2, started(1)),
            goal(3, said("active", "wrote the command and its tests")),
            goal(4, checked(1, 4)),
            goal(5, started(2)),
            goal(6, checked(2, 2)),
            goal(7, started(3)),
            goal(8, said("active", "one test left: tasks without a date")),
            goal(9, checked(3, 1)),
            goal(10, started(4)),
        ];
        let reports = reports(&events);
        let [report] = reports.as_slice() else {
            panic!("one goal, the task's: {reports:?}");
        };
        assert_eq!(report.task, Some(task()));
        assert_eq!(report.scope, GoalScope::Task);
        assert_eq!(report.status, GoalStatus::Pursuing);
        assert_eq!(report.kept.by, KeptBy::Cli);
        assert_eq!(report.lines_met(), 2);
        assert_eq!(report.row_words(), "try 4 · 2 of 3 lines met");
        assert_eq!(report.tries_words(), "4 so far · no limit set");

        let [judged, parse, overdue] = report.lines.as_slice() else {
            panic!("three lines");
        };
        assert_eq!(
            (judged.state, judged.checked_by.as_str(), judged.met_on_try),
            (LineState::Met, "the judge", Some(2))
        );
        assert_eq!(
            (parse.state, parse.checked_by.as_str(), parse.met_on_try),
            (LineState::Met, "cargo test due::parse", Some(1))
        );
        assert_eq!(overdue.state, LineState::NotMet);
        assert_eq!(overdue.checked_by, "cargo test overdue");
        assert_eq!(overdue.still_fails, "1 of 12 fails");

        // The bar beside each try: failing tests after it, 4, then 2, then 1.
        let failing: Vec<Option<Failing>> = report
            .tries
            .iter()
            .map(|a_try| a_try.still_failing)
            .collect();
        let tests = |count| {
            Some(Failing {
                count,
                what: FailingWhat::Tests,
            })
        };
        assert_eq!(failing, [tests(4), tests(2), tests(1), None]);
        assert!(report.tries[3].running);
        assert!(!report.tries[2].running);

        let [overdue, parse, judge] = report.checks.as_slice() else {
            panic!("the last check ran three things");
        };
        assert_eq!(overdue.summary, "1 failed · 11 passed");
        assert_eq!(
            overdue.output,
            [
                "test overdue::skips_tasks_without_a_date ... FAILED",
                "expected 2 tasks, got 3"
            ]
        );
        assert_eq!(parse.summary, "passed · 6 tests");
        assert_eq!(judge.summary, "1 of 1 line met");
        assert!(judge.agent.is_some());

        let says = report.cli_says.as_ref().expect("Codex says");
        assert_eq!(
            (says.cli.as_str(), says.status.as_str(), says.turns),
            ("Codex", "active", Some(2))
        );
        assert_eq!(says.note, "one test left: tasks without a date");
    }

    /// `c` mid-try: try 3 is still running, and the check you asked
    /// for says one test still fails.
    #[test]
    fn a_check_you_asked_for_says_where_the_running_try_stands() {
        let asked = |failed| match checked(3, failed) {
            GoalEvent::Checked(found) => GoalEvent::CheckedNow(found),
            other => other,
        };
        let mut events = vec![
            goal(1, set()),
            goal(2, started(1)),
            goal(3, checked(1, 4)),
            goal(4, started(2)),
            goal(5, checked(2, 2)),
            goal(6, started(3)),
            goal(
                7,
                GoalEvent::CheckingNow {
                    task: Some(task()),
                    attempt: 3,
                },
            ),
        ];
        let under_way = &reports(&events)[0];
        assert!(under_way.checking);
        assert_eq!(under_way.tries[2].still_failing, None);
        // Until it answers, the lines stand as the last try's check left them.
        assert_eq!(under_way.lines[2].still_fails, "2 of 12 fail");

        events.push(goal(8, asked(1)));
        let found = &reports(&events)[0];
        assert!(!found.checking);
        assert_eq!(found.row_words(), "try 3 · 2 of 3 lines met");
        assert_eq!(found.lines[2].still_fails, "1 of 12 fails");
        assert_eq!(found.checks[0].summary, "1 failed · 11 passed");
        let third = &found.tries[2];
        assert_eq!(
            third.still_failing,
            Some(Failing {
                count: 1,
                what: FailingWhat::Tests,
            })
        );
        assert!(third.running, "the check you asked for doesn't end the try");

        // The try's own check ends it, and is the one its bar shows from then on.
        events.push(goal(9, checked(3, 0)));
        let ended = &reports(&events)[0];
        assert!(!ended.tries[2].running);
        assert_eq!(
            ended.tries[2].still_failing.map(|failing| failing.count),
            Some(0)
        );
        assert_eq!(ended.lines[2].met_on_try, Some(3));
    }

    #[test]
    fn a_check_you_asked_for_that_never_answered_isnt_shown_running_for_ever() {
        let checking = || {
            goal(
                3,
                GoalEvent::CheckingNow {
                    task: Some(task()),
                    attempt: 1,
                },
            )
        };
        let before = vec![goal(1, set()), goal(2, started(1)), checking()];
        assert!(reports(&before)[0].checking);

        let mut dropped = before.clone();
        dropped.push(goal(
            4,
            GoalEvent::CheckNowDropped {
                task: Some(task()),
                why: "the run paused or stopped".to_owned(),
            },
        ));
        assert!(!reports(&dropped)[0].checking);

        // A run that parked under it, as when its engine was halted, runs nothing either.
        let mut parked = before;
        parked.push(at(
            4,
            RunEventKind::Run(RunLifecycle::Parked {
                reason: argonauts_core::events::run::ParkReason::Checkpoint,
                waiting_on: Vec::new(),
            }),
        ));
        let report = &reports(&parked)[0];
        assert!(!report.checking);
        assert_eq!(report.lines[0].state, LineState::Unchecked);
    }

    #[test]
    fn a_goal_that_holds_or_stops_says_so() {
        let mut events = vec![
            goal(1, set()),
            goal(2, started(1)),
            goal(3, checked(1, 2)),
            goal(4, started(2)),
            goal(5, checked(2, 0)),
            goal(
                6,
                GoalEvent::StatusChanged {
                    status: GoalStatus::Met,
                    by: Party::Conductor,
                    task: Some(task()),
                    why: None,
                },
            ),
        ];
        let met = &reports(&events)[0];
        assert_eq!(met.row_words(), "goal met in 2 tries");
        assert!(!met.running());
        assert!(met.tries.iter().all(|a_try| !a_try.running));

        events.truncate(4);
        events.push(goal(
            7,
            GoalEvent::ApproachChanged {
                after: 1,
                why: "the same failure 3 rounds running".to_owned(),
                task: Some(task()),
            },
        ));
        events.push(goal(
            8,
            GoalEvent::StatusChanged {
                status: GoalStatus::Stuck,
                by: Party::Conductor,
                task: Some(task()),
                why: Some("the same failure 3 rounds running, after a change of approach".into()),
            },
        ));
        let stuck = &reports(&events)[0];
        assert_eq!(stuck.status, GoalStatus::Stuck);
        assert!(stuck.tries[1].changed_approach);
        assert_eq!(
            stuck.ended_because.as_deref(),
            Some("the same failure 3 rounds running, after a change of approach")
        );
    }

    #[test]
    fn a_tries_words_come_from_its_report_without_the_goal_line() {
        assert_eq!(
            what_it_did("Fixed the date comparison. Reran the tests.\ngoal: active · 2 left"),
            "Fixed the date comparison"
        );
        assert_eq!(what_it_did("goal: complete · done"), "");
    }
}
