//! `/workflow show|diff|copy` and the workflow library of `/workflows`: the workflow files.
//!
//! A file in the repo's `.argonauts/` wins over yours in `~/.argonauts/`, which wins over the
//! built-in of the same name.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use argonauts_core::CardId;
use argonauts_core::NoteLevel;
use argonauts_core::api::DiffView;
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::input::{PresetAction, WorkflowAction};
use argonauts_core::layout::project_files;
use argonauts_core::log::DiffStat;
use argonauts_core::views::{WorkflowFile, WorkflowOrigin};
use argonauts_registry::{SettingsError, presets_in};
use similar::{ChangeTag, TextDiff};

use super::{Ctx, Effect};

/// What `show`, `diff`, and `copy` do, whichever kind of file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Show,
    Diff,
    Copy { project: bool },
}

struct Kind {
    noun: &'static str,
    /// The command that shows, copies, and diffs these files, or `None` for the Library tab of
    /// `/workflows`.
    command: Option<&'static str>,
    extension: &'static str,
    folder: &'static str,
    builtin: fn(&str) -> Result<String, SettingsError>,
    write: fn(&str, &Path) -> Result<PathBuf, SettingsError>,
}

const PRESETS: Kind = Kind {
    noun: "workflow",
    command: Some("/workflow"),
    extension: "toml",
    folder: project_files::PRESETS,
    builtin: argonauts_registry::builtin_preset,
    write: argonauts_registry::write_preset,
};

/// A workflow may be a script in place of a file of steps. A script wins over a file of steps
/// of the same name, as it does when the workflow runs.
const SCRIPTS: [&str; 2] = ["js", "mjs"];

pub(crate) async fn preset(
    ctx: &mut Ctx,
    name: Option<String>,
    action: PresetAction,
) -> anyhow::Result<Vec<Effect>> {
    let action = match action {
        PresetAction::Show => Action::Show,
        PresetAction::Diff => Action::Diff,
        PresetAction::Copy => Action::Copy { project: false },
        PresetAction::CopyToProject => Action::Copy { project: true },
        // Picking a preset is a setting, which the settings part handles.
        PresetAction::Use => return Ok(Vec::new()),
    };
    let name = name.unwrap_or_else(|| ctx.settings().workflow.clone());
    act(ctx, &PRESETS, &name, action).map(|_| Vec::new())
}

pub(crate) async fn workflow(
    ctx: &mut Ctx,
    taken: &[String],
    name: String,
    action: WorkflowAction,
) -> anyhow::Result<Vec<Effect>> {
    let shows = action == WorkflowAction::Show;
    let action = match action {
        WorkflowAction::Show => Action::Show,
        WorkflowAction::Diff => Action::Diff,
        WorkflowAction::Copy => Action::Copy { project: false },
        WorkflowAction::CopyToProject => Action::Copy { project: true },
    };
    let opened = act(ctx, &PRESETS, &name, action)?;
    // The Library tab is still on top, under any notice, so it gets the files as they are
    // now: a copy shows where the built-in was, and a diff with nothing to show says why.
    // After a pager opens, a list would open a second library over it.
    if !shows && !opened {
        library(ctx, taken, true);
    }
    Ok(Vec::new())
}

/// The workflow library: the built-ins, then your files, then the repo's, a file shadowing a
/// workflow of the same name. These are the workflows `/workflow` lists and `/<name>` runs.
pub(crate) fn library(ctx: &Ctx, taken: &[String], refresh: bool) {
    let dirs = Dirs::new(ctx, &PRESETS);
    let in_use = ctx.settings().workflow.clone();
    let found = presets_in(&dirs.global, Some(&dirs.project));
    let workflows = found
        .into_iter()
        .map(|info| {
            let builtin = (PRESETS.builtin)(&info.name).ok();
            let (origin, source, text) = if info.source == "built-in" {
                let text = builtin.clone().unwrap_or_default();
                (WorkflowOrigin::BuiltIn, "built in".to_owned(), text)
            } else {
                let path = PathBuf::from(&info.source);
                let origin = if path.starts_with(&dirs.project) {
                    WorkflowOrigin::Repo
                } else {
                    WorkflowOrigin::Yours
                };
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                (origin, ctx.env.display(&path), text)
            };
            let edited = match (origin, &builtin) {
                (WorkflowOrigin::BuiltIn, _) | (_, None) => None,
                (_, Some(builtin)) => Some(*builtin != text),
            };
            // A copy of a built-in keeps its name; any other file named like a command never
            // becomes that command, so the list says why `/<name>` won't run it.
            let problem = (builtin.is_none() && taken.contains(&info.name)).then(|| {
                format!(
                    "/{} is already a command, so this workflow isn't one; rename the file",
                    info.name
                )
            });
            WorkflowFile {
                in_use: info.name == in_use,
                name: info.name,
                origin,
                source,
                description: info.description,
                text,
                edited,
                problem,
            }
        })
        .collect();
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::WorkflowLibrary { workflows, refresh }),
    });
}

