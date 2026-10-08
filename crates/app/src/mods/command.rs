//! Runs a mod's command: inside the sandbox, with a timeout, reading only what it needs to
//! print its numbers. It can read the repository and its own folder, write temporary folders
//! and nothing else, and reach the network only when `sandbox.network` is on.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use argonauts_conductor::sandbox::selftest::self_test_once;
use argonauts_conductor::sandbox::{ProfileInput, SandboxWrap, session_profile};
use argonauts_core::CommandSpec;
use argonauts_core::settings::approvals::SandboxSettings;
use argonauts_core::settings::mods::{ModReading, span_words};
use argonauts_core::worker::{Access, SandboxMode, SandboxProfile};
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::AsyncReadExt as _;

/// The most a command's output is read. A reading is a few hundred bytes.
const MOST_OUTPUT: u64 = 64 * 1024;
/// The most of what it wrote to its error stream that's kept, to say why it failed.
const MOST_COMPLAINT: u64 = 4 * 1024;

/// How long a program that has exited may leave its output pipes open, through something it
/// started, before that is stopped and what it printed is read.
const GRACE: Duration = Duration::from_millis(200);

/// The variables a command sees. Nothing that could hold a key or a token.
const ENV: [&str; 7] = ["PATH", "HOME", "USER", "LANG", "TZ", "SHELL", "TERM"];

/// Where a command runs and what it may touch.
pub(crate) struct Place<'a> {
    /// The folder of the mod's file, which `./program` is beside.
    pub mod_dir: &'a Path,
    /// The repository, which the command starts in.
    pub repo: &'a Path,
    pub argonauts_home: &'a Path,
    pub user_home: &'a Path,
    /// A folder of its own to write in.
    pub scratch: &'a Path,
}

/// A program written `./name` is beside the mod's file. Anything else is as the shell would
/// find it.
pub(crate) fn program_path(program: &str, mod_dir: &Path) -> Option<PathBuf> {
    program
        .strip_prefix("./")
        .map(|beside| mod_dir.join(beside))
}

/// The program and arguments to start. A program beside the mod's file inside the repository
/// goes through `sh`, since the sandbox refuses to start a program from the folder it runs
/// in, and this one you trusted by hand.
fn argv(command: &[String], place: &Place<'_>) -> Result<Vec<String>, String> {
    let (program, args) = command.split_first().ok_or("it names no program")?;
    let Some(path) = program_path(program, place.mod_dir) else {
        return Ok(command.to_vec());
    };
    if !path.is_file() {
        return Err(format!("{program} isn't there yet"));
    }
    let path = path.to_string_lossy().into_owned();
    let mut argv = if Path::new(&path).starts_with(place.repo) {
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "exec \"$0\" \"$@\"".to_owned(),
            path,
        ]
    } else {
        vec![path]
    };
    argv.extend(args.iter().cloned());
    Ok(argv)
}

/// What a command sees of Argonauts' own variables: the allowlist, and its scratch folder as
/// the place for temporary files.
fn kept_env(vars: impl Iterator<Item = (String, String)>, scratch: &Path) -> Vec<(String, String)> {
    vars.filter(|(name, _)| ENV.contains(&name.as_str()) || name.starts_with("LC_"))
        .chain([("TMPDIR".to_owned(), scratch.to_string_lossy().into_owned())])
        .collect()
}

/// What a read-only agent session may touch, with the mod's own folder readable although it's
/// under `~/.argonauts`.
fn profile(
    sandbox: &dyn SandboxWrap,
    settings: &SandboxSettings,
    place: &Place<'_>,
) -> SandboxProfile {
    let input = ProfileInput {
        access: Access::ReadOnly,
        vendor: "",
        workdir: place.repo,
        session_dir: place.scratch,
        argonauts_home: place.argonauts_home,
        user_home: place.user_home,
        base_worktree: None,
        status_file: None,
        extra_writable: &[],
        sees_other_copies: false,
        commits: false,
    };
    // A read-only session's profile: it writes temporary folders, which a shell needs even for
    // a here-document, and nothing else.
    let mut profile = session_profile(&SandboxProfile::default(), &input);
    profile.mode = sandbox.mode();
    profile.writable.push(place.scratch.to_path_buf());
    profile.readable.push(place.mod_dir.to_path_buf());
    profile.network = settings.network;
    profile.proxy_port = None;
    profile
}

