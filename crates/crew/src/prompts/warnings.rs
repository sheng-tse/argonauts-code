//! What you should hear about your own role files: one that can't be used, one that no role
//! reads, and one for the reviewers or the judge that names a model or a CLI, since that tells
//! them who made the work. A run says what is wrong with the file an agent reads, once, and
//! `/prompts` and Doctor list them all, with a `[prompts.<role>]` table named after no role.

use std::sync::{Mutex, PoisonError};

use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::look::display_names::DisplayNames;
use argonauts_core::prompts::prompt_files::{self, PromptDirs, ROLES_DIR, RoleFile};
use argonauts_core::prompts::prompt_parts::Readers;
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::settings::Settings;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::{Note, NoteLevel};

use crate::engine::host::Host;

/// Every warning about your role files, one line each: the crew's five, a finer role's such
/// as `roles/critic.md`, and a file named after no role, which a typo would otherwise hide.
/// Empty when there is none.
pub fn about_role_files(dirs: &PromptDirs, settings: &Settings) -> Vec<String> {
    let names = agent_names(settings, Vec::new());
    let about_one = |(name, file): (String, prompt_files::YourFile)| {
        let Some(role) = CrewRole::of_file(&name) else {
            return Some(format!(
                "{} isn't named after a role, so no agent reads it. {THE_FIVE}",
                file.shown
            ));
        };
        about(
            prompt_files::parse_role_file(file).transpose()?,
            role,
            &names,
        )
    };
    dirs.files_in(ROLES_DIR)
        .into_iter()
        .filter_map(about_one)
        .collect()
}

const THE_FIVE: &str = "The crew's roles are main, orchestrator, worker, reviewer and judge";

/// A `[prompts.<role>]` table is found by the role's name, so one named after no role sets
/// nothing, and a typo in a settings file would otherwise never show. One line each.
pub fn about_role_tables(settings: &Settings) -> Vec<String> {
    let tables = settings.prompts.roles.keys();
    tables
        .filter(|name| Readers::of_file(name).is_none())
        .map(|name| {
            format!(
                "[prompts.{name}] in your settings isn't named after a role, so it sets \
                 nothing. {THE_FIVE}"
            )
        })
        .collect()
}

/// Says in the run's log what is wrong with the file `role`'s agents read, unless the run
/// said so already. An agent still starts: a warning never holds the work.
pub(super) fn say_once(host: &Host, dirs: &PromptDirs, role: RoleName) {
    let Some(file) = dirs.role(role) else {
        return;
    };
    // Whoever builds in this run made the work, whether or not the settings list them.
    let building = host.read(|state| {
        let tasks = state.tasks.by_id.values();
        tasks
            .filter_map(|task| task.worker.clone().or_else(|| task.spec.worker.clone()))
            .collect()
    });
    let names = agent_names(host.settings(), building);
    let Some(text) = about(file, CrewRole::read_by(role), &names) else {
        return;
    };
    // Reviewers of several tasks start side by side, and only one of them says it.
    static SAYING: Mutex<()> = Mutex::new(());
    let _saying = SAYING.lock().unwrap_or_else(PoisonError::into_inner);
    if host.read(|state| state.run.said.contains_key(&text)) {
        return;
    }
    let note = RunEventKind::Note(Note {
        level: NoteLevel::Warn,
        text,
    });
    if let Err(err) = host.record(None, None, note) {
        tracing::warn!("prompts: couldn't record a warning about a role file: {err}");
    }
}

fn about(file: Result<RoleFile, String>, role: CrewRole, names: &[String]) -> Option<String> {
    let file = match file {
        Ok(file) => file,
        Err(why) => return Some(format!("{why}, so it isn't used")),
    };
    let who = match role {
        CrewRole::Reviewer => "the reviewers",
        CrewRole::Judge => "the judge",
        _ => return None,
    };
    let named = prompt_files::names_one_of(&file.text, names)?;
    Some(format!(
        "{} names {named}, which tells {who} who made the work. They aren't told otherwise",
        file.shown
    ))
}

/// Every CLI and model on the crew, and each of `others`, by id and as the screen names it.
fn agent_names(settings: &Settings, others: Vec<ModelRef>) -> Vec<String> {
    let display = DisplayNames::default();
    let of_roles = settings.roles.values().flat_map(|role| &role.models);
    let mut names = Vec::new();
    for worker in settings
        .crew
        .members()
        .iter()
        .chain([&settings.crew.main])
        .chain(of_roles)
        .chain(&others)
    {
        names.push(worker.agent.clone());
        names.push(display.cli(&worker.agent));
        if let Some(model) = &worker.model {
            names.push(model.clone());
            names.extend(display.model(&worker.agent, Some(model)));
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(repo: &tempfile::TempDir, file: &str, text: &str) {
        let path = repo.path().join(".argonauts").join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_reviewer_file_that_names_a_cli_gets_a_warning() {
        let repo = tempfile::tempdir().unwrap();
        write(&repo, "roles/reviewer.md", "Go easy on Codex's code.\n");
        // The workers know who they are, so their file may name anyone.
        write(&repo, "roles/worker.md", "Ask Codex when stuck.\n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let warnings = about_role_files(&dirs, &Settings::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("roles/reviewer.md names Codex, which tells the reviewers"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_role_file_that_cant_be_used_says_why() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo,
            "roles/worker.md",
            "---\nmode: replce\n---\nKeep functions short.\n",
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert_eq!(
            about_role_files(&dirs, &Settings::default()),
            ["roles/worker.md: mode is `add` or `replace`, not `replce`, so it isn't used"]
        );
    }

    /// A finer role's own file is read in place of its crew role's, so it is checked the
    /// same way, and a file no role goes by is said to be read by nobody.
    #[test]
    fn a_finer_roles_file_and_a_misnamed_one_are_warned_about() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo,
            "roles/critic.md",
            "Codex wrote this, so look twice.\n",
        );
        write(&repo, "roles/reviwers.md", "Check error handling.\n");
        write(&repo, "roles/README.md", "Codex reads none of this.\n");
        let dirs = PromptDirs::new(Some(repo.path()), None);
        let warnings = about_role_files(&dirs, &Settings::default());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].starts_with("roles/critic.md names Codex, which tells the reviewers"),
            "{warnings:?}"
        );
        assert_eq!(
            warnings[1],
            "roles/reviwers.md isn't named after a role, so no agent reads it. The crew's \
             roles are main, orchestrator, worker, reviewer and judge"
        );
    }

    #[test]
    fn a_prompts_table_named_after_no_role_is_warned_about() {
        use argonauts_core::settings::prompts::{RolePrompts, Writer};

        let mut settings = Settings::default();
        assert!(about_role_tables(&settings).is_empty());
        for name in ["judge", "critic", "reviwers"] {
            let table = RolePrompts {
                writer: Some(Writer::BuiltIn),
                ..RolePrompts::default()
            };
            settings.prompts.roles.insert(name.to_owned(), table);
        }
        assert_eq!(
            about_role_tables(&settings),
            [
                "[prompts.reviwers] in your settings isn't named after a role, so it sets \
                 nothing. The crew's roles are main, orchestrator, worker, reviewer and judge"
            ]
        );
    }

    #[test]
    fn files_that_name_nobody_are_quiet() {
        let repo = tempfile::tempdir().unwrap();
        write(
            &repo,
            "roles/reviewer.md",
            "Treat a missing test as a required change.\n",
        );
        let dirs = PromptDirs::new(Some(repo.path()), None);
        assert!(about_role_files(&dirs, &Settings::default()).is_empty());
    }
}
