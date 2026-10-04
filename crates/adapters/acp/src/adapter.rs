//! The adapter for a `[[harness]]` entry with `transport = "acp"`: the launch command, with its
//! per-session home, and the session driver that reads the pipes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use argonauts_core::adapter::Transport;
use argonauts_core::driver::SessionDriver;
use argonauts_core::layout::session_files::HOME;
use argonauts_core::layout::user_files::HARNESSES;
use argonauts_core::paths::Paths;
use argonauts_core::registry::HarnessManifest;
use argonauts_core::registry::acp::{AcpProfile, SteerRoute, UsageSource};
use argonauts_core::registry::harness::HomeKeep;
use argonauts_core::{
    Adapter, AgentEvent, Capabilities, CommandSpec, SessionRequest, SessionRole, StopPlan,
    StreamParser, env_remove_except, extract_version,
};

use crate::driver::{AcpDriver, DriverProfile};
use crate::errors::ErrorRules;
use crate::hermes;
use crate::home::home_files;

/// The adapter for a `[[harness]]` entry with `transport = "acp"`.
pub fn acp_adapter(manifest: HarnessManifest) -> Arc<dyn Adapter> {
    Arc::new(AcpAdapter::new(manifest, None))
}

/// `Adapter` hands out `&'static str`s but a profile is read at run time, so its names are
/// leaked once. Profiles are loaded once per process.
pub(crate) struct AcpAdapter {
    id: &'static str,
    display_name: &'static str,
    binary: &'static str,
    family: &'static str,
    version_args: &'static [&'static str],
    tested_versions: &'static [&'static str],
    manifest: HarnessManifest,
    profile: Arc<DriverProfile>,
    /// Where home templates live. `None` reads `ARGONAUTS_HOME` at launch.
    argonauts_home: Option<PathBuf>,
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_owned().into_boxed_str())
}

fn leak_all(texts: &[String]) -> &'static [&'static str] {
    let leaked: Vec<&'static str> = texts.iter().map(|text| leak(text)).collect();
    Box::leak(leaked.into_boxed_slice())
}

impl AcpAdapter {
    pub(crate) fn new(manifest: HarnessManifest, argonauts_home: Option<PathBuf>) -> Self {
        let profile = DriverProfile::new(
            manifest.acp.clone().unwrap_or_default(),
            ErrorRules::compile(&manifest.errors),
        );
        Self {
            id: leak(&manifest.id),
            display_name: leak(&manifest.display_name),
            binary: leak(&manifest.binary),
            family: leak(&manifest.family),
            version_args: leak_all(&manifest.version_args),
            tested_versions: leak_all(&manifest.tested_versions),
            profile: Arc::new(profile),
            manifest,
            argonauts_home,
        }
    }

    fn acp(&self) -> &AcpProfile {
        &self.profile.acp
    }

    fn templates_home(&self) -> anyhow::Result<PathBuf> {
        match &self.argonauts_home {
            Some(home) => Ok(home.clone()),
            None => Ok(Paths::from_env()?.home().to_owned()),
        }
    }
}

