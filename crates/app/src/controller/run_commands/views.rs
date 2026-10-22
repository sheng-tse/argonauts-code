//! The run views, built by code at no token cost, and the narrator's summary and `/ask`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use argonauts_core::app::{AppEvent, RepoRuns, SessionCounted, ViewData, ViewRequest};
use argonauts_core::display_names::builtin_cli_name;
use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::ids::RepoId;
use argonauts_core::journal;
use argonauts_core::layout::{self, run_files, session_files};
use argonauts_core::names::ModelRef;
use argonauts_core::names::WorkflowId;
use argonauts_core::registry::ModelRegistry;
use argonauts_core::settings::Settings;
use argonauts_core::settings::endpoints::endpoint_settings;
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::views::{ContextRow, ContextView, RunFinishedAt, ServerCap, WindowFigures};
use argonauts_core::{NoteLevel, RunId};

use super::{Handled, on_screen};
use crate::client;
use crate::controller::Ctx;
use crate::controller::runs::{self, RunsState};
use crate::live_registry;
use crate::views;

/// The run's log, or the events this app holds when the log can't be read.
pub(super) fn run_events(ctx: &Ctx, runs: &RunsState, run: &RunId) -> Vec<RunEvent> {
    let path = run_dir_of(ctx, run).join(run_files::EVENTS);
    if let Ok(back) = journal::read_all(&path) {
        return back.events;
    }
    runs.attached
        .as_ref()
        .filter(|attached| &attached.run == run)
        .map(|attached| attached.events.clone())
        .unwrap_or_default()
}

