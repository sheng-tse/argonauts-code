//! `argonauts_lookup`: the main session answers about your own crew from Argonauts Code's records,
//! never from a guess. Reviews, tasks and fallbacks come from this repository's run records,
//! tokens and spend from the usage ledger, and the setup from the settings in effect. Where
//! there is no record, the answer says so, and so does the session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use argonauts_core::layout::{project_files, user_files};
use argonauts_core::meter::Spend;
use argonauts_core::names::ModelRef;
use argonauts_core::prompt_files::{self, PROJECT_DIR};
use argonauts_core::required::{self, RequiredStep};
use argonauts_core::settings::layer;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::text::state_words;
use argonauts_mcp::session::lookup::{DEFAULT_DAYS, WHAT};
use argonauts_tui::format;
use serde_json::Value;
use time::{Duration, OffsetDateTime};

use super::records::{self, Records, Review};
use super::{Call, ConversationState, fail};
use crate::controller::{Ctx, Effect};
use crate::usage_ledger::{UsageLedger, local_day};

pub(crate) const NAME: &str = argonauts_mcp::session::lookup::NAME;

pub(crate) const ABOUT: &str = "`argonauts_lookup` reads Argonauts Code's own records for this \
    repository: each model's reviews and what it sent back and why, tasks, tokens, spend, \
    fallbacks, and the setup in use. Answer any question about the user's crew, models or \
    setup from its figures, never from a guess, and where it finds no record, say so.";

pub(crate) fn offered(_ctx: &Ctx, conversation: &ConversationState) -> bool {
    conversation.place.is_none()
}

pub(crate) fn call(ctx: &mut Ctx, conversation: &mut ConversationState, call: Call) -> Vec<Effect> {
    match Query::read(&call.arguments) {
        Ok(query) => {
            let kept = &conversation.talk.steps_for_conversation;
            let answer = answer(ctx, kept, &query, OffsetDateTime::now_utc());
            call.answer(answer);
        }
        Err(problem) => fail(call, problem),
    }
    Vec::new()
}

/// One lookup, as the call asks for it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Query {
    pub what: String,
    pub days: u32,
    /// Only this model or CLI, lowercase.
    pub model: Option<String>,
    pub everywhere: bool,
}

impl Query {
    fn read(arguments: &Value) -> Result<Self, String> {
        let what = arguments
            .get("what")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        if !WHAT.iter().any(|(name, _)| *name == what) {
            let names: Vec<&str> = WHAT.iter().map(|(name, _)| *name).collect();
            return Err(format!(
                "There's no record of {what:?} to look up. Ask for one of {}.",
                names.join(", ")
            ));
        }
        let days = arguments
            .get("days")
            .and_then(Value::as_u64)
            .map(|days| days.clamp(1, 365) as u32)
            .unwrap_or(DEFAULT_DAYS);
        let model = arguments
            .get("model")
            .and_then(Value::as_str)
            .map(|model| model.trim().to_lowercase())
            .filter(|model| !model.is_empty());
        let everywhere = arguments
            .get("everywhere")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Self {
            what,
            days,
            model,
            everywhere,
        })
    }

    /// `over the last 7 days`.
    fn period(&self) -> String {
        match self.days {
            1 => "over the last day".to_owned(),
            days => format!("over the last {days} days"),
        }
    }

    /// Whether `worker` is the model or CLI asked about, by any part of its id or name.
    fn about(&self, ctx: &Ctx, worker: &ModelRef) -> bool {
        let Some(asked) = &self.model else {
            return true;
        };
        let named = [
            worker.agent.clone(),
            worker.model.clone().unwrap_or_default(),
            ctx.names.worker(worker),
        ];
        named
            .iter()
            .any(|name| name.to_lowercase().contains(asked.as_str()))
    }

    fn whose(&self) -> String {
        match &self.model {
            Some(model) => format!(" by {model}"),
            None => String::new(),
        }
    }
}

