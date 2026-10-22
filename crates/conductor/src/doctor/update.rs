//! `/update` and `argonauts update`: each installed agent CLI updated the way it was installed,
//! by Homebrew's or npm's command or by its own updater, such as `claude update`, with the
//! version read before and after. The CLIs update side by side, one at a time for each program
//! that updates several. Never a model call.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_core::Adapter;
use argonauts_core::adapter::Transport;
use argonauts_core::settings::Settings;
use tokio_util::sync::CancellationToken;

use super::newest::{install_source, update_program};
use super::{
    BinaryProblem, VERSION_TIMEOUT, command, find_binary, first_line, installed_version,
    read_version,
};
use crate::plan_usage::find_unsandboxed;

/// An updater downloads, so it gets far longer than a version check.
pub const UPDATE_TIMEOUT: Duration = Duration::from_secs(300);
/// An updater that has printed nothing this long is taken to hang, and stopped.
pub const UPDATE_QUIET: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliUpdate {
    /// Such as `Claude Code`.
    pub name: String,
    pub outcome: UpdateOutcome,
    /// The other ways Argonauts Code reaches the same program, such as `ACP`.
    pub also_through: Vec<String>,
}

/// A CLI to update once, with the other ways Argonauts Code reaches its program, such as
/// OpenCode's over ACP.
#[derive(Clone)]
pub struct OneCli {
    pub adapter: Arc<dyn Adapter>,
    pub also_through: Vec<String>,
}

/// Each CLI once: an adapter that runs over ACP the program another adapter runs, as OpenCode
/// over ACP runs `opencode`, is that CLI's second way in rather than a CLI of its own.
pub fn each_once(adapters: &[Arc<dyn Adapter>], settings: &Settings) -> Vec<OneCli> {
    each_once_found(adapters, settings, &|named| {
        crate::plan_usage::find_unsandboxed(&named.to_string_lossy(), None).ok()
    })
}

/// [`each_once`], with `found` saying where a program an adapter names is, so a name and the
/// path it stands for are one program, as an ACP profile carries the path `PATH` gave it.
fn each_once_found(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
    found: &dyn Fn(&Path) -> Option<PathBuf>,
) -> Vec<OneCli> {
    let program = |adapter: &dyn Adapter| -> PathBuf {
        let named = settings
            .agents
            .get(adapter.id())
            .and_then(|agent| agent.binary.clone())
            .unwrap_or_else(|| PathBuf::from(adapter.binary()));
        found(&named)
            .and_then(|path| path.canonicalize().ok())
            .unwrap_or(named)
    };
    let mut clis: Vec<OneCli> = Vec::new();
    for adapter in adapters {
        let reaches = (adapter.transport() == Transport::Acp)
            .then(|| {
                adapters.iter().find(|other| {
                    other.transport() != Transport::Acp
                        && program(other.as_ref()) == program(adapter.as_ref())
                })
            })
            .flatten();
        // A CLI is listed where the first of its ways in comes.
        let listed = reaches.unwrap_or(adapter);
        let at = match clis.iter().position(|cli| cli.adapter.id() == listed.id()) {
            Some(at) => at,
            None => {
                clis.push(OneCli {
                    adapter: listed.clone(),
                    also_through: Vec::new(),
                });
                clis.len() - 1
            }
        };
        if reaches.is_some() {
            clis[at].also_through.push("ACP".to_owned());
        }
    }
    clis
}

/// The columns of the Update report, sized from every CLI before any is updated, so a row
/// never moves as the next one comes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateColumns {
    name: usize,
    version: usize,
}

impl UpdateColumns {
    /// From each CLI's name and the version installed now, read before anything updates.
    pub async fn of(clis: &[OneCli], settings: &Settings) -> Self {
        let mut columns = Self::default();
        for cli in clis {
            let adapter = cli.adapter.as_ref();
            let binary = settings
                .agents
                .get(adapter.id())
                .and_then(|agent| agent.binary.as_deref());
            let version = installed_version(adapter, binary).await;
            columns.name = columns.name.max(adapter.display_name().chars().count());
            columns.version = columns
                .version
                .max(version.map_or(0, |version| version.chars().count()));
        }
        columns
    }

