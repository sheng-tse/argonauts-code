//! What an agent reads first, in order: the frame for its role, its instructions, your file
//! for the role, and what your request said for it. Each part is labelled by where it came
//! from, so the agent's Input tab shows them apart.

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::RoleName;
use argonauts_core::prompt_files::{PromptDirs, RoleFileMode};
use argonauts_core::prompt_parts::{self as parts, CrewRole};
use argonauts_core::settings::prompts::Writer;

use crate::engine::host::Host;
use crate::roles;

use super::instructions::{self, Agent, StepWords, Written};
use super::{catalog, files, frame, warnings, writer};

/// Everything the head of a prompt is made from.
#[derive(Debug, Clone)]
pub struct Sources<'a> {
    pub dirs: &'a PromptDirs,
    pub role: RoleName,
    pub writer: &'a Writer,
    /// The orchestrator's own words for this agent, when it wrote any.
    pub written: Option<Written>,
    /// Who made the work a reviewer or a judge is given, for a frame of yours that names them.
    pub author: Option<String>,
    /// Whether the run has other tasks than this agent's, which the workers' frame says.
    pub others_build_too: bool,
    /// What the workflow's own step says its agents do.
    pub step: Option<StepWords>,
    /// The one-line form of a role file, `roles.<role>.instructions`.
    pub setting: Option<&'a str>,
    /// What your request said for this role.
    pub request: Vec<String>,
}

/// Puts the head on a starting agent's prompt. `task` is what the step built: its built-in
/// prompt first, when it has one, then what the agent works on.
pub fn head(host: &Host, agent: &Agent<'_>, task: Vec<InputPart>) -> Vec<InputPart> {
    let dirs = files::dirs(host);
    warnings::say_once(host, &dirs, agent.role);
    assemble(sources(host, agent, &dirs), task)
}

fn sources<'a>(host: &'a Host, agent: &Agent<'_>, dirs: &'a PromptDirs) -> Sources<'a> {
    let settings = host.settings();
    Sources {
        dirs,
        role: agent.role,
        writer: settings.prompts.writer_for(agent.role),
        written: instructions::from_orchestrator(host, agent),
        author: author(host, agent),
        others_build_too: host.read(|state| state.cards.by_id.len() > 1),
        step: instructions::from_step(host, agent),
        setting: settings
            .roles
            .get(&agent.role)
            .and_then(|role| role.instructions.as_deref()),
        request: settings.prompts.request_for(agent.role),
    }
}

/// The brief of the task `agent` builds as a part, under the name of whoever wrote it, unless
/// Argonauts' own templates write for its role. A step that shows those words elsewhere in
/// its prompt asks first, so the agent reads them once.
///
/// `None` too when your file for the role takes the instructions' place: the step then keeps
/// the words where it shows them, so a worker whose instructions you replaced still reads
/// what its task is.
pub fn written(host: &Host, agent: &Agent<'_>) -> Option<InputPart> {
    if *host.settings().prompts.writer_for(agent.role) == Writer::BuiltIn {
        return None;
    }
    let yours = files::dirs(host).role(agent.role);
    if matches!(yours, Some(Ok(file)) if file.mode == RoleFileMode::Replace) {
        return None;
    }
    instructions::of_task(host, agent)
}

/// What the agent would be told with no writer model: the instructions `task` leads with
/// when someone wrote them, else the orchestrator's words, else Argonauts' own. `None` when
/// there is nothing for a writer to work from, or a writer wrote already.
pub fn draft(host: &Host, agent: &Agent<'_>, task: &[InputPart]) -> Option<String> {
    if !writer::open_to_a_writer(task) {
        return None;
    }
    let leading: Vec<&InputPart> = task
        .iter()
        .take_while(|part| part.kind.instructs())
        .collect();
    let given: Vec<&str> = leading
        .iter()
        .filter(|part| part.kind == InputPartKind::Instructions)
        .map(|part| part.text.as_str())
        .collect();
    let texts: Vec<String> = if !given.is_empty() {
        given.into_iter().map(str::to_owned).collect()
    } else if let Some(written) = instructions::from_orchestrator(host, agent) {
        vec![written.text]
    } else {
        let dirs = files::dirs(host);
        leading
            .into_iter()
            .map(|prompt| own_instructions(&dirs, prompt.clone()).text)
            .collect()
    };
    let draft = texts.join("\n\n");
    (!draft.trim().is_empty()).then_some(draft)
}

