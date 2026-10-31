//! A workflow of your own: `/workflow new` writes one from a template, `/workflow try` shows
//! what it would run and offers to run or edit it, `/workflow edit` opens its page and checks
//! the file each time it changes, and every workflow file is a command of its own name that
//! runs one request with it.

mod page;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use argonauts_core::RunId;
use argonauts_core::agents::cli_commands::{CliCommand, CommandKind, CommandScope, CommandSource};
use argonauts_core::app::workflow_page::WorkflowChange;
use argonauts_core::app::{AppEvent, ChoiceKind};
use argonauts_core::base::log::NoteLevel;
use argonauts_core::base::names::{ChoiceId, ModelRef, WorkflowId};
use argonauts_core::input::{SHARED_COMMANDS, SLASH_COMMANDS, slash_spec};
use argonauts_core::runs::events::approval::AnswerAction;
use argonauts_core::runs::events::run::RunLifecycle;
use argonauts_core::runs::events::{RunEvent, RunEventKind, WorkflowEvent};
use argonauts_core::settings::crew::CrewRole;
use argonauts_core::workflows::workflow_files;
use argonauts_runner::{DryRun, Library, LibraryError, Loaded, Origin, Row, dry_run};
use unicode_width::UnicodeWidthStr;

use super::base::ctx::Internal;
use super::clis::cli_commands::CliCommands;
use super::conversation::{self, ConversationState};
use super::{Ctx, Effect};
use crate::setup::choices::{self, EDIT_IT};

/// How often a file being edited is looked at, and for how long.
const LOOK_EVERY: Duration = Duration::from_millis(300);
const WATCH_FOR: Duration = Duration::from_secs(2 * 60 * 60);

/// What the workflow commands keep between two keys of yours.
#[derive(Default)]
pub(crate) struct State {
    /// The dry runs whose question is open.
    tried: BTreeMap<ChoiceId, Tried>,
    /// The file `/workflow edit` opened last, looked at until another takes its place.
    watching: Option<tokio::task::JoinHandle<()>>,
    /// The text the page wrote last. A save that left exactly this is the page's own, which
    /// it has shown already.
    wrote: Option<String>,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(watching) = self.watching.take() {
            watching.abort();
        }
    }
}

/// One dry run: the workflow and the request it was tried with.
struct Tried {
    workflow: String,
    request: String,
}

pub(crate) fn owns_choice(state: &State, id: &ChoiceId) -> bool {
    state.tried.contains_key(id)
}

/// The two folders a workflow of your own may sit in. Outside a repository there is only yours.
pub(crate) fn library(ctx: &Ctx) -> Library {
    Library {
        project: Some(ctx.env.repo_root.join(".argonauts/workflows")),
        yours: Some(ctx.env.paths.global_workflows()),
        waiting: argonauts_crew::prompts::trust::waiting(&ctx.env.repo_root, &ctx.env.state_dir)
            .all()
            .collect(),
    }
}

/// Your workflows as commands, for the `/` menu and the parser: each runs one request with it.
pub(crate) fn commands(ctx: &Ctx) -> Vec<CliCommand> {
    library(ctx)
        .list()
        .into_iter()
        .filter(|listed| listed.problem.is_none())
        // A file named after a command of Argonauts Code's own never hides that command.
        .filter(|listed| slash_spec(&listed.name).is_none())
        .filter_map(|listed| {
            let scope = match listed.origin {
                Origin::Project => CommandScope::Project,
                Origin::Yours => CommandScope::User,
                Origin::BuiltIn => return None,
            };
            Some(CliCommand {
                name: listed.name,
                aliases: Vec::new(),
                // Bare, it switches to the workflow; with words it runs that one request.
                args: "[request]".to_owned(),
                help: listed.about,
                kind: CommandKind::Workflow,
                source: CommandSource::Custom {
                    scope,
                    path: listed.path?,
                },
            })
        })
        .collect()
}

/// Every name a command already has, which a workflow can't take.
pub(crate) fn taken(agent_id: &CliCommands) -> Vec<String> {
    let ours = SLASH_COMMANDS
        .iter()
        .chain(SHARED_COMMANDS)
        .map(|spec| spec.name.to_owned());
    let theirs = agent_id.set().into_iter().flat_map(|set| {
        set.commands
            .iter()
            .filter(|command| command.kind != CommandKind::Workflow)
            .flat_map(|command| std::iter::once(&command.name).chain(&command.aliases))
            .cloned()
    });
    ours.chain(theirs).collect()
}

/// `/workflow new <name> [from <built-in>]`.
pub(crate) fn new(
    ctx: &Ctx,
    agent_id: &CliCommands,
    name: &str,
    from: Option<&str>,
) -> anyhow::Result<Vec<Effect>> {
    let taken = taken(agent_id);
    let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
    match library(ctx).create(name, from, &taken) {
        Ok(path) => {
            // What was written and where, then the three ways to use it. The folder's
            // README.md, written with its first workflow, explains each key.
            ctx.send(AppEvent::Record {
                title: format!("Wrote {name}"),
                tail: Some(ctx.env.shown(&path)),
                lines: Vec::new(),
                notes: vec![format!(
                    "/{name} <request> runs it once · /workflow {name} uses it · /workflow try \
                     previews it"
                )],
            });
            Ok(vec![Effect::CommandsChanged])
        }
        Err(err) => {
            ctx.notice(NoteLevel::Warn, err.to_string());
            Ok(Vec::new())
        }
    }
}

/// `/workflow try <name> <request>`: each step, its role, who would take it and how, on a question
/// that offers to run it or edit it. Nothing runs until you say so. The steps you require
/// include those a setup `kept` for the conversation.
pub(crate) fn try_out(
    ctx: &mut Ctx,
    state: &mut State,
    name: &str,
    request: &str,
    kept: &[argonauts_core::workflows::required::RequiredStep],
) -> anyhow::Result<Vec<Effect>> {
    let loaded = match library(ctx).load(name) {
        Ok(loaded) => Some(loaded),
        // A built-in that runs by phases of its own, as Council talks in its phases.
        Err(err) if runs_by_its_phases(name, &err) => None,
        Err(err) => {
            ctx.notice(NoteLevel::Warn, err.to_string());
            return Ok(Vec::new());
        }
    };
    let request = request.trim();
    let required = argonauts_crew::script::required::for_request(&library(ctx), kept, request);
    // The plan rows, then dim notes on what follows them.
    let (lines, notes): (Vec<String>, Vec<String>) = match &loaded {
        None => (phases_of(name), vec![checks_then_merge(ctx)]),
        Some(Loaded::Steps(file)) => {
            // A step you require shows where it would run, marked with a star.
            let file = required.added_to(file);
            let dry = dry_run(&file, &|role| role_models(ctx, role));
            let mut notes = vec![checks_then_merge(ctx)];
            if !required.tasks.is_empty() {
                notes.push(format!(
                    "the tasks are the ones you fixed: {}",
                    required.tasks.join(" · ")
                ));
            }
            if file.adapt {
                notes.push(
                    "adapt is on, so the orchestrator writes the steps for each request and \
                     keeps these roles"
                        .to_owned(),
                );
            }
            (plan(ctx, &file, &dry), notes)
        }
        Some(Loaded::Script(script)) => {
            let mut lines = vec![script.meta.description.clone()];
            lines.extend(script.meta.phases.iter().enumerate().map(|(index, phase)| {
                format!("{} {}  {}", index + 1, phase.name, phase.detail)
                    .trim_end()
                    .to_owned()
            }));
            let missing = required.missing_from(&script.meta);
            let note = if missing.is_empty() {
                "✓ the script reads · what its agents are is only known as it runs".to_owned()
            } else {
                // A run refuses it the same way, so it says so before anything runs.
                format!(
                    "✗ it has no phase for a step you require: {}",
                    missing.join(", ")
                )
            };
            (lines, vec![note])
        }
    };
    // The plan stays in the conversation whatever the answer; the question under it only asks.
    ctx.send(AppEvent::Record {
        title: format!("{name} for \"{request}\""),
        tail: Some("a preview, nothing runs yet".to_owned()),
        lines,
        notes,
    });
    let id = ctx.next_choice_id();
    ctx.send(AppEvent::Choice {
        choice: choices::app_choice(
            id.clone(),
            ChoiceKind::TryWorkflow,
            "Run it now?".to_owned(),
            Vec::new(),
            choices::options(ChoiceKind::TryWorkflow),
        ),
    });
    state.tried.insert(
        id,
        Tried {
            workflow: name.to_owned(),
            request: request.to_owned(),
        },
    );
    Ok(Vec::new())
}

