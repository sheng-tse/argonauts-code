//! `sandbox.mode = "container"`, and every session of an untrusted repository: the harness runs in
//! docker or podman, in `sandbox.container_image`, with nothing of this machine inside but the
//! profile's paths at their own places: the writable ones read-write, the readable ones
//! read-only, and denied areas inside them covered. The conductor commits from outside.
//!
//! Without network the container gets `--network none`. Its harness still reaches the proxy on
//! Linux: the launcher on this side bridges a Unix socket to the proxy, and a copy of the
//! launcher inside listens at the proxy's address and carries each connection over that socket.
//! Docker Desktop on macOS can't pass a Unix socket into a container, so there a session without
//! network that needs the proxy is refused.

use std::path::{Path, PathBuf};

use argonauts_core::CommandSpec;
use argonauts_core::worker::{SandboxMode, SandboxProfile};

use super::landlock::reachable_proxies;
use super::launcher::{BRIDGE_DIR, bridge_args, relay_args};
use super::profile::{
    PathRule, RuleKind, check_profile, is_git_lock, ordered_rules, resolve_path, temp_dirs,
};
use super::view::{Disk, Entry, RealDisk, reads, writes};
use super::{SandboxError, SandboxWrap, describe_network};

/// In the order they're tried.
const RUNTIMES: &[&str] = &["docker", "podman"];

/// Where the launcher sits inside the container, to relay the proxy.
const LAUNCHER_INSIDE: &str = "/run/argonauts/argonauts";

/// Marks every container a session starts, so one left behind can be found.
pub const SESSION_LABEL: &str = "dev.argonauts.session";

#[derive(Debug, Clone, Default)]
pub struct Container {
    /// docker or podman, absolute.
    runtime: Option<PathBuf>,
    /// `sandbox.container_image`.
    image: String,
    /// The `argonauts` binary: the proxy bridge outside, and the relay inside on Linux.
    launcher: Option<PathBuf>,
}

impl Container {
    /// Finds docker, then podman, on `PATH`.
    pub fn detect(image: &str) -> Self {
        let runtime = RUNTIMES
            .iter()
            .find_map(|name| which::which(name).ok())
            .filter(|path| path.is_absolute());
        Self {
            runtime,
            image: image.trim().to_owned(),
            launcher: std::env::current_exe().ok(),
        }
    }

    pub fn with_runtime(runtime: impl Into<PathBuf>, image: &str) -> Self {
        Self {
            runtime: Some(runtime.into()),
            image: image.trim().to_owned(),
            launcher: std::env::current_exe().ok(),
        }
    }

    /// The launcher binary that bridges and relays the proxy.
    pub fn with_launcher(mut self, launcher: impl Into<PathBuf>) -> Self {
        self.launcher = Some(launcher.into());
        self
    }

    pub fn runtime(&self) -> Option<&PathBuf> {
        self.runtime.as_ref()
    }

    fn runtime_name(&self) -> Option<String> {
        self.runtime
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
    }
}

impl SandboxWrap for Container {
    fn mode(&self) -> SandboxMode {
        SandboxMode::Container
    }

