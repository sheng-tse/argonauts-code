//! The Overview's Status tab, read by code: the runs in this folder and how everything is set
//! up. It is the panel's own read, so whatever goes wrong only leaves a part of the tab empty;
//! nothing here writes into the chat.
//!
//! The CLIs' versions are the ones Doctor found as the app started, and who each one is signed
//! in as is asked of the CLI's own login check once, off the loop, or read from the screen it
//! draws as it starts where it names the account nowhere else. A check that names no address
//! is followed by a live read of the CLI's plan usage, which does. The addresses stay on this
//! screen; nothing here sends them anywhere.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use argonauts_conductor::{AgentStatus, DoctorReport};
use argonauts_core::app::{AppEvent, ConfigEntry, ViewData};
use argonauts_core::limits::QuotaBoard;
use argonauts_core::names::VendorId;
use argonauts_core::status_tab::{
    CliStatus, Conversation, Folder, MainSession, SettingsFile, SettingsInEffect, StatusTabView,
    WorkflowSetup,
};

use super::chat::{self, ChatState};
use super::runs::{self, RunsState};
use super::status_line::header_agent;
use super::{Ctx, Effect, Outbox};
use crate::env::FolderGit;
use crate::setup::probe::LOGIN_TIMEOUT;

/// What the tab knows of the CLIs between reads: Doctor's last check, and who each one is
/// signed in as once that has been asked.
#[derive(Debug, Default)]
pub(crate) struct CliFacts {
    checked: Mutex<Option<DoctorReport>>,
    sign_ins: Mutex<SignIns>,
}

#[derive(Debug, Default)]
enum SignIns {
    #[default]
    NotAsked,
    Asking,
    Known(BTreeMap<String, SignIn>),
}

/// What a CLI's login check says of the account, where it says anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SignIn {
    account: Option<String>,
    plan: Option<String>,
    /// The check said it isn't signed in, or failed as one that isn't does.
    signed_out: bool,
}

impl CliFacts {
    /// Doctor's check of every CLI, as the app started or `/doctor` ran.
    pub(crate) fn checked(&self, report: DoctorReport) {
        *self.checked.lock().unwrap_or_else(PoisonError::into_inner) = Some(report);
    }

