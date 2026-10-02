//! `/preset show|diff|copy` and the Library tab of `/workflows`: the preset and workflow files.
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
use argonauts_core::layout::{project_files, user_files};
use argonauts_core::log::DiffStat;
use argonauts_core::views::{WorkflowFile, WorkflowOrigin};
use argonauts_crew::workflow::builtin::BUILTIN as BUILTIN_WORKFLOWS;
use argonauts_registry::workflows::WorkflowDirs;
use argonauts_registry::{SettingsError, workflows_in};
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
    noun: "preset",
    command: Some("/preset"),
    extension: "toml",
    folder: project_files::PRESETS,
    builtin: argonauts_registry::builtin_preset,
    write: argonauts_registry::write_preset,
};

/// The crew engine's built-in workflows, the same text it runs.
const WORKFLOWS: Kind = Kind {
    noun: "workflow",
    command: None,
    extension: "yaml",
    folder: project_files::WORKFLOWS,
    builtin: builtin_workflow,
    write: write_workflow,
};

fn builtin_workflow(name: &str) -> Result<String, SettingsError> {
    BUILTIN_WORKFLOWS
        .iter()
        .find(|(builtin, _)| *builtin == name)
        .map(|(_, text)| (*text).to_owned())
        .ok_or_else(|| {
            let names: Vec<&str> = BUILTIN_WORKFLOWS.iter().map(|(name, _)| *name).collect();
            SettingsError::Invalid {
                layer: "the built-in workflows".to_owned(),
                message: format!(
                    "there's no built-in workflow named {name:?}; the built-ins are {}",
                    names.join(", ")
                ),
            }
        })
}

/// Writes the built-in into `dir`, unless a file there already differs from it, which holds
/// your edits.
fn write_workflow(name: &str, dir: &Path) -> Result<PathBuf, SettingsError> {
    let text = builtin_workflow(name)?;
    let path = dir.join(format!("{name}.yaml"));
    let io = |err: std::io::Error| SettingsError::Io {
        path: path.clone(),
        message: err.to_string(),
    };
    match std::fs::read_to_string(&path) {
        Ok(existing) if existing == text => return Ok(path),
        Ok(_) => {
            return Err(SettingsError::Refused(format!(
                "{} has your edits, so it wasn't replaced",
                path.display()
            )));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io(err)),
    }
    std::fs::write(&path, text).map_err(io)?;
    Ok(path)
}

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
    let name = name.unwrap_or_else(|| ctx.settings().preset.clone());
    act(ctx, &PRESETS, &name, action).map(|_| Vec::new())
}

pub(crate) async fn workflow(
    ctx: &mut Ctx,
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
    let opened = act(ctx, &WORKFLOWS, &name, action)?;
    // The Library tab is still on top, under any notice, so it gets the files as they are
    // now: a copy shows where the built-in was, and a diff with nothing to show says why.
    // After a pager opens, a list would open a second library over it.
    if !shows && !opened {
        library(ctx, true);
    }
    Ok(Vec::new())
}

