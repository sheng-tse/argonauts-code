//! `/templates`: lists the workflows and the prompts, marks the workflow in use and the files
//! of your own, and copies one where your changes save: into `~/.argonauts/` for every folder
//! when they save everywhere, else into this folder's `.argonauts/`. Typed alone it asks which
//! one under the list, and the copy goes there at once.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::events::approval::{AnswerAction, AnswerOption};
use argonauts_core::input::PromptsAction;
use argonauts_core::names::ChoiceId;
use argonauts_core::prompt_files::{self, PROJECT_DIR, PromptDirs, Template};
use argonauts_core::settings::SettingScope;
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::settings::saving::SaveChangesTo;
use argonauts_core::template_list::{ChangedIn, TemplateEntry, TemplateList};
use argonauts_core::{layout, workflow_files};
use argonauts_runner::Origin;

use super::{Ctx, Effect, prompt_templates, settings, workflows};
use crate::choices::{self, choose, option};
use crate::prompts::catalog;
use crate::setup::reference;

/// What a `/templates` question closes with when nothing was picked.
const NOTHING_COPIED: &str = "Nothing copied.";

/// The keys of the list's options, top down.
const KEYS: &str = "123456789abcdfghijklmnopqrsuvwxyz";

#[derive(Default)]
pub(crate) struct TemplatesState {
    /// Open `/templates` questions.
    pending: HashMap<ChoiceId, Pending>,
}

enum Pending {
    /// Which workflow or role's prompts to copy, and every folder's `~/.argonauts/` when the
    /// copy goes there.
    Which { global: bool },
}

pub(crate) fn owns_choice(state: &TemplatesState, id: &ChoiceId) -> bool {
    state.pending.contains_key(id)
}

/// `asked` is the place a flag named for the copy.
pub(crate) fn command(
    ctx: &mut Ctx,
    state: &mut TemplatesState,
    name: Option<String>,
    asked: Option<SettingScope>,
) -> anyhow::Result<Vec<Effect>> {
    let global = match asked {
        Some(SettingScope::Session) => {
            anyhow::bail!(
                "A copy is a file, so --session doesn't fit; --global or --project says where it goes."
            )
        }
        Some(scope) => scope == SettingScope::Global,
        None => settings::save_changes_to(ctx) == SaveChangesTo::Everywhere,
    };
    let Some(name) = name else {
        let list = list(ctx);
        ctx.send(AppEvent::Templates { list: list.clone() });
        ask_which(ctx, state, &list, global);
        return Ok(Vec::new());
    };
    if let Some(copied) = copy(ctx, &name, global)? {
        ctx.notice(NoteLevel::Info, copied.said);
        return Ok(copied.effects);
    }
    if catalog::find(&name).is_some() {
        return prompt_templates::command(ctx, PromptsAction::Copy { name, global });
    }
    anyhow::bail!("There's no workflow, role or prompt named {name}. /templates lists them.")
}

/// A copy made, in the words that say where it went, and what follows from it.
struct Copied {
    said: String,
    effects: Vec<Effect>,
}

/// Copies the workflow or the role's prompts named `name`. `None` when it names neither.
fn copy(ctx: &Ctx, name: &str, global: bool) -> anyhow::Result<Option<Copied>> {
    let lower = name.to_ascii_lowercase();
    if let Some(text) = workflow_text(ctx, &lower)? {
        return Ok(Some(Copied {
            said: copy_workflow(ctx, &lower, &text, global)?,
            effects: vec![Effect::CommandsChanged],
        }));
    }
    let Some(role) = role_named(&lower) else {
        return Ok(None);
    };
    Ok(Some(Copied {
        said: copy_prompts_of(ctx, role, global)?,
        effects: Vec::new(),
    }))
}

