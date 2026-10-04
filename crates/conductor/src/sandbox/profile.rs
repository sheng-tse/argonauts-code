//! The profile a session runs in. It starts from what the engine asked for and adds every rule no
//! caller may loosen. Build and spike sessions write their worktree, temp, and the harness's own
//! state; read-only and no-tools sessions only temp and the harness's own state. The handoff
//! transcripts in the worktree, the harness's config, and the code it runs stay read-only, and so
//! do the worktree's git files, but for what a commit on its own branch needs when agents commit
//! (`sandbox.agents_commit`); hooks, config, and every other ref stay read-only then too. The
//! harness's records of your other sessions stay unreadable. No session reads `~/.argonauts`
//! (but its own session files and what its launch names there), any worktree but its own (and
//! the run's base worktree for read-only run sessions, or the run's other worktrees when
//! `sandbox.agents_see_other_copies` is on), other vendors' or other agents' state and
//! credentials, or `~/.ssh`, and none reaches a socket that acts for you, such as the ssh
//! agent's or docker's.
//!
//! Rules nest: the most specific path wins, so a session's own worktree can sit inside the
//! denied `~/.argonauts`, and the harness's config stays read-only inside its writable state.
//! A denied area is unwritable too, and a readable path is read-only unless it is also writable.

use std::path::{Component, Path, PathBuf};

use argonauts_core::CommandSpec;
use argonauts_core::layout::session_files;
use argonauts_core::worker::{Access, SandboxProfile};

use super::SandboxError;
use crate::handoff::transcript::WORKTREE_FOLDER;

/// Facts about one session that the profile depends on.
#[derive(Debug, Clone)]
pub struct ProfileInput<'a> {
    pub access: Access,
    /// The adapter id, such as `claude-code`, whose own state stays writable.
    pub vendor: &'a str,
    pub workdir: &'a Path,
    pub session_dir: &'a Path,
    /// `~/.argonauts`.
    pub argonauts_home: &'a Path,
    /// Your home, for credential stores and `~/.ssh`.
    pub user_home: &'a Path,
    /// For read-only run sessions: the run's base worktree, readable although it's a worktree.
    pub base_worktree: Option<&'a Path>,
    pub status_file: Option<&'a Path>,
    /// `sandbox.extra_writable`, already absolute.
    pub extra_writable: &'a [PathBuf],
    /// `sandbox.agents_see_other_copies`: the run's other worktrees are readable.
    pub sees_other_copies: bool,
    /// `sandbox.agents_commit`: a build or spike session in a linked worktree may commit on its
    /// own branch.
    pub commits: bool,
}

/// What a direct turn may never change in your checkout: Argonauts' own config and the files
/// that configure agent CLIs, which the main session and every later one load.
pub const PROTECTED_PATHS: &[&str] = &[
    ".argonauts/",
    ".claude/",
    ".mcp.json",
    ".agents/",
    ".codex/",
    ".opencode/",
    "opencode.json",
    "opencode.jsonc",
];

/// Git's files that run code, closed even when a direct turn may commit.
const DIRECT_GIT_KEPT: &[&str] = &["hooks", "config", "info"];

/// Harnesses whose state and credentials sit at known places in your home. No session reads
/// another one's.
const KNOWN_VENDORS: &[&str] = &["claude-code", "codex", "antigravity-cli", "opencode"];

/// Where a harness keeps its files in your home, relative to it.
struct VendorHome {
    /// State and credentials it writes while it runs.
    state: &'static [&'static str],
    /// Config, instructions, and code it runs, such as hooks, MCP servers, and plugins.
    /// Read-only, so a session can't plant a command that runs outside any sandbox the next time
    /// you start that harness yourself.
    config: &'static [&'static str],
    /// Records of your other sessions that it doesn't need while it runs: the prompts you typed,
    /// IDE auth tokens, and the shell setup other sessions source before each command.
    private: &'static [&'static str],
    /// Files it writes afresh before each command inside a folder that stays read-only, such as
    /// agy's `agentapi` shim in the `bin` it puts first on the agent's `PATH`. Only these names
    /// can be written there, so a session can't plant a command beside them.
    rewritten: &'static [&'static str],
}

const CLAUDE_CODE: VendorHome = VendorHome {
    state: &[".claude", "Library/Caches/claude-cli-nodejs"],
    // `~/.claude.json` holds your MCP servers. Claude Code 2.1.285 runs fine with it read-only.
    config: &[
        ".claude.json",
        ".claude.json.backup",
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".claude/CLAUDE.md",
        ".claude/agents",
        ".claude/commands",
        ".claude/hooks",
        ".claude/output-styles",
        ".claude/plugins",
        ".claude/rules",
        ".claude/skills",
        ".claude/chrome",
        ".claude/local",
    ],
    private: &[
        ".claude/history.jsonl",
        ".claude/ide",
        ".claude/shell-snapshots",
        ".claude/session-env",
        ".claude/file-history",
        ".claude/paste-cache",
        ".claude/debug",
    ],
    rewritten: &[],
};

const CODEX: VendorHome = VendorHome {
    state: &[".codex"],
    config: &[
        ".codex/config.toml",
        ".codex/managed_config.toml",
        ".codex/requirements.toml",
        ".codex/AGENTS.md",
        ".codex/AGENTS.override.md",
        ".codex/hooks.json",
        ".codex/hooks",
        ".codex/agents",
        ".codex/prompts",
        ".codex/rules",
        ".codex/skills",
        ".codex/plugins",
        ".codex/packages",
        ".codex/vendor_imports",
    ],
    private: &[
        ".codex/history.jsonl",
        ".codex/archived_sessions",
        ".codex/shell_snapshots",
    ],
    rewritten: &[],
};

const ANTIGRAVITY_CLI: VendorHome = VendorHome {
    state: &[".gemini"],
    config: &[
        ".gemini/settings.json",
        ".gemini/GEMINI.md",
        ".gemini/AGENTS.md",
        ".gemini/trustedFolders.json",
        ".gemini/config",
        ".gemini/agents",
        ".gemini/commands",
        ".gemini/extensions",
        ".gemini/hooks",
        ".gemini/skills",
        ".gemini/plugins",
        ".gemini/antigravity/mcp_config.json",
        ".gemini/antigravity-cli/settings.json",
        ".gemini/antigravity-cli/bin",
        ".gemini/antigravity-cli/builtin",
        ".gemini/antigravity-cli/updater",
    ],
    private: &[".gemini/antigravity-cli/history.jsonl"],
    rewritten: &[".gemini/antigravity-cli/bin/agentapi"],
};

const OPENCODE: VendorHome = VendorHome {
    state: &[
        ".local/share/opencode",
        ".local/state/opencode",
        ".cache/opencode",
    ],
    // Plugins there are code opencode loads, and it keeps no state there.
    config: &[".config/opencode"],
    private: &[],
    rewritten: &[],
};

const NO_HOME: VendorHome = VendorHome {
    state: &[],
    config: &[],
    private: &[],
    rewritten: &[],
};

/// The harness whose files in your home `vendor` uses. OpenCode over ACP runs the same
/// `opencode` program as the OpenCode adapter, so it keeps its state in the same places.
fn home_vendor(vendor: &str) -> &str {
    match vendor {
        "opencode-acp" => "opencode",
        other => other,
    }
}

fn vendor_home(vendor: &str) -> &'static VendorHome {
    match home_vendor(vendor) {
        "claude-code" => &CLAUDE_CODE,
        "codex" => &CODEX,
        "antigravity-cli" => &ANTIGRAVITY_CLI,
        "opencode" => &OPENCODE,
        _ => &NO_HOME,
    }
}

/// Relative to your home. Unreadable to every session, whatever its vendor.
const CREDENTIAL_STORES: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".docker",
    ".gnupg",
    ".netrc",
    ".git-credentials",
    ".config/gh",
    ".npmrc",
    ".pypirc",
    ".gem/credentials",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".cache/huggingface/token",
];

