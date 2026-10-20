//! The sandbox self-test, at startup and in `doctor`: inside a profile with the network on, as
//! CLI sessions have it, a write outside the allowed paths, a read of a denied path, and a
//! connection to a socket in a denied folder must all fail. If any succeeds, CLI sessions are
//! refused (`sandbox.fail_closed`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use argonauts_core::CommandSpec;
use argonauts_core::session_runner::{SandboxMode, SandboxProfile};
use tokio::sync::Mutex;

use super::SandboxWrap;

/// A sandbox that hangs this long counts as broken.
const TIMEOUT: Duration = Duration::from_secs(20);

/// A failure is tried again after this long, since a loaded machine or a full temp folder can
/// fail it once. A pass is kept for the life of the process.
const RETRY_FAILURE_AFTER: Duration = Duration::from_secs(30);

const HOW_TO_FIX: &str =
    "CLI sessions are refused until it passes. Run `argonauts doctor` to see why.";

/// Tries each operation on its argument and prints what happened: `$1` must be writable, `$2`
/// must not be, `$3` must be unreadable, the socket at `$4` must take a connection, and the one
/// at `$5` must not. Sockets are tried with `nc`, or `python3` where a Linux has no `nc`.
const PROBE: &str = r#"
connect() {
  if command -v nc >/dev/null 2>&1; then nc -U -w 2 "$1" </dev/null >/dev/null 2>&1
  else python3 -c 'import socket, sys; s = socket.socket(socket.AF_UNIX); s.settimeout(2); s.connect(sys.argv[1])' "$1" 2>/dev/null; fi
}
if (printf ok > "$1") 2>/dev/null; then echo allowed-write=ok; else echo allowed-write=blocked; fi
if (printf bad > "$2") 2>/dev/null; then echo outside-write=ok; else echo outside-write=blocked; fi
if cat "$3" >/dev/null 2>&1; then echo denied-read=ok; else echo denied-read=blocked; fi
if connect "$4"; then echo allowed-connect=ok; else echo allowed-connect=blocked; fi
if connect "$5"; then echo denied-connect=ok; else echo denied-connect=blocked; fi
"#;

/// A Unix socket path longer than this can't be bound (`sun_path` is 104 bytes on macOS).
const MAX_SOCKET_PATH: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfTest {
    pub ok: bool,
    /// What failed and how to fix it, or what passed.
    pub detail: String,
}

impl SelfTest {
    fn failed(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: format!("{} {HOW_TO_FIX}", detail.into()),
        }
    }
}

/// Runs the test in `scratch`, a temp directory it may create files in.
pub async fn self_test(sandbox: &dyn SandboxWrap, scratch: &Path) -> SelfTest {
    if sandbox.mode() == SandboxMode::None {
        return SelfTest {
            ok: false,
            detail: "no sandbox".to_owned(),
        };
    }
    let probe = match Probe::create(scratch).await {
        Ok(probe) => probe,
        Err(err) => {
            return SelfTest::failed(format!(
                "The sandbox self-test couldn't set up in {}: {err}.",
                scratch.display()
            ));
        }
    };
    let wrapped = match sandbox.wrap(probe.command(), &probe.profile()) {
        Ok(wrapped) => wrapped,
        Err(err) => return SelfTest::failed(format!("The sandbox doesn't work here: {err}.")),
    };
    let stdout = match run(&wrapped).await {
        Ok(stdout) => stdout,
        Err(err) => {
            return SelfTest::failed(format!("The sandbox self-test didn't run: {err}."));
        }
    };
    probe.judge(&stdout, &sandbox.describe(&probe.profile()))
}

/// The self-test for this sandbox, remembered: a pass for the life of the process, a failure
/// for [`RETRY_FAILURE_AFTER`].
pub async fn self_test_once(sandbox: &dyn SandboxWrap) -> SelfTest {
    static RESULTS: OnceLock<Mutex<HashMap<SandboxKey, (SelfTest, Instant)>>> = OnceLock::new();
    // The description tells apart two sandboxes with the same mode, such as a test's fake.
    let key = (sandbox.mode(), sandbox.describe(&SandboxProfile::default()));
    let mut results = RESULTS.get_or_init(Mutex::default).lock().await;
    if let Some((result, at)) = results.get(&key)
        && still_good(result, at.elapsed())
    {
        return result.clone();
    }
    let scratch = scratch_base().join(format!(
        "argonauts-sbx-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let result = self_test(sandbox, &scratch).await;
    if let Err(err) = std::fs::remove_dir_all(&scratch)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %scratch.display(), %err, "could not remove the sandbox self-test folder");
    }
    results.insert(key, (result.clone(), Instant::now()));
    result
}

/// The mode and description of a sandbox.
type SandboxKey = (SandboxMode, String);

fn still_good(result: &SelfTest, age: Duration) -> bool {
    result.ok || age < RETRY_FAILURE_AFTER
}

/// The temp folder, or `/tmp` when its path leaves no room for the probe's sockets once
/// `/private`, the scratch folder, and `denied/s` are added.
fn scratch_base() -> PathBuf {
    let temp = std::env::temp_dir();
    if temp.as_os_str().len() <= 60 {
        temp
    } else {
        PathBuf::from("/tmp")
    }
}

