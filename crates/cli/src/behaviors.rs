//! `argonauts behaviors`: each model's behaviors from your run records, most distinctive
//! first, or every agent session's actions written out for Hodoscope. With `--deep`, it updates
//! the deeper Behaviors map instead, asking at the terminal where the tab would ask.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;
use std::process::ExitCode;

use anyhow::anyhow;
use argonauts_app::DeepTerminal;
use argonauts_core::sanitize::sanitize_for_terminal;

use crate::args::BehaviorsArgs;
use crate::usage::usage;

pub async fn run(args: &BehaviorsArgs, set: &[String]) -> anyhow::Result<ExitCode> {
    if !args.deep {
        return ranked_or_exported(args);
    }
    let mut terminal = Shell::new();
    let finished = argonauts_app::behaviors_deep(args.since, set.to_vec(), &mut terminal).await?;
    terminal.end_progress();
    Ok(if finished {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The formats `--export` writes.
const FORMATS: [&str; 1] = ["hodoscope"];

fn ranked_or_exported(args: &BehaviorsArgs) -> anyhow::Result<ExitCode> {
    let text = match args.export.as_deref() {
        Some([format, dir]) if format == "hodoscope" => {
            argonauts_app::export_hodoscope(Path::new(dir), args.since)?
        }
        Some([format, _]) => {
            return Err(usage(anyhow!(
                "--export writes {}, not {format}",
                FORMATS.join(", ")
            )));
        }
        Some(_) => return Err(usage(anyhow!("--export takes a format and a folder"))),
        None => argonauts_app::behaviors_report(args.since)?,
    };
    println!("{}", sanitize_for_terminal(&text));
    Ok(ExitCode::SUCCESS)
}

/// Lines on stdout, and questions answered on stdin while both are a terminal.
struct Shell {
    at_terminal: bool,
    /// A progress line waits to be written over.
    open_line: bool,
    /// The last tenth of the way a line was written at, without a terminal.
    last_tenth: Option<usize>,
}

impl Shell {
    fn new() -> Shell {
        Shell {
            at_terminal: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            open_line: false,
            last_tenth: None,
        }
    }

    fn end_progress(&mut self) {
        if self.open_line {
            println!();
            self.open_line = false;
        }
    }
}

impl DeepTerminal for Shell {
    fn line(&mut self, text: &str) {
        self.end_progress();
        println!("{text}");
    }

    fn progress(&mut self, text: &str) {
        if self.at_terminal {
            print!("\r\x1b[2K{text}");
            let _ = std::io::stdout().flush();
            self.open_line = true;
            return;
        }
        // Without a terminal, a line at each tenth of the way, so a log stays short.
        let tenth = tenth_of(text);
        if tenth != self.last_tenth {
            self.last_tenth = tenth;
            println!("{text}");
        }
    }

    fn ask(&mut self, question: &[String]) -> Option<bool> {
        self.end_progress();
        if !self.at_terminal {
            return None;
        }
        let (asked, lines) = question.split_last()?;
        for line in lines {
            println!("{line}");
        }
        print!("{asked} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer).ok()?;
        Some(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
    }
}

/// Which tenth of the way `312 of 562 · …` is.
fn tenth_of(progress: &str) -> Option<usize> {
    let mut numbers = progress
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<usize>().ok());
    let done = numbers.next()??;
    let total = numbers.next()??;
    (total > 0).then(|| done * 10 / total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_terminal_progress_shows_each_tenth_once() {
        assert_eq!(tenth_of("312 of 562 · about 50 s left"), Some(5));
        assert_eq!(tenth_of("0 of 562"), Some(0));
        assert_eq!(tenth_of("562 of 562"), Some(10));
        assert_eq!(tenth_of("nothing"), None);
    }
}
