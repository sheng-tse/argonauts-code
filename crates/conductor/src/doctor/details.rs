//! What `argonauts doctor` reports beyond versions, kept apart from the frozen `DoctorReport`
//! so its readers keep compiling: per CLI the login state, flag probes, effective
//! compaction and fan-out per role, the user-hooks effect, whether the status tool works per
//! run, and the plan usage probe; then the sandbox self-test and any always-on agents. It never
//! makes a model call.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::agents::adapter::Transport;
use argonauts_core::agents::session_runner::SandboxMode;
use argonauts_core::models::limits::PlanUsageReading;
use argonauts_core::settings::Settings;
use argonauts_core::settings::agents::AgentSettings;
use argonauts_core::settings::approvals::UserHooks;
use argonauts_core::{Adapter, AuthMode, TweakEffect};
use serde::Serialize;

use super::always_on::{AlwaysOnAgent, always_on_agents};
use super::command::ask;
use super::roles::{RoleCheck, role_checks};
use super::versions::{FlagProbe, VersionFit, probe_flags, tested_range_label, version_fit};
use super::{VERSION_TIMEOUT, read_version, resolve_binary};
use crate::boards::plan_usage::{PROBE_TIMEOUT, Prober, run_probe};
use crate::cli_runner::capabilities::ANTIGRAVITY;
use crate::sandbox;
use crate::sandbox::policy::user_hooks;
use crate::sandbox::selftest::self_test_once;

