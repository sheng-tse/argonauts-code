//! The Overview's Status tab, read by code: the runs in this folder and how everything is set
//! up. It is the panel's own read, so whatever goes wrong only leaves a part of the tab empty;
//! nothing here writes into the conversation.
//!
//! The CLIs' versions are the ones Doctor found as the app started, and who each one is signed
//! in as is asked of the CLI's own login check once, off the loop, or read from the screen it
//! draws as it starts where it names the account nowhere else. A check that names no address
//! is followed by a live read of the CLI's plan usage, which does. The addresses stay on this
//! screen; nothing here sends them anywhere.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use argonauts_conductor::{AgentStatus, DoctorReport};
use argonauts_core::adapter::BannerAccount;
use argonauts_core::app::{AppEvent, ConfigEntry, ViewData};
use argonauts_core::limits::PlanUsageBoard;
use argonauts_core::names::AgentId;
use argonauts_core::settings::layer;
use argonauts_core::status_tab::{
    CliProvider, CliStatus, CliVersionCheck, Conversation, Folder, MainSession, SettingsFile,
    SettingsInEffect, StatusTabView, WorkflowSetup, providers_unread,
};

use super::conversation::{self, ConversationState};
use super::header_and_bottom_line::header_agent;
use super::runs::{self, RunsState};
use super::status_health;
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
    /// The CLI asked whether to trust this folder before it showed who is signed in.
    untrusted_folder: bool,
    /// What it can use, for a CLI that keeps providers of its own in place of one account.
    providers: Option<Vec<CliProvider>>,
    /// Its providers were asked for, and nothing it printed could be read as their list.
    providers_unread: bool,
    /// The model it uses when none is named, with its provider, as the tab says it.
    default_model: Option<String>,
}

impl CliFacts {
    /// Doctor's check of every CLI as the app started.
    pub(crate) fn checked(&self, report: DoctorReport) {
        *self.checked.lock().unwrap_or_else(PoisonError::into_inner) = Some(report);
    }

    /// One CLI as it is now, such as after its update, in place of what Doctor last found of
    /// it. Whether it is turned on stays as the settings had it.
    pub(crate) fn rechecked(&self, check: argonauts_conductor::AgentCheck) {
        let mut checked = self.checked.lock().unwrap_or_else(PoisonError::into_inner);
        let kept = checked
            .as_mut()
            .and_then(|report| report.agents.iter_mut().find(|kept| kept.id == check.id));
        if let Some(kept) = kept {
            *kept = argonauts_conductor::AgentCheck {
                enabled: kept.enabled,
                ..check
            };
        }
    }

    pub(crate) fn report(&self) -> Option<DoctorReport> {
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
    conversation: &ConversationState,
) -> anyhow::Result<Vec<Effect>> {
    let view = read(ctx, runs, conversation).await;
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

async fn read(
    ctx: &mut Ctx,
    runs: &mut RunsState,
    conversation: &ConversationState,
) -> StatusTabView {
    let listed = ctx.services.conductor.list().await.unwrap_or_default();
    // The run on screen, or else the newest one here.
    let followed = runs::attached(runs)
        .cloned()
        .or_else(|| listed.first().map(|summary| summary.run.clone()));
    let at_work = followed
        .as_ref()
        .map(|run| status_health::at_work(ctx, run))
        .unwrap_or_default();
    let run = match followed {
        Some(run) => ctx.services.conductor.status(&run).await.ok(),
        None => None,
    };
    let settings = ctx.settings();
    let (worker, effort) = header_agent(ctx);
    let context = conversation::context_row(conversation);
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
                &main.worker.agent,
                &sign_ins,
                ctx.services.plan_usage.as_ref(),
                &version_checks(ctx, &report),
                &listing_providers(ctx),
            )
        })
        .unwrap_or_default();
    let health = status_health::rows(ctx, &clis, &at_work);
    let in_cli = conversation::in_cli::here(ctx, conversation);
    StatusTabView {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        conversation: conversation::started(conversation).map(|started| Conversation {
            id: conversation::id(conversation).to_string(),
            started,
        }),
        folder: Folder {
            shown: ctx.env.display_repo(),
            git: ctx.env.git() == FolderGit::Repository,
            history_size: match ctx.env.git() {
                FolderGit::Hidden(_) => {
                    crate::hidden_git::size(&ctx.env.state_dir).map(super::folder_git::megabytes)
                }
                _ => None,
            },
        },
        workflow: WorkflowSetup {
            name: settings.workflow.clone(),
            ultracode: settings.ultracode,
            size: settings.workflow_size.as_str().to_owned(),
        },
        mode: settings.approvals.mode().label().to_owned(),
        clis,
        settings: settings_in_effect(ctx, conversation),
        main: Some(main),
        in_cli,
        runs: listed,
        run,
        health,
    }
}

