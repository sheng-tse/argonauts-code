//! The Overview tab's numbers: every CLI's work on this computer, summed from the usage
//! ledger, the work ledger and what the import took from Claude Code's and Codex's own
//! session files. Nothing here asks a model.

mod build;
mod import;
mod records;

#[cfg(test)]
pub(crate) use build::overview as build_overview;
#[cfg(test)]
pub(crate) use records::Record;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::overview::{ModelTokens, OverviewPeriod};
use argonauts_core::pricing::{ListPrice, PriceList};
use argonauts_core::registry::default_run;
use argonauts_core::runtimes::RUNTIMES;
use argonauts_core::settings::endpoints::endpoint_settings;
use argonauts_core::{NoteLevel, Paths};
use argonauts_tui::format::WallClock;
use time::OffsetDateTime;

pub(crate) use self::build::Sources;
pub(crate) use self::build::through_argonauts;
use self::import::Locations;
pub(crate) use self::import::doctor_lines;
use self::records::IMPORTED;
pub(crate) use self::records::Store;
use crate::controller::{Ctx, Effect};
use crate::usage_ledger::{LedgerLine, UsageLedger, local_day};
use crate::work_ledger::WorkLedger;

/// Answers the Overview tab: first its question when it carries the answer, then its numbers.
pub(crate) async fn view(
    ctx: &mut Ctx,
    period: OverviewPeriod,
    answer: Option<bool>,
) -> anyhow::Result<Vec<Effect>> {
    let paths = ctx.env.paths.clone();
    let store = Store::new(&paths);
    if let Some(yes) = answer
        && let Err(err) = answered(&store, yes)
    {
        ctx.notice(
            NoteLevel::Warn,
            format!("Couldn't keep your answer about the import: {err}"),
        );
    }
    let state = store.state();
    if state.import == Some(true) {
        import::start_if_due(
            Store::new(&paths),
            Locations::from_env(),
            answer == Some(true),
        );
    }
    let status = import::status(&state);
    // The CLIs the import can find, by the names the screen gives them.
    let names: BTreeMap<&str, String> = ["claude-code", "codex", "antigravity-cli", "opencode"]
        .into_iter()
        .map(|id| (id, ctx.names.cli(id)))
        .collect();
    let colors = ctx.settings().overview.year_colors;
    let prices = price_list(ctx);
    let spelling = ctx.names.names();
    let data = tokio::task::spawn_blocking(move || {
        let now = OffsetDateTime::now_utc();
        let records = match state.import {
            Some(true) => store.records(),
            _ => Arc::new(Vec::new()),
        };
        let (lines, work) = ledgers(&paths);
        let sources = Sources {
            records: &records,
            lines: &lines,
            work: &work,
            clock: WallClock::default(),
            prices: &prices,
            names: &spelling,
        };
        let today = local_day(now);
        let mut data = build::overview(&sources, period, today, status, colors, &|id| {
            names.get(id).cloned().unwrap_or_else(|| id.to_owned())
        });
        data.found = import::found_line(data.found.take(), &state);
        // Claude Code's own count is read only once you said yes to reading its folder.
        if state.import == Some(true) {
            let first = period.first_day(today);
            data.claude_stats = import::claude_count(&Locations::from_env(), first);
        }
        data
    })
    .await?;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Overview {
            overview: Box::new(data),
        }),
    });
    Ok(Vec::new())
}

/// Your prices, then the public model list's, with every model run on this computer free.
fn price_list(ctx: &Ctx) -> PriceList {
    let settings = ctx.settings();
    let yours = settings.overview.prices.clone();
    let public: Vec<(String, ListPrice)> = ctx
        .services
        .registry
        .catalog()
        .prices
        .iter()
        .map(|price| (price.model.clone(), price.list_price()))
        .collect();
    let local: Vec<String> = RUNTIMES
        .iter()
        .map(|runtime| runtime.id.to_owned())
        .chain(settings.endpoints.keys().cloned())
        .filter(|id| endpoint_settings(settings, id).is_some_and(|endpoint| endpoint.local))
        .collect();
    let registry = ctx.services.registry.as_ref();
    let defaults: Vec<(String, String)> = settings
        .agents
        .keys()
        .filter_map(|cli| Some((cli.clone(), default_run(registry, cli)?)))
        .collect();
    PriceList::new(yours, public, local).with_cli_defaults(defaults)
}

/// Keeps your answer, once.
fn answered(store: &Store, yes: bool) -> std::io::Result<()> {
    let mut state = store.state();
    state.import = Some(yes);
    store.save_state(&state)
}

/// Every model that has run here with its tokens in all, from every line of the usage
/// ledger in `usage_dir` and the import beside it: the list the Overview's numbers carry,
/// for the other tabs.
pub(crate) fn all_time_models(usage_dir: &Path, lines: &[LedgerLine]) -> Vec<ModelTokens> {
    let store = Store::at(&usage_dir.join(IMPORTED));
    let records = match store.state().import {
        Some(true) => store.records(),
        _ => Arc::new(Vec::new()),
    };
    build::all_time_models(&records, lines)
}

/// Every line of the usage ledger and the work ledger.
fn ledgers(paths: &Paths) -> (Vec<LedgerLine>, Vec<crate::work_ledger::WorkLine>) {
    let ledger = UsageLedger::new(paths);
    ledger.rebuilt_once();
    let lines = ledger.read_since(None);
    let work = WorkLedger::beside(&paths.usage_dir()).read_since(None);
    (lines, work)
}