    fn wrap(
        &self,
        command: CommandSpec,
        profile: &SandboxProfile,
    ) -> Result<CommandSpec, SandboxError> {
        let Some(runtime) = &self.runtime else {
            return Err(SandboxError::Unavailable(
                "neither docker nor podman is on PATH. Set sandbox.mode = \"os\"".to_owned(),
            ));
        };
        if self.image.is_empty() {
            return Err(SandboxError::Unavailable(
                "no container image is set. Set sandbox.container_image to an image with the \
                 harness installed, or sandbox.mode = \"os\""
                    .to_owned(),
            ));
        }
        check_profile(profile)?;
        let proxies = reachable_proxies(profile, &command);
        let launcher = if proxies.is_empty() {
            None
        } else if cfg!(target_os = "linux") {
            Some(
                self.launcher
                    .clone()
                    .filter(|path| path.is_absolute() && path.is_file())
                    .ok_or_else(|| {
                        SandboxError::Unavailable(
                            "can't find the argonauts binary that relays the proxy".to_owned(),
                        )
                    })?,
            )
        } else {
            return Err(SandboxError::Unavailable(
                "a container without network can't reach the proxy here: Docker Desktop doesn't \
                 pass a Unix socket into a container. Set sandbox.mode = \"os\", or \
                 sandbox.network = true"
                    .to_owned(),
            ));
        };

        let mounts = plan_mounts(&own_temp(profile), &RealDisk);
        make_missing(&mounts)?;
        let cover = cover_file()?;
        let mut args = vec![
            "run".to_owned(),
            "--rm".to_owned(),
            "-i".to_owned(),
            "--init".to_owned(),
            "--label".to_owned(),
            format!("{SESSION_LABEL}=1"),
            "--cap-drop".to_owned(),
            "ALL".to_owned(),
            "--security-opt".to_owned(),
            "no-new-privileges".to_owned(),
            "--user".to_owned(),
            format!("{}:{}", nix::unistd::getuid(), nix::unistd::getgid()),
            "--network".to_owned(),
            if profile.network { "bridge" } else { "none" }.to_owned(),
            "--workdir".to_owned(),
            command.cwd.to_string_lossy().into_owned(),
        ];
        for mount in &mounts.list {
            args.push("--mount".to_owned());
            args.push(mount.arg(&cover));
        }
        // By name only: the runtime takes each value from its own environment, so none shows
        // in the process list.
        let mut env_names: Vec<&str> = vec!["HOME"];
        for (name, _) in &command.env_set {
            if !env_names.contains(&name.as_str()) {
                env_names.push(name);
            }
        }
        for name in env_names {
            args.push("--env".to_owned());
            args.push(name.to_owned());
        }

        let program = inside_program(&command.program, &mounts);
        let runtime = runtime.to_string_lossy().into_owned();
        let Some(launcher) = launcher else {
            args.push(self.image.clone());
            args.push(program);
            args.extend(command.args.iter().cloned());
            return Ok(CommandSpec {
                program: runtime,
                args,
                ..command
            });
        };
        let dir = bridge_dir()?;
        args.push("--mount".to_owned());
        args.push(Mount::bind(&dir, false).arg(&cover));
        args.push("--mount".to_owned());
        args.push(Mount::binary(&launcher).arg(&cover));
        args.push(self.image.clone());
        args.push(LAUNCHER_INSIDE.to_owned());
        args.extend(relay_args(&dir, &proxies, &program));
        args.extend(command.args.iter().cloned());
        let mut bridged = bridge_args(&dir, &proxies, &runtime);
        bridged.extend(args);
        Ok(CommandSpec {
            program: launcher.to_string_lossy().into_owned(),
            args: bridged,
            ..command
        })
    }