/// Asks which of the listed ones to copy: the arrows move over them and Enter picks.
fn ask_which(ctx: &mut Ctx, state: &mut TemplatesState, list: &TemplateList, global: bool) {
    let entries = list
        .workflows
        .iter()
        .map(|entry| (entry, "workflow"))
        .chain(list.prompts.iter().map(|entry| (entry, "prompts")));
    let options: Vec<AnswerOption> = entries
        .zip(KEYS.chars())
        .map(|((entry, kind), key)| {
            let mut option = option(key, &entry.name, choose(&entry.name), false);
            let marks = [
                entry.in_use.then_some("in use"),
                entry
                    .changed
                    .map(|changed| changed.mark().trim_matches(['(', ')'])),
            ];
            let detail: Vec<&str> = std::iter::once(kind)
                .chain(marks.into_iter().flatten())
                .collect();
            option.detail = Some(detail.join(" · "));
            option
        })
        .collect();
    ask(
        ctx,
        state,
        Pending::Which { global },
        "Copy which one?".to_owned(),
        options,
    );
}

fn ask(
    ctx: &mut Ctx,
    state: &mut TemplatesState,
    pending: Pending,
    title: String,
    options: Vec<AnswerOption>,
) {
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: choices::app_choice(
            id.clone(),
            ChoiceKind::Templates,
            title,
            Vec::new(),
            options,
        ),
    });
    state.pending.insert(id, pending);
}

pub(crate) fn answer_choice(
    ctx: &mut Ctx,
    state: &mut TemplatesState,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let Some(pending) = state.pending.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    match (pending, action) {
        (Pending::Which { global }, AnswerAction::Choose { value: name }) => {
            match copy(ctx, &name, global) {
                Ok(Some(copied)) => {
                    close(ctx, id, copied.said);
                    Ok(copied.effects)
                }
                Ok(None) => {
                    close(ctx, id, NOTHING_COPIED);
                    anyhow::bail!("There's no workflow or role named {name} any more.")
                }
                Err(err) => {
                    close(ctx, id, NOTHING_COPIED);
                    Err(err)
                }
            }
        }
        (_, AnswerAction::Cancel) => {
            close(ctx, id, NOTHING_COPIED);
            Ok(Vec::new())
        }
        (pending, other) => {
            state.pending.insert(id, pending);
            anyhow::bail!("{other:?} doesn't answer the /templates question")
        }
    }
}

/// Esc: nothing is copied.
pub(crate) fn set_aside(
    ctx: &mut Ctx,
    state: &mut TemplatesState,
    id: ChoiceId,
) -> anyhow::Result<Vec<Effect>> {
    if state.pending.remove(&id).is_some() {
        close(ctx, id, NOTHING_COPIED);
    }
    Ok(Vec::new())
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

/// Every workflow, the built-in ones first in the picker's order, then the prompts by the role
/// that reads them.
fn list(ctx: &Ctx) -> TemplateList {
    let in_use = ctx.settings().workflow.clone();
    let library = workflows::library(ctx);
    let changed = |origin: Origin| match origin {
        Origin::Project => Some(ChangedIn::Here),
        Origin::Yours => Some(ChangedIn::Yours),
        Origin::BuiltIn => None,
    };
    let built_in = |name: &str| workflow_files::built_in(name).is_some();
    let mut workflows: Vec<TemplateEntry> = workflow_files::BUILT_IN
        .iter()
        .map(|(name, _)| TemplateEntry {
            name: shown_name(name),
            in_use: *name == in_use,
            changed: library.find(name).and_then(|(origin, _)| changed(origin)),
        })
        .collect();
    // Then the workflows of your own, which no built-in has the name of.
    let own = library
        .list()
        .into_iter()
        .filter(|workflow| !built_in(&workflow.name));
    workflows.extend(own.map(|workflow| TemplateEntry {
        in_use: workflow.name == in_use,
        changed: changed(workflow.origin),
        name: shown_name(&workflow.name),
    }));
    let dirs = catalog::dirs(&ctx.env);
    let all = catalog::all();
    let prompts = CrewRole::ALL
        .into_iter()
        .map(|role| TemplateEntry {
            name: role.plural().to_owned(),
            in_use: false,
            changed: changed_for(ctx, &dirs, &all, role),
        })
        .collect();
    let mut folder = ctx.env.shown(&reference::dir(&ctx.env));
    folder.push('/');
    TemplateList {
        folder,
        workflows,
        prompts,
    }
}

/// Where a prompt of `role` with your own text sits: this folder's wins, as it does when the
/// prompt is read.
fn changed_for(
    ctx: &Ctx,
    dirs: &PromptDirs,
    all: &[Template],
    role: CrewRole,
) -> Option<ChangedIn> {
    let project = ctx.env.repo_root.join(PROJECT_DIR);
    let mut yours = None;
    for template in all.iter().filter(|template| template.role == role) {
        match dirs.template(template.id) {
            Some(file) if file.path.starts_with(&project) => return Some(ChangedIn::Here),
            Some(_) => yours = Some(ChangedIn::Yours),
            None => {}
        }
    }
    yours
}

/// A workflow's name as the list shows it: `maker-checker` reads `Maker-checker`.
fn shown_name(name: &str) -> String {
    let mut letters = name.chars();
    letters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(letters).collect()
    })
}

