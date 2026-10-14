//! The run views, built by code at no token cost, and the narrator's summary and `/ask`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use argonauts_core::app::{AppEvent, RepoRuns, ViewData, ViewRequest};
use argonauts_core::events::context::ContextDetail;
use argonauts_core::events::run::RunLifecycle;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::ids::RepoId;
use argonauts_core::journal;
use argonauts_core::layout::run_files;
use argonauts_core::names::WorkflowId;
use argonauts_core::settings::sessions::RotateAt;
use argonauts_core::views::{ContextRow, ContextView};
use argonauts_core::{NoteLevel, RunId};

use super::{Handled, on_screen};
use crate::client;
use crate::controller::Ctx;
use crate::controller::attach::Stage;
use crate::controller::runs::{self, RunsState};
use crate::live_registry;
use crate::views;

/// The run's log, or the events this app holds when the log can't be read.
pub(super) fn run_events(ctx: &Ctx, runs: &RunsState, run: &RunId) -> Vec<RunEvent> {
    let path = ctx.env.run_dir(run).join(run_files::EVENTS);
    if let Ok(back) = journal::read_all(&path) {
        return back.events;
    }
    runs.attached
        .as_ref()
        .filter(|attached| &attached.run == run)
        .map(|attached| attached.events.clone())
        .unwrap_or_default()
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
        ViewRequest::Runs => runs::show_runs(ctx, false).await?,
        ViewRequest::SessionsEverywhere => runs::show_runs(ctx, true).await?,
        ViewRequest::Status => return status(ctx, runs).await,
        ViewRequest::Context => return context(ctx, runs, None).await,
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
        ViewRequest::RepoFiles => anyhow::bail!("the repo's files aren't a run's"),
        ViewRequest::AgentSessions { .. } => {
            anyhow::bail!("a run's agents' sessions are the conversation's to list")
        }
        ViewRequest::Learning { .. } => anyhow::bail!("the Learning pickers aren't run views"),
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

/// `/context`: every live session's context, including the conversation's own row. The tab asks
/// again while work runs, so a list that fails only leaves the other runs out.
pub(crate) async fn context(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: Option<ContextRow>,
) -> Handled {
    let (rows, _) = context_rows(ctx, runs, conversation).await;
    show(
        ctx,
        ViewData::Context {
            view: ContextView { rows },
        },
    );
    Ok(Vec::new())
}

/// `/context print`: the same sessions, once, for the conversation. The screen prints them
/// with the main session's own count, as it draws the tab.
pub(crate) async fn context_report(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: Option<ContextRow>,
) -> Handled {
    let (rows, left_out) = context_rows(ctx, runs, conversation).await;
    if let Some(err) = left_out {
        ctx.notice(
            NoteLevel::Warn,
            format!("Other runs' sessions are left out: {err}"),
        );
    }
    show(
        ctx,
        ViewData::ContextPrint {
            view: ContextView { rows },
        },
    );
    Ok(Vec::new())
}

/// Every live session's context: the conversation's own, the run on screen, then the other runs
/// that work now, with why the other runs are missing when they couldn't be listed.
async fn context_rows(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: Option<ContextRow>,
) -> (Vec<ContextRow>, Option<anyhow::Error>) {
    let rotate_at = ctx.settings().sessions.rotate_at.clone();
    let clis = ContextClis::here(ctx);
    let mut rows: Vec<ContextRow> = conversation
        .into_iter()
        .map(|row| clis.said_of(row))
        .collect();
    let attached = runs::attached(runs).cloned();
    if let Some(run) = &attached {
        let finished = runs
            .attached
            .as_ref()
            .is_some_and(|attached| matches!(attached.stage, Stage::Finished(_)));
        if !finished {
            let events = run_events(ctx, runs, run);
            rows.extend(capped_rows(&events, run, &rotate_at, &clis));
        }
    }
    let list = match ctx.services.conductor.list().await {
        Ok(list) => list,
        Err(err) => return (rows, Some(err.into())),
    };
    for other in list
        .iter()
        .filter(|summary| summary.live && Some(&summary.run) != attached.as_ref())
    {
        let events = run_events(ctx, runs, &other.run);
        rows.extend(capped_rows(&events, &other.run, &rotate_at, &clis));
    }
    (rows, None)
}

/// A run's live sessions, each capped where it rotates. A share of the window gives each
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
            None => NO_WORKFLOW_YET.to_owned(),
        });
    };
    views::required::mark(&mut view, &ctx.env, &kept_steps(&events));
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
