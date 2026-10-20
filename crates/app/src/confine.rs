//! The sandbox a CLI's own commands run in outside any session, such as OpenCode listing its
//! sessions, models or providers. One started in a repo may load the plugins its config sets
//! up, so each gets what a read-only session of that CLI gets, without the proxy, since
//! nothing it runs calls a model, and in a repo you haven't trusted, no network.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use argonauts_conductor::sandbox::selftest::self_test_once;
use argonauts_conductor::sandbox::{self, ProfileInput, SandboxWrap, session_profile};
use argonauts_core::CommandSpec;
use argonauts_core::confine::Confine;
use argonauts_core::future::BoxFuture;
use argonauts_core::paths::Paths;
use argonauts_core::session_runner::{Access, SandboxMode, SandboxProfile};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxSettings;

use crate::host::clis::RepoModels;

/// The sandbox `settings` give a CLI's own commands, for `repo` when there is one.
pub(crate) fn read_only(settings: &Settings, repo: Option<&RepoModels>) -> Arc<dyn Confine> {
    Arc::new(ReadOnly::new(settings, repo))
}

/// Each command reads the folder it starts in and writes only its CLI's own state and the
/// temporary folders, kept from your credentials, the other CLIs' state and Argonauts Code's
/// home. It reaches the network as `sandbox.network` says, but for one that starts in a repo
/// you haven't trusted, which reaches only this computer's own servers. The sandbox has to
/// pass its self-test first, as it does before a session.
struct ReadOnly {
    settings: SandboxSettings,
    /// Made on the first command, since finding a container runtime looks along `PATH`.
    sandbox: OnceLock<Arc<dyn SandboxWrap>>,
    /// Both homes are kept out, so a command runs only once both are known.
    argonauts_home: Option<PathBuf>,
    user_home: Option<PathBuf>,
    untrusted_repo: Option<PathBuf>,
}

impl fmt::Debug for ReadOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadOnly")
            .field("settings", &self.settings)
            .field("argonauts_home", &self.argonauts_home)
            .field("user_home", &self.user_home)
            .finish_non_exhaustive()
    }
}

impl ReadOnly {
    fn new(settings: &Settings, repo: Option<&RepoModels>) -> Self {
        Self {
            settings: settings.sandbox.clone(),
            sandbox: OnceLock::new(),
            argonauts_home: Paths::from_env().ok().map(|paths| paths.home().to_owned()),
            user_home: std::env::var_os("HOME").map(PathBuf::from),
            untrusted_repo: repo
                .filter(|repo| !repo.trusted())
                .map(|repo| repo.root().to_owned()),
        }
    }

    /// Whether a command started in `cwd` reaches the network: as `sandbox.network` says, but
    /// never from inside a repo you haven't trusted.
    fn network(&self, cwd: &Path) -> bool {
        let untrusted = self
            .untrusted_repo
            .as_ref()
            .is_some_and(|repo| cwd.starts_with(repo));
        self.settings.network && !untrusted
    }

    fn sandbox(&self) -> &dyn SandboxWrap {
        self.sandbox
            .get_or_init(|| sandbox::for_settings(&self.settings))
            .as_ref()
    }
}

impl Confine for ReadOnly {
    fn confine<'a>(
        &'a self,
        cli: &'a str,
        command: CommandSpec,
    ) -> BoxFuture<'a, Result<CommandSpec, String>> {
        Box::pin(async move {
            let (Some(argonauts_home), Some(user_home)) = (&self.argonauts_home, &self.user_home)
            else {
                return Err(
                    "your home or Argonauts Code's can't be found, so the sandbox can't keep it \
                     out"
                    .to_owned(),
                );
            };
            let sandbox = self.sandbox();
            if sandbox.mode() != SandboxMode::None {
                let test = self_test_once(sandbox).await;
                if !test.ok {
                    return Err(format!("the sandbox doesn't work here: {}", test.detail));
                }
            }
            let homes = (argonauts_home.as_path(), user_home.as_path());
            let network = self.network(&command.cwd);
            let profile = profile(sandbox, network, cli, &command.cwd, homes);
            sandbox
                .wrap(command, &profile)
                .map_err(|err| err.to_string())
        })
    }
}

