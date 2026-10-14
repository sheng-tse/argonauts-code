//! Screen-reader mode: flat labeled lines (`you:`, `claude-code, worker, task-002:`, `tool:`,
//! `narrator:`), no boxes, spinners, or redraws, menus and questions as numbered lists, and the
//! bell when something needs you. It grew out of the 0.0.1 plain renderer, and it suits any output
//! that isn't a terminal too.

mod about;
mod agent;
mod answer;
mod decisions;
mod lines;
mod offer;
mod pick;
mod questions;
mod reader;
mod run;
#[cfg(test)]
mod tests;
mod views;
mod words;

use std::io::{self, BufRead, IsTerminal, Write};

use tokio::sync::mpsc;

use crate::UiOptions;
use crate::runtime::{AppIo, run_in_tmux};
use crate::text::sanitize_line;

pub use lines::{answer_lines, lines_for, rings_for};
pub use pick::{Pick, Typed};
pub use questions::{ListTarget, QuestionList};
pub use reader::{Reader, Said, TmuxTakeOver};
pub use views::view_lines;

const BELL: &[u8] = b"\x07";

/// Reads app events out as lines on stdout, and typed lines from stdin as answers, commands,
/// and messages. Returns after `/exit`, Ctrl+D, or `AppEvent::Exit`.
pub async fn run_screen_reader(io: AppIo, options: UiOptions) -> anyhow::Result<()> {
    // The bell only rings on a terminal, so logs and pipes stay clean.
    let bell = io::stdout().is_terminal();
    run_lines(io, &options, io::stdout(), stdin_lines(), bell).await?;
    Ok(())
}

/// [`run_screen_reader`] on any output and any source of typed lines, where `typed` closing is
/// the end of input. Gives the output back.
pub async fn run_lines<W: Write>(
    mut io: AppIo,
    options: &UiOptions,
    mut out: W,
    mut typed: mpsc::UnboundedReceiver<String>,
    terminal: bool,
) -> anyhow::Result<W> {
    let mut reader = Reader::new(options);
    let (returned, mut returns) = mpsc::unbounded_channel();
    let mut typing = true;
    loop {
        let said = tokio::select! {
            // What the app already sent is read out before the next typed line is acted on.
            biased;
            event = io.events.recv() => match event {
                Some(event) => reader.event(&event),
                None => break,
            },
            line = typed.recv(), if typing => match line {
                Some(line) => reader.typed(&line),
                None => {
                    typing = false;
                    reader.input_ended()
                }
            },
            Some((job, opened)) = returns.recv() => reader.tmux_returned(job, opened),
        };
        let bell = terminal && said.bell && bell_on(&reader);
        write_said(&mut out, &said.lines, bell)?;
        for command in said.commands {
            if io.commands.send(command).await.is_err() {
                return Ok(out);
            }
        }
        if let Some(job) = said.tmux {
            let returned = returned.clone();
            tokio::spawn(async move {
                let opened = run_in_tmux(&job.argv, &job.sentinel).await;
                let _ = returned.send((job, opened));
            });
        }
        if said.exit {
            break;
        }
    }
    out.flush()?;
    Ok(out)
}

fn bell_on(reader: &Reader) -> bool {
    reader.state.ui.as_ref().is_none_or(|ui| ui.bell)
}

fn write_said<W: Write>(out: &mut W, lines: &[String], bell: bool) -> io::Result<()> {
    for line in lines {
        writeln!(out, "{}", sanitize_line(line))?;
    }
    if bell {
        out.write_all(BELL)?;
    }
    out.flush()
}

/// Typed lines from stdin, read on a thread of their own because stdin blocks.
fn stdin_lines() -> mpsc::UnboundedReceiver<String> {
    let (lines, typed) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    typed
}
