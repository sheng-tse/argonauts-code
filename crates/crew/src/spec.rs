//! What every session gets from the settings: its worker's model, the effort, the sandbox, the
//! limits, compaction, native fan-out, the goal hash, and its status file.

use std::path::{Path, PathBuf};

use serde_json::Value;

use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::names::{ModelRef, RoleName, SessionKey};
use argonauts_core::session_runner::{
    Access, RunnerKind, SandboxMode, SandboxProfile, SessionLimits, SessionShell, SessionSpec,
};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::settings::sessions::{FanoutSwitch, NativeFanout, SessionLifetime};
use argonauts_core::settings::shell::RoleShell;
use argonauts_core::steps::StepName;
use argonauts_core::{RunId, TaskId};

use crate::error::{EngineError, EngineResult};

/// Everything a spec is built from besides the settings.
#[derive(Debug, Clone)]
pub struct SpecInput {
    pub run: RunId,
    pub key: SessionKey,
    pub role: RoleName,
    pub worker: ModelRef,
    pub kind: RunnerKind,
    pub task: Option<TaskId>,
    pub step: Option<StepName>,
    pub access: Access,
    pub workdir: PathBuf,
    pub session_dir: PathBuf,
    pub prompt: Vec<InputPart>,
    pub json_schema: Option<Value>,
    /// The CLI session to resume.
    pub resume: Option<String>,
    pub lifetime: SessionLifetime,
    pub limits: SessionLimits,
    /// Required for `Build` and `Experiment`; the worker refuses them without it.
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
    /// It reads a change to judge it, whatever its role, so by default it may run the checks.
    pub reviews: bool,
}

pub fn build(settings: &Settings, input: SpecInput) -> EngineResult<SessionSpec> {
    let builds = matches!(input.access, Access::Build | Access::Experiment);
    if builds && input.goal_sha256.is_none() {
        return Err(EngineError::Refused(
            "a build or experiment session needs the approved goal's hash".to_owned(),
        ));
    }
    let temp = input.session_dir.join("tmp");
    let mut sandbox = sandbox(settings, input.access, &input.workdir, &temp)?;
    if input.kind == RunnerKind::Endpoint {
        // An endpoint runs no commands, so there is nothing to sandbox.
        sandbox.mode = SandboxMode::None;
    }
    sandbox.readable.extend(input.status_file.iter().cloned());
    sandbox.deny_read.extend(input.deny_read.iter().cloned());
    let role = settings.roles.get(&input.role);
    let own_workflows = may_run_own_workflows(settings, &input);
    let shell = reader_shell(settings, input.role, input.access, input.reviews);
    let native_fanout = if input.blind {
        NativeFanout::Switch(FanoutSwitch::Off)
    } else if input.own_workflow || (own_workflows && builds) {
        // A workflow of its own starts agents of its own, so no cap of ours fits it.
        NativeFanout::Switch(FanoutSwitch::On)
    } else {
        role.and_then(|role| role.native_fanout)
            .unwrap_or(settings.native_fanout)
    };
    Ok(SessionSpec {
        run: Some(input.run),
        task: input.task,
        key: input.key,
        role: input.role,
        step: input.step,
        effort: effort(settings, input.role, &input.worker),
        model: with_model(settings, &input.worker),
        access: input.access,
        workdir: input.workdir,
        session_dir: input.session_dir,
        // A session carried on was told when it began.
        prompt: separated(match input.resume {
            Some(_) => input.prompt,
            None => with_shell_rules(input.prompt, &shell),
        }),
        json_schema: input.json_schema,
        resume: input.resume,
        fork: false,
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
        cli_workflow: argonauts_core::session_runner::CliWorkflow {
            background_off: !own_workflows,
            ..argonauts_core::session_runner::CliWorkflow::default()
        },
        shell,
    })
}

/// What a read-only session may run: the `checks.run` commands for a reviewer, a judge, or any
/// session that reads a change to judge it, so it can run the tests it reads about, unless
/// `[roles.<role>] shell` says otherwise.
fn reader_shell(
    settings: &Settings,
    role: RoleName,
    access: Access,
    reviews: bool,
) -> SessionShell {
    if access != Access::ReadOnly {
        return SessionShell::None;
    }
    let shell = settings
        .roles
        .get(&role)
        .and_then(|role| role.shell)
        .unwrap_or_else(|| {
            if reviews {
                RoleShell::Checks
            } else {
                RoleShell::default_for(role)
            }
        });
    match shell {
        RoleShell::None => SessionShell::None,
        RoleShell::Full => SessionShell::Any,
        RoleShell::Checks => {
            let commands: Vec<String> = settings
                .checks
                .run
                .iter()
                .map(|check| command_line(&check.command))
                .filter(|line| !line.is_empty())
                .collect();
            if commands.is_empty() {
                SessionShell::None
            } else {
                SessionShell::Only(commands)
            }
        }
    }
}

