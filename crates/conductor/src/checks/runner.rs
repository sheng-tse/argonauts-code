//! The `CheckRunner` for crew runs. Each check runs inside a strict profile (no network, writes to
//! the worktree and temp only) with `checks.timeout`, and its process group is killed at the end.
//! A task's first check runs `checks.setup` in that worktree first, once.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use argonauts_core::events::task::{CheckKind, CheckOutcome};
use argonauts_core::future::BoxFuture;
use argonauts_core::limits::{CheckRequest, CheckRunner};
use argonauts_core::session_runner::{Access, SandboxMode, SandboxProfile};
use argonauts_core::settings::Settings;
use argonauts_core::{CommandSpec, Paths};
use tokio::time::Instant;

use super::env::check_env;
use super::setup::{copy_untracked, main_checkout, run_setup_command};
use super::summary::one_line_summary;
use super::{CheckProcess, Ran, execute};
use crate::control::StopHandle;
use crate::run_host::millis;
use crate::sandbox::selftest::self_test_once;
use crate::sandbox::{ProfileInput, SandboxWrap, session_profile};

/// The checks cell with `checks.run` empty, rather than a silent pass.
pub const NO_COMMAND_CHECKS: &str = "No command checks set · /config checks.run";

/// Used when neither the request nor `checks.timeout` gives a usable time limit.
const FALLBACK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The outcome to show when `checks.run` is empty: skipped, which fails nothing, and says so
/// rather than passing silently; merge rules that need the suite check for commands themselves.
pub fn no_command_checks() -> CheckOutcome {
    CheckOutcome {
        name: "checks.run".to_owned(),
        kind: CheckKind::Command,
        passed: true,
        command: None,
        exit_code: None,
        duration_ms: 0,
        summary: NO_COMMAND_CHECKS.to_owned(),
        output_tail: String::new(),
        violations: Vec::new(),
        sandboxed: false,
        skipped: true,
    }
}

pub struct SandboxedChecks {
    settings: Arc<Settings>,
    sandbox: Arc<dyn SandboxWrap>,
    /// Setup commands that already ran, by worktree.
    set_up: Mutex<HashSet<(PathBuf, Vec<String>)>>,
    /// Worktrees that already got their `checks.copy_untracked` files.
    copied: Mutex<HashSet<PathBuf>>,
}

impl SandboxedChecks {
    pub fn new(settings: Arc<Settings>, sandbox: Arc<dyn SandboxWrap>) -> Self {
        Self {
            settings,
            sandbox,
            set_up: Mutex::new(HashSet::new()),
            copied: Mutex::new(HashSet::new()),
        }
    }

    async fn run_request(&self, request: CheckRequest) -> CheckOutcome {
        let started = Instant::now();
        if request.command.is_empty() {
            return finished(&request, started, false, Err("the command is empty".into()));
        }
        if let Err(problem) = self.copy_untracked_once(&request.workdir).await {
            return finished(&request, started, false, Err(problem));
        }
        if request.setup {
            self.run_setup(request, started).await
        } else {
            self.run_sandboxed(request, started).await
        }
    }

    /// Setup runs as Argonauts Code, outside the sandbox and with network, since it installs
    /// dependencies. It still gets only the check environment.
    async fn run_setup(&self, request: CheckRequest, started: Instant) -> CheckOutcome {
        let key = (request.workdir.clone(), request.command.clone());
        let first = lock(&self.set_up).insert(key.clone());
        if !first {
            return CheckOutcome {
                passed: true,
                summary: "setup already ran in this worktree, skipped".to_owned(),
                ..finished(&request, started, false, Err(String::new()))
            };
        }
        let result = match CheckTemp::create() {
            Ok(temp) => {
                let env = check_env(run_host_vars(), temp.root());
                run_setup_command(
                    &request.command,
                    &request.workdir,
                    &env,
                    self.timeout(&request),
                )
                .await
                .map_err(|err| format!("could not run the setup command: {err}"))
            }
            Err(problem) => Err(problem),
        };
        let outcome = finished(&request, started, false, result);
        if !outcome.passed {
            // A failed setup may be fixed and asked for again.
            lock(&self.set_up).remove(&key);
        }
        outcome
    }

