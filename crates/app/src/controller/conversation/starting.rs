//! What the conversation's session holds at its start, before its CLI counts it: kept from the
//! last fresh session on the same CLI and model, or, on a very first run, worked out from the
//! CLI's own prompt and tools as recorded for its version and what this machine adds: the
//! instruction files, the CLI's skills, and Argonauts Code's own tools and the lines about them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use argonauts_core::app::AppEvent;
use argonauts_core::cli_commands::{CliCommandSet, CommandSource};
use argonauts_core::events::context::{
    ContextDetail, ContextKind, ContextUsage, FirstCount, NamedTokens, StartHolds, StartPart,
    StartedFrom, StartingCount, StartingTools,
};
use argonauts_core::names::ModelRef;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::prompt_parts::approx_tokens;
use argonauts_core::views::ContextRow;
use argonauts_core::{AgentEvent, NoteLevel};
use argonauts_mcp::session::main_tools;
use serde::Deserialize;

use super::{ConversationState, cli, standing, tools};
use crate::controller::Ctx;
use crate::controller::run_commands::window_figures;

/// Claude Code's colour names for the CLI's own part and for the instruction files, which the
/// other CLIs' parts borrow.
const OWN_COLOR: &str = "promptBorder";
const FILES_COLOR: &str = "claude";
const MESSAGES: &str = "Messages";
/// The colour of Argonauts Code's tools and the lines about them, for a CLI no recording ships
/// for.
const APP_TOOLS_COLOR: &str = "cyan_FOR_SUBAGENTS_ONLY";
/// How many imports deep Claude Code follows an instruction file's `@path` words.
const IMPORT_DEPTH: usize = 5;

#[derive(Debug, Deserialize)]
struct Baselines {
    cli: Vec<Baseline>,
}

/// A CLI's own prompt and tools at the version it was recorded on.
#[derive(Debug, Deserialize)]
struct Baseline {
    agent: String,
    version: String,
    model: Option<String>,
    window: Option<u64>,
    #[serde(default)]
    buffer: u64,
    parts: Vec<BaselinePart>,
    /// Where the CLI counts the instruction files, when it has a name of its own for them.
    files: Option<NamedPart>,
    /// Where it counts its skills, for a CLI that counts them apart.
    skills: Option<SkillsPart>,
    /// Where it counts Argonauts Code's tools, when apart from its own part.
    app_tools: Option<NamedPart>,
    /// Where it counts the lines about those tools, when apart from its own part.
    app_note: Option<NamedPart>,
    /// Whether it reads the files an instruction file imports with `@path`.
    #[serde(default)]
    follows_imports: bool,
    /// How many tools the CLI named at the recording's start, for a CLI that names them.
    tools: Option<u32>,
    /// How many of them Argonauts Code's read-only sessions offer, where it gives them fewer.
    read_only_tools: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct BaselinePart {
    /// None for the CLI's own prompt and tools, named after the CLI.
    name: Option<String>,
    tokens: u64,
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NamedPart {
    name: String,
    color: String,
}

#[derive(Debug, Deserialize)]
struct SkillsPart {
    name: String,
    color: String,
    /// The skills that come with the CLI, as recorded.
    #[serde(default)]
    built_in: u64,
}

/// The recordings, read once. A test holds the file to this shape.
static BASELINES: LazyLock<Vec<Baseline>> = LazyLock::new(|| {
    toml::from_str::<Baselines>(include_str!("starting_baselines.toml"))
        .map(|baselines| baselines.cli)
        .unwrap_or_default()
});

/// Which of a CLI's readings a fresh session's start is kept from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    /// Its count by category, from a CLI that can say what fills a session.
    ByCategory,
    /// Its first reading of how full the session is.
    HowFull,
}

impl Reading {
    pub(crate) fn of(ctx: &Ctx, agent: &str) -> Self {
        let counts = cli::adapter(ctx, agent)
            .is_some_and(|adapter| adapter.context_usage_line(ContextDetail::Summary).is_some());
        if counts {
            Self::ByCategory
        } else {
            Self::HowFull
        }
    }
}

/// A turn that starts its session afresh, until its start is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fresh {
    pub reading: Reading,
    /// When its CLI's own count first comes, where a recording says.
    pub first_count: Option<FirstCount>,
    /// The model the CLI said it runs, for a session left on the CLI's default.
    pub model: Option<String>,
    /// How many tools the CLI named as the session started, where it named any.
    pub tools: Option<u32>,
    /// What the CLI's first usage says its input held, cached tokens included.
    pub first_input: Option<u64>,
    /// A tool started before that first usage came.
    pub tool_before: bool,
    /// The session was asked for its count by category.
    pub asked: bool,
}

impl Fresh {
    pub(crate) fn of(ctx: &Ctx, agent: &str) -> Self {
        Self::reading(Reading::of(ctx, agent), first_count(ctx, agent))
    }

    fn reading(reading: Reading, first_count: Option<FirstCount>) -> Self {
        Self {
            reading,
            first_count,
            model: None,
            tools: None,
            first_input: None,
            tool_before: false,
            asked: false,
        }
    }