/// For Doctor: each CLI the Status tab shows as signed in without its address, and why.
pub(crate) fn unnamed_accounts(ctx: &Ctx) -> Vec<String> {
    let Some(report) = ctx.cli_facts.report() else {
        return Vec::new();
    };
    let sign_ins = ctx.cli_facts.sign_ins();
    let main = header_agent(ctx).0.agent;
    let checks = version_checks(ctx, &report);
    clis(
        &report,
        &main,
        &sign_ins,
        ctx.services.plan_usage.as_ref(),
        &checks,
        &listing_providers(ctx),
    )
    .into_iter()
    .filter(|cli| {
        cli.installed
            && cli.account.is_none()
            && cli.providers.is_none()
            && !cli.signed_out
            && !cli.untrusted_folder
            // One that keeps providers of its own has no address to show.
            && (!cli.lists_providers || cli.providers_unread)
    })
    .map(|cli| {
        if cli.lists_providers {
            return format!("{} · {}", cli.name, providers_unread(&cli.command));
        }
        format!(
            "{} · {} · {}",
            cli.name,
            argonauts_core::status_tab::UNNAMED_ACCOUNT,
            unnamed_why(&cli.id, sign_ins.contains_key(&cli.id))
        )
    })
    .collect()
}

/// The CLIs that keep a list of providers in place of one account, by id.
fn listing_providers(ctx: &Ctx) -> BTreeSet<String> {
    crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .map(|adapters| {
            adapters
                .iter()
                .filter(|adapter| !adapter.provider_checks().is_empty())
                .map(|adapter| adapter.id().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// How each installed CLI's version stands and what there is to update to, by id, from Doctor's
/// check and what sessions and the install sources have shown.
pub(crate) fn version_checks(
    ctx: &Ctx,
    report: &DoctorReport,
) -> BTreeMap<String, CliVersionCheck> {
    match crate::commands::adapters_here(Some(&ctx.env), ctx.settings.resolved()) {
        Ok(adapters) => crate::commands::version_checks(&ctx.env.paths, &adapters, report),
        Err(_) => BTreeMap::new(),
    }
}

/// Why a CLI's address isn't shown: what was asked, and what it said.
fn unnamed_why(id: &str, checked: bool) -> &'static str {
    match (id, checked) {
        ("codex", _) => {
            "its login check doesn't print the address, and no read of its plan usage has named it yet"
        }
        ("antigravity-cli", true) => "its start-up screen was read and showed no address",
        ("antigravity-cli", false) => {
            "its start-up screen is read once the Status tab opens, and hasn't been yet"
        }
        (_, true) => "its login check doesn't print the address",
        (_, false) => "it has no check that names the account",
    }
}

/// Every CLI Doctor checked, the main session's first, each with who it is signed in as or
/// what it can use, and how its version stands.
fn clis(
    report: &DoctorReport,
    main: &str,
    sign_ins: &BTreeMap<String, SignIn>,
    plan_usage: &dyn PlanUsageBoard,
    checks: &BTreeMap<String, CliVersionCheck>,
    listing: &BTreeSet<String>,
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
                && sign_in.providers.is_none()
                && !sign_in.signed_out
                && !sign_in.untrusted_folder
                && let Some(live) = plan_usage.live(&AgentId(check.id.clone()))
                && live.account.is_some()
            {
                sign_in.account = live.account;
                sign_in.plan = sign_in.plan.or(live.plan);
            }
            CliStatus {
                id: check.id.clone(),
                name: check.display_name.clone(),
                command: command_name(&check.binary),
                version: check.version.clone().filter(|_| installed),
                installed,
                main: check.id == main,
                account: sign_in.account,
                signed_out: sign_in.signed_out,
                untrusted_folder: sign_in.untrusted_folder,
                plan: sign_in.plan,
                lists_providers: listing.contains(&check.id),
                providers_unread: sign_in.providers_unread && sign_in.providers.is_none(),
                providers: sign_in.providers,
                default_model: sign_in.default_model,
                check: checks.get(&check.id).cloned().unwrap_or_default(),
            }
        })
        .collect();
    clis.sort_by_key(|cli| !cli.main);
    clis
}

/// `agy` for `/opt/bin/agy`: the command as you would type it.
fn command_name(binary: &str) -> String {
    Path::new(binary).file_name().map_or_else(
        || binary.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Each settings file with what it sets, the mods, the main session's tools and the MCP
/// servers its CLI brought.
fn settings_in_effect(ctx: &Ctx, conversation: &ConversationState) -> SettingsInEffect {
    let keys: Vec<ConfigEntry> = ctx
        .settings
        .entries()
        .into_iter()
        .filter(|entry| entry.layer != layer::BUILT_IN)
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
        tools: conversation::main_tools(ctx, conversation)
            .iter()
            .map(|name| tool_words(name))
            .collect(),
        tools_off: !ctx.settings().main_session.tools,
        mcp_servers: conversation::mcp_servers(conversation),
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
    // A CLI that keeps providers of its own is asked what it can use, by its own commands.
    let lists: Vec<(String, std::path::PathBuf, Arc<dyn argonauts_core::Adapter>)> = report
        .agents
        .iter()
        .filter(|check| check.enabled && check.status != AgentStatus::NotInstalled)
        .filter_map(|check| {
            let path = check.path.clone()?;
            let adapter = adapters.iter().find(|adapter| {
                adapter.id() == check.id && !adapter.provider_checks().is_empty()
            })?;
            Some((check.id.clone(), path, Arc::clone(adapter)))
        })
        .collect();
    let nothing_to_ask = checks.is_empty() && banners.is_empty() && lists.is_empty();
    if nothing_to_ask || !ctx.cli_facts.start_asking() {
        return;
    }
    let versions = version_checks(ctx, &report);
    let listing = listing_providers(ctx);
    let cwd = ctx.env.repo_root.clone();
    let repo = crate::host::clis::RepoModels::new(&ctx.env.repo_root, &ctx.env.state_dir);
    let confine = crate::confine::read_only(&ctx.settings.resolved().settings, Some(&repo));
    let readers = super::account::live_readers(ctx);
    let workers = Arc::clone(&ctx.services.workers);
    let internal = ctx.internal.clone();
    let facts = Arc::clone(&ctx.cli_facts);
    let plan_usage = Arc::clone(&ctx.services.plan_usage);
    let out = ctx.out.clone();
    let main = view
        .main
        .as_ref()
        .map(|main| main.worker.agent.clone())
        .unwrap_or_default();
    tokio::spawn(async move {
        let asked = checks
            .into_iter()
            .map(|(id, path, args)| async move { (id, sign_in(&path, &args).await) });
        let read = banners.into_iter().map(|(id, path, adapter)| {
            let cwd = cwd.clone();
            async move { (id, banner_sign_in(path, cwd, adapter).await) }
        });
        let listed = lists.into_iter().map(|(id, path, adapter)| {
            let asked = Asked {
                program: path,
                cwd: cwd.clone(),
                confine: Arc::clone(&confine),
            };
            async move { (id, providers(&asked, adapter).await) }
        });
        let (asked, read, listed) = futures::future::join3(
            futures::future::join_all(asked),
            futures::future::join_all(read),
            futures::future::join_all(listed),
        )
        .await;
        let known: BTreeMap<String, SignIn> = asked
            .into_iter()
            .chain(read)
            .chain(listed)
            .filter_map(|(id, sign_in)| Some((id, sign_in?)))
            .collect();
        facts.known(known.clone());
        view.clis = clis(
            &report,
            &main,
            &known,
            plan_usage.as_ref(),
            &versions,
            &listing,
        );
        send(&out, view.clone());
        // A CLI whose check names no address is read live as the Usage tab reads it, which
        // names it, and the tab is drawn again once that read is in.
        for worker in readers {
            let agent_id = worker.agent_id();
            let unnamed = known.get(&agent_id.0).is_some_and(|sign_in| {
                sign_in.account.is_none() && !sign_in.signed_out && !sign_in.untrusted_folder
            });
            let named = plan_usage
                .live(&agent_id)
                .is_some_and(|live| live.account.is_some());
            if !unnamed || named {
                continue;
            }
            let (facts, again, out, report, main, versions, listing) = (
                Arc::clone(&facts),
                Arc::clone(&plan_usage),
                out.clone(),
                report.clone(),
                main.clone(),
                versions.clone(),
                listing.clone(),
            );
            let mut view = view.clone();
            super::account::read_account_live(
                workers.as_ref(),
                &plan_usage,
                internal.clone(),
                &worker,
                move || {
                    view.clis = clis(
                        &report,
                        &main,
                        &facts.sign_ins(),
                        again.as_ref(),
                        &versions,
                        &listing,
                    );
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

/// How a CLI's listing commands run: its program, in the repo, inside the sandbox, since a
/// CLI started in a repo may load the plugins that repo sets up.
struct Asked {
    program: std::path::PathBuf,
    cwd: std::path::PathBuf,
    confine: Arc<dyn argonauts_core::confine::Confine>,
}

/// Asks a CLI that keeps providers of its own what it can use: each of its listing commands
/// in turn, with its updater kept off. A command that fails, runs out of time or can't be
/// sandboxed leaves its part unknown, and nothing but the CLI's own answers is read.
async fn providers(asked: &Asked, adapter: Arc<dyn argonauts_core::Adapter>) -> Option<SignIn> {
    let mut outputs = Vec::new();
    for args in adapter.provider_checks() {
        let mut spec =
            argonauts_core::CommandSpec::new(asked.program.display().to_string(), &asked.cwd)
                .args(args.iter().copied());
        spec.env_set = adapter
            .updater_off_env()
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let printed = match asked.confine.confine(adapter.id(), spec).await {
            Ok(spec) => listed(&spec).await,
            Err(why) => {
                tracing::warn!("asking {} for its providers: {why}", adapter.id());
                String::new()
            }
        };
        outputs.push(printed);
    }
    // It answered, and nothing in the answer is its list: that is said, never an account.
    let Some(found) = adapter.providers(&outputs) else {
        return Some(SignIn {
            providers_unread: true,
            ..SignIn::default()
        });
    };
    let default_model = found.default_model.map(|(route, provider)| {
        let model = route
            .split_once('/')
            .map_or(route.as_str(), |(_, model)| model);
        format!("{} on {provider}", model_words(adapter.id(), model))
    });
    Some(SignIn {
        providers: Some(found.providers),
        default_model,
        ..SignIn::default()
    })
}

/// What `spec` printed, or nothing when it fails or runs out of time.
async fn listed(spec: &argonauts_core::CommandSpec) -> String {
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    command.envs(spec.env_set.iter().cloned());
    match tokio::time::timeout(LOGIN_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => String::new(),
    }
}

/// `GLM 5` for `z-ai/glm-5`: a routed model as its name reads, without who publishes it.
fn model_words(cli: &str, model: &str) -> String {
    let name = model.rsplit('/').next().unwrap_or(model);
    argonauts_core::display_names::DisplayNames::default()
        .model(cli, Some(name))
        .unwrap_or_else(|| name.to_owned())
}

/// Reads who a CLI's start-up screen says is signed in, off the async threads, with the CLI's
/// own updater kept off. A screen that never said is still one that was read.
async fn banner_sign_in(
    program: std::path::PathBuf,
    cwd: std::path::PathBuf,
    adapter: Arc<dyn argonauts_core::Adapter>,
) -> Option<SignIn> {
    let read = tokio::task::spawn_blocking(move || {
        super::banner::read_banner(
            &program,
            &cwd,
            adapter.updater_off_env(),
            super::banner::BANNER_TIMEOUT,
            |screen| adapter.banner_account(screen),
        )
    });
    Some(sign_in_shown(read.await.ok()?))
}

/// What a start-up screen said, as the tab keeps it.
fn sign_in_shown(banner: Option<BannerAccount>) -> SignIn {
    match banner {
        Some(BannerAccount::SignedIn { account, plan }) => SignIn {
            account: Some(account),
            plan,
            ..SignIn::default()
        },
        Some(BannerAccount::SignedOut) => SignIn {
            signed_out: true,
            ..SignIn::default()
        },
        Some(BannerAccount::UntrustedFolder) => SignIn {
            untrusted_folder: true,
            ..SignIn::default()
        },
        None => SignIn::default(),
    }
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
        ..SignIn::default()
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
