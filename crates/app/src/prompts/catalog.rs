//! Every built-in prompt in one list: the crew's, the judge's and the narrator's, and the
//! app's own. `/prompts` lists them, and `/setup` writes them out as templates.

use std::io;
use std::path::Path;

use argonauts_core::names::RoleName;
use argonauts_core::prompt_files::{self, PromptDirs, RoleFileMode, Template, Written, YourFile};
use argonauts_core::prompt_list::{CHANGED, INSTRUCTIONS, PromptList, PromptRow, WriterChoice};
use argonauts_core::prompt_parts::{BUILT_IN, Readers, finer_roles};
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::prompts::Writer;

const fn own(id: &'static str, part: &'static str, text: &'static str) -> Template {
    Template {
        id,
        role: CrewRole::Main,
        part,
        text,
    }
}

pub(crate) const ESTIMATE: Template = own(
    "conversation/estimate",
    "size up a request",
    super::ESTIMATE,
);
pub(crate) const ANSWER: Template = own(
    "conversation/answer",
    "answer in the conversation",
    super::CONVERSATION_ANSWER,
);
pub(crate) const COMPACT: Template = own(
    "conversation/compact",
    "sum up for /compact",
    super::COMPACT,
);
pub(crate) const TITLE: Template = own("conversation/title", "name the conversation", super::TITLE);
pub(crate) const REVIEW: Template = own("conversation/review", "/review a change", super::REVIEW);
pub(crate) const STANDING: Template = own(
    "conversation/standing",
    "the main session's tools",
    super::STANDING,
);
pub(crate) const CARRY_SUMMARY: Template = own(
    "conversation/carry-summary",
    "sum up for another model",
    super::CARRY_SUMMARY,
);

const OWN: [Template; 7] = [
    ESTIMATE,
    ANSWER,
    COMPACT,
    TITLE,
    REVIEW,
    STANDING,
    CARRY_SUMMARY,
];

/// Each role's frame first, then the prompts of its steps.
pub(crate) fn all() -> Vec<Template> {
    let mut templates = argonauts_crew::prompts::catalog::ALL.to_vec();
    templates.extend(argonauts_policy::templates::ALL);
    templates.extend(OWN);
    templates
}

/// The prompt that goes by `id`: one of [`all`], or a finer role's own frame such as
/// `frames/critic`.
pub(crate) fn find(id: &str) -> Option<Template> {
    let listed = all().into_iter().find(|template| template.id == id);
    listed.or_else(|| {
        let role = RoleName::parse(id.strip_prefix(prompt_files::FRAMES)?)?;
        argonauts_crew::prompts::catalog::finer_frame(role)
    })
}

/// Your file an agent reads where the built-in would be. A finer role's frame with no file of
/// its own is its crew role's, so your `frames/reviewer.md` is what a critic reads too.
pub(crate) fn file_in_use(dirs: &PromptDirs, template: &Template) -> Option<YourFile> {
    let own = dirs.template(template.id);
    if own.is_some() || !template.is_frame() {
        return own;
    }
    let crew_frame = format!("{}{}", prompt_files::FRAMES, template.role.as_str());
    dirs.template(&crew_frame)
}

/// Tells this process where your prompt files are, once, when the app opens on a repository.
/// They are looked for again each time a prompt is sent, so what you trust, write or delete
/// counts from the next prompt on.
pub(crate) fn use_files_of(env: &crate::env::AppEnv) {
    let (repo, state, home) = (
        env.repo_root.clone(),
        env.state_dir.clone(),
        env.paths.home().to_owned(),
    );
    prompt_files::use_dirs_from(move || {
        argonauts_crew::prompts::trust::dirs(&repo, &state, Some(&home))
    });
}

/// The repository's `.argonauts`, then `~/.argonauts`, with the project files that came with
/// the repository passed over until you trust them.
pub(crate) fn dirs(env: &crate::env::AppEnv) -> PromptDirs {
    argonauts_crew::prompts::trust::dirs(&env.repo_root, &env.state_dir, Some(env.paths.home()))
}

/// The template as this process sends it: yours when you wrote one.
pub(crate) fn text(template: &Template) -> String {
    prompt_files::in_use(template.id, template.text)
}

/// Writes every frame and built-in prompt into `<argonauts_dir>/prompts/` with a README, each
/// as the built-in has it. A file you edited is yours and is kept, and a copy nobody edited is
/// brought up to what the built-in says now, so `/setup` may call this every time it runs,
/// with `~/.argonauts` or the project's `.argonauts`.
pub fn write_templates(argonauts_dir: &Path) -> io::Result<Written> {
    prompt_files::write_templates(argonauts_dir, &all())
}