    /// Wide enough for one more row's `name` and `version` too, such as Argonauts Code's own
    /// above the CLIs'.
    pub fn with_row(self, name: &str, version: &str) -> Self {
        Self {
            name: self.name.max(name.chars().count()),
            version: self.version.max(version.chars().count()),
        }
    }

    /// From the rows themselves, once every CLI is done.
    fn from_rows(updates: &[CliUpdate]) -> Self {
        Self {
            name: updates
                .iter()
                .map(|update| update.name.chars().count())
                .max()
                .unwrap_or(0),
            version: updates
                .iter()
                .filter_map(|update| before(&update.outcome))
                .map(|version| version.chars().count())
                .max()
                .unwrap_or(0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    NotInstalled {
        install: Option<String>,
    },
    /// Argonauts Code knows no updater of its own for this CLI.
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
    /// Stopped before it was done, or before it started.
    Stopped {
        version: Option<String>,
    },
}

/// How long an updater may run in all, and how long it may print nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateLimits {
    pub timeout: Duration,
    pub quiet: Duration,
}

impl UpdateLimits {
    /// `timeout` in all, and stopped after `UPDATE_QUIET` with nothing printed, or sooner
    /// where `timeout` is shorter.
    pub fn within(timeout: Duration) -> Self {
        Self {
            timeout,
            quiet: UPDATE_QUIET.min(timeout),
        }
    }
}

/// Updates each adapter's CLI once, side by side, as `timeout` allows each, until `stop`.
pub async fn update_clis(
    adapters: &[Arc<dyn Adapter>],
    settings: &Settings,
    timeout: Duration,
    stop: &CancellationToken,
) -> Vec<CliUpdate> {
    let limits = UpdateLimits::within(timeout);
    update_each(
        &each_once(adapters, settings),
        settings,
        limits,
        stop,
        |_| {},
    )
    .await
}

/// Updates `clis` side by side, as `limits` allow each, and hands `done` the rows so far in
/// `clis`' order each time more of them, counted from the first, are done: a slow updater
/// holds back the rows after it on the screen, never the work of the others. The CLIs that
/// one program updates, as Homebrew or npm does, go one at a time, since it installs one
/// thing at once. Once `stop` is cancelled, the updaters at work end with everything they
/// started, and every CLI not done yet reads stopped.
pub async fn update_each(
    clis: &[OneCli],
    settings: &Settings,
    limits: UpdateLimits,
    stop: &CancellationToken,
    mut done: impl FnMut(&[CliUpdate]),
) -> Vec<CliUpdate> {
    let steps: Vec<Step> = clis
        .iter()
        .map(|cli| {
            step(
                cli.adapter.as_ref(),
                binary_of(cli.adapter.as_ref(), settings),
            )
        })
        .collect();
    // The CLIs each updater program takes, in order. Each CLI that updates itself is its own.
    let mut queues: Vec<(Option<PathBuf>, Vec<usize>)> = Vec::new();
    for (at, step) in steps.iter().enumerate() {
        let updater = match step {
            Step::Run { updater, .. } => Some(updater.clone()),
            _ => None,
        };
        match queues
            .iter_mut()
            .find(|(program, _)| updater.is_some() && *program == updater)
        {
            Some((_, queue)) => queue.push(at),
            None => queues.push((updater, vec![at])),
        }
    }
    let (finished, mut each) = tokio::sync::mpsc::unbounded_channel();
    let steps = &steps;
    let work = async move {
        let queues = queues.into_iter().map(|(_, queue)| {
            let finished = finished.clone();
            async move {
                for at in queue {
                    let cli = &clis[at];
                    let adapter = cli.adapter.as_ref();
                    let outcome = if stop.is_cancelled() {
                        stopped(adapter, &steps[at]).await
                    } else {
                        take(adapter, &steps[at], limits, stop).await
                    };
                    let update = CliUpdate {
                        name: adapter.display_name().to_owned(),
                        outcome,
                        also_through: cli.also_through.clone(),
                    };
                    let _ = finished.send((at, update));
                }
            }
        });
        futures::future::join_all(queues).await;
    };
    let mut rows: Vec<Option<CliUpdate>> = vec![None; clis.len()];
    let mut shown = 0;
    let report = async {
        while let Some((at, update)) = each.recv().await {
            rows[at] = Some(update);
            let ready = rows.iter().take_while(|row| row.is_some()).count();
            if ready > shown {
                shown = ready;
                let so_far: Vec<CliUpdate> = rows[..ready].iter().flatten().cloned().collect();
                done(&so_far);
            }
        }
    };
    tokio::join!(work, report);
    rows.into_iter().flatten().collect()
}

/// Updates one CLI, found where `agents.<id>.binary` points or on `PATH`.
pub async fn update_cli(
    adapter: &dyn Adapter,
    settings: &Settings,
    timeout: Duration,
) -> CliUpdate {
    let step = step(adapter, binary_of(adapter, settings));
    let never = CancellationToken::new();
    CliUpdate {
        name: adapter.display_name().to_owned(),
        outcome: take(adapter, &step, UpdateLimits::within(timeout), &never).await,
        also_through: Vec::new(),
    }
}

fn binary_of<'a>(adapter: &dyn Adapter, settings: &'a Settings) -> Option<&'a Path> {
    settings
        .agents
        .get(adapter.id())
        .and_then(|agent| agent.binary.as_deref())
}