/// What `query` asks for. `kept` are the steps a setup kept for this conversation.
pub(super) fn answer(
    ctx: &Ctx,
    kept: &[RequiredStep],
    query: &Query,
    now: OffsetDateTime,
) -> String {
    match query.what.as_str() {
        "tokens" | "spend" => ledger(ctx, query, now),
        "setup" => setup(ctx, kept),
        what => {
            let since = now - Duration::days(i64::from(query.days));
            let records = records::read(&ctx.env.runs_dir(), since);
            match what {
                "reviews" => reviews(ctx, query, &records),
                "tasks" => tasks(ctx, query, &records),
                _ => fallbacks(ctx, query, &records),
            }
        }
    }
}

/// Reviews grouped by the model that made them, each with what it sent back and why.
fn reviews(ctx: &Ctx, query: &Query, records: &Records) -> String {
    let mut by_model: BTreeMap<String, Vec<&Review>> = BTreeMap::new();
    for review in &records.reviews {
        if query.about(ctx, &review.reviewer) {
            by_model
                .entry(ctx.names.worker(&review.reviewer))
                .or_default()
                .push(review);
        }
    }
    if by_model.is_empty() {
        let mut said = format!(
            "No record of a review{} in this repository {}.",
            query.whose(),
            query.period()
        );
        let others = reviewers(ctx, records);
        if query.model.is_some() && !others.is_empty() {
            said.push_str(&format!(
                " The reviews there are came from {}.",
                others.join(", ")
            ));
        }
        return said;
    }
    let mut lines = vec![format!(
        "Reviews in this repository {}, from the run records:",
        query.period()
    )];
    for (model, reviews) in by_model {
        let approved = reviews
            .iter()
            .filter(|review| review.verdict == ReviewVerdict::Approve)
            .count();
        let sent_back: Vec<&&Review> = reviews
            .iter()
            .filter(|review| review.verdict != ReviewVerdict::Approve)
            .collect();
        lines.push(format!(
            "- {model}: {} {}, approved {approved}, sent back {}.",
            reviews.len(),
            plural(reviews.len(), "review", "reviews"),
            sent_back.len()
        ));
        for review in sent_back.iter().take(MAX_LISTED) {
            let verdict = match review.verdict {
                ReviewVerdict::Reject => "rejected",
                _ => "changes asked",
            };
            lines.push(format!(
                "  - {} ({verdict}): {}",
                task_named(&review.task_id, &review.task, &review.run),
                review.summary
            ));
            for comment in review.comments.iter().take(MAX_COMMENTS) {
                lines.push(format!("    {comment}"));
            }
        }
        if sent_back.len() > MAX_LISTED {
            lines.push(format!("  and {} more", sent_back.len() - MAX_LISTED));
        }
    }
    lines.join("\n")
}

/// Sent-back reviews listed for one model, and comments for each, so an answer stays short.
const MAX_LISTED: usize = 20;
const MAX_COMMENTS: usize = 3;
const MAX_TASKS: usize = 40;

fn reviewers(ctx: &Ctx, records: &Records) -> Vec<String> {
    let mut names: Vec<String> = records
        .reviews
        .iter()
        .map(|review| ctx.names.worker(&review.reviewer))
        .collect();
    names.sort();
    names.dedup();
    names
}

fn tasks(ctx: &Ctx, query: &Query, records: &Records) -> String {
    let tasks: Vec<_> = records
        .tasks
        .iter()
        .filter(|task| {
            query.model.is_none() || task.worker.as_ref().is_some_and(|w| query.about(ctx, w))
        })
        .collect();
    if tasks.is_empty() {
        return format!(
            "No record of a task{} in this repository {}.",
            match &query.model {
                Some(model) => format!(" built by {model}"),
                None => String::new(),
            },
            query.period()
        );
    }
    let mut runs: Vec<_> = tasks.iter().map(|task| &task.run).collect();
    runs.sort();
    runs.dedup();
    let mut by_state: BTreeMap<String, usize> = BTreeMap::new();
    for task in &tasks {
        *by_state.entry(state(task.state)).or_default() += 1;
    }
    let counts: Vec<String> = by_state
        .iter()
        .map(|(state, count)| format!("{state} {count}"))
        .collect();
    let mut lines = vec![
        format!(
            "Tasks in this repository {}, from the run records: {} in {} {}.",
            query.period(),
            tasks.len(),
            runs.len(),
            plural(runs.len(), "run", "runs")
        ),
        counts.join(" · "),
    ];
    for task in tasks.iter().take(MAX_TASKS) {
        let built = match &task.worker {
            Some(worker) => format!(", built by {}", ctx.names.worker(worker)),
            None => String::new(),
        };
        lines.push(format!(
            "- {}: {}{built}",
            task_named(
                &Some(task.task_id.clone()),
                &Some(task.title.clone()),
                &task.run
            ),
            state(task.state)
        ));
    }
    if tasks.len() > MAX_TASKS {
        lines.push(format!("and {} more", tasks.len() - MAX_TASKS));
    }
    lines.join("\n")
}