/// The order `/prompts` lists the roles in: who builds and who judges first, since theirs are
/// the prompts changed most, then who plans, then the main agent's own.
const LISTED: [CrewRole; 5] = [
    CrewRole::Worker,
    CrewRole::Reviewer,
    CrewRole::Judge,
    CrewRole::Orchestrator,
    CrewRole::Main,
];

/// What `/prompts` shows: for every crew role, its frame, who writes its instructions with
/// your file for the role, and each built-in prompt of its steps. A finer role that has
/// something of its own comes after the crew role it would otherwise read.
pub(crate) fn list(dirs: &PromptDirs, settings: &Settings) -> PromptList {
    let templates = all();
    let mut rows = Vec::new();
    for role in LISTED {
        let of_role = || templates.iter().filter(|template| template.role == role);
        rows.extend(
            of_role()
                .filter(|template| template.is_frame())
                .map(|frame| template_row(dirs, frame)),
        );
        rows.push(instructions_row(dirs, settings, role.into()));
        rows.extend(
            of_role()
                .filter(|template| !template.is_frame())
                .map(|template| template_row(dirs, template)),
        );
        for finer in finer_roles().filter(|finer| CrewRole::read_by(*finer) == role) {
            rows.extend(finer_rows(dirs, settings, finer));
        }
    }
    PromptList {
        rows,
        writers: writers(settings),
        ..PromptList::default()
    }
}

/// What to warn about in your own files: a role file that can't be used or that no role
/// reads, one for the reviewers or the judge that names a model or a CLI, a template that no
/// prompt goes by, and a `[prompts.<role>]` table named after no role. One line each, empty
/// when there is none.
pub(crate) fn warnings(dirs: &PromptDirs, settings: &Settings) -> Vec<String> {
    use argonauts_crew::prompts::warnings::{about_role_files, about_role_tables};
    let mut warnings = about_role_files(dirs, settings);
    warnings.extend(unread_templates(dirs));
    warnings.extend(about_role_tables(settings));
    warnings
}

/// A file under `prompts/` takes a built-in's place by having its name, so one with another
/// name does nothing, and a typo would otherwise never show. A finer role may have a frame of
/// its own, such as `frames/critic.md`.
fn unread_templates(dirs: &PromptDirs) -> Vec<String> {
    let unread = dirs.files_in(prompt_files::PROMPTS_DIR).into_iter();
    unread
        .filter(|(name, _)| find(name).is_none())
        .map(|(_, file)| {
            format!(
                "{} isn't named after a prompt, so nothing reads it. /prompts lists the names",
                file.shown
            )
        })
        .collect()
}

fn template_row(dirs: &PromptDirs, template: &Template) -> PromptRow {
    let yours = dirs
        .template(template.id)
        .filter(|file| file.text.trim() != template.text.trim());
    // A copy nobody edited is the built-in as it is. One in the project is still a file you
    // put there, which `r` deletes. The ones the setup wrote for every project are not: they
    // would mark every row, and the list is for seeing what you changed.
    let copied = yours.is_none()
        && dirs
            .template_file(template.id)
            .is_some_and(|copy| !in_home(&copy));
    let has_file = yours.is_some() || copied;
    let mark = match &yours {
        Some(file) if in_home(file) => format!("{CHANGED} in ~/.argonauts"),
        Some(_) => CHANGED.to_owned(),
        None if copied => "copied, as built in".to_owned(),
        None => String::new(),
    };
    PromptRow {
        role: template.role.as_str().to_owned(),
        part: template.part.to_owned(),
        written_by: BUILT_IN.to_owned(),
        yours: mark,
        name: Some(template.id.to_owned()),
        has_file,
        text: yours.map_or_else(|| template.text.to_owned(), |file| file.text),
    }
}

/// A finer role's frame and instructions, once it has something of its own: a frame, a file
/// under `roles/`, or a writer. Until then it reads its crew role's, and the list stays the
/// crew's five.
fn finer_rows(dirs: &PromptDirs, settings: &Settings, finer: RoleName) -> Vec<PromptRow> {
    let Some(template) = argonauts_crew::prompts::catalog::finer_frame(finer) else {
        return Vec::new();
    };
    let readers = Readers::Finer(finer);
    let frame = PromptRow {
        role: finer.as_str().to_owned(),
        text: file_in_use(dirs, &template)
            .map_or_else(|| template.text.to_owned(), |file| file.text),
        ..template_row(dirs, &template)
    };
    let own_writer = settings.prompts.roles.get(finer.as_str());
    let on_its_own = frame.has_file
        || dirs.own_role_file(readers).is_some()
        || own_writer.is_some_and(|table| table.writer.is_some());
    if !on_its_own {
        return Vec::new();
    }
    vec![frame, instructions_row(dirs, settings, readers)]
}