    /// Takes in what the CLI says as the session starts: its model, its tools and what its
    /// first request held. True when the session is to be asked for its count by category
    /// now: a CLI that can count has said how full it is for the first time, and its count
    /// comes only when something asks for it.
    pub(crate) fn heard(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::Init { model, tools, .. } => {
                self.model = self.model.take().or_else(|| model.clone());
                if !tools.is_empty() {
                    self.tools = Some(u32::try_from(tools.len()).unwrap_or(u32::MAX));
                }
            }
            AgentEvent::ToolStarted { .. } if self.first_input.is_none() => {
                self.tool_before = true;
            }
            AgentEvent::Usage { input_tokens, .. } if self.first_input.is_none() => {
                self.first_input = Some(*input_tokens);
            }
            AgentEvent::ContextFill { .. }
                if self.reading == Reading::ByCategory && !self.asked =>
            {
                self.asked = true;
                return true;
            }
            _ => {}
        }
        false
    }

    /// The first reading to keep, with `window` and what Argonauts Code `sent` beside it.
    /// None before the CLI's first usage, and for a CLI whose usage totals its whole turn
    /// once a tool ran before it, since the first request is then lost in the total.
    pub(crate) fn first_fill(&self, window: Option<u64>, sent: u64) -> Option<FirstFill> {
        let totals = self.first_count.is_some_and(FirstCount::totals_the_turn);
        let input = self.first_input.filter(|_| !(totals && self.tool_before))?;
        Some(FirstFill {
            input,
            window,
            sent,
            tools: self.tools,
        })
    }

    /// `worker` by the model its CLI runs.
    pub(crate) fn named(&self, worker: &ModelRef) -> ModelRef {
        let model = worker.model.clone().or_else(|| self.model.clone());
        ModelRef::new(worker.agent.as_str(), model)
    }
}

/// What a fresh session of the conversation gets beside its CLI's own prompt and tools,
/// measured here at a token per 4 characters, as Argonauts Code counts what it sends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Measured {
    /// The definitions of the tools Argonauts Code offers it.
    app_tools: u64,
    /// The lines about those tools at the end of its instructions.
    app_note: u64,
    /// The skills its CLI lists, by name and description.
    skills: u64,
}

impl Measured {
    /// For a session offered the tools named `offered`, whose CLI lists `skills` tokens of
    /// skills.
    fn of(offered: &[String], skills: u64) -> Self {
        let app_tools = main_tools::all()
            .iter()
            .filter(|tool| offered.iter().any(|name| name == tool.name))
            .map(|tool| {
                let schema = (tool.input_schema)().to_string();
                approx_tokens(tool.name) + approx_tokens(tool.description) + approx_tokens(&schema)
            })
            .sum();
        let app_note = standing::lines(offered).map_or(0, |note| approx_tokens(&note));
        Self {
            app_tools,
            app_note,
            skills,
        }
    }
}

/// The main session's start as it was last worked out, and what from, so the conversation
/// works it out again only when one of those changed.
#[derive(Debug, Default)]
pub(crate) struct KeptStart {
    from: Option<StartFrom>,
    start: Option<StartingCount>,
    /// Fresh starts still being kept, which the next start waits for.
    writing: Vec<tokio::task::JoinHandle<()>>,
    /// The start the screen was sent last, and whether its window was its CLI's own figure.
    told: Option<(StartingCount, bool)>,
}

impl KeptStart {
    pub(crate) fn keeping(&mut self, write: tokio::task::JoinHandle<()>) {
        self.writing.retain(|write| !write.is_finished());
        self.writing.push(write);
    }

    /// A fresh start is being kept, so the start the screen has may be out of date.
    pub(crate) fn kept_fresh(&self) -> bool {
        !self.writing.is_empty()
    }
}

/// What a start was worked out from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StartFrom {
    asked: Asked,
    /// Every file the start read or looked for, as it stood then.
    read: Vec<(PathBuf, Option<Stamp>)>,
}

/// When a file was last changed and how long it was, or `None` for one that wasn't there.
type Stamp = (std::time::SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// What the controller knows of a start without reading a file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    worker: ModelRef,
    /// The instruction files the CLI loads by itself, by name.
    files: Vec<String>,
    /// How many imports deep the CLI reads them.
    depth: usize,
    window: Option<u64>,
    first_count: Option<FirstCount>,
    /// The tools Argonauts Code offers the session, by name.
    offered: Vec<String>,
    /// The skills its CLI lists, by name and description.
    skills: u64,
}

/// A start to work out on a blocking task, since it reads the kept starts and the instruction
/// files.
#[derive(Debug, Clone)]
pub(crate) struct StartAsk {
    asked: Asked,
    kept: PathBuf,
    workdir: PathBuf,
}

impl StartAsk {
    /// The start of `worker`'s session in this conversation, if it started now: the tools it
    /// would be offered, and the skills `commands` lists for its CLI.
    pub(crate) fn here(
        ctx: &Ctx,
        conversation: &ConversationState,
        worker: &ModelRef,
        commands: Option<&CliCommandSet>,
    ) -> Self {
        let agent = worker.agent.as_str();
        let skills = commands
            .filter(|set| set.agent_id.0 == agent)
            .map_or(0, |set| {
                set.commands
                    .iter()
                    .filter(|command| matches!(command.source, CommandSource::Skill { .. }))
                    .map(|skill| approx_tokens(&skill.name) + approx_tokens(&skill.help))
                    .sum()
            });
        let workdir = ctx.env.repo_root.clone();
        let files = cli::adapter(ctx, agent)
            .map(|adapter| adapter.instruction_files(&workdir, false))
            .unwrap_or_default();
        Self {
            asked: Asked {
                worker: worker.clone(),
                files,
                depth: import_depth(agent),
                window: window_of(ctx, worker),
                first_count: first_count(ctx, agent),
                offered: tools::offered(ctx, conversation),
                skills,
            },
            kept: ctx.env.paths.session_starts(),
            workdir,
        }
    }

