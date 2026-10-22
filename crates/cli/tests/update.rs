//! `argonauts update` as a shell runs it, with fake CLIs whose updaters only wait.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use argonauts_testkit::TempRepo;

/// A fake Claude Code whose update starts a child of its own, writes the child's id to
/// `pid`, and waits for it.
fn waiting_claude(dir: &Path, pid: &Path) -> std::path::PathBuf {
    let path = dir.join("claude");
    let body = format!(
        "#!/bin/sh\ncase \"$1\" in\n  --version) echo '2.1.292 (Claude Code)' ;;\n  update) sleep \
         30 & echo $! > '{}'; echo updating; wait ;;\nesac\n",
        pid.display()
    );
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn gone(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let alive = Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !alive {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// The CLIs' updaters run in process groups of their own, which the terminal's Ctrl+C doesn't
/// reach. Ctrl+C stops them all the same, with what they started, and says so.
#[test]
fn ctrl_c_stops_argonauts_update_and_its_updaters() {
    let repo = TempRepo::new().unwrap();
    let pid = repo.scratch().join("child.pid");
    let claude = waiting_claude(repo.scratch(), &pid);
    // A CLI's program is a safety setting, which only your own config file sets.
    let mut config = format!("[agents.claude-code]\nbinary = \"{}\"\n", claude.display());
    for id in ["codex", "antigravity-cli", "opencode", "opencode-acp"] {
        config.push_str(&format!("[agents.{id}]\nbinary = \"/nowhere/{id}\"\n"));
    }
    fs::create_dir_all(repo.home()).unwrap();
    fs::write(repo.home().join("config.toml"), config).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_argonauts"))
        .arg("update")
        .current_dir(repo.root())
        .envs(repo.envs())
        .env_remove("ARGONAUTS_SESSION")
        .env("ARGONAUTS_DISABLE_UPDATES", "1")
        // Only the system's own programs, so no real agent CLI can be found by name.
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let updater_child = loop {
        if let Ok(text) = fs::read_to_string(&pid)
            && !text.trim().is_empty()
        {
            break text.trim().to_owned();
        }
        assert!(Instant::now() < deadline, "the updater never started");
        std::thread::sleep(Duration::from_millis(50));
    };
    let interrupted = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());
    let output = child.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(130), "{said}");
    assert!(said.contains("Claude Code"), "{said}");
    assert!(said.contains("2.1.292   stopped"), "{said}");
    assert!(
        said.contains("Stopped. Each updater it started has ended."),
        "{said}"
    );
    assert!(
        gone(&updater_child),
        "the updater's own child {updater_child} was ended"
    );
}