/// Relative to your home. A session that reaches a container runtime's socket can start a
/// container with your home mounted, outside every rule here.
const CONTAINER_RUNTIMES: &[&str] = &[
    ".colima",
    ".orbstack",
    ".lima",
    ".rd",
    ".local/share/containers",
    ".config/containers",
    "Library/Containers/com.docker.docker",
    "Library/Group Containers/group.com.docker",
];

/// Relative to your home. Third-party agents keep instructions, memory, and keys here.
const OTHER_AGENTS: &[&str] = &[
    ".hermes",
    ".openclaw",
    ".grok",
    ".copilot",
    ".cursor",
    ".aider",
    ".config/goose",
    ".local/share/goose",
    ".config/amp",
    ".local/share/amp",
];

/// The session's own files that its harness reads: the status file, the MCP config, the schema,
/// a manifest harness's prompt, the report the status server writes, and a home made from a
/// harness template. Its raw logs and process record stay unreadable.
const OWN_SESSION_FILES: &[&str] = &[
    session_files::STATUS,
    session_files::MCP_CONFIG,
    session_files::SCHEMA,
    session_files::PROMPT,
    session_files::REPORT,
    session_files::HOME,
];

/// Session files no launch may open, whatever it names: raw logs hold other agents' words, and
/// the process record is the reaper's.
const PRIVATE_SESSION_FILES: &[&str] = &[
    session_files::RAW_STDOUT,
    session_files::RAW_STDERR,
    session_files::RAW_STDIN,
    session_files::PROCESS,
];

/// Every rule no caller may loosen, added to what the engine asked for. The engine's own
/// writable paths and `extra_writable` count only for build and spike sessions, and only
/// outside every denied area.
pub fn session_profile(requested: &SandboxProfile, input: &ProfileInput<'_>) -> SandboxProfile {
    let home = input.user_home;
    let own = vendor_home(input.vendor);
    let in_home =
        |paths: &[&str]| -> Vec<PathBuf> { paths.iter().map(|path| home.join(path)).collect() };
    let config = in_home(own.config);
    let own_transcripts = own_transcript_dir(input.vendor, home, input.workdir);

    let mut hard_deny = vec![input.argonauts_home.to_path_buf()];
    hard_deny.extend(in_home(CREDENTIAL_STORES));
    hard_deny.extend(
        KNOWN_VENDORS
            .iter()
            .filter(|vendor| **vendor != home_vendor(input.vendor))
            .flat_map(|vendor| vendor_state_dirs(vendor, home)),
    );
    hard_deny.extend(other_agents_state(home));
    hard_deny.extend(CONTAINER_RUNTIMES.iter().map(|path| home.join(path)));
    hard_deny.extend(outside_sockets(
        std::env::var_os("SSH_AUTH_SOCK"),
        std::env::var("DOCKER_HOST").ok(),
    ));
    hard_deny.extend(in_home(own.private));
    hard_deny.extend(transcript_dirs(input.vendor, home));

    let other_copies = input
        .sees_other_copies
        .then(|| run_worktrees(input.argonauts_home, input.workdir))
        .flatten();
    let mut deny_read = hard_deny.clone();
    // With other copies open, the engine's own list of them to deny no longer applies.
    deny_read.extend(
        requested
            .deny_read
            .iter()
            .filter(|path| {
                !other_copies
                    .as_ref()
                    .is_some_and(|run| path.starts_with(run))
            })
            .cloned(),
    );

    let temp = temp_dirs();
    let mut writable = temp.clone();
    writable.extend(in_home(own.state));
    writable.extend(in_home(own.rewritten));
    writable.extend(own_transcripts.clone());
    writable.push(input.session_dir.join(session_files::REPORT));
    writable.push(input.session_dir.join(session_files::HOME));
    let builds = input.access.writes();
    if builds {
        writable.push(input.workdir.to_path_buf());
        let fixed: Vec<PathBuf> = deny_read.iter().chain(&config).cloned().collect();
        let asked = requested.writable.iter().chain(input.extra_writable);
        writable.extend(outside_all(asked, &fixed));
    }

    let mut readable = vec![input.workdir.to_path_buf()];
    readable.extend(input.status_file.map(Path::to_path_buf));
    readable.extend(
        OWN_SESSION_FILES
            .iter()
            .map(|name| input.session_dir.join(name)),
    );
    if input.access == Access::ReadOnly {
        readable.extend(input.base_worktree.map(Path::to_path_buf));
    }
    readable.extend(other_copies);
    readable.extend(outside_all(requested.readable.iter(), &hard_deny));
    if builds {
        // A replacement reads the transcripts its handoffs left in the worktree, and can't
        // rewrite the record of what the session before it did.
        readable.push(input.workdir.join(WORKTREE_FOLDER));
    }
    // Readable paths that aren't writable become read-only, even inside the writable state.
    readable.extend(config);
    readable.extend(own_transcripts);
    // A direct turn works in your checkout, which every later session and your own CLI load:
    // their config stays closed, so nothing planted there reaches past this turn, even where it
    // doesn't exist yet.
    let closed = if input.access == Access::Direct {
        protected(input.workdir)
    } else {
        Vec::new()
    };
    readable.extend(closed.iter().cloned());
    // A hook or config planted in git's files would run outside any sandbox the next time you
    // use git, so they stay read-only, and only what a commit on the session's own branch needs
    // opens up when agents commit. A direct turn commits in your checkout as its CLI would,
    // with git's hooks and config still closed.
    if input.access == Access::Direct && input.commits {
        readable.extend(
            DIRECT_GIT_KEPT
                .iter()
                .map(|path| input.workdir.join(".git").join(path)),
        );
    } else {
        readable.push(input.workdir.join(".git"));
    }
    let git = LinkedGit::of(input.workdir);
    let git_dirs = git.as_ref().map(LinkedGit::dirs).unwrap_or_default();
    readable.extend(outside_all(git_dirs.iter(), &hard_deny));
    // Your home and your checkout stay read-only even when they sit in a writable temp folder.
    let checkout = git.as_ref().and_then(LinkedGit::checkout);
    readable.extend(
        [Some(home.to_path_buf()), checkout]
            .into_iter()
            .flatten()
            .filter(|path| inside_any(path, &temp)),
    );
    if let Some(git) = git.filter(|git| input.commits && builds && git.is_of(input.workdir)) {
        writable.extend(outside_all(git.commit_paths().iter(), &hard_deny));
        readable.extend(outside_all(git.kept_read_only().iter(), &hard_deny));
    }

    SandboxProfile {
        mode: requested.mode,
        writable: dedup(writable),
        readable: dedup(readable),
        deny_read: dedup(deny_read),
        network: requested.network,
        proxy_port: requested.proxy_port,
        ask_port: requested.ask_port,
        closed,
    }
}

/// A direct turn's protected paths in `workdir`, and git's hooks folder where `core.hooksPath`
/// moves it inside your checkout, such as `.husky`, since a hook planted there runs outside any
/// sandbox at your next commit.
fn protected(workdir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = PROTECTED_PATHS
        .iter()
        .map(|path| workdir.join(path.trim_end_matches('/')))
        .collect();
    paths.extend(hooks_folder(workdir));
    paths
}

/// `core.hooksPath` when it names a folder inside `workdir`. One outside it isn't writable.
fn hooks_folder(workdir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    if !output.status.success() || value.is_empty() || value.starts_with('~') {
        return None;
    }
    let folder = lexical_normal(&workdir.join(value));
    (folder.starts_with(workdir) && folder != workdir).then_some(folder)
}

/// The git folders a linked worktree's `.git` file points at: its own, and through `commondir`
/// the main repository's, with the hooks, config, and refs every checkout shares. Either can sit
/// in a writable temp folder.
struct LinkedGit {
    own: PathBuf,
    common: Option<PathBuf>,
    /// Such as `refs/heads/argonauts/run-1/card-a`, or `None` when `HEAD` is detached.
    branch: Option<PathBuf>,
}