/// A session held to a few commands is told which, right after what it is told to do, so it
/// never spends a call on one its CLI will refuse.
fn with_shell_rules(mut parts: Vec<InputPart>, shell: &SessionShell) -> Vec<InputPart> {
    let SessionShell::Only(commands) = shell else {
        return parts;
    };
    if parts.iter().any(|part| part.label == SHELL_RULES) {
        return parts;
    }
    let listed: Vec<String> = commands
        .iter()
        .map(|command| format!("- `{command}`"))
        .collect();
    let text = format!(
        "Your shell runs only these commands, each on its own, with any arguments after it:\n\
         {}\n\n\
         Anything else is refused: another command, two joined with `&&`, `;` or `|`, a \
         redirect, or an environment variable set in front. Read files with your read tools, \
         not the shell. The checks' results, when they ran, are in your input, so run a check \
         again only to see something they don't show.",
        listed.join("\n")
    );
    let at = parts
        .iter()
        .take_while(|part| part.kind.instructs() || part.kind == InputPartKind::Rules)
        .count();
    parts.insert(
        at,
        InputPart {
            kind: InputPartKind::Rules,
            label: SHELL_RULES.to_owned(),
            approx_tokens: u32::try_from(text.len() / 4).unwrap_or(u32::MAX),
            text,
            source: None,
        },
    );
    parts
}

const SHELL_RULES: &str = "your shell";

