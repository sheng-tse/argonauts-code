//! `!` commands. Each runs as you in the repo folder, with your shell and outside the sandbox,
//! since you typed it. Its output shows as it prints, and the command and its output wait for
//! the lead's next turn; running one never starts a turn by itself. One runs at a time.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use argonauts_core::NoteLevel;
use argonauts_core::app::{AppEvent, ShellEvent};
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::sanitize::sanitize_for_terminal;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;

use super::{Ctx, Internal};

/// How much of one command's output the lead gets, as Claude Code caps its own: the start and
/// the end, with the middle cut.
const KEPT_BYTES: usize = 30_000;

/// What the commands waiting for your next message hold together, a few commands' worth, so
/// a long session of them can't swamp the prompt. The oldest go first.
const HELD_BYTES: usize = 3 * KEPT_BYTES;

/// A job the command left in the background can hold its pipes open after it ends. Output
/// still on the way gets this long.
const DRAIN: Duration = Duration::from_millis(300);

pub(crate) const STILL_RUNNING: &str = "A command is still running. Ctrl+C stops it.";

#[derive(Default)]
pub(crate) struct ShellState {
    last_id: u32,
    running: Option<Running>,
}

struct Running {
    id: u32,
    command: String,
    output: Kept,
    /// The shell leads its own session and process group, so a stop reaches everything it
    /// started.
    group: Option<Pid>,
    stopped: bool,
}

/// From a command's task to the loop.
#[derive(Debug)]
pub(crate) enum ShellUpdate {
    Output { id: u32, text: String },
    Done { id: u32, exit_code: Option<i32> },
}

/// A finished command, kept for the lead's next turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellRan {
    /// Commands run one at a time and are numbered as they start, so a higher id ended later.
    pub id: u32,
    pub command: String,
    pub output: String,
    pub exit_code: Option<i32>,
    pub stopped: bool,
}

pub(crate) fn run(ctx: &mut Ctx, shell: &mut ShellState, command: String) {
    let command = command.trim_end().to_owned();
    if command.trim().is_empty() {
        return;
    }
    if shell.running.is_some() {
        ctx.notice(NoteLevel::Info, STILL_RUNNING);
        return;
    }
    shell.last_id += 1;
    let id = shell.last_id;
    ctx.send(AppEvent::Shell {
        event: ShellEvent::Started {
            id,
            command: command.clone(),
        },
    });
    let program = user_shell();
    let mut child = match spawn(&program, &command, &ctx.env.repo_root) {
        Ok(child) => child,
        Err(err) => {
            let text = format!("Couldn't start {}: {err}\n", program.display());
            ctx.send(AppEvent::Shell {
                event: ShellEvent::Output { id, text },
            });
            ctx.send(AppEvent::Shell {
                event: ShellEvent::Finished {
                    id,
                    exit_code: None,
                    stopped: false,
                },
            });
            return;
        }
    };
    let group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .map(Pid::from_raw);
    let readers = [
        child
            .stdout
            .take()
            .map(|pipe| forward(pipe, id, &ctx.internal)),
        child
            .stderr
            .take()
            .map(|pipe| forward(pipe, id, &ctx.internal)),
    ];
    tokio::spawn(watch(child, readers, id, ctx.internal.clone()));
    shell.running = Some(Running {
        id,
        command,
        output: Kept::default(),
        group,
        stopped: false,
    });
}

/// Ctrl+C: an interrupt, as a terminal sends one. A command that ignores it gets killed by the
/// next Ctrl+C.
pub(crate) fn stop(shell: &mut ShellState) {
    let Some(running) = shell.running.as_mut() else {
        return;
    };
    let signal = if running.stopped {
        Signal::SIGKILL
    } else {
        Signal::SIGINT
    };
    running.stopped = true;
    if let Some(group) = running.group {
        let _ = killpg(group, signal);
    }
}

/// The app is closing, so nothing it started outlives it.
pub(crate) fn close(shell: &mut ShellState) {
    if let Some(group) = shell.running.take().and_then(|running| running.group) {
        let _ = killpg(group, Signal::SIGKILL);
    }
}

/// Shows the output, and hands back the command once it ends.
pub(crate) fn on_update(
    ctx: &mut Ctx,
    shell: &mut ShellState,
    update: ShellUpdate,
) -> Option<ShellRan> {
    match update {
        ShellUpdate::Output { id, text } => {
            let running = shell.running.as_mut().filter(|running| running.id == id)?;
            running.output.push(&text);
            ctx.send(AppEvent::Shell {
                event: ShellEvent::Output { id, text },
            });
            None
        }
        ShellUpdate::Done { id, exit_code } => {
            let running = shell.running.take_if(|running| running.id == id)?;
            ctx.send(AppEvent::Shell {
                event: ShellEvent::Finished {
                    id,
                    exit_code,
                    stopped: running.stopped,
                },
            });
            Some(ShellRan {
                id,
                command: running.command,
                output: running.output.text(),
                exit_code,
                stopped: running.stopped,
            })
        }
    }
}

/// Keeps `ran` for your next message, past [`HELD_BYTES`] without the oldest commands.
pub(crate) fn hold(held: &mut Vec<ShellRan>, ran: ShellRan) {
    held.push(ran);
    let size = |ran: &ShellRan| ran.command.len() + ran.output.len();
    let mut total: usize = held.iter().map(size).sum();
    while held.len() > 1 && total > HELD_BYTES {
        total -= size(&held.remove(0));
    }
}

/// The commands you ran since the lead's last turn, oldest first, as one part of its prompt.
pub(crate) fn context_part(ran: &[ShellRan]) -> Option<InputPart> {
    let text = context_text(ran)?;
    Some(super::turn::part(
        InputPartKind::Message,
        "shell commands",
        text,
    ))
}