/// Runs `command` and reads what it printed as a mod's numbers. `Err` says in plain words why
/// there are none.
pub(crate) async fn run(
    command: &[String],
    timeout: Duration,
    sandbox: &dyn SandboxWrap,
    settings: &SandboxSettings,
    place: &Place<'_>,
) -> Result<ModReading, String> {
    let argv = argv(command, place)?;
    if sandbox.mode() != SandboxMode::None {
        let test = self_test_once(sandbox).await;
        if !test.ok {
            return Err(format!(
                "the sandbox doesn't work here, so its command didn't run: {}",
                test.detail
            ));
        }
    }
    std::fs::create_dir_all(place.scratch)
        .map_err(|err| format!("couldn't make its scratch folder: {err}"))?;
    let mut spec = CommandSpec::new(argv[0].clone(), place.repo).args(argv[1..].iter().cloned());
    spec.env_set = kept_env(std::env::vars(), place.scratch);
    let spec = sandbox
        .wrap(spec, &profile(sandbox, settings, place))
        .map_err(|err| format!("couldn't sandbox its command, so it didn't run: {err}"))?;
    let output = output_of(&spec, timeout).await?;
    ModReading::from_output(&output)
}

/// Starts the command in a process group of its own and returns what it printed. What it
/// started goes with it: at the timeout the whole group is stopped, and once the program
/// exits anything it left running is, after [`GRACE`], so nothing piles up from one reading to
/// the next and a finished program's output is kept.
async fn output_of(spec: &CommandSpec, timeout: Duration) -> Result<String, String> {
    let mut child = tokio::process::Command::new(&spec.program)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env_set.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("{} couldn't start: {err}", spec.program))?;
    let group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .map(Pid::from_raw);
    let stop_group = || {
        if let Some(group) = group {
            let _ = killpg(group, Signal::SIGKILL);
        }
    };
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let reading = async {
        tokio::join!(
            read_some(stdout, MOST_OUTPUT),
            read_some(stderr, MOST_COMPLAINT)
        )
    };
    tokio::pin!(reading);
    // Read while waiting, so a program that prints a lot never blocks on a full pipe.
    let mut read = None;
    let waited = tokio::time::timeout(timeout, async {
        loop {
            tokio::select! {
                status = child.wait() => break status,
                done = &mut reading, if read.is_none() => read = Some(done),
            }
        }
    })
    .await;
    let Ok(status) = waited else {
        stop_group();
        return Err(format!(
            "it took longer than {} and was stopped",
            span_words(timeout)
        ));
    };
    let status = status.map_err(|err| format!("it couldn't be waited for: {err}"));
    let (printed, complained) = match read {
        Some(read) => read,
        None => match tokio::time::timeout(GRACE, &mut reading).await {
            Ok(read) => read,
            Err(_) => {
                stop_group();
                reading.await
            }
        },
    };
    stop_group();
    let status = status?;
    // What it said last is usually why.
    let why = complained
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim);
    let ended = match (status.success(), status.code()) {
        (true, _) if !printed.trim().is_empty() => return Ok(printed),
        // A program with nothing to show yet can say so, as the starter `checks.sh` does.
        (true, _) if why.is_none() => return Ok(printed),
        (true, _) => "it printed nothing".to_owned(),
        (false, Some(code)) => format!("it ended with exit code {code}"),
        (false, None) => "it was stopped by a signal".to_owned(),
    };
    Err(match why {
        Some(why) => format!("{ended}: {why}"),
        None => ended,
    })
}

