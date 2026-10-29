//! Work outside the project under Seatbelt: what auto and autopilot open, what supervised and
//! auto preview open once asked, and the never-list that holds there whichever way a path
//! reaches it.

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
const TRY_EDIT: &str =
    r#"if (printf '\nexport X=1\n' >> "$1") 2>/dev/null; then echo wrote; else echo blocked; fi"#;

/// Homebrew's install: the program goes in the prefix's Cellar and a link to it in its `bin`.
const BREW_INSTALL: &str = r#"(mkdir -p "$1/Cellar/tool/1.0/bin" "$1/bin" && printf x > "$1/Cellar/tool/1.0/bin/tool" && ln -sf ../Cellar/tool/1.0/bin/tool "$1/bin/tool") 2>/dev/null && echo installed || echo blocked"#;
/// cargo's install: the program goes in `bin` and its record of what it installed beside it.
const CARGO_INSTALL: &str = r#"(mkdir -p "$1/bin" && printf x > "$1/bin/tool" && printf x > "$1/.crates.toml") 2>/dev/null && echo installed || echo blocked"#;

/// Startup files, tool configs, and where programs are installed, relative to your home: a
/// session that may write outside its project edits them as any file there.
const LIKE_ANY_FILE: &[&str] = &[
    ".zshrc",
    ".bashrc",
    ".profile",
    ".gitconfig",
    ".config/git/config",
    ".config/fish/config.fish",
    "Library/LaunchAgents/tool.plist",
    ".config/autostart/tool.desktop",
    ".vimrc",
    ".config/nvim/init.lua",
    ".cargo/config.toml",
    ".local/bin/tool",
    ".local/share/claude/versions/9.9.9",
];

/// The paths no mode that opens outside ever reaches, read or written, each way a path can
/// name them.
const NEVER_REACHED: &[&str] = &[
    ".ssh/id_rsa",
    "Downloads/../.ssh/id_rsa",
    "Downloads/keys/id_rsa",
    "work/app/.env",
    "work/app/../app/.env",
];

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
    profile_with(
        h,
        &OutsideSettings::default(),
        agent_id,
        mode,
        access,
        workdir,
    )
}

