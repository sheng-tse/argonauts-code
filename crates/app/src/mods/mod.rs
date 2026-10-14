//! Mod files in the app. It loads them and loads them again when one changes, runs the
//! commands of the ones that read a command, gathers the numbers of the built-in sources, and
//! answers `/mods` and what a mod's page asks: save, reset, trust.
//!
//! | Part | Where |
//! |---|---|
//! | the loaded files, and what each command last printed | this file |
//! | the numbers Argonauts Code already keeps, as readings | [`sources`] |
//! | a command, in the sandbox, with a timeout | [`command`] |
//! | writing a file | [`files`] |

mod command;
pub(crate) mod files;
mod sources;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use argonauts_conductor::sandbox::{self, SandboxWrap};
use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::input::ModPlaceChange;
use argonauts_core::settings::Settings;
use argonauts_core::settings::mods::view::{
    FileProblem, LineCli, LineNumbers, ModChange, ModRow, ModRowKind, ModsView,
};
use argonauts_core::settings::mods::{
    LoadedMod, MODS_DIR, ModFile, ModOrigin, ModPlace, ModReading, ModSet, ModSource, builtin,
    is_mod_name, load, span_words,
};
use argonauts_core::settings::ui::{ModName, Place, UiSettings};
use argonauts_core::views::ContextRow;
use argonauts_core::{NoteLevel, Spend};
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

use crate::controller::{Ctx, Effect, Internal};
use crate::env::AppEnv;
use crate::trust::TrustStore;
use crate::usage_ledger::{UsageLedger, local_day};

pub(crate) use sources::Numbers;

/// How often the folders are looked at for a change, and the commands for one that's due.
const TICK: Duration = Duration::from_secs(1);
/// How long after `/mods` was last opened or refreshed every command keeps running, so the
/// previews of mods that show nowhere stay live while you look at them.
const WANTED_FOR: Duration = Duration::from_secs(120);
/// How long the last good numbers stay on screen while a command keeps failing, or three of
/// its own intervals when that's longer.
const KEEP_FOR: Duration = Duration::from_secs(60);
/// How often the screen gets newer numbers for a file of yours that draws a built-in source
/// somewhere, which no command's output announces.
const BUILT_IN_EVERY: Duration = Duration::from_secs(10);

/// What a command last printed, and why its latest run printed nothing if it didn't.
#[derive(Debug, Clone, PartialEq)]
struct Ran {
    /// The last numbers it printed, with when. They stay through a run that fails, so one
    /// slow answer doesn't blank the card.
    good: Option<(ModReading, Instant)>,
    /// Why the latest run has no numbers.
    problem: Option<String>,
    at: Instant,
    /// The command as it was when it ran, so an edited one runs again at once.
    command: Vec<String>,
    /// And the program beside the mod's file, for the same reason.
    program: Option<ProgramStamp>,
}

/// A program's size and when it last changed.
type ProgramStamp = (u64, Option<std::time::SystemTime>);

/// The stamp of the program `command` names beside the mod's file, as `./checks.sh` does.
fn program_stamp(command: &[String], mod_dir: &Path) -> Option<ProgramStamp> {
    let path = command::program_path(command.first()?, mod_dir)?;
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()))
}

impl Ran {
    /// The numbers to draw now: the last good ones, until they're older than a few runs.
    fn reading(&self, every: Duration) -> Option<&ModReading> {
        let (reading, at) = self.good.as_ref()?;
        let fresh = self.problem.is_none() || at.elapsed() <= KEEP_FOR.max(every * 3);
        fresh.then_some(reading)
    }
}

#[derive(Debug, Default)]
struct Inner {
    set: ModSet,
    stamp: u64,
    ran: HashMap<String, Ran>,
    wanted_until: Option<Instant>,
    /// The project's mods already said to wait for your trust this session.
    told_waiting: HashSet<String>,
}

/// The folders mod files are read from.
#[derive(Debug, Clone)]
pub(crate) struct ModDirs {
    pub yours: PathBuf,
    pub project: PathBuf,
}

impl ModDirs {
    pub(crate) fn of(env: &AppEnv) -> Self {
        Self {
            yours: env.paths.home().join(MODS_DIR),
            project: crate::setup::project_folder(env).join(MODS_DIR),
        }
    }

    fn dir(&self, origin: ModOrigin) -> &Path {
        match origin {
            ModOrigin::Project => &self.project,
            ModOrigin::BuiltIn | ModOrigin::Yours => &self.yours,
        }
    }

    fn stamp(&self) -> u64 {
        load::stamp(&[Some(self.yours.as_path()), Some(self.project.as_path())])
    }

    pub(crate) fn load(&self) -> ModSet {
        ModSet::load(Some(&self.yours), Some(&self.project))
    }
}

pub(crate) struct ModsState {
    inner: Arc<Mutex<Inner>>,
    dirs: ModDirs,
}

