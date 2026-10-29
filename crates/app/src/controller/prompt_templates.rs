//! `/prompts`: who writes what each agent reads, and the templates behind every prompt.
//!
//! A template in the repo's `.argonauts/prompts/` wins over yours in `~/.argonauts/prompts/`,
//! which wins over the built-in of the same name. Deleting a file goes back.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::NoteLevel;
use argonauts_core::TaskId;
use argonauts_core::api::DiffView;
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::input::{PromptsAction, SlashCommand};
use argonauts_core::prompt_files::{self, Copied, PROJECT_DIR, PromptDirs, Template};
use argonauts_core::prompt_list::PromptList;
use argonauts_core::prompt_parts::Readers;
use argonauts_core::settings::prompts::Writer;

use super::files::file_diff;
use super::{Ctx, Effect};
use crate::prompts::catalog;

pub(crate) fn command(ctx: &Ctx, action: PromptsAction) -> anyhow::Result<Vec<Effect>> {
    let dirs = dirs(ctx);
    let said = match action {
        PromptsAction::List => {
            open(ctx, &dirs);
            return Ok(Vec::new());
        }
        PromptsAction::Show { name } => {
            show(ctx, &dirs, &template(&name)?);
            return Ok(Vec::new());
        }
        PromptsAction::Copy { name, global } => copy(ctx, &template(&name)?, global)?,
        PromptsAction::CopyAll { global } => copy_all(ctx, global)?,
        PromptsAction::Role { role, global } => role_file(ctx, &role, global)?,
        PromptsAction::Diff { name } => match diff(ctx, &dirs, &template(&name)?)? {
            Some(said) => said,
            None => return Ok(Vec::new()),
        },
        PromptsAction::Reset { name } => reset(&dirs, &template(&name)?)?,
        PromptsAction::ResetRole { role } => reset_role(&dirs, &role)?,
        PromptsAction::Trust => {
            super::prompt_trust::trust_all(ctx)?;
            refresh(ctx, None);
            return Ok(Vec::new());
        }
        // A setting, which the settings part saves: see `writer_setting`.
        PromptsAction::Writer { .. } => return Ok(Vec::new()),
    };
    ctx.notice(said.level, said.text.clone());
    refresh(ctx, Some(said.text));
    Ok(Vec::new())
}

/// What a command did, for the conversation and for the footer of the list when it is open.
struct Said {
    level: NoteLevel,
    text: String,
}

fn info(text: String) -> Said {
    Said {
        level: NoteLevel::Info,
        text,
    }
}

/// `/prompts writer [role] <who>` as the `/config` change it makes, so it is checked, saved
/// and shown like any other setting.
pub(crate) fn writer_setting(role: Option<&str>, who: &str) -> anyhow::Result<SlashCommand> {
    let writer = Writer::try_from(who.to_owned()).map_err(anyhow::Error::msg)?;
    let key = match role {
        None => "prompts.writer".to_owned(),
        Some(role) if known_role(role) => format!("prompts.{role}.writer"),
        Some(role) => bail!("{}", no_such_role(role)),
    };
    Ok(SlashCommand::Config {
        key: Some(key),
        value: Some(toml::Value::String(writer.to_string()).to_string()),
        reset: false,
    })
}

fn known_role(name: &str) -> bool {
    Readers::of_file(name).is_some()
}

fn readers(name: &str) -> anyhow::Result<Readers> {
    Readers::of_file(name).with_context(|| no_such_role(name))
}

fn no_such_role(name: &str) -> String {
    format!(
        "{name:?} isn't a role. The crew's are main, orchestrator, worker, reviewer and \
         judge, and a finer role such as critic goes by its own name"
    )
}

fn dirs(ctx: &Ctx) -> PromptDirs {
    catalog::dirs(&ctx.env)
}

fn template(name: &str) -> anyhow::Result<Template> {
    catalog::find(name)
        .with_context(|| format!("There's no prompt named {name}. /prompts lists them all"))
}