    /// The start `before` kept while nothing it was worked out from changed, and otherwise the
    /// start worked out again: as the last fresh session on its CLI and model started, or as
    /// recorded, with what this machine adds.
    pub(crate) fn work_out(self, before: KeptStart) -> KeptStart {
        let unchanged = before.from.as_ref().is_some_and(|from| {
            from.asked == self.asked && from.read.iter().all(|(path, was)| stamp(path) == *was)
        });
        if unchanged && before.start.is_some() {
            return before;
        }
        let writing = before.writing;
        let mut read = vec![(self.kept.clone(), stamp(&self.kept))];
        let asked = &self.asked;
        let start = kept(&self.kept, &asked.worker).unwrap_or_else(|| {
            let (files, looked_at) = read_files(&asked.files, asked.depth, &self.workdir);
            read.extend(looked_at.into_iter().map(|path| {
                let was = stamp(&path);
                (path, was)
            }));
            let measured = Measured::of(&asked.offered, asked.skills);
            estimated(
                &asked.worker,
                files,
                measured,
                asked.window,
                asked.first_count,
            )
        });
        KeptStart {
            from: Some(StartFrom {
                asked: self.asked,
                read,
            }),
            start: Some(start),
            writing,
            told: before.told,
        }
    }
}

/// What the main session holds at its start: the session's worker, else the worker the next turn
/// takes. Worked out once and again only when its CLI or model, the tools it's offered, its
/// skills, its instruction files or the kept starts change, on a blocking task.
pub(crate) async fn main_start(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    commands: Option<&CliCommandSet>,
) -> Option<StartingCount> {
    let worker = start_worker(ctx, conversation);
    let ask = StartAsk::here(ctx, conversation, &worker, commands);
    for write in std::mem::take(&mut conversation.start.writing) {
        if let Err(err) = write.await {
            ctx.notice(
                NoteLevel::Warn,
                format!("The Context tab couldn't keep a session's start: {err}"),
            );
        }
    }
    let before = std::mem::take(&mut conversation.start);
    match tokio::task::spawn_blocking(move || ask.work_out(before)).await {
        Ok(kept) => conversation.start = kept,
        Err(err) => ctx.notice(
            NoteLevel::Warn,
            format!("The Context tab couldn't work out the main session's start: {err}"),
        ),
    }
    conversation.start.start.clone()
}

/// Sends the screen the main session's start unless it's the one sent last, so the Context mod
/// above the input has it before the session's CLI counts it.
pub(crate) async fn tell_main_start(
    ctx: &Ctx,
    conversation: &mut ConversationState,
    commands: Option<&CliCommandSet>,
) {
    let Some(start) = main_start(ctx, conversation, commands).await else {
        return;
    };
    let worker = start_worker(ctx, conversation);
    let figures = window_figures(ctx.services.registry.as_ref(), &worker);
    let told = (start, figures.cli.is_some());
    if conversation.start.told.as_ref() == Some(&told) {
        return;
    }
    let estimated =
        cli::adapter(ctx, &told.0.agent).is_some_and(|adapter| adapter.estimates_context_fill());
    ctx.send(AppEvent::MainStart {
        start: Box::new(told.0.clone()),
        cli_window: told.1,
        estimated,
    });
    conversation.start.told = Some(told);
}

/// The session's worker, else the worker the next turn takes.
fn start_worker(ctx: &Ctx, conversation: &ConversationState) -> ModelRef {
    conversation.context.as_ref().map_or_else(
        || conversation.next_worker(ctx).clone(),
        |row| row.worker.clone(),
    )
}

/// `row` with what its CLI can say of it: whether it counts what fills a session, and whether
/// its fill is worked out from its totals.
pub(crate) fn as_its_cli_says(ctx: &Ctx, row: ContextRow) -> ContextRow {
    let agent = row.worker.agent.as_str();
    ContextRow {
        counts: Reading::of(ctx, agent) == Reading::ByCategory,
        estimated: cli::adapter(ctx, agent).is_some_and(|adapter| adapter.estimates_context_fill()),
        ..row
    }
}

/// Keeps `start` in the kept starts at `path` as the last fresh start of `worker`'s CLI and
/// model, under the model the CLI ran too, for a worker left on the CLI's default. It reads and
/// writes the file, so it runs on a blocking task.
pub(crate) fn keep(path: &Path, worker: &ModelRef, start: StartingCount) -> anyhow::Result<()> {
    let mut all = read_all(path);
    let ran = ModelRef::new(worker.agent.as_str(), start.model.clone());
    all.insert(key(&ran), start.clone());
    all.insert(key(worker), start);
    if let Some(dir) = path.parent() {
        create_private_dir_all(dir)?;
    }
    write_private_file(path, &serde_json::to_vec_pretty(&all)?)?;
    Ok(())
}

/// What a fresh session held at its CLI's first count by category, which came as
/// `first_count` says: every part but the messages, by the CLI's own names.
pub(crate) fn of_breakdown(
    worker: &ModelRef,
    usage: &ContextUsage,
    first_count: Option<FirstCount>,
) -> StartingCount {
    let parts = usage
        .of_kind(ContextKind::Used)
        .filter(|part| part.name != MESSAGES && part.tokens > 0)
        .map(|part| StartPart::named(part.name.clone(), part.tokens, part.color.clone()))
        .collect();
    StartingCount {
        agent: worker.agent.clone(),
        model: usage.model.clone().or_else(|| worker.model.clone()),
        window: Some(usage.window).filter(|window| *window > 0),
        buffer: if usage.auto_compact {
            usage.tokens_of(ContextKind::Buffer)
        } else {
            0
        },
        parts,
        files: Vec::new(),
        tools: None,
        from: StartedFrom::LastSession,
        first_count,
    }
}