/// The frame `agent` reads, for a writer that shouldn't repeat it.
pub fn frame_text(host: &Host, agent: &Agent<'_>) -> String {
    let dirs = files::dirs(host);
    frame_part(&sources(host, agent, &dirs)).text
}

/// The head, then the rest of `task` as it was. A prompt that already has its frame is left
/// alone, and instructions someone already wrote are kept as they are.
pub fn assemble(sources: Sources<'_>, task: Vec<InputPart>) -> Vec<InputPart> {
    if task.iter().any(|part| part.kind == InputPartKind::Frame) {
        return task;
    }
    let leading = task.iter().take_while(|part| part.kind.instructs()).count();
    let mut rest = task;
    let given: Vec<InputPart> = rest.drain(..leading).collect();

    let already_written = given
        .iter()
        .any(|part| part.kind == InputPartKind::Instructions);
    let mut told = match (sources.writer, sources.written.clone()) {
        _ if already_written => given
            .into_iter()
            .filter(|part| part.kind == InputPartKind::Instructions)
            .collect(),
        // A writer model that couldn't write leaves the orchestrator's words, when it has any.
        (Writer::Orchestrator | Writer::Model(_), Some(written)) => {
            vec![from_orchestrator(written)]
        }
        _ => given
            .into_iter()
            .map(|prompt| own_instructions(sources.dirs, prompt))
            .collect(),
    };
    // A file that can't be used is left out, and the run's log says why ([`warnings`]).
    if let Some(Ok(file)) = sources.dirs.role(sources.role) {
        let yours = parts::role_file(file.text, file.shown);
        match file.mode {
            RoleFileMode::Add => told.push(yours),
            RoleFileMode::Replace => told = vec![yours],
        }
    }
    if let Some(step) = sources.step.clone() {
        told.push(parts::from_workflow(step.text, step.step));
    }
    if let Some(line) = sources.setting.filter(|line| !line.trim().is_empty()) {
        let key = format!("roles.{}.instructions", sources.role.as_str());
        told.push(parts::role_setting(line, key));
    }
    if !sources.request.is_empty() {
        told.push(parts::from_request(sources.request.join("\n")));
    }

    let mut prompt = vec![frame_part(&sources)];
    prompt.extend(told);
    prompt.extend(rest);
    prompt
}

fn from_orchestrator(written: Written) -> InputPart {
    parts::from_orchestrator(written.text, Some(roles::short_name(&written.by)))
}

/// The role's frame: your template for the role itself, then for the crew role it follows,
/// then the built-in. A frame may say who else builds with `{{others}}`, and a frame of yours
/// may name the author with `{{author}}`.
fn frame_part(sources: &Sources<'_>) -> InputPart {
    let crew = CrewRole::read_by(sources.role);
    let built_in = frame_of(crew);
    let yours = [sources.role.as_str(), crew.as_str()]
        .into_iter()
        .find_map(|name| sources.dirs.template(&format!("frames/{name}")))
        .filter(|yours| yours.text.trim() != built_in.trim());
    let (text, file) = match yours {
        Some(yours) => (yours.text, Some(yours.shown)),
        None => (built_in.to_owned(), None),
    };
    let others = if sources.others_build_too {
        frame::OTHERS_BUILD_TOO
    } else {
        frame::NOBODY_ELSE_BUILDS
    };
    let author = sources.author.as_deref().unwrap_or("someone on the crew");
    let text = text
        .replace(frame::OTHERS, others)
        .replace(frame::AUTHOR, author);
    parts::frame(text, file)
}

fn frame_of(role: CrewRole) -> &'static str {
    match role {
        CrewRole::Main => frame::MAIN,
        CrewRole::Orchestrator => frame::ORCHESTRATOR,
        CrewRole::Workers => frame::WORKERS,
        CrewRole::Reviewers => frame::REVIEWERS,
        CrewRole::Judge => frame::JUDGE,
    }
}

