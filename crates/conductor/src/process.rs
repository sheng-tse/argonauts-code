//! Runs agent processes. Each one leads its own session and process group, its output is read
//! line by line, and signals go to the whole group so nothing it started is left behind.

use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;

use argonauts_core::agents::AgentGroup;
use argonauts_core::layout::session_files;
use argonauts_core::{CommandSpec, StopSignal, create_private_dir_all, write_private_file};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::rawlog::{CopyingReader, OutputCopies};

/// Longest output line kept, in bytes. The rest of a longer line is dropped and reported, and
/// only a raw log copy keeps it.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Lines buffered between the reader tasks and the consumer. Small, because lines can be large.
const LINE_QUEUE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One line of output, without its line ending. Invalid UTF-8 is replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub stream: Stream,
    pub text: String,
    /// Bytes cut from the end because the line was longer than [`MAX_LINE_BYTES`].
    pub dropped: usize,
}

#[derive(Debug)]
pub enum ProcessEvent {
    Line(OutputLine),
    /// Output written just before exit may still follow.
    Exited(io::Result<ExitStatus>),
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("could not write {path}: {source}")]
    File { path: PathBuf, source: io::Error },
    /// The program is not on `PATH`, or is a relative path.
    #[error("could not find `{program}` on PATH")]
    NotFound { program: String },
    /// The program lives inside the agent's working directory, where the agent could have put it.
    #[error("refusing to run `{program}` from {}, inside the agent's working directory", path.display())]
    InsideWorkdir { program: String, path: PathBuf },
    #[error("could not start `{program}`: {source}")]
    Spawn { program: String, source: io::Error },
}

/// A running process that leads its own session and process group. With no controlling terminal,
/// nothing it starts can read from or type into the user's terminal, or be suspended by it.
/// Dropping it before it exits kills the whole group.
#[derive(Debug)]
pub struct AgentProcess {
    group: u32,
    /// The leader's start time, once [`AgentProcess::record`] read it.
    start_time: OnceLock<u64>,
    launch: String,
    input: Option<mpsc::UnboundedSender<String>>,
    lines: mpsc::Receiver<OutputLine>,
    lines_open: bool,
    exit: oneshot::Receiver<io::Result<ExitStatus>>,
    exited: bool,
    readers: Vec<JoinHandle<()>>,
}

impl AgentProcess {
    /// Must be called inside a tokio runtime.
    pub fn spawn(spec: &CommandSpec) -> Result<Self, SpawnError> {
        Self::start(spec, None)
    }

    /// Like [`AgentProcess::spawn`], and copies stdout and stderr into the raw log exactly as
    /// they are read.
    pub fn spawn_logged(spec: &CommandSpec, copies: OutputCopies) -> Result<Self, SpawnError> {
        Self::start(spec, Some(copies))
    }

    fn start(spec: &CommandSpec, copies: Option<OutputCopies>) -> Result<Self, SpawnError> {
        let program = resolve_program(
            &spec.program,
            std::env::var_os("PATH").as_deref(),
            &spec.cwd,
        )?;
        write_spec_files(spec)?;
        let spawn_error = |source| SpawnError::Spawn {
            program: spec.program.clone(),
            source,
        };
        let launch = new_launch_id();
        let mut child = session_command(&program, spec, &launch)
            .spawn()
            .map_err(spawn_error)?;
        let group = child.id().ok_or_else(|| {
            spawn_error(io::Error::other(
                "the process exited before it could be tracked",
            ))
        })?;
        let (lines, readers) = start_readers(&mut child, copies);
        let input = child
            .stdin
            .take()
            .and_then(|stdin| start_input(stdin, spec));

        // The child moves into its own task so waiting never competes with reading.
        let (exit_tx, exit) = oneshot::channel();
        tokio::spawn(async move {
            let _ = exit_tx.send(child.wait().await);
        });

        Ok(Self {
            group,
            start_time: OnceLock::new(),
            launch,
            input,
            lines,
            lines_open: true,
            exit,
            exited: false,
            readers,
        })
    }

    /// The id in this launch's `ARGONAUTS_LAUNCH`, which everything it starts inherits.
    pub fn launch_id(&self) -> &str {
        &self.launch
    }

    /// The process id, which is also its process group id.
    pub fn id(&self) -> u32 {
        self.group
    }