/// What updating one CLI comes to before anything runs: what it is already, or the updater
/// to run.
enum Step {
    Outcome(UpdateOutcome),
    /// Installed, with no updater Argonauts Code knows.
    NoUpdater(PathBuf),
    /// Installed, and its updater can't be found.
    Missing {
        path: PathBuf,
        why: String,
    },
    Run {
        path: PathBuf,
        updater: PathBuf,
        args: Vec<String>,
    },
}

fn step(adapter: &dyn Adapter, binary: Option<&Path>) -> Step {
    let path = match find_binary(adapter, binary) {
        Ok(path) => path,
        Err(BinaryProblem::NotInstalled(_)) => {
            return Step::Outcome(UpdateOutcome::NotInstalled {
                install: adapter.setup_hints().install,
            });
        }
        Err(problem) => {
            return Step::Outcome(UpdateOutcome::Failed {
                version: None,
                why: problem.to_string(),
            });
        }
    };
    let source = install_source(&path, &adapter.install_sources());
    let Some((program, args)) = update_program(adapter, source.as_ref()) else {
        return Step::NoUpdater(path);
    };
    // Homebrew and npm update what they installed. Anything else updates itself.
    if program.is_empty() {
        return Step::Run {
            updater: path.clone(),
            path,
            args,
        };
    }
    match find_unsandboxed(&program, None) {
        Ok(updater) => Step::Run {
            path,
            updater,
            args,
        },
        Err(err) => Step::Missing {
            path,
            why: err.to_string(),
        },
    }
}

async fn take(
    adapter: &dyn Adapter,
    step: &Step,
    limits: UpdateLimits,
    stop: &CancellationToken,
) -> UpdateOutcome {
    match step {
        Step::Outcome(outcome) => outcome.clone(),
        Step::NoUpdater(path) => UpdateOutcome::NoUpdater {
            version: version_at(adapter, path).await,
        },
        Step::Missing { path, why } => UpdateOutcome::Failed {
            version: version_at(adapter, path).await,
            why: why.clone(),
        },
        Step::Run {
            path,
            updater,
            args,
        } => update_with(adapter, path, updater, args, limits, stop).await,
    }
}

/// A CLI left alone once the update was stopped, with its version as it stands.
async fn stopped(adapter: &dyn Adapter, step: &Step) -> UpdateOutcome {
    match step {
        Step::Outcome(outcome) => outcome.clone(),
        Step::NoUpdater(path) | Step::Missing { path, .. } | Step::Run { path, .. } => {
            UpdateOutcome::Stopped {
                version: version_at(adapter, path).await,
            }
        }
    }
}

async fn version_at(adapter: &dyn Adapter, path: &Path) -> Option<String> {
    read_version(path, adapter.version_args(), VERSION_TIMEOUT)
        .await
        .ok()
        .and_then(|output| adapter.parse_version(&output))
}