fn lock(inner: &Mutex<Inner>) -> std::sync::MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ModsState {
    /// Loads the files and starts watching them. The watcher ends with the controller.
    pub(crate) fn new(ctx: &Ctx) -> Self {
        let dirs = ModDirs::of(&ctx.env);
        let inner = Arc::new(Mutex::new(Inner {
            set: dirs.load(),
            stamp: dirs.stamp(),
            ..Inner::default()
        }));
        let watcher = Watcher {
            inner: Arc::clone(&inner),
            dirs: dirs.clone(),
            env: Arc::clone(&ctx.env),
            settings: ctx.settings.follow(),
            internal: ctx.internal.clone(),
            sandbox: None,
        };
        tokio::spawn(watcher.run());
        Self { inner, dirs }
    }
}

/// Looks at the folders every second and runs the commands that are due.
struct Watcher {
    inner: Arc<Mutex<Inner>>,
    dirs: ModDirs,
    env: Arc<AppEnv>,
    settings: watch::Receiver<Arc<Settings>>,
    internal: mpsc::UnboundedSender<Internal>,
    /// A test's fake. Unset, the sandbox `sandbox.mode` asks for.
    sandbox: Option<Arc<dyn SandboxWrap>>,
}

impl Watcher {
    async fn run(self) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut told: Option<Instant> = None;
        loop {
            tick.tick().await;
            if self.internal.is_closed() {
                return;
            }
            let changed = self.step().await;
            // The first look tells the screen your mods' names, which are commands.
            let due = match told {
                None => self.has_a_file(),
                Some(at) => at.elapsed() >= BUILT_IN_EVERY && self.places_a_file(),
            };
            if changed || due {
                told = Some(Instant::now());
                if self.internal.send(Internal::Mods).is_err() {
                    return;
                }
            }
        }
    }

    /// Whether a file of yours or the project's shows somewhere, so the screen needs its
    /// numbers kept fresh.
    fn places_a_file(&self) -> bool {
        lock(&self.inner)
            .set
            .mods
            .iter()
            .any(|loaded| loaded.origin != ModOrigin::BuiltIn && loaded.file.shows_somewhere())
    }

    fn has_a_file(&self) -> bool {
        lock(&self.inner)
            .set
            .mods
            .iter()
            .any(|loaded| loaded.origin != ModOrigin::BuiltIn)
    }

    /// One look. True when a file or a reading changed.
    async fn step(&self) -> bool {
        let mut changed = self.reload_if_changed();
        let settings = Arc::clone(&self.settings.borrow());
        for (name, file, mod_dir) in self.due(&settings) {
            let ModSource::Command {
                command, timeout, ..
            } = &file.data
            else {
                continue;
            };
            let sandbox = match &self.sandbox {
                Some(sandbox) => Arc::clone(sandbox),
                None => sandbox::for_settings(&settings.sandbox),
            };
            let user_home = self
                .env
                .user_home
                .clone()
                .unwrap_or_else(|| PathBuf::from("/"));
            let scratch =
                std::env::temp_dir().join(format!("argonauts-mods-{}", std::process::id()));
            let place = command::Place {
                mod_dir: &mod_dir,
                repo: &self.env.repo_root,
                argonauts_home: self.env.paths.home(),
                user_home: &user_home,
                scratch: &scratch,
            };
            let program = program_stamp(command, &mod_dir);
            let result = command::run(
                command,
                *timeout,
                sandbox.as_ref(),
                &settings.sandbox,
                &place,
            )
            .await;
            let mut inner = lock(&self.inner);
            let now = Instant::now();
            // An edited command starts over: the old one's numbers aren't its own.
            let before = inner
                .ran
                .remove(&name)
                .filter(|ran| &ran.command == command);
            let after = match result {
                Ok(reading) => Ran {
                    good: Some((reading, now)),
                    problem: None,
                    at: now,
                    command: command.clone(),
                    program,
                },
                Err(why) => Ran {
                    good: before.as_ref().and_then(|ran| ran.good.clone()),
                    problem: Some(why),
                    at: now,
                    command: command.clone(),
                    program,
                },
            };
            let shown = |ran: &Ran| {
                (
                    ran.good.as_ref().map(|(reading, _)| reading.clone()),
                    ran.problem.clone(),
                )
            };
            changed |= before.as_ref().map(shown) != Some(shown(&after));
            inner.ran.insert(name, after);
        }
        changed
    }

    fn reload_if_changed(&self) -> bool {
        let stamp = self.dirs.stamp();
        let mut inner = lock(&self.inner);
        if inner.stamp == stamp {
            return false;
        }
        inner.stamp = stamp;
        inner.set = self.dirs.load();
        // A mod that went away takes what it printed with it.
        let names: Vec<String> = inner.set.mods.iter().map(|m| m.file.name.clone()).collect();
        inner.ran.retain(|name, _| names.contains(name));
        true
    }

    /// The command mods to run now: those that may run, that show somewhere or are being
    /// looked at, and whose time has come.
    fn due(&self, settings: &Settings) -> Vec<(String, ModFile, PathBuf)> {
        if !settings.mods.commands {
            return Vec::new();
        }
        let inner = lock(&self.inner);
        let now = Instant::now();
        let looked_at = inner.wanted_until.is_some_and(|until| now < until);
        inner
            .set
            .mods
            .iter()
            .filter(|loaded| looked_at || loaded.file.shows_somewhere())
            .filter(|loaded| loaded.file.data.is_command())
            .filter(|loaded| may_run(loaded, &self.env.state_dir))
            .filter_map(|loaded| {
                let ModSource::Command { command, every, .. } = &loaded.file.data else {
                    return None;
                };
                let mod_dir = loaded.path.as_deref()?.parent()?.to_path_buf();
                let ran = inner.ran.get(&loaded.file.name);
                let due = ran.is_none_or(|ran| {
                    &ran.command != command
                        || ran.program != program_stamp(command, &mod_dir)
                        || now.duration_since(ran.at) >= *every
                });
                due.then(|| (loaded.file.name.clone(), loaded.file.clone(), mod_dir))
            })
            .collect()
    }
}

