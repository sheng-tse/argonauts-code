//! The worker's prompt as parts: the orchestrator's brief, the task, the goal, notes, decisions,
//! rules, the filtered status summary, and the report schema. The session view shows exactly
//! what was sent.

use std::fmt::Write as _;
use std::path::{Component, Path};

use argonauts_core::TaskId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::agent_status::FallbackEvent;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::goal::GoalFile;
use argonauts_core::events::plan::DesignDecision;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::task::{TaskKind, TaskSpec, TryCause};
use argonauts_core::hash::sha256_hex;
use argonauts_core::layout;
use argonauts_core::limits::FallbackNote;
use argonauts_core::names::{ModelRef, RoleName};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::settings::Settings;
use argonauts_core::steps::StepName;

use crate::approvals::autopilot;
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::fallback::context;
use crate::prompts;
use crate::prompts::instructions::Agent;
use crate::status::Viewer;
use crate::task::at_start;
use crate::task::support::{part, progress_of, record_of, task_events, worker_of};

/// Notes are cut to this many characters each, so one long file can't crowd out the task.
const NOTE_CHARS: usize = 8_000;

pub fn worker_parts(host: &Host, task: &TaskId) -> EngineResult<Vec<InputPart>> {
    let record = record_of(host, task)?;
    let (goal, decisions) =
        host.read(|state| (state.goal.approved.clone(), state.plan.decisions.clone()));
    // The frame for the worker's role goes on when the session starts. Where the orchestrator
    // wrote the task's brief, the worker reads it as the orchestrator's instructions, and the
    // task part keeps the facts: its paths, its kind, and what it depends on.
    let agent = Agent {
        role: RoleName::Worker,
        step: Some(StepName::Build),
        at: None,
        task: Some(task),
    };
    let brief = prompts::assemble::written(host, &agent);
    let text = match &brief {
        Some(_) => task_facts(&record.spec),
        None => task_text(&record.spec),
    };
    let mut parts: Vec<InputPart> = brief.into_iter().collect();
    parts.push(part(InputPartKind::Task, task.as_str(), text, None));
    if let Some(goal) = &goal {
        let source = Some(goal.path.display().to_string());
        parts.push(part(InputPartKind::Goal, "goal", goal_text(goal), source));
    }
    let tree = record
        .worktree
        .clone()
        .unwrap_or_else(|| host.repo().base_worktree.clone());
    parts.extend(note_parts(&tree, &crate::divide::repo_state_dir(host)));
    if !decisions.is_empty() {
        let ids = decisions
            .iter()
            .map(|d| d.id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(part(
            InputPartKind::Decisions,
            "decisions",
            decisions_text(&decisions),
            Some(ids),
        ));
    }
    if let Some(failing) = host.read(at_start::note) {
        parts.push(part(
            InputPartKind::Notes,
            at_start::ALREADY_FAILING,
            failing,
            None,
        ));
    }
    let worker = worker_of(host, &record);
    let loaded = host
        .services()
        .workers
        .get(&worker)
        .map(|model| model.instruction_files(&tree, false))
        .unwrap_or_default();
    parts.push(part(
        InputPartKind::Rules,
        "rules",
        rules_text(&record.spec, host.settings(), &tree, &loaded),
        None,
    ));
    // Where the worker has no status tool, the engine adds the summary to every start itself.
    if has_status_tool(host, &worker) {
        parts.push(status(host, task));
    }
    let build = &host.settings().prompts.build;
    let report = replaced(host, build, prompts::task::BUILD);
    // On autopilot nobody answers for the person, so the report offers no question for them.
    let report = if autopilot::applies_now(host, ApprovalKind::Question) {
        prompts::task::report_alone(report)
    } else {
        report.to_owned()
    };
    parts.push(part(
        InputPartKind::Schema,
        "report",
        report,
        (!build.trim().is_empty()).then(|| build.clone()),
    ));
    Ok(parts)
}

/// The Markdown file `prompts.<key>` names (`file`), which a learned workflow sets: a repo path
/// read from the run's base, or the full path of a file beside the run's learned workflow
/// version, which may be a candidate's in Argonauts Code's state. A file that's missing, empty,
/// or anywhere else keeps the built-in, which is your own template when you wrote one.
pub fn replaced(host: &Host, file: &str, built_in: &'static str) -> &'static str {
    let built_in = prompts::files::text(host, built_in);
    let file = file.trim();
    if file.is_empty() {
        return built_in;
    }
    let path = Path::new(file);
    let text = if path.is_absolute() {
        match crate::run_review::learned_workflow::learned_workflow_file(&host.repo().run_dir, path)
        {
            Some(path) => std::fs::read_to_string(path),
            None => {
                tracing::warn!(
                    "prompts: {file} isn't a file of this run's learned workflow, so the built-in is used"
                );
                return built_in;
            }
        }
    } else if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        let repo = host.repo();
        std::fs::read_to_string(repo.base_worktree.join(path))
            .or_else(|_| std::fs::read_to_string(repo.root.join(path)))
    } else {
        tracing::warn!("prompts: {file} isn't a path inside the repo, so the built-in is used");
        return built_in;
    };
    match text {
        Ok(text) if !text.trim().is_empty() => prompts::files::intern(text),
        Ok(_) => {
            tracing::warn!("prompts: {file} is empty, so the built-in is used");
            built_in
        }
        Err(err) => {
            tracing::warn!("prompts: {file}: {err}, so the built-in is used");
            built_in
        }
    }
}

/// Where the worker has no status tool (agy, and Codex for now), the status summary goes again
/// as a `Status` part with each delivered message and each resume.
pub fn status_part(host: &Host, task: &TaskId, has_status_tool: bool) -> Option<InputPart> {
    (!has_status_tool).then(|| status(host, task))
}

fn status(host: &Host, task: &TaskId) -> InputPart {
    let viewer = Viewer::Session {
        role: RoleName::Worker,
        task: Some(task.clone()),
        label: None,
    };
    part(
        InputPartKind::Status,
        "status",
        host.status_text(&viewer),
        None,
    )
}

/// The check output the worker's session resumes with.
pub fn check_output_part(output: &str) -> InputPart {
    let text = format!(
        "Your task's checks failed. Fix the cause, not the check, then end with your report \
         again.\n\n{output}"
    );
    part(InputPartKind::CheckOutput, "check output", text, None)
}

/// A reviewer's requested changes, for the worker's session.
pub fn review_part(round: u32, comments: &[String]) -> InputPart {
    let mut text = format!(
        "Review round {round} asked for changes. Make them, then end with your report again.\n"
    );
    for comment in comments {
        let _ = write!(text, "\n- {comment}");
    }
    part(InputPartKind::Message, "review", text, None)
}

pub fn message_part(label: &str, text: impl Into<String>) -> InputPart {
    part(InputPartKind::Message, label, text, None)
}

/// What a fresh session of `to` is told after `cause`, as `fallback.context` says: nothing for
/// `fresh`, else the CLI-neutral fallback note (the one given, or one built from the log at
/// no token cost) as a note. The note is saved, recorded as `Fallback::Note`, and returned as
/// a part for the new session.
pub async fn note_part(
    host: &Host,
    task: &TaskId,
    to: &ModelRef,
    cause: TryCause,
    why: &str,
    given: Option<FallbackNote>,
) -> EngineResult<Option<InputPart>> {
    let record = record_of(host, task)?;
    let mut note = match given {
        Some(note) => note,
        None => host
            .services()
            .fallback
            .note(&record.spec, &task_events(host, task)?),
    };
    if note.why_stopped.is_empty() {
        note.why_stopped = why.to_owned();
    }
    let progress = progress_of(host, task);
    let rules = context::applied(host, &record, &progress, to, cause);
    let told_as = context::prepare(host, task, to, &rules, &mut note).await?;
    let Some(told) = context::tell(told_as, note)? else {
        return Ok(None);
    };
    let dir = layout::task_dir(&host.repo().run_dir, task).join("notes");
    create_private_dir_all(&dir)?;
    let path = dir.join(format!("note-{}.json", record.tries + 1));
    write_private_file(&path, told.json.as_bytes())?;
    let event = FallbackEvent::Note {
        task: task.clone(),
        path: path.clone(),
        sha256: sha256_hex(told.json.as_bytes()),
    };
    host.record(Some(task), None, RunEventKind::Fallback(event))?;
    Ok(Some(part(
        InputPartKind::Fallback,
        "fallback",
        told.text,
        Some(path.display().to_string()),
    )))
}

/// The whole task: its brief, then its facts.
pub(crate) fn task_text(spec: &TaskSpec) -> String {
    let mut text = format!("{} · {}\n\n{}\n", spec.id, spec.title, spec.task);
    facts(&mut text, spec);
    text
}

/// The task without its brief, for a worker that reads the brief as its instructions.
fn task_facts(spec: &TaskSpec) -> String {
    let mut text = format!("{} · {}\n", spec.id, spec.title);
    facts(&mut text, spec);
    text
}

fn facts(text: &mut String, spec: &TaskSpec) {
    let kind = match &spec.kind {
        TaskKind::Captain => "the whole change".to_owned(),
        TaskKind::Interface => {
            "interfaces, with stubs that compile and fail \"not implemented\"".to_owned()
        }
        TaskKind::AcceptanceTests { for_task } => {
            format!("acceptance tests for {for_task}; they must fail against the stubs")
        }
        TaskKind::Build => "building".to_owned(),
        TaskKind::Chore => "merge or cleanup".to_owned(),
        TaskKind::Docs => "documentation".to_owned(),
        TaskKind::Experiment { topic, option } => format!("an experiment on {topic}: {option}"),
    };
    let _ = write!(text, "\nKind: {kind}");
    list(text, "Paths you own", &spec.owned_paths);
    list(text, "Read-only paths", &spec.read_only_paths);
    let deps: Vec<String> = spec.depends_on.iter().map(ToString::to_string).collect();
    list(text, "Already merged into the run before you", &deps);
    list(text, "Goal criteria it serves", &spec.criteria);
}

fn goal_text(goal: &GoalFile) -> String {
    let mut text = format!("{}\n", goal.summary);
    let criteria: Vec<String> = goal
        .criteria
        .iter()
        .map(|c| format!("{}: {}", c.id, c.text))
        .collect();
    list(&mut text, "Done when", &criteria);
    list(&mut text, "In scope", &goal.in_scope);
    list(&mut text, "Out of scope", &goal.out_of_scope);
    let assumptions: Vec<String> = goal.assumptions.iter().map(|a| a.text.clone()).collect();
    list(&mut text, "Assumptions", &assumptions);
    list(&mut text, "Answers", &goal.answers);
    text
}

fn decisions_text(decisions: &[DesignDecision]) -> String {
    let mut text = String::new();
    for d in decisions {
        let _ = writeln!(
            text,
            "{} · {}: {}. {}",
            d.id, d.title, d.choice, d.rationale
        );
    }
    text
}

/// `loaded` names the instruction files the worker's CLI loads by itself, which the rules leave
/// out so it doesn't read them twice.
fn rules_text(spec: &TaskSpec, settings: &Settings, tree: &Path, loaded: &[String]) -> String {
    let mut never = vec![".argonauts/**".to_owned()];
    never.extend(settings.limits.protected_paths.iter().cloned());
    never.extend(spec.forbidden_paths.iter().cloned());
    let mut text = String::from("Rules for this task:");
    if spec.owned_paths.is_empty() {
        text.push_str("\n- You may change anything in the worktree except the paths below.");
    }
    list(&mut text, "Never change", &never);
    list(&mut text, "Not checked out", &spec.hidden_paths);
    let allowlist = &settings.limits.dependency_allowlist;
    if allowlist.is_empty() {
        text.push_str("\n\nAdd no new dependencies, and don't edit lockfiles.");
    } else {
        let _ = write!(
            text,
            "\n\nNew dependencies only from this list: {}. Don't edit lockfiles.",
            allowlist.join(", ")
        );
    }
    for guide in ["AGENTS.md", "CLAUDE.md"] {
        if tree.join(guide).is_file() && !loaded.iter().any(|file| file == guide) {
            let _ = write!(text, "\n\nFollow the project's conventions in {guide}.");
        }
    }
    text
}

/// The crew's notes in the repo's state directory, then each `.argonauts/notes/*.md` in the
/// worktree, in name order. One part per topic file. The crew's sourced notes go as a quoted block
/// of reference data; a file with none, such as one you wrote, goes as it is. With `judged`, a
/// reviewer's or a judge's parts leave out every note that agent wrote.
pub fn note_parts_for(tree: &Path, state_dir: &Path, judged: Option<&ModelRef>) -> Vec<InputPart> {
    crate::run_review::notes::read_all(tree, state_dir)
        .into_iter()
        .filter_map(|file| {
            let text = if file.notes.is_empty() {
                file.text
            } else {
                let kept: Vec<_> = file
                    .notes
                    .into_iter()
                    .filter(|(_, source)| judged.is_none_or(|judged| source.worker != *judged))
                    .collect();
                if kept.is_empty() {
                    return None;
                }
                argonauts_learning::memory::quoted(&kept)
            };
            let text: String = text.chars().take(NOTE_CHARS).collect();
            Some(part(
                InputPartKind::Notes,
                "notes",
                text,
                Some(file.relative),
            ))
        })
        .collect()
}

fn note_parts(tree: &Path, state_dir: &Path) -> Vec<InputPart> {
    note_parts_for(tree, state_dir, None)
}

fn list(text: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = write!(text, "\n\n{title}:");
    for item in items {
        let _ = write!(text, "\n- {item}");
    }
}

/// Whether the worker's session runner serves the status tool.
pub(crate) fn has_status_tool(host: &Host, worker: &argonauts_core::names::ModelRef) -> bool {
    host.services()
        .workers
        .get(worker)
        .map(|w| w.capabilities().status_tool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::task_spec;

    #[test]
    fn the_task_part_lists_paths_and_dependencies() {
        let mut spec = task_spec("task-002", &["task-001"], None);
        spec.owned_paths = vec!["src/store/**".into()];
        spec.criteria = vec!["G1".into()];
        let text = task_text(&spec);
        assert!(text.starts_with("task-002 · Build task-002"));
        assert!(text.contains("Paths you own:\n- src/store/**"));
        assert!(text.contains("Already merged into the run before you:\n- task-001"));
        assert!(text.contains("Goal criteria it serves:\n- G1"));
    }

    #[test]
    fn rules_name_protected_and_forbidden_paths() {
        let mut spec = task_spec("task-001", &[], None);
        spec.forbidden_paths = vec!["migrations/**".into()];
        let mut settings = Settings::default();
        settings.limits.protected_paths = vec![".github/**".into()];
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
        let text = rules_text(&spec, &settings, dir.path(), &[]);
        for path in [".argonauts/**", ".github/**", "migrations/**"] {
            assert!(text.contains(&format!("- {path}")), "{text}");
        }
        assert!(text.contains("AGENTS.md"));
        assert!(!text.contains("CLAUDE.md"));
    }

    /// A CLI that loads the conventions itself isn't told to read them again. One that doesn't,
    /// or an isolated session that loads neither, is pointed at each.
    #[test]
    fn the_rules_name_only_instruction_files_the_cli_doesnt_load() {
        let spec = task_spec("task-001", &[], None);
        let settings = Settings::default();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
        let loads_both = ["CLAUDE.md".to_owned(), "AGENTS.md".to_owned()];
        let text = rules_text(&spec, &settings, dir.path(), &loads_both);
        assert!(!text.contains("conventions"), "{text}");
        let text = rules_text(&spec, &settings, dir.path(), &["AGENTS.md".to_owned()]);
        assert!(text.contains("conventions in CLAUDE.md"), "{text}");
        assert!(!text.contains("conventions in AGENTS.md"), "{text}");
        let text = rules_text(&spec, &settings, dir.path(), &[]);
        assert!(
            text.contains("conventions in AGENTS.md") && text.contains("conventions in CLAUDE.md")
        );
    }

    #[test]
    fn notes_are_read_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join(".argonauts/notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("b.md"), "second").unwrap();
        std::fs::write(notes.join("a.md"), "first").unwrap();
        std::fs::write(notes.join("skip.txt"), "no").unwrap();
        let state = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(state.path().join("notes")).unwrap();
        std::fs::write(state.path().join("notes/kept.md"), "the crew's").unwrap();
        let parts = note_parts(dir.path(), state.path());
        let texts: Vec<&str> = parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["the crew's", "first", "second"]);
        assert_eq!(parts[0].source.as_deref(), Some("notes/kept.md"));
        assert_eq!(parts[1].source.as_deref(), Some(".argonauts/notes/a.md"));
        assert_eq!(parts[0].kind, InputPartKind::Notes);
    }

    #[test]
    fn review_and_check_parts_carry_the_text() {
        let review = review_part(2, &["Handle the empty list".into()]);
        assert_eq!(review.kind, InputPartKind::Message);
        assert!(review.text.contains("round 2"));
        assert!(review.text.contains("- Handle the empty list"));
        let check = check_output_part("test: 1 failed");
        assert_eq!(check.kind, InputPartKind::CheckOutput);
        assert!(check.text.ends_with("test: 1 failed"));
        assert_eq!(
            check.approx_tokens as usize,
            check.text.chars().count().div_ceil(4)
        );
    }
}
