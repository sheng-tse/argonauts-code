//! What every session gets from the settings: its worker's model, the effort, the sandbox, the
//! limits, compaction, native fan-out, the goal hash, and its status file.

use std::path::{Path, PathBuf};

use serde_json::Value;

use argonauts_core::events::session::InputPart;
use argonauts_core::names::{RoleName, SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout, SessionLifetime};
use argonauts_core::steps::StepName;
use argonauts_core::worker::{
    Access, SandboxMode, SandboxProfile, SessionLimits, SessionSpec, WorkerKind,
};
use argonauts_core::{CardId, RunId};

use crate::error::{EngineError, EngineResult};

/// Everything a spec is built from besides the settings.
#[derive(Debug, Clone)]
pub struct SpecInput {
    pub run: RunId,
    pub key: SessionKey,
    pub role: RoleName,
    pub worker: WorkerRef,
    pub kind: WorkerKind,
    pub card: Option<CardId>,
    pub step: Option<StepName>,
    pub access: Access,
    pub workdir: PathBuf,
    pub session_dir: PathBuf,
    pub prompt: Vec<InputPart>,
    pub json_schema: Option<Value>,
    /// The vendor session to resume.
    pub resume: Option<String>,
    pub lifetime: SessionLifetime,
    pub limits: SessionLimits,
    /// Required for `Build` and `Spike`; the worker refuses them without it.
    pub goal_sha256: Option<String>,
    pub status_file: Option<PathBuf>,
    /// Blind reviewers and judges never get native fan-out, nor the CLI's own MCP servers,
    /// plugins, or project settings.
    pub blind: bool,
    /// Its step lets it run a workflow of its own, so its CLI fans out as it sees fit.
    pub own_workflow: bool,
    /// The run is on autopilot.
    pub autopilot: bool,
    /// Paths the session must never read: the state directory, other worktrees, `~/.ssh`.
    pub deny_read: Vec<PathBuf>,
}

pub fn build(settings: &Settings, input: SpecInput) -> EngineResult<SessionSpec> {
    let builds = matches!(input.access, Access::Build | Access::Spike);
    if builds && input.goal_sha256.is_none() {
        return Err(EngineError::Refused(
            "a build or experiment session needs the approved goal's hash".to_owned(),
        ));
    }
    let temp = input.session_dir.join("tmp");
    let mut sandbox = sandbox(settings, input.access, &input.workdir, &temp)?;
    if input.kind == WorkerKind::Endpoint {
        // An endpoint runs no commands, so there is nothing to sandbox.
        sandbox.mode = SandboxMode::None;
    }
    sandbox.readable.extend(input.status_file.iter().cloned());
    sandbox.deny_read.extend(input.deny_read.iter().cloned());
    let role = settings.roles.get(&input.role);
    let native_fanout = if input.blind {
        NativeFanout::Switch(FanoutSwitch::Off)
    } else if input.own_workflow {
        // A workflow of its own starts agents of its own, so no cap of ours fits it.
        NativeFanout::Switch(FanoutSwitch::On)
    } else {
        role.and_then(|role| role.native_fanout)
            .unwrap_or(settings.native_fanout)
    };
    Ok(SessionSpec {
        run: Some(input.run),
        card: input.card,
        key: input.key,
        role: input.role,
        step: input.step,
        effort: effort(settings, input.role, &input.worker),
        worker: with_model(settings, &input.worker),
        access: input.access,
        workdir: input.workdir,
        session_dir: input.session_dir,
        prompt: separated(input.prompt),
        json_schema: input.json_schema,
        resume: input.resume,
        lifetime: input.lifetime,
        // Long sessions stay open for steering and deliveries; a one-shot answer must end.
        keep_input_open: builds,
        limits: input.limits,
        goal_sha256: input.goal_sha256,
        sandbox,
        compaction: role
            .and_then(|role| role.compaction)
            .unwrap_or(settings.compaction),
        native_fanout,
        isolated: input.blind,
        autopilot: input.autopilot,
        status_file: input.status_file,
        env: Vec::new(),
        asking: None,
        cli_workflow: argonauts_core::worker::CliWorkflow::default(),
    })
}

