//! The exit watcher: a forked copy of the run host, since it has to outlive it. When the
//! run host exits first, it kills the agent's whole tree the way a release does. While it
//! watches, it notes every process it sees in the tree, so a process that left the session and
//! lost its parent is still killed. It keeps watching after the head exits, for as long as the
//! group, the session, or a noted process lives. Everything here after the fork makes system
//! calls only, since allocating in a forked child isn't safe.

use std::io;
use std::mem::size_of;

use nix::libc;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::{ForkResult, Pid, fork, getpid, getppid};

use super::procs::PID_SCRATCH;
use super::table::{CAPACITY, Row, Storage, Table};
use argonauts_core::agents::AgentGroup;

/// Descriptors above this are left open in the watcher. Agents' pipes sit far below it.
const MAX_FD_TO_CLOSE: i32 = 65_536;
/// How often a watcher whose head has exited checks that the group or session still has
/// members, in milliseconds.
const DRAIN_TICK_MS: i32 = 250;
/// How often a watcher whose head runs notes the tree, in milliseconds. Each note reads the
/// whole process table, so it's slower than the drain.
const NOTE_TICK_MS: i32 = 1_000;
/// Processes of the tree the watcher notes at most. Past that, new ones go unnoted until noted
/// ones exit.
const NOTED_CAPACITY: usize = 4_096;
/// A second pass catches what the first one's processes started before they died.
const SWEEPS: usize = 2;

#[derive(Clone, Copy)]
struct Target<'a> {
    run_host: i32,
    /// The process that forked the watcher.
    forking: i32,
    head: i32,
    pgid: i32,
    /// `ARGONAUTS_LAUNCH=<id>`, or empty to skip the environment sweep. Only Linux shows other
    /// processes' environments.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    launch_entry: &'a [u8],
}

/// Starts watching `run_host` on behalf of `record`'s agent. Returns the watcher's pid.
pub fn start(run_host: u32, record: &AgentGroup, launch_entry: &[u8]) -> io::Result<Pid> {
    let out_of_range = || io::Error::new(io::ErrorKind::InvalidInput, "process id out of range");
    let target = Target {
        run_host: i32::try_from(run_host).map_err(|_| out_of_range())?,
        forking: getpid().as_raw(),
        head: i32::try_from(record.pid).map_err(|_| out_of_range())?,
        pgid: i32::try_from(record.pgid).map_err(|_| out_of_range())?,
        launch_entry,
    };
    // Noted before the fork, so a process that leaves the tree right away is still found.
    let noted = Noted::of_tree(target.head, target.pgid);
    // SAFETY: getdtablesize only reads a limit.
    let open_limit = unsafe { libc::getdtablesize() }.clamp(3, MAX_FD_TO_CLOSE);
    // SAFETY: the child makes plain system calls only, allocates nothing, takes no locks, and
    // ends in `_exit` without returning or unwinding. `launch_entry` stays readable there, since
    // the child's memory is its own, apart from what the caller holds.
    match unsafe { fork() }.map_err(io::Error::from)? {
        ForkResult::Parent { child } => Ok(child),
        ForkResult::Child => unsafe { run(&target, noted, open_limit) },
    }
}

/// # Safety
/// Call only in a freshly forked child.
unsafe fn run(target: &Target, noted: Noted, open_limit: i32) -> ! {
    // SAFETY: this is the forked child.
    unsafe { detach(open_limit) };
    let mut tree = Tree {
        table: LazyTable::default(),
        noted,
        traced: true,
    };
    if run_host_exits_first(target, &mut tree) {
        sweep(target, &mut tree);
    }
    // SAFETY: ends the forked child without running the parent's exit handlers.
    unsafe { libc::_exit(0) }
}

/// # Safety
/// Call only in a freshly forked child.
unsafe fn detach(open_limit: i32) {
    // Its own session, so the terminal's Ctrl-C and hangup that end the run host spare the
    // watcher.
    let _ = nix::unistd::setsid();
    // Copies of agents' pipes would keep an agent from ever seeing end of input, and copies of
    // lock files would keep their locks held.
    for fd in 0..open_limit {
        // SAFETY: closing descriptors in the forked child touches nothing of the parent's.
        unsafe { libc::close(fd) };
    }
    // SAFETY: open and dup2 on the lowest free descriptors, 0 to 2.
    unsafe {
        if libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) == 0 {
            libc::dup2(0, 1);
            libc::dup2(0, 2);
        }
    }
}

fn run_host_exits_first(target: &Target, tree: &mut Tree) -> bool {
    let mut events = match Events::watch(target) {
        Watch::Started(events) => events,
        Watch::RunHostGone => return true,
        Watch::Failed => return false,
    };
    // When the run host forked this watcher, a new parent means it died before its exit was
    // being watched.
    if target.run_host == target.forking && getppid().as_raw() != target.run_host {
        return true;
    }
    loop {
        match events.wait() {
            Wake::RunHostExited => return true,
            Wake::Tick if !tree.note(target) => return false,
            Wake::Tick | Wake::HeadExited => {}
            Wake::Failed => return false,
        }
    }
}