/// What a read-only session of `cli` in `workdir` may touch, with the network as `network`
/// says and no proxy. `homes` are Argonauts Code's and yours.
fn profile(
    sandbox: &dyn SandboxWrap,
    network: bool,
    cli: &str,
    workdir: &Path,
    (argonauts_home, user_home): (&Path, &Path),
) -> SandboxProfile {
    let scratch = std::env::temp_dir().join("argonauts-listing");
    let input = ProfileInput {
        access: Access::ReadOnly,
        agent_id: cli,
        workdir,
        session_dir: &scratch,
        argonauts_home,
        user_home,
        base_worktree: None,
        status_file: None,
        extra_writable: &[],
        sees_other_worktrees: false,
        commits: false,
    };
    let mut profile = session_profile(&SandboxProfile::default(), &input);
    profile.mode = sandbox.mode();
    profile.network = network;
    profile.proxy_port = None;
    profile
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::sandbox::{Unsandboxed, can_read, can_write};

    use super::*;

    fn homes() -> (&'static Path, &'static Path) {
        (Path::new("/home/me/.argonauts"), Path::new("/home/me"))
    }

    #[test]
    fn opencode_writes_its_own_state_and_reads_the_repo_alone() {
        let repo = Path::new("/work/repo");
        let profile = profile(&Unsandboxed, false, "opencode", repo, homes());
        assert!(can_read(&profile, &repo.join("opencode.json")));
        assert!(!can_write(&profile, &repo.join("opencode.json")));
        assert!(!can_write(
            &profile,
            &repo.join(".opencode/plugin/planted.ts")
        ));
        assert!(can_write(
            &profile,
            Path::new("/home/me/.local/share/opencode/opencode.db")
        ));
        assert!(can_write(
            &profile,
            Path::new("/home/me/.cache/opencode/models.json")
        ));
        assert!(!can_write(
            &profile,
            Path::new("/home/me/.config/opencode/opencode.json")
        ));
        for kept_out in [
            "/home/me/.argonauts/settings.toml",
            "/home/me/.ssh/id_ed25519",
            "/home/me/.codex/config.toml",
        ] {
            assert!(!can_read(&profile, Path::new(kept_out)), "{kept_out}");
        }
        assert!(!profile.network);
        assert_eq!(profile.proxy_port, None);
    }

    #[tokio::test]
    async fn nothing_runs_while_a_home_is_unknown() {
        let unknown = ReadOnly {
            settings: SandboxSettings::default(),
            sandbox: OnceLock::new(),
            argonauts_home: None,
            user_home: Some(PathBuf::from("/home/me")),
            untrusted_repo: None,
        };
        let command = CommandSpec::new("opencode", "/work/repo").args(["session", "list"]);
        let why = unknown.confine("opencode", command).await.unwrap_err();
        assert!(why.contains("can't keep it out"), "{why}");
    }

    #[tokio::test]
    async fn with_no_sandbox_set_a_command_runs_as_it_is() {
        let settings = Settings {
            sandbox: SandboxSettings {
                mode: argonauts_core::settings::approvals::SandboxChoice::None,
                ..SandboxSettings::default()
            },
            ..Settings::default()
        };
        let confine = ReadOnly {
            settings: settings.sandbox.clone(),
            sandbox: OnceLock::new(),
            argonauts_home: Some(PathBuf::from("/home/me/.argonauts")),
            user_home: Some(PathBuf::from("/home/me")),
            untrusted_repo: None,
        };
        let command = CommandSpec::new("opencode", "/work/repo").args(["session", "list"]);
        let confined = confine.confine("opencode", command.clone()).await.unwrap();
        assert_eq!(confined, command);
    }

    /// A repo you haven't trusted may bring OpenCode plugins, so a command started in it
    /// reaches only this computer's own servers until you trust it, whatever sandbox.network
    /// says. Trusted, it follows sandbox.network.
    #[test]
    fn in_a_repo_you_havent_trusted_a_command_runs_offline() {
        let repo = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let repo_models = RepoModels::new(repo.path(), state.path());
        let mut settings = Settings::default();
        settings.sandbox.network = true;
        let untrusted = ReadOnly::new(&settings, Some(&repo_models));
        assert!(!untrusted.network(repo.path()));
        assert!(!untrusted.network(&repo.path().join("src")));
        // A model list asked from a folder of no project keeps the network.
        assert!(untrusted.network(&std::env::temp_dir()));

        let config = argonauts_core::project_config_path(repo.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "workflow = \"orchestrator\"\n").unwrap();
        crate::trust::TrustStore::load(state.path())
            .trust(&config)
            .unwrap();
        assert!(repo_models.trusted());
        assert!(ReadOnly::new(&settings, Some(&repo_models)).network(repo.path()));
        settings.sandbox.network = false;
        assert!(!ReadOnly::new(&settings, Some(&repo_models)).network(repo.path()));
    }
}