/// The project's mod runs its command only once you trusted its file as it is now, and with
/// it the program beside it. Yours run as they are.
fn may_run(loaded: &LoadedMod, state_dir: &Path) -> bool {
    if loaded.origin != ModOrigin::Project {
        return true;
    }
    let Some(path) = &loaded.path else {
        return false;
    };
    // Read when a project's mod asks, which most repositories never do.
    let trust = TrustStore::load(state_dir);
    trust.was_trusted(path)
        && programs_beside(loaded)
            .iter()
            .all(|program| trust.was_trusted(program))
}

/// The programs a mod's command names beside its file, as `./checks.sh` does, wherever they
/// sit in it: `sh ./checks.sh` runs one as surely as `./checks.sh` does.
fn programs_beside(loaded: &LoadedMod) -> Vec<PathBuf> {
    let ModSource::Command { command, .. } = &loaded.file.data else {
        return Vec::new();
    };
    let Some(dir) = loaded.path.as_deref().and_then(Path::parent) else {
        return Vec::new();
    };
    command
        .iter()
        .filter_map(|word| command::program_path(word, dir))
        .filter(|program| program.is_file())
        .collect()
}

/// What Argonauts Code keeps that the built-in sources and the bottom line's preview draw from.
async fn numbers(ctx: &Ctx, context: Option<ContextRow>) -> Numbers {
    let settings = ctx.settings();
    let plan_chips = crate::controller::plan_usage_chips(
        &settings.crew.members(),
        ctx.services.plan_usage.as_ref(),
        ctx.services.health.as_ref(),
    );
    let now = ctx.services.clock.now();
    let mut today: Vec<(String, Spend)> = Vec::new();
    for line in UsageLedger::new(&ctx.env.paths).read_since(Some(local_day(now))) {
        let label =
            crate::controller::agent_label(&argonauts_core::names::AgentId(line.agent_id.clone()));
        match today.iter_mut().find(|(cli, _)| *cli == label) {
            Some((_, spent)) => crate::views::sessions::add_spend(spent, &line.spent()),
            None => today.push((label, line.spent())),
        }
    }
    let mut runs = ctx.services.conductor.list().await.unwrap_or_default();
    runs.sort_by_key(|run| std::cmp::Reverse(run.updated));
    Numbers {
        context,
        plan_chips,
        today,
        runs,
        now: Some(now),
    }
}

/// What the bottom line's preview draws from.
fn line_numbers(ctx: &Ctx, numbers: &Numbers) -> LineNumbers {
    let settings = ctx.settings();
    let (agent, _) = crate::controller::header_agent(ctx);
    let main = agent.agent_id();
    let mut seen = Vec::new();
    let mut clis: Vec<LineCli> = Vec::new();
    for worker in settings.crew.members() {
        let agent_id = worker.agent_id();
        if seen.contains(&agent_id) {
            continue;
        }
        let reading = ctx.services.plan_usage.get(&agent_id);
        let window = reading.as_ref().and_then(|reading| reading.window.clone());
        let used = reading.as_ref().and_then(|reading| reading.used);
        let week = window
            .as_deref()
            .is_some_and(|name| name.contains("week") || name.contains("seven"));
        clis.push(LineCli {
            label: crate::controller::agent_label(&agent_id),
            id: agent_id.0.clone(),
            main: agent_id == main,
            five_hours: used.filter(|_| !week),
            seven_days: used.filter(|_| week),
        });
        seen.push(agent_id);
    }
    // The one in use comes first, as on the line itself.
    clis.sort_by_key(|cli| !cli.main);
    let spent = numbers
        .today
        .iter()
        .fold(Spend::default(), |mut all, (_, spent)| {
            crate::views::sessions::add_spend(&mut all, spent);
            all
        });
    let mode = settings.approvals.mode();
    LineNumbers {
        mode: match mode.label() {
            "autopilot" => "autopilot on".to_owned(),
            label => format!("{label} mode on"),
        },
        branch: crate::git::git(&ctx.env.repo_root, &["branch", "--show-current"])
            .ok()
            .map(|branch| branch.trim().to_owned())
            .filter(|branch| !branch.is_empty()),
        model: agent
            .model
            .as_deref()
            .map(argonauts_core::model_catalog::display_name_from_id)
            .unwrap_or_default(),
        clis,
        spend: (spent.cost_usd > 0.0).then(|| {
            let mark = if spent.cost_estimated { "~" } else { "" };
            format!("{mark}${:.2}", spent.cost_usd)
        }),
    }
}

