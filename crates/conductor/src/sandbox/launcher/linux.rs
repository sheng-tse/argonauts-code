//! The launcher on Linux. In order: make the writable paths that aren't there yet, leave the
//! proxy connector outside, enter new user, mount, and (without network) network namespaces,
//! build the view with the session's own `/dev/pts` and `/dev/shm`, bring up loopback and the
//! relay, then start a PID namespace whose first process mounts its own `/proc`, applies
//! Landlock (and, without network, a filter that refuses Unix sockets), and starts the CLI.
//! This process stays outside as the session's head and ends the way the CLI does.

use std::collections::HashMap;
use std::convert::Infallible;
use std::ffi::{CString, OsString};
use std::fs::{DirBuilder, File, OpenOptions};
use std::net::{SocketAddr, TcpListener};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use argonauts_core::session_runner::{SandboxMode, SandboxProfile};
use landlock::{
    ABI, AccessFs, CompatLevel, Compatible, PathBeneath, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, Scope,
};
use nix::errno::Errno;
use nix::libc;
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sched::{CloneFlags, unshare};
use nix::sys::signal::{SigHandler, Signal};
use nix::sys::statvfs::{FsFlags, statvfs};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, fork, getgid, getpid, getppid, getuid};

use super::relay;
use super::{LAUNCH_FAILED, LaunchPlan};
use crate::sandbox::view::{self, RealDisk, Step, View};

/// The Landlock ABI the boundary needs: v3 is the first that stops `truncate` outside the
/// writable paths (Linux 6.2).
const NEEDED_ABI: ABI = ABI::V3;
const NEEDED_ABI_NUMBER: i64 = 3;

/// Devices any program writes, as the Seatbelt profile allows them, plus POSIX shared memory,
/// which Linux keeps in `/dev/shm`.
const DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/tty",
    "/dev/ptmx",
    "/dev/pts",
    "/dev/shm",
];

pub(super) fn launch(
    plan: LaunchPlan,
    program: PathBuf,
    args: Vec<OsString>,
) -> Result<Infallible, String> {
    landlock_support()?;
    let isolate = !plan.network;
    // Forked first, so it holds nothing but its end of the socket pair.
    let connector = if isolate && !plan.proxies.is_empty() {
        Some(start_connector(&plan.proxies)?)
    } else {
        None
    };
    let cwd = std::env::current_dir().map_err(|err| format!("can't read the folder: {err}"))?;
    let profile = SandboxProfile {
        mode: SandboxMode::Os,
        writable: plan.writable,
        readable: plan.readable,
        deny_read: plan.deny_read,
        network: plan.network,
        // The view needs only the paths. The plan's proxies are the session's way out.
        proxy_port: None,
        ask_port: None,
        closed: plan.closed,
        listable: Vec::new(),
        loopback: false,
        outside: plan.outside,
    };
    let mut view = view::plan(&profile, &RealDisk);
    view::cover_names(&mut view, &profile, &RealDisk, &cwd);
    create(&view)?;

    enter_namespaces(isolate)?;
    // Taken in the new mount namespace, since a bind must come from one of its own mounts.
    let sources = open_sources(&view)?;
    build(&view, &sources)?;
    drop(sources);
    private_devices()?;
    // The old folder handle still points below the covers, where `..` reaches what they hide.
    std::env::set_current_dir(&cwd)
        .map_err(|err| format!("can't enter {} in the sandbox: {err}", cwd.display()))?;
    if isolate {
        loopback_up()?;
    }
    if let Some(ctl) = connector {
        start_relay(ctl, &plan.proxies)?;
    }
    let report = enter_pid_namespace()?;
    mount_proc()?;
    drop_capabilities()?;
    restrict(&view.writable)?;
    if isolate {
        refuse_unix_sockets()?;
    }
    run_cli(report, &program, &args)
}

/// Whether this kernel has the Landlock the sandbox needs, or exactly why not.
pub(crate) fn landlock_support() -> Result<(), String> {
    const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1;
    // SAFETY: asking for the ABI version takes no ruleset and touches no memory.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if abi < 0 {
        return Err(match Errno::last() {
            Errno::ENOSYS => "this kernel has no Landlock. It needs Linux 6.2 or newer, \
                              built with CONFIG_SECURITY_LANDLOCK"
                .to_owned(),
            Errno::EOPNOTSUPP => "Landlock is built into this kernel but turned off. Add \
                                  landlock to the kernel's lsm= boot parameter"
                .to_owned(),
            other => format!("the kernel refused to report its Landlock version: {other}"),
        });
    }
    if abi < NEEDED_ABI_NUMBER {
        return Err(format!(
            "this kernel has Landlock ABI {abi}, and the sandbox needs {NEEDED_ABI_NUMBER} \
             (Linux 6.2) to stop truncating files outside the writable paths"
        ));
    }
    Ok(())
}

/// Whether this process may make the namespaces a session needs, or exactly why not. Tried once
/// in a child, since a thread of a running app can't enter a user namespace itself.
pub(crate) fn namespace_support() -> Result<(), String> {
    static RESULT: OnceLock<Result<(), String>> = OnceLock::new();
    RESULT.get_or_init(probe_namespaces).clone()
}