/// A dry run's question answered: run that one request with the workflow, or open its file.
pub(crate) async fn answer_choice(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    state: &mut State,
    id: ChoiceId,
    action: AnswerAction,
) -> anyhow::Result<Vec<Effect>> {
    let Some(tried) = state.tried.remove(&id) else {
        ctx.notice(NoteLevel::Info, "That question is already closed.");
        return Ok(Vec::new());
    };
    match action {
        AnswerAction::Approve => {
            close(ctx, id, tried.closed("running it"));
            run(ctx, conversation, &tried.workflow, tried.request).await
        }
        AnswerAction::Choose { value } if value == EDIT_IT => {
            close(ctx, id, tried.closed(NOTHING_RAN));
            edit(ctx, state, &tried.workflow)
        }
        _ => {
            close(ctx, id, tried.closed(NOTHING_RAN));
            Ok(Vec::new())
        }
    }
}

/// A dry run's question set aside, as a new request does.
pub(crate) fn set_aside(ctx: &Ctx, state: &mut State, id: ChoiceId) -> anyhow::Result<Vec<Effect>> {
    if let Some(tried) = state.tried.remove(&id) {
        close(ctx, id, tried.closed(NOTHING_RAN));
    }
    Ok(Vec::new())
}

const NOTHING_RAN: &str = "nothing ran";

impl Tried {
    /// What closed the preview, naming it, as `Spec-first preview for 'add a burst limit' ·
    /// nothing ran`, so the line reads whole wherever it lands.
    fn closed(&self, outcome: &str) -> String {
        let mut name = self.workflow.chars();
        let name: String = name
            .next()
            .map(|first| first.to_uppercase().chain(name).collect())
            .unwrap_or_default();
        format!("{name} preview for '{}' · {outcome}", self.request)
    }
}

fn close(ctx: &Ctx, id: ChoiceId, outcome: impl Into<String>) {
    ctx.send(AppEvent::ChoiceClosed {
        id,
        outcome: outcome.into(),
    });
}

/// `/workflow edit <name>`: the page of a file of steps, with each step's keys to change and
/// a check on every change. A script has no keys to list, so its file goes to your editor
/// when you have one set. Either way the file is checked each time it is saved from here on.
pub(crate) fn edit(ctx: &Ctx, state: &mut State, name: &str) -> anyhow::Result<Vec<Effect>> {
    edit_with(ctx, state, name, editor(ctx))
}

/// One change made on a workflow's page.
pub(crate) fn page_change(
    ctx: &Ctx,
    state: &mut State,
    name: &str,
    change: WorkflowChange,
) -> anyhow::Result<Vec<Effect>> {
    page::change(ctx, state, name, change)
}

/// [`edit`] with the editor given: its program and arguments, which the file's path follows.
fn edit_with(
    ctx: &Ctx,
    state: &mut State,
    name: &str,
    editor: Option<Vec<String>>,
) -> anyhow::Result<Vec<Effect>> {
    let Some((origin, path)) = library(ctx).find(name) else {
        ctx.notice(
            NoteLevel::Warn,
            format!("There's no workflow named {name}. /workflow new {name} writes one."),
        );
        return Ok(Vec::new());
    };
    let Some(path) = path else {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "{name} is built in, so there's no file of yours to edit. /workflow new \
                 my-{name} from {name} starts one from it."
            ),
        );
        return Ok(Vec::new());
    };
    let whose = match origin {
        Origin::Project => "this repository's",
        _ => "yours",
    };
    state.watch(ctx, name, &path);
    if page::has_page(&path) {
        page::send(ctx, name, &path, None, false);
        return Ok(Vec::new());
    }
    let mut text = format!("{} · {whose}\n{}", ctx.env.shown(&path), checked(ctx, name));
    match editor {
        Some(mut argv) => {
            text.push_str(&format!(
                "\n  Opening it in {}. It is checked again each time you save it.",
                argv[0]
            ));
            ctx.notice(NoteLevel::Info, text);
            argv.push(path.display().to_string());
            ctx.send(AppEvent::run_in_terminal(argv, ctx.env.repo_root.clone()));
        }
        None => {
            text.push_str(
                "\n  Edit it in your editor. It is checked again each time you save it. With \
                 $VISUAL or $EDITOR set, /workflow edit opens it for you.",
            );
            ctx.notice(NoteLevel::Info, text);
        }
    }
    Ok(Vec::new())
}

/// The file `/workflow edit` watches was saved: its steps as they read now, or what is wrong,
/// in the conversation and on its page when that is open. The `/` menu hears of it too, since its
/// one line may have changed. A save the page made itself was shown when it was made.
pub(crate) fn saved(ctx: &Ctx, state: &mut State, name: &str) -> anyhow::Result<Vec<Effect>> {
    let path = library(ctx).find(name).and_then(|(_, path)| path);
    if let Some(path) = &path {
        let text = std::fs::read_to_string(path).ok();
        // Kept and not used up: of two saves from the page close together each is noticed, and
        // the first notice can arrive after the second save.
        if text.is_some() && text == state.wrote {
            return Ok(Vec::new());
        }
        state.wrote = None;
        if page::has_page(path) {
            page::send(
                ctx,
                name,
                path,
                Some("Saved from outside.".to_owned()),
                true,
            );
        }
    }
    ctx.notice(
        NoteLevel::Info,
        format!("{name} saved\n{}", checked(ctx, name)),
    );
    Ok(vec![Effect::CommandsChanged])
}