/// One mod's row: its file, the built-in card under it, and its numbers or why it has none.
/// `shipped` holds the built-in cards as they ship. `hidden_by_line` is the set's word on
/// whether the line draws a part of its own by this name.
fn card_row(
    ctx: &Ctx,
    dirs: &ModDirs,
    loaded: &LoadedMod,
    hidden_by_line: bool,
    shipped: &ModSet,
    numbers: &Numbers,
    ran: &HashMap<String, Ran>,
) -> ModRow {
    let file = &loaded.file;
    // Where it shows before any setting of yours, so a page says what you changed.
    let built_in = shipped
        .get(&file.name)
        .map(|built| built.clone().placed_by_lists(&UiSettings::default()).file);
    let mut needs_trust = false;
    let (reading, note) = match &file.data {
        ModSource::Builtin { source } => match sources::reading(*source, numbers) {
            Ok(reading) => (Some(reading), None),
            Err(why) => (None, Some(why)),
        },
        ModSource::Command { every, .. } => {
            if !ctx.settings().mods.commands {
                (
                    None,
                    Some("mods.commands is off, so its command doesn't run".to_owned()),
                )
            } else if !may_run(loaded, &ctx.env.state_dir) {
                needs_trust = true;
                (None, None)
            } else {
                match ran.get(&file.name) {
                    Some(ran) => match (ran.reading(*every), &ran.problem) {
                        (Some(reading), Some(why)) => (
                            Some(reading.clone()),
                            Some(format!(
                                "These are from before its last run, which failed: {why}"
                            )),
                        ),
                        (reading, why) => (reading.cloned(), why.clone()),
                    },
                    None => (
                        None,
                        Some(format!(
                            "its command runs every {}, and hasn't yet",
                            span_words(*every)
                        )),
                    ),
                }
            }
        }
    };
    let path = match &loaded.path {
        Some(path) => ctx.env.shown(path),
        None => ctx
            .env
            .shown(&files::mod_path(dirs.dir(ModOrigin::Yours), &file.name)),
    };
    ModRow {
        name: file.name.clone(),
        origin: loaded.origin,
        path,
        kind: ModRowKind::Card {
            file: Box::new(file.clone()),
            built_in: built_in.map(Box::new),
            reading,
            note,
            needs_trust,
            asks_for_line: loaded.asks_for_line && !file.places.contains(&ModPlace::BottomLine),
            hidden_by_line,
        },
    }
}

async fn view(
    ctx: &Ctx,
    state: &ModsState,
    context: Option<ContextRow>,
    open: Option<String>,
    refresh: bool,
) -> ModsView {
    let numbers = numbers(ctx, context).await;
    let inner = lock(&state.inner);
    let ui = &ctx.settings().ui;
    let shipped = ModSet::built_in();
    let mut rows: Vec<ModRow> = inner
        .set
        .mods
        .iter()
        .map(|loaded| {
            let hidden = inner.set.hidden_by_line(loaded);
            let loaded = loaded.clone().placed_by_lists(ui);
            card_row(
                ctx,
                &state.dirs,
                &loaded,
                hidden,
                &shipped,
                &numbers,
                &inner.ran,
            )
        })
        .collect();
    for own in &inner.set.own {
        let Some(built) = builtin::builtin(&own.name) else {
            continue;
        };
        let builtin::BuiltinKind::Own { about } = built.kind else {
            continue;
        };
        let path = match &own.path {
            Some(path) => ctx.env.shown(path),
            None => ctx.env.shown(&files::mod_path(
                state.dirs.dir(ModOrigin::Yours),
                &own.name,
            )),
        };
        // The line lays the project's file over yours key by key, so its page starts from
        // both and not from the one file a save goes into.
        let text = if own.name == builtin::BOTTOM_LINE {
            inner.set.line.to_toml()
        } else {
            own.text.clone()
        };
        rows.push(ModRow {
            name: own.name.clone(),
            origin: own.origin,
            path,
            kind: ModRowKind::Own {
                about: about.to_owned(),
                text,
                built_in: built.text.to_owned(),
            },
        });
    }
    ModsView {
        rows,
        problems: inner
            .set
            .problems
            .iter()
            .map(|problem| FileProblem {
                path: ctx.env.shown(&problem.path),
                problems: problem.problems.clone(),
            })
            .collect(),
        open,
        yours_dir: ctx.env.shown(&state.dirs.yours),
        project_dir: Some(ctx.env.shown(&state.dirs.project)),
        line: line_numbers(ctx, &numbers),
        refresh,
    }
}