fn profile_with(
    h: &Home,
    settings: &OutsideSettings,
    agent_id: &str,
    mode: ApprovalMode,
    access: Access,
    workdir: &Path,
) -> SandboxProfile {
    let outside = Outside::for_mode(settings, mode, Some(&h.project), &h.home);
    profile_from(h, agent_id, outside, access, workdir)
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
    for never in NEVER_REACHED {
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
    // A shell's setup is read and written as any file.
    assert_eq!(run(&profile, &h.project, TRY_READ, &at(".zshrc")), "read");
    assert_eq!(run(&profile, &h.project, TRY_EDIT, &at(".zshrc")), "wrote");
    // The never-list's system folders are read but never written.
    assert!(!can_write(&profile, Path::new("/usr/bin/x")));
    assert!(!can_write(&profile, Path::new("/etc/hosts")));
    for folder in ["/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32"] {
        let file = Path::new(folder).join("x");
        assert!(!can_write(&profile, &file), "{folder}");
    }
    assert_eq!(
        run(&profile, &h.project, TRY_READ, Path::new("/bin/sh")),
        "read"
    );
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

/// Where the mode opens outside only once asked, the asking holds the turn, and this sandbox
/// lets an approved write land anywhere but the never-list.
#[test]
fn a_supervised_direct_turn_writes_outside_once_asked_but_never_the_never_list() {
    let h = home();
    let at = |path: &str| h.home.join(path);
    for mode in [ApprovalMode::Supervised, ApprovalMode::AutoPreview] {
        let profile = profile(&h, mode, Access::Direct, &h.project);
        assert_eq!(
            run(&profile, &h.project, TRY_WRITE, &at("work/app/src/a.rs")),
            "wrote",
            "{mode:?}"
        );
        assert_eq!(
            run(&profile, &h.project, TRY_WRITE, &at("Downloads/y.md")),
            "wrote",
            "{mode:?}"
        );
        assert_eq!(
            run(&profile, &h.project, TRY_READ, &at("Downloads/x.png")),
            "read",
            "{mode:?}"
        );
        for never in NEVER_REACHED {
            assert_eq!(
                run(&profile, &h.project, TRY_READ, &at(never)),
                "blocked",
                "{mode:?} {never}"
            );
            assert_eq!(
                run(&profile, &h.project, TRY_WRITE, &at(never)),
                "blocked",
                "{mode:?} {never}"
            );
        }
        assert_eq!(
            run(&profile, &h.project, TRY_EDIT, &at(".zshrc")),
            "wrote",
            "{mode:?}"
        );
        assert!(!can_write(&profile, Path::new("/etc/hosts")));
        assert_eq!(
            run(&profile, &h.project, TRY_WRITE, &h.session.join("x")),
            "blocked",
            "{mode:?}"
        );
    }
}

/// A template a project commits, such as `.env.example`, reads and writes as any file, in the
/// default mode too, so git, a dev server or a test suite that opens one works. Every other env
/// file stays closed.
#[test]
fn the_env_templates_open_and_no_other_env_file_does() {
    let h = home();
    std::fs::write(h.project.join(".env.example"), "MADE_UP=").unwrap();
    std::fs::write(h.project.join(".env.local"), "MADE_UP=1").unwrap();
    let at = |name: &str| h.project.join(name);
    for mode in [ApprovalMode::Supervised, ApprovalMode::Autopilot] {
        let profile = profile(&h, mode, Access::Direct, &h.project);
        assert_eq!(
            run(&profile, &h.project, TRY_READ, &at(".env.example")),
            "read",
            "{mode:?}"
        );
        for template in [".env.example", ".env.sample", ".env.template", ".env.dist"] {
            assert_eq!(
                run(&profile, &h.project, TRY_WRITE, &at(template)),
                "wrote",
                "{mode:?} {template}"
            );
        }
        for secret in [".env", ".env.local", ".env.example.local"] {
            assert_eq!(
                run(&profile, &h.project, TRY_READ, &at(secret)),
                "blocked",
                "{mode:?} {secret}"
            );
        }
    }
}

#[test]
fn an_empty_ask_keeps_a_supervised_turn_to_its_project_as_before() {
    let h = home();
    let kept_in = OutsideSettings {
        ask: Vec::new(),
        ..OutsideSettings::default()
    };
    let profile = profile_with(
        &h,
        &kept_in,
        "claude-code",
        ApprovalMode::Supervised,
        Access::Direct,
        &h.project,
    );
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

/// A run's worker can't ask, so where the mode opens outside only once asked, nothing it wrote
/// there would wait for you: it reads outside and writes only its worktree, with the reach the
/// mode gives and with the one the run hands it.
#[test]
fn a_run_worker_in_supervised_reads_outside_and_writes_only_its_worktree() {
    let h = home();
    let tree = h.argonauts.join("worktrees/app/run-1/task-a");
    std::fs::create_dir_all(tree.join("src")).unwrap();
    std::fs::create_dir_all(h.home.join(".local/bin")).unwrap();
    let at = |path: &str| h.home.join(path);
    for mode in [ApprovalMode::Supervised, ApprovalMode::AutoPreview] {
        let asked = profile(&h, mode, Access::Build, &tree);
        let as_run = SandboxProfile {
            mode: SandboxMode::Os,
            ..session_profile(
                &SandboxProfile {
                    outside: asked.outside.clone().without_asking(),
                    ..SandboxProfile::default()
                },
                &ProfileInput {
                    access: Access::Build,
                    agent_id: "claude-code",
                    workdir: &tree,
                    session_dir: &h.session,
                    argonauts_home: &h.argonauts,
                    user_home: &h.home,
                    base_worktree: None,
                    status_file: None,
                    extra_writable: &[],
                    sees_other_worktrees: false,
                    commits: false,
                },
            )
        };
        for worker in [&asked, &as_run] {
            assert_eq!(
                run(worker, &tree, TRY_WRITE, &tree.join("src/a.rs")),
                "wrote",
                "{mode:?}"
            );
            assert_eq!(
                run(worker, &tree, TRY_READ, &at("Downloads/x.png")),
                "read",
                "{mode:?}"
            );
            for outside in ["Downloads/y.md", ".local/bin/x", "work/app/src/a.rs"] {
                assert_eq!(
                    run(worker, &tree, TRY_WRITE, &at(outside)),
                    "blocked",
                    "{mode:?} {outside}"
                );
            }
            assert_eq!(
                run(worker, &tree, TRY_READ, &at("Downloads/keys/id_rsa")),
                "blocked",
                "{mode:?}"
            );
        }
        let reviewer = profile(&h, mode, Access::ReadOnly, &tree);
        assert_eq!(
            run(&reviewer, &tree, TRY_WRITE, &at("Downloads/y.md")),
            "blocked",
            "{mode:?}"
        );
    }
}

/// A session that writes anywhere still never reaches a credential store, such as npm's token
/// file or gh's config: they stay closed to reads and writes. The stores exist here, so a refusal
/// is the sandbox's and not a missing folder's.
#[test]
fn a_session_that_writes_anywhere_never_reaches_a_credential_store() {
    let h = home();
    let tree = h.argonauts.join("worktrees/app/run-1/task-a");
    std::fs::create_dir_all(tree.join("src")).unwrap();
    std::fs::create_dir_all(h.home.join(".config/gh")).unwrap();
    let stores = [".npmrc", ".config/gh/config.yml"];
    for store in stores {
        std::fs::write(h.home.join(store), "made up").unwrap();
    }
    let at = |path: &str| h.home.join(path);
    let sessions = [
        profile(&h, ApprovalMode::Autopilot, Access::Build, &tree),
        profile(&h, ApprovalMode::Autopilot, Access::Direct, &h.project),
        profile(&h, ApprovalMode::Supervised, Access::Direct, &h.project),
    ];
    for (session, cwd) in sessions.iter().zip([&tree, &h.project, &h.project]) {
        assert_eq!(run(session, cwd, TRY_WRITE, &at("Downloads/y.md")), "wrote");
        for store in stores {
            assert_eq!(
                run(session, cwd, TRY_READ, &at(store)),
                "blocked",
                "{store}"
            );
            assert_eq!(
                run(session, cwd, TRY_WRITE, &at(store)),
                "blocked",
                "{store}"
            );
        }
    }
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

/// Makes the folders each of [`LIKE_ANY_FILE`] sits in, as a home that has them would.
fn with_tool_folders(h: &Home) {
    for file in LIKE_ANY_FILE {
        std::fs::create_dir_all(h.home.join(file).parent().unwrap()).unwrap();
    }
}

/// What a Homebrew install into a prefix in your home, a cargo install, and an edit to each of
/// [`LIKE_ANY_FILE`] came to for `session`: `installed`, `wrote` or `blocked`.
fn tries_installs(h: &Home, session: &SandboxProfile, cwd: &Path) -> Vec<(String, String)> {
    let mut tried = vec![
        (
            "brew".to_owned(),
            run(session, cwd, BREW_INSTALL, &h.home.join("homebrew")),
        ),
        (
            "cargo".to_owned(),
            run(session, cwd, CARGO_INSTALL, &h.home.join(".cargo")),
        ),
    ];
    for file in LIKE_ANY_FILE {
        let edit = run(session, cwd, TRY_EDIT, &h.home.join(file));
        tried.push(((*file).to_owned(), edit));
    }
    tried
}

fn assert_never_reached(h: &Home, session: &SandboxProfile, cwd: &Path, case: &str) {
    for never in NEVER_REACHED {
        let path = h.home.join(never);
        assert_eq!(
            run(session, cwd, TRY_READ, &path),
            "blocked",
            "{case} {never}"
        );
        assert_eq!(
            run(session, cwd, TRY_WRITE, &path),
            "blocked",
            "{case} {never}"
        );
    }
}

fn profile_from(
    h: &Home,
    agent_id: &str,
    outside: Outside,
    access: Access,
    workdir: &Path,
) -> SandboxProfile {
    let requested = SandboxProfile {
        outside,
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

/// On autopilot a session writes outside its project as any agent its CLI lets write there: a
/// Homebrew or cargo install lands, and so does an edit to a startup file or a tool config, in
/// a direct turn and in a run's worker. Only the never-list stays closed, its system folders
/// included, and every folder on `PATH` is written but where the never-list keeps it.
#[test]
fn on_autopilot_installs_and_startup_files_land_as_any_file() {
    let h = home();
    with_tool_folders(&h);
    let tree = h.argonauts.join("worktrees/app/run-1/task-a");
    std::fs::create_dir_all(tree.join("src")).unwrap();
    let sessions = [
        (
            profile(&h, ApprovalMode::Autopilot, Access::Direct, &h.project),
            &h.project,
        ),
        (
            profile(&h, ApprovalMode::Autopilot, Access::Build, &tree),
            &tree,
        ),
    ];
    for (session, cwd) in &sessions {
        let case = cwd.display().to_string();
        for (what, outcome) in tries_installs(&h, session, cwd) {
            assert_ne!(outcome, "blocked", "{case} {what}");
        }
        assert_never_reached(&h, session, cwd, &case);
        for program in [
            "/opt/homebrew/bin/tool",
            "/opt/homebrew/Cellar/tool/1.0/bin/tool",
            "/usr/local/bin/tool",
            "/opt/local/bin/tool",
            "/nix/store/tool",
        ] {
            assert!(can_write(session, Path::new(program)), "{case} {program}");
        }
        for system in ["/usr/bin/tool", "/etc/zshrc", "/System/Library/tool"] {
            assert!(!can_write(session, Path::new(system)), "{case} {system}");
        }
        for folder in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            if folder.is_absolute() && folder != Path::new("/") {
                let program = folder.join("tool");
                assert_eq!(
                    can_write(session, &program),
                    !session.outside.never.refuses_write(&program),
                    "{case} {}",
                    folder.display()
                );
            }
        }
    }
    assert_eq!(
        h.home.join("homebrew/bin/tool").canonicalize().unwrap(),
        h.home.join("homebrew/Cellar/tool/1.0/bin/tool")
    );
    assert!(h.home.join(".cargo/bin/tool").is_file());
    let zshrc = std::fs::read_to_string(h.home.join(".zshrc")).unwrap();
    assert!(
        zshrc.starts_with("# shell setup\n") && zshrc.ends_with("export X=1\n"),
        "{zshrc:?}"
    );
}

/// With `ask` emptied, supervised keeps to the project as before: an install or a startup file
/// edit outside is refused as any write there is, for every CLI, and nothing of it is left.
#[test]
fn supervised_with_asking_off_refuses_installs_and_startup_files_as_any_write_outside() {
    let h = home();
    with_tool_folders(&h);
    let kept_in = OutsideSettings {
        ask: Vec::new(),
        ..OutsideSettings::default()
    };
    for agent in ["claude-code", "codex", "opencode", "antigravity-cli"] {
        let session = profile_with(
            &h,
            &kept_in,
            agent,
            ApprovalMode::Supervised,
            Access::Direct,
            &h.project,
        );
        assert_eq!(
            run(&session, &h.project, TRY_WRITE, &h.project.join("src/a.rs")),
            "wrote",
            "{agent}"
        );
        assert_eq!(
            run(
                &session,
                &h.project,
                TRY_WRITE,
                &h.home.join("Downloads/y.md")
            ),
            "blocked",
            "{agent}"
        );
        for (what, outcome) in tries_installs(&h, &session, &h.project) {
            assert_eq!(outcome, "blocked", "{agent} {what}");
        }
    }
    assert_eq!(
        std::fs::read_to_string(h.home.join(".zshrc")).unwrap(),
        "# shell setup"
    );
    assert!(!h.home.join("homebrew").exists());
    assert!(!h.home.join(".cargo/bin").exists());
    for file in LIKE_ANY_FILE.iter().filter(|file| **file != ".zshrc") {
        assert!(!h.home.join(file).exists(), "{file}");
    }
}

/// In supervised and auto preview an install or a startup file edit outside waits for you as
/// any write there does. Claude Code asks its own way, and this sandbox lets the write land
/// once you said yes. Codex, OpenCode and agy can't ask first, so theirs is turned away, and the
/// turn you allow by answering lands it, the never-list still closed. A run's worker can't
/// ask, so it writes only its worktree.
#[test]
fn supervised_lets_installs_and_startup_files_land_once_asked() {
    for mode in [ApprovalMode::Supervised, ApprovalMode::AutoPreview] {
        let h = home();
        with_tool_folders(&h);
        let asking = profile(&h, mode, Access::Direct, &h.project);
        for (what, outcome) in tries_installs(&h, &asking, &h.project) {
            assert_ne!(outcome, "blocked", "{mode:?} {what}");
        }
        assert_never_reached(&h, &asking, &h.project, &format!("{mode:?}"));

        for agent in ["codex", "opencode", "antigravity-cli"] {
            let h = home();
            with_tool_folders(&h);
            let case = format!("{mode:?} {agent}");
            let outside =
                Outside::for_mode(&OutsideSettings::default(), mode, Some(&h.project), &h.home);
            let unasked = outside.clone().without_asking();
            let turned_away = profile_from(&h, agent, unasked, Access::Direct, &h.project);
            for (what, outcome) in tries_installs(&h, &turned_away, &h.project) {
                assert_eq!(outcome, "blocked", "{case} {what}");
            }
            assert_eq!(
                std::fs::read_to_string(h.home.join(".zshrc")).unwrap(),
                "# shell setup",
                "{case}"
            );
            for file in LIKE_ANY_FILE {
                let path = h.home.join(file);
                assert!(!outside.never.refuses_write(&path), "{case} {file}");
            }
            let allowed = Outside {
                read: true,
                write: true,
                after_asking: false,
                ..outside
            };
            let allowed = profile_from(&h, agent, allowed, Access::Direct, &h.project);
            for (what, outcome) in tries_installs(&h, &allowed, &h.project) {
                assert_ne!(outcome, "blocked", "{case} {what}");
            }
            assert_never_reached(&h, &allowed, &h.project, &case);
        }

        let h = home();
        with_tool_folders(&h);
        let tree = h.argonauts.join("worktrees/app/run-1/task-a");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        let worker = profile(&h, mode, Access::Build, &tree);
        for (what, outcome) in tries_installs(&h, &worker, &tree) {
            assert_eq!(outcome, "blocked", "{mode:?} worker {what}");
        }
    }
}

/// The never-list is yours to set. A startup file or a program folder you add to it is never
/// read or written, by a link or through `../` too, and a system folder you take off it is
/// written as any folder.
#[test]
fn the_never_list_keeps_what_you_add_and_opens_what_you_take_off() {
    let h = home();
    with_tool_folders(&h);
    std::os::unix::fs::symlink(h.home.join(".zshrc"), h.home.join("Downloads/rc")).unwrap();
    let mut never = OutsideSettings::default().never;
    never.extend(["~/.zshrc".to_owned(), "~/.cargo/bin".to_owned()]);
    let yours = OutsideSettings {
        never,
        ..OutsideSettings::default()
    };
    for mode in [ApprovalMode::Autopilot, ApprovalMode::Supervised] {
        let session = profile_with(&h, &yours, "claude-code", mode, Access::Direct, &h.project);
        for path in [
            ".zshrc",
            "Downloads/rc",
            "Downloads/../.zshrc",
            "work/app/../../.zshrc",
        ] {
            let path_in_home = h.home.join(path);
            assert_eq!(
                run(&session, &h.project, TRY_READ, &path_in_home),
                "blocked",
                "{mode:?} {path}"
            );
            assert_eq!(
                run(&session, &h.project, TRY_EDIT, &path_in_home),
                "blocked",
                "{mode:?} {path}"
            );
        }
        assert_eq!(
            run(&session, &h.project, CARGO_INSTALL, &h.home.join(".cargo")),
            "blocked",
            "{mode:?}"
        );
        assert_eq!(
            run(&session, &h.project, TRY_EDIT, &h.home.join(".bashrc")),
            "wrote",
            "{mode:?}"
        );
        assert_never_reached(&h, &session, &h.project, &format!("{mode:?}"));
    }
    assert_eq!(
        std::fs::read_to_string(h.home.join(".zshrc")).unwrap(),
        "# shell setup"
    );

    let emptied = OutsideSettings {
        never: Vec::new(),
        ..OutsideSettings::default()
    };
    let session = profile_with(
        &h,
        &emptied,
        "claude-code",
        ApprovalMode::Autopilot,
        Access::Direct,
        &h.project,
    );
    assert!(can_write(&session, Path::new("/etc/zshrc")));
    assert!(can_write(&session, Path::new("/usr/bin/tool")));
}