/// The run's folder in this project, or in whichever project recorded it: the Behaviors map
/// covers every project, and its examples open their runs.
fn run_dir_of(ctx: &Ctx, run: &RunId) -> PathBuf {
    let here = ctx.env.run_dir(run);
    if here.exists() {
        return here;
    }
    std::fs::read_dir(ctx.env.paths.home().join("state"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|project| {
            project
                .path()
                .join(layout::repo_state::RUNS)
                .join(run.as_str())
        })
        .find(|dir| dir.join(run_files::EVENTS).is_file())
        .unwrap_or(here)
}

/// Before the main agent's brief holds, Ctrl+T and Enter on the run's row open the main agent at
/// work, so only a run the screen doesn't hold yet gets this.
const NO_WORKFLOW_YET: &str = "This run hasn't started a workflow yet.";

fn show(ctx: &Ctx, data: ViewData) {
    ctx.send(AppEvent::View {
        data: Box::new(data),
    });
}

/// `Runs`, `Status`, `Diff`, `Raw`, `Workflow`, `Agent`, and `AllRepos`.
pub(crate) async fn view(ctx: &mut Ctx, runs: &mut RunsState, view: ViewRequest) -> Handled {
    match view {
        ViewRequest::ResumeList => runs::show_runs(ctx, false).await?,
        ViewRequest::SessionsEverywhere => runs::show_runs(ctx, true).await?,
        ViewRequest::Status => return status(ctx, runs).await,
        ViewRequest::Context => return context(ctx, runs, None).await,
        ViewRequest::SessionContext { run, session } => {
            let path =
                layout::session_dir(&ctx.env.run_dir(&run), &session).join(session_files::CONTEXT);
            let counted = kept_count(&path);
            show(
                ctx,
                ViewData::SessionContext {
                    run,
                    session,
                    counted,
                },
            );
        }
        ViewRequest::Diff { run, task, scope } => {
            let diff = match ctx.services.conductor.diff(&run, &task, scope).await {
                Ok(diff) => diff,
                Err(err) => {
                    return super::refused(ctx, err, || {
                        format!("Couldn't show the diff of {task}")
                    });
                }
            };
            show(ctx, ViewData::Diff { diff });
        }
        ViewRequest::Raw { run, session } => {
            let raw = match ctx.services.conductor.raw(&run, &session).await {
                Ok(raw) => raw,
                Err(err) => {
                    return super::refused(ctx, err, || {
                        format!("Couldn't show the raw log of {session}")
                    });
                }
            };
            show(ctx, ViewData::Raw { raw });
        }
        ViewRequest::Workflow { run, workflow } => {
            if let Some(why) = show_workflow(ctx, runs, &run, workflow.as_ref()) {
                ctx.hint(why);
            }
        }
        ViewRequest::Agent { run, session } => {
            let events = run_events(ctx, runs, &run);
            match views::agent_view(&events, &run, &session) {
                Some(view) => show(
                    ctx,
                    ViewData::Agent {
                        view: Box::new(view),
                    },
                ),
                None => ctx.hint(format!("{run} has no session {session}.")),
            }
        }
        ViewRequest::InnerAgent { run, agent, .. } => {
            let events = run_events(ctx, runs, &run);
            match views::inner_agent_view(&events, &run, &agent) {
                Some(view) => show(
                    ctx,
                    ViewData::Agent {
                        view: Box::new(view),
                    },
                ),
                None => ctx.hint(format!("{run} no longer reports that agent.")),
            }
        }
        ViewRequest::Goal { run } => {
            let events = run_events(ctx, runs, &run);
            match views::goal_view(&events, &run) {
                Some(view) => show(
                    ctx,
                    ViewData::Goal {
                        view: Box::new(view),
                    },
                ),
                None => ctx.hint(format!("{run} is a workflow, so it has no goal view.")),
            }
        }
        ViewRequest::AllRepos => {
            let repos = all_repos(ctx).await?;
            show(ctx, ViewData::AllRepos { repos });
        }
        ViewRequest::Crew => anyhow::bail!("the agent panel isn't one run's view"),
        ViewRequest::StatusTab => anyhow::bail!("the Status tab isn't one run's view"),
        ViewRequest::Mods { .. } => anyhow::bail!("the mods aren't a run's"),
        ViewRequest::Config
        | ViewRequest::Settings { .. }
        | ViewRequest::Models { .. }
        | ViewRequest::Approvals => {
            anyhow::bail!("the settings views aren't run views")
        }
        ViewRequest::WorkflowLibrary => anyhow::bail!("the workflow files aren't a run's"),
        ViewRequest::Usage { .. } => anyhow::bail!("usage isn't one run's view"),
        ViewRequest::ReadPlanUsage => anyhow::bail!("plan usage isn't one run's"),
        ViewRequest::Overview { .. } => anyhow::bail!("the Overview isn't one run's view"),
        ViewRequest::Behaviors(_) => anyhow::bail!("the Behaviors map isn't one run's view"),
        ViewRequest::RepoFiles => anyhow::bail!("the repo's files aren't a run's"),
        ViewRequest::Learning { .. } => anyhow::bail!("the Learning pickers aren't run views"),
        ViewRequest::Issues => anyhow::bail!("the issue list isn't a run's"),
        ViewRequest::DeepOffer => anyhow::bail!("the deeper map's questions aren't a run's"),
    }
    Ok(Vec::new())
}

/// `/status`: the code-built summary, at no token cost.
pub(crate) async fn status(ctx: &mut Ctx, runs: &mut RunsState) -> Handled {
    let run = match runs::attached(runs) {
        Some(run) => run.clone(),
        None => {
            let list = ctx
                .services
                .conductor
                .list()
                .await
                .context("listing this repo's runs")?;
            let Some(newest) = list.into_iter().next() else {
                ctx.notice(NoteLevel::Info, "No runs in this repo yet.");
                return Ok(Vec::new());
            };
            newest.run
        }
    };
    let summary = match ctx.services.conductor.status(&run).await {
        Ok(summary) => summary,
        Err(err) => {
            return super::refused(ctx, err, || "Couldn't read the run's status".to_owned());
        }
    };
    show(ctx, ViewData::Status { summary });
    Ok(Vec::new())
}

/// `s` in the status view: the narrator's summary.
pub(crate) async fn summary(ctx: &mut Ctx, runs: &mut RunsState) -> Handled {
    let Some(attached) = runs.attached.as_ref() else {
        ctx.notice(
            NoteLevel::Info,
            "No run is on screen. /status shows the latest run's summary for free.",
        );
        return Ok(Vec::new());
    };
    if !attached.narrator.is_on() {
        ctx.notice(
            NoteLevel::Info,
            "The narrator is off. /narrator turns it on, and /status shows the code-built \
             summary for free.",
        );
        return Ok(Vec::new());
    }
    attached.narrator.summary();
    Ok(Vec::new())
}

/// `/ask` during a run with the narrator on. "Where are we?" gets the code-built summary.
pub(crate) async fn ask_narrator(ctx: &mut Ctx, runs: &mut RunsState, question: String) -> Handled {
    if asks_for_status(&question) {
        return status(ctx, runs).await;
    }
    match runs.attached.as_ref() {
        Some(attached) => attached.narrator.ask(question),
        None => ctx.notice(NoteLevel::Info, super::NO_RUN),
    }
    Ok(Vec::new())
}

/// Questions about where the run stands, which the status summary answers without a model.
pub(super) fn asks_for_status(question: &str) -> bool {
    let words: String = question
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let question = words.split_whitespace().collect::<Vec<_>>().join(" ");
    const PHRASES: &[&str] = &[
        "where are we",
        "whats the status",
        "what is the status",
        "status update",
        "how far along",
        "hows it going",
        "how is it going",
        "whats happening",
        "what is happening",
    ];
    matches!(question.as_str(), "status" | "progress")
        || PHRASES.iter().any(|phrase| question.contains(phrase))
}

/// `/context`: every live session's context, including the conversation's own row, and every
/// session of the run you follow. The tab asks again while work runs, so a list that fails
/// only leaves the other runs out.
pub(crate) async fn context(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: Option<ContextRow>,
) -> Handled {
    let (view, _) = context_view(ctx, runs, conversation).await;
    show(ctx, ViewData::Context { view });
    Ok(Vec::new())
}

/// `/context print`: the same sessions, once, for the conversation. The screen prints them
/// with the main session's own count, as it draws the tab.
pub(crate) async fn context_report(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: Option<ContextRow>,
) -> Handled {
    let (view, left_out) = context_view(ctx, runs, conversation).await;
    if let Some(err) = left_out {
        ctx.notice(
            NoteLevel::Warn,
            format!("Other runs' sessions are left out: {err}"),
        );
    }
    show(ctx, ViewData::ContextPrint { view });
    Ok(Vec::new())
}

/// The sessions with where they run: the servers on this machine they use, and what holds the
/// main session's window to its size.
fn where_they_run(ctx: &Ctx, mut view: ContextView) -> ContextView {
    view.main_window = view
        .rows
        .iter()
        .find(|row| row.run.is_none())
        .map(|row| window_figures(ctx.services.registry.as_ref(), &row.worker))
        .filter(|figures| *figures != WindowFigures::default());
    view.local = local_servers(ctx.settings(), &view.rows);
    view
}

/// The servers on this machine the sessions run on, by endpoint id: a session's CLI is one, or
/// its model's route names one.
fn local_servers(settings: &Settings, rows: &[ContextRow]) -> Vec<String> {
    let mut local: Vec<String> = Vec::new();
    for worker in rows.iter().map(|row| &row.worker) {
        let routed = worker
            .model
            .as_deref()
            .and_then(|model| model.split_once('/'))
            .map(|(server, _)| server);
        for id in std::iter::once(worker.agent.as_str()).chain(routed) {
            let on_this_machine =
                endpoint_settings(settings, id).is_some_and(|endpoint| endpoint.local);
            if on_this_machine && !local.iter().any(|known| known == id) {
                local.push(id.to_owned());
            }
        }
    }
    local
}

/// What holds `worker`'s window to its size, from the registry. A CLI's model routed to a
/// server, as OpenCode's `ollama/qwen3-coder:30b-64k`, has the most the model takes and the
/// server's cap from the server's own entry for it, and the CLI's figure from the CLI's.
fn window_figures(registry: &dyn ModelRegistry, worker: &ModelRef) -> WindowFigures {
    let own = registry.get(worker);
    let routed = worker
        .model
        .as_deref()
        .and_then(|model| model.split_once('/'))
        .map(|(server, model)| ModelRef::new(server, Some(model.to_owned())));
    let (server, served) = match &routed {
        Some(routed) => (routed.agent.as_str(), registry.get(routed)),
        None => (worker.agent.as_str(), own.clone()),
    };
    let cli = own
        .filter(|_| routed.is_some())
        .and_then(|entry| entry.window.or(entry.context_window));
    WindowFigures {
        model: served.as_ref().and_then(|entry| entry.context_window),
        server: served
            .as_ref()
            .and_then(|entry| entry.window)
            .map(|tokens| ServerCap {
                name: builtin_cli_name(server).map_or_else(|| server.to_owned(), str::to_owned),
                tokens,
            }),
        cli,
    }
}

/// The sessions the Context tab lists: the conversation's own, every session of the run on
/// screen in the order they started, live or ended, then the live sessions of the other runs
/// that work now, with why the other runs are missing when they couldn't be listed. The run on
/// screen stays listed after it finishes, until the next one comes on screen or another
/// conversation does.
async fn context_view(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: Option<ContextRow>,
) -> (ContextView, Option<anyhow::Error>) {
    let rotate_at = ctx.settings().sessions.rotate_at.clone();
    let clis = ContextClis::here(ctx);
    let mut view = ContextView::of(
        conversation
            .into_iter()
            .map(|row| clis.said_of(row))
            .collect(),
    );
    let attached = runs::followed(runs).cloned();
    if let Some(run) = &attached {
        let events = run_events(ctx, runs, run);
        view.rows
            .extend(capped_rows(&events, run, &rotate_at, &clis));
        if let Some(at) = views::finished_at(&events) {
            view.finished.push(RunFinishedAt {
                run: run.clone(),
                at,
            });
        }
    }
    let list = match ctx.services.conductor.list().await {
        Ok(list) => list,
        Err(err) => return (where_they_run(ctx, view), Some(err.into())),
    };
    for other in list
        .iter()
        .filter(|summary| summary.live && Some(&summary.run) != attached.as_ref())
    {
        let events = run_events(ctx, runs, &other.run);
        let live = capped_rows(&events, &other.run, &rotate_at, &clis)
            .into_iter()
            .filter(|row| row.ended.is_none());
        view.rows.extend(live);
    }
    (where_they_run(ctx, view), None)
}

/// A session's last whole count as its folder keeps it, counted when the file was written.
/// Nothing for a session never counted, or a file that doesn't read as one.
fn kept_count(path: &Path) -> Option<SessionCounted> {
    let usage = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let at = std::fs::metadata(path)
        .and_then(|file| file.modified())
        .ok()?;
    Some(SessionCounted {
        usage: Box::new(usage),
        at: at.into(),
    })
}

/// A run's sessions, each capped where it rotates. A share of the window gives each
/// session its own cap, and none while its window is unknown.
fn capped_rows(
    events: &[RunEvent],
    run: &RunId,
    rotate_at: &RotateAt,
    clis: &ContextClis,
) -> Vec<ContextRow> {
    views::context_rows(events, run, 0)
        .into_iter()
        .map(|row| ContextRow {
            cap: rotate_at.tokens(row.window).ok().flatten(),
            ..clis.said_of(row)
        })
        .collect()
}

/// What the CLIs here can say of a session's context.
#[derive(Debug, Default)]
struct ContextClis {
    /// Those whose fill is worked out and not read off a request.
    estimating: Vec<String>,
    /// Those that can say what fills a session.
    counting: Vec<String>,
}

impl ContextClis {
    fn here(ctx: &Ctx) -> Self {
        let Ok(adapters) = crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
        else {
            return Self::default();
        };
        let ids = |keep: &dyn Fn(&dyn argonauts_core::adapter::Adapter) -> bool| {
            adapters
                .iter()
                .filter(|adapter| keep(adapter.as_ref()))
                .map(|adapter| adapter.id().to_owned())
                .collect()
        };
        Self {
            estimating: ids(&|adapter| adapter.estimates_context_fill()),
            counting: ids(&|adapter| adapter.context_usage_line(ContextDetail::Summary).is_some()),
        }
    }

    /// The row with what its CLI can say of it.
    fn said_of(&self, row: ContextRow) -> ContextRow {
        ContextRow {
            estimated: self.estimating.contains(&row.worker.agent),
            counts: self.counting.contains(&row.worker.agent),
            ..row
        }
    }
}

/// `s` in the workflow view: the run's workflow as it is now, after any approved change, goes
/// to your own library under the run's name, so it runs again by that name.
pub(crate) async fn save_workflow(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    agent_id: &crate::controller::cli_commands::CliCommands,
    run: RunId,
    workflow: Option<WorkflowId>,
) -> Handled {
    let events = run_events(ctx, runs, &run);
    let Some((name, definition)) = views::saved_workflow(&events, workflow.as_ref()) else {
        ctx.notice(
            NoteLevel::Info,
            "This run has no workflow to save yet.".to_owned(),
        );
        return Ok(Vec::new());
    };
    // Steps the runner ran are a script, which saves as a workflow of yours.
    let script = crate::controller::workflows::save_script(
        ctx,
        agent_id,
        &run,
        &events,
        workflow.as_ref(),
        &name,
    );
    if let Some(saved) = script {
        return saved;
    }
    let dir = ctx
        .env
        .paths
        .home()
        .join(argonauts_core::layout::user_files::WORKFLOWS);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", ctx.env.display(&dir)))?;
    // An earlier save keeps its name; this one takes the next free one.
    let path = (1..)
        .map(|n| match n {
            1 => dir.join(format!("{name}.yaml")),
            n => dir.join(format!("{name}-{n}.yaml")),
        })
        .find(|path| !path.exists())
        .context("finding a free file name")?;
    let saved_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&name)
        .to_owned();
    let mut definition = definition;
    definition.name.clone_from(&saved_name);
    let text = argonauts_crew::workflow::yaml::print(&definition)
        .map_err(|err| anyhow::anyhow!("writing the workflow as YAML: {err}"))?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", ctx.env.display(&path)))?;
    ctx.notice(
        NoteLevel::Info,
        format!(
            "Saved its phases as {saved_name} in {}. /workflow {saved_name} makes it the \
             workflow in use.",
            ctx.env.display(&path)
        ),
    );
    Ok(Vec::new())
}