/// Reads the folders again now, as after a save, and keeps the commands running while the
/// screen is being looked at.
fn look(state: &ModsState) {
    let mut inner = lock(&state.inner);
    inner.wanted_until = Some(Instant::now() + WANTED_FOR);
    let stamp = state.dirs.stamp();
    if inner.stamp != stamp {
        inner.stamp = stamp;
        inner.set = state.dirs.load();
    }
}

async fn send_view(
    ctx: &Ctx,
    state: &ModsState,
    context: Option<ContextRow>,
    open: Option<String>,
    refresh: bool,
) {
    let view = view(ctx, state, context, open, refresh).await;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Mods {
            view: Box::new(view),
        }),
    });
}

/// `/mods`, and `/mods <name>` with the page to open at once.
pub(crate) async fn open(
    ctx: &Ctx,
    state: &ModsState,
    context: Option<ContextRow>,
    name: Option<String>,
    refresh: bool,
) -> anyhow::Result<Vec<Effect>> {
    look(state);
    if let Some(name) = &name
        && !lock(&state.inner).set.names().contains(&name.as_str())
    {
        ctx.notice(NoteLevel::Info, no_such_mod(state, name));
        return Ok(Vec::new());
    }
    send_view(ctx, state, context, name, refresh).await;
    Ok(Vec::new())
}

fn no_such_mod(state: &ModsState, name: &str) -> String {
    let inner = lock(&state.inner);
    let named_bad = inner
        .set
        .problems
        .iter()
        .find(|problem| problem.path.file_stem().and_then(|stem| stem.to_str()) == Some(name));
    match named_bad {
        Some(problem) => format!(
            "{name}'s file didn't load: {}. /mods lists it with the rest",
            problem.problems.join("; ")
        ),
        None => format!(
            "There's no mod named {name}; the mods are {}. A new one is a file in {}",
            inner.set.names().join(", "),
            state.dirs.yours.display()
        ),
    }
}

/// A file or a reading changed: the screen gets the newer rows, for `/mods` when it's open and
/// for wherever a mod shows.
pub(crate) async fn changed(
    ctx: &Ctx,
    state: &ModsState,
    context: Option<ContextRow>,
) -> anyhow::Result<Vec<Effect>> {
    tell_waiting(ctx, state);
    send_view(ctx, state, context, None, true).await;
    Ok(Vec::new())
}

/// Says once a session that a placed project's mod waits for your trust, since until then its
/// card stays empty with no word on the screen.
fn tell_waiting(ctx: &Ctx, state: &ModsState) {
    if !ctx.settings().mods.commands {
        return;
    }
    let waiting: Vec<String> = {
        let mut inner = lock(&state.inner);
        let names: Vec<String> = inner
            .set
            .mods
            .iter()
            .filter(|loaded| {
                loaded.origin == ModOrigin::Project
                    && loaded.file.shows_somewhere()
                    && loaded.file.data.is_command()
                    && !may_run(loaded, &ctx.env.state_dir)
            })
            .map(|loaded| loaded.file.name.clone())
            .collect();
        names
            .into_iter()
            .filter(|name| inner.told_waiting.insert(name.clone()))
            .collect()
    };
    for name in waiting {
        ctx.notice(
            NoteLevel::Info,
            format!(
                "The project's mod {name} runs a command, so it waits for your trust: /mods \
                 {name}, then t"
            ),
        );
    }
}

/// The text of the mod in use, which a first save copies.
fn text_in_use(state: &ModsState, name: &str) -> String {
    let inner = lock(&state.inner);
    if let Some(own) = inner.set.own(name) {
        return own.text.clone();
    }
    let from_file = inner
        .set
        .get(name)
        .and_then(|loaded| loaded.path.as_deref())
        .and_then(|path| std::fs::read_to_string(path).ok());
    from_file
        .or_else(|| builtin::builtin(name).map(|built| built.text.to_owned()))
        .unwrap_or_default()
}