impl Adapter for AcpAdapter {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.display_name
    }

    fn binary(&self) -> &'static str {
        self.binary
    }

    fn model_base_url(&self) -> Option<String> {
        let template = self.manifest.home.as_ref()?;
        if template.env != hermes::HOME_ENV {
            return None;
        }
        let root = self.templates_home().ok()?.join(HARNESSES);
        hermes::model_base_url(&root.join(&template.template))
    }

    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        if !Path::new(self.binary).is_absolute() {
            bail!(
                "{}: the profile's binary must be an absolute path, not {:?}",
                self.id,
                self.binary
            );
        }
        if let Err(reason) = &self.profile.errors {
            bail!("{}: {reason}", self.id);
        }
        let home = match &self.manifest.home {
            Some(template) => {
                let home = match (template.keep, card_home(req)) {
                    (HomeKeep::Card, Some(card_home)) => card_home,
                    _ => {
                        let Some(scratch) = &req.scratch_dir else {
                            bail!("{}: a per-session home needs the session's folder", self.id);
                        };
                        scratch.join(HOME)
                    }
                };
                let mut files = home_files(&self.templates_home()?, &template.template, &home)
                    .with_context(|| format!("{}: making its home", self.id))?;
                if template.env == hermes::HOME_ENV {
                    hermes::tune_config(&mut files, &home, &req.workdir, req.native_fanout)
                        .with_context(|| format!("{}: making its home", self.id))?;
                }
                Some((template.env.clone(), home, files))
            }
            None => None,
        };
        let values = Placeholders {
            model: req.model.as_deref(),
            effort: req.effort.as_deref(),
            workdir: &req.workdir,
            session: req.resume_session.as_deref(),
            home: home.as_ref().map(|(_, home, _)| home.as_path()),
        };
        let template = match req.role {
            SessionRole::Build | SessionRole::Direct => &self.manifest.build_args,
            SessionRole::ReadOnly => &self.manifest.read_only_args,
        };
        let args = template
            .iter()
            .map(|arg| values.fill(arg))
            .collect::<anyhow::Result<Vec<_>>>()
            .with_context(|| format!("{}: filling its arguments", self.id))?;

        let mut spec = CommandSpec::new(self.binary, &req.workdir)
            .args(args)
            .args(req.extra_args.iter().cloned());
        // An agent that runs many providers would use any key it finds, and so would every
        // command its model runs. Only the keys the profile names reach it.
        let keep: Vec<&str> = self.manifest.env.iter().map(String::as_str).collect();
        spec.env_remove = env_remove_except(&keep);
        let mut fixed: Vec<(String, String)> = self
            .manifest
            .env_set
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if let Some((env, home, files)) = home {
            fixed.push((env, home.display().to_string()));
            spec.files = files;
        }
        // The caller's values never replace the profile's or the home's.
        spec.env_set = req
            .env_set
            .iter()
            .filter(|(name, _)| !fixed.iter().any(|(set, _)| set == name))
            .cloned()
            .collect();
        spec.env_set.extend(fixed);
        // The driver owns stdin for the whole session.
        spec.keep_stdin_open = true;
        Ok(spec)
    }

    /// Never used: the driver reads the pipes.
    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(KeepEverything)
    }

    fn version_args(&self) -> &'static [&'static str] {
        self.version_args
    }

    fn parse_version(&self, output: &str) -> Option<String> {
        extract_version(output)
    }

    fn tested_versions(&self) -> &'static [&'static str] {
        self.tested_versions
    }

    /// A profile names no model, so there is no cheapest one.
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("<model>", None)
    }

    fn stop_plan(&self) -> StopPlan {
        let default = StopPlan::default();
        let Some(waits) = &self.manifest.stop else {
            return default;
        };
        match (waits.after_interrupt.parse(), waits.after_terminate.parse()) {
            (Ok(Some(interrupt)), Ok(Some(terminate))) => {
                StopPlan::escalating(interrupt, terminate)
            }
            _ => default,
        }
    }

    /// Read-only sessions lean on the sandbox and the empty-diff check.
    fn capabilities(&self) -> Capabilities {
        let usage_updates = self.acp().usage.contains(&UsageSource::UsageUpdate);
        Capabilities {
            live_usage: usage_updates,
            reports_cost: usage_updates,
            enforces_read_only: false,
            ..Capabilities::default()
        }
    }

    /// `Some` only with `steer = "input"`, which the worker reads as mid-turn steering. The
    /// driver sends the message itself.
    fn input_line(&self, message: &str) -> Option<String> {
        (self.acp().steer == SteerRoute::Input).then(|| message.to_owned())
    }

    /// The profile's take-over arguments for its binary, with `{session}` and `{workdir}`.
    fn interactive_resume(&self, session_id: &str, workdir: &Path) -> Option<CommandSpec> {
        if self.manifest.interactive_resume.is_empty() {
            return None;
        }
        let values = Placeholders {
            model: None,
            effort: None,
            workdir,
            session: Some(session_id),
            home: None,
        };
        let args = self
            .manifest
            .interactive_resume
            .iter()
            .map(|arg| values.fill(arg))
            .collect::<anyhow::Result<Vec<_>>>()
            .ok()?;
        Some(CommandSpec::new(self.binary, workdir).args(args))
    }

    fn default_family(&self) -> &'static str {
        self.family
    }

    fn transport(&self) -> Transport {
        Transport::Acp
    }

    fn session_driver(&self) -> Option<Arc<dyn SessionDriver>> {
        Some(Arc::new(AcpDriver::new(Arc::clone(&self.profile))))
    }
}

/// The folder the worker passes for a home kept for the card, when the session is on one. A
/// later session of the card, such as a resume, gets the same folder and finds its session there.
fn card_home(req: &SessionRequest) -> Option<PathBuf> {
    let folder = Path::new(req.project.as_deref()?);
    folder.is_absolute().then(|| folder.to_path_buf())
}