/// What a fresh session on a CLI that says only how full it is held at its first reading, which
/// came as `first_count` says: what its first request took beyond the instruction files and what
/// Argonauts Code sent, which is the CLI's own prompt and tools, and the files. None when that
/// leaves nothing.
pub(crate) fn of_first_fill(
    worker: &ModelRef,
    files: Vec<NamedTokens>,
    fill: FirstFill,
    first_count: Option<FirstCount>,
) -> Option<StartingCount> {
    let FirstFill {
        input,
        window,
        sent,
        tools,
    } = fill;
    let in_files: u64 = files.iter().map(|file| file.tokens).sum();
    let own = input
        .checked_sub(in_files.saturating_add(sent))
        .filter(|own| *own > 0)?;
    let mut parts = vec![StartPart::holding(
        StartHolds::OwnPromptAndTools,
        own,
        OWN_COLOR,
    )];
    if in_files > 0 {
        parts.push(StartPart::holding(
            StartHolds::InstructionFiles,
            in_files,
            FILES_COLOR,
        ));
    }
    Some(StartingCount {
        agent: worker.agent.clone(),
        model: worker.model.clone(),
        window,
        buffer: 0,
        parts,
        files,
        tools: tools.map(|named| StartingTools {
            named,
            read_only: baseline_of(&worker.agent).and_then(|base| base.read_only_tools),
        }),
        from: StartedFrom::LastSession,
        first_count,
    })
}

/// What a CLI said of a fresh session's first request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FirstFill {
    /// What the request held, cached tokens included.
    pub input: u64,
    pub window: Option<u64>,
    /// What Argonauts Code sent with it.
    pub sent: u64,
    /// How many tools the CLI named as the session started.
    pub tools: Option<u32>,
}

/// The instruction files `agent`'s CLI loads by itself in `workdir`, by name, and how many
/// imports deep it reads them, for [`read_files`].
pub(crate) fn instruction_files(ctx: &Ctx, agent: &str, workdir: &Path) -> (Vec<String>, usize) {
    let names = cli::adapter(ctx, agent)
        .map(|adapter| adapter.instruction_files(workdir, false))
        .unwrap_or_default();
    (names, import_depth(agent))
}

/// How many imports deep `agent`'s CLI follows an instruction file's `@path` words.
fn import_depth(agent: &str) -> usize {
    if baseline_of(agent).is_some_and(|base| base.follows_imports) {
        IMPORT_DEPTH
    } else {
        0
    }
}

/// The instruction files `names` in `workdir`, at a token per 4 characters, as Argonauts Code
/// counts what it sends, with what each imports up to `depth` deep, and every path looked at.
/// It reads the files, so it runs on a blocking task.
pub(crate) fn read_files(
    names: &[String],
    depth: usize,
    workdir: &Path,
) -> (Vec<NamedTokens>, Vec<PathBuf>) {
    let mut looked_at = Vec::new();
    let mut files = Vec::new();
    for name in names {
        let places = [workdir.join(name), workdir.join(".claude").join(name)];
        let found = places.iter().find(|path| path.is_file()).cloned();
        looked_at.extend(places);
        let Some(path) = found else {
            continue;
        };
        let mut seen = BTreeSet::new();
        let tokens = file_tokens(&path, depth, &mut seen);
        looked_at.extend(seen);
        files.push(NamedTokens {
            name: name.clone(),
            tokens,
        });
    }
    (files, looked_at)
}

/// The tokens of the file at `path` and, up to `depth` imports deep, of each file it imports
/// with `@path`, relative to it. Each file counts once.
fn file_tokens(path: &Path, depth: usize, seen: &mut BTreeSet<PathBuf>) -> u64 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    if !seen.insert(path.canonicalize().unwrap_or_else(|_| path.to_path_buf())) {
        return 0;
    }
    let mut tokens = approx_tokens(&text);
    if depth > 0 {
        let dir = path.parent().unwrap_or(Path::new(""));
        for import in imports(&text) {
            tokens = tokens.saturating_add(file_tokens(&dir.join(import), depth - 1, seen));
        }
    }
    tokens
}

/// The paths `text` imports: each word that starts with `@`, outside code blocks and spans.
fn imports(text: &str) -> Vec<&str> {
    let mut fenced = false;
    let mut found = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let outside_spans = line.split('`').step_by(2);
        found.extend(
            outside_spans
                .flat_map(str::split_whitespace)
                .filter_map(|word| word.strip_prefix('@'))
                .filter(|path| !path.is_empty()),
        );
    }
    found
}

/// `worker`'s window, as the registry knows it: for a worker left on its CLI's default, the
/// window of the model that CLI runs by default.
pub(crate) fn window_of(ctx: &Ctx, worker: &ModelRef) -> Option<u64> {
    let registry = ctx.services.registry.as_ref();
    let cli_default = || {
        if worker.model.is_some() {
            return None;
        }
        let model = ctx.names.names().defaults.get(&worker.agent).cloned()?;
        registry.get(&ModelRef::new(worker.agent.as_str(), Some(model)))
    };
    let entry = registry.get(worker).or_else(cli_default)?;
    entry.window.or(entry.context_window)
}