/// How you ask for `verb` on `name`, said after "To ...,".
fn ask_for(kind: &Kind, verb: &str, name: &str) -> String {
    match kind.command {
        Some(command) => format!("run {command} {verb} {name}"),
        None => format!("{verb} {name} in the Library tab of /workflows"),
    }
}

/// True when a view opened, such as the diff pager.
fn act(ctx: &Ctx, kind: &Kind, name: &str, action: Action) -> anyhow::Result<bool> {
    // The name becomes a file name, so it must not reach outside the folder.
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        bail!("{name:?} isn't a {} name", kind.noun);
    }
    let dirs = Dirs::new(ctx, kind);
    match action {
        Action::Show => show(ctx, kind, &dirs, name).map(|()| false),
        Action::Diff => diff(ctx, kind, &dirs, name),
        Action::Copy { project } => copy(ctx, kind, &dirs, name, project).map(|()| false),
    }
}

struct Dirs {
    project: PathBuf,
    global: PathBuf,
}

impl Dirs {
    fn new(ctx: &Ctx, kind: &Kind) -> Self {
        Self {
            project: ctx.env.repo_root.join(".argonauts").join(kind.folder),
            global: ctx.env.paths.global_presets(),
        }
    }

    /// Your file for `name`, the repo's first, a script before a file of steps.
    fn find(&self, kind: &Kind, name: &str) -> Option<PathBuf> {
        let extensions = SCRIPTS.iter().chain([&kind.extension]);
        [&self.project, &self.global]
            .into_iter()
            .flat_map(|dir| {
                extensions
                    .clone()
                    .map(|extension| dir.join(format!("{name}.{extension}")))
            })
            .find(|path| path.is_file())
    }
}

fn show(ctx: &Ctx, kind: &Kind, dirs: &Dirs, name: &str) -> anyhow::Result<()> {
    let (from, text) = match dirs.find(kind, name) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", ctx.env.display(&path)))?;
            (ctx.env.display(&path), text)
        }
        None => match (kind.builtin)(name) {
            Ok(text) => ("built in".to_owned(), text),
            Err(err) => bail!("There's no {} file named {name}, and {err}", kind.noun),
        },
    };
    ctx.notice(
        NoteLevel::Info,
        format!("The {name} {}, {from}:\n\n{}", kind.noun, text.trim_end()),
    );
    Ok(())
}

/// True when the diff pager opened; a notice says why when it didn't.
fn diff(ctx: &Ctx, kind: &Kind, dirs: &Dirs, name: &str) -> anyhow::Result<bool> {
    let noun = kind.noun;
    let Some(path) = dirs.find(kind, name) else {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "You have no {noun} file named {name}, so the built-in one is in use. To edit \
                 one, {}.",
                ask_for(kind, "copy", name)
            ),
        );
        return Ok(false);
    };
    let shown = ctx.env.display(&path);
    let yours = std::fs::read_to_string(&path).with_context(|| format!("reading {shown}"))?;
    let builtin = (kind.builtin)(name)
        .with_context(|| format!("comparing {shown} with the built-in {name} {noun}"))?;
    if yours == builtin {
        ctx.notice(
            NoteLevel::Info,
            format!("{shown} matches the built-in {name} {noun}."),
        );
        return Ok(false);
    }
    let card = CardId::new(name).with_context(|| format!("{name:?} can't title a diff"))?;
    let diff = file_diff(&builtin, &yours, &format!("built-in {name}"), &shown);
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Diff {
            diff: DiffView {
                card,
                stat: diff.stat,
                text: diff.text,
                path,
            },
        }),
    });
    Ok(true)
}

