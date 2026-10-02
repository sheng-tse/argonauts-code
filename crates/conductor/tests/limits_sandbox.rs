//! A session can't read what its sandbox denies: the run's approval token, sealed drafts,
//! another session's raw log, other cards' worktrees, and sockets in `~/.argonauts`. Each test
//! runs one probe script twice: through the harness worker, and through the OS sandbox's wrap of
//! the profile the worker builds. That is Seatbelt on macOS and Landlock on Linux, so both draw
//! the same boundary.

#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use argonauts_conductor::harness_worker::HarnessWorker;
use argonauts_conductor::sandbox::{ProfileInput, SandboxWrap, session_profile};
use argonauts_core::layout::{self, run_files, session_files};
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::settings::Settings;
use argonauts_core::settings::approvals::SandboxChoice;
use argonauts_core::worker::{Access, SandboxProfile, Worker};
use argonauts_core::{
    Adapter, AgentEvent, CardId, CommandSpec, Paths, RepoId, RunId, SessionRequest, SpecFile,
    StopPlan, StreamParser,
};
use argonauts_testkit::services::{MemoryHealthBoard, MemoryQuotaBoard};
use argonauts_testkit::worker::{drain, session_spec};

/// `$1` is the results file. Then each check is three arguments: `read` or `write`, a name,
/// and a path. Each check adds `<name>=ok` or `<name>=blocked` to the results.
const PROBE: &str = r#"
out="$1"; shift
: > "$out"
while [ $# -ge 3 ]; do
  case "$1" in
    read) if cat "$3" >/dev/null 2>&1; then r=ok; else r=blocked; fi ;;
    write) if (printf x >> "$3") 2>/dev/null; then r=ok; else r=blocked; fi ;;
    connect)
      if command -v nc >/dev/null 2>&1; then nc -U -w 2 "$3" </dev/null >/dev/null 2>&1
      else python3 -c 'import socket, sys; s = socket.socket(socket.AF_UNIX); s.settimeout(2); s.connect(sys.argv[1])' "$3" 2>/dev/null; fi
      if [ $? -eq 0 ]; then r=ok; else r=blocked; fi ;;
    *) r=unknown ;;
  esac
  echo "$2=$r" >> "$out"
  shift 3
done
"#;

const VENDOR: &str = "probe";

#[cfg(target_os = "macos")]
fn os_sandbox() -> Arc<dyn SandboxWrap> {
    Arc::new(argonauts_conductor::sandbox::seatbelt::Seatbelt::new())
}

/// The launcher the conductor ships for tests, since this test binary isn't `argonauts`.
#[cfg(target_os = "linux")]
fn os_sandbox() -> Arc<dyn SandboxWrap> {
    Arc::new(
        argonauts_conductor::sandbox::landlock::Landlock::with_launcher(env!(
            "CARGO_BIN_EXE_argonauts-sandbox"
        )),
    )
}

struct Check {
    op: &'static str,
    name: &'static str,
    path: PathBuf,
    expected: &'static str,
}

fn check(op: &'static str, name: &'static str, path: PathBuf, expected: &'static str) -> Check {
    Check {
        op,
        name,
        path,
        expected,
    }
}

/// A run with two cards, a base worktree, a sealed draft, and a second session, all under a
/// temporary `ARGONAUTS_HOME`.
struct Layout {
    _root: tempfile::TempDir,
    _results: tempfile::TempDir,
    results: PathBuf,
    user_home: PathBuf,
    paths: Paths,
    run: RunId,
    key: SessionKey,
    run_dir: PathBuf,
    session_dir: PathBuf,
    status: PathBuf,
    base: PathBuf,
    card_a: PathBuf,
    card_b: PathBuf,
}