/// Argonauts' own instructions for a step: the built-in prompt, or your template for it.
fn own_instructions(dirs: &PromptDirs, prompt: InputPart) -> InputPart {
    let yours = catalog::of(&prompt.text)
        .and_then(|template| dirs.template(template.id))
        .filter(|yours| yours.text.trim() != prompt.text.trim());
    match yours {
        Some(yours) => parts::built_in(yours.text, Some(yours.shown)),
        None => parts::built_in(prompt.text, prompt.source),
    }
}

/// Who built the task the agent judges, as the screen names them.
fn author(host: &Host, agent: &Agent<'_>) -> Option<String> {
    let owner = host.read(|state| {
        let record = state.cards.get(agent.card?)?;
        record.owner.clone().or_else(|| record.spec.owner.clone())
    })?;
    Some(roles::display_name(&owner))
}

#[cfg(test)]
mod tests {
    use argonauts_core::prompt_parts::heading;

    use super::*;
    use crate::prompts::review::REVIEW;

    fn part(kind: InputPartKind, text: &str) -> InputPart {
        InputPart {
            kind,
            label: "prompt".to_owned(),
            text: text.to_owned(),
            approx_tokens: 1,
            source: None,
        }
    }

    fn sources<'a>(dirs: &'a PromptDirs, role: RoleName, writer: &'a Writer) -> Sources<'a> {
        Sources {
            dirs,
            role,
            writer,
            written: None,
            author: None,
            others_build_too: false,
            step: None,
            setting: None,
            request: Vec::new(),
        }
    }

    fn headings(prompt: &[InputPart]) -> Vec<String> {
        prompt.iter().filter_map(heading).collect()
    }

    fn write(dir: &tempfile::TempDir, file: &str, text: &str) {
        let path = dir.path().join(".argonauts").join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn review_task() -> Vec<InputPart> {
        vec![
            part(InputPartKind::Prompt, REVIEW),
            part(InputPartKind::StepInput, "{}"),
        ]
    }

    /// An agent reads the frame, the orchestrator's words, your file and your request, in
    /// that order, and then what it works on.
    #[test]
    fn an_agent_reads_the_four_parts_in_order() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo,
            "roles/reviewers.md",
            "Treat a missing test as a required change, never as a note.\n",
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let writer = Writer::Orchestrator;
        let prompt = assemble(
            Sources {
                written: Some(Written {
                    text: "Review the due-date change against docs/due.md.".to_owned(),
                    by: "claude-code/claude-opus-5-5".parse().unwrap(),
                }),
                request: vec!["focus on date parsing".to_owned()],
                ..sources(&dirs, RoleName::Reviewer, &writer)
            },
            review_task(),
        );
        assert_eq!(
            headings(&prompt),
            [
                "Frame · built in",
                "From the orchestrator · Opus 5.5",
                "Your file · roles/reviewers.md",
                "From your request",
            ]
        );
        assert!(prompt[0].text.contains("You aren't told who wrote it."));
        assert_eq!(prompt[3].text, "focus on date parsing");
        // The orchestrator's words are what the agent gets: the built-in prompt isn't sent too.
        assert!(prompt.iter().all(|part| part.text != REVIEW));
        assert_eq!(prompt.last().unwrap().kind, InputPartKind::StepInput);
    }

    #[test]
    fn where_the_orchestrator_wrote_nothing_argonauts_own_instructions_go() {
        let dirs = PromptDirs::default();
        let writer = Writer::Orchestrator;
        let prompt = assemble(sources(&dirs, RoleName::Reviewer, &writer), review_task());
        assert_eq!(
            headings(&prompt),
            ["Frame · built in", "Instructions · built in"]
        );
        assert_eq!(prompt[1].text, REVIEW);
        assert_eq!(prompt.len(), 3);
    }

    #[test]
    fn the_built_in_writer_keeps_argonauts_templates_whatever_the_orchestrator_wrote() {
        let dirs = PromptDirs::default();
        let writer = Writer::BuiltIn;
        let prompt = assemble(
            Sources {
                written: Some(Written {
                    text: "Review it my way.".to_owned(),
                    by: "codex".parse().unwrap(),
                }),
                ..sources(&dirs, RoleName::Reviewer, &writer)
            },
            review_task(),
        );
        assert_eq!(
            headings(&prompt),
            ["Frame · built in", "Instructions · built in"]
        );
    }