/// What a mod's page asked for: save its keys, delete its file, or trust it.
pub(crate) async fn change(
    ctx: &mut Ctx,
    state: &ModsState,
    context: Option<ContextRow>,
    change: ModChange,
) -> anyhow::Result<Vec<Effect>> {
    match change {
        ModChange::Save {
            name,
            pairs,
            origin,
        } => {
            let (pairs, elsewhere) = places_apart(ctx, state, &name, pairs);
            let mut said = Vec::new();
            if !pairs.is_empty() {
                let path = save_keys(ctx, state, &name, &pairs, origin)?;
                said.push(format!("Saved {}", ctx.env.shown(&path)));
            }
            if let Some(on) = elsewhere.on_line
                && set_on_line(ctx, state, &name, on)?
            {
                said.push(on_line_words(&name, on));
            }
            if !said.is_empty() {
                ctx.notice(NoteLevel::Info, said.join(". "));
            }
            set_in_lists(ctx, &elsewhere)?;
        }
        ModChange::Reset { name, origin } => {
            anyhow::ensure!(is_mod_name(&name), "{name} isn't a mod's name");
            let dir = state.dirs.dir(origin);
            let path = files::mod_path(dir, &name);
            if files::reset(dir, &name)? {
                ctx.notice(
                    NoteLevel::Info,
                    format!(
                        "Deleted {}; the built-in {name} is back",
                        ctx.env.shown(&path)
                    ),
                );
            }
        }
        ModChange::Trust { name } => {
            let path = lock(&state.inner)
                .set
                .get(&name)
                .filter(|loaded| loaded.origin == ModOrigin::Project)
                .and_then(|loaded| loaded.path.clone());
            let Some(path) = path else {
                ctx.notice(NoteLevel::Info, format!("{name} isn't a project's mod"));
                return Ok(Vec::new());
            };
            trust_mod_files(ctx, state, &name, &path)?;
            ctx.notice(
                NoteLevel::Info,
                format!(
                    "Trusted {} as it is now. A change to it asks again",
                    ctx.env.shown(&path)
                ),
            );
        }
    }
    look(state);
    send_view(ctx, state, context, None, true).await;
    Ok(Vec::new())
}

/// The mod `name` as loaded, with its places as the place lists in your settings have them.
fn loaded_mod(ctx: &Ctx, state: &ModsState, name: &str) -> Option<LoadedMod> {
    let loaded = lock(&state.inner).set.get(name).cloned()?;
    Some(loaded.placed_by_lists(&ctx.settings().ui))
}

/// What a save of a mod's places changes outside the mod's own file.
#[derive(Debug, Default)]
struct Elsewhere {
    /// Onto the bottom line, or off it. The line's own file lists what it shows.
    on_line: Option<bool>,
    /// A built-in card into a place list in your settings, or out of it. The band above the
    /// input and the Overview's tabs draw those cards from the lists.
    lists: Vec<(ModName, Place, bool)>,
}

/// A save of a mod's keys, with the places its own file doesn't decide taken apart: the pairs
/// to write into the mod's file, and what the save changes elsewhere. A tick for the bottom
/// line is written in the line's file, and a built-in card's tick in the place lists. The
/// mod's file never gains such a place this way, and loses a `bottom-line` it had only when the
/// mod leaves the line. A `places` that then says nothing new isn't written at all.
fn places_apart(
    ctx: &Ctx,
    state: &ModsState,
    name: &str,
    mut pairs: Vec<(String, String)>,
) -> (Vec<(String, String)>, Elsewhere) {
    let mut elsewhere = Elsewhere::default();
    let Some(at) = pairs.iter().position(|(key, _)| key == "places") else {
        return (pairs, elsewhere);
    };
    let Some(loaded) = loaded_mod(ctx, state, name) else {
        return (pairs, elsewhere);
    };
    let Ok(saved) = pairs[at].1.parse::<toml_edit::Value>() else {
        return (pairs, elsewhere);
    };
    let saved: Vec<ModPlace> = saved
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|place| ModPlace::from_word(place.as_str()?))
        .collect();
    let had = &loaded.file.places;
    let card = builtin::closed_card(name);
    for place in ModPlace::ALL {
        let now = saved.contains(&place);
        if let (Some(card), Some(list)) = (card, place.list())
            && now != had.contains(&place)
        {
            elsewhere.lists.push((card, list, now));
        }
    }
    let on_line = saved.contains(&ModPlace::BottomLine);
    elsewhere.on_line = Some(on_line);
    // The places the mod's own file decides.
    let own = |place: &ModPlace| {
        *place != ModPlace::BottomLine && (card.is_none() || place.list().is_none())
    };
    let own_of = |places: &[ModPlace]| -> Vec<ModPlace> {
        places.iter().copied().filter(|place| own(place)).collect()
    };
    let asked = loaded.asks_for_line;
    let leaves_the_line = asked && !on_line;
    if own_of(&saved) == own_of(had) && !leaves_the_line {
        pairs.remove(at);
    } else {
        let written = saved
            .iter()
            .filter(|place| own(place) || (**place == ModPlace::BottomLine && asked))
            .map(|place| toml::Value::String(place.word().to_owned()))
            .collect();
        pairs[at].1 = toml::Value::Array(written).to_string();
    }
    (pairs, elsewhere)
}

/// Writes a built-in card into the place lists a save ticked it into, and out of the ones it
/// was ticked out of. Each is one setting, saved where its value came from and announced.
fn set_in_lists(ctx: &mut Ctx, elsewhere: &Elsewhere) -> anyhow::Result<()> {
    for &(the_mod, place, show) in &elsewhere.lists {
        let change = if show {
            ModPlaceChange::Show { place }
        } else {
            ModPlaceChange::Hide { place }
        };
        let setting = change.setting(the_mod, &ctx.settings().ui);
        if let Some((key, value)) = setting {
            crate::controller::settings::set_where_it_came_from(ctx, key, value, None)?;
        }
    }
    Ok(())
}