fn fallbacks(ctx: &Ctx, query: &Query, records: &Records) -> String {
    let taken: Vec<_> = records
        .fallbacks
        .iter()
        .filter(|fallback| {
            query.about(ctx, &fallback.stopped)
                || fallback.to.as_ref().is_some_and(|to| query.about(ctx, to))
        })
        .collect();
    if taken.is_empty() {
        return format!(
            "No record of a fallback{} in this repository {}: no model stopped and fell back to \
             another.",
            query.whose(),
            query.period()
        );
    }
    let mut lines = vec![format!(
        "Fallbacks in this repository {}, from the run records:",
        query.period()
    )];
    for fallback in taken {
        let then = match &fallback.to {
            Some(to) => format!("it fell back to {}", ctx.names.worker(to)),
            None => "nothing could take its place".to_owned(),
        };
        lines.push(format!(
            "- {}: {} stopped ({}), and {then}.",
            task_named(
                &Some(fallback.task_id.clone()),
                &Some(fallback.task.clone()),
                &fallback.run
            ),
            ctx.names.worker(&fallback.stopped),
            fallback.reason
        ));
    }
    lines.join("\n")
}

/// Tokens or spend by model, from the usage ledger.
fn ledger(ctx: &Ctx, query: &Query, now: OffsetDateTime) -> String {
    let today = local_day(now);
    let first = today - Duration::days(i64::from(query.days) - 1);
    let repo = ctx.env.repo_root.display().to_string();
    let mut by_model: BTreeMap<String, Spend> = BTreeMap::new();
    for line in UsageLedger::new(&ctx.env.paths).read_since(Some(first)) {
        let worker = ModelRef::new(line.agent.clone(), line.model.clone());
        if (query.everywhere || line.repo == repo) && query.about(ctx, &worker) {
            let spent = by_model.entry(ctx.names.worker(&worker)).or_default();
            *spent = spent.plus(&line.spent());
        }
    }
    let place = if query.everywhere {
        "in every repository"
    } else {
        "in this repository"
    };
    let tokens = query.what == "tokens";
    if by_model.is_empty() {
        let what = if tokens { "tokens" } else { "spend" };
        return format!(
            "No record of {what}{} {place} {}.",
            query.whose(),
            query.period()
        );
    }
    let what = if tokens { "Tokens" } else { "Spend" };
    let mut lines = vec![format!(
        "{what} {place} {}, from the usage ledger:",
        query.period()
    )];
    let mut total = Spend::default();
    for (model, spent) in &by_model {
        total = total.plus(spent);
        lines.push(if tokens {
            format!(
                "- {model}: {} in, {} of them cached, {} out",
                format::tokens(spent.input_tokens),
                format::tokens(spent.cached_tokens),
                format::tokens(spent.output_tokens)
            )
        } else {
            format!("- {model}: {}", format::spend(spent))
        });
    }
    lines.push(if tokens {
        format!(
            "All together: {} tokens",
            format::tokens(total.total_tokens())
        )
    } else {
        format!("All together: {}", format::spend(&total))
    });
    lines.join("\n")
}