impl Layout {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let results = tempfile::tempdir().unwrap();
        let top = root.path().canonicalize().unwrap();
        let user_home = top.join("home");
        let paths = Paths::new(user_home.join(".argonauts"));
        let repo = RepoId::new("repo-1").unwrap();
        let run = RunId::new("run-1").unwrap();
        let key = SessionKey::numbered(1);
        let run_dir = paths.run_dir(&repo, &run);
        let session_dir = layout::session_dir(&run_dir, &key);
        let tree = |card: &str| paths.worktree_dir(&repo, &run, &CardId::new(card).unwrap());
        let layout = Self {
            results: results.path().canonicalize().unwrap().join("results.txt"),
            status: session_dir.join(session_files::STATUS),
            base: tree("base"),
            card_a: tree("card-a"),
            card_b: tree("card-b"),
            session_dir,
            run_dir,
            key,
            run,
            paths,
            user_home,
            _results: results,
            _root: root,
        };
        let other_session = layout::session_dir(&layout.run_dir, &SessionKey::numbered(2));
        for (path, contents) in [
            (layout.run_dir.join(run_files::APPROVE_TOKEN), "token"),
            (
                layout.run_dir.join(run_files::DRAFTS).join("d-1.md"),
                "sealed draft",
            ),
            (other_session.join(session_files::RAW_STDOUT), "{\"raw\":1}"),
            (layout.status.clone(), "{}"),
            (layout.base.join("README.md"), "base"),
            (layout.card_a.join("src.rs"), "card a"),
            (layout.card_b.join("hidden_test.rs"), "hidden test"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        layout
    }

    fn workdir(&self, access: Access) -> &Path {
        match access {
            Access::Build | Access::Spike => &self.card_a,
            _ => &self.base,
        }
    }

    fn probe_args(&self, checks: &[Check]) -> Vec<String> {
        let mut args = vec![
            "-c".to_owned(),
            PROBE.to_owned(),
            "sh".to_owned(),
            self.results.to_string_lossy().into_owned(),
        ];
        for check in checks {
            args.push(check.op.to_owned());
            args.push(check.name.to_owned());
            args.push(check.path.to_string_lossy().into_owned());
        }
        args
    }

    fn results(&self) -> HashMap<String, String> {
        std::fs::read_to_string(&self.results)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, result)| (name.to_owned(), result.to_owned()))
            .collect()
    }

    /// Runs the probe under the OS sandbox's wrap of the profile a session with `access` gets.
    fn probe_directly(&self, access: Access, checks: &[Check]) -> HashMap<String, String> {
        let workdir = self.workdir(access);
        let input = ProfileInput {
            access,
            vendor: VENDOR,
            workdir,
            session_dir: &self.session_dir,
            argonauts_home: self.paths.home(),
            user_home: &self.user_home,
            base_worktree: Some(&self.base),
            status_file: Some(&self.status),
            extra_writable: &[],
            sees_other_copies: false,
            commits: false,
        };
        let profile = session_profile(&SandboxProfile::default(), &input);
        let command = CommandSpec::new("/bin/sh", workdir).args(self.probe_args(checks));
        let wrapped = os_sandbox().wrap(command, &profile).unwrap();
        let status = std::process::Command::new(&wrapped.program)
            .args(&wrapped.args)
            .current_dir(&wrapped.cwd)
            .status()
            .unwrap();
        assert!(status.success(), "the probe failed: {status}");
        self.results()
    }

    /// Runs the probe as a harness session.
    async fn probe_through_worker(
        &self,
        access: Access,
        checks: &[Check],
        launch_files: &[SpecFile],
    ) -> HashMap<String, String> {
        let _ = std::fs::remove_file(&self.results);
        let adapter = Arc::new(ProbeAdapter {
            args: self.probe_args(checks),
            files: launch_files.to_vec(),
        });
        let mut settings = Settings::default();
        settings.sandbox.mode = SandboxChoice::Os;
        let worker = HarnessWorker::new(
            adapter,
            WorkerRef::new(VENDOR, Some("tiny".to_owned())),
            VENDOR.to_owned(),
            Arc::new(settings),
            Arc::new(MemoryQuotaBoard::default()),
            Arc::new(MemoryHealthBoard::default()),
        )
        .with_home(self.paths.clone())
        .with_sandbox(os_sandbox());

        let mut spec = session_spec("probe", access, self.workdir(access));
        spec.worker = WorkerRef::new(VENDOR, Some("tiny".to_owned()));
        spec.run = Some(self.run.clone());
        spec.key = self.key.clone();
        spec.session_dir = self.session_dir.clone();
        spec.status_file = Some(self.status.clone());
        if matches!(access, Access::Build | Access::Spike) {
            spec.card = Some(CardId::new("card-a").unwrap());
            spec.goal_sha256 = Some("0".repeat(64));
        }

        let handle = match worker.start(spec).await {
            Ok(handle) => handle,
            Err(err) => panic!("the worker refused the probe session: {err:?}"),
        };
        tokio::time::timeout(Duration::from_secs(60), drain(handle))
            .await
            .expect("the probe session never ended");
        self.results()
    }
}

fn assert_results(route: &str, results: &HashMap<String, String>, checks: &[Check]) {
    for check in checks {
        assert_eq!(
            results.get(check.name).map(String::as_str),
            Some(check.expected),
            "{route}: {} {}",
            check.op,
            check.path.display()
        );
    }
}

/// A harness that is only the probe script, with files its launch writes, as a real adapter's
/// settings or schema.
struct ProbeAdapter {
    args: Vec<String>,
    files: Vec<SpecFile>,
}

struct Silent;

impl StreamParser for Silent {
    fn parse_stdout_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn parse_stderr_line(&mut self, _line: &str) -> Vec<AgentEvent> {
        Vec::new()
    }
    fn finish(&mut self, _exit_code: Option<i32>) -> Vec<AgentEvent> {
        Vec::new()
    }
}

