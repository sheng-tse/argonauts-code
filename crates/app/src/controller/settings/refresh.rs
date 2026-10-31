//! The CLIs' model lists, read again in the background at start and whenever `/model` opens.
//! The picker shows the cached list with when it was read, and takes each new one as it lands.
//!
//! As `/model` opens, every source that lists by running or asking something is asked again,
//! all at once, and each answer reaches the picker as it lands, so a slow source never holds
//! back a fast one. While the picker stays open, the local servers are asked every 3 s, so a
//! model pulled meanwhile shows up; a local list that changed asks OpenCode once more, since
//! OpenCode lists the local models too. Nothing here makes a model call.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use argonauts_core::agents::runtimes::runtime;
use argonauts_core::app::model_picker::ModelSource;
use argonauts_core::app::{AppEvent, ModelChoice, ViewData};
use argonauts_core::base::names::{ModelRef, RoleName};
use argonauts_core::settings::endpoints::endpoint_settings;
use time::OffsetDateTime;

use super::memory_facts::{self, Reading};
use super::{Handled, SettingsState, models};
use crate::controller::{Ctx, Internal};
use crate::host::memory::{self, MeasuredRam, Ollama};

/// How often the local servers are asked while `/model` is open.
const TICK: Duration = Duration::from_secs(3);
/// Asked again when a local server's list changes, since it lists those models too.
const OPENCODE: &str = "opencode";

/// The model list last sent, so a refresh that lands knows which picker to bring up to date.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ModelsShown {
    /// `None` is the main agent.
    role: Option<RoleName>,
    refreshed: Option<OffsetDateTime>,
}

/// The sources being asked one by one, and the watch while `/model` is open.
#[derive(Debug, Default)]
pub(crate) struct Asking {
    /// By CLI or endpoint id.
    now: BTreeSet<String>,
    /// Those of them a tick asks while the picker is open, whose rows don't say they're asked:
    /// a local server answers within the tick, and saying so every 3 s would only flicker.
    quiet: BTreeSet<String>,
    /// What each source being asked listed before, to tell whether its answer changed it.
    before: BTreeMap<String, Vec<ModelRef>>,
    /// Set while `/model`'s picker is open; the tick stops once it's cleared.
    watch: Option<Arc<AtomicBool>>,
    /// What this machine gives models and what the local ones take, as read last.
    reading: Option<Reading>,
    /// A reading is under way.
    reading_now: bool,
}

/// Starts a refresh unless one is running or discovery is off. Discovery never calls a model.
pub(crate) fn start(ctx: &Ctx, settings: &mut SettingsState) {
    if settings.refreshing || !ctx.settings().registry.discover {
        return;
    }
    settings.refreshing = true;
    let registry = Arc::clone(&ctx.services.registry);
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        registry.discover().await;
        let _ = internal.send(Internal::ModelsRefreshed);
    });
}

/// Asks every source again, each on its own, as `/model` opens. Claude Code's aliases need
/// nothing run, so they aren't asked.
pub(crate) fn ask_each(ctx: &Ctx, settings: &mut SettingsState) {
    if !ctx.settings().registry.discover {
        return;
    }
    for cli in ctx.services.registry.sources() {
        ask(ctx, settings, &cli);
    }
    read_memory(ctx, settings);
}

/// Where Ollama answers: your `[endpoints.ollama]`, else its own address while
/// `registry.local_servers` lets the servers on this computer be asked. `None` when nothing
/// may be asked.
fn ollama_url(ctx: &Ctx) -> Option<String> {
    let settings = ctx.settings();
    let yours = settings
        .endpoints
        .get("ollama")
        .and_then(|endpoint| endpoint.base_url.clone());
    let own = || {
        settings
            .registry
            .local_servers
            .then(|| runtime("ollama").map(|ollama| ollama.address().to_owned()))
            .flatten()
    };
    yours.or_else(own)
}