pub const STATUS_TOOL_PER_RUN: &str = "status tool: per run";
pub const STATUS_TOOL_NOT_PER_RUN: &str = "status tool: not available per run";
/// agy's `--project` adds the project's settings to yours but keeps one store for every
/// conversation, so runs still see each other's.
pub const AGY_RUN_PROJECT: &str =
    "each run in an agy project of its own (--project <run>), sharing agy's one store";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DoctorDetails {
    pub clis: Vec<CliDetails>,
    pub sandbox: SandboxCheck,
    pub always_on: Vec<AlwaysOnAgent>,
    /// Each shown after a `!`, such as a long-lived role whose compaction is off.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CliDetails {
    pub id: String,
    /// The binary doctor ran: `agents.<id>.binary` when set, else the adapter's on `PATH`.
    pub binary: Option<PathBuf>,
    /// Why there's no binary to run, or why its version couldn't be read.
    pub problem: Option<String>,
    pub version: Option<String>,
    /// Whether the installed version is inside the tested range.
    pub in_tested_range: Option<bool>,
    /// A later patch of the newest version recorded, which should work.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub newer: bool,
    /// Such as `2.1.284 up to 2.2`.
    pub tested_range: Option<String>,
    /// Flags the adapter needs, found or missing in `--help`.
    pub flags: FlagProbe,
    /// From the adapter's verified login check; `None` where there is none.
    pub logged_in: Option<bool>,
    /// Why the login state is unknown, when it is.
    pub login_detail: Option<String>,
    pub roles: Vec<RoleCheck>,
    pub user_hooks_setting: UserHooks,
    pub user_hooks: TweakEffect,
    pub status_tool_per_run: bool,
    /// [`STATUS_TOOL_PER_RUN`] or [`STATUS_TOOL_NOT_PER_RUN`].
    pub status_tool: String,
    /// How the CLI keeps runs apart, where Argonauts Code passes it a project: [`AGY_RUN_PROJECT`].
    pub project: Option<String>,
    /// Codex: whether `codex app-server` answered the rate-limit read, and why not.
    pub plan_usage_probe: Option<ProbeCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeCheck {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SandboxCheck {
    pub ok: bool,
    pub detail: String,
    pub mode: SandboxMode,
}

pub async fn doctor_details(adapters: &[Arc<dyn Adapter>], settings: &Settings) -> DoctorDetails {
    let clis = futures::future::join_all(adapters.iter().map(|a| cli_details(a, settings)));
    let wrap = sandbox::for_settings(&settings.sandbox);
    let (clis, test, always_on) =
        tokio::join!(clis, self_test_once(wrap.as_ref()), always_on_agents());
    let sandbox = SandboxCheck {
        ok: test.ok,
        detail: test.detail,
        mode: wrap.mode(),
    };

    let mut warnings = Vec::new();
    let mut checked = Vec::with_capacity(clis.len());
    for (details, cli_warnings) in clis {
        warnings.extend(cli_warnings);
        checked.push(details);
    }
    if !sandbox.ok {
        warnings.push(if settings.sandbox.fail_closed {
            format!(
                "The sandbox self-test failed, so CLI sessions are refused: {}",
                sandbox.detail
            )
        } else {
            format!(
                "The sandbox self-test failed, and sandbox.fail_closed is off, so sessions run \
                 without a working sandbox: {}",
                sandbox.detail
            )
        });
    }
    if !always_on.is_empty() {
        let names: Vec<&str> = always_on.iter().map(|a| a.name.as_str()).collect();
        warnings.push(format!(
            "Always-on agents are running: {}. They run as you, outside Argonauts Code's sandbox, and \
             could reach your approval commands. Run them as another OS user or on another \
             machine.",
            names.join(", ")
        ));
    }
    DoctorDetails {
        clis: checked,
        sandbox,
        always_on,
        warnings,
    }
}

/// One CLI's details, and the warnings they raise.
async fn cli_details(adapter: &Arc<dyn Adapter>, settings: &Settings) -> (CliDetails, Vec<String>) {
    let id = adapter.id();
    let agent = settings.agents.get(id);
    let status_tool_per_run = id == "claude-code" || adapter.transport() == Transport::Acp;
    let roles = role_checks(adapter.as_ref(), settings);
    let mut warnings: Vec<String> = roles.iter().filter_map(|r| r.warning.clone()).collect();
    let mut details = CliDetails {
        id: id.to_owned(),
        binary: None,
        problem: None,
        version: None,
        in_tested_range: None,
        newer: false,
        tested_range: tested_range_label(adapter.tested_versions()),
        flags: FlagProbe::default(),
        logged_in: None,
        login_detail: None,
        roles,
        user_hooks_setting: settings.sandbox.user_hooks,
        user_hooks: user_hooks(adapter.as_ref(), &settings.sandbox).effect,
        status_tool_per_run,
        status_tool: if status_tool_per_run {
            STATUS_TOOL_PER_RUN
        } else {
            STATUS_TOOL_NOT_PER_RUN
        }
        .to_owned(),
        project: (id == ANTIGRAVITY).then(|| AGY_RUN_PROJECT.to_owned()),
        plan_usage_probe: None,
    };
    let path = match resolve_binary(adapter.as_ref(), agent.and_then(|a| a.binary.as_deref())) {
        Ok(path) => path,
        Err(problem) => {
            if id == "codex" {
                details.plan_usage_probe = Some(ProbeCheck {
                    ok: false,
                    detail: problem.clone(),
                });
            }
            details.problem = Some(problem);
            return (details, warnings);
        }
    };

    let (version, flags, (logged_in, login_detail)) = tokio::join!(
        read_version(&path, adapter.version_args(), VERSION_TIMEOUT),
        probe_flags(&path, id),
        login_state(adapter.as_ref(), agent, &path)
    );
    match version.map(|output| adapter.parse_version(&output)) {
        Ok(Some(version)) => {
            let fit = version_fit(&version, adapter.tested_versions());
            details.in_tested_range = Some(fit != VersionFit::Untested);
            details.newer = fit == VersionFit::Newer;
            if let Some(pin) = agent.and_then(|a| a.version_pin.as_deref())
                && pin != version
            {
                warnings.push(format!(
                    "{id} is {version}, but agents.{id}.version_pin is {pin}."
                ));
            }
            details.version = Some(version);
        }
        Ok(None) => details.problem = Some("could not find a version in its output".to_owned()),
        Err(problem) => details.problem = Some(problem),
    }
    if !flags.missing.is_empty() {
        warnings.push(format!(
            "{id} --help lacks flags Argonauts Code passes: {}. Update {id}, or expect its sessions \
             to fail.",
            flags.missing.join(", ")
        ));
    }
    if logged_in == Some(false) {
        let hint = adapter.setup_hints().login.unwrap_or_default();
        warnings.push(
            format!("{id} is not logged in. {hint}")
                .trim_end()
                .to_owned(),
        );
    }
    details.flags = flags;
    details.logged_in = logged_in;
    details.login_detail = login_detail;
    if id == "codex" && agent.is_none_or(|a| a.enabled) {
        details.plan_usage_probe = Some(codex_plan_usage_probe(adapter, &path).await);
    }
    details.binary = Some(path);
    (details, warnings)
}

/// Runs the adapter's verified login check, where it has one: exit 0 means logged in.
async fn login_state(
    adapter: &dyn Adapter,
    agent: Option<&AgentSettings>,
    binary: &Path,
) -> (Option<bool>, Option<String>) {
    if agent.is_some_and(|a| !a.enabled) {
        return (None, Some("off, so its login isn't checked".to_owned()));
    }
    if agent.is_some_and(|a| a.auth == AuthMode::ApiKey) {
        return (
            None,
            Some("uses an API key, so there's no login".to_owned()),
        );
    }
    let Some(check) = adapter.setup_hints().login_check else {
        return (None, Some("no verified login check".to_owned()));
    };
    let args: Vec<&str> = check.iter().map(String::as_str).collect();
    match ask(binary, &args, VERSION_TIMEOUT).await {
        Ok(answer) => (Some(answer.success), None),
        Err(problem) => (None, Some(problem)),
    }
}

/// Codex's rate-limit read over `codex app-server`, which spends nothing.
async fn codex_plan_usage_probe(adapter: &Arc<dyn Adapter>, binary: &Path) -> ProbeCheck {
    let workdir = std::env::temp_dir();
    if adapter.plan_usage_probe(&workdir, None).is_none() {
        return ProbeCheck {
            ok: false,
            detail: "this Codex adapter has no plan usage probe".to_owned(),
        };
    }
    let prober = Prober {
        adapter: Arc::clone(adapter),
        binary: binary.to_path_buf(),
        workdir,
        model: None,
    };
    match run_probe(&prober).await {
        Some(reading) => ProbeCheck {
            ok: true,
            detail: format!("reached codex app-server: {}", describe(&reading)),
        },
        None => ProbeCheck {
            ok: false,
            detail: format!(
                "codex app-server gave no rate-limit reading within {}s. It may have failed to \
                 start, you may be logged out, or its protocol may have changed",
                PROBE_TIMEOUT.as_secs()
            ),
        },
    }
}

fn describe(reading: &PlanUsageReading) -> String {
    let status = serde_json::to_value(reading.status)
        .ok()
        .and_then(|value| value.as_str().map(|s| s.replace('_', " ")))
        .unwrap_or_else(|| "unknown".to_owned());
    match (reading.used, &reading.window) {
        (Some(used), Some(window)) => {
            format!("{status}, {:.0}% of the {window} window used", used * 100.0)
        }
        (Some(used), None) => format!("{status}, {:.0}% used", used * 100.0),
        _ => status,
    }
}

fn effect_note(effect: &TweakEffect) -> Option<String> {
    match effect {
        TweakEffect::Exact => None,
        TweakEffect::Approximated { note } => Some(format!("approximated: {note}")),
        TweakEffect::NotPossible { note } => Some(format!("not possible: {note}")),
        TweakEffect::EnforcedByConductor => Some("enforced by Argonauts Code".to_owned()),
    }
}

impl fmt::Display for CliDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.binary {
            Some(binary) => writeln!(f, "{}  {}", self.id, binary.display())?,
            None => writeln!(f, "{}  not found", self.id)?,
        }
        if let Some(problem) = &self.problem {
            writeln!(f, "  problem      {problem}")?;
        }
        if let Some(version) = &self.version {
            let range = self.tested_range.as_deref().unwrap_or("none recorded");
            let judged = match (self.in_tested_range, self.newer) {
                (Some(true), true) => "newer than tested, should work",
                (Some(true), false) => "in the tested range",
                _ => "outside the tested range",
            };
            writeln!(f, "  version      {version}, {judged} ({range})")?;
        }
        if let Some(problem) = &self.flags.problem {
            writeln!(f, "  flags        could not read --help: {problem}")?;
        } else if !self.flags.missing.is_empty() {
            writeln!(
                f,
                "  flags        missing {}",
                self.flags.missing.join(", ")
            )?;
        } else if !self.flags.found.is_empty() {
            writeln!(
                f,
                "  flags        all {} found in --help",
                self.flags.found.len()
            )?;
        }
        match (self.logged_in, &self.login_detail) {
            (Some(true), _) => writeln!(f, "  login        logged in")?,
            (Some(false), _) => writeln!(f, "  login        not logged in")?,
            (None, Some(detail)) => writeln!(f, "  login        unknown, {detail}")?,
            (None, None) => {}
        }
        writeln!(f, "  {}", self.status_tool)?;
        if let Some(project) = &self.project {
            writeln!(f, "  project      {project}")?;
        }
        let hooks = match (self.user_hooks_setting, effect_note(&self.user_hooks)) {
            (UserHooks::Inherit, _) => "your hooks run".to_owned(),
            (UserHooks::Disable, None) => "off for Argonauts Code sessions".to_owned(),
            (UserHooks::Disable, Some(note)) => format!("still run, off is {note}"),
        };
        writeln!(f, "  user hooks   {hooks}")?;
        if let Some(probe) = &self.plan_usage_probe {
            let mark = if probe.ok { "ok" } else { "failed" };
            writeln!(f, "  plan usage probe  {mark}, {}", probe.detail)?;
        }
        if !self.roles.is_empty() {
            writeln!(f, "  compaction and fan-out, per role")?;
            let width = self
                .roles
                .iter()
                .map(|r| r.role.as_str().len())
                .max()
                .unwrap_or(0);
            for role in &self.roles {
                let mark = if role.warning.is_some() { "!" } else { " " };
                write!(
                    f,
                    "  {mark} {:width$}  {}  fan-out {}",
                    role.role.as_str(),
                    role.compaction,
                    role.fanout
                )?;
                if let Some(note) = effect_note(&role.fanout_effect) {
                    write!(f, " ({note})")?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

impl fmt::Display for DoctorDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for cli in &self.clis {
            write!(f, "{cli}")?;
        }
        let verdict = if self.sandbox.ok { "ok" } else { "failed" };
        writeln!(f, "sandbox  {verdict}, {}", self.sandbox.detail)?;
        if self.always_on.is_empty() {
            writeln!(f, "always-on agents  none")?;
        } else {
            for agent in &self.always_on {
                writeln!(f, "always-on agent  {}, {}", agent.name, agent.evidence)?;
            }
        }
        for warning in &self.warnings {
            writeln!(f, "! {warning}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::tests::FakeCli;
    use argonauts_core::base::names::{ModelRef, RoleName};
    use argonauts_core::settings::roles::RoleSettings;
    use argonauts_core::settings::sessions::{Compaction, SessionLifetime};

    #[tokio::test]
    async fn agy_status_tool_unavailable_reported() {
        let adapters: Vec<Arc<dyn Adapter>> = vec![
            Arc::new(FakeCli::named("antigravity-cli")),
            Arc::new(FakeCli::named("claude-code")),
            Arc::new(FakeCli::named("some-acp-agent").acp()),
        ];
        let details = doctor_details(&adapters, &Settings::default()).await;
        let status: Vec<(&str, &str)> = details
            .clis
            .iter()
            .map(|h| (h.id.as_str(), h.status_tool.as_str()))
            .collect();
        assert_eq!(
            status,
            [
                ("antigravity-cli", "status tool: not available per run"),
                ("claude-code", "status tool: per run"),
                ("some-acp-agent", "status tool: per run"),
            ]
        );
        assert!(!details.clis[0].status_tool_per_run);
        let projects: Vec<Option<&str>> =
            details.clis.iter().map(|h| h.project.as_deref()).collect();
        assert_eq!(projects, [Some(AGY_RUN_PROJECT), None, None]);
        let text = details.to_string();
        assert!(text.contains("antigravity-cli  not found\n"), "{text}");
        assert!(
            text.contains(&format!("  project      {AGY_RUN_PROJECT}\n")),
            "{text}"
        );
        assert!(
            text.contains("  status tool: not available per run\n"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn details_for_an_installed_cli() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"case "$*" in
  --version) echo "codex-cli 0.158.2" ;;
  "login status") echo "Logged in"; exit 0 ;;
  *) echo "  --json  --model  --sandbox" ;;
esac"#;
        let codex = FakeCli::script("codex", dir.path(), script, &["0.158.0"])
            .with_login_check(&["login", "status"]);
        let mut settings = Settings::default();
        settings.roles.insert(
            RoleName::Consultant,
            RoleSettings {
                models: vec![ModelRef::new("codex", None)],
                lifetime: Some(SessionLifetime::Project),
                compaction: Some(Compaction::Off),
                ..RoleSettings::default()
            },
        );
        let adapters: Vec<Arc<dyn Adapter>> = vec![Arc::new(codex)];
        let details = doctor_details(&adapters, &settings).await;
        let codex = &details.clis[0];
        assert_eq!(codex.version.as_deref(), Some("0.158.2"));
        assert_eq!(codex.in_tested_range, Some(true));
        assert_eq!(codex.logged_in, Some(true));
        assert!(codex.flags.missing.contains(&"--output-schema".to_owned()));
        assert!(codex.flags.found.contains(&"--json".to_owned()));
        assert!(
            details
                .warnings
                .iter()
                .any(|w| w.contains("--output-schema")),
            "{:?}",
            details.warnings
        );
        let probe = codex.plan_usage_probe.as_ref().unwrap();
        assert!(!probe.ok);
        assert_eq!(probe.detail, "this Codex adapter has no plan usage probe");
        // The fake's default compaction can't turn off, so the consultant isn't warned about.
        let consultant = codex
            .roles
            .iter()
            .find(|r| r.role == RoleName::Consultant)
            .unwrap();
        assert!(!consultant.compaction_off);
        assert!(consultant.compaction.starts_with("off →  not possible"));
        assert!(details.to_string().contains("  plan usage probe  failed, "));
    }

    #[tokio::test]
    async fn an_agent_thats_off_is_never_asked_for_its_login() {
        let dir = tempfile::tempdir().unwrap();
        let asked = dir.path().join("asked");
        let script = format!(
            r#"case "$*" in
  --version) echo "codex-cli 0.158.2" ;;
  "login status") touch {}; exit 0 ;;
  *) echo "  --json" ;;
esac"#,
            asked.display()
        );
        let codex = FakeCli::script("codex", dir.path(), &script, &["0.158.0"])
            .with_login_check(&["login", "status"]);
        let mut settings = Settings::default();
        settings.agents.insert(
            "codex".into(),
            AgentSettings {
                enabled: false,
                ..AgentSettings::default()
            },
        );
        let adapters: Vec<Arc<dyn Adapter>> = vec![Arc::new(codex)];
        let details = doctor_details(&adapters, &settings).await;
        let codex = &details.clis[0];
        assert_eq!(codex.version.as_deref(), Some("0.158.2"));
        assert_eq!(codex.logged_in, None);
        assert_eq!(
            codex.login_detail.as_deref(),
            Some("off, so its login isn't checked")
        );
        assert!(codex.plan_usage_probe.is_none());
        assert!(!asked.exists());

        let report = crate::doctor::doctor(&adapters, &settings).await;
        assert!(!report.agents[0].enabled);
        assert!(!report.agents[0].usable());
    }

    #[tokio::test]
    async fn doctor_warns_about_long_lived_roles_without_compaction() {
        let claude = FakeCli::named("claude-code").with_compaction(|_| Default::default());
        let mut settings = Settings::default();
        settings.roles.insert(
            RoleName::Consultant,
            RoleSettings {
                lifetime: Some(SessionLifetime::Project),
                compaction: Some(Compaction::Off),
                ..RoleSettings::default()
            },
        );
        let adapters: Vec<Arc<dyn Adapter>> = vec![Arc::new(claude)];
        let details = doctor_details(&adapters, &settings).await;
        let warning = "consultant keeps one session per project (lifetime project), but its compaction is off on claude-code.";
        assert!(
            details.warnings.iter().any(|w| w == warning),
            "{:?}",
            details.warnings
        );
        let text = details.to_string();
        assert!(text.contains(&format!("! {warning}\n")), "{text}");
        assert!(text.contains("  ! consultant"), "{text}");
    }
}
