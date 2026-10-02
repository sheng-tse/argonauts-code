//! What the screen calls each CLI and model: the adapters' names and the model catalog, sent
//! before the header, when the model lists are read again, and when settings change, so the
//! screen reads "Opus 5.5 on Claude Code" rather than ids.

use std::collections::BTreeMap;

use argonauts_core::app::AppEvent;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::names::WorkerRef;

use super::Ctx;

/// The names last sent, so an unchanged set goes out once. Notices name agents by them too,
/// as the screen does.
#[derive(Debug, Default)]
pub(crate) struct NamesSent(Option<DisplayNames>);

impl NamesSent {
    /// `Opus 5.5 on Claude Code`.
    pub(crate) fn worker(&self, worker: &WorkerRef) -> String {
        match &self.0 {
            Some(names) => names.worker(worker),
            None => DisplayNames::default().worker(worker),
        }
    }

    /// `Claude Code`.
    pub(crate) fn harness(&self, id: &str) -> String {
        match &self.0 {
            Some(names) => names.harness(id),
            None => DisplayNames::default().harness(id),
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

/// An adapter list that fails leaves the CLIs to the names Argonauts knows them by.
fn names(ctx: &Ctx) -> DisplayNames {
    let harnesses = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .map(|adapters| {
            adapters
                .iter()
                .map(|adapter| (adapter.id().to_owned(), adapter.display_name().to_owned()))
                .collect()
        })
        .unwrap_or_else(|_| BTreeMap::new());
    let configured = ctx
        .settings()
        .agents
        .iter()
        .filter_map(|(id, agent)| Some((id.clone(), agent.model.clone()?)))
        .collect();
    DisplayNames {
        harnesses,
        catalog: ctx.services.registry.catalog(),
        configured,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::AppEvent;
    use argonauts_core::names::WorkerRef;

    use crate::controller::test_support::Harness;

    fn worker(text: &str) -> WorkerRef {
        text.parse().unwrap()
    }

    /// First of all, so the header and the status line name the agent as the rest will.
    #[tokio::test]
    async fn the_screen_learns_the_names_first() {
        let mut harness = Harness::start().await;
        harness
            .until(|event| matches!(event, AppEvent::Status { .. }))
            .await;
        let AppEvent::Names { names } = &harness.seen()[0] else {
            panic!("{:#?}", harness.seen());
        };
        let sets = harness
            .seen()
            .iter()
            .filter(|event| matches!(event, AppEvent::Names { .. }))
            .count();
        assert_eq!(sets, 1, "an unchanged set goes out once");
        assert_eq!(
            names.harnesses.get("codex").map(String::as_str),
            Some("Codex")
        );
        assert_eq!(
            names.worker(&worker("claude-code/opus")),
            "Opus 5.5 on Claude Code"
        );
        assert_eq!(names.worker(&worker("antigravity-cli")), "Antigravity CLI");
        harness.exit().await;
    }
}