fn probe_namespaces() -> Result<(), String> {
    let uid = getuid();
    let gid = getgid();
    let writes = [
        (
            c"/proc/self/setgroups",
            CString::new("deny").unwrap_or_default(),
        ),
        (
            c"/proc/self/uid_map",
            CString::new(format!("{uid} {uid} 1")).unwrap_or_default(),
        ),
        (
            c"/proc/self/gid_map",
            CString::new(format!("{gid} {gid} 1")).unwrap_or_default(),
        ),
    ];
    // Close-on-exec, so a process another thread starts meanwhile can't hold it open.
    let (read_end, write_end) =
        nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC).map_err(|err| err.to_string())?;
    // SAFETY: the child only makes raw system calls on memory prepared before the fork, then
    // exits, so it never touches a lock another thread held.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            let fd = write_end.as_raw_fd();
            let flags = libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWNET;
            // SAFETY: raw system calls on memory prepared before the fork.
            unsafe {
                if libc::unshare(flags) != 0 {
                    report_and_exit(fd, 1);
                }
                for (path, text) in &writes {
                    let file = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                    if file < 0
                        || libc::write(file, text.as_ptr().cast(), text.as_bytes().len()) < 0
                    {
                        report_and_exit(fd, 2);
                    }
                    libc::close(file);
                }
                let private = libc::MS_REC | libc::MS_PRIVATE;
                if libc::mount(
                    std::ptr::null(),
                    c"/".as_ptr(),
                    std::ptr::null(),
                    private,
                    std::ptr::null(),
                ) != 0
                {
                    report_and_exit(fd, 3);
                }
                // A raw clone, since fork's handlers could wait on a lock another thread held.
                let flags = (libc::CLONE_NEWPID | libc::SIGCHLD) as libc::c_ulong;
                let first = libc::syscall(libc::SYS_clone, flags, 0usize, 0usize, 0usize, 0usize);
                if first < 0 {
                    report_and_exit(fd, 4);
                }
                if first == 0 {
                    let proc_flags = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
                    if libc::mount(
                        c"proc".as_ptr(),
                        c"/proc".as_ptr(),
                        c"proc".as_ptr(),
                        proc_flags,
                        std::ptr::null(),
                    ) != 0
                    {
                        report_and_exit(fd, 5);
                    }
                    libc::_exit(0)
                }
                let mut status = 0;
                while libc::waitpid(first as libc::pid_t, &mut status, 0) < 0 {
                    if Errno::last() != Errno::EINTR {
                        libc::_exit(1);
                    }
                }
                libc::_exit(
                    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                        0
                    } else {
                        1
                    },
                )
            }
        }
        Ok(ForkResult::Parent { child }) => {
            drop(write_end);
            let mut bytes = Vec::new();
            let _ = std::io::Read::read_to_end(&mut File::from(read_end), &mut bytes);
            let status = nix::sys::wait::waitpid(child, None).map_err(|err| err.to_string())?;
            if bytes.len() == 5 {
                let errno =
                    Errno::from_raw(i32::from_ne_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]));
                return Err(namespace_reason(bytes[0], errno));
            }
            match status {
                nix::sys::wait::WaitStatus::Exited(_, 0) => Ok(()),
                other => Err(format!("the namespace check ended oddly: {other:?}")),
            }
        }
        Err(err) => Err(format!("can't start the namespace check: {err}")),
    }
}

/// Sends the failed stage and errno to the parent, then exits. Safe to call after a fork.
fn report_and_exit(fd: libc::c_int, stage: u8) -> ! {
    let errno = Errno::last_raw();
    let mut bytes = [0u8; 5];
    bytes[0] = stage;
    bytes[1..].copy_from_slice(&errno.to_ne_bytes());
    // SAFETY: a write of a stack buffer, then exit.
    unsafe {
        libc::write(fd, bytes.as_ptr().cast(), bytes.len());
        libc::_exit(1)
    }
}

/// Why the kernel refused, in words that say what to change.
fn namespace_reason(stage: u8, errno: Errno) -> String {
    let sysctl = |name: &str| {
        std::fs::read_to_string(format!("/proc/sys/{name}"))
            .ok()
            .map(|value| value.trim().to_owned())
    };
    let step = match stage {
        1 => "make a user namespace",
        2 => "map your user into a user namespace",
        3 => "mount inside a user namespace",
        4 => "make a PID namespace",
        _ => "mount a /proc of the session's own",
    };
    // Past the mount, user namespaces work, so none of their switches is the reason.
    let why = if stage == 5 && errno == Errno::EPERM {
        "the kernel won't mount a fresh /proc where parts of the old one are masked, as a \
         container's are (Docker needs --security-opt systempaths=unconfined)"
            .to_owned()
    } else if stage > 3 {
        format!("the kernel said {errno}")
    } else if sysctl("kernel/unprivileged_userns_clone").as_deref() == Some("0") {
        "unprivileged user namespaces are turned off (kernel.unprivileged_userns_clone = 0)"
            .to_owned()
    } else if sysctl("user/max_user_namespaces").as_deref() == Some("0") {
        "user namespaces are turned off (user.max_user_namespaces = 0)".to_owned()
    } else if stage > 1
        && sysctl("kernel/apparmor_restrict_unprivileged_userns").as_deref() == Some("1")
    {
        "AppArmor restricts unprivileged user namespaces \
         (kernel.apparmor_restrict_unprivileged_userns = 1); allow them for argonauts with an \
         AppArmor profile"
            .to_owned()
    } else if errno == Errno::EPERM {
        "the kernel said no (EPERM), as a container's seccomp profile does".to_owned()
    } else {
        format!("the kernel said {errno}")
    };
    format!(
        "can't {step}: {why}. The Linux sandbox needs user, mount, network, and PID namespaces \
         to hide denied paths, keep the network to the proxy, and keep your other processes out \
         of sight"
    )
}