/// The parts go out concatenated, so each but the last ends in a blank line. Otherwise a card's
/// last line and the goal's first run together, as `Kind: the whole changeAdd median`.
fn separated(mut parts: Vec<InputPart>) -> Vec<InputPart> {
    let last = parts.len().saturating_sub(1);
    for part in &mut parts[..last] {
        let text = &mut part.text;
        if text.is_empty() || text.ends_with("\n\n") {
            continue;
        }
        text.push_str(if text.ends_with('\n') { "\n" } else { "\n\n" });
    }
    parts
}

/// `roles.<role>.effort`, else `agents.<harness>.effort`, else unset.
pub fn effort(settings: &Settings, role: RoleName, worker: &WorkerRef) -> Option<String> {
    settings
        .roles
        .get(&role)
        .and_then(|role| role.effort.clone())
        .or_else(|| {
            settings
                .agents
                .get(&worker.worker)
                .and_then(|agent| agent.effort.clone())
        })
}

/// The worker with its model filled in: its own reference's model, else
/// `agents.<harness>.model`.
pub fn with_model(settings: &Settings, worker: &WorkerRef) -> WorkerRef {
    if worker.model.is_some() {
        return worker.clone();
    }
    let model = settings
        .agents
        .get(&worker.worker)
        .and_then(|agent| agent.model.clone());
    WorkerRef::new(worker.worker.clone(), model)
}

/// Writable: the worktree and temp for building access, nothing for read-only. Network per
/// `sandbox.network`.
pub fn sandbox(
    settings: &Settings,
    access: Access,
    workdir: &Path,
    temp: &Path,
) -> EngineResult<SandboxProfile> {
    let mode = match settings.sandbox.mode {
        SandboxChoice::Os => SandboxMode::Os,
        SandboxChoice::Container => SandboxMode::Container,
        // Your own safety key; the hard limits flag it at every checkpoint.
        SandboxChoice::Tool | SandboxChoice::None => SandboxMode::None,
    };
    let mut writable = vec![temp.to_path_buf()];
    if matches!(access, Access::Build | Access::Spike) {
        writable.insert(0, workdir.to_path_buf());
        for extra in &settings.sandbox.extra_writable {
            writable.push(extra_path(extra, workdir)?);
        }
    }
    Ok(SandboxProfile {
        mode,
        writable,
        readable: Vec::new(),
        deny_read: Vec::new(),
        network: settings.sandbox.network && !matches!(access, Access::NoTools),
        proxy_port: None,
        ask_port: None,
        closed: Vec::new(),
    })
}

