//! What the screen calls each CLI and model: the adapters' names and the model catalog, sent
//! before the header, when the model lists are read again, and when settings change, so the
//! screen reads "Opus 5.5 on Claude Code" rather than ids.

use std::collections::BTreeMap;

use argonauts_core::app::AppEvent;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::ModelRef;

use super::Ctx;

/// The names last sent, so an unchanged set goes out once. Notices name agents by them too,
/// as the screen does.
#[derive(Debug, Default)]
pub(crate) struct NamesSent(Option<DisplayNames>);

impl NamesSent {
    /// `Opus 5.5 on Claude Code`.
    pub(crate) fn worker(&self, worker: &ModelRef) -> String {
        match &self.0 {
            Some(names) => names.worker(worker),
            None => DisplayNames::default().worker(worker),
        }
    }

    /// The worker with the model it runs when it names none: your setting for its CLI, else
    /// the CLI's own default.
    pub(crate) fn running(&self, worker: &ModelRef) -> ModelRef {
        match &self.0 {
            Some(names) => names.running(worker),
            None => worker.clone(),
        }
    }

    /// `Claude Code`.
    pub(crate) fn cli(&self, id: &str) -> String {
        match &self.0 {
            Some(names) => names.cli(id),
            None => DisplayNames::default().cli(id),
        }
    }

    /// The names last sent, or the ones Argonauts Code knows without asking before any went.
    pub(crate) fn names(&self) -> DisplayNames {
        self.0.clone().unwrap_or_default()
    }

    /// `Opus 5.5` for `opus` on `claude-code`.
    pub(crate) fn model(&self, cli: &str, model: &str) -> Option<String> {
        match &self.0 {
            Some(names) => names.model(cli, Some(model)),
            None => DisplayNames::default().model(cli, Some(model)),
        }
    }
}

pub(crate) fn send(ctx: &mut Ctx) {
    let names = names(ctx);
    if ctx.names.0.as_ref() == Some(&names) {
        return;
    }
    ctx.send(AppEvent::Names {
        names: Box::new(names.clone()),
    });
    ctx.names.0 = Some(names);
}

/// An adapter list that fails leaves the CLIs to the names Argonauts Code knows them by.
fn names(ctx: &Ctx) -> DisplayNames {
    let clis = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .map(|adapters| {
            adapters
                .iter()
                .map(|adapter| (adapter.id().to_owned(), adapter.display_name().to_owned()))
                .collect()
        })
        .unwrap_or_else(|_| BTreeMap::new());
    let configured: BTreeMap<String, String> = ctx
        .settings()
        .agents
        .iter()
        .filter_map(|(id, agent)| Some((id.clone(), agent.model.clone()?)))
        .collect();
    // A session that names no model may print its row before it says which model it runs, so
    // it's named by its CLI's default from the start, as the header is.
    let defaults = cli_defaults(ctx, clis.keys().map(String::as_str));
    DisplayNames {
        clis,
        catalog: ctx.services.registry.catalog(),
        configured,
        defaults,
    }
}

/// The model each CLI in `clis` runs when nothing names one: the one its model list marks, else
/// the one the catalog's "Default (recommended)" runs. A CLI with neither is left out.
pub(crate) fn cli_defaults<'a>(
    ctx: &Ctx,
    clis: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let registry = &ctx.services.registry;
    let catalog = registry.catalog();
    let listed = registry.list();
    clis.into_iter()
        .filter_map(|cli| {
            let marked = listed
                .iter()
                .find(|entry| entry.cli_default && entry.id.agent == cli)
                .and_then(|entry| entry.id.model.clone());
            let id = ModelRef::new(cli, None);
            let model = marked.or_else(|| catalog.entry(&id)?.runs.clone())?;
            Some((cli.to_owned(), model))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::AppEvent;
    use argonauts_core::names::ModelRef;

    use crate::controller::test_support::TestDriver;

    fn worker(text: &str) -> ModelRef {
        text.parse().unwrap()
    }

    /// First of all, so the header and the bottom line name the agent as the rest will.
    #[tokio::test]
    async fn the_screen_learns_the_names_first() {
        let mut driver = TestDriver::start().await;
        driver
            .until(|event| matches!(event, AppEvent::Status { .. }))
            .await;
        let AppEvent::Names { names } = &driver.seen()[0] else {
            panic!("{:#?}", driver.seen());
        };
        let sets = driver
            .seen()
            .iter()
            .filter(|event| matches!(event, AppEvent::Names { .. }))
            .count();
        assert_eq!(sets, 1, "an unchanged set goes out once");
        assert_eq!(names.clis.get("codex").map(String::as_str), Some("Codex"));
        assert_eq!(
            names.worker(&worker("claude-code/opus")),
            "Opus 5.5 on Claude Code"
        );
        assert_eq!(names.worker(&worker("antigravity-cli")), "agy");
        driver.exit().await;
    }

    /// A session that names no model runs its CLI's default, so it's named by that from its
    /// start, as the header names it, and never shows the bare CLI first.
    #[tokio::test]
    async fn a_cli_default_session_is_named_by_the_default() {
        let mut driver = TestDriver::start().await;
        driver
            .until(|event| matches!(event, AppEvent::Status { .. }))
            .await;
        let AppEvent::Names { names } = &driver.seen()[0] else {
            panic!("{:#?}", driver.seen());
        };
        assert_eq!(
            names.worker(&names.running(&worker("claude-code"))),
            "Opus 5.5 on Claude Code"
        );
        // A model the session names is its own.
        assert_eq!(
            names.worker(&names.running(&worker("claude-code/claude-haiku-4-5"))),
            "Haiku 4.5 on Claude Code"
        );
        driver.exit().await;
    }
}