/// The list as it is now, with what you should know about your own files.
fn listed(ctx: &Ctx, dirs: &PromptDirs) -> PromptList {
    PromptList {
        notes: notes(ctx, dirs),
        ..catalog::list(dirs, ctx.settings())
    }
}

fn open(ctx: &Ctx, dirs: &PromptDirs) {
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Prompts {
            view: Box::new(listed(ctx, dirs)),
        }),
    });
}

/// The list again for the one already open, after a file of yours came or went, with what was
/// done for its footer. Nothing opens for it once that list has closed.
fn refresh(ctx: &Ctx, said: Option<String>) {
    let view = PromptList {
        said,
        refresh: true,
        ..listed(ctx, &dirs(ctx))
    };
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Prompts {
            view: Box::new(view),
        }),
    });
}

/// What you should know about your own files: a role file that can't be used or that tells a
/// reviewer who made the work, and the files that wait for your trust.
fn notes(ctx: &Ctx, dirs: &PromptDirs) -> Vec<String> {
    let mut notes: Vec<String> = catalog::warnings(dirs, ctx.settings())
        .into_iter()
        .map(|warning| format!("{warning}."))
        .collect();
    if !dirs.waiting.is_empty() {
        let names: Vec<String> = dirs
            .waiting
            .iter()
            .map(|file| ctx.env.shown(file))
            .collect();
        notes.push(format!(
            "Not used until you trust them, since they came with this repository: {}. \
             /prompts trust uses them.",
            names.join(", ")
        ));
    }
    notes
}

/// The same for Doctor, which lists what is wrong and nothing when all is well.
pub(crate) fn doctor_lines(ctx: &Ctx) -> Vec<String> {
    notes(ctx, &dirs(ctx))
}

fn show(ctx: &Ctx, dirs: &PromptDirs, template: &Template) {
    let (from, text) = match catalog::file_in_use(dirs, template) {
        Some(yours) => (format!("from {}", yours.shown), yours.text),
        None => ("built in".to_owned(), template.text.to_owned()),
    };
    ctx.notice(
        NoteLevel::Info,
        format!("The {} prompt, {from}:\n\n{}", template.id, text.trim_end()),
    );
}

/// The folder a copy goes to: the project's `.argonauts`, or yours for every project.
fn write_dir(ctx: &Ctx, global: bool) -> PathBuf {
    if global {
        ctx.env.paths.home().to_owned()
    } else {
        ctx.env.repo_root.join(PROJECT_DIR)
    }
}

fn file_of(dir: &Path, template: &Template) -> PathBuf {
    dir.join(prompt_files::PROMPTS_DIR)
        .join(format!("{}.md", template.id))
}

fn copy(ctx: &Ctx, template: &Template, global: bool) -> anyhow::Result<Said> {
    let dir = write_dir(ctx, global);
    let path = file_of(&dir, template);
    let shown = ctx.env.shown(&path);
    let copied =
        prompt_files::write_template(&dir, template).with_context(|| format!("writing {shown}"))?;
    Ok(match copied {
        Copied::Fresh => info(format!(
            "Wrote {shown}. Edit it, and the next agent that starts reads your text. Delete it \
             to go back to the built-in."
        )),
        Copied::AsBuiltIn => info(format!(
            "{shown} is there already, as the built-in has it. Edit it, and the next agent \
             that starts reads your text."
        )),
        Copied::Yours => Said {
            level: NoteLevel::Warn,
            text: format!(
                "{shown} is there already with your own text, so it was kept. To see what you \
                 changed, run /prompts diff {}. To start over, run /prompts reset {}.",
                template.id, template.id
            ),
        },
    })
}