/// Reads this machine's memory and what Ollama's models take of it, where OpenCode lists
/// models that run on Ollama. It asks Ollama and the system only, and loads nothing.
fn read_memory(ctx: &Ctx, settings: &mut SettingsState) {
    let runs_ollama = ctx.services.registry.list().iter().any(|entry| {
        entry.id.agent == OPENCODE
            && entry
                .id
                .model
                .as_deref()
                .is_some_and(|model| model.starts_with("ollama/"))
    });
    if settings.sources.reading_now || !runs_ollama || !ctx.settings().registry.discover {
        return;
    }
    let Some(url) = ollama_url(ctx) else {
        return;
    };
    settings.sources.reading_now = true;
    let measured = MeasuredRam::in_home(ctx.env.paths.home());
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        let ollama = match memory::ask_ollama(&url).await {
            Some(mut ollama) => {
                measured.note(&url, &mut ollama);
                ollama
            }
            None => Ollama::default(),
        };
        let reading = memory::machine(ollama.in_use())
            .await
            .map(|machine| Box::new(Reading { machine, ollama }));
        let _ = internal.send(Internal::MemoryRead(reading));
    });
}

/// A reading of this machine's memory came back.
pub(crate) fn memory_read(
    ctx: &Ctx,
    settings: &mut SettingsState,
    reading: Option<Box<Reading>>,
) -> Handled {
    settings.sources.reading_now = false;
    if let Some(reading) = reading {
        settings.sources.reading = Some(*reading);
    }
    send_update(ctx, settings);
    Ok(Vec::new())
}

/// The choices for `role`, with what each local model takes of this machine where that's
/// been read.
fn choices(ctx: &Ctx, settings: &SettingsState, role: Option<RoleName>) -> Vec<ModelChoice> {
    let mut choices = models::choices(ctx, role);
    if let Some(reading) = &settings.sources.reading {
        let registry = &ctx.services.registry;
        memory_facts::annotate(
            &mut choices,
            reading,
            &registry.list(),
            &registry.catalog().local,
        );
    }
    choices
}

/// Asks one source again, unless it's being asked already.
fn ask(ctx: &Ctx, settings: &mut SettingsState, cli: &str) {
    if !settings.sources.now.insert(cli.to_owned()) {
        return;
    }
    settings.sources.quiet.remove(cli);
    settings
        .sources
        .before
        .insert(cli.to_owned(), listed_models(ctx, cli));
    let registry = Arc::clone(&ctx.services.registry);
    let internal = ctx.internal.clone();
    let cli = cli.to_owned();
    tokio::spawn(async move {
        registry.discover_source(&cli).await;
        let _ = internal.send(Internal::ModelSourceListed { cli });
    });
}

fn listed_models(ctx: &Ctx, cli: &str) -> Vec<ModelRef> {
    ctx.services
        .registry
        .list()
        .into_iter()
        .filter(|entry| entry.id.agent == cli && entry.discovered)
        .map(|entry| entry.id)
        .collect()
}

/// One source answered: the picker showing a model list gets it, and a local server whose
/// list changed has OpenCode asked again.
pub(crate) fn source_listed(ctx: &Ctx, settings: &mut SettingsState, cli: &str) -> Handled {
    settings.sources.now.remove(cli);
    settings.sources.quiet.remove(cli);
    let before = settings.sources.before.remove(cli).unwrap_or_default();
    let changed = before != listed_models(ctx, cli);
    let opencode_listed = ctx
        .services
        .registry
        .sources()
        .iter()
        .any(|id| id == OPENCODE);
    if changed && is_local(ctx, cli) && opencode_listed {
        ask(ctx, settings, OPENCODE);
    }
    send_update(ctx, settings);
    Ok(Vec::new())
}

/// A server on this computer: one of yours set `local`, or a known one such as Ollama, which
/// discovery asks with or without a table.
fn is_local(ctx: &Ctx, cli: &str) -> bool {
    endpoint_settings(ctx.settings(), cli).is_some_and(|endpoint| endpoint.local)
}