/// The workflow's steps in order with a line that says it is valid, or what is wrong with it.
fn checked(ctx: &Ctx, name: &str) -> String {
    match library(ctx).load(name) {
        Ok(Loaded::Steps(file)) => {
            let dry = dry_run(&file, &|role| role_models(ctx, role));
            format!("{}\n  ✓ the file is valid", table(ctx, &dry))
        }
        Ok(Loaded::Script(script)) => {
            format!("  ✓ the script reads: {}", script.meta.description)
        }
        Err(err) => format!("  ✗ {err}"),
    }
}

/// Your editor as a program and its arguments, read as Ctrl+G reads it: `$VISUAL`, else
/// `$EDITOR`. `None` without either, or with no terminal to hand over.
fn editor(ctx: &Ctx) -> Option<Vec<String>> {
    if !ctx.env.interactive {
        return None;
    }
    let (visual, editor) = (std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok());
    argonauts_tui::runtime::editor_command(visual.as_deref(), editor.as_deref()).ok()
}

impl State {
    /// Looks at `path` from now on, in place of the file watched before, and tells the
    /// controller each time it was saved.
    fn watch(&mut self, ctx: &Ctx, name: &str, path: &Path) {
        if let Some(before) = self.watching.take() {
            before.abort();
        }
        let (name, path) = (name.to_owned(), path.to_owned());
        let internal = ctx.internal.clone();
        // Read here and not in the task, which may first run after your first save.
        let mut seen = stamp(&path);
        self.watching = Some(tokio::spawn(async move {
            let until = tokio::time::Instant::now() + WATCH_FOR;
            while tokio::time::Instant::now() < until {
                tokio::time::sleep(LOOK_EVERY).await;
                let now = stamp(&path);
                if now == seen {
                    continue;
                }
                seen = now;
                // An editor that saves by writing a new file leaves none for a moment.
                if now.is_none() {
                    continue;
                }
                let saved = Internal::WorkflowSaved { name: name.clone() };
                if internal.send(saved).is_err() {
                    return;
                }
            }
        }));
    }
}

/// When the file was last written and how long it is, which together change on every save.
fn stamp(path: &PathBuf) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// `/<name> <request>`: one request with that workflow. The workflow in use stays as it was.
pub(crate) async fn run(
    ctx: &mut Ctx,
    conversation: &mut ConversationState,
    workflow: &str,
    request: String,
) -> anyhow::Result<Vec<Effect>> {
    if let Err(err) = library(ctx).load(workflow)
        && !runs_by_its_phases(workflow, &err)
    {
        ctx.notice(NoteLevel::Warn, err.to_string());
        return Ok(Vec::new());
    }
    conversation::start_with_workflow(ctx, conversation, request, workflow).await
}

/// A built-in's own phases as a try lists them: what it is for, then each phase with who does
/// what in it.
fn phases_of(workflow: &str) -> Vec<String> {
    let Some(written) = workflow_files::with_phases()
        .find(|(name, _)| *name == workflow)
        .and_then(|(name, text)| workflow_files::phases(text, name).ok().flatten())
    else {
        return Vec::new();
    };
    let mut lines = vec![written.description.clone()];
    lines.extend(written.phases.iter().enumerate().map(|(index, phase)| {
        format!("{} {}  {}", index + 1, phase.name, phase.detail)
            .trim_end()
            .to_owned()
    }));
    lines
}

/// Whether the workflow `name` seats a council as the next run reads it, by your file's phases
/// or else the built-in's, and the file of yours it read. A steps file of yours runs by its
/// steps, which never talk.
fn talks_reading(ctx: &Ctx, name: &str) -> (bool, Option<PathBuf>) {
    let phases = |text: &str| workflow_files::phases(text, name).ok().flatten();
    let (yours, read) = match library(ctx).find(name) {
        Some((_, Some(path))) if path.extension().is_none_or(|ext| ext != "toml") => {
            return (false, Some(path));
        }
        Some((_, Some(path))) => {
            let found = std::fs::read_to_string(&path).ok().and_then(|t| phases(&t));
            (found, Some(path))
        }
        _ => (None, None),
    };
    let talks = yours
        .or_else(|| workflow_files::built_in(name).and_then(phases))
        .is_some_and(|workflow| workflow.talks());
    (talks, read)
}

/// Whether the workflow in use talks, worked out again only once the settings, the workflow
/// folders or the file it read change: working it out reads the trust state, looks through the
/// folders and parses a file, and every turn's start asks.
#[derive(Debug, Default)]
pub(crate) struct Talking(std::sync::Mutex<Option<(Stamp, bool)>>);

/// What a [`Talking`] answer was worked out from: the settings' hash, the workflow, and when
/// the workflow folders and the file it read last changed.
#[derive(Debug, PartialEq)]
pub(crate) struct Stamp {
    settings: String,
    workflow: String,
    changed: Vec<Option<SystemTime>>,
    read: Option<PathBuf>,
}

impl Stamp {
    fn now(ctx: &Ctx, workflow: &str, read: Option<PathBuf>) -> Self {
        let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let project = ctx.env.repo_root.join(".argonauts/workflows");
        let yours = ctx.env.paths.global_workflows();
        let mut changed = vec![modified(&project), modified(&yours)];
        changed.extend(read.as_deref().map(modified));
        Self {
            settings: ctx.settings.sha256().to_owned(),
            workflow: workflow.to_owned(),
            changed,
            read,
        }
    }
}

impl Talking {
    pub(crate) fn talks(&self, ctx: &Ctx, name: &str) -> bool {
        let mut known = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((stamp, talks)) = known.as_ref()
            && *stamp == Stamp::now(ctx, name, stamp.read.clone())
        {
            return *talks;
        }
        let (talks, read) = talks_reading(ctx, name);
        *known = Some((Stamp::now(ctx, name, read), talks));
        talks
    }
}

/// A built-in with phases of its own and no steps, as Council talks in its phases, runs a
/// request by them. A file of yours that only sets settings has nothing to run by itself.
fn runs_by_its_phases(workflow: &str, err: &LibraryError) -> bool {
    matches!(err, LibraryError::NoSteps { .. })
        && workflow_files::with_phases().any(|(name, _)| name == workflow)
}

/// `s` in the workflow view of a run whose steps the runner ran: the script the orchestrator
/// wrote goes among your workflows under the run's name, so `/<name>` runs it again. `None`
/// for a run the runner didn't run, whose phases the caller saves as before.
pub(crate) fn save_script(
    ctx: &Ctx,
    agent_id: &CliCommands,
    run: &RunId,
    events: &[RunEvent],
    workflow: Option<&WorkflowId>,
    name: &str,
) -> Option<anyhow::Result<Vec<Effect>>> {
    let (id, followed) = runner_ran(events, workflow)?;
    let folder = ctx.env.run_dir(run).join("workflow").join(id.as_str());
    // `script.js`, then `script-2.js` and on as you sent it back: the last is what ran.
    let newest = (2..)
        .map(|n| folder.join(format!("script-{n}.js")))
        .take_while(|path| path.is_file())
        .last()
        .unwrap_or_else(|| folder.join("script.js"));
    let source = std::fs::read_to_string(newest).unwrap_or_default();
    if source.trim().is_empty() {
        // Nothing the orchestrator wrote was used: the workflow's own steps ran as written.
        ctx.notice(
            NoteLevel::Info,
            format!(
                "This run followed the steps of {followed} as written, so /{followed} <request> \
                 runs them again."
            ),
        );
        return Some(Ok(Vec::new()));
    }
    Some(write_script(ctx, agent_id, name, &source))
}