/// Values for a profile's argument placeholders. An argument that names one this session has
/// no value for is refused rather than passed on half filled.
struct Placeholders<'a> {
    model: Option<&'a str>,
    effort: Option<&'a str>,
    workdir: &'a Path,
    session: Option<&'a str>,
    home: Option<&'a Path>,
}

impl Placeholders<'_> {
    fn fill(&self, arg: &str) -> anyhow::Result<String> {
        let workdir = self.workdir.display().to_string();
        let home = self.home.map(|home| home.display().to_string());
        let values = [
            ("{model}", self.model),
            ("{effort}", self.effort),
            ("{workdir}", Some(workdir.as_str())),
            ("{session}", self.session),
            ("{home}", home.as_deref()),
        ];
        let mut filled = arg.to_owned();
        for (name, value) in values {
            if !filled.contains(name) {
                continue;
            }
            let Some(value) = value else {
                bail!("{arg:?} needs {name}, which this session doesn't have");
            };
            filled = filled.replace(name, value);
        }
        for name in ["{prompt}", "{prompt_file}", "{schema_file}"] {
            if filled.contains(name) {
                bail!("{arg:?} uses {name}, which ACP agents get over the protocol instead");
            }
        }
        Ok(filled)
    }
}

struct KeepEverything;

impl StreamParser for KeepEverything {
    fn parse_stdout_line(&mut self, line: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::unknown_line(line)]
    }

    fn parse_stderr_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let _ = line;
        Vec::new()
    }

    fn finish(&mut self, exit_code: Option<i32>) -> Vec<AgentEvent> {
        let _ = exit_code;
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use argonauts_core::VENDOR_KEY_ENV_VARS;
    use argonauts_core::registry::harness::{HomeKeep, HomeTemplate};
    use argonauts_core::settings::sessions::NativeFanout;

    use super::*;

    fn hermes(home: Option<HomeTemplate>) -> HarnessManifest {
        HarnessManifest {
            id: "hermes".into(),
            display_name: "Hermes Agent".into(),
            binary: "/opt/hermes/bin/hermes".into(),
            version_args: vec!["--version".into()],
            tested_versions: vec!["0.21.5".into()],
            read_only_args: vec!["acp".into()],
            build_args: vec!["acp".into(), "--cwd={workdir}".into()],
            family: "from-model".into(),
            transport: Transport::Acp,
            env_set: BTreeMap::from([("HERMES_ACP_SKIP_CONFIGURED_MCP".into(), "1".into())]),
            home,
            interactive_resume: vec!["--resume".into(), "{session}".into()],
            acp: Some(AcpProfile {
                steer: SteerRoute::Input,
                usage: vec![UsageSource::EndTurn, UsageSource::UsageUpdate],
                ..AcpProfile::default()
            }),
            ..HarnessManifest::default()
        }
    }

    fn template() -> HomeTemplate {
        HomeTemplate {
            env: "HERMES_HOME".into(),
            template: "hermes".into(),
            keep: HomeKeep::Card,
        }
    }

    #[test]
    fn acp_home_made_per_session() {
        let argonauts = tempfile::tempdir().unwrap();
        let template_dir = argonauts.path().join("harnesses/hermes");
        std::fs::create_dir_all(template_dir.join("skills")).unwrap();
        std::fs::write(template_dir.join("config.yaml"), "fallback_providers: []\n").unwrap();
        std::fs::write(template_dir.join("skills/README.md"), "No skills.\n").unwrap();
        let adapter = AcpAdapter::new(hermes(Some(template())), Some(argonauts.path().to_owned()));

        let request = |scratch: &str| SessionRequest {
            workdir: "/repo/wt".into(),
            role: SessionRole::Build,
            scratch_dir: Some(scratch.into()),
            env_set: vec![
                ("ARGONAUTS_SESSION".into(), "x".into()),
                ("HERMES_HOME".into(), "/mine".into()),
            ],
            ..SessionRequest::default()
        };
        let first = adapter.command(&request("/state/s1")).unwrap();
        let second = adapter.command(&request("/state/s2")).unwrap();
        assert_eq!(first.program, "/opt/hermes/bin/hermes");
        assert_eq!(first.args, ["acp", "--cwd=/repo/wt"]);
        assert_eq!(first.cwd, Path::new("/repo/wt"));
        assert!(first.keep_stdin_open);
        assert_eq!(
            first.env_set,
            [
                ("ARGONAUTS_SESSION".to_owned(), "x".to_owned()),
                ("HERMES_ACP_SKIP_CONFIGURED_MCP".to_owned(), "1".to_owned()),
                ("HERMES_HOME".to_owned(), "/state/s1/home".to_owned()),
            ]
        );
        let paths = |spec: &CommandSpec| {
            spec.files
                .iter()
                .map(|file| file.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            paths(&first),
            [
                PathBuf::from("/state/s1/home/config.yaml"),
                PathBuf::from("/state/s1/home/skills/README.md")
            ]
        );
        assert_eq!(
            paths(&second),
            [
                PathBuf::from("/state/s2/home/config.yaml"),
                PathBuf::from("/state/s2/home/skills/README.md")
            ]
        );
        assert!(first.files[0].contents.contains("fallback_providers: []"));
        // Hermes loads the worktree's reviewed skills.
        assert!(first.files[0].contents.contains("/repo/wt/.agents/skills"));
        assert!(
            second
                .env_set
                .contains(&("HERMES_HOME".to_owned(), "/state/s2/home".to_owned()))
        );

        let read_only = SessionRequest {
            role: SessionRole::ReadOnly,
            ..request("/state/s3")
        };
        assert_eq!(adapter.command(&read_only).unwrap().args, ["acp"]);

        // No session folder, a missing template, a template name that climbs out, or a link in
        // the template: refused.
        let no_scratch = SessionRequest {
            scratch_dir: None,
            ..request("/state/s4")
        };
        assert!(adapter.command(&no_scratch).is_err());
        let mut climbing = template();
        climbing.template = "../hermes".into();
        let adapter_climbing =
            AcpAdapter::new(hermes(Some(climbing)), Some(argonauts.path().to_owned()));
        let err = adapter_climbing.command(&request("/state/s5")).unwrap_err();
        assert!(
            format!("{err:#}").contains("must be a folder name"),
            "{err:#}"
        );
        let mut missing = template();
        missing.template = "goose".into();
        let adapter_missing =
            AcpAdapter::new(hermes(Some(missing)), Some(argonauts.path().to_owned()));
        let err = adapter_missing.command(&request("/state/s6")).unwrap_err();
        assert!(format!("{err:#}").contains("is missing"), "{err:#}");
        std::os::unix::fs::symlink("/etc/hosts", template_dir.join("hosts")).unwrap();
        let err = adapter.command(&request("/state/s7")).unwrap_err();
        assert!(format!("{err:#}").contains("link"), "{err:#}");

        // Without a home, no files, and the caller's variable stands.
        let bare = AcpAdapter::new(hermes(None), Some(argonauts.path().to_owned()));
        let spec = bare.command(&request("/state/s8")).unwrap();
        assert!(spec.files.is_empty());
        assert!(
            spec.env_set
                .contains(&("HERMES_HOME".to_owned(), "/mine".to_owned()))
        );
    }

    #[test]
    fn a_home_kept_for_the_card_lives_in_the_cards_folder() {
        let argonauts = tempfile::tempdir().unwrap();
        let template_dir = argonauts.path().join("harnesses/hermes");
        std::fs::create_dir_all(&template_dir).unwrap();
        std::fs::write(template_dir.join("config.yaml"), "fallback_providers: []\n").unwrap();
        let request = |scratch: &str, project: Option<&str>| SessionRequest {
            workdir: "/repo/wt".into(),
            role: SessionRole::Build,
            scratch_dir: Some(scratch.into()),
            project: project.map(str::to_owned),
            native_fanout: Some(NativeFanout::Limit { max: 2 }),
            ..SessionRequest::default()
        };
        let home_of = |spec: &CommandSpec| {
            spec.env_set
                .iter()
                .find(|(name, _)| name == "HERMES_HOME")
                .map(|(_, value)| value.clone())
        };

        let card = AcpAdapter::new(hermes(Some(template())), Some(argonauts.path().to_owned()));
        let folder = "/state/cards/c-001/homes/default/hermes";
        let first = card.command(&request("/state/s1", Some(folder))).unwrap();
        let resumed = card.command(&request("/state/s2", Some(folder))).unwrap();
        assert_eq!(home_of(&first).as_deref(), Some(folder));
        assert_eq!(home_of(&resumed).as_deref(), Some(folder));
        assert_eq!(
            first.files[0].path,
            PathBuf::from(folder).join("config.yaml")
        );
        assert!(
            first.files[0]
                .contents
                .contains("max_concurrent_children: 2")
        );

        // Off a card, or with a name that isn't a folder, the session's own folder holds it.
        let chat = card.command(&request("/state/s3", None)).unwrap();
        assert_eq!(home_of(&chat).as_deref(), Some("/state/s3/home"));
        let named = card.command(&request("/state/s4", Some("run-7"))).unwrap();
        assert_eq!(home_of(&named).as_deref(), Some("/state/s4/home"));

        // A home kept for the session never moves to the card's folder.
        let session = AcpAdapter::new(
            hermes(Some(HomeTemplate {
                keep: HomeKeep::Session,
                ..template()
            })),
            Some(argonauts.path().to_owned()),
        );
        let spec = session
            .command(&request("/state/s5", Some(folder)))
            .unwrap();
        assert_eq!(home_of(&spec).as_deref(), Some("/state/s5/home"));

        // Another agent's home config is left as its template has it.
        let other = AcpAdapter::new(
            hermes(Some(HomeTemplate {
                env: "GOOSE_HOME".into(),
                ..template()
            })),
            Some(argonauts.path().to_owned()),
        );
        let spec = other.command(&request("/state/s6", Some(folder))).unwrap();
        assert_eq!(spec.files[0].contents, "fallback_providers: []\n");
    }

    #[test]
    fn acp_command_scrubs_vendor_keys() {
        let request = SessionRequest {
            workdir: "/repo/wt".into(),
            env_set: vec![("HERMES_ACP_SKIP_CONFIGURED_MCP".into(), "0".into())],
            ..SessionRequest::default()
        };
        let spec = acp_adapter(hermes(None)).command(&request).unwrap();
        for name in VENDOR_KEY_ENV_VARS {
            assert!(
                spec.env_remove.iter().any(|removed| removed == name),
                "{name}"
            );
            assert!(!spec.env_set.iter().any(|(set, _)| set == name), "{name}");
        }
        assert_eq!(
            spec.env_set,
            [("HERMES_ACP_SKIP_CONFIGURED_MCP".to_owned(), "1".to_owned())]
        );

        // A key the profile names is the only one it keeps.
        let mut openrouter = hermes(None);
        openrouter.env = vec!["OPENROUTER_API_KEY".into()];
        let spec = acp_adapter(openrouter).command(&request).unwrap();
        for name in VENDOR_KEY_ENV_VARS {
            let removed = spec.env_remove.iter().any(|removed| removed == name);
            assert_eq!(removed, *name != "OPENROUTER_API_KEY", "{name}");
        }
    }

    #[test]
    fn the_adapter_describes_an_acp_profile() {
        let adapter = acp_adapter(hermes(None));
        assert_eq!(adapter.id(), "hermes");
        assert_eq!(adapter.transport(), Transport::Acp);
        assert_eq!(adapter.tested_versions(), ["0.21.5"]);
        assert_eq!(adapter.version_args(), ["--version"]);
        assert_eq!(adapter.default_family(), "from-model");
        assert!(!adapter.capabilities().enforces_read_only);
        assert!(adapter.capabilities().reports_cost);
        assert!(adapter.session_driver().is_some());
        assert_eq!(adapter.input_line("hi").as_deref(), Some("hi"));
        let resume = adapter
            .interactive_resume("s-9", Path::new("/repo"))
            .unwrap();
        assert_eq!(resume.args, ["--resume", "s-9"]);

        let mut quiet = hermes(None);
        quiet.acp = Some(AcpProfile::default());
        quiet.interactive_resume = Vec::new();
        quiet.binary = "hermes".into();
        let adapter = acp_adapter(quiet);
        assert_eq!(adapter.input_line("hi"), None);
        assert!(
            adapter
                .interactive_resume("s-9", Path::new("/repo"))
                .is_none()
        );
        let err = adapter.command(&SessionRequest::default()).unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");

        let mut launch = hermes(None);
        launch.read_only_args = vec!["acp".into(), "--model".into(), "{model}".into()];
        let adapter = acp_adapter(launch);
        let with_model = SessionRequest {
            model: Some("qwen3-coder".into()),
            ..SessionRequest::default()
        };
        assert_eq!(
            adapter.command(&with_model).unwrap().args,
            ["acp", "--model", "qwen3-coder"]
        );
        let err = adapter.command(&SessionRequest::default()).unwrap_err();
        assert!(format!("{err:#}").contains("{model}"), "{err:#}");
    }
}