    /// `None` once the process has exited and its output is closed. Cancel safe.
    pub async fn next_event(&mut self) -> Option<ProcessEvent> {
        let Self {
            lines,
            lines_open,
            exit,
            exited,
            ..
        } = self;
        loop {
            if !*lines_open && *exited {
                return None;
            }
            tokio::select! {
                line = lines.recv(), if *lines_open => match line {
                    Some(line) => return Some(ProcessEvent::Line(line)),
                    None => *lines_open = false,
                },
                status = &mut *exit, if !*exited => {
                    *exited = true;
                    let status = status.unwrap_or_else(|_| {
                        Err(io::Error::other("lost track of the process"))
                    });
                    return Some(ProcessEvent::Exited(status));
                }
            }
        }
    }

    pub fn try_next_line(&mut self) -> Option<OutputLine> {
        self.lines.try_recv().ok()
    }

    pub fn has_exited(&self) -> bool {
        self.exited
    }

    /// False when stdin was not kept open.
    pub fn send_input(&self, text: String) -> bool {
        self.input.as_ref().is_some_and(|tx| tx.send(text).is_ok())
    }

    pub fn close_input(&mut self) {
        self.input = None;
    }

    /// Signals the group, unless its id may belong to another group now: the leader was
    /// reaped and its pid went to another process.
    pub fn signal(&self, signal: StopSignal) -> io::Result<()> {
        if self.group_id_reused() {
            return Ok(());
        }
        signal_group(self.group, signal)
    }

    fn group_id_reused(&self) -> bool {
        let now = crate::liveness::start_time(self.group);
        match self.start_time.get() {
            Some(started) => now.is_some_and(|now| now != *started),
            // Never recorded, so once it has exited any process with its pid is another one.
            None => self.exited && now.is_some(),
        }
    }

    /// Stops reading output. Used when something the agent started keeps its pipes open.
    pub fn stop_reading(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
        self.lines_open = false;
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if !self.exited {
            let _ = signal_group(self.group, StopSignal::Kill);
        }
        for reader in &self.readers {
            reader.abort();
        }
    }
}

/// Sends `signal` to process group `group`. A group that no longer exists is not an error.
pub fn signal_group(group: u32, signal: StopSignal) -> io::Result<()> {
    let group = i32::try_from(group)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "process id out of range"))?;
    let signal = match signal {
        StopSignal::Interrupt => Signal::SIGINT,
        StopSignal::Terminate => Signal::SIGTERM,
        StopSignal::Kill => Signal::SIGKILL,
    };
    match killpg(Pid::from_raw(group), signal) {
        // macOS reports EPERM for a group whose members have all exited but not been reaped.
        Ok(()) | Err(Errno::ESRCH | Errno::EPERM) => Ok(()),
        Err(errno) => Err(io::Error::from(errno)),
    }
}

pub fn exit_parts(status: &ExitStatus) -> (Option<i32>, Option<i32>) {
    (status.code(), status.signal())
}

/// Makes the child lead a new session, and with it a process group whose id is its pid. On Linux
/// the child also gets SIGKILL when the supervisor dies. Its children don't inherit that, so only
/// an exit guard (`reaper::guard_group`) takes down the rest of its tree.
pub(crate) fn new_session(command: &mut Command) {
    #[cfg(target_os = "linux")]
    let supervisor = nix::unistd::getpid();
    // SAFETY: the closure runs in the child between fork and exec. It only makes syscalls
    // (setsid, and prctl and getppid on Linux), allocates nothing, and touches no memory shared
    // with the parent.
    unsafe {
        command.pre_exec(move || {
            nix::unistd::setsid().map_err(io::Error::from)?;
            #[cfg(target_os = "linux")]
            die_with_supervisor(supervisor)?;
            Ok(())
        });
    }
}

/// `PR_SET_PDEATHSIG` fires when the thread that forked exits, not the whole process. Agents are
/// spawned from runtime worker threads, which live as long as the supervisor's runtime.
#[cfg(target_os = "linux")]
fn die_with_supervisor(supervisor: Pid) -> io::Result<()> {
    nix::sys::prctl::set_pdeathsig(Signal::SIGKILL).map_err(io::Error::from)?;
    // The supervisor may have died before the signal was armed. A raw OS error, since
    // allocating is unsafe here.
    if nix::unistd::getppid() != supervisor {
        return Err(io::Error::from_raw_os_error(nix::libc::ESRCH));
    }
    Ok(())
}