/// A command as it is typed, an argument with a space or a quote in single quotes.
fn command_line(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| {
            if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c))
            {
                format!("'{}'", arg.replace('\'', "'\\''"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A crew agent starts workflows and background agents of its CLI's own only when you allow
/// it: every worker with `crew.worker_workflows`, or one step with `own_workflow`. Otherwise
/// a crew would run inside one agent, where nothing can watch or merge it, and the agent's
/// answer would come before that crew's.
fn may_run_own_workflows(settings: &Settings, input: &SpecInput) -> bool {
    !input.blind && (input.own_workflow || settings.crew.worker_workflows)
}

/// The parts go out concatenated, so each but the last ends in a blank line. Otherwise a task's
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

/// `roles.<role>.effort`, else `agents.<cli>.effort`, else unset.
pub fn effort(settings: &Settings, role: RoleName, worker: &ModelRef) -> Option<String> {
    settings
        .roles
        .get(&role)
        .and_then(|role| role.effort.clone())
        .or_else(|| {
            settings
                .agents
                .get(&worker.agent)
                .and_then(|agent| agent.effort.clone())
        })
}

/// The worker with its model filled in: its own reference's model, else
/// `agents.<cli>.model`.
pub fn with_model(settings: &Settings, worker: &ModelRef) -> ModelRef {
    if worker.model.is_some() {
        return worker.clone();
    }
    let model = settings
        .agents
        .get(&worker.agent)
        .and_then(|agent| agent.model.clone());
    ModelRef::new(worker.agent.clone(), model)
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
        // Your own safety key; the hard limits flag it at every approval.
        SandboxChoice::Tool | SandboxChoice::None => SandboxMode::None,
    };
    let mut writable = vec![temp.to_path_buf()];
    if matches!(access, Access::Build | Access::Experiment) {
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
        let worker = RoleSettings {
            effort: Some("high".to_owned()),
            ..RoleSettings::default()
        };
        settings.roles.insert(RoleName::Worker, worker);
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
    fn effort_is_the_roles_then_the_clis_then_unset() {
        let settings = settings();
        let codex = ModelRef::new("codex", None);
        let claude = ModelRef::new("claude-code", None);
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
    fn the_model_is_the_references_own_else_the_clis() {
        let settings = settings();
        let own = ModelRef::new("codex", Some("gpt-y".to_owned()));
        assert_eq!(with_model(&settings, &own), own);
        assert_eq!(
            with_model(&settings, &ModelRef::new("codex", None))
                .model
                .as_deref(),
            Some("gpt-x")
        );
        assert_eq!(
            with_model(&settings, &ModelRef::new("agy", None)).model,
            None
        );
    }

    #[test]
    fn a_blind_session_is_isolated_and_never_fans_out() {
        let input = |blind: bool| SpecInput {
            run: RunId::new("run-01").unwrap(),
            key: SessionKey::parse("s-001").unwrap(),
            role: RoleName::Reviewer,
            worker: ModelRef::new("claude-code", None),
            kind: RunnerKind::Cli,
            task: None,
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
            reviews: false,
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
        assert!(!free.cli_workflow.background_off);
        assert!(still_blind.cli_workflow.background_off);
    }

    #[test]
    fn reviewers_get_a_shell_for_the_checks_only() {
        let mut settings: Settings = serde_json::from_value(serde_json::json!({"checks": {"run": [
            {"name": "tests", "command": ["node", "--test"]},
            {"name": "lint", "command": ["sh", "-c", "npm run lint"]},
        ]}}))
        .unwrap();
        let only = SessionShell::Only(vec![
            "node --test".to_owned(),
            "sh -c 'npm run lint'".to_owned(),
        ]);
        assert_eq!(
            reader_shell(&settings, RoleName::Reviewer, Access::ReadOnly, false),
            only
        );
        assert_eq!(
            reader_shell(&settings, RoleName::Judge, Access::ReadOnly, false),
            only
        );
        assert_eq!(
            reader_shell(&settings, RoleName::Orchestrator, Access::ReadOnly, false),
            SessionShell::None
        );
        assert_eq!(
            reader_shell(&settings, RoleName::Reviewer, Access::Build, false),
            SessionShell::None,
            "a session that builds keeps its CLI's own shell"
        );
        settings.roles.entry(RoleName::Reviewer).or_default().shell = Some(RoleShell::Full);
        assert_eq!(
            reader_shell(&settings, RoleName::Reviewer, Access::ReadOnly, false),
            SessionShell::Any
        );
        settings.roles.entry(RoleName::Judge).or_default().shell = Some(RoleShell::None);
        assert_eq!(
            reader_shell(&settings, RoleName::Judge, Access::ReadOnly, false),
            SessionShell::None
        );
        settings.checks.run.clear();
        assert_eq!(
            reader_shell(
                &Settings::default(),
                RoleName::Reviewer,
                Access::ReadOnly,
                false
            ),
            SessionShell::None,
            "no checks, no shell"
        );
    }

    #[test]
    fn a_reviewer_held_to_the_checks_is_told_which_commands_run() {
        let rules = |label: &str, kind: InputPartKind| InputPart {
            kind,
            label: label.to_owned(),
            text: "x".to_owned(),
            approx_tokens: 1,
            source: None,
        };
        let parts = vec![
            rules("Frame", InputPartKind::Frame),
            rules("Instructions", InputPartKind::Instructions),
            rules("decide, don't ask", InputPartKind::Rules),
            rules("the change", InputPartKind::Evidence),
        ];
        let shell = SessionShell::Only(vec!["python3 -m unittest -q".to_owned()]);
        let told = with_shell_rules(parts.clone(), &shell);
        assert_eq!(told.len(), 5);
        assert_eq!(told[3].label, "your shell");
        assert_eq!(told[3].kind, InputPartKind::Rules);
        assert!(told[3].text.contains("- `python3 -m unittest -q`"));
        assert!(told[3].text.contains("`&&`, `;` or `|`"));
        assert_eq!(with_shell_rules(told.clone(), &shell), told, "told once");
        assert_eq!(with_shell_rules(parts.clone(), &SessionShell::Any), parts);
        assert_eq!(with_shell_rules(parts.clone(), &SessionShell::None), parts);
    }

    /// A workflow's step may give a review to a worker; reading a change to judge it is
    /// what earns the checks' shell, whatever the role is called.
    #[test]
    fn a_session_that_reviews_a_change_gets_the_checks_whatever_its_role() {
        let settings: Settings = serde_json::from_value(serde_json::json!({"checks": {"run": [
            {"name": "tests", "command": ["node", "--test"]},
        ]}}))
        .unwrap();
        assert_eq!(
            reader_shell(&settings, RoleName::Worker, Access::ReadOnly, true),
            SessionShell::Only(vec!["node --test".to_owned()])
        );
        assert_eq!(
            reader_shell(&settings, RoleName::Worker, Access::ReadOnly, false),
            SessionShell::None
        );
    }

    #[test]
    fn crew_agents_start_no_workflow_of_their_own_unless_allowed() {
        let input = |role: RoleName, access: Access| SpecInput {
            run: RunId::new("run-01").unwrap(),
            key: SessionKey::parse("s-001").unwrap(),
            role,
            worker: ModelRef::new("claude-code", None),
            kind: RunnerKind::Cli,
            task: Some(TaskId::new("task-a").unwrap()),
            step: None,
            access,
            workdir: PathBuf::from("/w"),
            session_dir: PathBuf::from("/s"),
            prompt: Vec::new(),
            json_schema: None,
            resume: None,
            lifetime: SessionLifetime::Task,
            limits: SessionLimits::default(),
            goal_sha256: Some("ab".repeat(32)),
            status_file: None,
            blind: false,
            own_workflow: false,
            autopilot: false,
            deny_read: Vec::new(),
            reviews: false,
        };
        let mut settings = Settings {
            ultracode: true,
            ..Settings::default()
        };
        for (role, access) in [
            (RoleName::Worker, Access::Build),
            (RoleName::Reviewer, Access::ReadOnly),
            (RoleName::Judge, Access::ReadOnly),
        ] {
            let spec = build(&settings, input(role, access)).unwrap();
            assert!(spec.cli_workflow.background_off, "{role:?}");
            assert!(
                !spec.cli_workflow.ultracode,
                "ultracode stays with the main session"
            );
            assert_ne!(spec.native_fanout, NativeFanout::Switch(FanoutSwitch::On));
        }
        settings.crew.worker_workflows = true;
        let worker = build(&settings, input(RoleName::Worker, Access::Build)).unwrap();
        assert!(!worker.cli_workflow.background_off);
        assert_eq!(worker.native_fanout, NativeFanout::Switch(FanoutSwitch::On));
        assert!(!worker.cli_workflow.ultracode);
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
