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
use std::sync::Arc;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::overview::OverviewPeriod;
use argonauts_core::{NoteLevel, Paths};
use argonauts_tui::format::WallClock;
use time::OffsetDateTime;

pub(crate) use self::build::Sources;
use self::import::Locations;
pub(crate) use self::records::Store;
use crate::controller::{Ctx, Effect};
use crate::usage_ledger::{UsageLedger, local_day};
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
        .map(|id| (id, ctx.names.harness(id)))
        .collect();
    let colors = ctx.settings().overview.year_colors;
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
        };
        build::overview(&sources, period, local_day(now), status, colors, &|id| {
            names.get(id).cloned().unwrap_or_else(|| id.to_owned())
        })
    })
    .await?;
    ctx.send(AppEvent::View {
        data: Box::new(ViewData::Overview {
            overview: Box::new(data),
        }),
    });
    Ok(Vec::new())
}

/// Keeps your answer, once.
fn answered(store: &Store, yes: bool) -> std::io::Result<()> {
    let mut state = store.state();
    state.import = Some(yes);
    store.save_state(&state)
}

/// Every line of the usage ledger and the work ledger.
fn ledgers(
    paths: &Paths,
) -> (
    Vec<crate::usage_ledger::LedgerLine>,
    Vec<crate::work_ledger::WorkLine>,
) {
    let lines = UsageLedger::new(paths).read_since(None);
    let work = WorkLedger::beside(&paths.usage_dir()).read_since(None);
    (lines, work)
}