/// The run's own workflow when `workflow` is `None`.
pub(crate) async fn workflow_view(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    workflow: Option<WorkflowId>,
) -> Handled {
    let Some(run) = on_screen(ctx, runs) else {
        return Ok(Vec::new());
    };
    // A command you typed is answered in the transcript, as its other answers are.
    if let Some(why) = show_workflow(ctx, runs, &run, workflow.as_ref()) {
        ctx.notice(NoteLevel::Info, why);
    }
    Ok(Vec::new())
}

/// The steps a setup kept for the run, which its start carries.
fn kept_steps(events: &[RunEvent]) -> Vec<argonauts_core::required::RequiredStep> {
    events
        .iter()
        .find_map(|event| match &event.kind {
            RunEventKind::Run(RunLifecycle::Started(started)) => {
                Some(started.required_steps.clone())
            }
            _ => None,
        })
        .unwrap_or_default()
}

/// Shows the workflow view, or says why there's none to show.
fn show_workflow(
    ctx: &Ctx,
    runs: &RunsState,
    run: &RunId,
    workflow: Option<&WorkflowId>,
) -> Option<String> {
    let events = run_events(ctx, runs, run);
    let Some(mut view) = views::workflow_view(&events, run, workflow) else {
        return Some(match workflow {
            Some(workflow) => format!("This run has no workflow {workflow}."),
            None if views::being_written(&events) => {
                argonauts_core::views::BEING_WRITTEN.to_owned()
            }
            None => NO_WORKFLOW_YET.to_owned(),
        });
    };
    views::required::mark(&mut view, &ctx.env, &kept_steps(&events));
    views::followed_heading(&mut view, &ctx.env.state_dir);
    show(
        ctx,
        ViewData::Workflow {
            view: Box::new(view),
        },
    );
    None
}