    fn report(&self) -> Option<DoctorReport> {
        self.checked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn sign_ins(&self) -> BTreeMap<String, SignIn> {
        match &*self.sign_ins.lock().unwrap_or_else(PoisonError::into_inner) {
            SignIns::Known(known) => known.clone(),
            _ => BTreeMap::new(),
        }
    }

    /// True the first time only, so the login checks run once a session.
    fn start_asking(&self) -> bool {
        let mut sign_ins = self.sign_ins.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(*sign_ins, SignIns::NotAsked) {
            return false;
        }
        *sign_ins = SignIns::Asking;
        true
    }

    fn known(&self, known: BTreeMap<String, SignIn>) {
        *self.sign_ins.lock().unwrap_or_else(PoisonError::into_inner) = SignIns::Known(known);
    }
}

pub(crate) async fn show(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    chat: &ChatState,
) -> anyhow::Result<Vec<Effect>> {
    let view = read(ctx, runs, chat).await;
    send(&ctx.out, view.clone());
    ask_sign_ins(ctx, view);
    Ok(Vec::new())
}

fn send(out: &Outbox, view: StatusTabView) {
    out.send(AppEvent::View {
        data: Box::new(ViewData::StatusTab {
            view: Box::new(view),
        }),
    });
}

async fn read(ctx: &mut Ctx, runs: &mut RunsState, chat: &ChatState) -> StatusTabView {
    let listed = ctx.services.conductor.list().await.unwrap_or_default();
    // The run on screen, or else the newest one here.
    let followed = runs::attached(runs)
        .cloned()
        .or_else(|| listed.first().map(|summary| summary.run.clone()));
    let run = match followed {
        Some(run) => ctx.services.conductor.status(&run).await.ok(),
        None => None,
    };
    let settings = ctx.settings();
    let (worker, effort) = header_agent(ctx);
    let context = chat::context_row(chat);
    let main = MainSession {
        worker,
        effort,
        context_used: context
            .as_ref()
            .map(|row| row.used)
            .filter(|used| *used > 0),
        context_window: context.as_ref().and_then(|row| row.window),
    };
    let clis = ctx
        .cli_facts
        .report()
        .map(|report| {
            let sign_ins = ctx.cli_facts.sign_ins();
            clis(
                &report,
                &main.worker.worker,
                &sign_ins,
                ctx.services.quota.as_ref(),
            )
        })
        .unwrap_or_default();
    StatusTabView {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        conversation: chat::started(chat).map(|started| Conversation {
            id: chat::id(chat).to_string(),
            started,
        }),
        folder: Folder {
            shown: ctx.env.display_repo(),
            git: ctx.env.git() == FolderGit::Repository,
        },
        workflow: WorkflowSetup {
            name: settings.workflow.clone(),
            ultracode: settings.ultracode,
            size: settings.workflow_size.as_str().to_owned(),
        },
        mode: settings.approvals.mode().label().to_owned(),
        clis,
        settings: settings_in_effect(ctx, chat),
        main: Some(main),
        runs: listed,
        run,
    }
}

/// For Doctor: each CLI the Status tab shows as signed in without its address, and why.
pub(crate) fn unnamed_accounts(ctx: &Ctx) -> Vec<String> {
    let Some(report) = ctx.cli_facts.report() else {
        return Vec::new();
    };
    let sign_ins = ctx.cli_facts.sign_ins();
    let main = header_agent(ctx).0.worker;
    clis(&report, &main, &sign_ins, ctx.services.quota.as_ref())
        .into_iter()
        .filter(|cli| cli.installed && cli.account.is_none() && !cli.signed_out)
        .map(|cli| {
            format!(
                "{} · {} · {}",
                cli.name,
                argonauts_core::status_tab::UNNAMED_ACCOUNT,
                unnamed_why(&cli.id, sign_ins.contains_key(&cli.id))
            )
        })
        .collect()
}

/// Why a CLI's address isn't shown: what was asked, and what it said.
fn unnamed_why(id: &str, checked: bool) -> &'static str {
    match (id, checked) {
        ("codex", _) => {
            "its login check doesn't print the address, and no read of its plan usage has named it yet"
        }
        ("antigravity-cli", true) => "its start-up screen showed no address",
        ("antigravity-cli", false) => {
            "its start-up screen is read once the Status tab opens, and hasn't been yet"
        }
        (_, true) => "its login check doesn't print the address",
        (_, false) => "it has no check that names the account",
    }
}

/// Every CLI Doctor checked, the main session's first, each with who it is signed in as.
fn clis(
    report: &DoctorReport,
    main: &str,
    sign_ins: &BTreeMap<String, SignIn>,
    quota: &dyn QuotaBoard,
) -> Vec<CliStatus> {
    let mut clis: Vec<CliStatus> = report
        .agents
        .iter()
        .map(|check| {
            let installed = check.status != AgentStatus::NotInstalled;
            let mut sign_in = sign_ins.get(&check.id).cloned().unwrap_or_default();
            // A CLI whose login check names no account may still name it when its plan
            // usage is read live, as Codex does.
            if sign_in.account.is_none()
                && !sign_in.signed_out
                && let Some(live) = quota.live(&VendorId(check.id.clone()))
                && live.account.is_some()
            {
                sign_in.account = live.account;
                sign_in.plan = sign_in.plan.or(live.plan);
            }
            CliStatus {
                id: check.id.clone(),
                name: check.display_name.clone(),
                version: check.version.clone().filter(|_| installed),
                installed,
                main: check.id == main,
                account: sign_in.account,
                signed_out: sign_in.signed_out,
                plan: sign_in.plan,
                note: match check.status {
                    AgentStatus::NotInstalled => "not installed".to_owned(),
                    AgentStatus::Tested => "checked".to_owned(),
                    AgentStatus::Newer => "newer than checked, works".to_owned(),
                    AgentStatus::Untested => "not a checked version, may still work".to_owned(),
                    AgentStatus::UnknownVersion => "unknown version".to_owned(),
                },
            }
        })
        .collect();
    clis.sort_by_key(|cli| !cli.main);
    clis
}