fn create(view: &View) -> Result<(), String> {
    for step in &view.steps {
        let Step::Create { path, dir } = step else {
            continue;
        };
        let made = if *dir {
            DirBuilder::new().mode(0o700).create(path)
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .map(drop)
        };
        match made {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(format!("can't make {}: {err}", path.display())),
        }
    }
    Ok(())
}

/// Handles on what each shown path is, taken before covers hide them.
fn open_sources(view: &View) -> Result<HashMap<PathBuf, File>, String> {
    let mut sources = HashMap::new();
    for step in &view.steps {
        let Step::Show { path, .. } = step else {
            continue;
        };
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|err| format!("can't open {}: {err}", path.display()))?;
        sources.insert(path.clone(), file);
    }
    Ok(sources)
}

fn enter_namespaces(isolate: bool) -> Result<(), String> {
    let uid = getuid();
    let gid = getgid();
    let mut flags = CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS;
    if isolate {
        flags |= CloneFlags::CLONE_NEWNET;
    }
    unshare(flags).map_err(|errno| namespace_reason(1, errno))?;
    let write = |path: &str, text: String| {
        std::fs::write(path, text).map_err(|err| {
            namespace_reason(2, err.raw_os_error().map_or(Errno::EIO, Errno::from_raw))
        })
    };
    write("/proc/self/setgroups", "deny".to_owned())?;
    write("/proc/self/uid_map", format!("{uid} {uid} 1"))?;
    write("/proc/self/gid_map", format!("{gid} {gid} 1"))?;
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .map_err(|errno| namespace_reason(3, errno))
}

/// A tmpfs in a fresh temp folder, seen only in this mount namespace, holding the file that
/// covers denied files.
struct Staging {
    dir: PathBuf,
    cover: PathBuf,
}

impl Staging {
    fn new() -> Result<Self, String> {
        let dir = std::env::temp_dir().join(format!(
            "argonauts-view-{}-{}",
            getpid(),
            uuid::Uuid::new_v4().simple()
        ));
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|err| format!("can't make {}: {err}", dir.display()))?;
        mount(
            Some("tmpfs"),
            &dir,
            Some("tmpfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some("mode=0700,size=64k"),
        )
        .map_err(|err| format!("can't mount the staging folder: {err}"))?;
        let cover = dir.join("cover");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o000)
            .open(&cover)
            .map_err(|err| format!("can't make the cover file: {err}"))?;
        Ok(Self { dir, cover })
    }

    /// The covers already placed keep the tmpfs alive.
    fn remove(self) {
        let _ = umount2(&self.dir, MntFlags::MNT_DETACH);
        let _ = std::fs::remove_dir(&self.dir);
    }
}

fn build(view: &View, sources: &HashMap<PathBuf, File>) -> Result<(), String> {
    let staging = Staging::new()?;
    let mut covers = Vec::new();
    let result = view.steps.iter().try_for_each(|step| match step {
        Step::Create { .. } => Ok(()),
        Step::HideDir { path, pass } => {
            // Walkable but not listable, even by the user it belongs to.
            let mode = if *pass { "0111" } else { "0000" };
            let hide = || {
                mount(
                    Some("tmpfs"),
                    path,
                    Some("tmpfs"),
                    MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
                    Some(format!("mode={mode},size=64k").as_str()),
                )
            };
            if covered(path, hide)? {
                covers.push(path.clone());
            }
            Ok(())
        }
        Step::HideFile { path } => covered(path, || bind(&staging.cover, path, false)).map(drop),
        Step::Show { path, dir } => {
            let source = sources
                .get(path)
                .ok_or_else(|| format!("lost the handle on {}", path.display()))?;
            mount_point(path, *dir)?;
            let from = PathBuf::from(format!("/proc/self/fd/{}", source.as_raw_fd()));
            bind(&from, path, true).map_err(|err| format!("can't show {}: {err}", path.display()))
        }
        Step::ReadOnly { path } => {
            read_only(path).map_err(|err| format!("can't make {} read-only: {err}", path.display()))
        }
    });
    // Nothing may be added to the covers once the view is built.
    let sealed = result.and_then(|()| {
        covers.iter().try_for_each(|path| {
            mount(
                None::<&str>,
                path,
                None::<&str>,
                MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
                None::<&str>,
            )
            .map_err(|err| format!("can't seal the cover on {}: {err}", path.display()))
        })
    });
    staging.remove();
    sealed
}

/// Puts a cover on `path` with `cover`, and says whether one went on. An entry gone since the
/// view was planned has nothing left to cover, such as a file written under a passing name
/// and renamed to its own while the session starts. One back by now is covered as it is.
fn covered(path: &Path, cover: impl Fn() -> nix::Result<()>) -> Result<bool, String> {
    let failed = |err: Errno| format!("can't cover {}: {err}", path.display());
    match cover() {
        Ok(()) => Ok(true),
        Err(Errno::ENOENT) if gone(path) => Ok(false),
        Err(Errno::ENOENT) => cover().map(|()| true).map_err(failed),
        Err(err) => Err(failed(err)),
    }
}

fn gone(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound)
}