/// Runs `updater` with `args` and says what it did to the CLI at `path`, from that CLI's
/// version before and after.
async fn update_with(
    adapter: &dyn Adapter,
    path: &Path,
    updater: &Path,
    args: &[String],
    limits: UpdateLimits,
    stop: &CancellationToken,
) -> UpdateOutcome {
    let before = version_at(adapter, path).await;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer =
        command::ask_while_it_talks(updater, &args, limits.timeout, limits.quiet, stop).await;
    let answer = match answer {
        Ok(answer) if answer.success => answer,
        Ok(answer) => {
            return UpdateOutcome::Failed {
                version: before,
                why: last_line(&answer.text).to_owned(),
            };
        }
        Err(command::Unanswered::Failed(why)) => {
            return UpdateOutcome::Failed {
                version: before,
                why,
            };
        }
        Err(command::Unanswered::Stopped) => return UpdateOutcome::Stopped { version: before },
    };
    match (before, version_at(adapter, path).await) {
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

/// One line per CLI, names and versions in columns sized from the rows:
/// `Claude Code       2.1.292 →  2.1.295`, `Codex             0.160.0   up to date`.
pub fn update_lines(updates: &[CliUpdate]) -> Vec<String> {
    update_lines_in(UpdateColumns::from_rows(updates), updates)
}

/// [`update_lines`] in `columns` sized before the rows came, at least as wide as each row
/// needs. A second way into a CLI follows its row: `· also through ACP`.
pub fn update_lines_in(columns: UpdateColumns, updates: &[CliUpdate]) -> Vec<String> {
    let rows = UpdateColumns::from_rows(updates);
    let name_width = columns.name.max(rows.name);
    let version_width = columns.version.max(rows.version);
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
                    format!("{name}   {version}   has no updater Argonauts Code knows")
                }
                UpdateOutcome::UpToDate { .. } => format!("{name}   {version}   up to date"),
                // The arrow takes two cells in some fonts, so two spaces keep it off the
                // version after it.
                UpdateOutcome::Updated { to, .. } => format!("{name}   {version} →  {to}"),
                UpdateOutcome::Failed { why, .. } => {
                    format!("{name}   {version}   couldn't update: {why}")
                }
                UpdateOutcome::Stopped { .. } => format!("{name}   {version}   stopped"),
            };
            let mut line = line.trim_end().to_owned();
            for way in &update.also_through {
                line.push_str(&format!(" · also through {way}"));
            }
            line
        })
        .collect()
}

/// [`update_lines_in`] under a row of another kind in the same columns, such as Argonauts
/// Code's own: its name, its version, then `words` on what became of it.
pub fn update_lines_under(
    first: (&str, &str, &str),
    columns: UpdateColumns,
    updates: &[CliUpdate],
) -> Vec<String> {
    let (name, version, words) = first;
    let columns = columns.with_row(name, version);
    let rows = UpdateColumns::from_rows(updates);
    let name_width = columns.name.max(rows.name);
    let version_width = columns.version.max(rows.version);
    let mut lines = vec![format!(
        "{name:name_width$}   {version:version_width$}   {words}"
    )];
    lines.extend(update_lines_in(columns, updates));
    lines
}