/// Puts the mod `name` on the bottom line, last on its right side, or takes it off. The line
/// shows what its own file lists, so this writes that file: the project's when the project
/// has one, else yours. False when the line already had it so.
fn set_on_line(ctx: &mut Ctx, state: &ModsState, name: &str, on: bool) -> anyhow::Result<bool> {
    use argonauts_core::settings::mods::bottom_line::PageEdits as _;
    let (before, origin) = {
        let inner = lock(&state.inner);
        let origin = inner
            .set
            .own(builtin::BOTTOM_LINE)
            .map_or(ModOrigin::Yours, |own| save_origin(own.origin));
        (inner.set.line.clone(), origin)
    };
    if before.has(name) == on {
        return Ok(false);
    }
    let mut after = before.clone();
    if on {
        after.right.push(name.to_owned());
    } else {
        after.left.retain(|part| part != name);
        after.right.retain(|part| part != name);
    }
    let pairs = after.changes_from(&before);
    save_keys(ctx, state, builtin::BOTTOM_LINE, &pairs, origin)?;
    Ok(true)
}

fn on_line_words(name: &str, on: bool) -> String {
    if on {
        format!(
            "{name} now shows on the bottom line, last on the right. /mods {} moves it",
            builtin::BOTTOM_LINE
        )
    } else {
        format!("{name} no longer shows on the bottom line")
    }
}

/// Writes keys into a mod's file, yours or the project's.
fn save_keys(
    ctx: &mut Ctx,
    state: &ModsState,
    name: &str,
    pairs: &[(String, String)],
    origin: ModOrigin,
) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(is_mod_name(name), "{name} isn't a mod's name");
    // A change you make keeps a file you trusted trusted. It never trusts one you didn't:
    // hiding, pinning or placing a cloned repository's mod doesn't say its command may run.
    let was_trusted = origin == ModOrigin::Project
        && lock(&state.inner)
            .set
            .get(name)
            .is_some_and(|loaded| may_run(loaded, &ctx.env.state_dir));
    let dir = state.dirs.dir(origin);
    let start = text_in_use(state, name);
    let path = files::save(dir, name, &start, pairs)?;
    if was_trusted {
        trust_mod_files(ctx, state, name, &path)?;
    }
    Ok(path)
}

/// Trusts a project mod's file, and the program beside it that its command names.
fn trust_mod_files(
    ctx: &mut Ctx,
    state: &ModsState,
    name: &str,
    path: &Path,
) -> anyhow::Result<()> {
    argonauts_core::paths::create_private_dir_all(&ctx.env.state_dir)?;
    ctx.settings.trust_mut().trust(path)?;
    look(state);
    let programs = lock(&state.inner)
        .set
        .get(name)
        .map(programs_beside)
        .unwrap_or_default();
    for program in programs {
        ctx.settings.trust_mut().trust(&program)?;
    }
    Ok(())
}