/// This repo first, then every other repo with a live run host, newest update first.
pub(crate) async fn all_repos(ctx: &Ctx) -> anyhow::Result<Vec<RepoRuns>> {
    let here = ctx
        .services
        .conductor
        .list()
        .await
        .context("listing this repo's runs")?;
    let mut others: BTreeMap<PathBuf, Vec<argonauts_core::api::RunSummary>> = BTreeMap::new();
    for live in live_registry::live_run_hosts(&ctx.env.paths) {
        if live.repo_root == ctx.env.repo_root {
            continue;
        }
        // A repo that moved or vanished since its run host registered has nothing to show.
        let Ok(repo) = RepoId::for_repo(&live.repo_root) else {
            continue;
        };
        let run_dir = ctx.env.paths.run_dir(&repo, &live.run);
        if let Some(summary) = client::summarize(&run_dir, ctx.settings()) {
            others.entry(live.repo_root).or_default().push(summary);
        }
    }
    let mut others: Vec<(PathBuf, Vec<_>)> = others.into_iter().collect();
    for (_, runs) in &mut others {
        runs.sort_by_key(|run| std::cmp::Reverse(run.updated));
    }
    others.sort_by(|(_, a), (_, b)| {
        let newest = |runs: &[argonauts_core::api::RunSummary]| runs.first().map(|run| run.updated);
        newest(b).cmp(&newest(a))
    });
    let mut repos = vec![RepoRuns {
        repo: ctx.env.display_repo(),
        runs: here,
    }];
    repos.extend(others.into_iter().map(|(root, runs)| RepoRuns {
        repo: ctx.env.display(&root),
        runs,
    }));
    Ok(repos)
}