/// `/model`'s picker opened, so the local servers are asked every 3 s until it closes.
pub(crate) fn watch(ctx: &Ctx, settings: &mut SettingsState, watching: bool) {
    if !watching {
        if let Some(watch) = settings.sources.watch.take() {
            watch.store(false, Ordering::Relaxed);
        }
        return;
    }
    if settings.sources.watch.is_some() {
        return;
    }
    let watch = Arc::new(AtomicBool::new(true));
    settings.sources.watch = Some(Arc::clone(&watch));
    let internal = ctx.internal.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            if !watch.load(Ordering::Relaxed) || internal.send(Internal::ModelsTick).is_err() {
                break;
            }
        }
    });
}

/// Each local server is asked again, starting nothing, and the open picker gets the lists as
/// they stand, with plan usage as its feed has it now and every time worded afresh.
pub(crate) fn tick(ctx: &Ctx, settings: &mut SettingsState) -> Handled {
    if settings.sources.watch.is_none() || !ctx.settings().registry.discover {
        return Ok(Vec::new());
    }
    let local: Vec<String> = ctx
        .services
        .registry
        .sources()
        .into_iter()
        .filter(|cli| is_local(ctx, cli))
        .collect();
    for cli in local {
        if settings.sources.now.contains(&cli) {
            continue;
        }
        ask(ctx, settings, &cli);
        settings.sources.quiet.insert(cli);
    }
    read_memory(ctx, settings);
    send_update(ctx, settings);
    Ok(Vec::new())
}

/// The sources asked now that say so on their rows: all but a tick's.
fn asked_aloud(settings: &SettingsState) -> impl Iterator<Item = &String> {
    settings
        .sources
        .now
        .iter()
        .filter(|cli| !settings.sources.quiet.contains(*cli))
}

/// Each source with when it last listed and whether it's being asked now.
fn sources(ctx: &Ctx, settings: &SettingsState) -> Vec<ModelSource> {
    ctx.services
        .registry
        .sources()
        .into_iter()
        .map(|cli| {
            let (listed, failed) = ctx.services.registry.source_listed(&cli);
            ModelSource {
                asking: asked_aloud(settings).any(|asked| *asked == cli),
                note: ctx.services.registry.source_note(&cli),
                cli,
                listed,
                failed,
            }
        })
        .collect()
}

/// The models `role` may use, with how fresh the list is.
pub(crate) fn view(ctx: &Ctx, settings: &mut SettingsState, role: Option<RoleName>) -> ViewData {
    let role = role.filter(|role| *role != RoleName::Main);
    let refreshed = ctx.services.registry.refreshed();
    settings.shown = Some(ModelsShown { role, refreshed });
    ViewData::Models {
        choices: choices(ctx, settings, role),
        role,
        refreshed,
        refreshing: settings.refreshing || asked_aloud(settings).next().is_some(),
        update: false,
        sources: sources(ctx, settings),
        memory: settings
            .sources
            .reading
            .as_ref()
            .map(|reading| reading.machine),
    }
}

/// The open picker's list as it stands now.
fn send_update(ctx: &Ctx, settings: &mut SettingsState) {
    let refreshing = settings.refreshing || asked_aloud(settings).next().is_some();
    let sources = sources(ctx, settings);
    let memory = settings
        .sources
        .reading
        .as_ref()
        .map(|reading| reading.machine);
    let Some(role) = settings.shown.as_ref().map(|shown| shown.role) else {
        return;
    };
    let choices = choices(ctx, settings, role);
    let Some(shown) = settings.shown.as_mut() else {
        return;
    };
    let refreshed = ctx.services.registry.refreshed();
    shown.refreshed = refreshed;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Models {
            choices,
            role,
            refreshed,
            refreshing,
            update: true,
            sources,
            memory,
        }),
    });
}

/// A full refresh merged: the picker showing a model list gets the new one. A list read no
/// later than the one it shows changes nothing, so nothing goes out.
pub(crate) fn merged(ctx: &Ctx, settings: &mut SettingsState) -> Handled {
    settings.refreshing = false;
    let refreshed = ctx.services.registry.refreshed();
    let Some(shown) = settings.shown.as_ref() else {
        return Ok(Vec::new());
    };
    if shown.refreshed == refreshed {
        return Ok(Vec::new());
    }
    send_update(ctx, settings);
    Ok(Vec::new())
}