/// A very first run's start: as recorded where a recording of the CLI ships, else only what
/// this machine adds.
fn estimated(
    worker: &ModelRef,
    files: Vec<NamedTokens>,
    measured: Measured,
    window: Option<u64>,
    first_count: Option<FirstCount>,
) -> StartingCount {
    match baseline_of(&worker.agent) {
        Some(baseline) => recorded(baseline, worker, files, &measured, window, first_count),
        None => unrecorded(worker, files, &measured, window),
    }
}

/// The CLI's recorded prompt and tools, and what this machine adds, in the CLI's order:
/// Argonauts Code's tools and the lines about them after the CLI's own, then the files and
/// skills.
fn recorded(
    baseline: &Baseline,
    worker: &ModelRef,
    files: Vec<NamedTokens>,
    measured: &Measured,
    window: Option<u64>,
    first_count: Option<FirstCount>,
) -> StartingCount {
    let mut parts: Vec<StartPart> = baseline
        .parts
        .iter()
        .map(|part| {
            let color = part.color.as_deref().unwrap_or(OWN_COLOR);
            match &part.name {
                Some(name) => StartPart::named(name.clone(), part.tokens, color),
                None => StartPart::holding(StartHolds::OwnPromptAndTools, part.tokens, color),
            }
        })
        .collect();
    for (apart, tokens) in [
        (&baseline.app_tools, measured.app_tools),
        (&baseline.app_note, measured.app_note),
    ] {
        match apart {
            Some(apart) if tokens > 0 => {
                parts.push(StartPart::named(apart.name.clone(), tokens, &apart.color));
            }
            _ => add_to_own(&mut parts, tokens),
        }
    }
    let in_files: u64 = files.iter().map(|file| file.tokens).sum();
    if in_files > 0 {
        parts.push(match &baseline.files {
            Some(named) => StartPart::named(named.name.clone(), in_files, &named.color),
            None => StartPart::holding(StartHolds::InstructionFiles, in_files, FILES_COLOR),
        });
    }
    // A CLI that counts no skills apart from its prompt is left at its recorded part.
    if let Some(skills) = &baseline.skills {
        let tokens = skills.built_in.saturating_add(measured.skills);
        if tokens > 0 {
            parts.push(StartPart::named(skills.name.clone(), tokens, &skills.color));
        }
    }
    let recorded_model = worker.model.is_some() && worker.model == baseline.model;
    StartingCount {
        agent: worker.agent.clone(),
        model: worker.model.clone(),
        window: window.or(baseline.window.filter(|_| recorded_model)),
        buffer: baseline.buffer,
        parts,
        files,
        tools: baseline.tools.map(|named| StartingTools {
            named,
            read_only: baseline.read_only_tools,
        }),
        from: StartedFrom::Recording {
            version: baseline.version.clone(),
        },
        first_count,
    }
}

/// What this machine adds, for a CLI no recording ships for: Argonauts Code's tools with the
/// lines about them, and the instruction files.
fn unrecorded(
    worker: &ModelRef,
    files: Vec<NamedTokens>,
    measured: &Measured,
    window: Option<u64>,
) -> StartingCount {
    let mut parts = Vec::new();
    let app = measured.app_tools.saturating_add(measured.app_note);
    if app > 0 {
        parts.push(StartPart::holding(
            StartHolds::AppTools,
            app,
            APP_TOOLS_COLOR,
        ));
    }
    let in_files: u64 = files.iter().map(|file| file.tokens).sum();
    if in_files > 0 {
        parts.push(StartPart::holding(
            StartHolds::InstructionFiles,
            in_files,
            FILES_COLOR,
        ));
    }
    StartingCount {
        agent: worker.agent.clone(),
        model: worker.model.clone(),
        window,
        buffer: 0,
        parts,
        files,
        tools: None,
        from: StartedFrom::Unrecorded,
        first_count: None,
    }
}

fn baseline_of(agent: &str) -> Option<&'static Baseline> {
    BASELINES.iter().find(|base| base.agent == agent)
}

/// Adds `tokens` to the CLI's own prompt and tools.
fn add_to_own(parts: &mut [StartPart], tokens: u64) {
    let own = Some(StartHolds::OwnPromptAndTools);
    if let Some(part) = parts.iter_mut().find(|part| part.holds == own) {
        part.tokens = part.tokens.saturating_add(tokens);
    }
}

/// When `agent`'s CLI first counts a fresh session itself, as its adapter says.
pub(crate) fn first_count(ctx: &Ctx, agent: &str) -> Option<FirstCount> {
    cli::adapter(ctx, agent).and_then(|adapter| adapter.first_count())
}

fn key(worker: &ModelRef) -> String {
    match &worker.model {
        Some(model) => format!("{} {model}", worker.agent),
        None => worker.agent.clone(),
    }
}

/// Every kept start. A file missing or not read as one keeps none, and the next fresh
/// session writes it again.
fn read_all(path: &Path) -> BTreeMap<String, StartingCount> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn kept(path: &Path, worker: &ModelRef) -> Option<StartingCount> {
    read_all(path).remove(&key(worker))
}

#[cfg(test)]
mod tests {
    use argonauts_core::adapter::Adapter;

    use super::*;

    fn files(tokens: u64) -> Vec<NamedTokens> {
        vec![NamedTokens {
            name: "AGENTS.md".to_owned(),
            tokens,
        }]
    }

    fn fill(input: u64, sent: u64, tools: Option<u32>) -> FirstFill {
        FirstFill {
            input,
            window: None,
            sent,
            tools,
        }
    }