/// A bare name is searched for on the absolute entries of `path_var`, so a relative entry such as
/// `node_modules/.bin` can't pick up a binary from the agent's directory. A path must be
/// absolute. Either way, a program inside `workdir` is refused.
pub(crate) fn resolve_program(
    program: &str,
    path_var: Option<&OsStr>,
    workdir: &Path,
) -> Result<PathBuf, SpawnError> {
    let not_found = || SpawnError::NotFound {
        program: program.to_owned(),
    };
    let found = if program.contains('/') {
        let path = PathBuf::from(program);
        if !path.is_absolute() {
            return Err(not_found());
        }
        path
    } else {
        path_var
            .map(std::env::split_paths)
            .into_iter()
            .flatten()
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
            .ok_or_else(not_found)?
    };
    if is_inside(&found, workdir) {
        return Err(SpawnError::InsideWorkdir {
            program: program.to_owned(),
            path: found,
        });
    }
    Ok(found)
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn is_inside(path: &Path, dir: &Path) -> bool {
    if path.starts_with(dir) {
        return true;
    }
    match (path.canonicalize(), dir.canonicalize()) {
        (Ok(path), Ok(dir)) => path.starts_with(dir),
        _ => false,
    }
}

/// Unique to one launch, unlike session keys, which repeat across runs.
fn new_launch_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn session_command(program: &Path, spec: &CommandSpec, launch: &str) -> Command {
    let wants_stdin = spec.stdin.is_some() || spec.keep_stdin_open;
    let mut command = Command::new(program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(if wants_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    new_session(&mut command);
    for name in &spec.env_remove {
        command.env_remove(name);
    }
    command.envs(spec.env_set.iter().map(|(k, v)| (k, v)));
    // OpenCode 1.18.34 works in `$PWD` rather than its real directory, so the PWD inherited from
    // your shell sent a card's session into your own checkout.
    command.env("PWD", &spec.cwd);
    // Set last, so no spec can forge another launch's marker.
    command.env(crate::reaper::LAUNCH_MARKER, launch);
    // No terminal to ask on, so git must fail instead of waiting for a password.
    command.env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn start_readers(
    child: &mut Child,
    copies: Option<OutputCopies>,
) -> (mpsc::Receiver<OutputLine>, Vec<JoinHandle<()>>) {
    let (stdout_copy, stderr_copy) = match copies {
        Some(OutputCopies { stdout, stderr }) => (Some(stdout), Some(stderr)),
        None => (None, None),
    };
    let (line_tx, lines) = mpsc::channel(LINE_QUEUE);
    let mut readers = Vec::with_capacity(2);
    if let Some(stdout) = child.stdout.take() {
        let stdout = CopyingReader::new(stdout, stdout_copy);
        readers.push(tokio::spawn(read_lines(
            stdout,
            Stream::Stdout,
            line_tx.clone(),
        )));
    }
    if let Some(stderr) = child.stderr.take() {
        let stderr = CopyingReader::new(stderr, stderr_copy);
        readers.push(tokio::spawn(read_lines(stderr, Stream::Stderr, line_tx)));
    }
    (lines, readers)
}

fn start_input(stdin: ChildStdin, spec: &CommandSpec) -> Option<mpsc::UnboundedSender<String>> {
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    tokio::spawn(feed_input(stdin, spec.stdin.clone(), input_rx));
    // Dropping the sender right away makes the writer close stdin after the first text.
    spec.keep_stdin_open.then_some(input_tx)
}

fn write_spec_files(spec: &CommandSpec) -> Result<(), SpawnError> {
    for file in &spec.files {
        let file_error = |source| SpawnError::File {
            path: file.path.clone(),
            source,
        };
        if let Some(parent) = file.path.parent() {
            create_private_dir_all(parent).map_err(file_error)?;
        }
        write_private_file(&file.path, file.contents.as_bytes()).map_err(file_error)?;
    }
    Ok(())
}

async fn feed_input(
    mut stdin: ChildStdin,
    first: Option<String>,
    mut more: mpsc::UnboundedReceiver<String>,
) {
    if let Some(text) = first
        && write_all(&mut stdin, &text).await.is_err()
    {
        return;
    }
    while let Some(text) = more.recv().await {
        if write_all(&mut stdin, &text).await.is_err() {
            return;
        }
    }
}

async fn write_all(stdin: &mut ChildStdin, text: &str) -> io::Result<()> {
    stdin.write_all(text.as_bytes()).await?;
    stdin.flush().await
}

async fn read_lines<R>(reader: R, stream: Stream, tx: mpsc::Sender<OutputLine>)
where
    R: AsyncRead + Unpin,
{
    let mut reader = BufReader::with_capacity(64 * 1024, reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_capped_line(&mut reader, &mut line, MAX_LINE_BYTES).await {
            Ok(Capped { consumed: 0, .. }) | Err(_) => return,
            Ok(Capped { dropped, .. }) => {
                let text = decode_line(std::mem::take(&mut line));
                let line = OutputLine {
                    stream,
                    text,
                    dropped,
                };
                if tx.send(line).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Capped {
    /// 0 only at end of input.
    pub consumed: usize,
    pub dropped: usize,
}

/// Reads one line into `buf`, keeping at most `cap` bytes of it and leaving out the newline.
pub(crate) async fn read_capped_line<R>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> io::Result<Capped>
where
    R: AsyncBufRead + Unpin,
{
    let mut done = Capped {
        consumed: 0,
        dropped: 0,
    };
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(done);
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let chunk = &available[..newline.unwrap_or(available.len())];
        let kept = chunk.len().min(cap.saturating_sub(buf.len()));
        buf.extend_from_slice(&chunk[..kept]);
        done.dropped += chunk.len() - kept;
        let used = newline.map_or(available.len(), |i| i + 1);
        reader.consume(used);
        done.consumed += used;
        if newline.is_some() {
            return Ok(done);
        }
    }
}

fn decode_line(mut bytes: Vec<u8>) -> String {
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
}

/// The child's pipes, for a session driver that speaks a protocol over them. Stderr is still
/// read as lines by the [`AgentProcess`].
#[derive(Debug)]
pub struct DriverPipes {
    pub stdin: ChildStdin,
    pub stdout: tokio::process::ChildStdout,
}

impl AgentProcess {
    /// Like [`AgentProcess::spawn`], but hands stdin and stdout to the caller, for a session
    /// driver. `spec.stdin` is not written, since the driver owns stdin. Must be called inside a
    /// tokio runtime.
    pub fn spawn_piped(spec: &CommandSpec) -> Result<(Self, DriverPipes), SpawnError> {
        let program = resolve_program(
            &spec.program,
            std::env::var_os("PATH").as_deref(),
            &spec.cwd,
        )?;
        write_spec_files(spec)?;
        let spawn_error = |source| SpawnError::Spawn {
            program: spec.program.clone(),
            source,
        };
        let launch = new_launch_id();
        let mut command = session_command(&program, spec, &launch);
        command.stdin(Stdio::piped());
        let mut child = command.spawn().map_err(spawn_error)?;
        let group = child.id().ok_or_else(|| {
            spawn_error(io::Error::other(
                "the process exited before it could be tracked",
            ))
        })?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = signal_group(group, StopSignal::Kill);
            return Err(spawn_error(io::Error::other(
                "the process started without its pipes",
            )));
        };
        // Stdout is gone from the child by now, so only stderr is read as lines.
        let (lines, readers) = start_readers(&mut child, None);
        let (exit_tx, exit) = oneshot::channel();
        tokio::spawn(async move {
            let _ = exit_tx.send(child.wait().await);
        });
        let process = Self {
            group,
            start_time: OnceLock::new(),
            launch,
            input: None,
            lines,
            lines_open: true,
            exit,
            exited: false,
            readers,
        };
        Ok((process, DriverPipes { stdin, stdout }))
    }

    /// The group, pid, and start time (`crate::liveness::start_time`), read right after the
    /// spawn.
    pub fn record(&self) -> io::Result<AgentGroup> {
        let start_time = match self.start_time.get() {
            Some(started) => *started,
            None => crate::liveness::start_time(self.group).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("process {} exited before it could be recorded", self.group),
                )
            })?,
        };
        let _ = self.start_time.set(start_time);
        Ok(AgentGroup {
            pgid: self.group,
            pid: self.group,
            start_time,
        })
    }
}

/// Writes `layout::session_files::PROCESS` in `session_dir`, private to you. The file is
/// replaced whole, so a crash never leaves half a record for recovery to read.
pub fn write_process_file(session_dir: &Path, record: &AgentGroup) -> io::Result<()> {
    create_private_dir_all(session_dir)?;
    let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    let path = session_dir.join(session_files::PROCESS);
    let partial = session_dir.join(format!("{}.partial", session_files::PROCESS));
    write_private_file(&partial, &json)?;
    std::fs::rename(&partial, &path)
}

/// The record in `session_dir`, or `None` when the session never wrote one.
pub fn read_process_file(session_dir: &Path) -> io::Result<Option<AgentGroup>> {
    match std::fs::read(session_dir.join(session_files::PROCESS)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{sh_spec, wait_until_gone};
    use argonauts_core::SpecFile;
    use std::time::{Duration, Instant};

    async fn collect(mut process: AgentProcess) -> (Vec<OutputLine>, ExitStatus) {
        let mut lines = Vec::new();
        let mut status = None;
        while let Some(event) = process.next_event().await {
            match event {
                ProcessEvent::Line(line) => lines.push(line),
                ProcessEvent::Exited(exit) => status = Some(exit.unwrap()),
            }
        }
        (lines, status.unwrap())
    }

    fn texts(lines: &[OutputLine], stream: Stream) -> Vec<&str> {
        lines
            .iter()
            .filter(|l| l.stream == stream)
            .map(|l| l.text.as_str())
            .collect()
    }

    #[tokio::test]
    async fn reads_both_streams_line_by_line() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            dir.path(),
            "printf 'one\\ntwo\\r\\n'; echo oops >&2; printf 'no newline'",
        );
        let (lines, status) = collect(AgentProcess::spawn(&spec).unwrap()).await;
        assert!(status.success());
        assert_eq!(texts(&lines, Stream::Stdout), ["one", "two", "no newline"]);
        assert_eq!(texts(&lines, Stream::Stderr), ["oops"]);
    }

    #[tokio::test]
    async fn the_raw_log_keeps_what_lines_cut_and_decode() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let raw = crate::rawlog::RawLog::create(&session).unwrap();
        let long = MAX_LINE_BYTES + 100;
        let script = format!(
            "head -c {long} /dev/zero | tr '\\0' a; printf '\\n\\377 bad\\r\\nend'; \
             printf 'warn\\r\\n\\376\\n' >&2"
        );
        let spec = sh_spec(dir.path(), &script);
        let process = AgentProcess::spawn_logged(&spec, raw.output_copies()).unwrap();
        let (lines, status) = collect(process).await;
        assert!(status.success());

        let stdout: Vec<_> = lines
            .iter()
            .filter(|l| l.stream == Stream::Stdout)
            .collect();
        assert_eq!(stdout[0].text.len(), MAX_LINE_BYTES);
        assert_eq!(stdout[0].dropped, 100);
        assert_eq!(stdout[1].text, "\u{fffd} bad");
        assert_eq!(texts(&lines, Stream::Stderr), ["warn", "\u{fffd}"]);

        let mut expected = vec![b'a'; long];
        expected.extend_from_slice(b"\n\xff bad\r\nend\n");
        let logged = std::fs::read(raw.stdout_path()).unwrap();
        assert!(
            logged == expected,
            "the raw stdout differs from what was printed"
        );
        let logged = std::fs::read(raw.stderr_path()).unwrap();
        assert_eq!(logged, b"warn\r\n\xfe\n");
    }

    #[tokio::test]
    async fn runs_in_its_own_session_and_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(dir.path(), "sleep 5");
        let process = AgentProcess::spawn(&spec).unwrap();
        let pid = Pid::from_raw(process.id() as i32);
        assert_eq!(nix::unistd::getpgid(Some(pid)).unwrap(), pid);
        assert_eq!(nix::unistd::getsid(Some(pid)).unwrap(), pid);
        assert_ne!(pid, nix::unistd::getpgrp());
    }

    #[tokio::test]
    async fn the_terminal_is_out_of_reach() {
        let dir = tempfile::tempdir().unwrap();
        let spec = sh_spec(
            dir.path(),
            "if (: < /dev/tty) 2>/dev/null; then echo tty; else echo no-tty; fi; echo \"prompt=$GIT_TERMINAL_PROMPT\"",
        );
        let started = Instant::now();
        let (lines, status) = collect(AgentProcess::spawn(&spec).unwrap()).await;
        assert!(status.success());
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(texts(&lines, Stream::Stdout), ["no-tty", "prompt=0"]);
    }

    #[tokio::test]
    async fn pwd_names_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec::new("/usr/bin/printenv", dir.path()).arg("PWD");
        let (lines, status) = collect(AgentProcess::spawn(&spec).unwrap()).await;
        assert!(status.success());
        let pwd = dir.path().display().to_string();
        assert_eq!(texts(&lines, Stream::Stdout), [pwd.as_str()]);
    }

    #[tokio::test]
    async fn writes_files_env_and_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/schema.json");
        let mut spec = sh_spec(
            dir.path(),
            "cat \"$1\"; echo; echo \"set=$SET_ME home=${HOME:-gone}\"; cat",
        );
        spec.args.push(file.display().to_string());
        spec.files.push(SpecFile {
            path: file.clone(),
            contents: "{\"type\":\"object\"}".into(),
        });
        spec.env_set.push(("SET_ME".into(), "yes".into()));
        spec.env_remove.push("HOME".into());
        spec.stdin = Some("from stdin\n".into());

        let (lines, status) = collect(AgentProcess::spawn(&spec).unwrap()).await;
        assert!(status.success());
        assert_eq!(
            texts(&lines, Stream::Stdout),
            ["{\"type\":\"object\"}", "set=yes home=gone", "from stdin"]
        );
    }

    #[tokio::test]
    async fn every_launch_gets_its_own_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = sh_spec(dir.path(), "printf '%s' \"$ARGONAUTS_LAUNCH\"");
        spec.env_set
            .push((crate::reaper::LAUNCH_MARKER.into(), "forged".into()));
        let first = AgentProcess::spawn(&spec).unwrap();
        let second = AgentProcess::spawn(&spec).unwrap();
        assert_ne!(first.launch_id(), second.launch_id());
        let launch = first.launch_id().to_owned();
        let (lines, _) = collect(first).await;
        assert_eq!(texts(&lines, Stream::Stdout), [launch]);
    }

    #[tokio::test]
    async fn kept_open_stdin_accepts_more_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = sh_spec(
            dir.path(),
            "read a; echo \"got $a\"; read b; echo \"got $b\"",
        );
        spec.stdin = Some("first\n".into());
        spec.keep_stdin_open = true;
        let mut process = AgentProcess::spawn(&spec).unwrap();
        match process.next_event().await {
            Some(ProcessEvent::Line(line)) => assert_eq!(line.text, "got first"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(process.send_input("second\n".into()));
        process.close_input();
        let (lines, status) = collect(process).await;
        assert!(status.success());
        assert_eq!(texts(&lines, Stream::Stdout), ["got second"]);
    }

    #[tokio::test]
    async fn stdin_is_closed_unless_kept_open() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = sh_spec(dir.path(), "cat; echo done");
        spec.stdin = Some("only this\n".into());
        let process = AgentProcess::spawn(&spec).unwrap();
        assert!(!process.send_input("more".into()));
        let (lines, _) = collect(process).await;
        assert_eq!(texts(&lines, Stream::Stdout), ["only this", "done"]);
    }

    #[tokio::test]
    async fn signals_reach_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let spec = sh_spec(
            dir.path(),
            &format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
        );
        let process = AgentProcess::spawn(&spec).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pid_file.exists()
            || std::fs::read_to_string(&pid_file)
                .unwrap()
                .trim()
                .is_empty()
        {
            assert!(Instant::now() < deadline, "the script never wrote its pid");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let grandchild: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        process.signal(StopSignal::Terminate).unwrap();
        let (_, status) = collect(process).await;
        assert_eq!(exit_parts(&status).1, Some(Signal::SIGTERM as i32));
        wait_until_gone(grandchild).await;
    }

    #[tokio::test]
    async fn dropping_a_running_process_kills_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let process = AgentProcess::spawn(&sh_spec(dir.path(), "sleep 30")).unwrap();
        let pid = process.id() as i32;
        drop(process);
        wait_until_gone(pid).await;
    }

    #[tokio::test]
    async fn missing_program_is_a_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec::new("argonauts-no-such-program", dir.path());
        let err = AgentProcess::spawn(&spec).unwrap_err();
        assert!(matches!(err, SpawnError::NotFound { .. }), "{err}");
        assert!(err.to_string().contains("argonauts-no-such-program"));
    }

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn programs_resolve_only_from_absolute_path_entries() {
        let dir = tempfile::tempdir().unwrap();
        let tools = dir.path().join("tools");
        let work = dir.path().join("work");
        executable(&tools.join("agent"));
        executable(&work.join("bin/agent"));

        let path = std::env::join_paths([PathBuf::from("bin"), tools.clone()]).unwrap();
        let found = resolve_program("agent", Some(&path), &work).unwrap();
        assert_eq!(found, tools.join("agent"));

        let relative_only = std::env::join_paths([PathBuf::from("bin")]).unwrap();
        let err = resolve_program("agent", Some(&relative_only), &work).unwrap_err();
        assert!(matches!(err, SpawnError::NotFound { .. }), "{err}");

        let inside = std::env::join_paths([work.join("bin")]).unwrap();
        let err = resolve_program("agent", Some(&inside), &work).unwrap_err();
        assert!(matches!(err, SpawnError::InsideWorkdir { .. }), "{err}");

        let planted = work.join("bin/agent");
        let err = resolve_program(planted.to_str().unwrap(), None, &work).unwrap_err();
        assert!(matches!(err, SpawnError::InsideWorkdir { .. }), "{err}");
        assert!(resolve_program("./bin/agent", None, &work).is_err());
        assert_eq!(
            resolve_program("/bin/sh", None, &work).unwrap(),
            Path::new("/bin/sh")
        );
    }

    #[tokio::test]
    async fn spec_files_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("scratch/schema.json");
        let mut spec = sh_spec(dir.path(), "true");
        spec.files.push(SpecFile {
            path: file.clone(),
            contents: "{}".into(),
        });
        collect(AgentProcess::spawn(&spec).unwrap()).await;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn signalling_a_gone_group_is_fine() {
        // Group ids this large are never handed out.
        signal_group(i32::MAX as u32 - 7, StopSignal::Interrupt).unwrap();
    }

    #[tokio::test]
    async fn long_lines_are_capped_and_the_cut_is_reported() {
        let input: &[u8] = b"abcdefghij\nshort\n\xff tail";
        let mut reader = tokio::io::BufReader::with_capacity(4, input);
        let mut buf = Vec::new();
        assert_eq!(
            read_capped_line(&mut reader, &mut buf, 4).await.unwrap(),
            Capped {
                consumed: 11,
                dropped: 6
            }
        );
        assert_eq!(buf, b"abcd");
        buf.clear();
        let short = read_capped_line(&mut reader, &mut buf, 4).await.unwrap();
        assert_eq!(buf, b"shor");
        assert_eq!(short.dropped, 1);
        buf.clear();
        let tail = read_capped_line(&mut reader, &mut buf, 64).await.unwrap();
        assert_eq!(tail.dropped, 0);
        assert_eq!(decode_line(buf.clone()), "\u{fffd} tail");
        buf.clear();
        assert_eq!(
            read_capped_line(&mut reader, &mut buf, 64)
                .await
                .unwrap()
                .consumed,
            0
        );
    }

    #[tokio::test]
    async fn spawn_piped_hands_over_stdin_and_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let script = r#"read line; echo "got $line"; echo "note" >&2"#;
        let (process, pipes) = AgentProcess::spawn_piped(&sh_spec(dir.path(), script)).unwrap();
        let DriverPipes { mut stdin, stdout } = pipes;
        stdin.write_all(b"hello\n").await.unwrap();
        stdin.flush().await.unwrap();
        let mut reply = String::new();
        BufReader::new(stdout).read_line(&mut reply).await.unwrap();
        assert_eq!(reply, "got hello\n");
        assert!(!process.send_input("ignored".into()));

        let (lines, status) = collect(process).await;
        assert!(status.success());
        assert!(texts(&lines, Stream::Stdout).is_empty());
        assert_eq!(texts(&lines, Stream::Stderr), ["note"]);
    }

    #[test]
    fn process_file_is_private_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        assert_eq!(read_process_file(&session).unwrap(), None);
        let record = AgentGroup {
            pgid: 4242,
            pid: 4242,
            start_time: 1_790_000_000,
        };
        write_process_file(&session, &record).unwrap();
        let path = session.join(session_files::PROCESS);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(read_process_file(&session).unwrap(), Some(record));
    }
}
