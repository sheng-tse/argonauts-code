//! `/model` asks every source again as it opens, each on its own: an answer reaches the open
//! picker as it lands, so a slow source never holds back a fast one, and one that never answers
//! keeps saying it's being asked while the others' rows come in. The sources sit behind the
//! app's own registry, the one that follows the settings.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::app::{AppEvent, ViewData};
use argonauts_core::future::BoxFuture;
use argonauts_core::input::SlashCommand;
use argonauts_core::model_picker::ModelSource;
use argonauts_core::names::RoleName;
use argonauts_core::registry::{ModelEntry, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_registry::models::sources::{Answer, ListSource};
use argonauts_registry::models::{FileRegistry, cli_entry};
use argonauts_testkit::{FakeDirectory, FakeRunner};
use tokio::sync::Notify;

use super::tests::Fixture;
use super::*;
use crate::controller::Internal;
use crate::host::services::LiveRegistry;

/// When a source answers.
#[derive(Debug)]
enum Pace {
    AtOnce,
    /// Once the test lets it.
    When(Arc<Notify>),
    Never,
    /// At once, with an error and what it listed before.
    Fails,
}

/// A source that lists one model at its own pace.
#[derive(Debug)]
struct Paced {
    cli: &'static str,
    model: &'static str,
    pace: Pace,
}

impl ListSource for Paced {
    fn cli(&self) -> &str {
        self.cli
    }

    fn list<'a>(&'a self, settings: &'a Settings, last: Vec<ModelEntry>) -> BoxFuture<'a, Answer> {
        Box::pin(async move {
            match &self.pace {
                Pace::AtOnce => {}
                Pace::When(go) => go.notified().await,
                Pace::Never => std::future::pending::<()>().await,
                Pace::Fails => return Answer::failed(last, "exit status: 1"),
            }
            Answer::listed(vec![cli_entry(settings, self.cli, Some(self.model))])
        })
    }
}

const SOURCES: [&str; 3] = ["fast-cli", "slow-cli", "never-cli"];

/// `sources` installed and nothing else asked, no local server either, behind the app's
/// registry.
fn installed(fixture: &mut Fixture, sources: Vec<Paced>) {
    let mut settings = Settings::default();
    for agent in settings.agents.values_mut() {
        agent.enabled = false;
    }
    settings.registry.local_servers = false;
    let directory = Arc::new(FakeDirectory::new());
    let mut registry = FileRegistry::from_files(&settings, None, None);
    for source in sources {
        directory.add(FakeRunner::cli(source.cli, source.cli));
        registry = registry.with_source(Arc::new(source));
    }
    let registry: Arc<dyn ModelRegistry> = Arc::new(registry);
    let (_, settings_now) = tokio::sync::watch::channel(Arc::new(settings));
    fixture.ctx.services.registry = Arc::new(LiveRegistry::built_by(settings_now, move |_| {
        Arc::clone(&registry)
    }));
    fixture.ctx.services.workers = directory;
}

/// The three sources installed and nothing else asked, no local server either.
fn paced_sources(fixture: &mut Fixture, go: &Arc<Notify>) {
    let paced = |cli, model, pace| Paced { cli, model, pace };
    installed(
        fixture,
        vec![
            paced("fast-cli", "quick-1", Pace::AtOnce),
            paced("slow-cli", "steady-1", Pace::When(Arc::clone(go))),
            paced("never-cli", "lost-1", Pace::Never),
        ],
    );
}

/// `/model` as you type it, for the main agent or `role`.
async fn open_model(fixture: &mut Fixture, role: Option<RoleName>) {
    let model = SlashCommand::Model {
        role,
        worker: None,
        effort: None,
    };
    command(&mut fixture.ctx, &mut fixture.state, model, None)
        .await
        .unwrap();
}

/// The next source to answer, passed to the controller as its loop would.
async fn answered(fixture: &mut Fixture) -> String {
    let next = tokio::time::timeout(Duration::from_secs(5), fixture.internal.recv())
        .await
        .expect("a source answered")
        .expect("the channel is open");
    let Internal::ModelSourceListed { cli } = next else {
        panic!("only sources answer here");
    };
    refresh::source_listed(&fixture.ctx, &mut fixture.state, &cli).unwrap();
    cli
}

/// The one picker update sent since the last look.
fn only_update(fixture: &mut Fixture) -> ViewData {
    let mut updates: Vec<ViewData> = fixture
        .drain()
        .into_iter()
        .filter_map(|event| match event {
            AppEvent::View { data } => match *data {
                data @ ViewData::Models { update: true, .. } => Some(data),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 1, "one update per answer");
    updates.remove(0)
}

fn asking(view: &ViewData) -> Vec<&str> {
    let ViewData::Models { sources, .. } = view else {
        panic!("a model list");
    };
    sources
        .iter()
        .filter(|source| source.asking)
        .map(|source| source.cli.as_str())
        .collect()
}

fn listed(view: &ViewData) -> Vec<String> {
    let ViewData::Models { choices, .. } = view else {
        panic!("a model list");
    };
    choices
        .iter()
        .filter(|choice| choice.worker.model.is_some())
        .map(|choice| choice.worker.to_string())
        .collect()
}

fn source<'a>(view: &'a ViewData, cli: &str) -> &'a ModelSource {
    let ViewData::Models { sources, .. } = view else {
        panic!("a model list");
    };
    sources
        .iter()
        .find(|source| source.cli == cli)
        .expect("the source is listed")
}

#[tokio::test]
async fn each_source_reaches_the_open_picker_as_it_answers() {
    let mut fixture = Fixture::new(false, |_| {});
    let go = Arc::new(Notify::new());
    paced_sources(&mut fixture, &go);

    open_model(&mut fixture, Some(RoleName::Reviewer)).await;
    fixture.drain();
    let opened = refresh::view(&fixture.ctx, &mut fixture.state, None);
    assert_eq!(
        asking(&opened),
        SOURCES,
        "it opens at once, asking all three"
    );
    assert!(listed(&opened).is_empty());

    assert_eq!(answered(&mut fixture).await, "fast-cli");
    let update = only_update(&mut fixture);
    assert_eq!(asking(&update), ["slow-cli", "never-cli"]);
    assert_eq!(listed(&update), ["fast-cli/quick-1"]);

    go.notify_one();
    assert_eq!(answered(&mut fixture).await, "slow-cli");
    let update = only_update(&mut fixture);
    assert_eq!(asking(&update), ["never-cli"]);
    let mut rows = listed(&update);
    rows.sort();
    assert_eq!(rows, ["fast-cli/quick-1", "slow-cli/steady-1"]);

    // The one that never answers holds nothing back, and nothing more arrives.
    let more = tokio::time::timeout(Duration::from_millis(100), fixture.internal.recv()).await;
    assert!(more.is_err(), "only the two that answered came in");
}

#[tokio::test]
async fn a_source_that_fails_keeps_its_list_and_says_so() {
    let mut fixture = Fixture::new(false, |_| {});
    let paced = |cli, model, pace| Paced { cli, model, pace };
    installed(
        &mut fixture,
        vec![
            paced("fast-cli", "quick-1", Pace::AtOnce),
            paced("failing-cli", "gone-1", Pace::Fails),
        ],
    );

    open_model(&mut fixture, Some(RoleName::Reviewer)).await;
    fixture.drain();
    let mut answers = vec![answered(&mut fixture).await, answered(&mut fixture).await];
    answers.sort();
    assert_eq!(answers, ["failing-cli", "fast-cli"]);

    let shown = refresh::view(&fixture.ctx, &mut fixture.state, None);
    assert!(asking(&shown).is_empty());
    assert!(source(&shown, "failing-cli").failed, "the failed ask shows");
    assert!(!source(&shown, "fast-cli").failed);
    assert!(source(&shown, "fast-cli").listed.is_some());
    assert_eq!(listed(&shown), ["fast-cli/quick-1"]);
}