    /// A part by the CLI's own name, or by what it holds.
    fn label(part: &StartPart) -> &str {
        match part.holds {
            Some(StartHolds::OwnPromptAndTools) => "own prompt and tools",
            Some(StartHolds::InstructionFiles) => "instruction files",
            Some(StartHolds::AppTools) => "our tools",
            None => part.name.as_str(),
        }
    }

    fn parts(start: &StartingCount) -> Vec<(&str, u64)> {
        start
            .parts
            .iter()
            .map(|part| (label(part), part.tokens))
            .collect()
    }

    #[test]
    fn every_cli_has_its_recorded_prompt_and_tools() {
        let agents: Vec<&str> = BASELINES.iter().map(|base| base.agent.as_str()).collect();
        assert_eq!(
            agents,
            ["claude-code", "codex", "opencode", "antigravity-cli"]
        );
        assert!(BASELINES.iter().all(|base| !base.parts.is_empty()));
    }

    #[test]
    fn a_first_run_on_codex_adds_the_files_here_to_its_recorded_part() {
        let worker = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
        let start = estimated(
            &worker,
            files(1_200),
            Measured::default(),
            Some(372_000),
            Some(FirstCount::Turn),
        );
        assert_eq!(
            parts(&start),
            [
                ("own prompt and tools", 15_236),
                ("instruction files", 1_200)
            ]
        );
        assert_eq!(start.first_count, Some(FirstCount::Turn));
        assert_eq!(
            start.from,
            StartedFrom::Recording {
                version: "0.163.0".to_owned()
            }
        );
        assert_eq!(start.window, Some(372_000));
    }

    #[test]
    fn claude_code_counts_its_files_as_memory_files_and_keeps_its_buffer() {
        let worker = ModelRef::new("claude-code", Some("claude-haiku-4-5-20251001".to_owned()));
        let start = estimated(&worker, files(900), Measured::default(), None, None);
        assert_eq!(
            parts(&start),
            [
                ("System prompt", 6_888),
                ("System tools", 15_135),
                ("Memory files", 900),
                ("Skills", 1_656),
            ]
        );
        assert_eq!(start.window, Some(200_000));
        assert_eq!(start.buffer, 33_000);
    }

    #[test]
    fn another_model_takes_no_window_from_the_recording() {
        for model in [Some("claude-haiku-5-5".to_owned()), None] {
            let worker = ModelRef::new("claude-code", model);
            let start = estimated(&worker, Vec::new(), Measured::default(), None, None);
            assert_eq!(start.window, None, "{worker:?}");
        }
    }