    #[test]
    fn your_templates_take_the_built_ins_place_and_are_named() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo,
            "prompts/frames/reviewers.md",
            "You review. {{author}} wrote this.\n",
        );
        write(&repo, "prompts/review/review.md", "Review it strictly.\n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let writer = Writer::Orchestrator;
        let prompt = assemble(
            Sources {
                author: Some("GPT 5.6 on Codex".to_owned()),
                ..sources(&dirs, RoleName::Reviewer, &writer)
            },
            review_task(),
        );
        assert_eq!(
            headings(&prompt),
            [
                "Frame · prompts/frames/reviewers.md",
                "Instructions · prompts/review/review.md",
            ]
        );
        assert_eq!(prompt[0].text, "You review. GPT 5.6 on Codex wrote this.\n");
        assert_eq!(prompt[1].text, "Review it strictly.\n");
    }

    #[test]
    fn a_template_you_copied_and_left_alone_still_counts_as_built_in() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo, "prompts/review/review.md", REVIEW);
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let writer = Writer::Orchestrator;
        let prompt = assemble(sources(&dirs, RoleName::Reviewer, &writer), review_task());
        assert_eq!(headings(&prompt)[1], "Instructions · built in");
    }

    #[test]
    fn a_role_file_that_says_replace_takes_the_instructions_place() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo,
            "roles/judge.md",
            "---\nmode: replace\n---\nScore on cost alone.\n",
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let writer = Writer::Orchestrator;
        let prompt = assemble(
            Sources {
                setting: Some("Be brief."),
                ..sources(&dirs, RoleName::Judge, &writer)
            },
            vec![part(InputPartKind::Prompt, crate::prompts::plan::JUDGE)],
        );
        assert_eq!(
            headings(&prompt),
            [
                "Frame · built in",
                "Your file · roles/judge.md",
                "Your settings · roles.judge.instructions",
            ]
        );
        assert_eq!(prompt[1].text, "Score on cost alone.");
    }

    /// From what stands longest to what is most particular: your file for the role, the
    /// workflow's step, then this one request.
    #[test]
    fn your_words_come_last_from_standing_to_particular() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo, "roles/reviewers.md", "Check error handling.\n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let writer = Writer::Orchestrator;
        let prompt = assemble(
            Sources {
                step: Some(StepWords {
                    text: "Review the diff for retries".to_owned(),
                    step: "review-panel, step review".to_owned(),
                }),
                request: vec!["focus on date parsing".to_owned()],
                ..sources(&dirs, RoleName::Reviewer, &writer)
            },
            review_task(),
        );
        assert_eq!(
            headings(&prompt),
            [
                "Frame · built in",
                "Instructions · built in",
                "Your file · roles/reviewers.md",
                "From the workflow · review-panel, step review",
                "From your request",
            ]
        );
    }

    #[test]
    fn the_workers_frame_says_whether_anyone_else_builds() {
        let dirs = PromptDirs::default();
        let writer = Writer::Orchestrator;
        let alone = assemble(sources(&dirs, RoleName::Worker, &writer), Vec::new());
        assert!(alone[0].text.contains(frame::NOBODY_ELSE_BUILDS));
        let crew = assemble(
            Sources {
                others_build_too: true,
                ..sources(&dirs, RoleName::Worker, &writer)
            },
            Vec::new(),
        );
        assert!(crew[0].text.contains(frame::OTHERS_BUILD_TOO));
        assert!(!crew[0].text.contains(frame::OTHERS));
    }

    #[test]
    fn instructions_already_written_are_kept_and_a_framed_prompt_is_left_alone() {
        let dirs = PromptDirs::default();
        let writer = Writer::Orchestrator;
        let task = vec![
            parts::from_writer("Review for races.", "GPT 5.6 on Codex".to_owned()),
            part(InputPartKind::StepInput, "{}"),
        ];
        let prompt = assemble(sources(&dirs, RoleName::Reviewer, &writer), task);
        assert_eq!(
            headings(&prompt),
            ["Frame · built in", "From the writer · GPT 5.6 on Codex"]
        );
        let again = assemble(sources(&dirs, RoleName::Reviewer, &writer), prompt.clone());
        assert_eq!(again, prompt);
    }
}
