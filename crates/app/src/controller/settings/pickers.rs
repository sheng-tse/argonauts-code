//! What a settings command with no argument answers: its picker's rows, a model list, or the
//! approvals view.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::app::{MissingCli, SettingRow, ViewData};
use argonauts_core::input::{ROLE_SANDBOX_KEYS, SlashCommand};
use argonauts_core::layout::{project_files, user_files};
use argonauts_core::model_picker::{EffortSlider, EffortStop};
use argonauts_core::names::RoleName;
use argonauts_core::settings::SettingScope;
use argonauts_core::views::LearningArea;
use argonauts_crew::workflow::builtin::BUILTIN as BUILTIN_WORKFLOWS;
use argonauts_policy::{SettingsReloader, approval_policy};
use argonauts_registry::workflows::WorkflowDirs;
use argonauts_registry::workflows_in;

use super::rows::{self, role_named};
use super::{args, labels, models};
use crate::builtin_adapters;
use crate::commands::adapters_here;
use crate::controller::Ctx;
use crate::host::services::installed;

/// The views that answer `command`, in the order they go out. `/model <role>` answers with its
/// models elsewhere, since the list's freshness lives with the settings state.
pub(crate) fn picker(ctx: &Ctx, command: &SlashCommand) -> Vec<ViewData> {
    let settings = &ctx.settings;
    match command {
        SlashCommand::Model { role: None, .. } => {
            vec![view(
                rows::rows(settings, Some("model"), None),
                "model",
                None,
            )]
        }
        SlashCommand::Effort { role, .. } => {
            if let Some(slider) = effort_slider(ctx, *role) {
                return vec![ViewData::Effort { slider }];
            }
            let section = role.map(|role| format!("roles.{}.effort", role.as_str()));
            vec![view(
                effort_rows(ctx, section.as_deref()),
                "effort",
                section,
            )]
        }
        SlashCommand::Workflow { .. } => vec![view(preset_rows(ctx), "workflow", None)],
        SlashCommand::Workflows => vec![view(workflow_rows(ctx), "phases", None)],
        SlashCommand::Agents { .. } => {
            vec![ViewData::Settings {
                rows: rows::rows(settings, Some("agents"), None),
                command: Some("agents".to_owned()),
                section: None,
                not_installed: not_installed(ctx),
            }]
        }
        SlashCommand::Approvals { key, .. } => {
            let section = key.as_deref().map(|key| args::under("approvals", key));
            vec![
                approvals(ctx),
                view(
                    rows::rows(settings, Some("approvals"), section.as_deref()),
                    "approvals",
                    section,
                ),
            ]
        }
        SlashCommand::Budget { key, .. } => {
            let section = key.as_deref().map(|key| args::under("limits", key));
            let found = rows::rows(settings, Some("limits"), section.as_deref());
            vec![view(found, "limits", section)]
        }
        SlashCommand::Fallback { role, key, .. } => {
            let base = role.map(args::fallback_section);
            let section = match (&base, key) {
                (Some(base), Some(key)) => Some(args::under(base, key)),
                (Some(base), None) => Some(base.clone()),
                (None, Some(key)) => Some(args::under("fallback", key)),
                (None, None) => None,
            };
            let found = rows::rows(settings, Some("fallback"), section.as_deref());
            vec![view(found, "fallback", section)]
        }
        SlashCommand::Permissions {
            role: None, key, ..
        } => {
            let found = rows::rows(settings, Some("permissions"), key.as_deref());
            vec![view(found, "permissions", key.clone())]
        }
        SlashCommand::Permissions {
            role: Some(role), ..
        } => {
            let section = format!("roles.{}.sandbox", role.as_str());
            // Every agent's value comes along, so a key the role leaves unset can say what
            // it follows.
            let found = rows::rows(settings, Some("permissions"), None)
                .into_iter()
                .filter(|row| {
                    rows::in_section(&row.key, &section)
                        || ROLE_SANDBOX_KEYS
                            .iter()
                            .any(|leaf| row.key == format!("sandbox.{leaf}"))
                })
                .collect();
            vec![view(found, "permissions", Some(section))]
        }
        SlashCommand::Narrator { .. } => {
            vec![view(
                rows::rows(settings, Some("narrator"), None),
                "narrator",
                None,
            )]
        }
        SlashCommand::Verbosity { .. } => {
            vec![view(
                rows::rows(settings, Some("verbosity"), None),
                "verbosity",
                None,
            )]
        }
        SlashCommand::Theme { .. } => {
            vec![view(
                rows::rows(settings, Some("theme"), None),
                "theme",
                None,
            )]
        }
        SlashCommand::Config { key, .. } => {
            let found = rows::rows(settings, Some("config"), key.as_deref());
            vec![view(found, "config", key.clone())]
        }
        SlashCommand::Records { agent } => learning(ctx, LearningArea::Records, agent.clone()),
        SlashCommand::Recipes { name } => learning(ctx, LearningArea::Recipes, name.clone()),
        SlashCommand::Skills => learning(ctx, LearningArea::Skills, None),
        SlashCommand::Memory => learning(ctx, LearningArea::Memory, None),
        _ => Vec::new(),
    }
}