/// What the watcher knows of the agent's tree.
struct Tree {
    table: LazyTable,
    noted: Noted,
    /// Whether the head's session and group ids still reach the agent's processes. Once
    /// both are empty they're free to reuse, and only the noted processes do.
    traced: bool,
}

impl Tree {
    /// Notes the tree as it is now. Returns whether any of it lives: the group, the session, or a
    /// noted process.
    fn note(&mut self, target: &Target) -> bool {
        let group_lives = killpg(Pid::from_raw(target.pgid), None).is_ok();
        let Some(table) = self.table.get() else {
            // Can't tell, so keep watching until the guard is dropped.
            return true;
        };
        if table.read().is_err() {
            return true;
        }
        if self.traced
            && !group_lives
            && !table
                .procs()
                .any(|proc| proc.sid == target.head || proc.pgid == target.pgid)
        {
            self.traced = false;
        }
        if self.traced {
            table.mark_tree(target.head, target.pgid);
        }
        table.mark_noted(self.noted.procs());
        self.noted.update(table);
        self.traced || !self.noted.procs().is_empty()
    }
}

/// Processes seen in the tree that were still alive at the last note, by pid and start time.
/// Its memory is allocated before the fork and never grows.
struct Noted {
    procs: Box<[(i32, u64)]>,
    len: usize,
}

impl Noted {
    fn new() -> Self {
        Self {
            procs: vec![(0, 0); NOTED_CAPACITY].into_boxed_slice(),
            len: 0,
        }
    }

    /// The tree as it is now. It reads the table into memory of its own, so call it before the
    /// fork.
    fn of_tree(head: i32, pgid: i32) -> Self {
        let mut noted = Self::new();
        let mut storage = Storage::allocate();
        let mut table = storage.table();
        if table.read().is_ok() {
            table.mark_tree(head, pgid);
            noted.update(&table);
        }
        noted
    }

    fn procs(&self) -> &[(i32, u64)] {
        &self.procs[..self.len]
    }

    /// Forgets the processes that are gone from `table`, then notes its marked ones.
    fn update(&mut self, table: &Table) {
        let mut kept = 0;
        for at in 0..self.len {
            let (pid, start) = self.procs[at];
            if table.has(pid, start) {
                self.procs[kept] = (pid, start);
                kept += 1;
            }
        }
        self.len = kept;
        for proc in table.doomed() {
            if self.len == self.procs.len() {
                return;
            }
            let entry = (proc.pid, proc.start);
            if proc.pid > 1 && !self.procs().contains(&entry) {
                self.procs[self.len] = entry;
                self.len += 1;
            }
        }
    }
}

fn sweep(target: &Target, tree: &mut Tree) {
    let me = getpid().as_raw();
    let Some(table) = tree.table.get() else {
        if tree.traced {
            let _ = killpg(Pid::from_raw(target.pgid), Signal::SIGKILL);
        }
        return;
    };
    // Ids of 0 skip the session and group, which may be someone else's by now.
    let (head, pgid) = if tree.traced {
        (target.head, target.pgid)
    } else {
        (0, 0)
    };
    for _ in 0..SWEEPS {
        let _ = super::sweep_tree(table, head, pgid, tree.noted.procs(), me);
        #[cfg(target_os = "linux")]
        if !target.launch_entry.is_empty() {
            let _ = super::kill_launch(table, target.launch_entry, me);
        }
    }
}

/// The watcher's table, in memory mapped straight from the kernel the first time it's needed.
#[derive(Default)]
struct LazyTable {
    table: Option<Table<'static>>,
}

impl LazyTable {
    fn get(&mut self) -> Option<&mut Table<'static>> {
        if self.table.is_none() {
            // SAFETY: all-zero is a valid `Row` and a valid `i32`.
            let rows = unsafe { map_zeroed::<Row>(CAPACITY) }?;
            let pids = unsafe { map_zeroed::<i32>(PID_SCRATCH) }?;
            self.table = Some(Table::new(rows, pids));
        }
        self.table.as_mut()
    }
}

/// Zeroed memory for `count` values, never unmapped, since the watcher exits soon after.
///
/// # Safety
/// All-zero bytes must be a valid `T`.
unsafe fn map_zeroed<T>(count: usize) -> Option<&'static mut [T]> {
    if count == 0 {
        return Some(&mut []);
    }
    let bytes = count.checked_mul(size_of::<T>())?;
    // SAFETY: a fresh private anonymous mapping, owned by nothing else.
    let memory = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            bytes,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if memory == libc::MAP_FAILED {
        return None;
    }
    // SAFETY: the mapping is page-aligned, `bytes` long, zeroed, and all-zero is a valid `T`.
    Some(unsafe { std::slice::from_raw_parts_mut(memory.cast::<T>(), count) })
}