/// The Library tab of the workflow view: the built-ins, then your files, then the repo's, a
/// file shadowing a workflow of the same name.
pub(crate) fn library(ctx: &Ctx, refresh: bool) {
    let dirs = Dirs::new(ctx, &WORKFLOWS);
    let in_use = ctx.settings().workflow.as_str().to_owned();
    let found = workflows_in(&WorkflowDirs {
        global: &dirs.global,
        project: Some(&dirs.project),
        builtins: BUILTIN_WORKFLOWS,
    });
    let workflows = found
        .into_iter()
        .map(|info| {
            let builtin = builtin_workflow(&info.name).ok();
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
            WorkflowFile {
                in_use: info.name == in_use,
                name: info.name,
                origin,
                source,
                description: info.description,
                text,
                edited,
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
        let global = match kind.noun {
            "preset" => ctx.env.paths.global_presets(),
            _ => ctx.env.paths.home().join(user_files::WORKFLOWS),
        };
        Self {
            project: ctx.env.repo_root.join(".argonauts").join(kind.folder),
            global,
        }
    }

    /// Your file for `name`, the repo's first.
    fn find(&self, kind: &Kind, name: &str) -> Option<PathBuf> {
        let file = format!("{name}.{}", kind.extension);
        [&self.project, &self.global]
            .into_iter()
            .map(|dir| dir.join(&file))
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

struct FileDiff {
    text: String,
    stat: DiffStat,
}

fn file_diff(old: &str, new: &str, old_name: &str, new_name: &str) -> FileDiff {
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
                command: SlashCommand::Preset {
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

    #[tokio::test]
    async fn the_library_lists_built_ins_yours_and_the_repos() {
        let mut harness = Harness::start().await;
        let project = harness.repo.root().join(".argonauts/workflows");
        let global = harness.repo.home().join(user_files::WORKFLOWS);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(project.join("release.yaml"), "name: release\n").unwrap();
        std::fs::write(global.join("solo.yaml"), "name: solo\n# mine\n").unwrap();
        harness
            .send(UiCommand::Request {
                view: argonauts_core::app::ViewRequest::WorkflowLibrary,
            })
            .await;
        let release = library_row(&mut harness, "release").await;
        assert_eq!(release.origin, WorkflowOrigin::Repo);
        assert_eq!(release.edited, None);
        assert_eq!(release.text, "name: release\n");
        harness
            .send(UiCommand::Request {
                view: argonauts_core::app::ViewRequest::WorkflowLibrary,
            })
            .await;
        let solo = library_row(&mut harness, "solo").await;
        assert_eq!(solo.origin, WorkflowOrigin::Yours);
        assert_eq!(solo.edited, Some(true));
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
        let dir = harness.repo.root().join(".argonauts/presets");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.toml"), "preset = \"mine\"\n").unwrap();

        let shown = preset_command(&mut harness, "mine", PresetAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("preset = \"mine\""), "{text}");
        assert!(text.contains(".argonauts/presets/mine.toml"), "{text}");

        let none = preset_command(&mut harness, "peer-crew", PresetAction::Diff).await;
        let (_, text) = notice(&none).unwrap();
        assert!(text.contains("/preset copy peer-crew"), "{text}");

        let bad = preset_command(&mut harness, "../escape", PresetAction::Show).await;
        let (level, text) = notice(&bad).unwrap();
        assert_eq!(level, NoteLevel::Error);
        assert!(text.contains("isn't a preset name"), "{text}");
        harness.exit().await;
    }

    #[tokio::test]
    async fn workflow_show_prefers_the_repo_file_then_yours() {
        let mut harness = Harness::start().await;
        let project = harness.repo.root().join(".argonauts/workflows");
        let global = harness.repo.home().join(user_files::WORKFLOWS);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(project.join("mine.yaml"), "name: mine\n# the repo's\n").unwrap();
        std::fs::write(global.join("mine.yaml"), "name: mine\n# yours\n").unwrap();
        std::fs::write(global.join("solo-mine.yaml"), "name: solo-mine\n").unwrap();

        let shown = workflow_command(&mut harness, "mine", WorkflowAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("# the repo's"), "{text}");
        assert!(!text.contains("# yours"), "{text}");
        assert!(text.contains(".argonauts/workflows/mine.yaml"), "{text}");

        let shown = workflow_command(&mut harness, "solo-mine", WorkflowAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("name: solo-mine"), "{text}");
        assert!(text.contains("workflows/solo-mine.yaml"), "{text}");

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
        assert!(
            text.contains("copy nobody-wrote-this in the Library tab of /workflows"),
            "{text}"
        );
        // A diff that opens no pager sends the list again, for the tab still on top.
        assert!(next_library(&mut harness).await.1);

        let bad = workflow_command(&mut harness, "../escape", WorkflowAction::Copy).await;
        let (level, text) = notice(&bad).unwrap();
        assert_eq!(level, NoteLevel::Error);
        assert!(text.contains("isn't a workflow name"), "{text}");
        assert!(!harness.repo.home().join("escape.yaml").exists());
        harness.exit().await;
    }

    #[tokio::test]
    async fn the_engines_built_in_workflows_show_diff_and_copy() {
        let mut harness = Harness::start().await;
        let shown = workflow_command(&mut harness, "solo", WorkflowAction::Show).await;
        let (level, text) = notice(&shown).unwrap();
        assert_eq!(level, NoteLevel::Info);
        assert!(text.contains("built in"), "{text}");
        assert!(
            text.contains(builtin_workflow("solo").unwrap().trim_end()),
            "{text}"
        );

        let copied = workflow_command(&mut harness, "solo", WorkflowAction::Copy).await;
        let (level, text) = notice(&copied).unwrap();
        assert_eq!(level, NoteLevel::Info, "{text}");
        let yours = harness
            .repo
            .home()
            .join(user_files::WORKFLOWS)
            .join("solo.yaml");
        assert_eq!(
            std::fs::read_to_string(&yours).unwrap(),
            builtin_workflow("solo").unwrap()
        );
        // The open Library tab gets the list again, with your copy where the built-in was.
        let (files, refresh) = next_library(&mut harness).await;
        assert!(refresh);
        let solo = files.into_iter().find(|file| file.name == "solo").unwrap();
        assert_eq!(solo.origin, WorkflowOrigin::Yours);
        assert!(
            solo.source.ends_with("workflows/solo.yaml"),
            "{}",
            solo.source
        );
        assert_eq!(solo.edited, Some(false));

        let same = workflow_command(&mut harness, "solo", WorkflowAction::Diff).await;
        let (_, text) = notice(&same).unwrap();
        assert!(
            text.contains("matches the built-in solo workflow"),
            "{text}"
        );
        assert_eq!(library_row(&mut harness, "solo").await.edited, Some(false));

        std::fs::write(&yours, "name: solo\n# mine\n").unwrap();
        let edited = workflow_command(&mut harness, "solo", WorkflowAction::Diff).await;
        let AppEvent::View { data } = &edited else {
            panic!("{edited:?}");
        };
        assert!(matches!(**data, ViewData::Diff { .. }), "{edited:?}");
        let kept = workflow_command(&mut harness, "solo", WorkflowAction::Copy).await;
        let (level, text) = notice(&kept).unwrap();
        assert_eq!(level, NoteLevel::Warn, "{text}");
        assert_eq!(
            std::fs::read_to_string(&yours).unwrap(),
            "name: solo\n# mine\n"
        );
        assert_eq!(library_row(&mut harness, "solo").await.edited, Some(true));
        harness.exit().await;
    }

    #[tokio::test]
    async fn a_registry_error_reaches_you_as_a_notice() {
        let mut harness = Harness::start().await;
        // Whatever the registry answers, copy either writes the file or says why not.
        let copied = preset_command(&mut harness, "peer-crew", PresetAction::Copy).await;
        let (level, text) = notice(&copied).unwrap();
        match level {
            NoteLevel::Info => assert!(text.starts_with("Wrote "), "{text}"),
            _ => assert!(text.contains("peer-crew"), "{text}"),
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