/// What the setup leaves for the prompts: every template and their README in
/// `~/.argonauts/prompts/`, there to read and to edit. The repository gets none, since files
/// nobody asked for would show up in its `git status`: `/prompts copy` puts one there.
/// A copy nobody edits changes nothing, so running the setup again is safe. A failure is a
/// notice, and the conversation opens anyway.
pub(crate) fn write_for_setup(ctx: &Ctx) {
    let home = ctx.env.paths.home();
    if let Err(err) = catalog::write_templates(home) {
        let dir = ctx.env.display(&home.join(prompt_files::PROMPTS_DIR));
        ctx.notice(
            NoteLevel::Warn,
            format!("Couldn't write the prompt templates into {dir}: {err}"),
        );
    }
}

fn copy_all(ctx: &Ctx, global: bool) -> anyhow::Result<Said> {
    let dir = write_dir(ctx, global);
    let done = catalog::write_templates(&dir)
        .with_context(|| format!("writing the templates into {}", ctx.env.shown(&dir)))?;
    let dir = ctx.env.shown(&done.dir);
    let wrote = match done.written.len() {
        0 => format!("Every template is in {dir} already."),
        1 => format!("Wrote 1 template into {dir}, beside its README."),
        n => format!("Wrote {n} templates into {dir}, beside their README."),
    };
    let kept = match done.kept.len() {
        0 => String::new(),
        1 => " One file holds your own text and was kept.".to_owned(),
        n => format!(" {n} files hold your own text and were kept."),
    };
    Ok(info(format!(
        "{wrote}{kept} Each follows the built-in until you edit it."
    )))
}

/// `/prompts role <role>`: a starter `roles/<role>.md`, which changes nothing until you write
/// your instructions in it. A finer role such as `critic` gets a file of its own.
fn role_file(ctx: &Ctx, role: &str, global: bool) -> anyhow::Result<Said> {
    let readers = readers(role)?;
    let dir = write_dir(ctx, global);
    let path = dir
        .join(prompt_files::ROLES_DIR)
        .join(format!("{}.md", readers.as_str()));
    let shown = ctx.env.shown(&path);
    let written =
        prompt_files::write_role_file(&dir, readers).with_context(|| format!("writing {shown}"))?;
    let who = readers.the_agents();
    // A finer role's file stands in for its crew role's, which is easy to miss.
    let instead = match readers {
        Readers::Crew(_) => String::new(),
        Readers::Finer(_) => format!(
            ", in place of {}/{}.md",
            prompt_files::ROLES_DIR,
            readers.crew().as_str()
        ),
    };
    Ok(info(if written {
        format!(
            "Wrote {shown}. Write your instructions for {who} in it. Agents that start after \
             that read them{instead}."
        )
    } else {
        format!("{shown} is there already: it holds your instructions for {who}.")
    }))
}

/// Opens the comparison of your file with the built-in, or says why there is none to open.
fn diff(ctx: &Ctx, dirs: &PromptDirs, template: &Template) -> anyhow::Result<Option<Said>> {
    let name = template.id;
    let Some(yours) = dirs.template(name) else {
        return Ok(Some(info(match dirs.template_file(name) {
            Some(copy) => format!(
                "{} is a copy nobody edited, so the built-in {name} prompt is in use.",
                copy.shown
            ),
            None => format!(
                "You have no file for {name}, so the built-in is in use. To edit it, run \
                 /prompts copy {name}."
            ),
        })));
    };
    if yours.text == template.text {
        return Ok(Some(info(format!(
            "{} matches the built-in {name} prompt.",
            yours.shown
        ))));
    }
    let title = name.replace('/', "-");
    let task = TaskId::new(&title).with_context(|| format!("{title:?} can't title a diff"))?;
    let diff = file_diff(
        template.text,
        &yours.text,
        &format!("built-in {name}"),
        &yours.shown,
    );
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Diff {
            diff: DiffView {
                task,
                stat: diff.stat,
                text: diff.text,
                path: yours.path,
            },
        }),
    });
    Ok(None)
}

