//! The run views, built by code at no token cost, and the narrator's summary and `/ask`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use argonauts_core::app::{AppEvent, RepoRuns, ViewData, ViewRequest};
use argonauts_core::events::RunEvent;
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

fn show(ctx: &Ctx, data: ViewData) {
    ctx.send(AppEvent::View {
        data: Box::new(data),
    });
}

/// `Runs`, `Status`, `Diff`, `Raw`, `Workflow`, `Agent`, and `AllRepos`.
pub(crate) async fn view(ctx: &mut Ctx, runs: &mut RunsState, view: ViewRequest) -> Handled {
    match view {
        ViewRequest::Runs => runs::show_runs(ctx).await?,
        ViewRequest::Status => return status(ctx, runs).await,
        ViewRequest::Context => return context(ctx, runs, None).await,
        ViewRequest::Diff { run, card, scope } => {
            let diff = ctx
                .services
                .conductor
                .diff(&run, &card, scope)
                .await
                .with_context(|| format!("the diff of {card} in {run}"))?;
            show(ctx, ViewData::Diff { diff });
        }
        ViewRequest::Raw { run, session } => {
            let raw = ctx
                .services
                .conductor
                .raw(&run, &session)
                .await
                .with_context(|| format!("the raw log of {session} in {run}"))?;
            show(ctx, ViewData::Raw { raw });
        }
        ViewRequest::Workflow { run, workflow } => {
            show_workflow(ctx, runs, &run, workflow.as_ref());
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
                None => ctx.notice(NoteLevel::Info, format!("{run} has no session {session}.")),
            }
        }
        ViewRequest::AllRepos => {
            let repos = all_repos(ctx).await?;
            show(ctx, ViewData::AllRepos { repos });
        }
        ViewRequest::Crew => anyhow::bail!("the crew panel isn't one run's view"),
        ViewRequest::Config
        | ViewRequest::Settings { .. }
        | ViewRequest::Models { .. }
        | ViewRequest::Approvals => {
            anyhow::bail!("the settings views aren't run views")
        }
        ViewRequest::WorkflowLibrary => anyhow::bail!("the workflow files aren't a run's"),
        ViewRequest::RepoFiles => anyhow::bail!("the repo's files aren't a run's"),
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
    let summary = ctx
        .services
        .conductor
        .status(&run)
        .await
        .with_context(|| format!("the status of {run}"))?;
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

/// `/context`: every live session's context, including the chat's own row.
pub(crate) async fn context(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    chat: Option<ContextRow>,
) -> Handled {
    let rotate_at = ctx.settings().sessions.rotate_at.clone();
    let mut rows: Vec<ContextRow> = chat.into_iter().collect();
    let attached = runs::attached(runs).cloned();
    if let Some(run) = &attached {
        let finished = runs
            .attached
            .as_ref()
            .is_some_and(|attached| matches!(attached.stage, Stage::Finished(_)));
        if !finished {
            rows.extend(capped_rows(&run_events(ctx, runs, run), run, &rotate_at));
        }
    }
    match ctx.services.conductor.list().await {
        Ok(list) => {
            for other in list
                .iter()
                .filter(|summary| summary.live && Some(&summary.run) != attached.as_ref())
            {
                let events = run_events(ctx, runs, &other.run);
                rows.extend(capped_rows(&events, &other.run, &rotate_at));
            }
        }
        Err(err) => ctx.notice(
            NoteLevel::Warn,
            format!("Other runs' sessions are left out: {err}"),
        ),
    }
    show(
        ctx,
        ViewData::Context {
            view: ContextView { rows },
        },
    );
    Ok(Vec::new())
}

/// A run's live sessions, each capped where it rotates. A share of the window gives each
/// session its own cap, and none while its window is unknown.
fn capped_rows(events: &[RunEvent], run: &RunId, rotate_at: &RotateAt) -> Vec<ContextRow> {
    views::context_rows(events, run, 0)
        .into_iter()
        .map(|row| ContextRow {
            cap: rotate_at.tokens(row.window).ok().flatten(),
            ..row
        })
        .collect()
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
    show_workflow(ctx, runs, &run, workflow.as_ref());
    Ok(Vec::new())
}

fn show_workflow(ctx: &Ctx, runs: &RunsState, run: &RunId, workflow: Option<&WorkflowId>) {
    let events = run_events(ctx, runs, run);
    match views::workflow_view(&events, run, workflow) {
        Some(view) => show(
            ctx,
            ViewData::Workflow {
                view: Box::new(view),
            },
        ),
        None => {
            let text = match workflow {
                Some(workflow) => format!("This run has no workflow {workflow}."),
                None => "This run hasn't started a workflow yet. Ctrl+T shows it once one starts."
                    .to_owned(),
            };
            ctx.notice(NoteLevel::Info, text);
        }
    }
}

/// This repo first, then every other repo with a live supervisor, newest update first.
pub(crate) async fn all_repos(ctx: &Ctx) -> anyhow::Result<Vec<RepoRuns>> {
    let here = ctx
        .services
        .conductor
        .list()
        .await
        .context("listing this repo's runs")?;
    let mut others: BTreeMap<PathBuf, Vec<argonauts_core::api::RunSummary>> = BTreeMap::new();
    for live in live_registry::live_supervisors(&ctx.env.paths) {
        if live.repo_root == ctx.env.repo_root {
            continue;
        }
        // A repo that moved or vanished since its supervisor registered has nothing to show.
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