fn bind(from: &Path, to: &Path, recursive: bool) -> nix::Result<()> {
    let mut flags = MsFlags::MS_BIND;
    if recursive {
        flags |= MsFlags::MS_REC;
    }
    mount(Some(from), to, None::<&str>, flags, None::<&str>)
}

/// A place to bind `path` inside the cover above it: folders on the way the session can walk
/// through but not list, and an empty folder or file at the end.
fn mount_point(path: &Path, dir: bool) -> Result<(), String> {
    // A path bound again inside a read-only folder is already there, and nothing can be made.
    if std::fs::symlink_metadata(path).is_ok() {
        return Ok(());
    }
    let made = |err: std::io::Error| format!("can't place {}: {err}", path.display());
    if let Some(parent) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o111)
            .create(parent)
            .map_err(made)?;
    }
    let result = if dir {
        DirBuilder::new().mode(0o711).create(path)
    } else {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o000)
            .open(path)
            .map(drop)
    };
    match result {
        Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => Err(made(err)),
        _ => Ok(()),
    }
}

/// Binds `path` over itself and makes the bind read-only. A user namespace may not change the
/// other flags of a mount it was handed, so they are carried over.
fn read_only(path: &Path) -> nix::Result<()> {
    bind(path, path, true)?;
    // `ST_RELATIME`, which nix leaves out for musl.
    const RELATIME: FsFlags = FsFlags::from_bits_retain(0x1000);
    let current = mount_flags(path)?;
    let mut flags = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY;
    let carried = [
        (FsFlags::ST_NOSUID, MsFlags::MS_NOSUID),
        (FsFlags::ST_NODEV, MsFlags::MS_NODEV),
        (FsFlags::ST_NOEXEC, MsFlags::MS_NOEXEC),
        (FsFlags::ST_NOATIME, MsFlags::MS_NOATIME),
        (FsFlags::ST_NODIRATIME, MsFlags::MS_NODIRATIME),
        (RELATIME, MsFlags::MS_RELATIME),
    ];
    for (has, keep) in carried {
        if current.contains(has) {
            flags |= keep;
        }
    }
    if !current.intersects(FsFlags::ST_NOATIME | RELATIME) {
        flags |= MsFlags::MS_STRICTATIME;
    }
    mount(None::<&str>, path, None::<&str>, flags, None::<&str>)
}

/// The flags of the mount at `path`, every bit of them. nix's own `statvfs` drops the ones it
/// doesn't name, and on musl that is `ST_RELATIME`, so a remount would ask for strict atime,
/// which a user namespace may not change.
fn mount_flags(path: &Path) -> nix::Result<FsFlags> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Errno::EINVAL)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a C string and `stat` has room for what statvfs writes.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(Errno::last());
    }
    // SAFETY: statvfs succeeded, so it filled `stat`.
    let flags = unsafe { stat.assume_init() }.f_flag;
    Ok(FsFlags::from_bits_retain(flags))
}

/// The session's own `/dev/pts` and `/dev/shm`. The system's `/dev/pts` holds your terminals,
/// where raw escape sequences could set titles, fill the clipboard, or type into your shell, and
/// the system's `/dev/shm` would let sessions trade files outside their worktrees.
fn private_devices() -> Result<(), String> {
    let pts = Path::new("/dev/pts");
    if pts.is_dir() {
        mount(
            Some("devpts"),
            pts,
            Some("devpts"),
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
            Some("newinstance,ptmxmode=0666,mode=0620"),
        )
        .map_err(|err| format!("can't give the session its own /dev/pts: {err}"))?;
        // Where `/dev/ptmx` is a link to `pts/ptmx`, as containers make it, it points there
        // already.
        let ptmx = Path::new("/dev/ptmx");
        if std::fs::symlink_metadata(ptmx).is_ok_and(|meta| !meta.file_type().is_symlink()) {
            bind(&pts.join("ptmx"), ptmx, false)
                .map_err(|err| format!("can't give the session its own /dev/ptmx: {err}"))?;
        }
    }
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        // As big as the one it covers.
        let size = statvfs(shm)
            .ok()
            .map(|stats| stats.blocks() * stats.fragment_size())
            .filter(|bytes| *bytes > 0);
        let options = match size {
            Some(bytes) => format!("mode=1777,size={bytes}"),
            None => "mode=1777".to_owned(),
        };
        mount(
            Some("tmpfs"),
            shm,
            Some("tmpfs"),
            MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some(options.as_str()),
        )
        .map_err(|err| format!("can't give the session its own /dev/shm: {err}"))?;
    }
    Ok(())
}

/// Starts the session's PID namespace, where its processes see and signal only each other and
/// read no other process's environment. Returns in the namespace's first process, holding where
/// to report how the CLI ended. This process stays outside as the session's head.
fn enter_pid_namespace() -> Result<OwnedFd, String> {
    unshare(CloneFlags::CLONE_NEWPID).map_err(|errno| namespace_reason(4, errno))?;
    let (read_end, write_end) = nix::unistd::pipe2(nix::fcntl::OFlag::O_CLOEXEC)
        .map_err(|err| format!("can't make the session's status pipe: {err}"))?;
    // SAFETY: this process is single-threaded, so the child may run ordinary code.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            drop(read_end);
            // Its parent is outside the namespace, so `getppid` can't tell whether the head
            // died before the signal was armed. A closed pipe can.
            let head_alive =
                nix::sys::prctl::set_pdeathsig(Signal::SIGKILL).is_ok() && !reader_gone(&write_end);
            if !head_alive {
                std::process::exit(LAUNCH_FAILED);
            }
            Ok(write_end)
        }
        Ok(ForkResult::Parent { child }) => {
            drop(write_end);
            main_agent(child, read_end)
        }
        Err(err) => Err(format!("can't start the session's processes: {err}")),
    }
}