/// The files and sockets the test touches, all inside the scratch directory.
struct Probe {
    scratch: PathBuf,
    allowed: PathBuf,
    outside: PathBuf,
    denied: PathBuf,
    secret: PathBuf,
    allowed_socket: PathBuf,
    denied_socket: PathBuf,
    listeners: Vec<tokio::task::JoinHandle<()>>,
}

impl Probe {
    async fn create(scratch: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(scratch)?;
        let scratch = scratch.canonicalize()?;
        let mut probe = Self {
            allowed: scratch.join("allowed"),
            outside: scratch.join("outside.txt"),
            denied: scratch.join("denied"),
            secret: scratch.join("denied").join("secret.txt"),
            allowed_socket: scratch.join("allowed").join("s"),
            denied_socket: scratch.join("denied").join("s"),
            scratch,
            listeners: Vec::new(),
        };
        std::fs::create_dir_all(&probe.allowed)?;
        std::fs::create_dir_all(&probe.denied)?;
        std::fs::write(&probe.secret, "secret")?;
        match std::fs::remove_file(&probe.outside) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        for socket in [probe.allowed_socket.clone(), probe.denied_socket.clone()] {
            probe.listeners.push(listen(&socket)?);
            // A socket nobody can reach would make a blocked connection prove nothing.
            tokio::net::UnixStream::connect(&socket).await?;
        }
        Ok(probe)
    }

    /// Only `allowed` is writable, so the scratch directory itself isn't, even in temp. The
    /// network is on, as in a CLI session, or every socket would be blocked anyway.
    fn profile(&self) -> SandboxProfile {
        SandboxProfile {
            mode: SandboxMode::Os,
            writable: vec![self.allowed.clone()],
            readable: Vec::new(),
            deny_read: vec![self.denied.clone()],
            network: true,
            proxy_port: None,
            ask_port: None,
            closed: Vec::new(),
            listable: Vec::new(),
            loopback: false,
        }
    }

    fn command(&self) -> CommandSpec {
        let arg = |path: &Path| path.to_string_lossy().into_owned();
        CommandSpec::new("/bin/sh", &self.scratch).args([
            "-c".to_owned(),
            PROBE.to_owned(),
            "sh".to_owned(),
            arg(&self.allowed.join("probe.txt")),
            arg(&self.outside),
            arg(&self.secret),
            arg(&self.allowed_socket),
            arg(&self.denied_socket),
        ])
    }

    fn judge(&self, stdout: &str, described: &str) -> SelfTest {
        let said = |line: &str| stdout.lines().any(|out| out.trim() == line);
        if said("outside-write=ok") || self.outside.exists() {
            return SelfTest::failed("The sandbox let a command write outside its allowed paths.");
        }
        if said("denied-read=ok") {
            return SelfTest::failed("The sandbox let a command read a denied file.");
        }
        if said("denied-connect=ok") {
            return SelfTest::failed(
                "The sandbox let a command connect to a socket in a denied folder.",
            );
        }
        // A sandbox that blocks everything, or never ran the probe, proves nothing.
        let expected = [
            "allowed-write=ok",
            "outside-write=blocked",
            "denied-read=blocked",
            "allowed-connect=ok",
            "denied-connect=blocked",
        ];
        if !expected.iter().all(|line| said(line)) {
            return SelfTest::failed(format!(
                "The sandbox self-test gave an unexpected answer: {}.",
                stdout.trim().replace('\n', ", ")
            ));
        }
        // A CLI's own sandbox can't start inside this one, and needn't: inside ours it is off.
        let name = described.split(" · ").next().unwrap_or(described);
        SelfTest {
            ok: true,
            detail: format!(
                "{name} blocks writes outside allowed paths, reads of denied files, and \
                 connections to sockets in denied folders"
            ),
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        for listener in &self.listeners {
            listener.abort();
        }
    }
}

/// Takes each connection and closes it, so the probe's `nc` returns at once.
fn listen(path: &Path) -> std::io::Result<tokio::task::JoinHandle<()>> {
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(std::io::Error::other(format!(
            "{} is too long for a socket",
            path.display()
        )));
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    Ok(tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    }))
}