fn copy(ctx: &Ctx, kind: &Kind, dirs: &Dirs, name: &str, project: bool) -> anyhow::Result<()> {
    let noun = kind.noun;
    let dir = if project { &dirs.project } else { &dirs.global };
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", ctx.env.display(dir)))?;
    match (kind.write)(name, dir) {
        Ok(path) => {
            let wins = if project {
                format!("It now wins over yours and the built-in {name} {noun} in this repo.")
            } else {
                format!("It now wins over the built-in {name} {noun}.")
            };
            ctx.notice(
                NoteLevel::Info,
                format!("Wrote {}. {wins}", ctx.env.display(&path)),
            );
            Ok(())
        }
        Err(SettingsError::Refused(_)) => {
            let path = dir.join(format!("{name}.{}", kind.extension));
            ctx.notice(
                NoteLevel::Warn,
                format!(
                    "You edited {}, so it was kept. To see what changed, {}. To start over, \
                     delete the file.",
                    ctx.env.display(&path),
                    ask_for(kind, "diff", name)
                ),
            );
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("copying the {name} {noun}")),
    }
}

pub(crate) struct FileDiff {
    pub text: String,
    pub stat: DiffStat,
}

pub(crate) fn file_diff(old: &str, new: &str, old_name: &str, new_name: &str) -> FileDiff {
    let diff = TextDiff::from_lines(old, new);
    let mut stat = DiffStat {
        files_changed: 1,
        insertions: 0,
        deletions: 0,
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => stat.insertions += 1,
            ChangeTag::Delete => stat.deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    let text = diff
        .unified_diff()
        .context_radius(3)
        .header(old_name, new_name)
        .to_string();
    FileDiff { text, stat }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::Harness;
    use super::*;
    use argonauts_core::app::UiCommand;
    use argonauts_core::input::SlashCommand;

    fn notice(event: &AppEvent) -> Option<(NoteLevel, &str)> {
        match event {
            AppEvent::Notice { level, text } => Some((*level, text.as_str())),
            _ => None,
        }
    }

    async fn preset_command(harness: &mut Harness, name: &str, action: PresetAction) -> AppEvent {
        harness
            .send(UiCommand::Slash {
                command: SlashCommand::Workflow {
                    name: Some(name.to_owned()),
                    action,
                },
            })
            .await;
        harness
            .until(|event| notice(event).is_some() || matches!(event, AppEvent::View { .. }))
            .await
    }

    /// The next Library list's row for `name`.
    async fn library_row(harness: &mut Harness, name: &str) -> WorkflowFile {
        next_library(harness)
            .await
            .0
            .into_iter()
            .find(|file| file.name == name)
            .unwrap_or_else(|| panic!("{name} in the library"))
    }

    /// The next Library list, and whether it's a refresh for the tab already open.
    async fn next_library(harness: &mut Harness) -> (Vec<WorkflowFile>, bool) {
        let event = harness
            .until(|event| match event {
                AppEvent::View { data } => {
                    matches!(data.as_ref(), ViewData::WorkflowLibrary { .. })
                }
                _ => false,
            })
            .await;
        let AppEvent::View { data } = event else {
            unreachable!()
        };
        let ViewData::WorkflowLibrary { workflows, refresh } = *data else {
            unreachable!()
        };
        (workflows, refresh)
    }

    /// A file of steps of your own, as `/workflow new` would start one.
    const RELEASE: &str = "name = \"release\"\nabout = \"Cut a release.\"\n\n[[step]]\nname = \"Tag\"\nrole = \"workers\"\n";

    #[tokio::test]
    async fn the_library_lists_the_workflows_built_in_yours_and_the_repos() {
        let mut harness = Harness::start().await;
        let project = harness.repo.root().join(".argonauts/workflows");
        let global = harness.env.paths.global_presets();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(project.join("release.toml"), RELEASE).unwrap();
        std::fs::write(global.join("solo.toml"), "# mine\n").unwrap();
        let clash = RELEASE.replace("release", "model");
        std::fs::write(project.join("model.toml"), clash).unwrap();
        harness
            .send(UiCommand::Request {
                view: argonauts_core::app::ViewRequest::WorkflowLibrary,
            })
            .await;
        let (files, _) = next_library(&mut harness).await;
        let release = files.iter().find(|file| file.name == "release").unwrap();
        assert_eq!(release.origin, WorkflowOrigin::Repo);
        assert_eq!(release.edited, None);
        assert_eq!(release.text, RELEASE);
        assert_eq!(release.description, "Cut a release.");
        assert_eq!(release.problem, None);
        let solo = files.iter().find(|file| file.name == "solo").unwrap();
        assert_eq!(solo.origin, WorkflowOrigin::Yours);
        assert_eq!(solo.edited, Some(true));
        assert_eq!(solo.problem, None, "a copy of a built-in keeps its name");
        // A file named like a command never becomes that command, and the list says why.
        let model = files.iter().find(|file| file.name == "model").unwrap();
        let problem = model.problem.as_deref().unwrap_or_default();
        assert!(
            problem.starts_with("/model is already a command"),
            "{problem}"
        );
        // The workflows are the ones `/workflow` lists, the one in use marked.
        let council = files.iter().find(|file| file.name == "council").unwrap();
        assert_eq!(council.origin, WorkflowOrigin::BuiltIn);
        assert!(council.in_use);
        assert!(council.text.contains("[[step]]"), "{}", council.text);
        // The fixed phases the engine keeps for itself are no workflow of their own.
        assert!(!files.iter().any(|file| file.name == "script"), "{files:?}");
        assert!(files.iter().all(|file| !file.source.ends_with(".yaml")));
        harness.exit().await;
    }

    async fn workflow_command(
        harness: &mut Harness,
        name: &str,
        action: WorkflowAction,
    ) -> AppEvent {
        harness
            .send(UiCommand::WorkflowFile {
                name: name.to_owned(),
                action,
            })
            .await;
        harness
            .until(|event| notice(event).is_some() || matches!(event, AppEvent::View { .. }))
            .await
    }

    #[tokio::test]
    async fn show_prints_your_file_first_and_diff_needs_one() {
        let mut harness = Harness::start().await;
        let dir = harness.repo.root().join(".argonauts/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.toml"), "workflow = \"mine\"\n").unwrap();

        let shown = preset_command(&mut harness, "mine", PresetAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("workflow = \"mine\""), "{text}");
        assert!(text.contains(".argonauts/workflows/mine.toml"), "{text}");

        let none = preset_command(&mut harness, "council", PresetAction::Diff).await;
        let (_, text) = notice(&none).unwrap();
        assert!(text.contains("/workflow copy council"), "{text}");

        let bad = preset_command(&mut harness, "../escape", PresetAction::Show).await;
        let (level, text) = notice(&bad).unwrap();
        assert_eq!(level, NoteLevel::Error);
        assert!(text.contains("isn't a workflow name"), "{text}");
        harness.exit().await;
    }

    #[tokio::test]
    async fn workflow_show_prefers_the_repo_file_then_yours_and_reads_a_script() {
        let mut harness = Harness::start().await;
        let project = harness.repo.root().join(".argonauts/workflows");
        let global = harness.env.paths.global_presets();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            project.join("mine.toml"),
            format!("# the repo's\n{RELEASE}"),
        )
        .unwrap();
        std::fs::write(global.join("mine.toml"), format!("# yours\n{RELEASE}")).unwrap();
        let sweep = "export const meta = { name: 'sweep', description: 'Find and fix' }\n";
        std::fs::write(global.join("sweep.js"), sweep).unwrap();

        let shown = workflow_command(&mut harness, "mine", WorkflowAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("# the repo's"), "{text}");
        assert!(!text.contains("# yours"), "{text}");
        assert!(text.contains(".argonauts/workflows/mine.toml"), "{text}");

        let shown = workflow_command(&mut harness, "sweep", WorkflowAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("description: 'Find and fix'"), "{text}");
        assert!(text.contains("workflows/sweep.js"), "{text}");

        let missing =
            workflow_command(&mut harness, "nobody-wrote-this", WorkflowAction::Show).await;
        let (level, text) = notice(&missing).unwrap();
        assert_eq!(level, NoteLevel::Error);
        assert!(
            text.contains("no workflow file named nobody-wrote-this"),
            "{text}"
        );

        let none = workflow_command(&mut harness, "nobody-wrote-this", WorkflowAction::Diff).await;
        let (_, text) = notice(&none).unwrap();
        assert!(text.contains("/workflow copy nobody-wrote-this"), "{text}");
        // A diff that opens no pager sends the list again, for the library still on top.
        assert!(next_library(&mut harness).await.1);

        let bad = workflow_command(&mut harness, "../escape", WorkflowAction::Copy).await;
        let (level, text) = notice(&bad).unwrap();
        assert_eq!(level, NoteLevel::Error);
        assert!(text.contains("isn't a workflow name"), "{text}");
        assert!(!harness.repo.home().join("escape.toml").exists());
        harness.exit().await;
    }

    #[tokio::test]
    async fn the_built_in_workflows_show_diff_and_copy_from_the_library() {
        let mut harness = Harness::start().await;
        let builtin = argonauts_registry::builtin_preset("maker-checker").unwrap();
        let shown = workflow_command(&mut harness, "maker-checker", WorkflowAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("built in"), "{text}");
        assert!(text.contains(builtin.trim_end()), "{text}");

        let copied = workflow_command(&mut harness, "maker-checker", WorkflowAction::Copy).await;
        let (level, text) = notice(&copied).unwrap();
        assert_eq!(level, NoteLevel::Info, "{text}");
        let yours = harness
            .env
            .paths
            .global_presets()
            .join("maker-checker.toml");
        assert_eq!(std::fs::read_to_string(&yours).unwrap(), builtin);
        // The open library gets the list again, with your copy where the built-in was.
        let (files, refresh) = next_library(&mut harness).await;
        assert!(refresh);
        let copy = files
            .into_iter()
            .find(|file| file.name == "maker-checker")
            .unwrap();
        assert_eq!(copy.origin, WorkflowOrigin::Yours);
        assert!(
            copy.source.ends_with("workflows/maker-checker.toml"),
            "{}",
            copy.source
        );
        assert_eq!(copy.edited, Some(false));

        let same = workflow_command(&mut harness, "maker-checker", WorkflowAction::Diff).await;
        let (_, text) = notice(&same).unwrap();
        assert!(
            text.contains("matches the built-in maker-checker workflow"),
            "{text}"
        );
        let row = library_row(&mut harness, "maker-checker").await;
        assert_eq!(row.edited, Some(false));

        let edited_text = format!("{builtin}# mine\n");
        std::fs::write(&yours, &edited_text).unwrap();
        let edited = workflow_command(&mut harness, "maker-checker", WorkflowAction::Diff).await;
        let AppEvent::View { data } = &edited else {
            panic!("{edited:?}");
        };
        assert!(matches!(**data, ViewData::Diff { .. }), "{edited:?}");
        let kept = workflow_command(&mut harness, "maker-checker", WorkflowAction::Copy).await;
        let (level, text) = notice(&kept).unwrap();
        assert_eq!(level, NoteLevel::Warn, "{text}");
        assert_eq!(std::fs::read_to_string(&yours).unwrap(), edited_text);
        let row = library_row(&mut harness, "maker-checker").await;
        assert_eq!(row.edited, Some(true));
        harness.exit().await;
    }

    #[tokio::test]
    async fn a_registry_error_reaches_you_as_a_notice() {
        let mut harness = Harness::start().await;
        // Whatever the registry answers, copy either writes the file or says why not.
        let copied = preset_command(&mut harness, "council", PresetAction::Copy).await;
        let (level, text) = notice(&copied).unwrap();
        match level {
            NoteLevel::Info => assert!(text.starts_with("Wrote "), "{text}"),
            _ => assert!(text.contains("council"), "{text}"),
        }
        harness.exit().await;
    }

    #[test]
    fn the_diff_counts_lines_both_ways() {
        let diff = file_diff("a\nb\nc\n", "a\nB\nc\nd\n", "built-in x", "~/x.toml");
        assert_eq!(diff.stat.insertions, 2);
        assert_eq!(diff.stat.deletions, 1);
        assert!(
            diff.text.starts_with("--- built-in x\n+++ ~/x.toml\n"),
            "{}",
            diff.text
        );
        assert!(diff.text.contains("-b\n+B\n"), "{}", diff.text);
    }
}
