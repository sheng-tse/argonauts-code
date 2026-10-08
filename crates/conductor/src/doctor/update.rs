//! `/update` and `argonauts update`: each installed agent CLI's own updater in turn, such as
//! `claude update`, with the version read before and after. Never a model call.

use std::sync::Arc;
use std::time::Duration;

use argonauts_core::Adapter;
use argonauts_core::settings::Settings;

use super::{BinaryProblem, VERSION_TIMEOUT, command, find_binary, first_line, read_version};

/// An updater downloads, so it gets far longer than a version check.
pub const UPDATE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliUpdate {
    /// Such as `Claude Code`.
    pub name: String,
    pub outcome: UpdateOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    NotInstalled {
        install: Option<String>,
    },
    /// Argonauts knows no updater of its own for this CLI.
    NoUpdater {
        version: Option<String>,
    },
    UpToDate {
        version: String,
    },
    Updated {
        from: Option<String>,
        to: String,
    },
    Failed {
        version: Option<String>,
        why: String,
    },
}

/// Runs each adapter's updater, one after another, as `timeout` allows each.
pub async fn update_clis(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
    timeout: Duration,
) -> Vec<CliUpdate> {
    let mut updates = Vec::new();
    for adapter in adapters {
        let binary = settings
            .agents
            .get(adapter.id())
            .and_then(|agent| agent.binary.as_deref());
        updates.push(CliUpdate {
            name: adapter.display_name().to_owned(),
            outcome: update_one(adapter.as_ref(), binary, timeout).await,
        });
    }
    updates
}

async fn update_one(
    adapter: &dyn Adapter,
    binary: Option<&std::path::Path>,
    timeout: Duration,
) -> UpdateOutcome {
    let path = match find_binary(adapter, binary) {
        Ok(path) => path,
        Err(BinaryProblem::NotInstalled(_)) => {
            return UpdateOutcome::NotInstalled {
                install: adapter.setup_hints().install,
            };
        }
        Err(problem) => {
            return UpdateOutcome::Failed {
                version: None,
                why: problem.to_string(),
            };
        }
    };
    let version = || async {
        read_version(&path, adapter.version_args(), VERSION_TIMEOUT)
            .await
            .ok()
            .and_then(|output| adapter.parse_version(&output))
    };
    let before = version().await;
    let Some(args) = adapter.update_args() else {
        return UpdateOutcome::NoUpdater { version: before };
    };
    let answer = match command::ask(&path, args, timeout).await {
        Ok(answer) if answer.success => answer,
        Ok(answer) => {
            return UpdateOutcome::Failed {
                version: before,
                why: last_line(&answer.text).to_owned(),
            };
        }
        Err(why) => {
            return UpdateOutcome::Failed {
                version: before,
                why,
            };
        }
    };
    match (before, version().await) {
        (Some(before), Some(after)) if before == after => {
            UpdateOutcome::UpToDate { version: after }
        }
        (before, Some(after)) => UpdateOutcome::Updated {
            from: before,
            to: after,
        },
        (before, None) => UpdateOutcome::Failed {
            version: before,
            why: format!(
                "its version can't be read after the update: {}",
                first_line(&answer.text)
            ),
        },
    }
}

/// An updater's last words say best what went wrong.
fn last_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("it failed and said nothing")
}

/// One line per CLI, names and versions in columns:
/// `Claude Code       2.1.292 → 2.1.295`, `Codex             0.160.0   up to date`.
pub fn update_lines(updates: &[CliUpdate]) -> Vec<String> {
    let name_width = updates
        .iter()
        .map(|update| update.name.chars().count())
        .max()
        .unwrap_or(0);
    let version_width = updates
        .iter()
        .filter_map(|update| before(&update.outcome))
        .map(|version| version.chars().count())
        .max()
        .unwrap_or(0);
    updates
        .iter()
        .map(|update| {
            let name = format!("{:name_width$}", update.name);
            let version = format!("{:version_width$}", before(&update.outcome).unwrap_or("?"));
            let line = match &update.outcome {
                UpdateOutcome::NotInstalled {
                    install: Some(install),
                } => {
                    format!("{name}   not installed · {install}")
                }
                UpdateOutcome::NotInstalled { install: None } => format!("{name}   not installed"),
                UpdateOutcome::NoUpdater { .. } => {
                    format!("{name}   {version}   has no updater Argonauts knows")
                }
                UpdateOutcome::UpToDate { .. } => format!("{name}   {version}   up to date"),
                UpdateOutcome::Updated { to, .. } => format!("{name}   {version} → {to}"),
                UpdateOutcome::Failed { why, .. } => {
                    format!("{name}   {version}   couldn't update: {why}")
                }
            };
            line.trim_end().to_owned()
        })
        .collect()
}