#[cfg(test)]
mod window_tests {
    use argonauts_core::registry::ModelEntry;
    use argonauts_core::settings::agents::EndpointSettings;
    use argonauts_core::views::ContextRow;
    use argonauts_testkit::services::StaticRegistry;

    use super::*;

    fn entry(id: &str, context_window: Option<u64>, window: Option<u64>) -> ModelEntry {
        let mut entry: ModelEntry = serde_json::from_value(serde_json::json!({
            "id": id,
            "provider": "qwen",
            "kind": "cli",
            "access": "local",
            "cost": { "unit": "none" },
        }))
        .expect("a registry entry");
        entry.context_window = context_window;
        entry.window = window;
        entry
    }

    /// Qwen on OpenCode through Ollama: the model takes 256k, the 64k tag holds it to 64k, and
    /// OpenCode's own list says 64k.
    #[test]
    fn a_routed_model_s_window_has_each_figure_that_holds_it() {
        let registry = StaticRegistry::default();
        registry.add(entry(
            "ollama/qwen3-coder:30b-64k",
            Some(262_144),
            Some(65_536),
        ));
        registry.add(entry(
            "opencode/ollama/qwen3-coder:30b-64k",
            None,
            Some(65_536),
        ));
        let worker: ModelRef = "opencode/ollama/qwen3-coder:30b-64k".parse().unwrap();
        assert_eq!(
            window_figures(&registry, &worker),
            WindowFigures {
                model: Some(262_144),
                server: Some(ServerCap {
                    name: "Ollama".to_owned(),
                    tokens: 65_536,
                }),
                cli: Some(65_536),
            }
        );
        // A model straight on its server has no CLI between.
        let gemma: ModelRef = "ollama/gemma3:12b-32k".parse().unwrap();
        registry.add(entry("ollama/gemma3:12b-32k", Some(131_072), Some(32_768)));
        let figures = window_figures(&registry, &gemma);
        assert_eq!(figures.cli, None);
        assert_eq!(figures.server.map(|cap| cap.tokens), Some(32_768));
        // A model nothing knows has no figures.
        let unknown: ModelRef = "opencode/openrouter/qwen3-coder".parse().unwrap();
        assert_eq!(
            window_figures(&registry, &unknown),
            WindowFigures::default()
        );
    }

    #[test]
    fn the_servers_a_session_runs_on_here_are_listed_once() {
        let row = |worker: &str| ContextRow {
            run: None,
            session: argonauts_core::names::SessionKey::numbered(1),
            label: worker.to_owned(),
            worker: worker.parse().unwrap(),
            used: 0,
            window: None,
            cap: None,
            tool_calls: 0,
            spent: argonauts_core::Spend::default(),
            rotations: 0,
            estimated: false,
            counts: false,
            categories: Vec::new(),
            recorded: None,
            ended: None,
            answered_for: None,
        };
        let mut settings = Settings::default();
        let rows = [
            row("opencode/ollama/qwen3-coder:30b-64k"),
            row("ollama/gemma3:12b-32k"),
            row("opencode/openrouter/qwen3-coder"),
            row("claude-code/claude-haiku-5-5"),
        ];
        assert_eq!(local_servers(&settings, &rows), ["ollama"]);
        // Ollama moved to another machine is no longer local.
        settings.endpoints.insert(
            "ollama".into(),
            EndpointSettings {
                base_url: Some("http://gpu-box:11434/v1".into()),
                ..EndpointSettings::default()
            },
        );
        assert!(local_servers(&settings, &rows).is_empty());
    }
}