impl LinkedGit {
    /// `None` when `.git` is a folder or missing: a session in your own checkout never commits.
    fn of(workdir: &Path) -> Option<Self> {
        let pointer = std::fs::read_to_string(workdir.join(".git")).ok()?;
        let named = pointer
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?;
        let own = lexical_normal(&workdir.join(named.trim()));
        let common = std::fs::read_to_string(own.join("commondir"))
            .ok()
            .map(|common| lexical_normal(&own.join(common.trim())));
        let branch = std::fs::read_to_string(own.join("HEAD"))
            .ok()
            .and_then(|head| branch_ref(&head));
        Some(Self {
            own,
            common,
            branch,
        })
    }

    fn dirs(&self) -> Vec<PathBuf> {
        self.common.iter().chain([&self.own]).cloned().collect()
    }

    /// The main checkout, whose `.git` folder is the common one.
    fn checkout(&self) -> Option<PathBuf> {
        let common = self.common.as_ref()?;
        (common.file_name() == Some(".git".as_ref()))
            .then(|| common.parent().map(Path::to_path_buf))
            .flatten()
    }

    /// Whether these are git's own folders for `workdir`: the worktree's folder sits in the
    /// repository's `worktrees`, and its `gitdir` names this worktree. A `.git` file rewritten
    /// while nothing of ours held the session, as with `sandbox.mode = "tool"`, can't make some
    /// other folder writable.
    fn is_of(&self, workdir: &Path) -> bool {
        let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let in_worktrees = self.own.parent().is_some_and(|parent| {
            parent.file_name() == Some("worktrees".as_ref())
                && self
                    .common
                    .as_ref()
                    .is_some_and(|common| parent.parent().map(real) == Some(real(common)))
        });
        let points_back = std::fs::read_to_string(self.own.join("gitdir"))
            .is_ok_and(|named| real(Path::new(named.trim())) == real(&workdir.join(".git")));
        in_worktrees && points_back
    }

    /// What a commit writes: the worktree's own git folder (its index, `HEAD`'s log, and the
    /// lock on `HEAD`), the objects, and the branch's ref with its lock and log. A ref or log
    /// that isn't there yet stays out, since the Linux sandbox would make it as a folder; a
    /// packed ref then can't move, and the conductor commits after the session instead.
    fn commit_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![self.own.clone()];
        let Some(common) = &self.common else {
            return paths;
        };
        paths.push(common.join("objects"));
        if let Some(branch) = &self.branch {
            let lock = format!("{}.lock", branch.to_string_lossy());
            paths.push(common.join(lock));
            paths.extend(
                [common.join(branch), common.join("logs").join(branch)]
                    .into_iter()
                    .filter(|path| path.is_file()),
            );
        }
        paths
    }

    /// Inside what [`Self::commit_paths`] opens: the files that would point git elsewhere or turn
    /// on a worktree's own config, `HEAD` while it names the branch (so no commit lands on
    /// another), and the packs and alternates, which hold your history and say where else to
    /// read it.
    fn kept_read_only(&self) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = ["commondir", "gitdir", "config.worktree"]
            .iter()
            .map(|name| self.own.join(name))
            .collect();
        if self.branch.is_some() {
            paths.push(self.own.join("HEAD"));
        }
        if let Some(common) = &self.common {
            paths.push(common.join("objects/pack"));
            paths.push(common.join("objects/info"));
        }
        paths
    }
}

/// The branch `HEAD` names, as a path under the git folder. Anything but a plain branch name,
/// such as one that climbs out with `..`, counts as detached.
fn branch_ref(head: &str) -> Option<PathBuf> {
    let name = head.trim().strip_prefix("ref:")?.trim();
    let branch = Path::new(name);
    let plain = branch
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    (plain && branch.starts_with("refs/heads") && branch.components().count() > 2)
        .then(|| branch.to_path_buf())
}

/// Git takes a lock by making the file itself and fails when one is there, so a sandbox never
/// makes a writable lock ahead. Neither Landlock nor a bind mount can grant making that one name,
/// so on Linux and in a container an agent's commit can't move its branch, and the conductor
/// commits after the session.
pub(super) fn is_git_lock(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "lock")
}

/// The folder holding every worktree of the session's run, `~/.argonauts/worktrees/<repo>/<run>`,
/// when the session works in one of them.
fn run_worktrees(argonauts_home: &Path, workdir: &Path) -> Option<PathBuf> {
    let trees = argonauts_home.join("worktrees");
    let inside = workdir.strip_prefix(&trees).ok()?;
    let parts: Vec<_> = inside.components().collect();
    match parts.as_slice() {
        [
            Component::Normal(repo),
            Component::Normal(run),
            Component::Normal(_),
        ] => Some(trees.join(repo).join(run)),
        _ => None,
    }
}

/// Everything a harness keeps in your home, such as `~/.claude` and `~/.claude.json` for Claude
/// Code or `~/.codex` for Codex. Empty for a harness that runs from a per-session home made from
/// a template.
pub fn vendor_state_dirs(vendor: &str, user_home: &Path) -> Vec<PathBuf> {
    let home = vendor_home(vendor);
    home.state
        .iter()
        .chain(home.config)
        .map(|path| user_home.join(path))
        .collect()
}

/// The files a harness writes afresh inside a read-only folder of its (see `VendorHome`). The
/// conductor makes any that are missing before a session starts, since the session can write
/// them but not make a new name beside them.
pub fn rewritten_files(vendor: &str, user_home: &Path) -> Vec<PathBuf> {
    vendor_home(vendor)
        .rewritten
        .iter()
        .map(|path| user_home.join(path))
        .collect()
}

/// Folders where a harness keeps every session's transcript, which no session reads but for its
/// own. Codex and agy keep theirs by date or id, so they stay open to every session of the same
/// harness until each session gets a home of its own.
fn transcript_dirs(vendor: &str, user_home: &Path) -> Vec<PathBuf> {
    match vendor {
        "claude-code" => vec![user_home.join(".claude/projects")],
        _ => Vec::new(),
    }
}

/// The folder a session's own transcripts go to, inside [`transcript_dirs`].
fn own_transcript_dir(vendor: &str, user_home: &Path, workdir: &Path) -> Option<PathBuf> {
    match vendor {
        "claude-code" => claude_project_dir(user_home, workdir),
        _ => None,
    }
}

/// Claude Code 2.1.285 keeps a working directory's transcripts in `~/.claude/projects/<name>`,
/// where the name is the directory's real path with every UTF-16 unit but an ASCII letter or
/// digit turned into `-`. Past 200 units it cuts the name and adds a hash, which isn't
/// reproduced here, so such a session keeps no transcript rather than reading everyone's.
fn claude_project_dir(user_home: &Path, workdir: &Path) -> Option<PathBuf> {
    const LONGEST_NAME: usize = 200;
    let real = resolve_path(workdir);
    let name: String = real
        .to_string_lossy()
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    (name.len() <= LONGEST_NAME).then(|| user_home.join(".claude/projects").join(name))
}

/// Sockets that act for you outside the sandbox: the ssh agent and docker's. They can sit where
/// a session may go, such as temp, so each is a denied area of its own.
fn outside_sockets(
    ssh_auth_sock: Option<std::ffi::OsString>,
    docker_host: Option<String>,
) -> Vec<PathBuf> {
    let mut sockets = vec![PathBuf::from("/var/run/docker.sock")];
    sockets.extend(ssh_auth_sock.map(PathBuf::from));
    sockets.extend(
        docker_host
            .as_deref()
            .and_then(|host| host.strip_prefix("unix://"))
            .map(PathBuf::from),
    );
    sockets.retain(|path| path.is_absolute());
    sockets
}

/// The paths a launch names inside its session folder: the files the adapter writes, such as
/// Claude Code's settings or Codex's output schema, and every path argument there, such as agy's
/// agent folder. They are read-only exceptions to the denied `~/.argonauts`. The raw logs and
/// the process record stay out, whatever the launch names.
pub fn launch_paths(command: &CommandSpec, session_dir: &Path) -> Vec<PathBuf> {
    let session_dir = lexical_normal(session_dir);
    let named = command
        .files
        .iter()
        .map(|file| file.path.clone())
        .chain(command.args.iter().map(PathBuf::from));
    let mut paths = Vec::new();
    for path in named
        .filter(|path| path.is_absolute())
        .map(|path| lexical_normal(&path))
    {
        let Ok(inside) = path.strip_prefix(&session_dir) else {
            continue;
        };
        let private = inside.components().next().is_none_or(|first| {
            PRIVATE_SESSION_FILES
                .iter()
                .any(|name| first.as_os_str() == *name)
        });
        if !private {
            paths.push(path);
        }
    }
    dedup(paths)
}