/// A Learning picker: the area's rows from the learning files, and its settings as the
/// Settings tab lists them: `/records` has `routing.*` and `outcomes.share`, and so on. A
/// picker that can't be built says why instead.
fn learning(ctx: &Ctx, area: LearningArea, name: Option<String>) -> Vec<ViewData> {
    let mut settings_rows: Vec<SettingRow> = Vec::new();
    for section in area.settings() {
        for row in rows::rows(&ctx.settings, Some("config"), Some(section)) {
            if !settings_rows.iter().any(|kept| kept.key == row.key) {
                settings_rows.push(row);
            }
        }
    }
    let store = crate::learning::store(&ctx.env);
    let focus = crate::learning::view::Focus { name };
    match crate::learning::view::build(area, &store, ctx.settings(), &focus, settings_rows) {
        Ok(view) => vec![ViewData::Learning {
            view: Box::new(view),
        }],
        Err(err) => {
            ctx.notice(
                argonauts_core::NoteLevel::Warn,
                format!("/{} couldn't read its files: {err:#}", area.command()),
            );
            Vec::new()
        }
    }
}

/// The approval policy's view: modes, deciders, shadow records, and spot checks.
pub(crate) fn approvals(ctx: &Ctx) -> ViewData {
    let shared = ctx.settings.shared();
    let reload: SettingsReloader = Arc::new(move || Some((*shared).clone()));
    let policy = approval_policy(
        ctx.settings.shared(),
        reload,
        &ctx.env.state_dir,
        Arc::clone(&ctx.services.workers),
        Arc::clone(&ctx.services.steps),
    );
    let mut view = policy.view();
    // The settings check's warnings about approvals, such as a picked answerer the approver
    // can't check, show where you'd change them.
    view.warnings = ctx
        .settings
        .resolved()
        .warnings
        .iter()
        .filter(|warning| warning.contains("approvals."))
        .cloned()
        .collect();
    ViewData::Approvals { view }
}

fn view(rows: Vec<SettingRow>, command: &str, section: Option<String>) -> ViewData {
    ViewData::Settings {
        rows,
        command: Some(command.to_owned()),
        section,
        not_installed: Vec::new(),
    }
}

/// The agent CLIs this machine doesn't have, with what each adapter says installs it. Only
/// PATH and `agents.<id>.binary` are looked at, so `/agents` opens at once.
fn not_installed(ctx: &Ctx) -> Vec<MissingCli> {
    let resolved = ctx.settings.resolved();
    let adapters = adapters_here(Some(&ctx.env), resolved).unwrap_or_else(|err| {
        tracing::warn!("listing the agent CLIs for /agents: {err:#}");
        builtin_adapters()
    });
    adapters
        .iter()
        .filter(|adapter| !installed(&resolved.settings, adapter))
        .map(|adapter| MissingCli {
            id: adapter.id().to_owned(),
            install: adapter.setup_hints().install,
        })
        .collect()
}

/// `/effort`'s slider over the levels the role's model takes, starting on the level set now,
/// else the model's default, which for a CLI's default is that of the model it runs. `None`
/// while the router picks the worker, so there's no one model, and for a role whose model takes
/// no effort setting. The default agent's view always opens, since it holds All hands.
fn effort_slider(ctx: &Ctx, role: Option<RoleName>) -> Option<EffortSlider> {
    let role = role.filter(|role| *role != RoleName::Main);
    let (worker, levels) = models::efforts(ctx, role);
    let worker = worker?;
    let registry = &ctx.services.registry;
    let catalog = registry.catalog();
    let entry = registry.get(&worker);
    let label = catalog.label(&worker);
    let name = entry
        .as_ref()
        .and_then(|entry| entry.name.clone())
        .unwrap_or(label.name);
    let default = entry
        .and_then(|entry| entry.default_effort)
        .or_else(|| catalog.entry(&worker)?.default_effort.clone())
        .or_else(|| models::default_effort(ctx, &worker));
    let stops = levels
        .into_iter()
        .map(|level| EffortStop {
            description: catalog.effort_note(&worker, &level).unwrap_or_default(),
            level,
        })
        .collect();
    let current = labels::effort_set(ctx.settings(), role, &worker.worker);
    let slider = EffortSlider::new(
        role,
        worker.clone(),
        name.clone(),
        stops,
        default.as_deref(),
        current.as_deref(),
    );
    match role {
        None => {
            let slider = slider.unwrap_or_else(|| EffortSlider::without_levels(None, worker, name));
            Some(slider.with_ultracode(ctx.settings().ultracode))
        }
        Some(_) => slider,
    }
}

