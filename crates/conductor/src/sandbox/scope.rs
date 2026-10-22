//! The permission scope a session driver answers permission requests from. It mirrors the OS
//! profile, so what the driver approves is what the sandbox would allow anyway, plus the paths no
//! task may touch whoever the agent is.

use std::path::{Path, PathBuf};

use argonauts_core::driver::PermissionScope;
use argonauts_core::session_runner::{Access, SandboxProfile};

use super::profile::resolve_path;

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

/// The workdir is the only root. A no-tools session gets no roots and nothing writable, so its
/// driver rejects every request. A denied area that holds the workdir or a writable path, such
/// as `~/.argonauts` around a task's worktree, is left out of `deny`: the driver approves
/// nothing outside the roots and writable paths anyway, and the OS sandbox still denies the
/// rest of that area.
pub fn permission_scope(
    profile: &SandboxProfile,
    workdir: &Path,
    access: Access,
    task_forbidden: &[String],
) -> PermissionScope {
    let uses_tools = access != Access::NoTools;
    let roots = if uses_tools {
        vec![workdir.to_path_buf()]
    } else {
        Vec::new()
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
    use argonauts_core::layout::session_files;

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
}