/// `/mods <name> <words>`, `/pin <name>` and `/unpin <name>` for a mod that is a file. No
/// words open its page. One word puts it in a place or takes it out, which is one key of its
/// file.
pub(crate) async fn file_command(
    ctx: &mut Ctx,
    state: &ModsState,
    context: Option<ContextRow>,
    name: String,
    words: Vec<String>,
) -> anyhow::Result<Vec<Effect>> {
    look(state);
    let Some(word) = words.first() else {
        return open(ctx, state, context, Some(name), false).await;
    };
    let Some(loaded) = loaded_mod(ctx, state, &name) else {
        let why = if builtin::draws_itself(&name) {
            format!("{name} draws itself; /mods {name} opens its page")
        } else {
            no_such_mod(state, &name)
        };
        ctx.notice(NoteLevel::Info, why);
        return Ok(Vec::new());
    };
    // Its own command hides it everywhere or shows it again, and its places stay.
    if let ("toggle" | "show" | "hide", 1) = (word.as_str(), words.len()) {
        let shown = match word.as_str() {
            "show" => true,
            "hide" => false,
            _ => !loaded.file.shown,
        };
        if shown == loaded.file.shown {
            ctx.notice(NoteLevel::Info, "Already so; nothing changed");
            return Ok(Vec::new());
        }
        let after = ModFile {
            shown,
            ..loaded.file.clone()
        };
        let pairs = after.changes_from(&loaded.file);
        save_keys(ctx, state, &name, &pairs, save_origin(loaded.origin))?;
        let places: Vec<&str> = after.places.iter().map(|place| place.phrase()).collect();
        let said = match (shown, places.is_empty()) {
            (false, _) => format!("{name} is hidden. /{name} shows it again"),
            (true, false) => format!("{name} shows again {}", places.join(", ")),
            (true, true) => format!(
                "{name} isn't hidden any more, and it has no place yet. /mods {name} above \
                 puts it above the input"
            ),
        };
        ctx.notice(NoteLevel::Info, said);
        look(state);
        send_view(ctx, state, context, None, true).await;
        return Ok(Vec::new());
    }
    let (place, show) = match (word.as_str(), words.len()) {
        ("pin", 1) => (ModPlace::Above, Some(true)),
        ("unpin", 1) => (ModPlace::Above, Some(false)),
        (word, 1) => match ModPlace::from_word(word) {
            Some(place) => (place, None),
            None => {
                ctx.notice(
                    NoteLevel::Info,
                    format!(
                        "After a mod's name, use a place ({}), or nothing for its page",
                        argonauts_core::settings::mods::check::place_words()
                    ),
                );
                return Ok(Vec::new());
            }
        },
        _ => {
            ctx.notice(NoteLevel::Info, format!("Use /mods {name} [place]"));
            return Ok(Vec::new());
        }
    };
    let mut after = loaded.file.clone();
    // A hidden mod keeps its places, so what counts is the list and not what shows now.
    let placed = after.places.contains(&place);
    // A place alone toggles, as it does for the built-in cards.
    let show = show.unwrap_or(!placed);
    if show == placed {
        ctx.notice(NoteLevel::Info, "Already so; nothing changed");
        return Ok(Vec::new());
    }
    if show {
        after.places.push(place);
        after.places.sort();
    } else {
        after.places.retain(|have| *have != place);
    }
    let pairs = after.changes_from(&loaded.file);
    let (pairs, elsewhere) = places_apart(ctx, state, &name, pairs);
    if !pairs.is_empty() {
        save_keys(ctx, state, &name, &pairs, save_origin(loaded.origin))?;
    }
    let said = match elsewhere.on_line {
        Some(on) if place == ModPlace::BottomLine => {
            set_on_line(ctx, state, &name, on)?;
            on_line_words(&name, on)
        }
        _ if show => format!("{name} now shows {}", place.phrase()),
        _ => format!("{name} no longer shows {}", place.phrase()),
    };
    let said = match (show, after.shown) {
        (true, false) => format!("{said}, once /{name} shows it again"),
        _ => said,
    };
    ctx.notice(NoteLevel::Info, said);
    set_in_lists(ctx, &elsewhere)?;
    look(state);
    send_view(ctx, state, context, None, true).await;
    Ok(Vec::new())
}

/// Where a change to a mod is saved: the project's stays the project's, anything else is
/// yours.
fn save_origin(origin: ModOrigin) -> ModOrigin {
    match origin {
        ModOrigin::Project => ModOrigin::Project,
        ModOrigin::BuiltIn | ModOrigin::Yours => ModOrigin::Yours,
    }
}

/// What Doctor says about mod files: nothing when all load, else each file with why not.
/// Every mod in use, the built-in ones first.
pub(crate) fn loaded(env: &AppEnv) -> ModSet {
    ModDirs::of(env).load()
}

pub(crate) fn doctor_lines(env: &AppEnv) -> Vec<String> {
    let set = ModDirs::of(env).load();
    problem_lines(&set, |path| env.shown(path))
}

fn problem_lines(set: &ModSet, shown: impl Fn(&Path) -> String) -> Vec<String> {
    let mut lines = Vec::new();
    for problem in &set.problems {
        lines.push(format!(
            "mod {} · didn't load, and the rest work without it",
            shown(&problem.path)
        ));
        lines.extend(problem.problems.iter().map(|why| format!("  {why}")));
    }
    for loaded in set.mods.iter().filter(|loaded| set.hidden_by_line(loaded)) {
        let Some(path) = &loaded.path else {
            continue;
        };
        lines.push(format!(
            "mod {} · never shows on the bottom line",
            shown(path)
        ));
        lines.push(format!("  {}", line_clash(&loaded.file.name)));
    }
    lines
}

/// Why a mod of this name never shows on the bottom line, and what to do.
fn line_clash(name: &str) -> String {
    format!(
        "the line draws its own {name} part by that name; rename the file to put this mod there"
    )
}

/// The same for `argonauts doctor`, each file marked `!` as its other problems are: your
/// mods, and this repository's when the folder is in one.
pub fn doctor_problems(repo: Option<&Path>) -> Vec<String> {
    let lines = match AppEnv::discover(repo) {
        Ok(env) => doctor_lines(&env),
        Err(_) => match argonauts_core::paths::Paths::from_env() {
            Ok(paths) => {
                let set = ModSet::load(Some(&paths.home().join(MODS_DIR)), None);
                problem_lines(&set, |path| path.display().to_string())
            }
            Err(_) => Vec::new(),
        },
    };
    marked(lines)
}

/// Each file's line with the `!` the command line's doctor puts before a problem. The reasons
/// under it stay as they are.
fn marked(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .map(|line| match line.starts_with(' ') {
            true => line,
            false => format!("! {line}"),
        })
        .collect()
}

#[cfg(test)]
mod tests;