/// `~/` is your home, and a relative path is inside the worktree. A path that climbs out of the
/// worktree with `..` is refused, since it could open the state directory.
fn extra_path(extra: &str, workdir: &Path) -> EngineResult<PathBuf> {
    let path = Path::new(extra);
    if path
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(EngineError::Refused(format!(
            "sandbox.extra_writable {extra:?} climbs out with \"..\""
        )));
    }
    if let Some(rest) = extra.strip_prefix("~/") {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            EngineError::Refused(format!("sandbox.extra_writable {extra:?} needs HOME"))
        })?;
        return Ok(PathBuf::from(home).join(rest));
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use argonauts_core::settings::agents::AgentSettings;
    use argonauts_core::settings::roles::RoleSettings;

    fn settings() -> Settings {
        let mut settings = Settings::default();
        let owner = RoleSettings {
            effort: Some("high".to_owned()),
            ..RoleSettings::default()
        };
        settings.roles.insert(RoleName::Worker, owner);
        let codex = AgentSettings {
            effort: Some("low".to_owned()),
            model: Some("gpt-x".to_owned()),
            ..AgentSettings::default()
        };
        settings.agents.insert("codex".to_owned(), codex);
        settings
    }

    #[test]
    fn prompt_parts_never_run_together() {
        let part = |text: &str| InputPart {
            kind: argonauts_core::events::session::InputPartKind::Prompt,
            label: "part".to_owned(),
            text: text.to_owned(),
            approx_tokens: 1,
            source: None,
        };
        let parts = separated(vec![
            part("Kind: the whole change"),
            part("Add median.\n"),
            part("Rules.\n\n"),
            part(""),
            part("Report."),
        ]);
        let sent: String = parts.iter().map(|part| part.text.as_str()).collect();
        assert_eq!(
            sent,
            "Kind: the whole change\n\nAdd median.\n\nRules.\n\nReport."
        );
        assert!(separated(Vec::new()).is_empty());
    }

    #[test]
    fn effort_is_the_roles_then_the_harnesss_then_unset() {
        let settings = settings();
        let codex = WorkerRef::new("codex", None);
        let claude = WorkerRef::new("claude-code", None);
        assert_eq!(
            effort(&settings, RoleName::Worker, &codex).as_deref(),
            Some("high")
        );
        assert_eq!(
            effort(&settings, RoleName::Reviewer, &codex).as_deref(),
            Some("low")
        );
        assert_eq!(effort(&settings, RoleName::Reviewer, &claude), None);
    }

    #[test]
    fn the_model_is_the_references_own_else_the_harnesss() {
        let settings = settings();
        let own = WorkerRef::new("codex", Some("gpt-y".to_owned()));
        assert_eq!(with_model(&settings, &own), own);
        assert_eq!(
            with_model(&settings, &WorkerRef::new("codex", None))
                .model
                .as_deref(),
            Some("gpt-x")
        );
        assert_eq!(
            with_model(&settings, &WorkerRef::new("agy", None)).model,
            None
        );
    }

    #[test]
    fn a_blind_session_is_isolated_and_never_fans_out() {
        let input = |blind: bool| SpecInput {
            run: RunId::new("run-01").unwrap(),
            key: SessionKey::parse("s-001").unwrap(),
            role: RoleName::Reviewer,
            worker: WorkerRef::new("claude-code", None),
            kind: WorkerKind::Harness,
            card: None,
            step: None,
            access: Access::ReadOnly,
            workdir: PathBuf::from("/w"),
            session_dir: PathBuf::from("/s"),
            prompt: Vec::new(),
            json_schema: None,
            resume: None,
            lifetime: SessionLifetime::Task,
            limits: SessionLimits::default(),
            goal_sha256: None,
            status_file: None,
            blind,
            own_workflow: false,
            autopilot: false,
            deny_read: Vec::new(),
        };
        let settings = Settings::default();
        let blind = build(&settings, input(true)).unwrap();
        assert!(blind.isolated);
        assert_eq!(blind.native_fanout, NativeFanout::Switch(FanoutSwitch::Off));
        // A step that lets its worker run a workflow of its own lifts the cap, never for a
        // blind session.
        let own = |blind: bool| SpecInput {
            own_workflow: true,
            ..input(blind)
        };
        let free = build(&settings, own(false)).unwrap();
        assert_eq!(free.native_fanout, NativeFanout::Switch(FanoutSwitch::On));
        let still_blind = build(&settings, own(true)).unwrap();
        assert_eq!(
            still_blind.native_fanout,
            NativeFanout::Switch(FanoutSwitch::Off)
        );
        let seeing = build(&settings, input(false)).unwrap();
        assert!(
            !seeing.isolated,
            "the CLI loads its own agents, skills, and servers"
        );
    }

    #[test]
    fn only_building_access_writes_the_worktree() {
        let settings = Settings::default();
        let work = Path::new("/w");
        let temp = Path::new("/t");
        let build = sandbox(&settings, Access::Build, work, temp).unwrap();
        assert_eq!(build.writable, vec![work.to_path_buf(), temp.to_path_buf()]);
        let read = sandbox(&settings, Access::ReadOnly, work, temp).unwrap();
        assert_eq!(read.writable, vec![temp.to_path_buf()]);
        assert!(
            !sandbox(&settings, Access::NoTools, work, temp)
                .unwrap()
                .network
        );
    }

    #[test]
    fn extra_writable_paths_may_not_climb_out() {
        let mut settings = Settings::default();
        settings.sandbox.extra_writable = vec!["../../state".to_owned()];
        let refused = sandbox(&settings, Access::Build, Path::new("/w"), Path::new("/t"));
        assert!(matches!(refused, Err(EngineError::Refused(_))));
    }
}
