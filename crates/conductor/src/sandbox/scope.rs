//! The permission scope a session driver answers permission requests from. It mirrors the OS
//! profile, so what the driver approves is what the sandbox would allow anyway, plus the paths no
//! task may touch whoever the agent is.

use std::path::{Path, PathBuf};

use argonauts_core::agents::driver::PermissionScope;
use argonauts_core::agents::session_runner::{Access, SandboxProfile};

use super::profile::{outside_reach, resolve_path};

/// Relative to the worktree. A task's own forbidden paths come on top.
pub const FORBIDDEN_PATHS: &[&str] = &[
    ".argonauts/**",
    ".claude/**",
    ".codex/**",
    ".agents/**",
    ".mcp.json",
    ".hermes/**",
    ".hermes.md",
    "HERMES.md",
    "AGENTS.override.md",
    ".openclaw/**",
];

/// The workdir is the only root, but where the session reads outside its project: once asked
/// in a direct turn, whose driver then asks you, and without asking in a run's session, every
/// folder is, the denied areas still closed. A no-tools session gets no roots and nothing
/// writable, so its driver rejects every request. A denied area that holds the workdir or a
/// writable path, such as `~/.argonauts` around a task's worktree, is left out of `deny`: the
/// driver approves nothing outside the roots and writable paths anyway, and the OS sandbox
/// still denies the rest of that area.
pub fn permission_scope(
    profile: &SandboxProfile,
    workdir: &Path,
    access: Access,
    task_forbidden: &[String],
) -> PermissionScope {
    let uses_tools = access != Access::NoTools;
    let reach = outside_reach(profile);
    let roots = match (uses_tools, reach.read || reach.after_asking) {
        (false, _) => Vec::new(),
        (true, true) => vec![workdir.to_path_buf(), PathBuf::from("/")],
        (true, false) => vec![workdir.to_path_buf()],
    };
    let writable = if uses_tools {
        profile.writable.clone()
    } else {
        Vec::new()
    };
    let read_only = profile
        .readable
        .iter()
        .filter(|path| uses_tools && !profile.writable.contains(path))
        .cloned()
        .collect();

    let granted: Vec<PathBuf> = std::iter::once(workdir)
        .chain(profile.writable.iter().map(PathBuf::as_path))
        .map(resolve_path)
        .collect();
    let deny = profile
        .deny_read
        .iter()
        .filter(|area| {
            let area = resolve_path(area);
            !granted.iter().any(|path| path.starts_with(&area))
        })
        .cloned()
        .collect();

    let mut forbidden: Vec<String> = FORBIDDEN_PATHS
        .iter()
        .map(|glob| glob.to_string())
        .collect();
    for glob in task_forbidden {
        if !forbidden.contains(glob) {
            forbidden.push(glob.clone());
        }
    }

    PermissionScope {
        roots,
        writable,
        read_only,
        deny,
        forbidden,
        network: profile.network,
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::layout::session_files;

    use super::super::profile::{ProfileInput, can_read, session_profile};
    use super::*;

    #[test]
    fn permission_scope_matches_profile() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/repo/runs/run-1/sessions/s-1");
        let tree = argonauts.join("worktrees/repo/run-1/task-a");
        let status = session.join(session_files::STATUS);
        let input = ProfileInput {
            access: Access::Build,
            agent_id: "claude-code",
            workdir: &tree,
            session_dir: &session,
            argonauts_home: &argonauts,
            user_home: &home,
            base_worktree: None,
            status_file: Some(&status),
            extra_writable: &[],
            sees_other_worktrees: false,
            commits: false,
        };
        let requested = SandboxProfile {
            network: true,
            ..SandboxProfile::default()
        };
        let profile = session_profile(&requested, &input);
        let task = vec!["migrations/**".to_owned(), ".claude/**".to_owned()];
        let scope = permission_scope(&profile, &tree, Access::Build, &task);

        assert_eq!(scope.roots, std::slice::from_ref(&tree));
        assert_eq!(scope.writable, profile.writable);
        assert!(scope.network);
        assert!(!scope.deny.is_empty());
        for area in &scope.deny {
            assert!(profile.deny_read.contains(area));
            assert!(!can_read(&profile, &area.join("x")), "{area:?}");
            assert!(!tree.starts_with(area), "{area:?} holds the workdir");
        }
        assert!(scope.deny.contains(&home.join(".ssh")));
        assert!(!scope.deny.contains(&argonauts));
        assert_eq!(&scope.forbidden[..FORBIDDEN_PATHS.len()], FORBIDDEN_PATHS);
        assert_eq!(scope.forbidden.last().unwrap(), "migrations/**");
        assert_eq!(scope.forbidden.len(), FORBIDDEN_PATHS.len() + 1);

        let quiet = permission_scope(&profile, &tree, Access::NoTools, &[]);
        assert!(quiet.roots.is_empty() && quiet.writable.is_empty());
        assert_eq!(quiet.deny, scope.deny);

        let offline = SandboxProfile {
            network: false,
            ..profile
        };
        assert!(!permission_scope(&offline, &tree, Access::ReadOnly, &[]).network);
    }
    /// A generic ACP agent reads outside its project where the session may: a direct turn in
    /// supervised once asked, a run's session there without asking, and either on autopilot.
    /// The never-list's folders stay denied. Where the mode keeps it in, or a container shows
    /// it nothing outside, its workdir is its only root.
    #[test]
    fn a_session_that_reads_outside_has_every_folder_as_a_root() {
        use argonauts_core::agents::session_runner::SandboxMode;
        use argonauts_core::safety::outside::Outside;
        use argonauts_core::settings::approvals::ApprovalMode;
        use argonauts_core::settings::outside::OutsideSettings;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let session = argonauts.join("state/app/conversations/c-1/turn-1");
        let project = home.join("work/app");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let settings = OutsideSettings::default();
        let kept_in = OutsideSettings {
            ask: Vec::new(),
            ..OutsideSettings::default()
        };
        let scope = |settings: &OutsideSettings, mode, access, unasked: bool, sandbox| {
            let mut outside = Outside::for_mode(settings, mode, Some(&project), &home);
            if unasked {
                outside = outside.without_asking();
            }
            let requested = SandboxProfile {
                mode: sandbox,
                outside,
                ..SandboxProfile::default()
            };
            let input = ProfileInput {
                access,
                agent_id: "acp",
                workdir: &project,
                session_dir: &session,
                argonauts_home: &argonauts,
                user_home: &home,
                base_worktree: None,
                status_file: None,
                extra_writable: &[],
                sees_other_worktrees: false,
                commits: false,
            };
            let profile = SandboxProfile {
                mode: sandbox,
                ..session_profile(&requested, &input)
            };
            permission_scope(&profile, &project, access, &[])
        };
        let everywhere = vec![project.clone(), PathBuf::from("/")];
        let os = SandboxMode::Os;
        for (settings, mode, access, unasked) in [
            (&settings, ApprovalMode::Supervised, Access::Direct, false),
            (&settings, ApprovalMode::Supervised, Access::Build, true),
            (&settings, ApprovalMode::Autopilot, Access::Build, false),
        ] {
            let scope = scope(settings, mode, access, unasked, os);
            assert_eq!(scope.roots, everywhere, "{mode:?} {access:?}");
            assert!(
                scope.deny.contains(&home.join(".ssh")),
                "{mode:?} {access:?}"
            );
        }
        let supervised_run = scope(&settings, ApprovalMode::Supervised, Access::Build, true, os);
        assert!(!supervised_run.writable.contains(&PathBuf::from("/")));
        for kept in [
            scope(
                &kept_in,
                ApprovalMode::Supervised,
                Access::Direct,
                false,
                os,
            ),
            scope(
                &settings,
                ApprovalMode::Autopilot,
                Access::Build,
                false,
                SandboxMode::Container,
            ),
        ] {
            assert_eq!(kept.roots, std::slice::from_ref(&project));
        }
    }
}