/// Deletes your file for the template: the project's when there is one, else yours for every
/// project, so one reset undoes the file that's in use. A copy nobody edited goes the same way.
fn reset(dirs: &PromptDirs, template: &Template) -> anyhow::Result<Said> {
    let name = template.id;
    let Some(yours) = dirs.template_file(name) else {
        return Ok(info(format!(
            "You have no file for {name}. The built-in is in use."
        )));
    };
    std::fs::remove_file(&yours.path).with_context(|| format!("deleting {}", yours.shown))?;
    let now = match catalog::file_in_use(dirs, template) {
        Some(next) => format!("{} is in use now", next.shown),
        None => "The built-in is in use again".to_owned(),
    };
    Ok(info(format!("Deleted {}. {now}.", yours.shown)))
}

/// Deletes your file for a role the same way, so the role's agents read their frame and
/// their instructions with nothing of yours after them. A finer role goes back to reading
/// its crew role's file.
fn reset_role(dirs: &PromptDirs, role: &str) -> anyhow::Result<Said> {
    let readers = readers(role)?;
    let who = readers.the_agents();
    let Some(yours) = dirs.own_role_file(readers) else {
        return Ok(info(match readers {
            Readers::Crew(_) => format!("No file of yours speaks to {who}."),
            Readers::Finer(_) => format!("No file of yours speaks to {who} alone."),
        }));
    };
    std::fs::remove_file(&yours.path).with_context(|| format!("deleting {}", yours.shown))?;
    let next = dirs
        .own_role_file(readers)
        .or_else(|| dirs.own_role_file(readers.crew()));
    let now = match next {
        Some(next) => format!("{} is in use now", next.shown),
        None => format!("No file of yours speaks to {who} now"),
    };
    Ok(info(format!("Deleted {}. {now}.", yours.shown)))
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::UiCommand;

    use super::super::test_support::TestDriver;
    use super::*;

    /// What the command said or opened. The list it refreshes afterwards is for a list
    /// already open, so it is passed over here.
    async fn prompts(driver: &mut TestDriver, action: PromptsAction) -> AppEvent {
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Prompts { action },
            })
            .await;
        driver
            .until(|event| match event {
                AppEvent::Notice { .. } => true,
                AppEvent::View { data } => !matches!(
                    data.as_ref(),
                    ViewData::Prompts { view } if view.refresh
                ),
                _ => false,
            })
            .await
    }

    fn text(event: &AppEvent) -> &str {
        match event {
            AppEvent::Notice { text, .. } => text,
            other => panic!("not a notice: {other:?}"),
        }
    }

    fn list(event: AppEvent) -> PromptList {
        match event {
            AppEvent::View { data } => match *data {
                ViewData::Prompts { view } => *view,
                other => panic!("not the prompts: {other:?}"),
            },
            other => panic!("not a view: {other:?}"),
        }
    }

    fn named(name: &str) -> String {
        name.to_owned()
    }

    #[tokio::test]
    async fn the_list_shows_roles_writers_and_what_each_key_acts_on() {
        let mut driver = TestDriver::start().await;
        let list = list(prompts(&mut driver, PromptsAction::List).await);
        assert!(!list.refresh);
        let table = list.table();
        assert!(table.contains("written by"), "{table}");
        assert!(table.contains("the orchestrator"), "{table}");
        assert!(table.contains("frames/reviewer"), "{table}");
        assert_eq!(list.writers[0].shown, "the orchestrator");
        assert_eq!(list.writers[1].value, "built-in");
        assert!(list.notes.is_empty(), "{:?}", list.notes);
    }

    /// A copy or a reset sends the list again for the one already open, with what was done
    /// for its footer.
    #[tokio::test]
    async fn the_open_list_hears_what_a_key_did() {
        let mut driver = TestDriver::start().await;
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Prompts {
                    action: PromptsAction::Copy {
                        name: named("frames/judge"),
                        global: false,
                    },
                },
            })
            .await;
        let refreshed = driver
            .until(|event| matches!(event, AppEvent::View { .. }))
            .await;
        let list = list(refreshed);
        assert!(list.refresh);
        let said = list.said.unwrap_or_default();
        assert!(
            said.starts_with("Wrote .argonauts/prompts/frames/judge.md."),
            "{said}"
        );
        let judge = list
            .rows
            .iter()
            .find(|row| row.name.as_deref() == Some("frames/judge"))
            .unwrap();
        assert_eq!(judge.yours, "copied, as built in");
        assert!(judge.has_file);
    }

    /// `/prompts role reviewers` writes a starter that changes nothing until you write in it.
    #[tokio::test]
    async fn a_starter_role_file_is_written_once() {
        let mut driver = TestDriver::start().await;
        let role = |global| PromptsAction::Role {
            role: named("reviewer"),
            global,
        };
        let file = driver.env.repo_root.join(".argonauts/roles/reviewer.md");
        let wrote = prompts(&mut driver, role(false)).await;
        assert!(
            text(&wrote).starts_with("Wrote .argonauts/roles/reviewer.md. Write your"),
            "{}",
            text(&wrote)
        );
        let starter = std::fs::read_to_string(&file).unwrap();
        assert!(starter.starts_with("---\n# Your own instructions for the reviewers."));
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        let row = |list: &PromptList| {
            let found = list
                .rows
                .iter()
                .find(|row| row.role == "reviewer" && row.name.is_none());
            found.unwrap().clone()
        };
        assert_eq!(row(&listed).yours, "", "an empty starter is no role file");

        std::fs::write(&file, format!("{starter}Check error handling.\n")).unwrap();
        let again = prompts(&mut driver, role(false)).await;
        assert!(
            text(&again).contains("is there already"),
            "{}",
            text(&again)
        );
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert_eq!(row(&listed).yours, "roles/reviewer.md");

        // Deleting it takes your words back, and a second time there is nothing to delete.
        let reset = || PromptsAction::ResetRole {
            role: named("reviewer"),
        };
        let deleted = prompts(&mut driver, reset()).await;
        assert_eq!(
            text(&deleted),
            "Deleted roles/reviewer.md. No file of yours speaks to the reviewers now."
        );
        assert!(!file.exists());
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert_eq!(row(&listed).yours, "");
        let nothing = prompts(&mut driver, reset()).await;
        assert_eq!(text(&nothing), "No file of yours speaks to the reviewers.");

        let unknown = PromptsAction::Role {
            role: named("captain"),
            global: false,
        };
        let refused = prompts(&mut driver, unknown).await;
        assert!(
            text(&refused).starts_with("\"captain\" isn't a role. The crew's are main,"),
            "{}",
            text(&refused)
        );
    }

    /// A finer role is made, listed and deleted by its own name, and the words say that its
    /// file stands in for its crew role's.
    #[tokio::test]
    async fn a_finer_role_gets_a_file_and_a_frame_of_its_own() {
        let mut driver = TestDriver::start().await;
        let dir = driver.env.repo_root.join(".argonauts");
        std::fs::create_dir_all(dir.join("roles")).unwrap();
        std::fs::write(dir.join("roles/reviewer.md"), "Check error handling.\n").unwrap();
        let critic = |list: &PromptList| -> Vec<String> {
            let rows = list.rows.iter().filter(|row| row.role == "critic");
            rows.map(|row| format!("{} · {}", row.part, row.yours))
                .collect()
        };
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert!(critic(&listed).is_empty(), "{:?}", critic(&listed));

        let role = PromptsAction::Role {
            role: named("critic"),
            global: false,
        };
        let wrote = prompts(&mut driver, role).await;
        assert_eq!(
            text(&wrote),
            "Wrote .argonauts/roles/critic.md. Write your instructions for the critic role in \
             it. Agents that start after that read them, in place of roles/reviewer.md."
        );
        let file = dir.join("roles/critic.md");
        let starter = std::fs::read_to_string(&file).unwrap();
        assert!(
            starter.contains("the critic role reads it in place of roles/reviewer.md"),
            "{starter}"
        );
        std::fs::write(&file, format!("{starter}Argue the opposite case.\n")).unwrap();
        let copy = PromptsAction::Copy {
            name: named("frames/critic"),
            global: false,
        };
        let copied = prompts(&mut driver, copy).await;
        assert!(
            text(&copied).starts_with("Wrote .argonauts/prompts/frames/critic.md."),
            "{}",
            text(&copied)
        );
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert_eq!(
            critic(&listed),
            [
                "frame · copied, as built in",
                "instructions · roles/critic.md"
            ]
        );
        assert!(listed.notes.is_empty(), "{:?}", listed.notes);

        let reset = || PromptsAction::ResetRole {
            role: named("critic"),
        };
        let deleted = prompts(&mut driver, reset()).await;
        assert_eq!(
            text(&deleted),
            "Deleted roles/critic.md. roles/reviewer.md is in use now."
        );
        let nothing = prompts(&mut driver, reset()).await;
        assert_eq!(
            text(&nothing),
            "No file of yours speaks to the critic role alone."
        );
        let frame = PromptsAction::Reset {
            name: named("frames/critic"),
        };
        let deleted = prompts(&mut driver, frame).await;
        assert_eq!(
            text(&deleted),
            "Deleted prompts/frames/critic.md. The built-in is in use again."
        );
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert!(critic(&listed).is_empty(), "{:?}", critic(&listed));
    }

    #[tokio::test]
    async fn copy_edit_show_and_reset_go_round() {
        let mut driver = TestDriver::start().await;
        let name = "frames/reviewer";
        let file = driver
            .env
            .repo_root
            .join(".argonauts/prompts/frames/reviewer.md");

        let copied = prompts(
            &mut driver,
            PromptsAction::Copy {
                name: named(name),
                global: false,
            },
        )
        .await;
        assert!(text(&copied).starts_with("Wrote "), "{}", text(&copied));
        let built_in = std::fs::read_to_string(&file).unwrap();
        assert!(built_in.contains("You aren't told who wrote it."));

        // Until you edit it, the file is a copy: the built-in is still what's in use.
        let copy = PromptsAction::Copy {
            name: named(name),
            global: false,
        };
        let again = prompts(&mut driver, copy).await;
        assert!(
            text(&again).contains("as the built-in has it"),
            "{}",
            text(&again)
        );
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert!(
            listed.table().contains("copied, as built in"),
            "{}",
            listed.table()
        );
        let diffed = prompts(&mut driver, PromptsAction::Diff { name: named(name) }).await;
        assert!(
            text(&diffed).contains("is a copy nobody edited"),
            "{}",
            text(&diffed)
        );

        // Your edit is what's in use, and a second copy never overwrites it.
        std::fs::write(&file, "You review. {{author}} wrote this.\n").unwrap();
        let shown = prompts(&mut driver, PromptsAction::Show { name: named(name) }).await;
        assert!(
            text(&shown).contains("{{author}} wrote this."),
            "{}",
            text(&shown)
        );
        assert!(text(&shown).contains("from prompts/frames/reviewer.md"));
        let again = prompts(
            &mut driver,
            PromptsAction::Copy {
                name: named(name),
                global: false,
            },
        )
        .await;
        assert!(text(&again).contains("was kept"), "{}", text(&again));
        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert!(listed.table().contains("✎ changed"), "{}", listed.table());
        let diffed = prompts(&mut driver, PromptsAction::Diff { name: named(name) }).await;
        assert!(matches!(diffed, AppEvent::View { .. }), "{diffed:?}");

        // Deleting it goes back.
        let reset = prompts(&mut driver, PromptsAction::Reset { name: named(name) }).await;
        assert!(
            text(&reset).contains("The built-in is in use again"),
            "{}",
            text(&reset)
        );
        assert!(!file.exists());
        let shown = prompts(&mut driver, PromptsAction::Show { name: named(name) }).await;
        assert!(text(&shown).contains("built in"), "{}", text(&shown));
    }

    /// A reviewers file that names a CLI on the crew tells the reviewers who made the work.
    /// `/prompts` says so under its table, and so does Doctor.
    #[tokio::test]
    async fn a_reviewers_file_that_names_a_cli_is_warned_about() {
        use super::super::test_support::CliOptions;

        // Nothing real runs for Doctor: every CLI points nowhere.
        let ids = [
            "claude-code",
            "codex",
            "antigravity-cli",
            "opencode",
            "opencode-acp",
        ];
        let flags = ids
            .iter()
            .map(|id| format!(r#"agents.{id}.binary="/nowhere/{id}""#))
            .collect();
        let mut driver = TestDriver::start_with(CliOptions {
            flags,
            ..CliOptions::default()
        })
        .await;
        let file = driver.env.repo_root.join(".argonauts/roles/reviewer.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "Go easy on Codex's code.\n").unwrap();
        let warning = "roles/reviewer.md names Codex, which tells the reviewers who made the \
                       work. They aren't told otherwise.";

        let listed = list(prompts(&mut driver, PromptsAction::List).await);
        assert_eq!(listed.notes, [warning]);

        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Doctor,
            })
            .await;
        let report = driver
            .until(|event| matches!(event, AppEvent::Report { .. }))
            .await;
        let AppEvent::Report { lines, .. } = report else {
            unreachable!()
        };
        assert_eq!(lines.last().map(String::as_str), Some(warning), "{lines:?}");
        driver.exit().await;
    }

    #[tokio::test]
    async fn copy_all_writes_the_templates_and_the_readme() {
        let mut driver = TestDriver::start().await;
        let done = prompts(&mut driver, PromptsAction::CopyAll { global: false }).await;
        assert!(
            text(&done).contains("templates into .argonauts/prompts, beside their README"),
            "{}",
            text(&done)
        );
        let dir = driver.env.repo_root.join(".argonauts/prompts");
        assert!(dir.join("README.md").is_file());
        assert!(dir.join("review/review.md").is_file());
    }

    /// `/prompts writer judge built-in` saves at once where your changes go, like any setting.
    #[tokio::test]
    async fn picking_a_writer_saves_it_at_once() {
        let mut driver = TestDriver::start().await;
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::Prompts {
                    action: PromptsAction::Writer {
                        role: Some("judge".to_owned()),
                        who: "built-in".to_owned(),
                    },
                },
            })
            .await;
        let saved = driver
            .until(|event| {
                matches!(
                    event,
                    AppEvent::SettingChanged { .. }
                        | AppEvent::Choice { .. }
                        | AppEvent::Notice { .. }
                )
            })
            .await;
        let AppEvent::SettingChanged { key, value, .. } = saved else {
            panic!("not saved: {saved:?}");
        };
        assert_eq!(
            (key.as_str(), value.as_str()),
            ("prompts.judge.writer", "\"built-in\"")
        );
    }

    #[test]
    fn the_writer_is_saved_as_a_setting() {
        let crew = writer_setting(None, "built-in").unwrap();
        assert_eq!(
            crew,
            SlashCommand::Config {
                key: Some("prompts.writer".into()),
                value: Some("\"built-in\"".into()),
                reset: false,
            }
        );
        let judge = writer_setting(Some("judge"), "codex/gpt-5.6").unwrap();
        assert_eq!(
            judge,
            SlashCommand::Config {
                key: Some("prompts.judge.writer".into()),
                value: Some("\"codex/gpt-5.6\"".into()),
                reset: false,
            }
        );
        // The settings catalog knows both keys, so a form made from it lists them.
        for key in ["prompts.writer", "prompts.judge.writer"] {
            let meta = argonauts_registry::setting_meta(key);
            assert!(meta.is_some_and(|meta| !meta.safety), "{key}");
        }
        assert!(writer_setting(Some("captain"), "built-in").is_err());
        assert!(writer_setting(None, "not a model!").is_err());
    }
}