    /// Claude Code's recorded parts, its own skills, its window and its buffer are its reply
    /// before its first answer.
    #[test]
    fn claude_codes_recorded_parts_are_its_recordings() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../adapters/claude-code/fixtures");
        let stdout = std::fs::read_to_string(fixtures.join("context_usage_summary.stdout.jsonl"))
            .expect("the recording");
        let reply = stdout
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find_map(|line| count_in(&line).cloned())
            .expect("its count by category");
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixtures.join("context_usage_summary.meta.json"))
                .expect("its meta"),
        )
        .expect("json");
        let base = baseline_of("claude-code").expect("Claude Code's recording");
        assert_eq!(meta["cli_version"].as_str(), Some(base.version.as_str()));
        let counted = |name: &str| {
            reply["categories"]
                .as_array()
                .and_then(|parts| parts.iter().find(|part| part["name"] == name))
                .and_then(|part| part["tokens"].as_u64())
        };
        for part in &base.parts {
            let name = part.name.as_deref().expect("a name Claude Code gives it");
            assert_eq!(counted(name), Some(part.tokens), "{name}");
        }
        let built_in: u64 = reply["skills"]["skillFrontmatter"]
            .as_array()
            .expect("its skills")
            .iter()
            .filter(|skill| skill["source"] == "built-in")
            .filter_map(|skill| skill["tokens"].as_u64())
            .sum();
        assert_eq!(
            base.skills.as_ref().map(|skills| skills.built_in),
            Some(built_in)
        );
        assert_eq!(reply["model"].as_str(), base.model.as_deref());
        let window = reply["maxTokens"].as_u64();
        assert_eq!(window, base.window);
        let compacts_at = reply["autoCompactThreshold"]
            .as_u64()
            .expect("where it compacts");
        assert_eq!(window.map(|window| window - compacts_at), Some(base.buffer));
    }

    /// The object in `value` that holds a count by category and its skills.
    fn count_in(value: &serde_json::Value) -> Option<&serde_json::Value> {
        match value {
            serde_json::Value::Object(map)
                if map.contains_key("categories") && map.contains_key("skills") =>
            {
                Some(value)
            }
            serde_json::Value::Object(map) => map.values().find_map(count_in),
            serde_json::Value::Array(items) => items.iter().find_map(count_in),
            _ => None,
        }
    }

    #[test]
    fn a_first_reading_leaves_the_files_and_what_was_sent_out_of_the_own_part() {
        let worker = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
        let start =
            of_first_fill(&worker, files(1_000), fill(20_000, 40, None), None).expect("a start");
        assert_eq!(start.parts[0].tokens, 18_960);
        assert_eq!(start.used(), 19_960);
        assert_eq!(start.tools, None);
        assert!(of_first_fill(&worker, files(1_000), fill(900, 40, None), None).is_none());
    }

    #[test]
    fn a_kept_start_is_read_back_by_cli_and_model() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join("session-starts.json");
        let worker = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
        let start =
            of_first_fill(&worker, Vec::new(), fill(16_000, 10, None), None).expect("start");
        let mut all = BTreeMap::new();
        all.insert(key(&worker), start.clone());
        std::fs::write(&path, serde_json::to_vec(&all).expect("json")).expect("written");
        assert_eq!(kept(&path, &worker), Some(start));
        assert_eq!(kept(&path, &ModelRef::new("codex", None)), None);
    }

    #[test]
    fn a_first_run_on_claude_code_adds_our_tools_their_lines_and_its_skills_where_it_counts_them() {
        let worker = ModelRef::new("claude-code", None);
        let measured = Measured {
            app_tools: 2_540,
            app_note: 300,
            skills: 710,
        };
        let start = estimated(&worker, files(900), measured, None, None);
        assert_eq!(
            parts(&start),
            [
                ("System prompt", 6_888),
                ("System tools", 15_135),
                ("MCP tools", 2_540),
                ("MCP server instructions", 300),
                ("Memory files", 900),
                ("Skills", 2_366),
            ]
        );
    }

    #[test]
    fn a_cli_with_no_place_of_its_own_for_them_counts_our_tools_in_its_own_part() {
        let worker = ModelRef::new("codex", None);
        let measured = Measured {
            app_tools: 100,
            app_note: 20,
            skills: 50,
        };
        let start = estimated(&worker, Vec::new(), measured, None, None);
        assert_eq!(parts(&start), [("own prompt and tools", 15_356)]);
    }

    #[test]
    fn a_cli_no_recording_ships_for_starts_with_what_this_machine_adds() {
        let worker = ModelRef::new("ollama", Some("llama3.2".to_owned()));
        let measured = Measured {
            app_tools: 100,
            app_note: 20,
            skills: 50,
        };
        let start = estimated(&worker, files(300), measured, Some(128_000), None);
        assert_eq!(
            parts(&start),
            [("our tools", 120), ("instruction files", 300)]
        );
        assert_eq!(start.from, StartedFrom::Unrecorded);
        assert_eq!((start.window, start.first_count), (Some(128_000), None));
    }

    /// The start is worked out again only once what it is worked out from has changed: an
    /// instruction file, the kept starts, or the tools the session is offered.
    #[test]
    fn a_start_is_worked_out_again_only_when_what_it_reads_changes() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path();
        std::fs::write(root.join("AGENTS.md"), "a".repeat(400)).expect("written");
        let worker = ModelRef::new("codex", Some("gpt-6-luna".to_owned()));
        let ask = |offered: &[&str]| StartAsk {
            asked: Asked {
                worker: worker.clone(),
                files: vec!["AGENTS.md".to_owned()],
                depth: 0,
                window: Some(372_000),
                first_count: Some(FirstCount::Turn),
                offered: offered.iter().map(|&name| name.to_owned()).collect(),
                skills: 0,
            },
            kept: root.join("session-starts.json"),
            workdir: root.to_path_buf(),
        };
        let files = |kept: &KeptStart| {
            let start = kept.start.as_ref().expect("a start");
            parts(start)
                .into_iter()
                .find(|(name, _)| *name == "instruction files")
                .map(|(_, tokens)| tokens)
        };
        let first = ask(&[]).work_out(KeptStart::default());
        assert_eq!(files(&first), Some(100));
        let from = first.from.clone();
        let again = ask(&[]).work_out(first);
        assert_eq!((again.from == from, files(&again)), (true, Some(100)));

        std::fs::write(root.join("AGENTS.md"), "a".repeat(800)).expect("written");
        let edited = ask(&[]).work_out(again);
        assert_eq!(files(&edited), Some(200));
        let offered = ask(&["start_workflow"]).work_out(edited);
        assert_ne!(
            offered.from.as_ref().map(|from| &from.asked.offered),
            from.as_ref().map(|from| &from.asked.offered)
        );

        let start =
            of_first_fill(&worker, Vec::new(), fill(16_000, 10, None), None).expect("start");
        keep(&root.join("session-starts.json"), &worker, start.clone()).expect("kept");
        let kept = ask(&["start_workflow"]).work_out(offered);
        assert_eq!(kept.start, Some(start));
    }

    #[test]
    fn an_instruction_file_counts_what_it_imports_once_each() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs")).expect("a folder");
        std::fs::write(
            root.join("CLAUDE.md"),
            "@AGENTS.md\nSee @docs/style.md and `@not/this.md`.\n```\n@nor/this.md\n```\n",
        )
        .expect("written");
        std::fs::write(root.join("AGENTS.md"), "a".repeat(400)).expect("written");
        std::fs::write(
            root.join("docs/style.md"),
            format!("{}\n@../AGENTS.md", "b".repeat(80)),
        )
        .expect("written");
        let alone = approx_tokens(&std::fs::read_to_string(root.join("CLAUDE.md")).expect("read"));
        let style =
            approx_tokens(&std::fs::read_to_string(root.join("docs/style.md")).expect("read"));
        let path = root.join("CLAUDE.md");
        assert_eq!(
            file_tokens(&path, IMPORT_DEPTH, &mut BTreeSet::new()),
            alone + 100 + style
        );
        assert_eq!(file_tokens(&path, 0, &mut BTreeSet::new()), alone);
    }

    /// What a fresh session's start is kept as from the recording at `stdout`, replayed
    /// through `adapter`'s own parser with no instruction file and `sent` from Argonauts Code.
    fn replayed(adapter: &dyn Adapter, stdout: &str, sent: u64) -> StartingCount {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../adapters")
            .join(stdout);
        let recording = std::fs::read_to_string(path).expect("the recording");
        let mut parser = adapter.parser();
        let mut events = Vec::new();
        for line in recording.lines() {
            events.extend(parser.parse_stdout_line(line));
        }
        events.extend(parser.finish(Some(0)));
        let mut fresh = Fresh::reading(Reading::HowFull, adapter.first_count());
        let worker = ModelRef::new(adapter.id(), None);
        for event in &events {
            fresh.heard(event);
            if matches!(event, AgentEvent::ContextFill { .. }) {
                let fill = fresh.first_fill(None, sent).expect("its first request");
                return of_first_fill(&worker, Vec::new(), fill, None).expect("a start");
            }
        }
        panic!("{stdout} never says how full the session is");
    }

    #[test]
    fn a_first_reading_keeps_what_the_first_request_held_as_each_recording_did() {
        let prompt = approx_tokens("Reply with exactly the word: ok");
        let codex = replayed(
            &argonauts_adapter_codex::CodexAdapter::new(),
            "codex/fixtures/read_only_ok.stdout.jsonl",
            prompt,
        );
        // The recording's prompt took 14 tokens.
        let opencode = replayed(
            &argonauts_adapter_opencode::OpenCodeAdapter::new(),
            "opencode/fixtures/export_last_steps.stdout.jsonl",
            14,
        );
        let agy = replayed(
            &argonauts_adapter_antigravity_cli::AntigravityCli::new(),
            "antigravity-cli/fixtures/readonly_ok.stdout.jsonl",
            prompt,
        );
        for (start, own) in [(codex, 15_236), (opencode, 7_743), (agy, 4_226)] {
            assert_eq!(start.parts[0].tokens, own, "{start:?}");
            let recorded = baseline_of(&start.agent).map(|base| base.parts[0].tokens);
            assert_eq!(recorded, Some(own), "{}", start.agent);
        }
    }

    #[test]
    fn a_turn_total_keeps_nothing_once_a_tool_ran_before_it() {
        let usage = AgentEvent::Usage {
            input_tokens: 40_000,
            output_tokens: 300,
            cached_tokens: 30_000,
            cost_usd: None,
            cumulative: true,
        };
        let tool = AgentEvent::ToolStarted {
            id: "t-1".to_owned(),
            name: "command_execution".to_owned(),
            input: serde_json::Value::Null,
            parent: None,
        };
        let mut turn = Fresh::reading(Reading::HowFull, Some(FirstCount::Turn));
        let mut step = Fresh::reading(Reading::HowFull, Some(FirstCount::Step));
        for fresh in [&mut turn, &mut step] {
            fresh.heard(&tool);
            fresh.heard(&usage);
        }
        assert_eq!(turn.first_fill(None, 8), None);
        assert_eq!(
            step.first_fill(None, 8).map(|fill| fill.input),
            Some(40_000)
        );
    }

    #[test]
    fn a_cli_that_counts_is_asked_once_at_its_first_reading() {
        let fill = AgentEvent::ContextFill {
            used: 20_000,
            size: None,
            sent: None,
        };
        let mut counts = Fresh::reading(Reading::ByCategory, Some(FirstCount::Answer));
        assert!(counts.heard(&fill));
        assert!(!counts.heard(&fill));
        let mut how_full = Fresh::reading(Reading::HowFull, Some(FirstCount::Step));
        assert!(!how_full.heard(&fill));
    }

    #[test]
    fn agy_names_its_tools_at_start_and_a_read_only_session_offers_fewer() {
        let worker = ModelRef::new("antigravity-cli", None);
        let start = estimated(&worker, Vec::new(), Measured::default(), None, None);
        assert_eq!(
            start.tools,
            Some(StartingTools {
                named: 60,
                read_only: Some(5),
            })
        );
        let mut fresh = Fresh::reading(Reading::HowFull, Some(FirstCount::Step));
        fresh.heard(&AgentEvent::Init {
            session_id: None,
            model: None,
            tools: vec!["view_file".to_owned(), "run_command".to_owned()],
            plugins: Vec::new(),
            capabilities: Vec::new(),
            version: None,
        });
        assert_eq!(fresh.named(&worker), worker);
        let start =
            of_first_fill(&worker, Vec::new(), fill(5_000, 8, fresh.tools), None).expect("a start");
        assert_eq!(
            start.tools,
            Some(StartingTools {
                named: 2,
                read_only: Some(5),
            })
        );
    }

    /// The counts agy's own recording holds: the tools its start names, and those of the
    /// read-only agent Argonauts Code gave it.
    #[test]
    fn agys_tool_counts_are_its_recordings() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../adapters/antigravity-cli/fixtures");
        let stdout = std::fs::read_to_string(fixtures.join("readonly_ok.stdout.jsonl"))
            .expect("the recording");
        let init: serde_json::Value =
            serde_json::from_str(stdout.lines().next().expect("a first line")).expect("json");
        let named = init["init"]["tools"].as_array().expect("tools").len();
        let meta: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixtures.join("readonly_ok.meta.json")).expect("its meta"),
        )
        .expect("json");
        let agent = meta["files"]
            .as_array()
            .and_then(|files| {
                files.iter().find_map(|file| {
                    file["contents"]
                        .as_str()
                        .filter(|text| text.contains("tools:"))
                })
            })
            .expect("the read-only agent's file");
        let offered = agent
            .lines()
            .filter(|line| line.starts_with("  - "))
            .count();
        let base = baseline_of("antigravity-cli").expect("agy's recording");
        assert_eq!(base.tools, Some(u32::try_from(named).expect("a count")));
        assert_eq!(
            base.read_only_tools,
            Some(u32::try_from(offered).expect("a count"))
        );
    }
}