/// The setup in use: what decides where work goes, then everything the user set, then their
/// own files.
fn setup(ctx: &Ctx, kept: &[RequiredStep]) -> String {
    const IN_USE: [&str; 4] = ["crew.main", "workflow", "ultracode", "workflow_size"];
    let entries = ctx.settings.entries();
    let mut lines = vec!["The setup in use here:".to_owned()];
    for key in IN_USE {
        if let Some(entry) = entries.iter().find(|entry| entry.key == key) {
            lines.push(format!("- {key} = {} ({})", entry.value, entry.layer));
        }
    }
    let set: Vec<String> = entries
        .iter()
        .filter(|entry| !IN_USE.contains(&entry.key.as_str()))
        .filter(|entry| entry.layer != layer::BUILT_IN && entry.layer != layer::WORKFLOW)
        .map(|entry| format!("- {} = {} ({})", entry.key, entry.value, entry.layer))
        .collect();
    if set.is_empty() {
        lines.push("The user set nothing else: every other setting is built in.".to_owned());
    } else {
        lines.push("Set by the user:".to_owned());
        lines.extend(set);
    }
    let dirs = [
        ctx.env.repo_root.join(PROJECT_DIR),
        ctx.env.paths.home().to_path_buf(),
    ];
    for (folder, what) in [
        (prompt_files::ROLES_DIR, "Their instructions for a role"),
        (prompt_files::PROMPTS_DIR, "Their own prompts"),
        ("mods", "Their mods"),
    ] {
        let files: Vec<String> = dirs
            .iter()
            .flat_map(|dir| files_under(&dir.join(folder)))
            .map(|path| ctx.env.shown(&path))
            .collect();
        if !files.is_empty() {
            lines.push(format!("{what}: {}", files.join(", ")));
        }
    }
    lines.extend(required_steps(ctx, kept));
    lines.join("\n")
}

/// The steps every workflow must contain, from `always.toml` here and in yours and the ones a
/// setup `kept` for this conversation, with any file that isn't used and why.
fn required_steps(ctx: &Ctx, kept: &[RequiredStep]) -> Vec<String> {
    let yours = ctx.env.paths.home().join(user_files::WORKFLOWS);
    let here = ctx
        .env
        .repo_root
        .join(PROJECT_DIR)
        .join(project_files::WORKFLOWS);
    let read = required::read_with(&[&yours, &here], kept, "");
    let (this_conversation, always): (Vec<&RequiredStep>, Vec<&RequiredStep>) =
        read.required.steps.iter().partition(|step| {
            kept.iter()
                .any(|one| required::same_name(&one.name, &step.name))
        });
    let mut lines = Vec::new();
    for (heading, steps) in [
        ("Steps every workflow must contain:", always),
        (
            "Steps every workflow in this conversation must contain:",
            this_conversation,
        ),
    ] {
        if !steps.is_empty() {
            lines.push(heading.to_owned());
            lines.extend(
                steps
                    .into_iter()
                    .map(|step| format!("- {}", required::brief::step_line(step))),
            );
        }
    }
    lines.extend(read.problems);
    lines
}

/// The files of a folder of yours, its README left out, two folders deep at most.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                if depth > 0 {
                    walk(&path, depth - 1, out);
                }
            } else if path
                .file_name()
                .is_some_and(|name| name != prompt_files::README_FILE)
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, 1, &mut out);
    out
}

/// `task-003 "Add due dates" (run r-002)`.
fn task_named(
    task_id: &Option<argonauts_core::TaskId>,
    title: &Option<String>,
    run: &argonauts_core::RunId,
) -> String {
    let mut named = task_id
        .as_ref()
        .map(|task_id| task_id.to_string())
        .unwrap_or_else(|| "a change".to_owned());
    if let Some(title) = title.as_ref().filter(|title| !title.is_empty()) {
        named.push_str(&format!(" \"{title}\""));
    }
    format!("{named} (run {run})")
}