impl Adapter for ProbeAdapter {
    fn id(&self) -> &'static str {
        VENDOR
    }
    fn display_name(&self) -> &'static str {
        "Probe"
    }
    fn binary(&self) -> &'static str {
        "sh"
    }
    fn command(&self, req: &SessionRequest) -> anyhow::Result<CommandSpec> {
        let mut command = CommandSpec::new("/bin/sh", &req.workdir).args(self.args.iter().cloned());
        command.files = self.files.clone();
        Ok(command)
    }
    fn parser(&self) -> Box<dyn StreamParser> {
        Box::new(Silent)
    }
    fn version_args(&self) -> &'static [&'static str] {
        &["--version"]
    }
    fn parse_version(&self, _output: &str) -> Option<String> {
        None
    }
    fn tested_versions(&self) -> &'static [&'static str] {
        &["1.0.0"]
    }
    fn cheapest_model(&self) -> (&'static str, Option<&'static str>) {
        ("tiny", None)
    }
    fn stop_plan(&self) -> StopPlan {
        StopPlan::escalating(Duration::from_secs(1), Duration::from_secs(1))
    }
}

/// A listener that takes each connection and closes it, as the run's control socket would.
fn listen(path: &Path) {
    let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
}

#[tokio::test]
async fn limits_readonly_session_cannot_read_state_or_drafts() {
    let l = Layout::new();
    // Beside the run's own `control.sock`, whose full path in a temp folder is too long to bind.
    let socket = l.paths.home().join("s");
    listen(&socket);
    let checks = [
        check("connect", "argonauts-socket", socket, "blocked"),
        check(
            "read",
            "token",
            l.run_dir.join(run_files::APPROVE_TOKEN),
            "blocked",
        ),
        check("read", "draft", l.run_dir.join("drafts/d-1.md"), "blocked"),
        check(
            "read",
            "other-raw-log",
            layout::session_dir(&l.run_dir, &SessionKey::numbered(2))
                .join(session_files::RAW_STDOUT),
            "blocked",
        ),
        check(
            "read",
            "other-card",
            l.card_b.join("hidden_test.rs"),
            "blocked",
        ),
        check("read", "build-card", l.card_a.join("src.rs"), "blocked"),
        check(
            "write",
            "pending-answers",
            l.run_dir.join(run_files::PENDING_ANSWERS),
            "blocked",
        ),
        check("read", "own-status", l.status.clone(), "ok"),
        check("read", "own-worktree", l.base.join("README.md"), "ok"),
        check(
            "write",
            "own-worktree-write",
            l.base.join("README.md"),
            "blocked",
        ),
    ];

    assert_results(
        "os sandbox",
        &l.probe_directly(Access::ReadOnly, &checks),
        &checks,
    );
    let results = l.probe_through_worker(Access::ReadOnly, &checks, &[]).await;
    assert_results("worker", &results, &checks);
    assert_eq!(
        std::fs::read_to_string(l.base.join("README.md")).unwrap(),
        "base"
    );
}

#[tokio::test]
async fn limits_build_session_cannot_read_other_worktrees() {
    let l = Layout::new();
    let checks = [
        check(
            "read",
            "other-card",
            l.card_b.join("hidden_test.rs"),
            "blocked",
        ),
        check(
            "write",
            "other-card-write",
            l.card_b.join("hidden_test.rs"),
            "blocked",
        ),
        check("read", "base", l.base.join("README.md"), "blocked"),
        check(
            "read",
            "token",
            l.run_dir.join(run_files::APPROVE_TOKEN),
            "blocked",
        ),
        check("read", "own-status", l.status.clone(), "ok"),
        check("read", "own-worktree", l.card_a.join("src.rs"), "ok"),
        check("write", "own-worktree-write", l.card_a.join("new.rs"), "ok"),
    ];

    assert_results(
        "os sandbox",
        &l.probe_directly(Access::Build, &checks),
        &checks,
    );
    let results = l.probe_through_worker(Access::Build, &checks, &[]).await;
    assert_results("worker", &results, &checks);
    assert_eq!(
        std::fs::read_to_string(l.card_b.join("hidden_test.rs")).unwrap(),
        "hidden test"
    );
}

/// What the launch names in its session folder is readable, as Claude Code's `--settings` file
/// is. The session's raw log stays unreadable even when the launch names it.
#[tokio::test]
async fn limits_session_reads_the_files_its_launch_names() {
    let l = Layout::new();
    let settings = l.session_dir.join("claude-settings.json");
    let raw = l.session_dir.join(session_files::RAW_STDOUT);
    let checks = [
        check("read", "launch-file", settings.clone(), "ok"),
        check("write", "launch-file-write", settings.clone(), "blocked"),
        check("read", "own-raw-log", raw, "blocked"),
    ];
    let files = [SpecFile {
        path: settings.clone(),
        contents: "{}".into(),
    }];
    for access in [Access::ReadOnly, Access::Build] {
        let results = l.probe_through_worker(access, &checks, &files).await;
        assert_results("worker", &results, &checks);
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{}");
    }
}