/// `.argonauts/` in this folder, or `~/.argonauts/`.
fn write_dir(ctx: &Ctx, global: bool) -> PathBuf {
    if global {
        ctx.env.paths.home().to_owned()
    } else {
        ctx.env.repo_root.join(PROJECT_DIR)
    }
}

/// The text of the workflow `name` as it runs now: your file's, or the built-in's.
fn workflow_text(ctx: &Ctx, name: &str) -> anyhow::Result<Option<String>> {
    match workflows::library(ctx).find(name) {
        Some((_, Some(path))) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", ctx.env.shown(&path)))?;
            Ok(Some(text))
        }
        _ => Ok(workflow_files::built_in(name).map(str::to_owned)),
    }
}

fn copy_workflow(ctx: &Ctx, name: &str, text: &str, global: bool) -> anyhow::Result<String> {
    let folder = write_dir(ctx, global).join(layout::user_files::WORKFLOWS);
    let path = folder.join(format!("{name}.toml"));
    let shown = ctx.env.shown(&path);
    if path.exists() {
        return Ok(format!(
            "{shown} is there already, so it was kept. /workflow edit {name} opens it."
        ));
    }
    write(&folder, &path, text).with_context(|| format!("writing {shown}"))?;
    Ok(format!(
        "Wrote {shown}. It takes the built-in {name}'s place: edit it, and the next run reads \
         your file. Delete it to go back to the built-in."
    ))
}

fn write(folder: &Path, path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(folder)?;
    std::fs::write(path, text)
}

/// `reviewer` or `reviewers`.
fn role_named(name: &str) -> Option<CrewRole> {
    CrewRole::ALL
        .into_iter()
        .find(|role| role.as_str() == name || role.plural() == name)
}