/// The commands and their output in words, for a message to the lead or to a run. Output is
/// cleaned of terminal escapes, which mean nothing to a model and could reach a terminal that
/// shows the prompt.
pub(crate) fn context_text(ran: &[ShellRan]) -> Option<String> {
    if ran.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Before this message I ran these commands myself, in the repo folder. Their output is \
         here as context.\n",
    );
    for command in ran {
        text.push_str(&format!(
            "\n$ {}\n",
            sanitize_for_terminal(&command.command)
        ));
        let output = sanitize_for_terminal(&settled(&command.output));
        if !output.is_empty() {
            text.push_str(&output);
            text.push('\n');
        }
        let end = match (command.stopped, command.exit_code) {
            (true, _) => "(stopped with Ctrl+C)".to_owned(),
            (false, Some(0)) => String::new(),
            (false, Some(code)) => format!("(exit code {code})"),
            (false, None) => "(ended by a signal)".to_owned(),
        };
        if !end.is_empty() {
            text.push_str(&end);
            text.push('\n');
        }
    }
    Some(text)
}

/// Your shell, as your terminal would start it, or `sh` without one.
fn user_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map_or_else(|| PathBuf::from("/bin/sh"), PathBuf::from)
}

fn spawn(program: &Path, command: &str, folder: &Path) -> std::io::Result<Child> {
    let mut shell = Command::new(program);
    shell
        .arg("-c")
        .arg(command)
        .current_dir(folder)
        // Nothing can type into it, so a command that waits for input ends instead of hanging.
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: setsid is async-signal-safe and touches no memory the parent shares. A session
    // of its own leaves the command no terminal, so one that asks there, such as sudo for a
    // password, fails at once rather than stopping in the background for good. Its process
    // group is the shell's pid, which a stop signals.
    unsafe {
        shell.pre_exec(|| {
            nix::unistd::setsid()
                .map(drop)
                .map_err(std::io::Error::from)
        });
    }
    shell.spawn()
}

async fn watch(
    mut child: Child,
    readers: [Option<tokio::task::JoinHandle<()>>; 2],
    id: u32,
    internal: UnboundedSender<Internal>,
) {
    let exit_code = child.wait().await.ok().and_then(|status| status.code());
    for reader in readers.into_iter().flatten() {
        let abort = reader.abort_handle();
        if tokio::time::timeout(DRAIN, reader).await.is_err() {
            abort.abort();
        }
    }
    let _ = internal.send(Internal::Shell(ShellUpdate::Done { id, exit_code }));
}

/// Standard output and error go out as one stream, in the order they arrive, as a terminal
/// shows them.
fn forward<R>(
    mut pipe: R,
    id: u32,
    internal: &UnboundedSender<Internal>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let internal = internal.clone();
    tokio::spawn(async move {
        let mut buffer = vec![0; 8192];
        let mut pending = Vec::new();
        loop {
            let read = match pipe.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            pending.extend_from_slice(&buffer[..read]);
            let text = whole_chars(&mut pending);
            if text.is_empty() {
                continue;
            }
            if internal
                .send(Internal::Shell(ShellUpdate::Output { id, text }))
                .is_err()
            {
                return;
            }
        }
        if !pending.is_empty() {
            let text = String::from_utf8_lossy(&pending).into_owned();
            let _ = internal.send(Internal::Shell(ShellUpdate::Output { id, text }));
        }
    })
}

/// The text at the front of `bytes`, leaving a character a read cut in two for the next read.
fn whole_chars(bytes: &mut Vec<u8>) -> String {
    let end = match std::str::from_utf8(bytes) {
        Err(err) if err.error_len().is_none() => err.valid_up_to(),
        _ => bytes.len(),
    };
    let text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    bytes.drain(..end);
    text
}

/// Each line as a terminal leaves it: a carriage return starts the line over, as a progress
/// bar does.
fn settled(output: &str) -> String {
    output
        .replace("\r\n", "\n")
        .trim_end_matches('\n')
        .split('\n')
        .map(|line| {
            line.trim_end_matches('\r')
                .rsplit('\r')
                .next()
                .unwrap_or("")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The start and the end of a command's output, with the middle cut once it passes
/// [`KEPT_BYTES`].
#[derive(Debug, Default)]
struct Kept {
    head: String,
    tail: String,
    cut_lines: usize,
    cut: bool,
}

impl Kept {
    fn push(&mut self, text: &str) {
        let half = KEPT_BYTES / 2;
        let room = floor_boundary(text, half.saturating_sub(self.head.len()));
        self.head.push_str(&text[..room]);
        self.tail.push_str(&text[room..]);
        // Trimmed in batches, so a chatty command isn't copied once per read.
        if self.tail.len() > 2 * half {
            self.trim_tail(half);
        }
    }

    fn trim_tail(&mut self, keep: usize) {
        if self.tail.len() <= keep {
            return;
        }
        let from = ceil_boundary(&self.tail, self.tail.len() - keep);
        self.cut_lines += self.tail[..from].matches('\n').count();
        self.cut = true;
        self.tail.drain(..from);
    }

    fn text(mut self) -> String {
        self.trim_tail(KEPT_BYTES / 2);
        if !self.cut {
            return self.head + &self.tail;
        }
        let lines = if self.cut_lines == 1 { "line" } else { "lines" };
        format!(
            "{}\n[… {} {lines} cut …]\n{}",
            self.head.trim_end_matches('\n'),
            self.cut_lines,
            self.tail
        )
    }
}

fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests;
