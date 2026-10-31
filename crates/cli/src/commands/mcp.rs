//! `argonauts mcp serve`: your runs to an MCP client such as Hermes or OpenClaw. An outside agent
//! can keep the server for days, so `mcp.*` and the repo's trust are read again on every call,
//! and the server keeps whichever of those and the start values is stricter.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use argonauts_core::base::layout::repo_state;
use argonauts_core::{Paths, RepoId, project_config_path, project_local_config_path, sha256_hex};
use argonauts_mcp::{OutsideAgentOptions, OutsideAgentReloader, OutsideAgentSettings};
use serde::Deserialize;

/// A direct turn's questions: where the app listens, and which tools ask it.
pub struct Asking<'a> {
    pub endpoint: &'a str,
    pub approve: bool,
    /// The main session's tools to offer, by name.
    pub tools: &'a [String],
    /// Crew roles it may ask, such as `reviewer`.
    pub roles: &'a [String],
}

/// One agent session's tools. `report` is the step's schema file and where valid reports go.
pub async fn session(
    status_file: Option<&Path>,
    report: Option<(&Path, &Path)>,
    asking: Option<Asking<'_>>,
) -> anyhow::Result<()> {
    let report = match report {
        Some((schema, report_file)) => {
            let text = std::fs::read_to_string(schema)
                .with_context(|| format!("reading the report schema {}", schema.display()))?;
            let schema = serde_json::from_str(&text)
                .with_context(|| format!("reading the report schema {}", schema.display()))?;
            Some(argonauts_mcp::ReportTool {
                schema,
                report_file: report_file.to_owned(),
            })
        }
        None => None,
    };
    let asking = match asking {
        Some(asking) => Some(argonauts_mcp::AskTool {
            endpoint: asking.endpoint.to_owned(),
            token: std::env::var(APPROVE_TOKEN_ENV).unwrap_or_default(),
            approve: asking.approve,
            tools: asking.tools.to_vec(),
            roles: asking
                .roles
                .iter()
                .map(|role| role.parse())
                .collect::<Result<_, _>>()
                .map_err(|err| anyhow::anyhow!("--ask-role: {err}"))?,
        }),
        None => None,
    };
    argonauts_mcp::serve_session(status_file, report, asking).await
}

/// Set by the session's MCP config, so the token stays out of the process list.
const APPROVE_TOKEN_ENV: &str = "ARGONAUTS_APPROVE_TOKEN";

pub async fn serve(repo: Option<&Path>, set: &[String]) -> anyhow::Result<ExitCode> {
    let root = argonauts_app::repo_root(repo)?;
    let root = root
        .canonicalize()
        .with_context(|| format!("opening {}", root.display()))?;
    let start = outside_agent_settings(&root, set)?;
    let reload: OutsideAgentReloader = {
        let root = root.clone();
        let set = set.to_vec();
        Arc::new(move || outside_agent_settings(&root, &set).ok())
    };
    let options = OutsideAgentOptions {
        mcp: start.mcp,
        repo_trusted: start.repo_trusted,
        reload: Some(reload),
    };
    argonauts_mcp::serve_user_with(argonauts_app::conductor(&root)?, options).await?;
    Ok(ExitCode::SUCCESS)
}

/// The repo's `mcp.*` as resolved now, safety keys from untrusted files left out, and whether
/// you trust the repo.
fn outside_agent_settings(root: &Path, set: &[String]) -> anyhow::Result<OutsideAgentSettings> {
    let settings = argonauts_app::current_settings(Some(root), set.to_vec())?;
    let home = Paths::from_env()?;
    Ok(OutsideAgentSettings {
        mcp: settings.mcp,
        repo_trusted: repo_trusted(&home, root),
    })
}

/// You set the repo up with `argonauts setup`, which made its `.argonauts` folder, and you
/// trusted each settings file in it as it is now. An unreadable trust record trusts nothing.
fn repo_trusted(home: &Paths, root: &Path) -> bool {
    if !root.join(".argonauts").is_dir() {
        return false;
    }
    let Ok(repo) = RepoId::for_repo(root) else {
        return false;
    };
    let trusted = TrustRecord::read(&home.state_dir(&repo).join(repo_state::TRUSTED));
    [project_config_path(root), project_local_config_path(root)]
        .iter()
        .all(|file| trusted.holds(file))
}

/// `trusted.json`, which the app writes when you trust a project file: each file's path and
/// the SHA-256 of what it held then.
#[derive(Debug, Default, Deserialize)]
struct TrustRecord {
    #[serde(default)]
    files: BTreeMap<String, String>,
}

impl TrustRecord {
    fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// A file that doesn't exist sets nothing, so there's nothing to distrust.
    fn holds(&self, file: &Path) -> bool {
        match std::fs::read(file) {
            Ok(bytes) => self.files.get(&file.display().to_string()) == Some(&sha256_hex(&bytes)),
            Err(_) => !file.exists(),
        }
    }
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::TempRepo;
    use serde_json::json;

    use super::*;

    fn trust(repo: &TempRepo, files: &[&Path]) {
        let root = repo.root().canonicalize().unwrap();
        let state = Paths::new(repo.home()).state_dir(&RepoId::for_repo(&root).unwrap());
        std::fs::create_dir_all(&state).unwrap();
        let files: BTreeMap<String, String> = files
            .iter()
            .map(|file| {
                let hash = sha256_hex(&std::fs::read(file).unwrap());
                (file.display().to_string(), hash)
            })
            .collect();
        std::fs::write(
            state.join(repo_state::TRUSTED),
            json!({ "files": files }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_repo_is_trusted_once_set_up_and_each_settings_file_trusted() {
        let repo = TempRepo::new().unwrap();
        let home = Paths::new(repo.home());
        let root = repo.root().canonicalize().unwrap();
        assert!(!repo_trusted(&home, &root), "not set up");

        std::fs::create_dir_all(root.join(".argonauts")).unwrap();
        assert!(repo_trusted(&home, &root), "set up with no settings files");

        let project = repo
            .write(".argonauts/config.toml", "[mcp]\nruns_per_day = 3\n")
            .unwrap();
        let project = project.canonicalize().unwrap();
        assert!(!repo_trusted(&home, &root), "a file you never trusted");
        trust(&repo, &[&project]);
        assert!(repo_trusted(&home, &root));

        let local = repo
            .write(".argonauts/config.local.toml", "[ui]\nbell = false\n")
            .unwrap();
        assert!(!repo_trusted(&home, &root), "a new untrusted file");
        trust(&repo, &[&project, &local.canonicalize().unwrap()]);
        assert!(repo_trusted(&home, &root));

        std::fs::write(&project, "[mcp]\nruns_per_day = 300\n").unwrap();
        assert!(!repo_trusted(&home, &root), "changed since you trusted it");
    }

    #[test]
    fn an_unreadable_trust_record_trusts_nothing() {
        let repo = TempRepo::new().unwrap();
        let root = repo.root().canonicalize().unwrap();
        repo.write(".argonauts/config.toml", "").unwrap();
        let state = Paths::new(repo.home()).state_dir(&RepoId::for_repo(&root).unwrap());
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join(repo_state::TRUSTED), "not json").unwrap();
        assert!(!repo_trusted(&Paths::new(repo.home()), &root));
    }
}