/// Whether the file is yours for every project, rather than this project's.
fn in_home(file: &YourFile) -> bool {
    file.shown.starts_with('~')
}

/// A role's instructions: who writes them, and your file for the role.
fn instructions_row(dirs: &PromptDirs, settings: &Settings, readers: Readers) -> PromptRow {
    let writer = match readers {
        Readers::Crew(role) => settings.prompts.writer_of(role),
        Readers::Finer(role) => settings.prompts.writer_for(role),
    };
    let how = match writer {
        Writer::Orchestrator => {
            "The orchestrator writes them for each agent and each request. Where it wrote none, \
             the agent reads the prompt for its step."
                .to_owned()
        }
        Writer::BuiltIn => "Each agent reads the prompt for its step, as it is built in or as \
                            you changed it."
            .to_owned(),
        Writer::Model(model) => format!(
            "{model} writes them before each agent starts, from what the agent would have \
             been told."
        ),
    };
    let own = dirs.own_role(readers);
    let (yours, has_file) = match &own {
        Some(Ok(file)) => (file.shown.clone(), true),
        Some(Err(why)) => (format!("not used: {why}"), true),
        None => (String::new(), false),
    };
    // A finer role with no file of its own reads its crew role's.
    let read = match own {
        Some(own) => own.ok(),
        None => dirs.own_role(readers.crew()).and_then(Result::ok),
    };
    let theirs = read.map_or_else(String::new, |file| {
        let mode = match file.mode {
            RoleFileMode::Add => "added after them",
            RoleFileMode::Replace => "in their place",
        };
        format!("\n\nYour file · {}, {mode}:\n\n{}", file.shown, file.text)
    });
    PromptRow {
        role: readers.as_str().to_owned(),
        part: INSTRUCTIONS.to_owned(),
        written_by: writer.shown(),
        yours,
        name: None,
        has_file,
        text: format!("{how}{theirs}"),
    }
}