/// Whether nobody holds the read end of the pipe `fd` writes to any more.
fn reader_gone(fd: &OwnedFd) -> bool {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    // SAFETY: polls one descriptor this process owns, without waiting.
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    ready > 0 && poll.revents & libc::POLLERR != 0
}

/// The session's head, outside its PID namespace: waits for the namespace's first process and
/// ends as the CLI did, so Argonauts Code sees the CLI's own exit. The signals Argonauts
/// Code sends the session's process group reach the CLI directly, so the head only has to
/// outlive them.
fn main_agent(first: Pid, report: OwnedFd) -> ! {
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        // SAFETY: ignoring a signal installs no handler.
        let _ = unsafe { nix::sys::signal::signal(signal, SigHandler::SigIgn) };
    }
    let waited = loop {
        match waitpid(first, None) {
            Err(Errno::EINTR) => continue,
            other => break other,
        }
    };
    let mut bytes = [0u8; 5];
    let reported = std::io::Read::read_exact(&mut File::from(report), &mut bytes).is_ok();
    let value = i32::from_ne_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    match (reported, bytes[0], waited) {
        (true, EXITED, _) => std::process::exit(value),
        (true, _, _) => die_by(value),
        (false, _, Ok(WaitStatus::Signaled(_, signal, _))) => die_by(signal as i32),
        (false, _, Ok(WaitStatus::Exited(_, code))) => std::process::exit(code),
        (false, _, _) => std::process::exit(LAUNCH_FAILED),
    }
}

const EXITED: u8 = 0;
const SIGNALED: u8 = 1;

/// Ends this process by `signal`, as the CLI ended, or with the shell's `128 + signal` when
/// the signal won't end it.
fn die_by(signal: i32) -> ! {
    // A core file would be written into the worktree.
    let no_core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: plain calls with values that outlive them.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &no_core);
        libc::signal(signal, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(signal);
    }
    std::process::exit(128 + signal)
}

/// A `/proc` of the session's own PID namespace, where no other process shows.
fn mount_proc() -> Result<(), String> {
    mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None::<&str>,
    )
    .map_err(|errno| namespace_reason(5, errno))
}

/// The PID namespace's first process: starts the CLI, reaps whatever ends up orphaned here,
/// and when the CLI ends, reports how and exits, which ends everything left in the namespace.
fn run_cli(report: OwnedFd, program: &Path, args: &[OsString]) -> Result<Infallible, String> {
    // SAFETY: this process is single-threaded, so the child may run ordinary code.
    let cli = match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            drop(report);
            let err = std::process::Command::new(program).args(args).exec();
            eprintln!(
                "argonauts sandbox: can't start {}: {err}",
                program.display()
            );
            std::process::exit(LAUNCH_FAILED);
        }
        Ok(ForkResult::Parent { child }) => child,
        Err(err) => return Err(format!("can't start {}: {err}", program.display())),
    };
    let (kind, value) = loop {
        match waitpid(None::<Pid>, None) {
            Ok(WaitStatus::Exited(pid, code)) if pid == cli => break (EXITED, code),
            Ok(WaitStatus::Signaled(pid, signal, _)) if pid == cli => {
                break (SIGNALED, signal as i32);
            }
            Ok(_) | Err(Errno::EINTR) => {}
            Err(_) => break (EXITED, LAUNCH_FAILED),
        }
    };
    let mut bytes = [kind, 0, 0, 0, 0];
    bytes[1..].copy_from_slice(&value.to_ne_bytes());
    let _ = std::io::Write::write_all(&mut File::from(report), &bytes);
    std::process::exit(if kind == EXITED { value } else { 128 + value })
}

/// A new network namespace starts with loopback down.
fn loopback_up() -> Result<(), String> {
    let failed = |err: std::io::Error| format!("can't bring up loopback: {err}");
    // SAFETY: a plain socket call; the descriptor is closed below.
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if sock < 0 {
        return Err(failed(std::io::Error::last_os_error()));
    }
    // SAFETY: `ifreq` is plain data, the name fits with its terminating zero, and both ioctls
    // read and write only the struct.
    let result = unsafe {
        let mut request: libc::ifreq = std::mem::zeroed();
        for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo\0") {
            *slot = *byte as libc::c_char;
        }
        if libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut request) < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            request.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
            if libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &request) < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    };
    // SAFETY: closes the socket made above.
    unsafe { libc::close(sock) };
    result.map_err(failed)
}

/// The connector stays in this network namespace, and dies with the session.
fn start_connector(proxies: &[SocketAddr]) -> Result<UnixStream, String> {
    let (inside, outside) =
        UnixStream::pair().map_err(|err| format!("can't make the proxy socket pair: {err}"))?;
    let parent = getpid();
    // SAFETY: this process is single-threaded, so the child may run ordinary code.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            drop(inside);
            helper_setup(parent);
            relay::serve_connector(outside, proxies);
            std::process::exit(0)
        }
        Ok(ForkResult::Parent { .. }) => Ok(inside),
        Err(err) => Err(format!("can't start the proxy connector: {err}")),
    }
}