/// Writes every prompt `role` reads, leaving a file with your own text as it is.
fn copy_prompts_of(ctx: &Ctx, role: CrewRole, global: bool) -> anyhow::Result<String> {
    let dir = write_dir(ctx, global);
    let of_role: Vec<Template> = catalog::all()
        .into_iter()
        .filter(|template| template.role == role)
        .collect();
    let folder = ctx.env.shown(&dir.join(prompt_files::PROMPTS_DIR));
    let written = prompt_files::write_templates(&dir, &of_role)
        .with_context(|| format!("writing into {folder}"))?;
    let who = role.plural();
    let kept = match written.kept.len() {
        0 => String::new(),
        1 => " 1 file holds your own text and was kept.".to_owned(),
        kept => format!(" {kept} files hold your own text and were kept."),
    };
    Ok(format!(
        "The prompts the {who} read are in {folder}/: {} written now.{kept} Edit one, and the \
         next agent that starts reads your text. A file nobody edited follows the built-in.",
        written.written.len()
    ))
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::{AnswerTarget, AppChoice, UiCommand};
    use argonauts_core::input::SlashCommand;

    use super::*;
    use crate::controller::test_support::TestDriver;

    /// `/templates`, with `place` as a flag would name it.
    fn templates(name: Option<&str>, place: Option<SettingScope>) -> UiCommand {
        let command = SlashCommand::Templates {
            name: name.map(str::to_owned),
        };
        let command = match place {
            Some(scope) => SlashCommand::SavedIn {
                scope,
                change: Box::new(command),
            },
            None => command,
        };
        UiCommand::Slash { command }
    }

    async fn listed(driver: &mut TestDriver) -> TemplateList {
        driver.send(templates(None, None)).await;
        match driver
            .until(|event| matches!(event, AppEvent::Templates { .. }))
            .await
        {
            AppEvent::Templates { list } => list,
            _ => unreachable!(),
        }
    }

    async fn said(driver: &mut TestDriver, part: &str) -> String {
        match driver
            .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains(part)))
            .await
        {
            AppEvent::Notice { text, .. } => text,
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn the_list_marks_the_workflow_in_use_and_what_you_changed() {
        let mut driver = TestDriver::start().await;
        let list = listed(&mut driver).await;
        assert!(list.folder.ends_with("/templates/"), "{}", list.folder);
        let names: Vec<&str> = list.workflows.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names[..2], ["Council", "Orchestrator"]);
        assert!(names.contains(&"Maker-checker") && names.contains(&"Best-of-n"));
        let in_use: Vec<&str> = list
            .workflows
            .iter()
            .filter(|workflow| workflow.in_use)
            .map(|workflow| workflow.name.as_str())
            .collect();
        // The default workflow, which the picker lists first.
        assert_eq!(in_use, ["Council"]);
        assert!(list.workflows.iter().all(|w| w.changed.is_none()));
        let prompts: Vec<&str> = list.prompts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            prompts,
            ["main", "orchestrator", "workers", "reviewers", "judge"]
        );
        assert!(list.prompts.iter().all(|p| p.changed.is_none()));

        // A copy of a workflow in this folder takes the built-in's place.
        let here = Some(SettingScope::Project);
        driver.send(templates(Some("Maker-checker"), here)).await;
        let wrote = said(&mut driver, "maker-checker.toml").await;
        assert!(wrote.starts_with("Wrote "), "{wrote}");
        let file = driver
            .env
            .repo_root
            .join(".argonauts/workflows/maker-checker.toml");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            workflow_files::built_in("maker-checker").unwrap()
        );
        driver.send(templates(Some("maker-checker"), here)).await;
        let kept = said(&mut driver, "is there already").await;
        assert!(kept.contains("so it was kept"), "{kept}");

        // A role's prompts go in together, where your changes go: every folder. One you edit
        // marks the role.
        driver.send(templates(Some("reviewers"), None)).await;
        let wrote = said(&mut driver, "The prompts the reviewers read").await;
        assert!(!wrote.contains("0 written now"), "{wrote}");
        let frame = driver.env.paths.home().join("prompts/frames/reviewer.md");
        assert!(frame.is_file());
        std::fs::write(&frame, "Review as I would.").unwrap();

        let list = listed(&mut driver).await;
        let changed = |entries: &[TemplateEntry], name: &str| {
            entries
                .iter()
                .find(|entry| entry.name == name)
                .and_then(|entry| entry.changed)
        };
        assert_eq!(
            changed(&list.workflows, "Maker-checker"),
            Some(ChangedIn::Here)
        );
        assert_eq!(changed(&list.prompts, "reviewers"), Some(ChangedIn::Yours));
        assert_eq!(changed(&list.prompts, "workers"), None);
        driver.exit().await;
    }

    async fn question(driver: &mut TestDriver) -> AppChoice {
        let asked = driver
            .until(|event| {
                matches!(event, AppEvent::Choice { choice } if choice.kind == ChoiceKind::Templates)
            })
            .await;
        match asked {
            AppEvent::Choice { choice } => choice,
            _ => unreachable!(),
        }
    }

    /// Answers with `action`, and returns the line the question closed with.
    async fn answer(driver: &mut TestDriver, choice: &AppChoice, action: AnswerAction) -> String {
        driver
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice {
                    id: choice.id.clone(),
                },
                action,
                text: None,
                evidence_sha256: None,
                tasks: Vec::new(),
                remember: false,
            })
            .await;
        let closed = driver
            .until(|event| matches!(event, AppEvent::ChoiceClosed { id, .. } if *id == choice.id))
            .await;
        match closed {
            AppEvent::ChoiceClosed { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }

    async fn pick(driver: &mut TestDriver, choice: &AppChoice, label: &str) -> String {
        let option = choice
            .options
            .iter()
            .find(|option| option.label == label)
            .unwrap_or_else(|| panic!("no option {label} in {:?}", choice.options));
        answer(driver, choice, option.action.clone()).await
    }

    /// Typed alone, the command asks which of the listed ones to copy, then where, and the
    /// copy lands there.
    #[tokio::test]
    async fn typed_alone_it_asks_which_one_then_copies_it_where_your_changes_go() {
        let mut driver = TestDriver::start().await;
        let list = listed(&mut driver).await;
        let which = question(&mut driver).await;
        assert_eq!(which.title, "Copy which one?");
        let offered: Vec<&str> = which.options.iter().map(|o| o.label.as_str()).collect();
        let in_the_list: Vec<&str> = list
            .workflows
            .iter()
            .chain(&list.prompts)
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(offered, in_the_list);
        assert_eq!(
            which.options[0].detail.as_deref(),
            Some("workflow · in use")
        );
        assert_eq!(
            which.options.last().unwrap().detail.as_deref(),
            Some("prompts")
        );
        let mut keys: Vec<char> = which.options.iter().map(|o| o.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), which.options.len(), "one key each");

        // With --project, the pick goes into this folder, and nothing asks where.
        driver
            .send(templates(None, Some(SettingScope::Project)))
            .await;
        let which = question(&mut driver).await;
        let wrote = pick(&mut driver, &which, "Maker-checker").await;
        assert!(wrote.starts_with("Wrote "), "{wrote}");
        let file = driver
            .env
            .repo_root
            .join(".argonauts/workflows/maker-checker.toml");
        assert!(file.is_file());

        // A role's prompts, where your changes go: every folder.
        driver.send(templates(None, None)).await;
        let which = question(&mut driver).await;
        assert_eq!(
            which
                .options
                .iter()
                .find(|o| o.label == "Maker-checker")
                .and_then(|o| o.detail.as_deref()),
            Some("workflow · changed here")
        );
        let wrote = pick(&mut driver, &which, "reviewers").await;
        assert!(
            wrote.starts_with("The prompts the reviewers read"),
            "{wrote}"
        );
        assert!(
            driver
                .env
                .paths
                .home()
                .join("prompts/frames/reviewer.md")
                .is_file()
        );

        // Leaving the question copies nothing.
        driver.send(templates(None, None)).await;
        let which = question(&mut driver).await;
        assert_eq!(
            answer(&mut driver, &which, AnswerAction::Cancel).await,
            NOTHING_COPIED
        );
        pick_nothing_new(&driver);
        let session = templates(Some("captain"), Some(SettingScope::Session));
        driver.send(session).await;
        let refused = said(&mut driver, "--session").await;
        assert!(refused.contains("A copy is a file"), "{refused}");
        driver.exit().await;
    }

    /// Only the one workflow copied so far is in this folder.
    fn pick_nothing_new(driver: &TestDriver) {
        let copied: Vec<String> =
            std::fs::read_dir(driver.env.repo_root.join(".argonauts/workflows"))
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
        assert_eq!(copied, ["maker-checker.toml"]);
    }

    #[tokio::test]
    async fn a_prompt_goes_by_its_id_and_an_unknown_name_says_so() {
        let mut driver = TestDriver::start().await;
        driver
            .send(templates(
                Some("frames/judge"),
                Some(SettingScope::ProjectLocal),
            ))
            .await;
        let wrote = said(&mut driver, "frames/judge.md").await;
        assert!(wrote.starts_with("Wrote "), "{wrote}");
        assert!(
            driver
                .env
                .repo_root
                .join(".argonauts/prompts/frames/judge.md")
                .is_file()
        );
        driver.send(templates(Some("nope"), None)).await;
        let missing = said(&mut driver, "nope").await;
        assert!(
            missing.contains("There's no workflow, role or prompt named nope"),
            "{missing}"
        );
        driver.exit().await;
    }
}