/// Up to `most` bytes of a stream, as text.
async fn read_some(stream: Option<impl tokio::io::AsyncRead + Unpin>, most: u64) -> String {
    let mut bytes = Vec::new();
    if let Some(stream) = stream {
        // What was read before an error is still worth having.
        let _ = stream.take(most).read_to_end(&mut bytes).await;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use argonauts_conductor::sandbox::Unsandboxed;
    use argonauts_core::settings::mods::ModLevel;

    use super::*;

    struct Folders {
        _temp: tempfile::TempDir,
        mods: PathBuf,
        repo: PathBuf,
        home: PathBuf,
        scratch: PathBuf,
    }

    impl Folders {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let folders = Self {
                mods: root.join("home/.argonauts/mods"),
                repo: root.join("repo"),
                home: root.join("home"),
                scratch: root.join("scratch"),
                _temp: temp,
            };
            std::fs::create_dir_all(&folders.mods).unwrap();
            std::fs::create_dir_all(&folders.repo).unwrap();
            folders
        }

        fn place(&self) -> Place<'_> {
            Place {
                mod_dir: &self.mods,
                repo: &self.repo,
                argonauts_home: &self.home,
                user_home: &self.home,
                scratch: &self.scratch,
            }
        }

        fn script(&self, dir: &Path, name: &str, body: &str) {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        async fn run(&self, command: &[&str], timeout: Duration) -> Result<ModReading, String> {
            let command: Vec<String> = command.iter().map(|word| (*word).to_owned()).collect();
            run(
                &command,
                timeout,
                &Unsandboxed,
                &SandboxSettings::default(),
                &self.place(),
            )
            .await
        }
    }

    const SECOND: Duration = Duration::from_secs(5);

    /// Whether a process whose command line holds `marker` is still running a moment after
    /// it was stopped, which the system takes to tidy away.
    async fn still_running(marker: &str) -> bool {
        for _ in 0..20 {
            let listed = std::process::Command::new("/bin/ps")
                .args(["-ax", "-o", "command="])
                .output()
                .unwrap();
            if !String::from_utf8_lossy(&listed.stdout).contains(marker) {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let listed = std::process::Command::new("/bin/ps")
            .args(["-ax", "-o", "pid,ppid,pgid,stat,command"])
            .output()
            .unwrap();
        for line in String::from_utf8_lossy(&listed.stdout).lines() {
            if line.contains(marker) {
                eprintln!("still running: {line}");
            }
        }
        true
    }

    /// What a command starts goes with it: once it has printed and exited, and at its
    /// timeout, so nothing piles up from one reading to the next.
    #[tokio::test]
    async fn what_a_command_starts_never_outlives_it() {
        let folders = Folders::new();
        let reading = folders
            .run(
                &["sh", "-c", r#"sleep 31.5317 & echo '{"summary": "ok"}'"#],
                SECOND,
            )
            .await
            .unwrap();
        assert_eq!(reading.summary, "ok");
        assert!(!still_running("sleep 31.5317").await);

        let late = folders
            .run(
                &["sh", "-c", "sleep 97.3141 & sleep 97.3141"],
                Duration::from_millis(500),
            )
            .await;
        assert!(late.is_err_and(|why| why.contains("was stopped")));
        assert!(!still_running("sleep 97.3141").await);
    }

    #[tokio::test]
    async fn a_script_beside_the_mod_prints_its_numbers() {
        let folders = Folders::new();
        folders.script(
            &folders.mods,
            "checks.sh",
            r#"echo '{"summary": "2 of 3 passed", "level": "warn", "parts": [{"label": "passed", "value": 2}]}'"#,
        );
        let reading = folders.run(&["./checks.sh"], SECOND).await.unwrap();
        assert_eq!(reading.summary, "2 of 3 passed");
        assert_eq!(reading.level, ModLevel::Warn);
        assert_eq!(reading.parts[0].value, 2.0);
    }

    #[tokio::test]
    async fn plain_words_from_a_program_on_the_path_are_the_summary() {
        let folders = Folders::new();
        let reading = folders.run(&["echo", "main"], SECOND).await.unwrap();
        assert_eq!(reading.summary, "main");
    }

    #[tokio::test]
    async fn it_starts_in_the_repository_with_a_scratch_folder() {
        let folders = Folders::new();
        folders.script(
            &folders.mods,
            "where.sh",
            r#"echo "$(basename "$(pwd -P)") $(basename "$TMPDIR")""#,
        );
        let reading = folders.run(&["./where.sh"], SECOND).await.unwrap();
        assert_eq!(reading.summary, "repo scratch");
    }

    #[test]
    fn a_command_sees_no_keys_or_tokens() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/a"),
            ("LC_ALL", "C"),
            ("ANTHROPIC_API_KEY", "sk-x"),
            ("GH_TOKEN", "t"),
            ("ARGONAUTS_HOME", "/home/a/.argonauts"),
            ("SSH_AUTH_SOCK", "/tmp/agent"),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned()));
        let kept: Vec<String> = kept_env(vars.into_iter(), Path::new("/scratch"))
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(kept, ["PATH", "HOME", "LC_ALL", "TMPDIR"]);
    }

    #[tokio::test]
    async fn a_project_s_script_runs_through_sh() {
        let folders = Folders::new();
        let project_mods = folders.repo.join(".argonauts/mods");
        folders.script(&project_mods, "ci.sh", "echo green");
        let mut place = folders.place();
        place.mod_dir = &project_mods;
        let argv = argv(&["./ci.sh".to_owned(), "--fast".to_owned()], &place).unwrap();
        assert_eq!(argv[..3], ["/bin/sh", "-c", "exec \"$0\" \"$@\""]);
        assert_eq!(argv[4], "--fast");
        let reading = run(
            &["./ci.sh".to_owned()],
            SECOND,
            &Unsandboxed,
            &SandboxSettings::default(),
            &place,
        )
        .await
        .unwrap();
        assert_eq!(reading.summary, "green");
    }

    #[tokio::test]
    async fn the_starter_checks_program_waits_for_your_numbers_then_prints_the_picture() {
        use argonauts_core::settings::mods::{ModBody, builtin};
        let folders = Folders::new();
        let (_, file, text) = builtin::STARTER_PROGRAMS[0];
        let body = text.strip_prefix("#!/bin/sh\n").unwrap();
        folders.script(&folders.mods, file, body);
        assert_eq!(
            folders.run(&["./checks.sh"], SECOND).await.unwrap_err(),
            "it printed nothing: put your numbers in checks.sh, beside checks.toml"
        );
        let yours = body.replace("passed=0 running=0 failed=0", "passed=7 running=1 failed=1");
        assert_ne!(yours, body);
        folders.script(&folders.mods, file, &yours);
        assert_eq!(
            folders.run(&["./checks.sh"], SECOND).await.unwrap(),
            ModReading::sample(ModBody::Stack)
        );
    }

    #[tokio::test]
    async fn what_goes_wrong_is_said_in_plain_words() {
        let folders = Folders::new();
        assert_eq!(
            folders.run(&["./missing.sh"], SECOND).await.unwrap_err(),
            "./missing.sh isn't there yet"
        );
        folders.script(&folders.mods, "fails.sh", "exit 3");
        assert_eq!(
            folders.run(&["./fails.sh"], SECOND).await.unwrap_err(),
            "it ended with exit code 3"
        );
        folders.script(&folders.mods, "says.sh", "echo 'no token set' >&2; exit 2");
        assert_eq!(
            folders.run(&["./says.sh"], SECOND).await.unwrap_err(),
            "it ended with exit code 2: no token set"
        );
        folders.script(&folders.mods, "quiet.sh", "true");
        assert_eq!(
            folders.run(&["./quiet.sh"], SECOND).await.unwrap_err(),
            "it printed nothing"
        );
        folders.script(&folders.mods, "waits.sh", "echo 'no numbers yet' >&2");
        assert_eq!(
            folders.run(&["./waits.sh"], SECOND).await.unwrap_err(),
            "it printed nothing: no numbers yet"
        );
        // What it grumbles beside its numbers is nobody's business.
        folders.script(&folders.mods, "both.sh", "echo slow >&2; echo main");
        assert_eq!(
            folders.run(&["./both.sh"], SECOND).await.unwrap().summary,
            "main"
        );
        assert!(
            folders
                .run(&["no-such-program-anywhere"], SECOND)
                .await
                .unwrap_err()
                .contains("couldn't start")
        );
    }

    #[tokio::test]
    async fn a_command_that_takes_too_long_is_stopped() {
        let folders = Folders::new();
        folders.script(&folders.mods, "slow.sh", "sleep 30; echo late");
        let started = std::time::Instant::now();
        let err = folders
            .run(&["./slow.sh"], Duration::from_millis(200))
            .await
            .unwrap_err();
        assert_eq!(err, "it took longer than 200ms and was stopped");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_profile_writes_only_temporary_folders_and_reads_its_own() {
        let folders = Folders::new();
        let place = folders.place();
        let profile = profile(&Unsandboxed, &SandboxSettings::default(), &place);
        assert!(profile.writable.contains(&folders.scratch));
        // Nothing of yours is writable: not the repository, not your home, not the mods.
        for yours in [&folders.repo, &folders.home, &folders.mods] {
            assert!(!profile.writable.contains(yours), "{}", yours.display());
        }
        assert!(profile.readable.contains(&folders.mods));
        assert!(profile.deny_read.contains(&folders.home));
    }
}