    async fn run_sandboxed(&self, request: CheckRequest, started: Instant) -> CheckOutcome {
        let wrapped = self.sandbox.mode() != SandboxMode::None;
        if wrapped {
            let test = self_test_once(self.sandbox.as_ref()).await;
            if !test.ok {
                let problem = format!(
                    "the sandbox doesn't work, so checks can't run: {}",
                    test.detail
                );
                return finished(&request, started, false, Err(problem));
            }
        }
        let temp = match CheckTemp::create() {
            Ok(temp) => temp,
            Err(problem) => return finished(&request, started, false, Err(problem)),
        };
        let workdir = request
            .workdir
            .canonicalize()
            .unwrap_or_else(|_| request.workdir.clone());
        let env = check_env(run_host_vars(), temp.root());
        let mut spec = CommandSpec::new(request.command[0].clone(), &workdir)
            .args(request.command[1..].iter().cloned());
        spec.env_set = env;
        let profile = self.check_profile(&request.sandbox, &workdir, temp.root());
        let spec = match self.sandbox.wrap(spec, &profile) {
            Ok(spec) => spec,
            Err(err) => {
                let problem = format!("could not sandbox the check, so it didn't run: {err}");
                return finished(&request, started, false, Err(problem));
            }
        };
        let result = run_spec(&spec, self.timeout(&request)).await;
        finished(&request, started, wrapped, result)
    }

    /// A build session's profile with the writes narrowed to the worktree and the check's own
    /// temp directory, and no network.
    fn check_profile(
        &self,
        requested: &SandboxProfile,
        workdir: &Path,
        temp: &Path,
    ) -> SandboxProfile {
        let user_home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let argonauts_home = Paths::from_env()
            .map(|paths| paths.home().to_path_buf())
            .unwrap_or_else(|_| user_home.join(".argonauts"));
        let input = ProfileInput {
            access: Access::Build,
            agent_id: "",
            workdir,
            session_dir: temp,
            argonauts_home: &argonauts_home,
            user_home: &user_home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_worktrees: false,
            commits: false,
        };
        let mut profile = session_profile(requested, &input);
        profile.mode = self.sandbox.mode();
        profile.writable = vec![workdir.to_path_buf(), temp.to_path_buf()];
        // Not even the proxy: a check's command needs no model API.
        profile.network = false;
        profile.proxy_port = None;
        profile
    }

    async fn copy_untracked_once(&self, worktree: &Path) -> Result<(), String> {
        let globs = self.settings.checks.copy_untracked.clone();
        if globs.is_empty() || lock(&self.copied).contains(worktree) {
            return Ok(());
        }
        let target = worktree.to_path_buf();
        let copying = tokio::task::spawn_blocking(move || {
            let main = main_checkout(&target)?;
            if target.canonicalize().is_ok_and(|target| target == main) {
                return Ok(Vec::new());
            }
            copy_untracked(&main, &target, &globs)
        });
        match copying.await {
            Ok(Ok(_)) => {
                lock(&self.copied).insert(worktree.to_path_buf());
                Ok(())
            }
            Ok(Err(err)) => Err(format!("could not copy untracked files: {err}")),
            Err(err) => Err(format!("could not copy untracked files: {err}")),
        }
    }

    fn timeout(&self, request: &CheckRequest) -> Duration {
        if !request.timeout.is_zero() {
            return request.timeout;
        }
        match self.settings.checks.timeout.parse() {
            Ok(Some(timeout)) if !timeout.is_zero() => timeout,
            _ => FALLBACK_TIMEOUT,
        }
    }
}