/// Each role's effort, offering only the levels its model takes.
fn effort_rows(ctx: &Ctx, section: Option<&str>) -> Vec<SettingRow> {
    let mut found = rows::rows(&ctx.settings, Some("effort"), section);
    for row in &mut found {
        let role = row
            .key
            .split('.')
            .nth(1)
            .and_then(role_named)
            .filter(|role| *role != RoleName::Main);
        let (_, levels) = models::efforts(ctx, role);
        row.allowed = levels.iter().map(|level| args::quoted(level)).collect();
    }
    found
}

/// One row per preset you can pick, each `preset = "<name>"` with what it does. The row of the
/// preset in effect carries its layer; the others are `built-in`, since nothing sets them.
fn preset_rows(ctx: &Ctx) -> Vec<SettingRow> {
    let current = ctx.settings().workflow.clone();
    let layer = ctx.settings.layer("workflow");
    let presets = args::preset_list(ctx);
    let allowed: Vec<String> = presets
        .iter()
        .map(|preset| args::quoted(&preset.name))
        .collect();
    presets
        .into_iter()
        .map(|preset| {
            let description = match (preset.description.is_empty(), preset.source.as_str()) {
                (_, "built-in") => preset.description,
                (true, source) => format!("From {}", ctx.env.shown(Path::new(source))),
                (false, source) => format!(
                    "{} (from {})",
                    preset.description,
                    ctx.env.shown(Path::new(source))
                ),
            };
            SettingRow {
                key: "workflow".to_owned(),
                value: args::quoted(&preset.name),
                layer: if preset.name == current {
                    layer.clone()
                } else {
                    "built-in".to_owned()
                },
                // A preset never picks another, so this key is never the preset's.
                preset_from: None,
                default_scope: SettingScope::Project,
                safety: false,
                allowed: allowed.clone(),
                description,
            }
        })
        .collect()
}

/// One row per workflow: the crew engine's built-ins, then your files and the project's, a file
/// shadowing a workflow of the same name, each with its description and where it came from.
fn workflow_rows(ctx: &Ctx) -> Vec<SettingRow> {
    let current = ctx.settings().phases.as_str().to_owned();
    let layer = ctx.settings.layer("phases");
    let global = ctx.env.paths.home().join(user_files::WORKFLOWS);
    let project = ctx
        .env
        .project_config()
        .with_file_name(project_files::WORKFLOWS);
    let found = workflows_in(&WorkflowDirs {
        global: &global,
        project: Some(&project),
        builtins: BUILTIN_WORKFLOWS,
    });
    let mut names: Vec<(String, String)> = found
        .into_iter()
        .map(|info| {
            let from = if info.source == "built-in" {
                "Built in".to_owned()
            } else {
                format!("From {}", ctx.env.shown(Path::new(&info.source)))
            };
            let description = if info.description.is_empty() {
                from
            } else {
                format!("{from}: {}", info.description)
            };
            (info.name, description)
        })
        .collect();
    if !names.iter().any(|(name, _)| *name == current) {
        names.insert(0, (current.clone(), String::new()));
    }
    let allowed: Vec<String> = names.iter().map(|(name, _)| args::quoted(name)).collect();
    names
        .into_iter()
        .map(|(name, description)| SettingRow {
            key: "phases".to_owned(),
            value: args::quoted(&name),
            layer: if name == current {
                layer.clone()
            } else {
                "built-in".to_owned()
            },
            preset_from: if name == current {
                ctx.settings.preset_from(&layer)
            } else {
                None
            },
            default_scope: SettingScope::Project,
            safety: false,
            allowed: allowed.clone(),
            description,
        })
        .collect()
}