/// Each settings file with what it sets, the mods, the main session's tools and the MCP
/// servers its CLI brought.
fn settings_in_effect(ctx: &Ctx, chat: &ChatState) -> SettingsInEffect {
    let keys: Vec<ConfigEntry> = ctx
        .settings
        .entries()
        .into_iter()
        .filter(|entry| entry.layer != "built-in")
        .collect();
    let mut files: Vec<SettingsFile> = Vec::new();
    for entry in &keys {
        match files.iter_mut().find(|file| file.layer == entry.layer) {
            Some(file) => file.keys += 1,
            None => files.push(SettingsFile {
                path: layer_path(ctx, &entry.layer),
                layer: entry.layer.clone(),
                keys: 1,
            }),
        }
    }
    let mods = crate::mods::loaded(&ctx.env);
    let built_in = mods
        .mods
        .iter()
        .filter(|loaded| loaded.origin == argonauts_core::settings::mods::ModOrigin::BuiltIn)
        .count();
    let yours = mods
        .mods
        .iter()
        .filter(|loaded| loaded.origin != argonauts_core::settings::mods::ModOrigin::BuiltIn)
        .map(|loaded| loaded.file.name.clone())
        .collect();
    SettingsInEffect {
        files,
        mods_built_in: built_in,
        mods_yours: yours,
        tools: chat::main_tools(ctx, chat)
            .iter()
            .map(|name| tool_words(name))
            .collect(),
        tools_off: !ctx.settings().main_session.tools,
        mcp_servers: chat::mcp_servers(chat),
        keys,
    }
}

/// Where a layer's keys come from, as the tab names it.
fn layer_path(ctx: &Ctx, layer: &str) -> String {
    let repo = &ctx.env.repo_root;
    match layer {
        "global" => ctx.env.display(&ctx.env.paths.global_config()),
        "project" => shown_in(repo, &argonauts_core::paths::project_config_path(repo)),
        "project-local" => shown_in(
            repo,
            &repo
                .join(".argonauts")
                .join(argonauts_core::layout::project_files::CONFIG_LOCAL),
        ),
        "workflow" => format!("the {} workflow", ctx.settings().workflow),
        "flag" => "--set on the command line".to_owned(),
        "session" => "this session".to_owned(),
        other => other.to_owned(),
    }
}