/// The id of the workflow the runner ran, and the name of the workflow it followed.
fn runner_ran(events: &[RunEvent], workflow: Option<&WorkflowId>) -> Option<(WorkflowId, String)> {
    let started = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Workflow(WorkflowEvent::Started(started))
            if workflow.map_or(started.parent.is_none(), |id| *id == started.id) =>
        {
            Some(started)
        }
        _ => None,
    })?;
    let from = started.from.as_ref()?;
    if from.as_str() != argonauts_crew::script::PHASES_NAME {
        return None;
    }
    let followed = events.iter().find_map(|event| match &event.kind {
        RunEventKind::Run(RunLifecycle::Started(run)) => Some(run.workflow.clone()),
        _ => None,
    })?;
    Some((started.id.clone(), followed))
}

/// Writes the script among your workflows under the first free name made from `name`.
fn write_script(
    ctx: &Ctx,
    agent_id: &CliCommands,
    name: &str,
    source: &str,
) -> anyhow::Result<Vec<Effect>> {
    // Every command's name is taken, the CLI's own as well as Argonauts Code's.
    let taken = taken(agent_id);
    let ours: Vec<&str> = taken.iter().map(String::as_str).collect();
    let base: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(32)
        .collect();
    let base = match base.trim_matches('-') {
        "" => "saved".to_owned(),
        trimmed => trimmed.to_owned(),
    };
    let library = library(ctx);
    let free = (1..)
        .map(|n| match n {
            1 => base.clone(),
            n => format!("{base}-{n}"),
        })
        .find(|candidate| {
            argonauts_runner::check_name(candidate, &ours).is_ok()
                && library.find(candidate).is_none()
        })
        .unwrap_or(base);
    let dir = ctx.env.paths.global_workflows();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{free}.js"));
    std::fs::write(&path, source)?;
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Saved the script this run followed as {free} in {}. /{free} <request> runs it \
             again, and /workflow edit {free} opens it.",
            ctx.env.display(&path)
        ),
    );
    Ok(vec![Effect::CommandsChanged])
}

/// The models a role has, in order: the role's own list where the settings give it one.
fn role_models(ctx: &Ctx, role: CrewRole) -> Vec<ModelRef> {
    let settings = ctx.settings();
    let own = settings
        .roles
        .get(&role.fine_role())
        .map(|role| role.models.clone())
        .filter(|workers| !workers.is_empty() && role != CrewRole::Main);
    own.unwrap_or_else(|| settings.models_for(role))
}