impl CheckRunner for SandboxedChecks {
    fn run(&self, request: CheckRequest) -> BoxFuture<'_, CheckOutcome> {
        Box::pin(self.run_request(request))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Argonauts Code's own variables that are valid text; the allowlist picks from these.
fn run_host_vars() -> impl Iterator<Item = (String, String)> {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
}

async fn run_spec(spec: &CommandSpec, timeout: Duration) -> Result<Ran, String> {
    for file in &spec.files {
        if let Some(parent) = file.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("could not write {}: {err}", file.path.display()))?;
        }
        std::fs::write(&file.path, &file.contents)
            .map_err(|err| format!("could not write {}: {err}", file.path.display()))?;
    }
    let mut argv = Vec::with_capacity(spec.args.len() + 1);
    argv.push(spec.program.clone());
    argv.extend(spec.args.iter().cloned());
    let env = spec
        .env_set
        .iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    let process = CheckProcess::new(&argv, &spec.cwd, env)
        .map_err(|err| format!("could not run the checks: {err}"))?;
    execute(&process, timeout, &StopHandle::new())
        .await
        .map_err(|err| format!("could not run the checks: {err}"))
}

fn finished(
    request: &CheckRequest,
    started: Instant,
    sandboxed: bool,
    result: Result<Ran, String>,
) -> CheckOutcome {
    let (passed, exit_code, summary, output_tail) = match result {
        Ok(ran) => (
            ran.passed(),
            ran.exit_code,
            summarize(&ran),
            ran.output_with_note(),
        ),
        Err(problem) => (false, None, problem, String::new()),
    };
    CheckOutcome {
        name: request.name.clone(),
        kind: CheckKind::Command,
        passed,
        command: Some(request.command.clone()),
        exit_code,
        duration_ms: millis(started.elapsed()),
        summary,
        output_tail,
        violations: Vec::new(),
        sandboxed,
        skipped: false,
    }
}

fn summarize(ran: &Ran) -> String {
    if let Some(why) = &ran.ended_early {
        return why.clone();
    }
    if let Some(line) = one_line_summary(&ran.output) {
        return line;
    }
    match ran.exit_code {
        Some(0) => "passed".to_owned(),
        Some(code) => format!("failed with exit code {code}"),
        None => "ended by a signal".to_owned(),
    }
}

/// A fresh directory per check, holding its `HOME` and `TMPDIR`, removed when dropped.
struct CheckTemp {
    root: PathBuf,
}