enum Watch {
    Started(Events),
    RunHostGone,
    Failed,
}

enum Wake {
    RunHostExited,
    HeadExited,
    /// Time to note the tree and check that it still lives.
    Tick,
    Failed,
}

/// Exits come through kqueue's `NOTE_EXIT`.
#[cfg(target_os = "macos")]
struct Events {
    queue: nix::sys::event::Kqueue,
    run_host: i32,
    head: i32,
    head_running: bool,
}

#[cfg(target_os = "macos")]
impl Events {
    fn watch(target: &Target) -> Watch {
        use nix::errno::Errno;
        let Ok(queue) = nix::sys::event::Kqueue::new() else {
            return Watch::Failed;
        };
        match queue.kevent(&[exit_of(target.run_host)], &mut [], None) {
            Ok(_) => {}
            Err(Errno::ESRCH) => return Watch::RunHostGone,
            Err(_) => return Watch::Failed,
        }
        // A head that's gone already leaves the watcher checking for members.
        let head_running = queue.kevent(&[exit_of(target.head)], &mut [], None).is_ok();
        Watch::Started(Self {
            queue,
            run_host: target.run_host,
            head: target.head,
            head_running,
        })
    }

    fn wait(&mut self) -> Wake {
        let tick = libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::c_long::from(DRAIN_TICK_MS) * 1_000_000,
        };
        let note = libc::timespec {
            tv_sec: libc::time_t::from(NOTE_TICK_MS / 1_000),
            tv_nsec: 0,
        };
        let timeout = Some(if self.head_running { note } else { tick });
        let mut fired = [exit_of(0), exit_of(0)];
        loop {
            match self.queue.kevent(&[], &mut fired, timeout) {
                Ok(count) => {
                    let exited = |pid: i32| {
                        fired[..count]
                            .iter()
                            .any(|event| event.ident() == pid as usize)
                    };
                    if exited(self.run_host) {
                        return Wake::RunHostExited;
                    }
                    if exited(self.head) {
                        self.head_running = false;
                        return Wake::HeadExited;
                    }
                    if count == 0 {
                        return Wake::Tick;
                    }
                }
                Err(nix::errno::Errno::EINTR) => {}
                Err(_) => return Wake::Failed,
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn exit_of(pid: i32) -> nix::sys::event::KEvent {
    use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent};
    KEvent::new(
        pid as usize,
        EventFilter::EVFILT_PROC,
        EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
        FilterFlag::NOTE_EXIT,
        0,
        0,
    )
}

/// Exits come through pidfds, which turn readable when their process exits. Without pidfds
/// (Linux before 5.3), the run host's exit shows as the watcher getting a new parent.
#[cfg(target_os = "linux")]
struct Events {
    run_host: Option<std::os::fd::OwnedFd>,
    head: Option<std::os::fd::OwnedFd>,
    parent: i32,
}

#[cfg(target_os = "linux")]
impl Events {
    fn watch(target: &Target) -> Watch {
        use nix::errno::Errno;
        let run_host = match pidfd_open(target.run_host) {
            Ok(fd) => Some(fd),
            Err(Errno::ESRCH) => return Watch::RunHostGone,
            Err(_) if target.run_host == target.forking => None,
            Err(_) => return Watch::Failed,
        };
        Watch::Started(Self {
            run_host,
            // A head that's gone already leaves the watcher checking for members.
            head: pidfd_open(target.head).ok(),
            parent: target.run_host,
        })
    }

    fn wait(&mut self) -> Wake {
        use std::os::fd::AsRawFd;
        let poll_of = |fd: &Option<std::os::fd::OwnedFd>| libc::pollfd {
            // poll skips negative descriptors.
            fd: fd.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        };
        let mut fds = [poll_of(&self.run_host), poll_of(&self.head)];
        let timeout = if self.run_host.is_some() && self.head.is_some() {
            NOTE_TICK_MS
        } else {
            DRAIN_TICK_MS
        };
        loop {
            // SAFETY: `fds` holds two initialized entries.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
            if ready < 0 {
                if nix::errno::Errno::last() == nix::errno::Errno::EINTR {
                    continue;
                }
                return Wake::Failed;
            }
            if fds[0].revents != 0 {
                return Wake::RunHostExited;
            }
            if fds[1].revents != 0 {
                self.head = None;
                return Wake::HeadExited;
            }
            if self.run_host.is_none() && getppid().as_raw() != self.parent {
                return Wake::RunHostExited;
            }
            return Wake::Tick;
        }
    }
}

#[cfg(target_os = "linux")]
fn pidfd_open(pid: i32) -> nix::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    // SAFETY: pidfd_open takes a pid and flags, and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(nix::errno::Errno::last());
    }
    let fd = i32::try_from(fd).map_err(|_| nix::errno::Errno::EBADF)?;
    // SAFETY: `fd` was just opened and nothing else holds it.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}