fn before(outcome: &UpdateOutcome) -> Option<&str> {
    match outcome {
        UpdateOutcome::NotInstalled { .. } => None,
        UpdateOutcome::NoUpdater { version }
        | UpdateOutcome::Failed { version, .. }
        | UpdateOutcome::Stopped { version } => version.as_deref(),
        UpdateOutcome::UpToDate { version } => Some(version),
        UpdateOutcome::Updated { from, .. } => from.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doctor::tests::FakeCli;

    /// A stop that never comes.
    fn never() -> CancellationToken {
        CancellationToken::new()
    }

    /// A CLI whose version lives in a file its updater rewrites.
    fn updating(id: &'static str, dir: &std::path::Path, update: &str) -> Arc<dyn Adapter> {
        let version = dir.join(format!("{id}-version"));
        std::fs::write(&version, "1.0.0\n").unwrap();
        let body = format!(
            "case \"$1\" in\n  --version) cat '{}' ;;\n  update) {update} ;;\nesac",
            version.display()
        );
        let body = body.replace("$VERSION", &version.display().to_string());
        Arc::new(FakeCli::script(id, dir, &body, &["1.0.0"]).with_update(&["update"]))
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
            Arc::new(FakeCli::script("plain", dir.path(), "echo 3.1.4", &[])) as Arc<dyn Adapter>,
            Arc::new(FakeCli::named("absent")) as Arc<dyn Adapter>,
        ];
        let updates = update_clis(&adapters, &Settings::default(), VERSION_TIMEOUT, &never()).await;
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

    /// An install from Homebrew or npm is updated by that program, never by the CLI itself.
    #[tokio::test]
    async fn another_program_updates_the_cli_it_installed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let adapter = updating("installed", dir.path(), "echo 'the wrong updater'; exit 1");
        let version = dir.path().join("installed-version");
        let installer = dir.path().join("installer");
        let body = format!(
            "#!/bin/sh\n[ \"$1 $2\" = 'upgrade installed' ] || exit 2\necho 2.0.0 > '{}'\n",
            version.display()
        );
        std::fs::write(&installer, body).unwrap();
        std::fs::set_permissions(&installer, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cli = find_binary(adapter.as_ref(), None).ok().unwrap();
        let args = ["upgrade".to_owned(), "installed".to_owned()];
        let limits = UpdateLimits::within(VERSION_TIMEOUT);
        let outcome =
            update_with(adapter.as_ref(), &cli, &installer, &args, limits, &never()).await;
        assert_eq!(
            outcome,
            UpdateOutcome::Updated {
                from: Some("1.0.0".into()),
                to: "2.0.0".into()
            }
        );
        // The same again changes nothing, and says so.
        let outcome =
            update_with(adapter.as_ref(), &cli, &installer, &args, limits, &never()).await;
        assert_eq!(
            outcome,
            UpdateOutcome::UpToDate {
                version: "2.0.0".into()
            }
        );
    }

    /// OpenCode over ACP runs OpenCode's own program, so OpenCode is updated once and its row
    /// says the second way in. The columns are set before any CLI is done, so the first row
    /// is already as wide as the last.
    #[tokio::test]
    async fn a_second_way_into_a_cli_is_its_row_and_rows_keep_their_columns() {
        let dir = tempfile::tempdir().unwrap();
        let opencode = FakeCli::script("opencode", dir.path(), "echo 2.0.32", &[]);
        let acp = FakeCli::named("opencode-acp").acp().running(&opencode);
        let adapters: Vec<Arc<dyn Adapter>> = vec![
            Arc::new(FakeCli::script("codex", dir.path(), "echo 0.163.0", &[])),
            Arc::new(acp),
            Arc::new(opencode),
            Arc::new(FakeCli::script(
                "antigravity-cli",
                dir.path(),
                "echo 1.4.2",
                &[],
            )),
        ];
        let settings = Settings::default();
        let clis = each_once(&adapters, &settings);
        let ids: Vec<&str> = clis.iter().map(|cli| cli.adapter.id()).collect();
        assert_eq!(ids, ["codex", "opencode", "antigravity-cli"]);
        assert_eq!(clis[1].also_through, ["ACP"]);

        let columns = UpdateColumns::of(&clis, &settings).await;
        let mut seen = Vec::new();
        let limits = UpdateLimits::within(VERSION_TIMEOUT);
        let updates = update_each(&clis, &settings, limits, &never(), |so_far| {
            seen.push(update_lines_in(columns, so_far));
        })
        .await;
        let rows = update_lines_in(columns, &updates);
        assert_eq!(
            rows,
            [
                "codex             0.163.0   has no updater Argonauts Code knows",
                "opencode          2.0.32    has no updater Argonauts Code knows · also through ACP",
                "antigravity-cli   1.4.2     has no updater Argonauts Code knows",
            ]
        );
        // The rows come from the first on, each time in the same columns.
        assert_eq!(seen.last(), Some(&rows));
        for so_far in &seen {
            assert_eq!(so_far[..], rows[..so_far.len()]);
        }
    }

    /// The ACP profile names the path `PATH` gave its program, the built-in adapter only its
    /// name, and they're still one OpenCode.
    #[test]
    fn a_name_and_the_path_it_stands_for_are_one_cli() {
        let dir = tempfile::tempdir().unwrap();
        let opencode = FakeCli::script("opencode", dir.path(), "echo 2.0.32", &[]);
        let path = PathBuf::from(opencode.binary());
        let acp = FakeCli::named("opencode-acp").acp().running(&opencode);
        let builtin = opencode.with_binary("opencode");
        let adapters: Vec<Arc<dyn Adapter>> = vec![Arc::new(builtin), Arc::new(acp)];
        let found = |named: &Path| {
            Some(if named == Path::new("opencode") {
                path.clone()
            } else {
                named.to_path_buf()
            })
        };
        let clis = each_once_found(&adapters, &Settings::default(), &found);
        let ids: Vec<&str> = clis.iter().map(|cli| cli.adapter.id()).collect();
        assert_eq!(ids, ["opencode"]);
        assert_eq!(clis[0].also_through, ["ACP"]);
    }

    /// The process `pid` is gone, within a few seconds of its group being ended.
    async fn ends(pid: i32) -> bool {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        for _ in 0..150 {
            if kill(Pid::from_raw(pid), None).is_err() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// An updater that starts a child of its own, writes the child's id to `pid`, and waits.
    fn waits_with_a_child(id: &'static str, dir: &Path, pid: &Path) -> Arc<dyn Adapter> {
        let body = format!(
            "sleep 30 & echo $! > '{}'; echo downloading; wait",
            pid.display()
        );
        updating(id, dir, &body)
    }

    async fn written(path: &Path) -> i32 {
        loop {
            if let Some(pid) = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Once the stop comes, the updater at work ends with the child it started, a CLI done
    /// before it keeps its row, and the rest read stopped with the version they kept.
    #[tokio::test]
    async fn a_stopped_update_ends_its_updaters_and_keeps_the_rows_so_far() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("child.pid");
        let adapters = vec![
            updating("quick", dir.path(), "echo Already up to date"),
            waits_with_a_child("slow", dir.path(), &pid),
        ];
        let settings = Settings::default();
        let clis = each_once(&adapters, &settings);
        let limits = UpdateLimits::within(Duration::from_secs(60));
        let stop = CancellationToken::new();
        let began = std::time::Instant::now();
        // The stop comes once the slow updater has its child and the quick one is done, so
        // the quick one's row is one done before it, however busy the machine.
        let quick_done = tokio::sync::Notify::new();
        let stopper = async {
            let child = written(&pid).await;
            quick_done.notified().await;
            stop.cancel();
            child
        };
        let (updates, child) = tokio::join!(
            update_each(&clis, &settings, limits, &stop, |done| {
                if !done.is_empty() {
                    quick_done.notify_one();
                }
            }),
            stopper
        );
        assert!(
            began.elapsed() < Duration::from_secs(10),
            "{:?}",
            began.elapsed()
        );
        let outcomes: Vec<&UpdateOutcome> = updates.iter().map(|update| &update.outcome).collect();
        assert_eq!(
            outcomes,
            [
                &UpdateOutcome::UpToDate {
                    version: "1.0.0".into()
                },
                &UpdateOutcome::Stopped {
                    version: Some("1.0.0".into())
                },
            ]
        );
        assert!(
            ends(child).await,
            "the updater's own child {child} was ended"
        );
        assert_eq!(
            update_lines(&updates)[1],
            "slow    1.0.0   stopped",
            "{updates:?}"
        );

        // A stop before anything starts leaves every CLI as it was.
        let updates = update_each(&clis, &settings, limits, &stop, |_| {}).await;
        assert!(
            updates.iter().all(|update| update.outcome
                == UpdateOutcome::Stopped {
                    version: Some("1.0.0".into())
                }),
            "{updates:?}"
        );
    }

    /// An update left behind, as one is when the app quits midway, ends its updater's whole
    /// process group, not only the updater.
    #[tokio::test]
    async fn a_dropped_update_ends_its_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("child.pid");
        let adapter = waits_with_a_child("dropped", dir.path(), &pid);
        let cli = find_binary(adapter.as_ref(), None).ok().unwrap();
        let limits = UpdateLimits::within(Duration::from_secs(60));
        let args = ["update".to_owned()];
        let stop = never();
        let update = update_with(adapter.as_ref(), &cli, &cli, &args, limits, &stop);
        let child = tokio::select! {
            outcome = update => panic!("the update ended on its own: {outcome:?}"),
            child = written(&pid) => child,
        };
        assert!(
            ends(child).await,
            "the updater's own child {child} was ended"
        );
    }

    /// An updater that prints nothing for its quiet limit is taken to hang, and stopped long
    /// before its whole time is up, with the version it left.
    #[tokio::test]
    async fn an_updater_that_says_nothing_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = updating("silent", dir.path(), "sleep 30");
        let cli = find_binary(adapter.as_ref(), None).ok().unwrap();
        let limits = UpdateLimits {
            timeout: Duration::from_secs(20),
            quiet: Duration::from_millis(300),
        };
        let began = std::time::Instant::now();
        let outcome = update_with(
            adapter.as_ref(),
            &cli,
            &cli,
            &["update".to_owned()],
            limits,
            &never(),
        )
        .await;
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        let UpdateOutcome::Failed { version, why } = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(version.as_deref(), Some("1.0.0"));
        assert!(why.contains("printed nothing for 0s"), "{why}");

        // One that keeps saying something has its whole time.
        let talking = updating(
            "talking",
            dir.path(),
            "for i in 1 2 3 4 5; do echo working; sleep 0.1; done; echo 2.0.0 > '$VERSION'",
        );
        let cli = find_binary(talking.as_ref(), None).ok().unwrap();
        let limits = UpdateLimits {
            timeout: Duration::from_secs(20),
            quiet: Duration::from_millis(300),
        };
        let outcome = update_with(
            talking.as_ref(),
            &cli,
            &cli,
            &["update".to_owned()],
            limits,
            &never(),
        )
        .await;
        assert_eq!(
            outcome,
            UpdateOutcome::Updated {
                from: Some("1.0.0".into()),
                to: "2.0.0".into()
            }
        );
    }

    /// Each CLI's own updater runs beside the others, so two slow ones take the time of one,
    /// and the rows still come in their order.
    #[tokio::test]
    async fn updaters_run_side_by_side_and_rows_keep_their_order() {
        let dir = tempfile::tempdir().unwrap();
        let slow = "sleep 1; echo 2.0.0 > '$VERSION'; echo Updated";
        let adapters = vec![
            updating("first", dir.path(), slow),
            updating("second", dir.path(), slow),
            updating("third", dir.path(), "echo Already up to date"),
        ];
        let settings = Settings::default();
        let clis = each_once(&adapters, &settings);
        let began = std::time::Instant::now();
        let mut seen = Vec::new();
        let updates = update_each(
            &clis,
            &settings,
            UpdateLimits::within(VERSION_TIMEOUT),
            &never(),
            |so_far| {
                seen.push(
                    so_far
                        .iter()
                        .map(|update| update.name.clone())
                        .collect::<Vec<_>>(),
                );
            },
        )
        .await;
        assert!(
            began.elapsed() < Duration::from_millis(1_900),
            "{:?}",
            began.elapsed()
        );
        let names: Vec<&str> = updates.iter().map(|update| update.name.as_str()).collect();
        assert_eq!(names, ["first", "second", "third"]);
        assert_eq!(seen.last().map(Vec::len), Some(3));
        assert!(
            seen.iter()
                .all(|so_far| so_far[..] == ["first", "second", "third"][..so_far.len()])
        );
    }

    #[test]
    fn a_row_of_another_kind_goes_first_in_the_same_columns() {
        let codex = CliUpdate {
            name: "Codex".to_owned(),
            outcome: UpdateOutcome::UpToDate {
                version: "0.163.0".into(),
            },
            also_through: Vec::new(),
        };
        assert_eq!(
            update_lines_under(
                ("Argonauts Code", "0.1.0", "up to date"),
                UpdateColumns::default(),
                &[codex]
            ),
            [
                "Argonauts Code   0.1.0     up to date",
                "Codex            0.163.0   up to date"
            ]
        );
    }

    #[test]
    fn lines_put_names_and_versions_in_columns() {
        let update = |name: &str, outcome| CliUpdate {
            name: name.to_owned(),
            outcome,
            also_through: Vec::new(),
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
                "agy",
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
                "Claude Code   2.1.292 →  2.1.295",
                "Codex         0.160.0   up to date",
                "agy           1.3.0   →  1.3.2",
                "OpenCode      not installed · npm i -g opencode-ai@latest",
                "Hermes        1.0.0     couldn't update: no network",
            ]
        );
    }
}
