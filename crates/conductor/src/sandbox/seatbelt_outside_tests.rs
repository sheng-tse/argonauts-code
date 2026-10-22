//! Work outside the project under Seatbelt: what auto and autopilot open, and the never-list
//! that holds there whichever way a path reaches it.

use std::path::{Path, PathBuf};

use argonauts_core::CommandSpec;
use argonauts_core::outside::Outside;
use argonauts_core::session_runner::{Access, SandboxMode, SandboxProfile};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_core::settings::outside::OutsideSettings;

use super::SandboxWrap;
use super::profile::{ProfileInput, can_write, session_profile};
use super::seatbelt::Seatbelt;

const TRY_READ: &str = r#"if cat "$1" >/dev/null 2>&1; then echo read; else echo blocked; fi"#;
const TRY_WRITE: &str =
    r#"if (printf x > "$1") 2>/dev/null; then echo wrote; else echo blocked; fi"#;

fn run(profile: &SandboxProfile, cwd: &Path, script: &str, path: &Path) -> String {
    let command = CommandSpec::new("/bin/sh", cwd).args([
        "-c".to_owned(),
        script.to_owned(),
        "sh".to_owned(),
        path.to_string_lossy().into_owned(),
    ]);
    let wrapped = Seatbelt::new().wrap(command, profile).unwrap();
    let output = std::process::Command::new(&wrapped.program)
        .args(&wrapped.args)
        .current_dir(&wrapped.cwd)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A home with a picture in Downloads, a key in `.ssh`, a link from Downloads to it, and a
/// project with a made-up `.env`.
struct Home {
    _temp: tempfile::TempDir,
    home: PathBuf,
    argonauts: PathBuf,
    session: PathBuf,
    project: PathBuf,
}

fn home() -> Home {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let home = root.join("home");
    let argonauts = home.join(".argonauts");
    let session = argonauts.join("state/app/conversations/c-1/turn-1");
    let project = home.join("work/app");
    for dir in [&session, &home.join("Downloads"), &home.join(".ssh")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(home.join("Downloads/x.png"), "picture").unwrap();
    std::fs::write(project.join("src/lib.rs"), "").unwrap();
    std::fs::write(home.join(".ssh/id_rsa"), "not a real key").unwrap();
    std::fs::write(home.join(".zshrc"), "# shell setup").unwrap();
    std::fs::write(project.join(".env"), "MADE_UP=1").unwrap();
    std::os::unix::fs::symlink(home.join(".ssh"), home.join("Downloads/keys")).unwrap();
    Home {
        _temp: temp,
        home,
        argonauts,
        session,
        project,
    }
}

fn profile(h: &Home, mode: ApprovalMode, access: Access, workdir: &Path) -> SandboxProfile {
    profile_as(h, "claude-code", mode, access, workdir)
}

fn profile_as(
    h: &Home,
    agent_id: &str,
    mode: ApprovalMode,
    access: Access,
    workdir: &Path,
) -> SandboxProfile {
    let requested = SandboxProfile {
        outside: Outside::for_mode(&OutsideSettings::default(), mode, Some(&h.project), &h.home),
        ..SandboxProfile::default()
    };
    let input = ProfileInput {
        access,
        agent_id,
        workdir,
        session_dir: &h.session,
        argonauts_home: &h.argonauts,
        user_home: &h.home,
        base_worktree: None,
        status_file: None,
        extra_writable: &[],
        sees_other_worktrees: false,
        commits: false,
    };
    SandboxProfile {
        mode: SandboxMode::Os,
        ..session_profile(&requested, &input)
    }
}

#[test]
fn a_direct_turn_on_autopilot_reads_and_writes_anywhere_but_the_never_list() {
    let h = home();
    let profile = profile(&h, ApprovalMode::Autopilot, Access::Direct, &h.project);
    let at = |path: &str| h.home.join(path);
    assert_eq!(
        run(&profile, &h.project, TRY_READ, &at("Downloads/x.png")),
        "read"
    );
    assert_eq!(
        run(&profile, &h.project, TRY_WRITE, &at("Downloads/y.md")),
        "wrote"
    );
    assert_eq!(
        run(&profile, &h.project, TRY_WRITE, &at("work/app/src/a.rs")),
        "wrote"
    );
    for never in [
        ".ssh/id_rsa",
        "Downloads/../.ssh/id_rsa",
        "Downloads/keys/id_rsa",
        "work/app/.env",
        "work/app/../app/.env",
    ] {
        assert_eq!(
            run(&profile, &h.project, TRY_READ, &at(never)),
            "blocked",
            "{never}"
        );
        assert_eq!(
            run(&profile, &h.project, TRY_WRITE, &at(never)),
            "blocked",
            "{never}"
        );
    }
    // A shell's setup is read as every shell reads it, and never written.
    assert_eq!(run(&profile, &h.project, TRY_READ, &at(".zshrc")), "read");
    assert_eq!(
        run(&profile, &h.project, TRY_WRITE, &at(".zshrc")),
        "blocked"
    );
    assert!(!can_write(&profile, Path::new("/usr/bin/x")));
    assert!(!can_write(&profile, Path::new("/etc/hosts")));
    // Argonauts Code's own records stay closed.
    assert_eq!(
        run(&profile, &h.project, TRY_WRITE, &h.session.join("x")),
        "blocked"
    );
}

/// The never-list keeps the CLIs' sign-in from every agent, but a CLI still reads its own, or
/// it can't sign in: Claude Code its keychain and credentials, Codex its `auth.json`.
#[test]
fn a_cli_on_autopilot_still_reads_its_own_sign_in() {
    let h = home();
    let at = |path: &str| h.home.join(path);
    for file in [
        "Library/Keychains/login.keychain-db",
        ".claude/.credentials.json",
        ".codex/auth.json",
    ] {
        std::fs::create_dir_all(at(file).parent().unwrap()).unwrap();
        std::fs::write(at(file), "made up").unwrap();
    }
    let try_read =
        |profile: &SandboxProfile, path: &str| run(profile, &h.project, TRY_READ, &at(path));
    let mode = ApprovalMode::Autopilot;
    let claude = profile_as(&h, "claude-code", mode, Access::Direct, &h.project);
    assert_eq!(
        try_read(&claude, "Library/Keychains/login.keychain-db"),
        "read"
    );
    assert_eq!(try_read(&claude, ".claude/.credentials.json"), "read");
    assert_eq!(try_read(&claude, ".codex/auth.json"), "blocked");
    assert_eq!(try_read(&claude, ".ssh/id_rsa"), "blocked");

    let codex = profile_as(&h, "codex", mode, Access::Direct, &h.project);
    assert_eq!(try_read(&codex, ".codex/auth.json"), "read");
    assert_eq!(
        run(&codex, &h.project, TRY_WRITE, &at(".codex/auth.json")),
        "wrote",
        "a refreshed sign-in is written back"
    );
    assert_eq!(try_read(&codex, ".claude/.credentials.json"), "blocked");

    let agy = profile_as(&h, "antigravity-cli", mode, Access::Direct, &h.project);
    assert_eq!(
        try_read(&agy, "Library/Keychains/login.keychain-db"),
        "blocked"
    );
    assert_eq!(try_read(&agy, ".codex/auth.json"), "blocked");
}

#[test]
fn a_supervised_direct_turn_writes_only_its_project_as_before() {
    let h = home();
    let profile = profile(&h, ApprovalMode::Supervised, Access::Direct, &h.project);
    let at = |path: &str| h.home.join(path);
    assert_eq!(
        run(&profile, &h.project, TRY_WRITE, &at("work/app/src/a.rs")),
        "wrote"
    );
    assert_eq!(
        run(&profile, &h.project, TRY_WRITE, &at("Downloads/y.md")),
        "blocked"
    );
    assert_eq!(
        run(&profile, &h.project, TRY_READ, &at("work/app/.env")),
        "read"
    );
}

#[test]
fn a_run_worker_on_autopilot_writes_outside_in_place_and_the_project_only_in_its_worktree() {
    let h = home();
    let tree = h.argonauts.join("worktrees/app/run-1/task-a");
    std::fs::create_dir_all(tree.join("src")).unwrap();
    let profile = profile(&h, ApprovalMode::Autopilot, Access::Build, &tree);
    let at = |path: &str| h.home.join(path);
    assert_eq!(
        run(&profile, &tree, TRY_WRITE, &tree.join("src/a.rs")),
        "wrote"
    );
    assert_eq!(
        run(&profile, &tree, TRY_WRITE, &at("work/app/src/a.rs")),
        "blocked"
    );
    assert_eq!(
        run(&profile, &tree, TRY_READ, &at("work/app/src/lib.rs")),
        "read"
    );
    assert_eq!(
        run(&profile, &tree, TRY_WRITE, &at("Downloads/y.md")),
        "wrote"
    );
    assert_eq!(
        run(&profile, &tree, TRY_READ, &at("Downloads/x.png")),
        "read"
    );
    assert_eq!(
        run(&profile, &tree, TRY_READ, &at("Downloads/keys/id_rsa")),
        "blocked"
    );
    // A reviewer reads anywhere and writes nowhere outside.
    let reviewer = profile_for_reviewer(&h, &tree);
    assert_eq!(
        run(&reviewer, &tree, TRY_READ, &at("Downloads/x.png")),
        "read"
    );
    assert_eq!(
        run(&reviewer, &tree, TRY_WRITE, &at("Downloads/y.md")),
        "blocked"
    );
    assert_eq!(
        run(&reviewer, &tree, TRY_READ, &at(".ssh/id_rsa")),
        "blocked"
    );
}

fn profile_for_reviewer(h: &Home, tree: &Path) -> SandboxProfile {
    profile(h, ApprovalMode::Autopilot, Access::ReadOnly, tree)
}