/// Who may write a role's instructions: the orchestrator, Argonauts Code's own templates, then each
/// agent on the crew.
fn writers(settings: &Settings) -> Vec<WriterChoice> {
    let names = argonauts_core::display_names::DisplayNames::default();
    let fixed = [Writer::Orchestrator, Writer::BuiltIn]
        .into_iter()
        .map(|writer| WriterChoice {
            value: writer.to_string(),
            shown: writer.shown(),
        });
    let members = settings.crew.members();
    let mut crew: Vec<&argonauts_core::names::ModelRef> = Vec::new();
    for worker in std::iter::once(&settings.crew.main).chain(&members) {
        if !crew.contains(&worker) {
            crew.push(worker);
        }
    }
    let models = crew.into_iter().map(|worker| WriterChoice {
        value: worker.to_string(),
        shown: names.worker(worker),
    });
    fixed.chain(models).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn write(repo: &tempfile::TempDir, file: &str, text: &str) {
        let path = repo.path().join(".argonauts").join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn every_prompt_is_listed_once() {
        let templates = all();
        let ids: BTreeSet<&str> = templates.iter().map(|template| template.id).collect();
        assert_eq!(ids.len(), templates.len());
        for id in [
            "frames/reviewer",
            "review/review",
            "approvals/judge",
            "conversation/compact",
        ] {
            assert!(find(id).is_some(), "{id}");
        }
    }

    /// Nothing is locked: every file under `crates/core/templates/prompts/` is a template at
    /// the path its id names, so one added there can be written out and replaced. The README
    /// is the folder's own page, and `.argonauts/approval-rules.md` already replaces the
    /// judge's default rules.
    #[test]
    fn every_prompt_file_is_a_template_at_the_path_its_id_names() {
        let folder =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/templates/prompts");
        let templates = all();
        let mut files = Vec::new();
        collect(&folder, &mut files);
        let mut found = 0;
        for path in files {
            let name = path.strip_prefix(&folder).unwrap().to_str().unwrap();
            if name == "README.md" || name == "approvals/default-rules.md" {
                continue;
            }
            let id = name.strip_suffix(".md").unwrap();
            let template = templates
                .iter()
                .find(|template| template.id == id)
                .unwrap_or_else(|| panic!("{name} is in no catalog: add an entry for it"));
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                template.text,
                "{id}"
            );
            found += 1;
        }
        assert_eq!(found, templates.len(), "every template has its file");
    }

    fn collect(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                files.push(path);
            }
        }
    }

    /// The table: a role's frame, its instructions with who writes them and
    /// your file for the role, and a mark on what you changed.
    #[test]
    fn the_list_shows_who_writes_and_what_you_changed() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo, "roles/worker.md", "Keep functions short.\n");
        write(
            &repo,
            "prompts/frames/reviewer.md",
            "You review, strictly.\n",
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let settings: Settings =
            toml::from_str("[prompts.judge]\nwriter = \"built-in\"\n").unwrap();
        let list = list(&dirs, &settings);
        let find = |role: &str, part: &str| {
            let found = list
                .rows
                .iter()
                .find(|row| row.role == role && row.part == part);
            found
                .unwrap_or_else(|| panic!("no {part} row for {role}"))
                .clone()
        };
        let workers = find("worker", "instructions");
        assert_eq!(workers.written_by, "the orchestrator");
        assert_eq!(workers.yours, "roles/worker.md");
        assert_eq!(workers.name, None);
        assert!(
            workers
                .text
                .ends_with("added after them:\n\nKeep functions short."),
            "{}",
            workers.text
        );
        let frame = find("reviewer", "frame");
        assert_eq!(frame.written_by, "built in");
        assert_eq!(frame.yours, "✎ changed");
        assert_eq!(frame.name.as_deref(), Some("frames/reviewer"));
        assert!(frame.has_file);
        assert_eq!(frame.text, "You review, strictly.\n");
        let untouched = find("judge", "frame");
        assert!(!untouched.has_file);
        assert!(untouched.text.contains("You aren't told who wrote it."));
        assert_eq!(find("judge", "instructions").written_by, "built in");
        assert_eq!(find("orchestrator", "how it plans").written_by, "built in");
        // A role's frame comes first, then its instructions, then its steps.
        let first: Vec<&str> = list
            .rows
            .iter()
            .take(2)
            .map(|row| row.part.as_str())
            .collect();
        assert_eq!(first, ["frame", "instructions"]);
    }

    /// The list stays the crew's five until a finer role has a file, a frame or a writer of its
    /// own. Then it is listed after the crew role it would otherwise read, by its own name, so
    /// the keys act on its files.
    #[test]
    fn a_finer_role_with_something_of_its_own_is_listed_after_its_crew_role() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo, "roles/reviewer.md", "Check error handling.\n");
        write(
            &repo,
            "prompts/frames/reviewer.md",
            "You review, strictly.\n",
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let roles = |list: &PromptList| -> Vec<String> {
            let mut roles: Vec<String> = list.rows.iter().map(|row| row.role.clone()).collect();
            roles.dedup();
            roles
        };
        let the_five = ["worker", "reviewer", "judge", "orchestrator", "main"];
        assert_eq!(roles(&list(&dirs, &Settings::default())), the_five);

        write(&repo, "roles/critic.md", "Argue the opposite case.\n");
        let settings: Settings =
            toml::from_str("[prompts.draft]\nwriter = \"built-in\"\n").unwrap();
        let list = list(&dirs, &settings);
        assert_eq!(
            roles(&list),
            [
                "worker",
                "reviewer",
                "critic",
                "judge",
                "orchestrator",
                "draft",
                "main"
            ]
        );
        let of = |role: &str| -> Vec<PromptRow> {
            let rows = list.rows.iter().filter(|row| row.role == role);
            rows.cloned().collect()
        };
        let critic = of("critic");
        assert_eq!(critic.len(), 2, "{critic:?}");
        // With no frame of its own it reads the reviewers', as you changed it.
        assert_eq!(critic[0].name.as_deref(), Some("frames/critic"));
        assert_eq!(critic[0].text, "You review, strictly.\n");
        assert_eq!((critic[0].yours.as_str(), critic[0].has_file), ("", false));
        assert_eq!(critic[1].part, "instructions");
        assert_eq!(critic[1].yours, "roles/critic.md");
        assert!(critic[1].text.ends_with("Argue the opposite case."));
        // The draft role has a writer of its own and still reads the orchestrator's file, if
        // any.
        let draft = of("draft");
        assert_eq!(draft[1].written_by, "built in");
        assert!(!draft[1].has_file);

        // A frame of its own is found by name, written, and marked once edited.
        let frame = find("frames/critic").unwrap();
        assert_eq!(frame.role, CrewRole::Reviewer);
        prompt_files::write_template(&repo.path().join(".argonauts"), &frame).unwrap();
        assert_eq!(
            file_in_use(&dirs, &frame).map(|file| file.shown).as_deref(),
            Some("prompts/frames/reviewer.md")
        );
        write(&repo, "prompts/frames/critic.md", "You argue against.\n");
        assert_eq!(
            file_in_use(&dirs, &frame).map(|file| file.shown).as_deref(),
            Some("prompts/frames/critic.md")
        );
        assert_eq!(find("frames/critc"), None);
    }

    /// A template with a name no prompt has is read by nothing. A copy nobody edited and a
    /// finer role's frame are neither yours to fix nor wrong.
    #[test]
    fn a_template_no_prompt_goes_by_is_warned_about() {
        let repo = tempfile::tempdir().unwrap();
        write_templates(&repo.path().join(".argonauts")).unwrap();
        write(&repo, "prompts/frames/critic.md", "You argue against.\n");
        write(&repo, "prompts/frames/reviewer.md", "You review.\n");
        write(&repo, "prompts/review/reveiw.md", "Review the change.\n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(
            warnings(&dirs, &Settings::default()),
            [
                "prompts/review/reveiw.md isn't named after a prompt, so nothing reads it. \
                 /prompts lists the names"
            ]
        );
    }

    /// A typo in a settings file is listed beside the ones in file names.
    #[test]
    fn a_prompts_table_no_role_goes_by_is_warned_about() {
        let repo = tempfile::tempdir().unwrap();
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let settings: Settings = toml::from_str(
            "[prompts.critic]\nwriter = \"built-in\"\n\n[prompts.reviwers]\nwriter = \
             \"built-in\"\n",
        )
        .unwrap();
        let warnings = warnings(&dirs, &settings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("[prompts.reviwers] in your settings isn't named after"),
            "{warnings:?}"
        );
    }

    #[test]
    fn the_writers_are_the_orchestrator_the_templates_and_the_crew() {
        let mut settings = Settings::default();
        settings.crew = argonauts_core::settings::crew::CrewSettings::of(vec![
            "codex".parse().unwrap(),
            "claude-code/opus".parse().unwrap(),
        ]);
        settings.crew.main = "codex".parse().unwrap();
        let shown: Vec<(String, String)> = writers(&settings)
            .into_iter()
            .map(|choice| (choice.value, choice.shown))
            .collect();
        let pair = |value: &str, shown: &str| (value.to_owned(), shown.to_owned());
        assert_eq!(
            shown,
            [
                pair("orchestrator", "the orchestrator"),
                pair("built-in", "built in"),
                pair("codex", "Codex"),
                pair("claude-code/opus", "Opus on Claude Code"),
            ]
        );
    }

    #[test]
    fn setup_writes_every_template_and_keeps_yours() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo, "prompts/plan/draft.md", "My drafting prompt.\n");
        let dir = repo.path().join(".argonauts");
        let done = write_templates(&dir).unwrap();
        assert_eq!(done.kept, ["plan/draft.md"]);
        assert_eq!(done.written.len(), all().len() - 1);
        assert!(dir.join("prompts/README.md").is_file());
        assert!(dir.join("prompts/frames/judge.md").is_file());
        // Written out and untouched, nothing reads as changed except the one that was yours.
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let changed: Vec<PromptRow> = list(&dirs, &Settings::default())
            .rows
            .into_iter()
            .filter(|row| row.yours.contains("changed"))
            .collect();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].name.as_deref(), Some("plan/draft"));
    }

    /// The setup writes the templates for every project. Until you edit one, the list reads
    /// as it did before, so what you changed still stands out.
    #[test]
    fn the_copies_the_setup_wrote_leave_the_list_as_it_was() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let dirs = PromptDirs::new(Some(repo.path()), Some(home.path()));
        let before = list(&dirs, &Settings::default());
        write_templates(home.path()).unwrap();
        assert_eq!(list(&dirs, &Settings::default()), before);

        std::fs::write(
            home.path().join("prompts/frames/judge.md"),
            "You sign off.\n",
        )
        .unwrap();
        let marked: Vec<PromptRow> = list(&dirs, &Settings::default())
            .rows
            .into_iter()
            .filter(|row| !row.yours.is_empty())
            .collect();
        assert_eq!(marked.len(), 1, "{marked:?}");
        assert_eq!(marked[0].yours, "✎ changed in ~/.argonauts");
        assert!(marked[0].has_file);
    }
}