/// The dry run as aligned columns: step, role, who would take it, how.
fn table(ctx: &Ctx, dry: &DryRun) -> String {
    let who = |row: &Row| who_would_take(ctx, row);
    let mut rows = vec![[
        "step".to_owned(),
        "role".to_owned(),
        "who would take it".to_owned(),
        "how".to_owned(),
    ]];
    for row in &dry.rows {
        let mark = row
            .number
            .map_or_else(|| "★".to_owned(), |number| number.to_string());
        rows.push([
            format!("{mark} {}", row.step),
            row.role.plural().to_owned(),
            who(row),
            row.how.clone(),
        ]);
    }
    let width = |column: usize| {
        rows.iter()
            .map(|row| row[column].width())
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2)];
    rows.iter()
        .map(|row| {
            let mut line = String::from("  ");
            for (cell, width) in row.iter().zip(widths) {
                line.push_str(cell);
                line.push_str(&" ".repeat(width - cell.width() + 3));
            }
            line.push_str(&row[3]);
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The whole plan, a step to a row: its number and name, its role, who would take it (one to
/// a line when several would), and what it does with how it runs, such as `one per task · at
/// most 3 rounds`.
fn plan(ctx: &Ctx, file: &argonauts_runner::WorkflowFile, dry: &DryRun) -> Vec<String> {
    let rows: Vec<[String; 2]> = dry
        .rows
        .iter()
        .map(|row| {
            let mark = row
                .number
                .map_or_else(|| "★".to_owned(), |number| number.to_string());
            [format!("{mark} {}", row.step), row.role.plural().to_owned()]
        })
        .collect();
    let who: Vec<Vec<String>> = dry
        .rows
        .iter()
        .map(|row| {
            let count = row.who.len();
            row.who
                .iter()
                .enumerate()
                .map(|(at, worker)| {
                    // A worker that names no model runs its CLI's, which the plan names.
                    let name = ctx.names.worker(worker);
                    if at + 1 < count {
                        format!("{name},")
                    } else {
                        name
                    }
                })
                .collect()
        })
        .collect();
    let widest = |column: usize| rows.iter().map(|row| row[column].width()).max();
    let step_width = widest(0).unwrap_or(0);
    let role_width = widest(1).unwrap_or(0);
    let who_width = who
        .iter()
        .flatten()
        .map(|name| name.width())
        .max()
        .unwrap_or(0);
    let pad = |cell: &str, width: usize| format!("{cell}{}", " ".repeat(width - cell.width() + 3));
    let mut lines = Vec::new();
    for (index, (row, dry_row)) in rows.iter().zip(&dry.rows).enumerate() {
        let does = file
            .steps
            .get(index)
            .and_then(|step| step.does.as_deref())
            .map(what_it_does)
            .filter(|does| !does.is_empty());
        let mut what = does.map_or_else(
            || dry_row.how.clone(),
            |does| format!("{does} · {}", dry_row.how),
        );
        if let Some(rounds) = rounds_of_returns(ctx, file, index) {
            what.push_str(&format!(" · up to {rounds} rounds"));
        }
        let names = match who[index].as_slice() {
            [] => vec![String::new()],
            names => names.to_vec(),
        };
        for (at, name) in names.iter().enumerate() {
            let (step, role, what) = if at == 0 {
                (row[0].as_str(), row[1].as_str(), what.as_str())
            } else {
                ("", "", "")
            };
            let line = format!(
                "{}{}{}{what}",
                pad(step, step_width),
                pad(role, role_width),
                pad(name, who_width)
            );
            lines.push(line.trim_end().to_owned());
        }
    }
    lines
}

/// A step a later one sends back to, with no rounds of its own set, runs the first time and
/// then once for each round of changes the reviews may ask for.
fn rounds_of_returns(
    ctx: &Ctx,
    file: &argonauts_runner::WorkflowFile,
    index: usize,
) -> Option<u32> {
    let step = file.steps.get(index)?;
    let returned_to = file
        .steps
        .iter()
        .any(|other| other.send_back_to.as_deref() == Some(step.name.as_str()));
    (returned_to && step.max_rounds.is_none()).then(|| 1 + ctx.settings().review.max_change_rounds)
}

/// A step's ask in a few words: its first sentence, with `the request` and `the task` for
/// what each run fills in. A request pasted after a colon goes, since the preview's title
/// already says it.
fn what_it_does(does: &str) -> String {
    const MOST: usize = 48;
    let mut said = does.to_owned();
    for (field, words) in [("{request}", "the request"), ("{task}", "the task")] {
        said = said
            .replace(&format!("for: {field}"), &format!("for {words}"))
            .replace(&format!(": {field}"), "")
            .replace(field, words);
    }
    let first = said
        .split_inclusive(". ")
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches(['.', ':'])
        .trim();
    if first.width() <= MOST {
        return first.to_owned();
    }
    let mut cut = String::new();
    for c in first.chars() {
        if cut.width() + c.to_string().width() >= MOST {
            break;
        }
        cut.push(c);
    }
    format!("{}…", cut.trim_end())
}

/// What runs after the steps: the checks in your settings by name, then the merge.
fn checks_then_merge(ctx: &Ctx) -> String {
    let settings = ctx.settings();
    let names: Vec<&str> = settings
        .checks
        .run
        .iter()
        .map(|check| check.name.trim())
        .filter(|name| !name.is_empty())
        .collect();
    match names.as_slice() {
        [] if settings.checks.run.is_empty() => "then merge".to_owned(),
        [] => "then the checks in your settings · then merge".to_owned(),
        few if few.len() <= 3 => format!("then the checks: {} · then merge", few.join(", ")),
        _ => "then the checks in your settings · then merge".to_owned(),
    }
}

/// Who would take a step: one by its full name, several by their models alone, since they
/// share a cell.
fn who_would_take(ctx: &Ctx, row: &Row) -> String {
    match row.who.as_slice() {
        [one] => ctx.names.worker(one),
        many => many
            .iter()
            .map(|worker| model_alone(ctx, worker))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// `Gemini 3.6 Pro` of `Gemini 3.6 Pro on agy`.
fn model_alone(ctx: &Ctx, worker: &ModelRef) -> String {
    let full = ctx.names.worker(worker);
    let cli = ctx.names.cli(&worker.agent);
    full.strip_suffix(&format!(" on {cli}"))
        .map(str::to_owned)
        .unwrap_or(full)
}

#[cfg(test)]
mod tests {
    /// Every built-in workflow, as the first run writes it into your workflows folder, reads
    /// back as a workflow its run can start: the settings tables it carries, `[goal]` among
    /// them, are left to the settings.
    #[test]
    fn every_built_in_workflow_written_out_reads_back_as_a_workflow() {
        for workflow in argonauts_registry::workflow_choices() {
            let text = argonauts_registry::builtin_workflow(&workflow.name).expect("a built-in");
            let read = argonauts_runner::WorkflowFile::parse(&text);
            assert!(read.is_ok(), "{}: {read:?}", workflow.name);
        }
    }

    use argonauts_core::RunId;
    use argonauts_core::app::workflow_page::WorkflowPage;
    use argonauts_core::app::{AnswerTarget, AppChoice, AppEvent, UiCommand, ViewData};
    use argonauts_core::input::SlashCommand;
    use argonauts_core::runs::api::{RunState, RunSummary};
    use argonauts_core::runs::events::run::RunMode;

    use super::*;
    use crate::controller::base::test_support::{CliOptions, TestCtx, TestDriver};

    async fn said(driver: &mut TestDriver, command: SlashCommand) -> (NoteLevel, String) {
        driver.send(UiCommand::Slash { command }).await;
        match driver
            .until(|event| matches!(event, AppEvent::Notice { .. }))
            .await
        {
            AppEvent::Notice { level, text } => (level, text),
            _ => unreachable!("only a notice ends the wait"),
        }
    }

    /// Writes a workflow, waiting for the line that says so.
    async fn wrote(driver: &mut TestDriver, command: SlashCommand) {
        driver.send(UiCommand::Slash { command }).await;
        driver
            .until(|event| matches!(event, AppEvent::Record { .. }))
            .await;
    }

    fn new(name: &str, from: Option<&str>) -> SlashCommand {
        SlashCommand::WorkflowNew {
            name: name.to_owned(),
            from: from.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn new_writes_a_workflow_that_becomes_a_command_of_its_name() {
        let mut driver = TestDriver::start().await;
        driver
            .send(UiCommand::Slash {
                command: new("spec-first", None),
            })
            .await;
        // What was written and where, then the three commands, in two lines.
        let wrote = driver
            .until(|event| matches!(event, AppEvent::Record { .. }))
            .await;
        assert_eq!(
            wrote,
            AppEvent::Record {
                title: "Wrote spec-first".to_owned(),
                tail: Some(".argonauts/workflows/spec-first.toml".to_owned()),
                lines: Vec::new(),
                notes: vec![
                    "/spec-first <request> runs it once · /workflow spec-first uses it · \
                     /workflow try previews it"
                        .to_owned()
                ],
            }
        );
        let file = driver
            .repo
            .root()
            .join(".argonauts/workflows/spec-first.toml");
        assert!(
            std::fs::read_to_string(&file)
                .unwrap()
                .contains("name  = \"spec-first\"")
        );

        // The `/` menu hears of it, with its one line.
        let listed = driver
            .until(|event| matches!(event, AppEvent::CliCommands { .. }))
            .await;
        let AppEvent::CliCommands { set } = listed else {
            unreachable!("only the command set ends the wait");
        };
        let yours: Vec<(&str, &str, &str)> = set
            .workflows()
            .map(|command| {
                (
                    command.name.as_str(),
                    command.args.as_str(),
                    command.help.as_str(),
                )
            })
            .collect();
        assert_eq!(
            yours,
            [(
                "spec-first",
                "[request]",
                "One line that says what this workflow is for."
            )]
        );

        // A name a command already has, and a name already taken.
        let (level, taken) = said(&mut driver, new("model", None)).await;
        assert_eq!(level, NoteLevel::Warn);
        assert!(taken.starts_with("/model is already a command"), "{taken}");
        let (_, twice) = said(&mut driver, new("spec-first", None)).await;
        assert!(
            twice.contains("already a workflow named spec-first"),
            "{twice}"
        );
        driver.exit().await;
    }

    /// The plan the preview leaves in the conversation, and the question that asks under it.
    async fn tried(driver: &mut TestDriver, name: &str, request: &str) -> (Planned, AppChoice) {
        let command = SlashCommand::WorkflowTry {
            name: name.to_owned(),
            request: request.to_owned(),
        };
        driver.send(UiCommand::Slash { command }).await;
        let planned = match driver
            .until(|event| matches!(event, AppEvent::Record { .. }))
            .await
        {
            AppEvent::Record {
                title,
                tail,
                lines,
                notes,
            } => Planned {
                title,
                tail,
                lines,
                notes,
            },
            _ => unreachable!("only a record ends the wait"),
        };
        match driver
            .until(|event| matches!(event, AppEvent::Choice { .. }))
            .await
        {
            AppEvent::Choice { choice } => (planned, choice),
            _ => unreachable!("only a choice ends the wait"),
        }
    }

    struct Planned {
        title: String,
        tail: Option<String>,
        lines: Vec<String>,
        notes: Vec<String>,
    }

    async fn closed(driver: &mut TestDriver) -> String {
        match driver
            .until(|event| matches!(event, AppEvent::ChoiceClosed { .. }))
            .await
        {
            AppEvent::ChoiceClosed { outcome, .. } => outcome,
            _ => unreachable!("only a closed choice ends the wait"),
        }
    }

    /// Council runs by phases of its own, so a try of it lists them, and nothing runs.
    #[tokio::test]
    async fn trying_council_lists_its_own_phases() {
        let mut driver = TestDriver::start().await;
        let (planned, question) = tried(&mut driver, "council", "add due dates").await;
        assert_eq!(planned.title, "council for \"add due dates\"");
        assert_eq!(question.title, "Run it now?");
        assert!(
            planned.lines[0].starts_with("No single planner"),
            "{:?}",
            planned.lines
        );
        assert!(
            planned.lines.iter().any(|line| line.starts_with("1 Talk")),
            "{:?}",
            planned.lines
        );
        assert!(driver.conductor.starts().is_empty(), "nothing runs");
        driver.exit().await;
    }

    #[tokio::test]
    async fn try_shows_each_step_and_who_would_take_it_and_runs_nothing() {
        let mut driver = TestDriver::start().await;
        wrote(&mut driver, new("spec-first", None)).await;
        let (planned, question) = tried(&mut driver, "spec-first", " add due dates ").await;
        assert_eq!(question.kind, ChoiceKind::TryWorkflow);
        assert_eq!(planned.title, "spec-first for \"add due dates\"");
        assert_eq!(planned.tail.as_deref(), Some("a preview, nothing runs yet"));
        // The whole plan: each step with its role, who would take it, what it does and how,
        // then what runs after the steps; the question only asks.
        assert_eq!(question.title, "Run it now?");
        assert!(question.body.is_empty());
        let lines = &planned.lines;
        let step = |at: usize, starts: &str, role: &str, ends: &str| {
            let line = &lines[at];
            assert!(
                line.starts_with(starts) && line.contains(role) && line.ends_with(ends),
                "{lines:#?}"
            );
        };
        step(0, "1 Plan", "orchestrator", "· 1 agent");
        step(1, "2 Build", "workers", "one per task · up to 3 rounds");
        step(
            2,
            "3 Review",
            "reviewers",
            "one per task · sends back to Build",
        );
        assert!(
            planned
                .notes
                .iter()
                .any(|line| line.ends_with("then merge")),
            "{:#?}",
            planned.notes
        );
        let offered: Vec<(char, &str, bool)> = question
            .options
            .iter()
            .map(|option| (option.key, option.label.as_str(), option.recommended))
            .collect();
        assert_eq!(offered, [('y', "Yes, run it", false), ('n', "No", true)]);
        assert!(driver.conductor.starts().is_empty(), "nothing runs");

        // Esc closes it, and still nothing ran.
        driver
            .send(UiCommand::SetAside {
                to: AnswerTarget::Choice { id: question.id },
            })
            .await;
        assert_eq!(
            closed(&mut driver).await,
            "Spec-first preview for 'add due dates' · nothing ran"
        );
        assert!(driver.conductor.starts().is_empty());

        let (level, missing) = said(
            &mut driver,
            SlashCommand::WorkflowTry {
                name: "nope".to_owned(),
                request: "x".to_owned(),
            },
        )
        .await;
        assert_eq!(level, NoteLevel::Warn);
        assert!(missing.contains("no workflow named nope"), "{missing}");
        driver.exit().await;
    }

    #[tokio::test]
    async fn run_it_on_a_dry_run_runs_that_one_request_with_the_workflow() {
        let mut driver = TestDriver::start().await;
        wrote(&mut driver, new("mine", Some("maker-checker"))).await;
        let (planned, question) = tried(&mut driver, "mine", "add due dates").await;
        assert!(
            planned
                .notes
                .iter()
                .any(|line| line.starts_with("adapt is on, so the orchestrator writes the steps")),
            "{:#?}",
            planned.notes
        );
        driver
            .send(UiCommand::Answer {
                to: AnswerTarget::Choice { id: question.id },
                action: AnswerAction::Approve,
                text: None,
                evidence_sha256: None,
                tasks: Vec::new(),
                remember: false,
            })
            .await;
        assert_eq!(
            closed(&mut driver).await,
            "Mine preview for 'add due dates' · running it"
        );
        driver
            .until(|event| matches!(event, AppEvent::Attached { .. }))
            .await;
        let started = driver.conductor.starts();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].request, "add due dates");
        assert_eq!(started[0].request_settings, ["workflow=\"mine\""]);
        driver.exit().await;
    }

    #[tokio::test]
    async fn a_workflows_own_command_runs_one_request_with_it() {
        let mut driver = TestDriver::start().await;
        wrote(&mut driver, new("mine", Some("maker-checker"))).await;
        driver
            .send(UiCommand::Slash {
                command: SlashCommand::WorkflowRequest {
                    workflow: "mine".to_owned(),
                    request: " add due dates ".to_owned(),
                },
            })
            .await;
        driver
            .until(|event| matches!(event, AppEvent::Attached { .. }))
            .await;
        let started = driver.conductor.starts();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].request, "add due dates");
        assert_eq!(started[0].mode, RunMode::Crew);
        assert_eq!(started[0].request_settings, ["workflow=\"mine\""]);
        // The workflow in use stays as it was: nothing is written to your settings.
        let global = std::fs::read_to_string(driver.env.paths.home().join("config.toml"));
        assert_eq!(
            global.unwrap(),
            crate::controller::base::test_support::SET_UP_GLOBAL
        );
        driver.exit().await;
    }

    fn ask(workflow: &str, request: &str) -> UiCommand {
        UiCommand::Slash {
            command: SlashCommand::WorkflowRequest {
                workflow: workflow.to_owned(),
                request: request.to_owned(),
            },
        }
    }

    fn going(run: &str, workflow: &str) -> RunSummary {
        RunSummary {
            run: RunId::new(run).unwrap(),
            title: format!("{run} title"),
            mode: RunMode::Crew,
            workflow: workflow.to_owned(),
            state: RunState::Running,
            stopped_at: String::new(),
            updated: time::OffsetDateTime::UNIX_EPOCH,
            spent: argonauts_core::Spend::default(),
            sessions_to_resume: 0,
            waiting: Vec::new(),
            live: true,
            remind: false,
            name: None,
            shape_known: true,
        }
    }

    /// A second workflow started while the first works takes the screen, and the first goes
    /// on with a row of its own at once, not at the next minute's listing.
    #[tokio::test]
    async fn two_workflows_run_at_once_each_with_its_own_row() {
        let mut driver = TestDriver::start().await;
        wrote(&mut driver, new("mine", Some("maker-checker"))).await;
        wrote(&mut driver, new("other", Some("maker-checker"))).await;
        driver.send(ask("mine", "add due dates")).await;
        driver
            .until(|event| matches!(event, AppEvent::Attached { .. }))
            .await;
        driver.conductor.set_runs(vec![going("run-001", "mine")]);

        driver.send(ask("other", "fix the median")).await;
        driver
            .until(|event| matches!(event, AppEvent::Attached { summary, .. } if summary.run.as_str() == "run-002"))
            .await;
        let rows = driver
            .until(|event| matches!(event, AppEvent::BackgroundRuns { runs } if !runs.is_empty()))
            .await;
        let AppEvent::BackgroundRuns { runs } = rows else {
            unreachable!("only the rows end the wait");
        };
        let rows: Vec<&str> = runs.iter().map(|run| run.run.as_str()).collect();
        assert_eq!(rows, ["run-001"]);
        let started = driver.conductor.starts();
        let workflows: Vec<&[String]> = started
            .iter()
            .map(|start| start.request_settings.as_slice())
            .collect();
        assert_eq!(
            workflows,
            [
                ["workflow=\"mine\"".to_owned()],
                ["workflow=\"other\"".to_owned()]
            ]
        );
        driver.exit().await;
    }

    async fn page(driver: &mut TestDriver) -> WorkflowPage {
        match driver
            .until(|event| {
                matches!(event, AppEvent::View { data }
                    if matches!(data.as_ref(), ViewData::WorkflowPage { .. }))
            })
            .await
        {
            AppEvent::View { data } => match *data {
                ViewData::WorkflowPage { page } => *page,
                _ => unreachable!("only the page ends the wait"),
            },
            _ => unreachable!("only a view ends the wait"),
        }
    }

    fn edit(name: &str) -> UiCommand {
        UiCommand::Slash {
            command: SlashCommand::WorkflowEdit {
                name: name.to_owned(),
            },
        }
    }

    fn on_page(name: &str, change: WorkflowChange) -> UiCommand {
        UiCommand::WorkflowPage {
            name: name.to_owned(),
            change,
        }
    }

    fn set(step: Option<u32>, key: &str, value: &str) -> WorkflowChange {
        WorkflowChange::Set {
            step,
            key: key.to_owned(),
            value: value.to_owned(),
        }
    }

    #[tokio::test]
    async fn edit_opens_the_page_with_each_step_and_who_would_take_it() {
        let mut driver = TestDriver::start().await;
        wrote(&mut driver, new("spec-first", None)).await;
        driver.send(edit("spec-first")).await;
        let page = page(&mut driver).await;
        assert_eq!(page.path, ".argonauts/workflows/spec-first.toml");
        assert_eq!(page.about, "One line that says what this workflow is for.");
        assert!(!page.refresh);
        assert!(page.check.valid, "{:?}", page.check);
        assert!(
            page.check.lines[0].starts_with("the file is valid · about "),
            "{:?}",
            page.check
        );
        let titles: Vec<&str> = page
            .groups
            .iter()
            .map(|group| group.title.as_str())
            .collect();
        assert_eq!(titles, ["Workflow", "Plan", "Build", "Review"]);
        assert_eq!(
            page.groups[0].how,
            "the steps run exactly as written · no goal"
        );
        let review = &page.groups[3];
        assert_eq!(review.step, Some(2));
        assert_eq!(review.role, "reviewer");
        assert!(
            !review.who.is_empty(),
            "the dry run names who would take it"
        );
        assert!(review.how.contains("sends back to Build"), "{}", review.how);
        let field = |key: &str| review.fields.iter().find(|field| field.key == key).unwrap();
        assert_eq!(field("send_back_to").value, "Build");
        // A step may point at any other step, and at none.
        assert_eq!(field("send_back_to").choices, ["Plan", "Build"]);
        assert!(field("send_back_to").optional);
        assert_eq!(
            field("role").choices,
            ["orchestrator", "worker", "reviewer", "judge", "main"]
        );
        // A flag nobody wrote reads false.
        assert_eq!(field("independent").value, "false");
        assert_eq!(field("until").value, "");

        let (_, built_in) = said(
            &mut driver,
            SlashCommand::WorkflowEdit {
                name: "council".to_owned(),
            },
        )
        .await;
        assert!(
            built_in.contains("/workflow new my-council from council"),
            "{built_in}"
        );
        driver.exit().await;
    }

    #[tokio::test]
    async fn a_change_on_the_page_is_checked_and_saved_with_the_comments_kept() {
        let mut driver = TestDriver::start().await;
        wrote(&mut driver, new("spec-first", None)).await;
        let file = driver
            .repo
            .root()
            .join(".argonauts/workflows/spec-first.toml");
        let before = std::fs::read_to_string(&file).unwrap();
        driver.send(edit("spec-first")).await;
        page(&mut driver).await;

        driver
            .send(on_page(
                "spec-first",
                set(Some(1), "until", "its own tests pass"),
            ))
            .await;
        let saved = page(&mut driver).await;
        assert!(saved.refresh);
        assert_eq!(saved.said.as_deref(), Some("Saved."));
        assert!(
            saved.groups[2].how.contains("until its own tests pass"),
            "{}",
            saved.groups[2].how
        );
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("until = \"its own tests pass\""), "{text}");
        assert!(
            text.contains("# one agent per task, side by side"),
            "{text}"
        );

        // One that would stop the steps from running is refused, and the file stays.
        driver
            .send(on_page("spec-first", set(Some(2), "send_back_to", "Biuld")))
            .await;
        let refused = page(&mut driver).await;
        let why = refused.said.unwrap();
        assert!(
            why.starts_with("Not saved: step 3 (Review): send_back_to names Biuld"),
            "{why}"
        );
        assert!(refused.check.valid);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text);

        // The file's own keys change the same way, and steps are added, moved and removed.
        driver
            .send(on_page("spec-first", set(None, "about", "Spec first.")))
            .await;
        assert_eq!(page(&mut driver).await.about, "Spec first.");
        driver
            .send(on_page(
                "spec-first",
                WorkflowChange::AddStep { after: Some(2) },
            ))
            .await;
        let added = page(&mut driver).await;
        assert_eq!(added.groups[4].title, "Step 4");
        driver
            .send(on_page(
                "spec-first",
                WorkflowChange::MoveStep { step: 3, up: true },
            ))
            .await;
        let moved = page(&mut driver).await;
        assert_eq!(moved.groups[3].title, "Step 4");
        assert_eq!(moved.groups[4].title, "Review");
        driver
            .send(on_page(
                "spec-first",
                WorkflowChange::RemoveStep { step: 2 },
            ))
            .await;
        let removed = page(&mut driver).await;
        assert_eq!(removed.groups.len(), 4);
        assert_ne!(std::fs::read_to_string(&file).unwrap(), before);

        // The page's own saves are shown on the page, so the conversation hears of none of them.
        tokio::time::sleep(LOOK_EVERY * 3).await;
        let events = driver.exit().await;
        let told = events
            .iter()
            .any(|event| matches!(event, AppEvent::Notice { text, .. } if text.contains("saved")));
        assert!(!told, "a save from the page needs no notice");
    }

    /// Each turn's start asks whether the workflow in use talks: the answer stands until the
    /// settings or a workflow file change, and then it is worked out again.
    #[tokio::test]
    async fn whether_a_workflow_talks_is_worked_out_again_once_a_file_changes() {
        let (test, _) = TestCtx::new(CliOptions::default());
        let talking = Talking::default();
        assert!(talking.talks(&test.ctx, "council"));
        assert!(talking.talks(&test.ctx, "council"));
        let dir = test.ctx.env.paths.global_workflows();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("council.js"), "// steps of yours\n").unwrap();
        assert!(
            !talking.talks(&test.ctx, "council"),
            "a script of yours runs by its steps, which never talk"
        );
    }

    #[tokio::test]
    async fn a_save_of_the_pages_noticed_twice_is_told_to_nobody() {
        let (mut test, _) = TestCtx::new(CliOptions::default());
        let dir = test.ctx.env.repo_root.join(".argonauts/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mine.toml");
        let own = "[[step]]\nname = \"Do\"\nrole = \"worker\"\n";
        let mut state = State::default();
        std::fs::write(&file, own).unwrap();
        state.wrote = Some(own.to_owned());

        for _ in 0..2 {
            assert!(saved(&test.ctx, &mut state, "mine").unwrap().is_empty());
        }
        assert!(
            test.events.try_recv().is_err(),
            "a save from the page needs no notice"
        );

        // A save from outside is told, and so is one that puts the page's own text back.
        for text in ["[[step]]\nname = \"Do\"\nrole = \"judge\"\n", own] {
            std::fs::write(&file, text).unwrap();
            assert!(!saved(&test.ctx, &mut state, "mine").unwrap().is_empty());
            let sent: Vec<AppEvent> = std::iter::from_fn(|| test.events.try_recv().ok()).collect();
            let told = sent.iter().any(
                |event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("mine saved")),
            );
            assert!(told, "{sent:?}");
        }
    }

    #[tokio::test]
    async fn a_save_from_outside_is_checked_on_the_page_and_in_the_conversation() {
        let mut driver = TestDriver::start().await;
        let dir = driver.repo.root().join(".argonauts/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("broken.toml");
        std::fs::write(&file, "[[step]]\nname = \"Do\"\n").unwrap();
        driver.send(edit("broken")).await;
        let opened = page(&mut driver).await;
        assert!(!opened.check.valid);
        assert_eq!(opened.check.lines, ["step 1 (Do): it needs a role"]);
        // What is written shows all the same, so it can be mended here.
        assert_eq!(opened.groups[1].title, "Do");
        assert_eq!(opened.groups[1].role, "");

        // Saved with its role from an editor, the file is checked again without being asked.
        std::fs::write(&file, "[[step]]\nname = \"Do\"\nrole = \"worker\"\n").unwrap();
        let fresh = page(&mut driver).await;
        assert!(fresh.refresh);
        assert!(fresh.check.valid, "{:?}", fresh.check);
        assert_eq!(fresh.said.as_deref(), Some("Saved from outside."));
        let fixed = driver
            .until(|event| matches!(event, AppEvent::Notice { text, .. } if text.starts_with("broken saved")))
            .await;
        let AppEvent::Notice { text, .. } = fixed else {
            unreachable!("only the notice ends the wait");
        };
        assert!(text.contains("1 Do"), "{text}");
        assert!(text.ends_with("✓ the file is valid"), "{text}");

        // A broken file is mended on the page a key at a time.
        std::fs::write(&file, "[[step]]\nname = \"Do\"\n").unwrap();
        page(&mut driver).await;
        driver
            .send(on_page("broken", set(Some(0), "role", "judge")))
            .await;
        let mended = page(&mut driver).await;
        assert!(mended.check.valid, "{:?}", mended.check);
        assert_eq!(mended.groups[1].role, "judge");
        driver.exit().await;
    }

    #[tokio::test]
    async fn e_on_the_page_hands_the_file_to_your_editor() {
        let (mut test, _) = TestCtx::new(CliOptions::default());
        let file = library(&test.ctx)
            .create("spec-first", None, &[])
            .expect("writes the template");
        let mut state = State::default();
        let editor = vec!["code".to_owned(), "--wait".to_owned()];
        let open = WorkflowChange::OpenInEditor;
        page::change_with(
            &test.ctx,
            &mut state,
            "spec-first",
            open.clone(),
            Some(editor),
        )
        .unwrap();
        let argv = loop {
            let event = test.events.recv().await.expect("the handover");
            if let AppEvent::RunInTerminal { argv, .. } = event {
                break argv;
            }
        };
        assert_eq!(argv, ["code", "--wait", &file.display().to_string()]);

        // Without an editor set, the page says how to get one.
        page::change_with(&test.ctx, &mut state, "spec-first", open, None).unwrap();
        let said = loop {
            let AppEvent::View { data } = test.events.recv().await.expect("the page again") else {
                continue;
            };
            if let ViewData::WorkflowPage { page } = *data {
                break page.said;
            }
        };
        assert!(said.unwrap().contains("$VISUAL or $EDITOR"));
    }

    const SWEEP: &str = "export const meta = { name: 'sweep', description: 'Find and fix', \
                         phases: [{ name: 'Find' }, { name: 'Fix' }] }\n\
                         await agent('Find what is wrong', { role: 'worker' })\n";

    #[tokio::test]
    async fn edit_hands_a_script_to_your_editor_and_watches_it() {
        let (mut test, _) = TestCtx::new(CliOptions::default());
        let dir = test.ctx.env.repo_root.join(".argonauts/workflows");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sweep.js");
        std::fs::write(&file, SWEEP).unwrap();
        let mut state = State::default();
        let editor = vec!["code".to_owned(), "--wait".to_owned()];
        edit_with(&test.ctx, &mut state, "sweep", Some(editor)).unwrap();
        let mut said = None;
        let argv = loop {
            match test.events.recv().await.expect("the handover") {
                AppEvent::RunInTerminal { argv, .. } => break argv,
                AppEvent::Notice { text, .. } => said = Some(text),
                _ => {}
            }
        };
        assert_eq!(argv, ["code", "--wait", &file.display().to_string()]);
        let said = said.expect("the check comes before the editor opens");
        assert!(said.contains("✓ the script reads: Find and fix"), "{said}");
        assert!(said.contains("Opening it in code."), "{said}");

        // A save reaches the controller, which checks the file again.
        std::fs::write(&file, format!("{SWEEP}\n// a note\n")).unwrap();
        let saved = tokio::time::timeout(Duration::from_secs(10), test.internal.recv()).await;
        assert!(
            matches!(saved, Ok(Some(Internal::WorkflowSaved { ref name })) if name == "sweep"),
            "the save was noticed"
        );
    }

    #[test]
    fn a_steps_ask_reads_in_a_few_words() {
        assert_eq!(
            what_it_does("Split the request into tasks: {request}"),
            "Split the request into tasks"
        );
        assert_eq!(
            what_it_does("Write a short spec and acceptance tests for: {request}. List the tasks."),
            "Write a short spec and acceptance tests for the…"
        );
        assert_eq!(what_it_does("Implement {task}."), "Implement the task");
    }
}