/// Where a build session's package managers keep their caches: a folder in temp shared by
/// sandboxed sessions, never your own `~/.cargo` or `~/.cache`. Your later builds run what a
/// cache holds, such as a crate's `build.rs`, so a session must not be able to change yours.
pub fn build_cache_env(root: &Path) -> Vec<(String, String)> {
    const CACHES: &[(&str, &str)] = &[
        ("CARGO_HOME", "cargo"),
        ("XDG_CACHE_HOME", "xdg"),
        ("npm_config_cache", "npm"),
        ("npm_config_store_dir", "pnpm"),
        ("npm_config_devdir", "node-gyp"),
        ("YARN_CACHE_FOLDER", "yarn"),
        ("BUN_INSTALL_CACHE_DIR", "bun"),
        ("PIP_CACHE_DIR", "pip"),
        ("UV_CACHE_DIR", "uv"),
        ("GOMODCACHE", "go/mod"),
        ("GOCACHE", "go/build"),
        ("GRADLE_USER_HOME", "gradle"),
    ];
    CACHES
        .iter()
        .map(|(name, folder)| {
            let path = root.join(folder).to_string_lossy().into_owned();
            ((*name).to_owned(), path)
        })
        .collect()
}

/// The folder [`build_cache_env`] points at.
pub fn build_cache_root() -> PathBuf {
    std::env::temp_dir().join("argonauts-build-caches")
}

/// State and credentials of third-party agents, such as `~/.hermes` and `~/.openclaw`, which no
/// session may read.
pub fn other_agents_state(user_home: &Path) -> Vec<PathBuf> {
    OTHER_AGENTS
        .iter()
        .map(|path| user_home.join(path))
        .collect()
}

/// `$TMPDIR` and the system temp folders.
pub fn temp_dirs() -> Vec<PathBuf> {
    let system: &[&str] = if cfg!(target_os = "macos") {
        &["/private/tmp", "/private/var/folders"]
    } else {
        &["/tmp", "/var/tmp"]
    };
    let mut dirs = vec![std::env::temp_dir()];
    dirs.extend(system.iter().map(PathBuf::from));
    dedup(dirs)
}

/// Whether `profile` lets a session read `path`'s contents.
pub fn can_read(profile: &SandboxProfile, path: &Path) -> bool {
    let path = resolve_path(path);
    ordered_rules(profile)
        .iter()
        .filter(|rule| path.starts_with(&rule.resolved))
        .fold(true, |allowed, rule| match rule.kind {
            RuleKind::Deny => false,
            RuleKind::Read | RuleKind::ReadOnly => true,
            RuleKind::Write | RuleKind::Stat => allowed,
        })
}

/// Whether `profile` lets a session create or change `path`. Device files such as `/dev/null`
/// are left out, since every profile allows them.
pub fn can_write(profile: &SandboxProfile, path: &Path) -> bool {
    let path = resolve_path(path);
    ordered_rules(profile)
        .iter()
        .filter(|rule| path.starts_with(&rule.resolved))
        .fold(false, |allowed, rule| match rule.kind {
            RuleKind::Deny | RuleKind::ReadOnly => false,
            RuleKind::Write => true,
            RuleKind::Read | RuleKind::Stat => allowed,
        })
}

/// Every path must be absolute and free of control characters, so it means one thing to the
/// OS sandbox.
pub fn check_profile(profile: &SandboxProfile) -> Result<(), SandboxError> {
    let paths = profile
        .writable
        .iter()
        .chain(&profile.readable)
        .chain(&profile.deny_read);
    for path in paths {
        if !path.is_absolute() {
            return Err(SandboxError::Invalid(format!(
                "{} is not an absolute path",
                path.display()
            )));
        }
        if path.to_string_lossy().chars().any(char::is_control) {
            return Err(SandboxError::Invalid(format!(
                "{} has a control character",
                path.display()
            )));
        }
    }
    Ok(())
}

/// What a rule does to paths under it. The order is the tie-break for one path: a path both
/// writable and denied stays denied, and a readable exception beats its own deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RuleKind {
    Write,
    /// Denies reads and writes.
    Deny,
    /// Lets a session stat one folder on the way to an exception inside a denied area. git
    /// stats every parent of its worktree, and fails without it.
    Stat,
    /// A readable path that is also writable.
    Read,
    /// A readable path that isn't writable, even inside a writable temp folder, such as a
    /// read-only session's checkout under `/tmp`.
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathRule {
    pub kind: RuleKind,
    /// As the profile gives it.
    pub path: PathBuf,
    /// With symlinks resolved, as the OS sandbox sees it: `/tmp` is `/private/tmp` on macOS.
    pub resolved: PathBuf,
}

/// The profile's rules, shallowest path first, so that the last rule matching a path is the
/// most specific one. That is how Seatbelt resolves overlapping rules.
pub(crate) fn ordered_rules(profile: &SandboxProfile) -> Vec<PathRule> {
    let rule = |kind, path: &PathBuf| PathRule {
        kind,
        path: path.clone(),
        resolved: resolve_path(path),
    };
    let mut rules: Vec<PathRule> = profile
        .writable
        .iter()
        .map(|path| rule(RuleKind::Write, path))
        .chain(
            profile
                .deny_read
                .iter()
                .map(|path| rule(RuleKind::Deny, path)),
        )
        .collect();
    let writable: Vec<PathBuf> = rules
        .iter()
        .filter(|rule| rule.kind == RuleKind::Write)
        .map(|rule| rule.resolved.clone())
        .collect();
    for path in &profile.readable {
        let mut readable = rule(RuleKind::ReadOnly, path);
        if writable.contains(&readable.resolved) {
            readable.kind = RuleKind::Read;
        }
        rules.push(readable);
    }

    let denied: Vec<PathBuf> = rules
        .iter()
        .filter(|rule| rule.kind == RuleKind::Deny)
        .map(|rule| rule.resolved.clone())
        .collect();
    let mut stats = Vec::new();
    for exception in rules.iter().filter(|rule| rule.kind != RuleKind::Deny) {
        for area in &denied {
            let mut folder = exception.resolved.parent();
            while let Some(dir) = folder.filter(|dir| dir.starts_with(area)) {
                stats.push(dir.to_path_buf());
                folder = dir.parent();
            }
        }
    }
    rules.extend(dedup(stats).into_iter().map(|dir| PathRule {
        kind: RuleKind::Stat,
        path: dir.clone(),
        resolved: dir,
    }));

    rules.sort_by_key(|rule| (rule.resolved.components().count(), rule.kind));
    rules
}

/// `path` with `.` and `..` removed and symlinks resolved as far as the path exists. The rest
/// is kept as written, since a file may not exist yet.
fn inside_any(path: &Path, folders: &[PathBuf]) -> bool {
    let path = resolve_path(path);
    folders
        .iter()
        .any(|folder| path.starts_with(resolve_path(folder)))
}

pub fn resolve_path(path: &Path) -> PathBuf {
    let normal = lexical_normal(path);
    let mut existing = normal.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(real, |path: PathBuf, name| path.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return normal,
        }
    }
}

fn lexical_normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// The absolute paths in `paths` that sit outside every area in `denied`.
fn outside_all<'a>(paths: impl Iterator<Item = &'a PathBuf>, denied: &[PathBuf]) -> Vec<PathBuf> {
    let denied: Vec<PathBuf> = denied.iter().map(|path| resolve_path(path)).collect();
    paths
        .filter(|path| path.is_absolute())
        .filter(|path| {
            let resolved = resolve_path(path);
            let inside = denied.iter().any(|area| resolved.starts_with(area));
            if inside {
                tracing::warn!(
                    path = %path.display(),
                    "left a path out of the sandbox profile: it is inside a denied area"
                );
            }
            !inside
        })
        .cloned()
        .collect()
}

fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = Vec::with_capacity(paths.len());
    for path in paths {
        if !seen.contains(&path) {
            seen.push(path);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Layout {
        _temp: tempfile::TempDir,
        home: PathBuf,
        argonauts: PathBuf,
        run: PathBuf,
        session: PathBuf,
        own_tree: PathBuf,
        other_tree: PathBuf,
        base_tree: PathBuf,
    }

    fn layout() -> Layout {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let home = root.join("home");
        let argonauts = home.join(".argonauts");
        let run = argonauts.join("state/repo-1/runs/run-1");
        let session = run.join("sessions/s-1");
        let trees = argonauts.join("worktrees/repo-1/run-1");
        for dir in [&session, &trees.join("card-a"), &trees.join("card-b")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Layout {
            home,
            argonauts,
            session,
            own_tree: trees.join("card-a"),
            other_tree: trees.join("card-b"),
            base_tree: trees.join("base"),
            run,
            _temp: temp,
        }
    }

    fn profile_for(l: &Layout, access: Access, vendor: &str, extra: &[PathBuf]) -> SandboxProfile {
        let status = l.session.join(session_files::STATUS);
        let workdir = match access {
            Access::Build | Access::Spike | Access::Direct => l.own_tree.clone(),
            Access::ReadOnly => l.base_tree.clone(),
            Access::NoTools => l.session.join("work"),
        };
        let input = ProfileInput {
            access,
            vendor,
            workdir: &workdir,
            session_dir: &l.session,
            argonauts_home: &l.argonauts,
            user_home: &l.home,
            base_worktree: Some(&l.base_tree),
            status_file: Some(&status),
            extra_writable: extra,
            sees_other_copies: false,
            commits: false,
        };
        session_profile(&SandboxProfile::default(), &input)
    }

    #[test]
    fn sandbox_profile_denies_credentials_and_state() {
        let l = layout();
        for access in [
            Access::Build,
            Access::Spike,
            Access::ReadOnly,
            Access::NoTools,
        ] {
            let profile = profile_for(&l, access, "claude-code", &[]);
            for secret in [
                l.run.join("approve.token"),
                l.run.join("control.sock"),
                l.run.join("pending-answers.jsonl"),
                l.run.join("drafts/d-1.md"),
                l.run.join("sessions/s-2/raw.stdout.jsonl"),
                l.session.join(session_files::RAW_STDOUT),
                l.argonauts.join("config.toml"),
                l.home.join(".ssh/id_ed25519"),
                l.home.join(".aws/credentials"),
                l.home.join(".codex/auth.json"),
                l.home.join(".gemini/oauth_creds.json"),
                l.home.join(".local/share/opencode/auth.json"),
                l.home.join(".colima/default/docker.sock"),
                l.home.join(".orbstack/run/docker.sock"),
                l.home
                    .join("Library/Containers/com.docker.docker/Data/docker.raw.sock"),
                l.home.join(".config/containers/auth.json"),
            ] {
                assert!(!can_read(&profile, &secret), "{access:?} reads {secret:?}");
                assert!(
                    !can_write(&profile, &secret),
                    "{access:?} writes {secret:?}"
                );
            }
            assert!(can_read(&profile, &l.session.join(session_files::STATUS)));
            assert!(can_read(
                &profile,
                &l.session.join(session_files::MCP_CONFIG)
            ));
            assert!(!can_write(&profile, &l.session.join(session_files::STATUS)));
            assert!(can_write(&profile, &l.session.join(session_files::REPORT)));
            assert!(can_write(&profile, &l.home.join(".claude/todos/x.json")));
            assert!(can_write(&profile, &std::env::temp_dir().join("scratch")));
        }
    }

    #[test]
    fn a_replacement_reads_its_handoff_transcripts_but_cant_change_them() {
        let l = layout();
        let transcript = l.own_tree.join(".argonauts/handoff/1/transcript.md");
        for access in [Access::Build, Access::Spike] {
            for vendor in ["claude-code", "codex", "antigravity-cli", "opencode"] {
                let profile = profile_for(&l, access, vendor, &[]);
                assert!(can_read(&profile, &transcript), "{access:?} {vendor}");
                assert!(!can_write(&profile, &transcript), "{access:?} {vendor}");
                assert!(can_write(&profile, &l.own_tree.join("src/lib.rs")));
                // Only its own worktree's, and never the run's copy in the state folder.
                let other = l.other_tree.join(".argonauts/handoff/1/transcript.md");
                assert!(!can_read(&profile, &other), "{access:?} {vendor}");
                let kept = l.run.join("cards/card-a/handoff-1/transcript.md");
                assert!(!can_read(&profile, &kept), "{access:?} {vendor}");
            }
        }
    }

    #[test]
    fn vendor_config_stays_read_only() {
        let l = layout();
        let config = [
            ("claude-code", ".claude.json"),
            ("claude-code", ".claude/settings.json"),
            ("claude-code", ".claude/settings.local.json"),
            ("claude-code", ".claude/hooks/pre.sh"),
            ("claude-code", ".claude/agents/a.md"),
            ("claude-code", ".claude/plugins/p/hooks/hooks.json"),
            ("claude-code", ".claude/CLAUDE.md"),
            ("codex", ".codex/config.toml"),
            ("codex", ".codex/rules/default.rules"),
            ("codex", ".codex/AGENTS.md"),
            ("antigravity-cli", ".gemini/settings.json"),
            ("antigravity-cli", ".gemini/GEMINI.md"),
            ("antigravity-cli", ".gemini/antigravity/mcp_config.json"),
            ("antigravity-cli", ".gemini/antigravity-cli/bin/agy"),
            ("opencode", ".config/opencode/opencode.json"),
            ("opencode", ".config/opencode/plugin/p.ts"),
        ];
        for access in [
            Access::Build,
            Access::Spike,
            Access::ReadOnly,
            Access::NoTools,
        ] {
            for (vendor, path) in config {
                let profile = profile_for(&l, access, vendor, &[]);
                let path = l.home.join(path);
                assert!(
                    can_read(&profile, &path),
                    "{access:?} {vendor} reads {path:?}"
                );
                assert!(
                    !can_write(&profile, &path),
                    "{access:?} {vendor} writes {path:?}"
                );
            }
        }
        // agy's `agentapi` shim is the one name its `bin` takes.
        for access in [Access::Build, Access::ReadOnly] {
            let profile = profile_for(&l, access, "antigravity-cli", &[]);
            let bin = l.home.join(".gemini/antigravity-cli/bin");
            assert!(can_write(&profile, &bin.join("agentapi")), "{access:?}");
            assert!(!can_write(&profile, &bin.join("git")), "{access:?}");
            assert!(
                !can_write(&profile, &bin.join("agentapi.d/x")),
                "{access:?}"
            );
        }
        assert_eq!(
            rewritten_files("antigravity-cli", &l.home),
            [l.home.join(".gemini/antigravity-cli/bin/agentapi")]
        );
        assert!(rewritten_files("codex", &l.home).is_empty());
        // The state around the config stays writable.
        for (vendor, path) in [
            ("codex", ".codex/sessions/2026/09/30/rollout-a.jsonl"),
            ("codex", ".codex/auth.json"),
            (
                "antigravity-cli",
                ".gemini/antigravity-cli/conversations/a.pb",
            ),
            ("opencode", ".local/share/opencode/storage/a.json"),
        ] {
            let profile = profile_for(&l, Access::ReadOnly, vendor, &[]);
            assert!(can_write(&profile, &l.home.join(path)), "{vendor} {path}");
        }
        // Not even `extra_writable` opens config.
        let extra = [
            l.home.join(".claude/settings.json"),
            l.home.join(".claude/hooks"),
        ];
        let profile = profile_for(&l, Access::Build, "claude-code", &extra);
        for path in &extra {
            assert!(!profile.writable.contains(path), "{path:?}");
        }
        assert!(!can_write(&profile, &extra[0]));
        assert!(!can_write(&profile, &extra[1].join("pre.sh")));
    }

    // OpenCode 1.18.34 over ACP stopped at start with EEXIST on mkdir ~/.local/share/opencode,
    // because that folder was denied to every harness but `opencode`.
    #[test]
    fn opencode_over_acp_keeps_opencodes_state() {
        let l = layout();
        let state = l.home.join(".local/share/opencode/storage/a.json");
        for access in [Access::ReadOnly, Access::Build] {
            let profile = profile_for(&l, access, "opencode-acp", &[]);
            assert!(can_write(&profile, &state), "{access:?}");
            assert!(can_read(
                &profile,
                &l.home.join(".config/opencode/opencode.json")
            ));
            assert!(!can_write(
                &profile,
                &l.home.join(".config/opencode/opencode.json")
            ));
            assert!(!can_read(&profile, &l.home.join(".codex/auth.json")));
        }
        let profile = profile_for(&l, Access::Build, "codex", &[]);
        assert!(!can_read(&profile, &state));
    }

    #[test]
    fn a_session_reads_only_its_own_transcripts() {
        let l = layout();
        for access in [
            Access::Build,
            Access::Spike,
            Access::ReadOnly,
            Access::NoTools,
        ] {
            let profile = profile_for(&l, access, "claude-code", &[]);
            let workdir = match access {
                Access::Build | Access::Spike | Access::Direct => l.own_tree.clone(),
                Access::ReadOnly => l.base_tree.clone(),
                Access::NoTools => l.session.join("work"),
            };
            let own = claude_project_dir(&l.home, &workdir).unwrap();
            let theirs = claude_project_dir(&l.home, &l.other_tree).unwrap();
            assert!(can_read(&profile, &own.join("s-1.jsonl")), "{access:?}");
            assert!(can_write(&profile, &own.join("s-1.jsonl")), "{access:?}");
            for record in [
                theirs.join("s-2.jsonl"),
                l.home
                    .join(".claude/projects/-Users-you-code-app/s-3.jsonl"),
                l.home.join(".claude/history.jsonl"),
                l.home.join(".claude/ide/1234.lock"),
                l.home.join(".claude/shell-snapshots/snapshot-zsh-1.sh"),
                l.home.join(".claude/file-history/s-2/a@v1"),
            ] {
                assert!(!can_read(&profile, &record), "{access:?} reads {record:?}");
                assert!(
                    !can_write(&profile, &record),
                    "{access:?} writes {record:?}"
                );
            }
        }
        let codex = profile_for(&l, Access::Build, "codex", &[]);
        for record in [".codex/history.jsonl", ".codex/shell_snapshots/a.sh"] {
            assert!(!can_read(&codex, &l.home.join(record)), "{record}");
        }
    }

    #[test]
    fn claude_project_dir_matches_claude_code() {
        let home = Path::new("/home/you");
        assert_eq!(
            claude_project_dir(home, Path::new("/nowhere/my_app.v2/ü")),
            Some(PathBuf::from(
                "/home/you/.claude/projects/-nowhere-my-app-v2--"
            ))
        );
        let deep = PathBuf::from("/nowhere").join("a".repeat(300));
        assert_eq!(claude_project_dir(home, &deep), None);
    }

    #[test]
    fn sandbox_profile_denies_other_worktrees() {
        let l = layout();
        let build = profile_for(&l, Access::Build, "codex", &[]);
        assert!(can_read(&build, &l.own_tree.join("src/lib.rs")));
        assert!(can_write(&build, &l.own_tree.join("src/lib.rs")));
        for other in [&l.other_tree, &l.base_tree] {
            assert!(!can_read(&build, &other.join("tests/hidden.rs")));
            assert!(!can_write(&build, &other.join("tests/hidden.rs")));
        }

        let review = profile_for(&l, Access::ReadOnly, "codex", &[]);
        assert!(can_read(&review, &l.base_tree.join("src/lib.rs")));
        assert!(!can_write(&review, &l.base_tree.join("src/lib.rs")));
        assert!(!can_read(&review, &l.own_tree.join("src/lib.rs")));
        assert!(!can_read(&review, &l.other_tree.join("src/lib.rs")));

        // A build session never gets the base worktree, even when the caller passes one.
        assert!(!build.readable.contains(&l.base_tree));
    }

    #[test]
    fn sandbox_profile_denies_other_agents_state() {
        let l = layout();
        let profile = profile_for(&l, Access::Build, "claude-code", &[]);
        for path in [
            ".hermes/.env",
            ".hermes/memories/MEMORY.md",
            ".openclaw/openclaw.json",
            ".grok/sessions/a.json",
            ".config/goose/config.yaml",
        ] {
            assert!(!can_read(&profile, &l.home.join(path)), "reads {path}");
        }
        // Another vendor's cache stays denied, wherever the temp folder puts the home.
        assert!(!can_write(&profile, &l.home.join(".cache/opencode/x")));
        assert!(!can_read(&profile, &l.home.join(".cache/opencode/x")));
        // Its own state is writable, other vendors' state isn't.
        let codex = profile_for(&l, Access::ReadOnly, "codex", &[]);
        assert!(can_write(&codex, &l.home.join(".codex/sessions/a.jsonl")));
        assert!(!can_read(&codex, &l.home.join(".claude.json")));
        assert!(!can_read(&codex, &l.home.join(".claude/projects/a.jsonl")));
    }

    #[test]
    fn build_sessions_never_write_your_package_caches() {
        let l = layout();
        let profile = profile_for(&l, Access::Build, "codex", &[]);
        // In your home only the harness's own state, the worktree, and the session's own files
        // are writable.
        let vendor_state = vendor_state_dirs("codex", &l.home);
        for path in profile
            .writable
            .iter()
            .filter(|path| path.starts_with(&l.home))
        {
            let own = vendor_state.contains(path) || path.starts_with(&l.session);
            assert!(own || *path == l.own_tree, "{path:?}");
        }
        let root = PathBuf::from("/tmp/caches");
        let env = build_cache_env(&root);
        let value = |name: &str| {
            env.iter()
                .find(|(set, _)| set == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(value("CARGO_HOME").as_deref(), Some("/tmp/caches/cargo"));
        assert_eq!(value("XDG_CACHE_HOME").as_deref(), Some("/tmp/caches/xdg"));
        assert!(
            env.iter()
                .all(|(_, value)| value.starts_with("/tmp/caches/"))
        );
        assert!(build_cache_root().starts_with(std::env::temp_dir()));
    }

    #[test]
    fn outside_sockets_cover_the_ssh_agent_and_docker() {
        let sockets = outside_sockets(
            Some("/private/tmp/com.apple.launchd.x/Listeners".into()),
            Some("unix:///Users/me/.colima/docker.sock".into()),
        );
        assert_eq!(
            sockets,
            [
                PathBuf::from("/var/run/docker.sock"),
                PathBuf::from("/private/tmp/com.apple.launchd.x/Listeners"),
                PathBuf::from("/Users/me/.colima/docker.sock"),
            ]
        );
        let sockets = outside_sockets(Some("relative".into()), Some("tcp://host:2375".into()));
        assert_eq!(sockets, [PathBuf::from("/var/run/docker.sock")]);
    }

    #[test]
    fn launch_paths_open_only_what_the_launch_names_in_its_session() {
        let session = Path::new("/state/runs/r/sessions/s-1");
        let mut command = CommandSpec::new("claude", "/w").args([
            "--settings",
            "/state/runs/r/sessions/s-1/claude-settings.json",
            "--add-dir",
            "/state/runs/r/sessions/s-1/agy-agent",
            "--output-schema",
            "/state/runs/r/sessions/s-1/../s-2/codex-output-schema.json",
            "--raw",
            "/state/runs/r/sessions/s-1/raw.stdout.jsonl",
            "--whole-session",
            "/state/runs/r/sessions/s-1",
            "/elsewhere/file.json",
            "relative/file.json",
        ]);
        command.files.push(argonauts_core::SpecFile {
            path: session.join("codex-output-schema.json"),
            contents: "{}".into(),
        });
        command.files.push(argonauts_core::SpecFile {
            path: session.join("process.json"),
            contents: "{}".into(),
        });
        assert_eq!(
            launch_paths(&command, session),
            [
                session.join("codex-output-schema.json"),
                session.join("claude-settings.json"),
                session.join("agy-agent"),
            ]
        );
    }

    #[test]
    fn readonly_profile_cannot_write_workdir() {
        let l = layout();
        let extra = [l.home.join("extra-cache")];
        for access in [Access::ReadOnly, Access::NoTools] {
            let profile = profile_for(&l, access, "codex", &extra);
            let workdir = match access {
                Access::ReadOnly => l.base_tree.clone(),
                _ => l.session.join("work"),
            };
            assert!(can_read(&profile, &workdir.join("README.md")));
            assert!(!can_write(&profile, &workdir.join("README.md")));
            assert!(!can_write(&profile, &l.run.join("pending-answers.jsonl")));
            assert!(can_write(&profile, &l.home.join(".codex/log/x")));
            // The test's home sits in temp, which is writable, so check the lists themselves.
            for build_only in [l.home.join(".cargo/registry"), l.home.join("extra-cache")] {
                assert!(!profile.writable.contains(&build_only), "{build_only:?}");
            }
        }
    }

    #[test]
    fn git_files_stay_read_only_in_a_writable_worktree() {
        let l = layout();
        for tree in [&l.own_tree, &l.base_tree] {
            std::fs::create_dir_all(tree.join(".git/hooks")).unwrap();
        }
        for access in [Access::Build, Access::Spike, Access::ReadOnly] {
            let profile = profile_for(&l, access, "claude-code", &[]);
            let workdir = match access {
                Access::ReadOnly => &l.base_tree,
                _ => &l.own_tree,
            };
            for path in ["HEAD", "index.lock", "hooks/pre-commit", "config"] {
                let path = workdir.join(".git").join(path);
                assert!(!can_write(&profile, &path), "{access:?} writes {path:?}");
            }
            assert!(can_read(&profile, &workdir.join(".git/HEAD")), "{access:?}");
        }
        let build = profile_for(&l, Access::Build, "codex", &[]);
        assert!(can_write(&build, &l.own_tree.join("src/lib.rs")));
        assert!(can_write(&build, &l.own_tree.join(".gitignore")));
    }

    #[test]
    fn a_linked_worktree_cannot_write_the_repository_it_points_at() {
        let l = layout();
        // The main repository in temp, which every session may write.
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().canonicalize().unwrap().join("repo");
        let own_git_dir = repo.join(".git/worktrees/card-a");
        std::fs::create_dir_all(&own_git_dir).unwrap();
        std::fs::write(own_git_dir.join("commondir"), "../..\n").unwrap();
        std::fs::write(
            l.own_tree.join(".git"),
            format!("gitdir: {}\n", own_git_dir.display()),
        )
        .unwrap();
        let profile = profile_for(&l, Access::Build, "codex", &[]);
        for path in [
            repo.join(".git/hooks/pre-commit"),
            repo.join(".git/config"),
            repo.join(".git/refs/heads/main"),
            own_git_dir.join("index.lock"),
            l.own_tree.join(".git"),
        ] {
            assert!(!can_write(&profile, &path), "writes {path:?}");
            assert!(can_read(&profile, &path), "can't read {path:?}");
        }
        // The checkout itself stays read-only too, though temp is writable around it.
        assert!(!can_write(&profile, &repo.join("README.md")));
        assert!(can_read(&profile, &repo.join("README.md")));
        assert!(can_write(&profile, &temp.path().join("scratch")));
        assert!(can_write(&profile, &l.own_tree.join("src/lib.rs")));
    }

    /// A profile for `workdir` with `sandbox.agents_see_other_copies` and
    /// `sandbox.agents_commit` as given.
    fn switched(
        l: &Layout,
        access: Access,
        workdir: &Path,
        requested: &SandboxProfile,
        sees_other_copies: bool,
        commits: bool,
    ) -> SandboxProfile {
        let input = ProfileInput {
            access,
            vendor: "codex",
            workdir,
            session_dir: &l.session,
            argonauts_home: &l.argonauts,
            user_home: &l.home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_copies,
            commits,
        };
        session_profile(requested, &input)
    }

    /// A card's worktree linked to a repository in temp, on its own branch.
    struct Repo {
        _temp: tempfile::TempDir,
        common: PathBuf,
        own: PathBuf,
    }

    const BRANCH: &str = "refs/heads/argonauts/run-1/card-a";

    fn linked_repo(l: &Layout, head: &str) -> Repo {
        let temp = tempfile::tempdir().unwrap();
        let common = temp.path().canonicalize().unwrap().join("repo/.git");
        let own = common.join("worktrees/card-a");
        for dir in [
            own.clone(),
            common.join("refs/heads/argonauts/run-1"),
            common.join("logs/refs/heads/argonauts/run-1"),
            common.join("objects/pack"),
            common.join("hooks"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(own.join("commondir"), "../..\n").unwrap();
        let back = format!("{}\n", l.own_tree.join(".git").display());
        std::fs::write(own.join("gitdir"), back).unwrap();
        std::fs::write(own.join("HEAD"), head).unwrap();
        std::fs::write(common.join(BRANCH), "0123\n").unwrap();
        std::fs::write(common.join("logs").join(BRANCH), "").unwrap();
        let pointer = format!("gitdir: {}\n", own.display());
        std::fs::write(l.own_tree.join(".git"), pointer).unwrap();
        Repo {
            _temp: temp,
            common,
            own,
        }
    }

    #[test]
    fn agents_commit_opens_what_a_commit_on_its_own_branch_needs() {
        let l = layout();
        let r = linked_repo(&l, &format!("ref: {BRANCH}\n"));
        let plain = SandboxProfile::default();
        let profile = switched(&l, Access::Build, &l.own_tree, &plain, false, true);
        for path in [
            r.own.join("index"),
            r.own.join("index.lock"),
            r.own.join("HEAD.lock"),
            r.own.join("logs/HEAD"),
            r.own.join("COMMIT_EDITMSG"),
            r.common.join("objects/ab/cdef0123"),
            r.common.join(BRANCH),
            r.common.join(format!("{BRANCH}.lock")),
            r.common.join("logs").join(BRANCH),
        ] {
            assert!(can_write(&profile, &path), "can't write {path:?}");
        }
        for path in [
            r.own.join("HEAD"),
            r.own.join("commondir"),
            r.own.join("gitdir"),
            r.own.join("config.worktree"),
            r.common.join("config"),
            r.common.join("hooks/pre-commit"),
            r.common.join("packed-refs"),
            r.common.join("refs/heads/main"),
            r.common.join("refs/heads/main.lock"),
            r.common.join("refs/heads/argonauts/run-1/integration"),
            r.common.join("refs/heads/argonauts/run-1/integration.lock"),
            r.common.join("objects/pack/pack-1.pack"),
            r.common.join("objects/info/alternates"),
            l.own_tree.join(".git"),
        ] {
            assert!(!can_write(&profile, &path), "writes {path:?}");
            assert!(can_read(&profile, &path), "can't read {path:?}");
        }

        // Off, or in a session that only reads, git's files stay read-only.
        let off = switched(&l, Access::Build, &l.own_tree, &plain, false, false);
        let reading = switched(&l, Access::ReadOnly, &l.own_tree, &plain, false, true);
        for profile in [off, reading] {
            for path in [r.own.join("index.lock"), r.common.join(BRANCH)] {
                assert!(!can_write(&profile, &path), "writes {path:?}");
            }
        }
    }

    /// A `.git` file rewritten to name another folder, as an agent could while only its tool's
    /// own sandbox held it, opens nothing for writing.
    #[test]
    fn a_rewritten_git_file_opens_nothing() {
        let l = layout();
        let _repo = linked_repo(&l, &format!("ref: {BRANCH}\n"));
        let plain = SandboxProfile::default();
        let elsewhere = l.home.join("Documents/worktrees/card-a");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("commondir"), "../..\n").unwrap();
        std::fs::write(elsewhere.join("HEAD"), format!("ref: {BRANCH}\n")).unwrap();
        let pointer = format!("gitdir: {}\n", elsewhere.display());
        std::fs::write(l.own_tree.join(".git"), pointer).unwrap();
        let profile = switched(&l, Access::Build, &l.own_tree, &plain, false, true);
        for path in [
            elsewhere.join("index"),
            l.home.join("Documents/objects/ab/cd"),
            l.home.join("Documents/notes.txt"),
        ] {
            assert!(!can_write(&profile, &path), "writes {path:?}");
        }
    }

    #[test]
    fn a_detached_worktree_commits_on_its_own_head_only() {
        let l = layout();
        let r = linked_repo(&l, "0123456789abcdef\n");
        let plain = SandboxProfile::default();
        let profile = switched(&l, Access::Build, &l.own_tree, &plain, false, true);
        assert!(can_write(&profile, &r.own.join("HEAD")));
        assert!(!can_write(&profile, &r.common.join(BRANCH)));
        assert!(!can_write(&profile, &r.own.join("commondir")));
    }

    #[test]
    fn only_a_plain_branch_counts_as_head_naming_one() {
        assert_eq!(
            branch_ref("ref: refs/heads/argonauts/run-1/card-a\n"),
            Some(PathBuf::from("refs/heads/argonauts/run-1/card-a"))
        );
        for head in [
            "0123456789abcdef",
            "ref: refs/heads/../../config",
            "ref: /etc/passwd",
            "ref: refs/heads",
            "ref: refs/tags/v1",
        ] {
            assert_eq!(branch_ref(head), None, "{head}");
        }
    }

    #[test]
    fn agents_see_other_copies_reads_the_runs_other_worktrees() {
        let l = layout();
        // The engine lists the other copies to deny, which the switch overrides.
        let requested = SandboxProfile {
            deny_read: vec![l.other_tree.clone(), l.home.join("private")],
            ..SandboxProfile::default()
        };
        let open = switched(&l, Access::Build, &l.own_tree, &requested, true, false);
        for other in [&l.other_tree, &l.base_tree] {
            let file = other.join("src/lib.rs");
            assert!(can_read(&open, &file), "can't read {file:?}");
            assert!(!can_write(&open, &file), "writes {file:?}");
        }
        assert!(can_write(&open, &l.own_tree.join("src/lib.rs")));
        assert!(!can_read(&open, &l.home.join("private/notes")));
        assert!(!can_read(&open, &l.session.join(session_files::RAW_STDOUT)));
        assert!(!can_read(&open, &l.run.join("approve.token")));

        let closed = switched(&l, Access::Build, &l.own_tree, &requested, false, false);
        assert!(!can_read(&closed, &l.other_tree.join("src/lib.rs")));
        assert!(!can_read(&closed, &l.base_tree.join("src/lib.rs")));

        // A session outside the run's worktrees, such as chat in your checkout, gets nothing more.
        let checkout = l.home.join("code/app");
        let chat = switched(&l, Access::Build, &checkout, &requested, true, false);
        assert!(!can_read(&chat, &l.other_tree.join("src/lib.rs")));
    }

    #[test]
    fn the_runs_worktrees_are_found_only_from_one_of_them() {
        let home = Path::new("/h/.argonauts");
        let run = home.join("worktrees/repo-1/run-1");
        assert_eq!(run_worktrees(home, &run.join("card-a")), Some(run.clone()));
        assert_eq!(run_worktrees(home, &run.join("base")), Some(run.clone()));
        for elsewhere in [
            run.clone(),
            run.join("card-a/src"),
            home.join("state/repo-1"),
            PathBuf::from("/h/code/app"),
        ] {
            assert_eq!(run_worktrees(home, &elsewhere), None, "{elsewhere:?}");
        }
    }

    fn linked_git_dirs(workdir: &Path) -> Vec<PathBuf> {
        LinkedGit::of(workdir)
            .map(|git| git.dirs())
            .unwrap_or_default()
    }

    #[test]
    fn linked_git_dirs_follow_the_pointer_and_commondir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(tree.join(".git")).unwrap();
        assert!(linked_git_dirs(&tree).is_empty());
        assert!(linked_git_dirs(&root.join("no-repo")).is_empty());

        let linked = root.join("linked");
        let own = root.join("repo/.git/worktrees/linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(
            linked.join(".git"),
            "gitdir: ../repo/.git/worktrees/linked\n",
        )
        .unwrap();
        // No `commondir`, as in a submodule: only the folder it names.
        assert_eq!(linked_git_dirs(&linked), std::slice::from_ref(&own));
        std::fs::write(own.join("commondir"), "../..\n").unwrap();
        assert_eq!(linked_git_dirs(&linked), [root.join("repo/.git"), own]);
    }

    #[test]
    fn readonly_checkout_in_temp_stays_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().canonicalize().unwrap();
        let l = layout();
        let input = ProfileInput {
            access: Access::ReadOnly,
            vendor: "codex",
            workdir: &repo,
            session_dir: &l.session,
            argonauts_home: &l.argonauts,
            user_home: &l.home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_copies: false,
            commits: false,
        };
        let profile = session_profile(&SandboxProfile::default(), &input);
        assert!(can_write(
            &profile,
            &repo.parent().unwrap().join("elsewhere")
        ));
        assert!(can_read(&profile, &repo.join("src/lib.rs")));
        assert!(!can_write(&profile, &repo.join("src/lib.rs")));
    }

    #[test]
    fn extra_writable_never_opens_a_denied_area() {
        let l = layout();
        let extra = [
            l.home.join("extra-cache"),
            l.home.join(".ssh"),
            l.argonauts.join("state"),
            l.other_tree.clone(),
            PathBuf::from("relative/cache"),
        ];
        let profile = profile_for(&l, Access::Build, "codex", &extra);
        assert!(can_write(&profile, &l.home.join("extra-cache/x")));
        assert!(profile.writable.contains(&l.home.join("extra-cache")));
        for denied in &extra[1..] {
            assert!(!profile.writable.contains(denied), "{denied:?}");
        }
        assert!(!can_read(&profile, &l.other_tree.join("x")));
    }

    #[test]
    fn network_stays_as_requested() {
        let l = layout();
        let workdir = l.own_tree.clone();
        let input = ProfileInput {
            access: Access::Build,
            vendor: "codex",
            workdir: &workdir,
            session_dir: &l.session,
            argonauts_home: &l.argonauts,
            user_home: &l.home,
            base_worktree: None,
            status_file: None,
            extra_writable: &[],
            sees_other_copies: false,
            commits: false,
        };
        for (network, proxy_port) in [(true, None), (false, None), (false, Some(4128))] {
            let requested = SandboxProfile {
                network,
                proxy_port,
                ..SandboxProfile::default()
            };
            let profile = session_profile(&requested, &input);
            assert_eq!((profile.network, profile.proxy_port), (network, proxy_port));
        }
    }

    #[test]
    fn resolve_path_follows_symlinks_for_missing_files() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().canonicalize().unwrap();
        let link = real.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            resolve_path(&link.join("a/../b/c.txt")),
            real.join("b/c.txt")
        );
    }

    #[test]
    fn stat_rules_open_only_the_way_to_an_exception() {
        let profile = SandboxProfile {
            readable: vec!["/a/b/c".into()],
            deny_read: vec!["/a".into()],
            ..SandboxProfile::default()
        };
        let stats: Vec<PathBuf> = ordered_rules(&profile)
            .into_iter()
            .filter(|rule| rule.kind == RuleKind::Stat)
            .map(|rule| rule.path)
            .collect();
        assert_eq!(stats, [PathBuf::from("/a"), PathBuf::from("/a/b")]);
    }

    #[test]
    fn check_profile_rejects_relative_and_control_paths() {
        for bad in ["relative", "/a\nb"] {
            let profile = SandboxProfile {
                deny_read: vec![bad.into()],
                ..SandboxProfile::default()
            };
            assert!(matches!(
                check_profile(&profile),
                Err(SandboxError::Invalid(_))
            ));
        }
    }
}