/// The relay listens inside, where each proxy listens outside.
fn start_relay(ctl: UnixStream, proxies: &[SocketAddr]) -> Result<(), String> {
    let listeners = proxies
        .iter()
        .map(|addr| {
            TcpListener::bind(addr)
                .map_err(|err| format!("can't listen at {addr} in the sandbox: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parent = getpid();
    // SAFETY: this process is still single-threaded.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            helper_setup(parent);
            relay::serve_relay(ctl, listeners);
            std::process::exit(0)
        }
        Ok(ForkResult::Parent { .. }) => Ok(()),
        Err(err) => Err(format!("can't start the proxy relay: {err}")),
    }
}

/// A helper dies with the CLI it serves, and keeps none of its input or output open, so
/// Argonauts Code sees the CLI's output end when the CLI does.
fn helper_setup(parent: Pid) {
    if nix::sys::prctl::set_pdeathsig(Signal::SIGKILL).is_err() || getppid() != parent {
        std::process::exit(0);
    }
    if let Ok(null) = OpenOptions::new().read(true).write(true).open("/dev/null") {
        for fd in [0, 1, 2] {
            // SAFETY: points a standard descriptor at /dev/null.
            unsafe { libc::dup2(null.as_raw_fd(), fd) };
        }
    }
}

/// The namespace's capabilities stay behind. A user's CLI would lose them at exec anyway,
/// but root keeps them across exec unless told not to, and with them could open the covers.
fn drop_capabilities() -> Result<(), String> {
    const SECBIT_NOROOT: libc::c_ulong = 1 << 0;
    const SECBIT_NOROOT_LOCKED: libc::c_ulong = 1 << 1;
    const SECBIT_NO_SETUID_FIXUP: libc::c_ulong = 1 << 2;
    const SECBIT_NO_SETUID_FIXUP_LOCKED: libc::c_ulong = 1 << 3;
    const SECBIT_KEEP_CAPS_LOCKED: libc::c_ulong = 1 << 5;
    const CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    #[repr(C)]
    struct Header {
        version: u32,
        pid: libc::c_int,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let failed = |what: &str| format!("can't drop {what}: {}", std::io::Error::last_os_error());
    let bits = SECBIT_NOROOT
        | SECBIT_NOROOT_LOCKED
        | SECBIT_NO_SETUID_FIXUP
        | SECBIT_NO_SETUID_FIXUP_LOCKED
        | SECBIT_KEEP_CAPS_LOCKED;
    // SAFETY: prctl and capset with plain values and structs that outlive the calls.
    unsafe {
        if libc::prctl(libc::PR_SET_SECUREBITS, bits, 0, 0, 0) != 0 {
            return Err(failed("root's capabilities"));
        }
        let mut cap = 0;
        while libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) == 0 {
            cap += 1;
        }
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        );
        let header = Header {
            version: CAPABILITY_VERSION_3,
            pid: 0,
        };
        let data = [Data {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }; 2];
        if libc::syscall(libc::SYS_capset, &header, data.as_ptr()) != 0 {
            return Err(failed("capabilities"));
        }
    }
    Ok(())
}

/// Landlock allows writes only to the writable paths and the devices, keeps the session from
/// tracing anything outside it, and (from ABI 6) from reaching abstract sockets outside. Landlock
/// leaves signals unscoped, since a scoped session's exit couldn't deliver its helpers'
/// parent-death signal; the PID namespace already keeps them among the session's processes.
fn restrict(writable: &[PathBuf]) -> Result<(), String> {
    let failed = |err: &dyn std::fmt::Display| format!("can't apply Landlock: {err}");
    let write = AccessFs::from_write(NEEDED_ABI);
    let file_write = write & AccessFs::from_file(NEEDED_ABI);
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(write)
        .map_err(|err| failed(&err))?
        .set_compatibility(CompatLevel::BestEffort)
        .scope(Scope::AbstractUnixSocket)
        .map_err(|err| failed(&err))?
        .create()
        .map_err(|err| failed(&err))?
        .set_compatibility(CompatLevel::HardRequirement);
    let paths = writable.iter().map(|path| (path.clone(), true));
    let devices = DEVICES.iter().map(|path| (PathBuf::from(path), false));
    for (path, needed) in paths.chain(devices) {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let fd = match PathFd::new(&path) {
            Ok(fd) => fd,
            // `/dev/tty` won't open in a session with no terminal, which has none to write.
            Err(_) if !needed => continue,
            Err(err) => return Err(failed(&err)),
        };
        let rights = if meta.is_dir() { write } else { file_write };
        ruleset = ruleset
            .add_rule(PathBeneath::new(fd, rights))
            .map_err(|err| failed(&err))?;
    }
    let status = ruleset.restrict_self().map_err(|err| failed(&err))?;
    if status.ruleset == RulesetStatus::NotEnforced || !status.no_new_privs {
        return Err("Landlock isn't enforced on this kernel".to_owned());
    }
    Ok(())
}

/// Without network a session opens no Unix socket, as the agents' own sandboxes have it. Yours
/// sit in temp and `/run/user` (tmux, D-Bus, an editor's), where a connection could run work
/// outside the sandbox, and Landlock can't tell them from the session's own. io_uring goes too,
/// since it could open one past this filter. Socket pairs, which CLIs use for their
/// children, stay.
fn refuse_unix_sockets() -> Result<(), String> {
    #[cfg(target_arch = "x86_64")]
    const ARCH: Option<u32> = Some(0xc000_003e);
    #[cfg(target_arch = "aarch64")]
    const ARCH: Option<u32> = Some(0xc000_00b7);
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    const ARCH: Option<u32> = None;
    let Some(arch) = ARCH else {
        return Err("the Linux sandbox can't filter system calls on this processor".to_owned());
    };
    let program = unix_socket_filter(arch, cfg!(target_arch = "x86_64"));
    let filter = SockFprog {
        len: u16::try_from(program.len()).unwrap_or(u16::MAX),
        filter: program.as_ptr(),
    };
    // SAFETY: prctl with plain values and a filter that outlives the call.
    let failed = unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::prctl(
                libc::PR_SET_SECCOMP,
                SECCOMP_MODE_FILTER,
                &filter as *const SockFprog,
            ) != 0
    };
    if failed {
        return Err(format!(
            "can't keep the session from Unix sockets: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

const SECCOMP_MODE_FILTER: libc::c_ulong = 2;

/// `struct sock_filter`: one classic BPF instruction.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

/// `struct sock_fprog`.
#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

/// Refuses `socket(AF_UNIX, ..)`, `io_uring_setup`, x32 calls, and calls from any other ABI
/// with EPERM, and allows the rest. `arch` is the kernel's `AUDIT_ARCH_*` for this one.
fn unix_socket_filter(arch: u32, has_x32: bool) -> Vec<SockFilter> {
    /// Where a jump goes: on to the next instruction, or to one of the two returns at the end.
    #[derive(Clone, Copy)]
    enum To {
        Next,
        Allow,
        Deny,
    }
    use To::{Allow, Deny, Next};
    let syscall = |nr: libc::c_long| u32::try_from(nr).unwrap_or(u32::MAX);
    let mut steps = vec![
        (LOAD, ARCH_AT, Next, Next),
        (JEQ, arch, Next, Deny),
        (LOAD, NR_AT, Next, Next),
    ];
    if has_x32 {
        steps.push((JGE, X32_BIT, Deny, Next));
    }
    steps.extend([
        (JEQ, syscall(libc::SYS_io_uring_setup), Deny, Next),
        (JEQ, syscall(libc::SYS_socket), Next, Allow),
        (LOAD, FIRST_ARG_AT, Next, Next),
        (JEQ, libc::AF_UNIX as u32, Deny, Allow),
    ]);
    let allow = steps.len();
    let deny = allow + 1;
    let jump = |from: usize, to: To| {
        let target = match to {
            Next => return 0,
            Allow => allow,
            Deny => deny,
        };
        u8::try_from(target - from - 1).unwrap_or(u8::MAX)
    };
    let mut program: Vec<SockFilter> = steps
        .iter()
        .enumerate()
        .map(|(at, &(code, k, taken, not_taken))| SockFilter {
            code,
            jt: jump(at, taken),
            jf: jump(at, not_taken),
            k,
        })
        .collect();
    for k in [ALLOW, DENY] {
        program.push(SockFilter {
            code: RET,
            jt: 0,
            jf: 0,
            k,
        });
    }
    program
}

const LOAD: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
const JEQ: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
const JGE: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
const RET: u16 = 0x06; // BPF_RET | BPF_K
const ALLOW: u32 = 0x7fff_0000; // SECCOMP_RET_ALLOW
const DENY: u32 = 0x0005_0000 | libc::EPERM as u32; // SECCOMP_RET_ERRNO
/// Offsets in `struct seccomp_data`. A socket's domain is the low half of the first argument.
const NR_AT: u32 = 0;
const ARCH_AT: u32 = 4;
const FIRST_ARG_AT: u32 = 16;
const X32_BIT: u32 = 0x4000_0000;

/// Outside a container: serves the proxy sockets, runs the container, and removes the sockets'
/// folder once it ends. Signals reach the whole process group; the runtime stops its container
/// on them, and this process waits for it.
pub(super) fn run_bridge(
    dir: &Path,
    proxies: &[SocketAddr],
    runtime: PathBuf,
    args: Vec<OsString>,
) -> Result<Infallible, String> {
    const PASSED_ON: [Signal; 3] = [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP];
    for (index, proxy) in proxies.iter().enumerate() {
        let socket = super::bridge_socket(dir, index);
        let listener = UnixListener::bind(&socket)
            .map_err(|err| format!("can't listen at {}: {err}", socket.display()))?;
        let proxy = *proxy;
        std::thread::spawn(move || relay::serve_unix_to_tcp(listener, proxy));
    }
    for signal in PASSED_ON {
        // SAFETY: ignoring a signal installs no handler.
        let _ = unsafe { nix::sys::signal::signal(signal, SigHandler::SigIgn) };
    }
    let mut runtime_command = std::process::Command::new(&runtime);
    runtime_command.args(&args);
    // SAFETY: the hook only makes async-signal-safe calls.
    unsafe {
        runtime_command.pre_exec(|| {
            for signal in PASSED_ON {
                nix::sys::signal::signal(signal, SigHandler::SigDfl)?;
            }
            nix::sys::prctl::set_pdeathsig(Signal::SIGKILL)?;
            Ok(())
        });
    }
    let status = runtime_command.status();
    let _ = std::fs::remove_dir_all(dir);
    let status = status.map_err(|err| format!("can't start {}: {err}", runtime.display()))?;
    let code = status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1);
    std::process::exit(code)
}

/// Inside a container: listens at each proxy's address, carries each connection to the bridge's
/// socket, and becomes the CLI.
pub(super) fn run_relay(
    dir: &Path,
    proxies: &[SocketAddr],
    program: PathBuf,
    args: Vec<OsString>,
) -> Result<Infallible, String> {
    // Container runtimes bring loopback up themselves; a bare network namespace leaves it down.
    let _ = loopback_up();
    let listeners = proxies
        .iter()
        .map(|addr| {
            TcpListener::bind(addr)
                .map_err(|err| format!("can't listen at {addr} in the container: {err}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let parent = getpid();
    // SAFETY: this process is single-threaded, so the child may run ordinary code.
    match unsafe { fork() } {
        Ok(ForkResult::Child) => {
            helper_setup(parent);
            let threads: Vec<_> = listeners
                .into_iter()
                .enumerate()
                .map(|(index, listener)| {
                    let socket = super::bridge_socket(dir, index);
                    std::thread::spawn(move || relay::serve_tcp_to_unix(listener, socket))
                })
                .collect();
            for thread in threads {
                let _ = thread.join();
            }
            std::process::exit(0)
        }
        Ok(ForkResult::Parent { .. }) => {}
        Err(err) => return Err(format!("can't start the proxy relay: {err}")),
    }
    drop(listeners);
    let err = std::process::Command::new(&program).args(&args).exec();
    Err(format!("can't start {}: {err}", program.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the kernel would do with one call, by running the filter as it does.
    fn verdict(program: &[SockFilter], arch: u32, nr: u32, first_arg: u32) -> u32 {
        let mut at = 0;
        let mut loaded = 0;
        loop {
            let step = program[at];
            at += 1;
            match step.code {
                LOAD => {
                    loaded = match step.k {
                        NR_AT => nr,
                        ARCH_AT => arch,
                        FIRST_ARG_AT => first_arg,
                        other => panic!("loads offset {other}"),
                    };
                }
                JEQ | JGE => {
                    let taken = if step.code == JEQ {
                        loaded == step.k
                    } else {
                        loaded >= step.k
                    };
                    at += usize::from(if taken { step.jt } else { step.jf });
                }
                RET => return step.k,
                other => panic!("unknown instruction {other:#x}"),
            }
        }
    }

    #[test]
    fn a_masked_proc_is_named_as_the_reason() {
        let masked = namespace_reason(5, Errno::EPERM);
        assert!(masked.contains("/proc"), "{masked}");
        assert!(masked.contains("systempaths=unconfined"), "{masked}");
        assert!(!masked.contains("AppArmor"), "{masked}");
        let pid = namespace_reason(4, Errno::EINVAL);
        assert!(
            pid.starts_with("can't make a PID namespace: the kernel said"),
            "{pid}"
        );
    }

    #[test]
    fn the_filter_refuses_unix_sockets_and_io_uring_only() {
        const NATIVE: u32 = 0xc000_003e;
        let socket = u32::try_from(libc::SYS_socket).unwrap();
        let socketpair = u32::try_from(libc::SYS_socketpair).unwrap();
        let uring = u32::try_from(libc::SYS_io_uring_setup).unwrap();
        let read = u32::try_from(libc::SYS_read).unwrap();
        let unix = libc::AF_UNIX as u32;
        let inet = libc::AF_INET as u32;
        for has_x32 in [true, false] {
            let program = unix_socket_filter(NATIVE, has_x32);
            assert_eq!(verdict(&program, NATIVE, socket, unix), DENY);
            assert_eq!(verdict(&program, NATIVE, socket, inet), ALLOW);
            assert_eq!(verdict(&program, NATIVE, socketpair, unix), ALLOW);
            assert_eq!(verdict(&program, NATIVE, uring, 0), DENY);
            assert_eq!(verdict(&program, NATIVE, read, unix), ALLOW);
            // Another ABI, such as 32-bit x86, gets nothing.
            assert_eq!(verdict(&program, 0x4000_0003, read, 0), DENY);
        }
        // Nor does x32, whose calls carry this bit under the native arch.
        let program = unix_socket_filter(NATIVE, true);
        for nr in [socket, read] {
            assert_eq!(verdict(&program, NATIVE, X32_BIT | nr, unix), DENY);
        }
    }

    /// A cover whose entry went since the view was planned is left out, one whose entry is back
    /// by now goes on as it is, and any other failure stops the launch.
    #[test]
    fn a_cover_on_an_entry_gone_since_the_plan_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let renamed = dir.path().join("process.json.partial");
        assert_eq!(covered(&renamed, || Err(Errno::ENOENT)), Ok(false));

        let back = dir.path().join("process.json");
        std::fs::write(&back, "{}").unwrap();
        let tries = std::cell::Cell::new(0);
        let cover = || {
            tries.set(tries.get() + 1);
            if tries.get() == 1 {
                Err(Errno::ENOENT)
            } else {
                Ok(())
            }
        };
        assert_eq!(covered(&back, cover), Ok(true));
        assert_eq!(tries.get(), 2);

        let refused = covered(&back, || Err(Errno::EPERM)).unwrap_err();
        assert!(refused.starts_with("can't cover "), "{refused}");
    }
}