async fn run(command: &CommandSpec) -> Result<String, String> {
    let mut process = tokio::process::Command::new(&command.program);
    process
        .args(&command.args)
        .current_dir(&command.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for name in &command.env_remove {
        process.env_remove(name);
    }
    process.envs(command.env_set.iter().map(|(k, v)| (k, v)));
    let output = tokio::time::timeout(TIMEOUT, process.output())
        .await
        .map_err(|_| format!("it took longer than {} seconds", TIMEOUT.as_secs()))?
        .map_err(|err| format!("could not start {}: {err}", command.program))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use argonauts_core::session_runner::SandboxProfile;

    use super::super::{SandboxError, Unsandboxed};
    use super::*;

    /// Runs the command unwrapped while claiming to be an OS sandbox.
    struct Pretend;

    impl SandboxWrap for Pretend {
        fn mode(&self) -> SandboxMode {
            SandboxMode::Os
        }
        fn wrap(
            &self,
            command: CommandSpec,
            _profile: &SandboxProfile,
        ) -> Result<CommandSpec, SandboxError> {
            Ok(command)
        }
        fn describe(&self, _profile: &SandboxProfile) -> String {
            "pretend".to_owned()
        }
    }

    /// Blocks everything, as a sandbox that never starts the command would.
    struct Wall;

    impl SandboxWrap for Wall {
        fn mode(&self) -> SandboxMode {
            SandboxMode::Os
        }
        fn wrap(
            &self,
            command: CommandSpec,
            _profile: &SandboxProfile,
        ) -> Result<CommandSpec, SandboxError> {
            Ok(CommandSpec {
                program: "/usr/bin/false".into(),
                args: Vec::new(),
                ..command
            })
        }
        fn describe(&self, _profile: &SandboxProfile) -> String {
            "wall".to_owned()
        }
    }

    struct Missing;

    impl SandboxWrap for Missing {
        fn mode(&self) -> SandboxMode {
            SandboxMode::Os
        }
        fn wrap(
            &self,
            _command: CommandSpec,
            _profile: &SandboxProfile,
        ) -> Result<CommandSpec, SandboxError> {
            Err(SandboxError::Unavailable("no sandbox-exec".into()))
        }
        fn describe(&self, _profile: &SandboxProfile) -> String {
            "missing".to_owned()
        }
    }

    #[tokio::test]
    async fn self_test_fails_closed() {
        for (sandbox, expected) in [
            (&Pretend as &dyn SandboxWrap, "write outside"),
            (&Wall, "unexpected answer"),
            (&Missing, "no sandbox-exec"),
        ] {
            let scratch = tempfile::tempdir().unwrap();
            let result = self_test(sandbox, scratch.path()).await;
            assert!(!result.ok, "{result:?}");
            assert!(result.detail.contains(expected), "{result:?}");
            assert!(result.detail.contains("refused"), "{result:?}");
        }

        let scratch = tempfile::tempdir().unwrap();
        let result = self_test(&Unsandboxed, scratch.path()).await;
        assert_eq!(
            result,
            SelfTest {
                ok: false,
                detail: "no sandbox".into()
            }
        );
        assert!(!self_test_once(&Unsandboxed).await.ok);
    }

    /// A Seatbelt with the file rules but none for sockets, as the profile was before it covered
    /// them.
    #[cfg(target_os = "macos")]
    struct FilesOnly;

    #[cfg(target_os = "macos")]
    impl SandboxWrap for FilesOnly {
        fn mode(&self) -> SandboxMode {
            SandboxMode::Os
        }
        fn wrap(
            &self,
            command: CommandSpec,
            profile: &SandboxProfile,
        ) -> Result<CommandSpec, SandboxError> {
            let seatbelt = super::super::seatbelt::Seatbelt::new();
            let mut wrapped = seatbelt.wrap(command, profile)?;
            wrapped.args[1] = wrapped.args[1]
                .lines()
                .filter(|line| !line.contains("unix-socket"))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(wrapped)
        }
        fn describe(&self, _profile: &SandboxProfile) -> String {
            "files only".to_owned()
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn self_test_catches_a_reachable_socket_in_a_denied_folder() {
        let scratch = tempfile::tempdir().unwrap();
        let result = self_test(&FilesOnly, scratch.path()).await;
        assert!(!result.ok, "{result:?}");
        assert!(result.detail.contains("connect to a socket"), "{result:?}");
    }

    #[test]
    fn a_failure_is_tried_again_and_a_pass_is_kept() {
        let pass = SelfTest {
            ok: true,
            detail: String::new(),
        };
        let fail = SelfTest {
            ok: false,
            detail: String::new(),
        };
        let long = Duration::from_secs(24 * 3600);
        assert!(still_good(&pass, long));
        assert!(still_good(&fail, Duration::from_secs(1)));
        assert!(!still_good(&fail, RETRY_FAILURE_AFTER));
    }

    #[tokio::test]
    async fn self_test_once_remembers_each_sandbox() {
        let first = self_test_once(&Pretend).await;
        assert!(!first.ok);
        assert_eq!(self_test_once(&Pretend).await, first);
        assert!(
            self_test_once(&Missing)
                .await
                .detail
                .contains("no sandbox-exec")
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn seatbelt_self_test_passes() {
        let scratch = tempfile::tempdir().unwrap();
        let result = self_test(&super::super::seatbelt::Seatbelt::new(), scratch.path()).await;
        assert!(result.ok, "{result:?}");
        assert!(result.detail.starts_with("seatbelt blocks"), "{result:?}");
        assert!(
            result.detail.contains("sockets in denied folders"),
            "{result:?}"
        );
        // Inside ours a CLI turns its own sandbox off, so that it can't start isn't news.
        assert!(!result.detail.contains("own sandbox"), "{result:?}");
        assert!(
            self_test_once(&super::super::seatbelt::Seatbelt::new())
                .await
                .ok
        );
    }
}