/// A file inside the folder, from the folder.
fn shown_in(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo)
        .map(|inside| inside.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// What a tool of the main session does, in a few words.
fn tool_words(name: &str) -> String {
    match name {
        "argonauts_start_workflow" => "start a workflow".to_owned(),
        "argonauts_set_goal" => "set a goal".to_owned(),
        "argonauts_setup" => "setup".to_owned(),
        "argonauts_guide" => "the guide".to_owned(),
        "argonauts_lookup" => "records".to_owned(),
        other => other
            .trim_start_matches("argonauts_")
            .replace('_', " ")
            .to_owned(),
    }
}

/// The first time the tab opens, each installed CLI's login check runs once, off the loop, and
/// the tab is drawn again with who each one is signed in as.
fn ask_sign_ins(ctx: &Ctx, mut view: StatusTabView) {
    let Some(report) = ctx.cli_facts.report() else {
        return;
    };
    let adapters = match crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved()) {
        Ok(adapters) => adapters,
        Err(_) => return,
    };
    let checks: Vec<(String, std::path::PathBuf, Vec<String>)> = report
        .agents
        .iter()
        .filter(|check| check.enabled && check.status != AgentStatus::NotInstalled)
        .filter_map(|check| {
            let path = check.path.clone()?;
            let adapter = adapters.iter().find(|adapter| adapter.id() == check.id)?;
            let args = adapter.setup_hints().login_check?;
            Some((check.id.clone(), path, args))
        })
        .collect();
    // A CLI that names who is signed in only as it starts has its start-up screen read.
    let banners: Vec<(String, std::path::PathBuf, Arc<dyn argonauts_core::Adapter>)> = report
        .agents
        .iter()
        .filter(|check| check.enabled && check.status != AgentStatus::NotInstalled)
        .filter_map(|check| {
            let path = check.path.clone()?;
            let adapter = adapters
                .iter()
                .find(|adapter| adapter.id() == check.id && adapter.reads_banner())?;
            Some((check.id.clone(), path, Arc::clone(adapter)))
        })
        .collect();
    if (checks.is_empty() && banners.is_empty()) || !ctx.cli_facts.start_asking() {
        return;
    }
    let cwd = ctx.env.repo_root.clone();
    let readers = super::account::live_readers(ctx);
    let workers = Arc::clone(&ctx.services.workers);
    let internal = ctx.internal.clone();
    let facts = Arc::clone(&ctx.cli_facts);
    let quota = Arc::clone(&ctx.services.quota);
    let out = ctx.out.clone();
    let main = view
        .main
        .as_ref()
        .map(|main| main.worker.worker.clone())
        .unwrap_or_default();
    tokio::spawn(async move {
        let asked = checks
            .into_iter()
            .map(|(id, path, args)| async move { (id, sign_in(&path, &args).await) });
        let read = banners.into_iter().map(|(id, path, adapter)| {
            let cwd = cwd.clone();
            async move { (id, banner_sign_in(path, cwd, adapter).await) }
        });
        let (asked, read) = futures::future::join(
            futures::future::join_all(asked),
            futures::future::join_all(read),
        )
        .await;
        let known: BTreeMap<String, SignIn> = asked
            .into_iter()
            .chain(read)
            .filter_map(|(id, sign_in)| Some((id, sign_in?)))
            .collect();
        facts.known(known.clone());
        view.clis = clis(&report, &main, &known, quota.as_ref());
        send(&out, view.clone());
        // A CLI whose check names no address is read live as the Usage tab reads it, which
        // names it, and the tab is drawn again once that read is in.
        for worker in readers {
            let vendor = worker.vendor();
            let unnamed = known
                .get(&vendor.0)
                .is_some_and(|sign_in| sign_in.account.is_none() && !sign_in.signed_out);
            let named = quota
                .live(&vendor)
                .is_some_and(|live| live.account.is_some());
            if !unnamed || named {
                continue;
            }
            let (facts, again, out, report, main) = (
                Arc::clone(&facts),
                Arc::clone(&quota),
                out.clone(),
                report.clone(),
                main.clone(),
            );
            let mut view = view.clone();
            super::account::read_account_live(
                workers.as_ref(),
                &quota,
                internal.clone(),
                &worker,
                move || {
                    view.clis = clis(&report, &main, &facts.sign_ins(), again.as_ref());
                    send(&out, view);
                },
            );
        }
    });
}

/// Runs a CLI's login check and reads the account and plan from what it prints, where it
/// prints them as Claude Code's `auth status` does.
async fn sign_in(program: &Path, args: &[String]) -> Option<SignIn> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(LOGIN_TIMEOUT, child)
        .await
        .ok()?
        .ok()?;
    // A check that fails is one that found no one signed in, as `codex login status` does.
    if !output.status.success() {
        return Some(SignIn {
            signed_out: true,
            ..SignIn::default()
        });
    }
    Some(sign_in_from(&String::from_utf8_lossy(&output.stdout)))
}

/// Reads who a CLI's start-up screen says is signed in, off the async threads.
async fn banner_sign_in(
    program: std::path::PathBuf,
    cwd: std::path::PathBuf,
    adapter: Arc<dyn argonauts_core::Adapter>,
) -> Option<SignIn> {
    let read = tokio::task::spawn_blocking(move || {
        super::banner::read_banner(&program, &cwd, super::banner::BANNER_TIMEOUT, |screen| {
            adapter.banner_account(screen)
        })
    });
    let found = read.await.ok().flatten()?;
    Some(SignIn {
        account: found.account,
        plan: found.plan,
        signed_out: found.signed_out,
    })
}

fn sign_in_from(printed: &str) -> SignIn {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(printed.trim()) else {
        return SignIn::default();
    };
    let text = |key: &str| {
        fields
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    if fields.get("loggedIn") == Some(&serde_json::Value::Bool(false)) {
        return SignIn {
            signed_out: true,
            ..SignIn::default()
        };
    }
    SignIn {
        account: text("email"),
        plan: text("subscriptionType").map(|plan| capitalized(&plan)),
        signed_out: false,
    }
}

/// `max` as `Max`, as the CLIs' own screens name a plan.
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
#[path = "status_tab_tests.rs"]
mod tests;