/// A task's state as the screen says it, such as `merged into the run`.
fn state(state: Option<argonauts_core::events::task::TaskState>) -> String {
    let name = state
        .and_then(|state| serde_json::to_value(state).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "not started".to_owned());
    state_words(&name).replace('_', " ")
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

#[cfg(test)]
mod tests {
    use argonauts_core::events::agent_status::FallbackPlan;
    use argonauts_core::events::task::{TaskKind, TaskLimit, TaskSpec, TaskState};
    use argonauts_core::events::{FallbackEvent, ReviewEvent, RunEvent, RunEventKind, TaskEvent};
    use argonauts_core::layout::run_files;
    use argonauts_core::settings::crew::CrewRole;
    use argonauts_core::settings::roles::FallbackMode;
    use argonauts_core::steps::judgment::{ReviewComment, ReviewOutput};
    use argonauts_core::steps::plan::Severity;
    use argonauts_core::{RunId, TaskId};
    use serde_json::json;

    use super::*;
    use crate::controller::test_support::{CliOptions, TestCtx};
    use crate::usage_ledger::LedgerLine;

    fn ctx() -> TestCtx {
        TestCtx::new(CliOptions::default()).0
    }

    fn query(what: &str, model: Option<&str>) -> Query {
        Query::read(&json!({"what": what, "model": model})).expect("a query")
    }

    fn gemini() -> ModelRef {
        ModelRef::new("antigravity-cli", Some("gemini-3.6-pro".to_owned()))
    }

    fn opus() -> ModelRef {
        ModelRef::new("claude-code", Some("opus".to_owned()))
    }

    fn event(seq: u64, ts: OffsetDateTime, task_id: &TaskId, kind: RunEventKind) -> RunEvent {
        RunEvent {
            seq,
            ts,
            run_id: RunId::new("r-001").expect("a run id"),
            task_id: Some(task_id.clone()),
            session: None,
            source: "argonauts-code".to_owned(),
            kind,
        }
    }

    fn task(task_id: &TaskId, title: &str) -> RunEventKind {
        RunEventKind::Task(TaskEvent::Created(Box::new(TaskSpec {
            id: task_id.clone(),
            title: title.to_owned(),
            task: title.to_owned(),
            kind: TaskKind::Captain,
            owned_paths: Vec::new(),
            read_only_paths: Vec::new(),
            forbidden_paths: Vec::new(),
            hidden_paths: Vec::new(),
            depends_on: Vec::new(),
            criteria: Vec::new(),
            worker: Some(opus()),
            reviewers: vec![gemini()],
            limit: TaskLimit::default(),
            predictions: None,
            item: None,
            way: None,
        })))
    }

    fn reviewed(verdict: ReviewVerdict, summary: &str) -> RunEventKind {
        RunEventKind::Review(ReviewEvent::Reviewed {
            reviewer: gemini(),
            blind: true,
            round: 1,
            review: ReviewOutput {
                verdict,
                summary: summary.to_owned(),
                comments: vec![ReviewComment {
                    path: "src/task.rs".to_owned(),
                    line: Some(12),
                    severity: Severity::High,
                    text: "No test covers --due.".to_owned(),
                }],
                tamper_suspected: false,
            },
            picked_by: None,
        })
    }

    /// One run's journal: two tasks, two reviews of the first, and a fallback on the second.
    fn write_run(test: &TestCtx, now: OffsetDateTime) {
        let first = TaskId::new("task-001").expect("a task id");
        let second = TaskId::new("task-002").expect("a task id");
        let then = now - Duration::hours(2);
        let plan = FallbackPlan {
            task: second.clone(),
            title: "Parse dates".to_owned(),
            stopped: opus(),
            reason: "usage limit".to_owned(),
            resets_at: None,
            work: argonauts_core::DiffStat::default(),
            checks_ran: false,
            candidates: Vec::new(),
            mode: FallbackMode::Auto,
            strategy: Default::default(),
            race_count: 1,
            also_moving: Vec::new(),
        };
        let codex = ModelRef::new("codex", Some("gpt-5.6".to_owned()));
        let events = [
            event(0, then, &first, task(&first, "Add due dates")),
            event(1, then, &second, task(&second, "Parse dates")),
            event(
                2,
                then,
                &first,
                reviewed(ReviewVerdict::RequestChanges, "A test is missing."),
            ),
            event(
                3,
                then,
                &first,
                reviewed(ReviewVerdict::Approve, "Good now."),
            ),
            event(
                4,
                then,
                &first,
                RunEventKind::Task(TaskEvent::State {
                    from: None,
                    to: TaskState::Merged,
                    reason: String::new(),
                }),
            ),
            event(
                5,
                then,
                &second,
                RunEventKind::Fallback(FallbackEvent::Needed(plan)),
            ),
            event(
                6,
                then,
                &second,
                RunEventKind::Fallback(FallbackEvent::FellBack {
                    task: second.clone(),
                    to: codex,
                    mode: FallbackMode::Auto,
                    by: argonauts_core::events::run::Party::Rules,
                    context: None,
                }),
            ),
        ];
        let dir = test.ctx.env.runs_dir().join("r-001");
        std::fs::create_dir_all(&dir).unwrap();
        let text: String = events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap() + "\n")
            .collect();
        std::fs::write(dir.join(run_files::EVENTS), text).unwrap();
    }

    #[test]
    fn reviews_say_what_each_model_sent_back_and_why() {
        let test = ctx();
        let now = OffsetDateTime::now_utc();
        write_run(&test, now);
        let said = answer(&test.ctx, &[], &query("reviews", None), now);
        assert!(
            said.starts_with("Reviews in this repository over the last 7 days"),
            "{said}"
        );
        assert!(
            said.contains(": 2 reviews, approved 1, sent back 1."),
            "{said}"
        );
        assert!(
            said.contains(
                "task-001 \"Add due dates\" (run r-001) (changes asked): A test is missing."
            ),
            "{said}"
        );
        assert!(
            said.contains("[high] src/task.rs:12 No test covers --due."),
            "{said}"
        );
    }

    #[test]
    fn a_model_with_no_reviews_has_no_record() {
        let test = ctx();
        let now = OffsetDateTime::now_utc();
        write_run(&test, now);
        let said = answer(&test.ctx, &[], &query("reviews", Some("astra")), now);
        assert!(
            said.starts_with("No record of a review by astra in this repository"),
            "{said}"
        );
        assert!(said.contains("The reviews there are came from"), "{said}");
        let gemini = answer(&test.ctx, &[], &query("reviews", Some("gemini")), now);
        assert!(gemini.contains("2 reviews"), "{gemini}");
        let long_ago = now + Duration::days(30);
        let none = answer(&test.ctx, &[], &query("reviews", None), long_ago);
        assert!(none.starts_with("No record of a review"), "{none}");
    }

    #[test]
    fn tasks_and_fallbacks_come_from_the_run_records() {
        let test = ctx();
        let now = OffsetDateTime::now_utc();
        write_run(&test, now);
        let tasks = answer(&test.ctx, &[], &query("tasks", None), now);
        assert!(tasks.contains("2 in 1 run."), "{tasks}");
        assert!(tasks.contains("merged 1"), "{tasks}");
        assert!(
            tasks.contains("task-002 \"Parse dates\" (run r-001): not started"),
            "{tasks}"
        );
        let fallbacks = answer(&test.ctx, &[], &query("fallbacks", None), now);
        assert!(
            fallbacks.contains("stopped (usage limit), and "),
            "{fallbacks}"
        );
        assert!(fallbacks.contains(", and it fell back to "), "{fallbacks}");
        let none = answer(&test.ctx, &[], &query("fallbacks", Some("astra")), now);
        assert!(
            none.starts_with("No record of a fallback by astra"),
            "{none}"
        );
    }

    #[test]
    fn tokens_and_spend_come_from_the_usage_ledger() {
        let test = ctx();
        let now = OffsetDateTime::now_utc();
        let none = answer(&test.ctx, &[], &query("spend", None), now);
        assert!(
            none.starts_with("No record of spend in this repository"),
            "{none}"
        );
        let line = |repo: &str, cost: f64| LedgerLine {
            day: local_day(now),
            repo: repo.to_owned(),
            run: None,
            agent: "claude-code".to_owned(),
            model: Some("opus".to_owned()),
            agent_id: "anthropic".to_owned(),
            input: 1000,
            cached: 400,
            output: 200,
            cost,
            estimated: false,
            partial: false,
            hour: None,
            conversation: None,
            took: 0,
            billing: None,
        };
        let here = test.ctx.env.repo_root.display().to_string();
        UsageLedger::new(&test.ctx.env.paths)
            .append(&[line(&here, 1.5), line("/elsewhere", 2.0)])
            .unwrap();
        let spend = answer(&test.ctx, &[], &query("spend", None), now);
        assert!(spend.contains("All together: $1.50"), "{spend}");
        let everywhere = Query {
            everywhere: true,
            ..query("spend", None)
        };
        let all = answer(&test.ctx, &[], &everywhere, now);
        assert!(all.contains("in every repository"), "{all}");
        assert!(all.contains("All together: $3.50"), "{all}");
        let tokens = answer(&test.ctx, &[], &query("tokens", None), now);
        let counts = format!(
            "{} in, {} of them cached, {} out",
            format::tokens(1000),
            format::tokens(400),
            format::tokens(200)
        );
        assert!(tokens.contains(&counts), "{tokens}");
        assert!(tokens.contains("All together:"), "{tokens}");
    }

    /// The workflow's own values are its, not the user's.
    #[test]
    fn the_setup_never_says_the_user_set_what_the_workflow_sets() {
        let test = TestCtx::new(CliOptions {
            prepare: Some(Box::new(|repo: &argonauts_testkit::TempRepo| {
                std::fs::write(repo.global_config(), "workflow = \"orchestrator\"\n").unwrap();
            })),
            ..CliOptions::default()
        })
        .0;
        let from_the_workflow = test
            .ctx
            .settings
            .entries()
            .iter()
            .filter(|entry| entry.layer == layer::WORKFLOW && entry.key != "workflow_size")
            .count();
        assert!(from_the_workflow > 0);
        let said = answer(
            &test.ctx,
            &[],
            &query("setup", None),
            OffsetDateTime::now_utc(),
        );
        let set_by_the_user = said
            .split_once("Set by the user:")
            .map_or("", |(_, rest)| rest);
        assert!(
            !set_by_the_user.contains(&format!("({})", layer::WORKFLOW)),
            "{said}"
        );
    }

    #[test]
    fn the_setup_names_what_the_user_set() {
        let test = ctx();
        let roles = test.repo.root().join(".argonauts/roles");
        std::fs::create_dir_all(&roles).unwrap();
        std::fs::write(roles.join("reviewer.md"), "Be strict.").unwrap();
        let said = answer(
            &test.ctx,
            &[],
            &query("setup", None),
            OffsetDateTime::now_utc(),
        );
        assert!(said.starts_with("The setup in use here:"), "{said}");
        assert!(said.contains("- workflow = "), "{said}");
        assert!(
            said.contains("Their instructions for a role: .argonauts/roles/reviewer.md"),
            "{said}"
        );
        assert!(!said.contains("Steps every workflow"), "{said}");
        let workflows = test.repo.root().join(".argonauts/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("always.toml"),
            "[[step]]\nname = \"Benchmark\"\nwhen = \"last\"\n",
        )
        .unwrap();
        let said = answer(
            &test.ctx,
            &[],
            &query("setup", None),
            OffsetDateTime::now_utc(),
        );
        assert!(
            said.contains("Steps every workflow must contain:\n- Benchmark, last, by the workers"),
            "{said}"
        );

        let kept = [RequiredStep {
            name: "Benchmark".to_owned(),
            role: CrewRole::Reviewer,
            does: None,
            when: Some("first".to_owned()),
        }];
        let said = answer(
            &test.ctx,
            &kept,
            &query("setup", None),
            OffsetDateTime::now_utc(),
        );
        assert!(
            said.contains(
                "Steps every workflow in this conversation must contain:\n\
                 - Benchmark, first, by the reviewers"
            ),
            "{said}"
        );
        assert!(!said.contains("Benchmark, last"), "{said}");
    }

    #[test]
    fn a_lookup_of_nothing_known_names_what_there_is() {
        let err = Query::read(&json!({"what": "moods"})).unwrap_err();
        assert!(
            err.contains("reviews, tasks, tokens, spend, fallbacks, setup"),
            "{err}"
        );
        let days = Query::read(&json!({"what": "tasks", "days": 9000})).unwrap();
        assert_eq!(days.days, 365);
    }
}