impl CheckTemp {
    fn create() -> Result<Self, String> {
        let base = std::env::temp_dir();
        let base = base.canonicalize().unwrap_or(base);
        let root = base.join(format!("argonauts-check-{}", uuid::Uuid::new_v4().simple()));
        for dir in [root.join("home"), root.join("tmp")] {
            std::fs::create_dir_all(&dir).map_err(|err| {
                format!(
                    "could not create the check's temp directory {}: {err}",
                    dir.display()
                )
            })?;
        }
        Ok(Self { root })
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for CheckTemp {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.root) {
            tracing::warn!(
                "could not remove the check's temp directory {}: {err}",
                self.root.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Unsandboxed;
    use argonauts_core::settings::protocol::CheckSettings;
    use argonauts_core::{RunId, TaskId};
    use argonauts_testkit::repo::TempRepo;

    fn checks(settings: Settings) -> SandboxedChecks {
        SandboxedChecks::new(Arc::new(settings), Arc::new(Unsandboxed))
    }

    fn request(workdir: &Path, script: &str) -> CheckRequest {
        CheckRequest {
            run: RunId::generate(),
            task: TaskId::numbered(1),
            name: "test".to_owned(),
            command: vec!["/bin/sh".into(), "-c".into(), script.into()],
            workdir: workdir.to_path_buf(),
            sandbox: SandboxProfile::default(),
            timeout: Duration::from_secs(20),
            setup: false,
        }
    }

    fn setup_request(workdir: &Path, script: &str) -> CheckRequest {
        CheckRequest {
            setup: true,
            ..request(workdir, script)
        }
    }

    fn worktree(repo: &TempRepo, name: &str) -> PathBuf {
        let path = repo.scratch().join(name);
        repo.git(&["worktree", "add", "--detach", &path.display().to_string()])
            .unwrap();
        path
    }

    #[tokio::test]
    async fn checks_setup_runs_once_per_worktree() {
        let repo = TempRepo::new().unwrap();
        repo.write(".gitignore", ".env.test\n").unwrap();
        repo.commit_all("ignore env").unwrap();
        repo.write(".env.test", "PORT=1").unwrap();
        repo.write("scratch.txt", "not copied").unwrap();
        let first = worktree(&repo, "wt-1");
        let second = worktree(&repo, "wt-2");
        let settings = Settings {
            checks: CheckSettings {
                copy_untracked: vec![".env.test".to_owned()],
                ..CheckSettings::default()
            },
            ..Settings::default()
        };
        let checks = checks(settings);
        let install = "echo installed >> setup.log; echo \"$HOME\" > home.txt; test \"$CI\" = 1";

        let outcome = checks.run(setup_request(&first, install)).await;
        assert!(outcome.passed, "{outcome:?}");
        assert!(!outcome.sandboxed);
        assert_eq!(outcome.summary, "passed");
        assert!(first.join(".env.test").exists());
        assert!(!first.join("scratch.txt").exists());
        let home = std::fs::read_to_string(first.join("home.txt")).unwrap();
        assert!(home.contains("argonauts-check-"), "{home}");
        assert!(!Path::new(home.trim()).exists(), "the temp home is removed");

        let again = checks.run(setup_request(&first, install)).await;
        assert!(again.passed);
        assert!(again.summary.contains("already ran"), "{}", again.summary);
        assert_eq!(
            std::fs::read_to_string(first.join("setup.log")).unwrap(),
            "installed\n"
        );

        let other = checks.run(setup_request(&second, install)).await;
        assert!(other.passed);
        assert_eq!(
            std::fs::read_to_string(second.join("setup.log")).unwrap(),
            "installed\n"
        );
        assert!(second.join(".env.test").exists());

        // A failed setup may run again once fixed.
        let failing = "echo try >> tries.log; exit 1";
        let failed = checks.run(setup_request(&first, failing)).await;
        assert!(!failed.passed);
        assert_eq!(failed.summary, "failed with exit code 1");
        let retried = checks.run(setup_request(&first, failing)).await;
        assert!(!retried.summary.contains("already ran"));
        assert_eq!(
            std::fs::read_to_string(first.join("tries.log")).unwrap(),
            "try\ntry\n"
        );
    }

    #[tokio::test]
    async fn empty_checks_run_says_so() {
        let outcome = no_command_checks();
        assert_eq!(outcome.summary, NO_COMMAND_CHECKS);
        assert_eq!(
            outcome.summary,
            "No command checks set · /config checks.run"
        );
        assert!(outcome.passed);
        assert_eq!(outcome.command, None);
        assert_eq!(outcome.kind, CheckKind::Command);

        let dir = tempfile::tempdir().unwrap();
        let mut empty = request(dir.path(), "");
        empty.command.clear();
        let outcome = checks(Settings::default()).run(empty).await;
        assert!(!outcome.passed);
        assert_eq!(outcome.summary, "the command is empty");
    }

    #[tokio::test]
    async fn check_timeout_kills_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut slow = request(
            dir.path(),
            "sleep 30 & echo $! > bg.pid; echo started; sleep 30",
        );
        slow.timeout = Duration::from_millis(500);
        let started = std::time::Instant::now();
        let outcome = checks(Settings::default()).run(slow).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!outcome.passed);
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.summary, "timed out after 500ms");
        assert!(outcome.output_tail.starts_with("started\n"));
        assert!(
            outcome
                .output_tail
                .ends_with("[argonauts] check timed out after 500ms")
        );

        let pid: i32 = std::fs::read_to_string(dir.path().join("bg.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(pid);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while nix::sys::signal::kill(pid, None).is_ok() {
            assert!(
                std::time::Instant::now() < deadline,
                "the check's background child survived"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn checks_summarize_and_keep_a_tail() {
        let dir = tempfile::tempdir().unwrap();
        let cargo =
            "echo 'test result: FAILED. 41 passed; 1 failed; 0 ignored; 0 measured'; exit 101";
        let outcome = checks(Settings::default())
            .run(request(dir.path(), cargo))
            .await;
        assert!(!outcome.passed);
        assert_eq!(outcome.exit_code, Some(101));
        assert_eq!(outcome.summary, "41 passed, 1 failed");
        assert!(!outcome.sandboxed);

        let loud = "i=0; while [ $i -lt 3000 ]; do echo line-$i-xxxxxxxxxxxx; i=$((i+1)); done";
        let outcome = checks(Settings::default())
            .run(request(dir.path(), loud))
            .await;
        assert!(outcome.passed);
        assert!(outcome.output_tail.len() <= super::super::OUTPUT_TAIL_BYTES + 4);
        assert!(outcome.output_tail.ends_with("line-2999-xxxxxxxxxxxx\n"));
    }

    #[test]
    fn a_check_gets_no_network_even_from_a_proxied_session() {
        let dir = tempfile::tempdir().unwrap();
        for network in [false, true] {
            let session = SandboxProfile {
                network,
                proxy_port: Some(4128),
                ..SandboxProfile::default()
            };
            let profile =
                checks(Settings::default()).check_profile(&session, dir.path(), dir.path());
            assert!(!profile.network);
            assert_eq!(profile.proxy_port, None);
        }
    }

    #[tokio::test]
    async fn a_missing_sandbox_fails_closed() {
        struct Broken;
        impl SandboxWrap for Broken {
            fn mode(&self) -> SandboxMode {
                SandboxMode::Container
            }
            fn wrap(
                &self,
                _command: CommandSpec,
                _profile: &SandboxProfile,
            ) -> Result<CommandSpec, crate::sandbox::SandboxError> {
                Err(crate::sandbox::SandboxError::Unavailable(
                    "no docker".into(),
                ))
            }
            fn describe(&self, _profile: &SandboxProfile) -> String {
                "broken".into()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let checks = SandboxedChecks::new(Arc::new(Settings::default()), Arc::new(Broken));
        let outcome = checks.run(request(dir.path(), "touch ran")).await;
        assert!(!outcome.passed);
        assert!(!outcome.sandboxed);
        assert!(!dir.path().join("ran").exists());
    }

    #[cfg(target_os = "macos")]
    fn os_sandboxed() -> SandboxedChecks {
        use argonauts_core::settings::approvals::SandboxChoice;
        let mut settings = Settings::default();
        settings.sandbox.mode = SandboxChoice::Os;
        let sandbox = crate::sandbox::for_settings(&settings.sandbox);
        SandboxedChecks::new(Arc::new(settings), sandbox)
    }

    /// Waits on the `sandbox` builder's Seatbelt profile and self-test.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn check_write_outside_worktree_fails() {
        let worktree = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let escape = outside.path().join("escape.txt");
        let checks = os_sandboxed();

        let inside = checks
            .run(request(worktree.path(), "echo ok > inside.txt"))
            .await;
        assert!(inside.sandboxed, "{inside:?}");
        assert!(inside.passed, "{inside:?}");

        let script = format!("echo x > '{}'", escape.display());
        let outcome = checks.run(request(worktree.path(), &script)).await;
        assert!(outcome.sandboxed, "{outcome:?}");
        assert!(!outcome.passed, "{outcome:?}");
        assert!(!escape.exists());
    }

    /// Waits on the `sandbox` builder's Seatbelt profile and self-test. Skips where this machine
    /// has no network to begin with.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn check_network_fails() {
        let dir = tempfile::tempdir().unwrap();
        let connect = "/usr/bin/nc -z -G 3 1.1.1.1 443";
        let open = checks(Settings::default())
            .run(request(dir.path(), connect))
            .await;
        if !open.passed {
            eprintln!("no network here, so there's nothing to block: {open:?}");
            return;
        }
        let outcome = os_sandboxed().run(request(dir.path(), connect)).await;
        assert!(outcome.sandboxed, "{outcome:?}");
        assert!(!outcome.passed, "{outcome:?}");
    }
}
