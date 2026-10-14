//! Recaps and titles, built by code.

use argonauts_core::api::StatusSummary;
use argonauts_core::app::TurnReply;
use argonauts_core::events::run::{ClosingAnswer, RunLifecycle, RunMode, RunOutcome};
use argonauts_core::events::task::TaskState;
use argonauts_core::events::{RunEvent, RunEventKind};

/// Longest title, in characters. The terminal cuts the rest anyway.
const TITLE_MAX: usize = 60;

/// Such as "Export now takes --json. All 4 tasks merged, and the checks pass. Ask for another
/// change, or /resume an older run." A run that ended starts with what changed, from its
/// closing answer, rather than its goal, which only repeats what you asked.
pub(crate) fn recap_from_summary(
    summary: &StatusSummary,
    outcome: Option<RunOutcome>,
    closing: Option<&ClosingAnswer>,
) -> String {
    let main_agent = match outcome {
        Some(_) => closing.and_then(what_changed),
        None => Some(goal_line(summary)),
    };
    let tasks = Tasks::count(summary);
    let rest = match outcome {
        Some(RunOutcome::Merged) => match tasks.merged_phrase() {
            Some(merged) => format!(
                "{}, and the checks pass. Ask for another change, or /resume an older run.",
                capitalized(&merged)
            ),
            None => "It merged, and the checks pass. Ask for another change, or /resume an older \
                     run."
                .to_owned(),
        },
        // A run that didn't merge only merged its tasks into its own branch, so they're done,
        // never "merged" beside an ending that says nothing merged.
        Some(RunOutcome::PullRequest) => {
            with_done(&tasks, "The work waits in a pull request for review")
        }
        // The recap is of the run on screen, which `/resume` alone carries on.
        Some(RunOutcome::BranchLeft) => format!(
            "{} /resume picks it up.",
            with_done(&tasks, "The branches are kept for later")
        ),
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

/// The newest closing answer in a run's events.
pub(crate) fn closing_in(events: &[RunEvent]) -> Option<&ClosingAnswer> {
    events.iter().rev().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::ClosingAnswer(answer)) => Some(answer.as_ref()),
        _ => None,
    })
}

/// The agent's first sentence, or without an agent's words, the files that changed.
fn what_changed(closing: &ClosingAnswer) -> Option<String> {
    if closing.by.is_some() {
        let text = closing.text.trim();
        let first = match text.find(". ") {
            Some(end) => &text[..end],
            None => text,
        };
        if !first.is_empty() {
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
    use argonauts_core::api::TaskBrief;
    use argonauts_core::events::run::{CaptainOrCrewDecision, ChangedFile};
    use argonauts_core::{RunId, TaskId};

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
            recap_from_summary(&merged, Some(RunOutcome::Merged), None),
            "All 4 tasks merged, and the checks pass. Ask for another change, or /resume an \
             older run."
        );
        let stopped = summary(vec![
            task(1, TaskState::Merged),
            task(2, TaskState::Running),
            task(3, TaskState::Cancelled),
        ]);
        assert_eq!(
            recap_from_summary(&stopped, Some(RunOutcome::Stopped), None),
            "The run stopped, with 1 of 2 tasks done. /resume goes on from there."
        );
        let mut going = stopped.clone();
        going.waiting = vec!["the merge approval".to_owned()];
        assert_eq!(
            recap_from_summary(&going, None, None),
            "Export now takes --json. 1 of 2 tasks merged, 1 working. Waiting on the merge \
             approval."
        );
        let mut needs_you = summary(vec![task(1, TaskState::NeedsYou)]);
        needs_you.waiting = vec!["Fix add so it returns a + b needs you".to_owned()];
        assert_eq!(
            recap_from_summary(&needs_you, None, None),
            "Export now takes --json. The task isn't merged. Waiting on Fix add so it returns a + \
             b needs you."
        );
        // Before any question, a run that waits on you says only that, never that it's under way.
        let mut asking = summary(Vec::new());
        asking.waiting = vec!["Question from build:greet".to_owned()];
        assert_eq!(
            recap_from_summary(&asking, None, None),
            "Export now takes --json. Waiting on Question from build:greet."
        );
        let mut working = summary(Vec::new());
        working.recent = vec!["Goal approved".to_owned()];
        assert_eq!(
            recap_from_summary(&working, None, None),
            "Export now takes --json. It's under way. Last: Goal approved."
        );
        let empty = StatusSummary::default();
        assert_eq!(
            recap_from_summary(&empty, Some(RunOutcome::Abandoned), None),
            "You abandoned the run, so nothing merged."
        );
        let one = summary(vec![task(1, TaskState::Merged)]);
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::PullRequest), None),
            "The work waits in a pull request for review, with the task done."
        );
        // A run left on its branches never says its task merged.
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::BranchLeft), None),
            "The branches are kept for later, with the task done. /resume picks it up."
        );
        assert_eq!(
            recap_from_summary(&stopped, Some(RunOutcome::Failed), None),
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
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::Merged), Some(&closing)),
            "Fixed parse() and format_cents() in invoice/money.py. The task merged, and the \
             checks pass. Ask for another change, or /resume an older run."
        );
        closing.by = None;
        assert_eq!(
            recap_from_summary(&one, Some(RunOutcome::Merged), Some(&closing)),
            "Changed invoice/money.py. The task merged, and the checks pass. Ask for another \
             change, or /resume an older run."
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