    fn describe(&self, profile: &SandboxProfile) -> String {
        let Some(runtime) = self.runtime_name() else {
            return "container · no docker or podman".to_owned();
        };
        let image = if self.image.is_empty() {
            "no image set"
        } else {
            self.image.as_str()
        };
        format!(
            "container · {runtime} · {image} · {}",
            describe_network(profile)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MountKind {
    Bind {
        read_only: bool,
    },
    /// An empty folder over a denied one.
    CoverDir,
    /// The unreadable cover file over a denied file.
    CoverFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    path: PathBuf,
    /// Where the bind comes from, when not `path` itself.
    source: Option<PathBuf>,
    kind: MountKind,
}

impl Mount {
    fn bind(path: &Path, read_only: bool) -> Self {
        Self {
            path: path.to_path_buf(),
            source: None,
            kind: MountKind::Bind { read_only },
        }
    }

    fn binary(launcher: &Path) -> Self {
        Self {
            path: PathBuf::from(LAUNCHER_INSIDE),
            source: Some(launcher.to_path_buf()),
            kind: MountKind::Bind { read_only: true },
        }
    }

    /// The `--mount` value. Fields are CSV, so a path with a comma or quote is quoted.
    fn arg(&self, cover: &Path) -> String {
        let field = |key: &str, path: &Path| {
            let value = format!("{key}={}", path.to_string_lossy());
            if value.contains([',', '"']) {
                format!("\"{}\"", value.replace('"', "\"\""))
            } else {
                value
            }
        };
        let dst = field("dst", &self.path);
        match &self.kind {
            MountKind::Bind { read_only } => {
                let src = field("src", self.source.as_deref().unwrap_or(&self.path));
                let ro = if *read_only { ",readonly" } else { "" };
                format!("type=bind,{src},{dst}{ro}")
            }
            MountKind::CoverDir => format!("type=tmpfs,{dst},tmpfs-mode=0"),
            MountKind::CoverFile => format!("type=bind,{},{dst},readonly", field("src", cover)),
        }
    }
}

#[derive(Debug, Default)]
struct Mounts {
    list: Vec<Mount>,
    /// Writable paths to make before the container starts, so their binds have a source.
    create: Vec<(PathBuf, bool)>,
}

/// What the container sees: the profile's paths and nothing else. A readable file inside a
/// denied area comes through its folder, with the folder's other entries covered, since the
/// conductor replaces some of those files by renaming a new one over them.
fn plan_mounts(profile: &SandboxProfile, disk: &dyn Disk) -> Mounts {
    let rules = ordered_rules(profile);
    let mut mounts = Mounts::default();
    let mut bound: Vec<PathBuf> = Vec::new();
    for rule in rules.iter().filter(|rule| {
        matches!(
            rule.kind,
            RuleKind::Write | RuleKind::Read | RuleKind::ReadOnly
        )
    }) {
        let path = rule.resolved.as_path();
        let write = writes(&rules, path);
        if !write && !reads(&rules, path) {
            continue;
        }
        let made = mounts.create.iter().find(|(made, _)| made == path);
        let mut entry = match made {
            Some((_, true)) => Entry::Dir,
            Some((_, false)) => Entry::Other,
            None => disk.entry(path),
        };
        if entry == Entry::Missing && write && !is_git_lock(path) {
            let has_parent = path
                .parent()
                .is_some_and(|parent| disk.entry(parent) == Entry::Dir);
            if !has_parent {
                continue;
            }
            let dir = path.extension().is_none_or(|ext| ext.is_empty());
            mounts.create.push((path.to_path_buf(), dir));
            entry = if dir { Entry::Dir } else { Entry::Other };
        }
        let folder = match entry {
            Entry::Dir => None,
            Entry::Missing | Entry::Other => folder_in_denied_area(&rules, path),
        };
        let (target, target_write) = match folder {
            Some(folder) => {
                let folder_write = writes(&rules, &folder);
                (folder, folder_write)
            }
            None if entry == Entry::Missing => continue,
            None => (path.to_path_buf(), write),
        };
        if bound.contains(&target) {
            continue;
        }
        // A path inside a bind with the same rights comes through already.
        let inherited = bound
            .iter()
            .filter(|outer| target.starts_with(outer))
            .max_by_key(|outer| outer.components().count())
            .is_some_and(|outer| writes(&rules, outer) == target_write && reads(&rules, &target));
        bound.push(target.clone());
        if !inherited {
            mounts.list.push(Mount::bind(&target, !target_write));
        }
        if target != path && write {
            mounts.list.push(Mount::bind(path, false));
        }
        if target != path {
            for child in disk.children(&target) {
                if !reads(&rules, &child) && !writes(&rules, &child) {
                    mounts.list.extend(cover(&child, disk.entry(&child)));
                }
            }
        }
    }
    // Denied areas inside what is shown are covered again.
    for rule in rules.iter().filter(|rule| rule.kind == RuleKind::Deny) {
        let path = rule.resolved.as_path();
        let shown = bound
            .iter()
            .any(|outer| path.starts_with(outer) && path != outer);
        let already = mounts.list.iter().any(|mount| mount.path == path);
        if shown && !already {
            mounts.list.extend(cover(path, disk.entry(path)));
        }
    }
    mounts
}

/// The profile without your temp folders, so the container keeps its own. Yours hold other
/// programs' sockets, such as tmux's or an editor's, and on Linux one bound into a container
/// still answers. Paths inside them that the profile names, such as a gate's home, still come in.
fn own_temp(profile: &SandboxProfile) -> SandboxProfile {
    let temp: Vec<PathBuf> = temp_dirs().iter().map(|path| resolve_path(path)).collect();
    let mut profile = profile.clone();
    profile
        .writable
        .retain(|path| !temp.contains(&resolve_path(path)));
    profile
}

fn cover(path: &Path, entry: Entry) -> Option<Mount> {
    let kind = match entry {
        Entry::Missing => return None,
        Entry::Dir => MountKind::CoverDir,
        Entry::Other => MountKind::CoverFile,
    };
    Some(Mount {
        path: path.to_path_buf(),
        source: None,
        kind,
    })
}

/// The folder holding `path` when it sits strictly inside a denied area.
fn folder_in_denied_area(rules: &[PathRule], path: &Path) -> Option<PathBuf> {
    let folder = path.parent()?;
    let area = rules
        .iter()
        .filter(|rule| rule.kind == RuleKind::Deny && path.starts_with(&rule.resolved))
        .max_by_key(|rule| rule.resolved.components().count())?;
    (folder != area.resolved && folder.starts_with(&area.resolved)).then(|| folder.to_path_buf())
}

fn make_missing(mounts: &Mounts) -> Result<(), SandboxError> {
    for (path, dir) in &mounts.create {
        let made = if *dir {
            std::fs::create_dir(path)
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map(drop)
        };
        match made {
            Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(SandboxError::Unavailable(format!(
                    "can't make {} for the container: {err}",
                    path.display()
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

/// An empty file bound over denied files. It stays readable, since Docker Desktop refuses to
/// bind a file it can't read, but holds nothing. It sits in a folder of this user's own, so no
/// one else can put a link or contents there.
fn cover_file() -> Result<PathBuf, SandboxError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    let unavailable = |path: &Path, err: &dyn std::fmt::Display| {
        SandboxError::Unavailable(format!("can't make {}: {err}", path.display()))
    };
    let uid = nix::unistd::getuid().as_raw();
    let dir = std::env::temp_dir().join(format!("argonauts-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(unavailable(&dir, &err));
        }
        _ => {}
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|err| unavailable(&dir, &err))?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(unavailable(&dir, &"it isn't a private folder of yours"));
    }
    let path = dir.join("container-cover");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.is_file() && meta.len() == 0 && meta.permissions().mode() & 0o777 == 0o400 {
            return Ok(path);
        }
        std::fs::remove_file(&path).map_err(|err| unavailable(&path, &err))?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .open(&path)
        .map_err(|err| unavailable(&path, &err))?;
    Ok(path)
}

/// A fresh folder for the proxy bridge's socket, short enough for a socket path. Only you may
/// enter it, so no other user on this machine reaches the proxy through its socket.
fn bridge_dir() -> Result<PathBuf, SandboxError> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = PathBuf::from("/tmp").join(format!(
        "{BRIDGE_DIR}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|err| SandboxError::Unavailable(format!("can't make {}: {err}", dir.display())))?;
    Ok(dir)
}

/// The program as the container finds it: at its own path when a mount brings it in, otherwise
/// by name on the image's `PATH`.
fn inside_program(program: &str, mounts: &Mounts) -> String {
    let path = Path::new(program);
    let mounted = mounts.list.iter().any(|mount| {
        matches!(mount.kind, MountKind::Bind { .. })
            && mount.source.is_none()
            && path.starts_with(&mount.path)
    });
    if path.is_absolute() && mounted {
        return program.to_owned();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct FakeDisk(BTreeMap<PathBuf, Entry>);

    impl FakeDisk {
        fn new(paths: &[&str]) -> Self {
            let mut map = BTreeMap::new();
            for path in paths {
                let entry = if path.ends_with('/') {
                    Entry::Dir
                } else {
                    Entry::Other
                };
                let path = PathBuf::from(path.trim_end_matches('/'));
                for parent in path.ancestors().skip(1) {
                    map.entry(parent.to_path_buf()).or_insert(Entry::Dir);
                }
                map.insert(path, entry);
            }
            Self(map)
        }
    }

    impl Disk for FakeDisk {
        fn entry(&self, path: &Path) -> Entry {
            self.0.get(path).copied().unwrap_or(Entry::Missing)
        }

        fn children(&self, dir: &Path) -> Vec<PathBuf> {
            self.0
                .keys()
                .filter(|path| path.parent() == Some(dir))
                .cloned()
                .collect()
        }
    }

    const HOME: &str = "/nonexistent-argonauts-test/home";

    fn at(path: &str) -> PathBuf {
        PathBuf::from(format!("{HOME}{path}"))
    }

    fn profile(writable: &[PathBuf], readable: &[PathBuf], deny: &[PathBuf]) -> SandboxProfile {
        SandboxProfile {
            mode: SandboxMode::Container,
            writable: writable.to_vec(),
            readable: readable.to_vec(),
            deny_read: deny.to_vec(),
            network: false,
            proxy_port: None,
        }
    }

    #[test]
    fn only_the_profiles_paths_go_in() {
        let session = at("/.argonauts/s1");
        let disk = FakeDisk::new(&[
            &format!("{HOME}/.argonauts/wt/mine/src/"),
            &format!("{HOME}/.argonauts/wt/other/"),
            &format!("{HOME}/.argonauts/s1/status.json"),
            &format!("{HOME}/.argonauts/s1/raw.stdout.jsonl"),
            &format!("{HOME}/.claude/settings.json"),
            &format!("{HOME}/.claude/projects/x/"),
            &format!("{HOME}/.ssh/id"),
        ]);
        let mine = at("/.argonauts/wt/mine");
        let mounts = plan_mounts(
            &profile(
                &[mine.clone(), at("/.claude"), session.join("report.json")],
                &[
                    mine.clone(),
                    at("/.claude/settings.json"),
                    session.join("status.json"),
                    session.join("report.json"),
                ],
                &[at("/.argonauts"), at("/.claude/projects"), at("/.ssh")],
            ),
            &disk,
        );
        assert_eq!(
            mounts.list,
            vec![
                Mount::bind(&at("/.claude"), false),
                Mount::bind(&at("/.claude/settings.json"), true),
                Mount::bind(&mine, false),
                Mount::bind(&session, true),
                Mount::bind(&session.join("report.json"), false),
                Mount {
                    path: session.join("raw.stdout.jsonl"),
                    source: None,
                    kind: MountKind::CoverFile
                },
                Mount {
                    path: at("/.claude/projects"),
                    source: None,
                    kind: MountKind::CoverDir
                },
            ]
        );
        assert_eq!(mounts.create, vec![(session.join("report.json"), false)]);
    }

    #[test]
    fn a_missing_lock_is_never_made_ahead_of_git() {
        let refs = at("/repo/.git/refs/heads");
        let disk = FakeDisk::new(&[&format!("{}/", refs.display())]);
        let lock = refs.join("card-a.lock");
        let mounts = plan_mounts(&profile(&[lock], &[], &[]), &disk);
        assert!(mounts.create.is_empty(), "{:?}", mounts.create);
        assert!(mounts.list.is_empty(), "{:?}", mounts.list);
    }

    #[test]
    fn mount_arguments_quote_odd_paths() {
        let cover = Path::new("/tmp/cover");
        assert_eq!(
            Mount::bind(Path::new("/a,b"), true).arg(cover),
            "type=bind,\"src=/a,b\",\"dst=/a,b\",readonly"
        );
        assert_eq!(
            cover_mount("/d", MountKind::CoverDir).arg(cover),
            "type=tmpfs,dst=/d,tmpfs-mode=0"
        );
        assert_eq!(
            cover_mount("/f", MountKind::CoverFile).arg(cover),
            "type=bind,src=/tmp/cover,dst=/f,readonly"
        );
    }

    fn cover_mount(path: &str, kind: MountKind) -> Mount {
        Mount {
            path: PathBuf::from(path),
            source: None,
            kind,
        }
    }

    #[test]
    fn programs_come_from_the_image_unless_mounted() {
        let mounts = Mounts {
            list: vec![Mount::bind(Path::new("/opt/tools"), true)],
            create: Vec::new(),
        };
        assert_eq!(inside_program("/usr/local/bin/claude", &mounts), "claude");
        assert_eq!(inside_program("codex", &mounts), "codex");
        assert_eq!(
            inside_program("/opt/tools/fake", &mounts),
            "/opt/tools/fake"
        );
    }

    #[test]
    fn container_without_a_runtime_or_image_is_unavailable() {
        let none = Container::default();
        let err = none
            .wrap(CommandSpec::new("/bin/sh", "/"), &SandboxProfile::default())
            .unwrap_err();
        assert!(
            matches!(&err, SandboxError::Unavailable(why) if why.contains("docker nor podman")),
            "{err}"
        );
        assert_eq!(
            none.describe(&SandboxProfile::default()),
            "container · no docker or podman"
        );
        let no_image = Container::with_runtime("/usr/local/bin/podman", " ");
        let err = no_image
            .wrap(CommandSpec::new("/bin/sh", "/"), &SandboxProfile::default())
            .unwrap_err();
        assert!(
            matches!(&err, SandboxError::Unavailable(why) if why.contains("sandbox.container_image")),
            "{err}"
        );
        assert_eq!(no_image.mode(), SandboxMode::Container);
    }

    #[test]
    fn the_wrap_runs_the_image_with_no_network_and_the_worktree_mounted() {
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().canonicalize().unwrap().join("wt");
        std::fs::create_dir(&worktree).unwrap();
        let mut command = CommandSpec::new("/usr/local/bin/claude", &worktree).args(["-p", "hi"]);
        command
            .env_set
            .push(("ANTHROPIC_MODEL".to_owned(), "haiku".to_owned()));
        let both = std::slice::from_ref(&worktree);
        let profile = profile(both, both, &[]);
        let wrapped = Container::with_runtime("/usr/bin/docker", "argonauts/harness:1")
            .wrap(command, &profile)
            .unwrap();
        assert_eq!(wrapped.program, "/usr/bin/docker");
        let args = wrapped.args.join(" ");
        let worktree = worktree.to_string_lossy();
        for part in [
            "run --rm -i --init",
            "--network none",
            "--cap-drop ALL",
            &format!("--workdir {worktree}"),
            &format!("--mount type=bind,src={worktree},dst={worktree}"),
            "--env HOME --env ANTHROPIC_MODEL",
            "argonauts/harness:1 claude -p hi",
        ] {
            assert!(args.contains(part), "{part} not in {args}");
        }
        assert!(
            wrapped
                .env_set
                .iter()
                .any(|(name, value)| name == "ANTHROPIC_MODEL" && value == "haiku")
        );
    }

    #[test]
    fn a_proxy_goes_through_the_bridge_on_linux_and_is_refused_elsewhere() {
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().canonicalize().unwrap();
        let mut command = CommandSpec::new("/usr/local/bin/codex", &worktree);
        command
            .env_set
            .push(("HTTPS_PROXY".to_owned(), "http://127.0.0.1:7003".to_owned()));
        let profile = profile(std::slice::from_ref(&worktree), &[], &[]);
        let container = Container::with_runtime("/usr/bin/docker", "img").with_launcher("/bin/sh");
        let result = container.wrap(command, &profile);
        if cfg!(target_os = "linux") {
            let wrapped = result.unwrap();
            assert_eq!(wrapped.program, "/bin/sh");
            let args = wrapped.args.join(" ");
            assert!(args.contains("bridge"), "{args}");
            assert!(args.contains("/usr/bin/docker run"), "{args}");
            assert!(
                args.contains(&format!("img {LAUNCHER_INSIDE} __argonauts-sandbox relay")),
                "{args}"
            );
            assert!(args.ends_with("127.0.0.1:7003 codex"), "{args}");
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, SandboxError::Unavailable(why) if why.contains("Unix socket")),
                "{err}"
            );
        }
    }

    #[test]
    fn the_profiles_proxy_is_the_one_bridged() {
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().canonicalize().unwrap();
        let command = CommandSpec::new("/usr/local/bin/codex", &worktree);
        let profile = SandboxProfile {
            proxy_port: Some(4128),
            ..profile(std::slice::from_ref(&worktree), &[], &[])
        };
        let container = Container::with_runtime("/usr/bin/docker", "img").with_launcher("/bin/sh");
        assert_eq!(
            container.describe(&profile),
            "container · docker · img · network only through the proxy on port 4128"
        );
        let result = container.wrap(command, &profile);
        if cfg!(target_os = "linux") {
            let args = result.unwrap().args.join(" ");
            assert!(args.ends_with("127.0.0.1:4128 codex"), "{args}");
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, SandboxError::Unavailable(why) if why.contains("Unix socket")),
                "{err}"
            );
        }
    }

    #[test]
    fn a_container_keeps_its_own_temp() {
        let scratch = tempfile::tempdir().unwrap();
        let worktree = scratch.path().canonicalize().unwrap();
        let mut writable = vec![worktree.clone()];
        writable.extend(temp_dirs());
        let profile = profile(&writable, std::slice::from_ref(&worktree), &[]);
        let wrapped = Container::with_runtime("/usr/bin/docker", "img")
            .wrap(CommandSpec::new("sh", &worktree), &profile)
            .unwrap();
        let args = wrapped.args.join(" ");
        for temp in temp_dirs() {
            let src = format!("src={},", resolve_path(&temp).display());
            assert!(!args.contains(&src), "{src} in {args}");
        }
        assert!(
            args.contains(&format!("src={},", worktree.display())),
            "{args}"
        );
    }

    #[test]
    fn the_bridge_folder_is_yours_alone() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = bridge_dir().unwrap();
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert_eq!(meta.uid(), nix::unistd::getuid().as_raw());
    }

    #[test]
    fn detect_finds_only_absolute_runtimes() {
        if let Some(runtime) = Container::detect("img").runtime() {
            assert!(runtime.is_absolute());
            let name = runtime.file_name().unwrap().to_string_lossy().into_owned();
            assert!(RUNTIMES.contains(&name.as_str()), "{name}");
        }
    }
}