fn before(outcome: &UpdateOutcome) -> Option<&str> {
    match outcome {
        UpdateOutcome::NotInstalled { .. } => None,
        UpdateOutcome::NoUpdater { version } | UpdateOutcome::Failed { version, .. } => {
            version.as_deref()
        }
        UpdateOutcome::UpToDate { version } => Some(version),
        UpdateOutcome::Updated { from, .. } => from.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::tests::FakeHarness;

    /// A CLI whose version lives in a file its updater rewrites.
    fn updating(id: &'static str, dir: &std::path::Path, update: &str) -> Arc<dyn Adapter> {
        let version = dir.join(format!("{id}-version"));
        std::fs::write(&version, "1.0.0\n").unwrap();
        let body = format!(
            "case \"$1\" in\n  --version) cat '{}' ;;\n  update) {update} ;;\nesac",
            version.display()
        );
        let body = body.replace("$VERSION", &version.display().to_string());
        Arc::new(FakeHarness::script(id, dir, &body, &["1.0.0"]).with_update(&["update"]))
    }

    #[tokio::test]
    async fn each_cli_runs_its_own_updater_and_says_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let adapters = vec![
            updating("moved", dir.path(), "echo 2.0.0 > '$VERSION'; echo Updated"),
            updating("current", dir.path(), "echo Already up to date"),
            updating(
                "offline",
                dir.path(),
                "echo checking; echo 'no network' >&2; exit 1",
            ),
            Arc::new(FakeHarness::script("plain", dir.path(), "echo 3.1.4", &[]))
                as Arc<dyn Adapter>,
            Arc::new(FakeHarness::named("absent")) as Arc<dyn Adapter>,
        ];
        let updates = update_clis(&adapters, &Settings::default(), VERSION_TIMEOUT).await;
        let outcomes: Vec<&UpdateOutcome> = updates.iter().map(|update| &update.outcome).collect();
        assert_eq!(
            outcomes,
            [
                &UpdateOutcome::Updated {
                    from: Some("1.0.0".into()),
                    to: "2.0.0".into()
                },
                &UpdateOutcome::UpToDate {
                    version: "1.0.0".into()
                },
                &UpdateOutcome::Failed {
                    version: Some("1.0.0".into()),
                    why: "no network".into()
                },
                &UpdateOutcome::NoUpdater {
                    version: Some("3.1.4".into())
                },
                &UpdateOutcome::NotInstalled { install: None },
            ]
        );
    }

    #[test]
    fn lines_put_names_and_versions_in_columns() {
        let update = |name: &str, outcome| CliUpdate {
            name: name.to_owned(),
            outcome,
        };
        let lines = update_lines(&[
            update(
                "Claude Code",
                UpdateOutcome::Updated {
                    from: Some("2.1.292".into()),
                    to: "2.1.295".into(),
                },
            ),
            update(
                "Codex",
                UpdateOutcome::UpToDate {
                    version: "0.160.0".into(),
                },
            ),
            update(
                "Antigravity CLI",
                UpdateOutcome::Updated {
                    from: Some("1.3.0".into()),
                    to: "1.3.2".into(),
                },
            ),
            update(
                "OpenCode",
                UpdateOutcome::NotInstalled {
                    install: Some("npm i -g opencode-ai@latest".into()),
                },
            ),
            update(
                "Hermes",
                UpdateOutcome::Failed {
                    version: Some("1.0.0".into()),
                    why: "no network".into(),
                },
            ),
        ]);
        assert_eq!(
            lines,
            [
                "Claude Code       2.1.292 → 2.1.295",
                "Codex             0.160.0   up to date",
                "Antigravity CLI   1.3.0   → 1.3.2",
                "OpenCode          not installed · npm i -g opencode-ai@latest",
                "Hermes            1.0.0     couldn't update: no network",
            ]
        );
    }
}
