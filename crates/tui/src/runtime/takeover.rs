//! Handing a card's agent to you: here (the screen suspends), a tmux split with a `wait-for`
//! sentinel, or a new terminal tab where the terminal supports it, else here with a notice.
//! Every command is an argument vector, never a shell string.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use argonauts_core::adapter::KNOWN_AGENT_IDS;
use argonauts_core::api::TakeOverTicket;
use argonauts_core::session::CommandSpec;
use argonauts_core::settings::ui::TakeoverWhere;
use tokio::process::Command;

pub const ENDPOINT_REFUSAL: &str =
    "A direct endpoint has no UI to hand over. /reassign the task instead.";
const NOT_IN_TMUX: &str = "Not inside tmux, so the session runs here.";
const NO_TABS: &str = "This terminal can't open a tab from here, so the session runs here.";
/// How often a tmux pane is checked for its program's exit.
const PANE_WATCH: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TakeOverPlan {
    /// The screen suspends while `argv` runs in `cwd`. `notice` says why it runs here when you
    /// asked for another place.
    Here {
        argv: Vec<String>,
        cwd: PathBuf,
        notice: Option<String>,
    },
    /// `argv` opens a tmux pane beside this one and prints its id; `tmux wait-for sentinel`
    /// returns once the pane's program exits.
    TmuxSplit { argv: Vec<String>, sentinel: String },
    /// The same in a new tmux window, a tab in tmux's status line.
    NewTab { argv: Vec<String>, sentinel: String },
    /// Nothing to hand over, with the reason to show.
    Refused(String),
}

/// What the terminal offers, from the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TermEnv {
    pub in_tmux: bool,
    /// `$SHELL`, for working in the worktree when the vendor's own UI can't run.
    pub shell: Option<String>,
}

impl TermEnv {
    pub fn from_process() -> Self {
        let set = |key: &str| std::env::var(key).ok().filter(|value| !value.is_empty());
        Self {
            in_tmux: set("TMUX").is_some(),
            shell: set("SHELL"),
        }
    }
}

/// What to run for this ticket, given where you asked for it and what the terminal offers.
pub fn plan_take_over(ticket: &TakeOverTicket, place: TakeoverWhere) -> TakeOverPlan {
    plan_take_over_in(ticket, place, &TermEnv::from_process())
}

pub fn plan_take_over_in(
    ticket: &TakeOverTicket,
    place: TakeoverWhere,
    env: &TermEnv,
) -> TakeOverPlan {
    let Some(child) = child_argv(ticket, env) else {
        return TakeOverPlan::Refused(ENDPOINT_REFUSAL.to_owned());
    };
    let cwd = ticket
        .command
        .as_ref()
        .map(|spec| spec.cwd.clone())
        .filter(|cwd| !cwd.as_os_str().is_empty())
        .unwrap_or_else(|| ticket.worktree.clone());
    let here = |notice: Option<&str>| TakeOverPlan::Here {
        argv: child.clone(),
        cwd: cwd.clone(),
        notice: notice.map(str::to_owned),
    };
    let sentinel = sentinel_for(ticket);
    match place {
        TakeoverWhere::Here => here(None),
        TakeoverWhere::TmuxSplit if env.in_tmux => TakeOverPlan::TmuxSplit {
            argv: tmux_argv(&["split-window", "-h"], &cwd, &child),
            sentinel,
        },
        TakeoverWhere::TmuxSplit => here(Some(NOT_IN_TMUX)),
        TakeoverWhere::NewTab if env.in_tmux => TakeOverPlan::NewTab {
            argv: tmux_argv(&["new-window"], &cwd, &child),
            sentinel,
        },
        TakeoverWhere::NewTab => here(Some(NO_TABS)),
    }
}

/// The vendor's own UI, with its environment changes through `env`. Without one, a vendor
/// with a UI of its own is out of quota, and you get your shell in the worktree; an endpoint
/// has nothing to hand over.
fn child_argv(ticket: &TakeOverTicket, env: &TermEnv) -> Option<Vec<String>> {
    match &ticket.command {
        Some(spec) => Some(command_argv(spec)),
        None if has_own_ui(&ticket.vendor) => Some(vec![
            env.shell.clone().unwrap_or_else(|| "/bin/sh".to_owned()),
        ]),
        None => None,
    }
}

fn has_own_ui(vendor: &str) -> bool {
    KNOWN_AGENT_IDS.contains(&vendor) || vendor == "opencode"
}

fn command_argv(spec: &CommandSpec) -> Vec<String> {
    let mut argv = Vec::new();
    if !spec.env_set.is_empty() || !spec.env_remove.is_empty() {
        argv.push("env".to_owned());
        for name in &spec.env_remove {
            argv.extend(["-u".to_owned(), name.clone()]);
        }
        for (name, value) in &spec.env_set {
            argv.push(format!("{name}={value}"));
        }
    }
    argv.push(spec.program.clone());
    argv.extend(spec.args.iter().cloned());
    argv
}

/// tmux reads an argument ending in `;` as the end of a command, even in a program's own
/// arguments, unless the `;` is escaped.
fn tmux_argv(open: &[&str], cwd: &Path, child: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = ["tmux"]
        .iter()
        .chain(open)
        .map(|s| (*s).to_owned())
        .collect();
    argv.extend(["-P", "-F", "#{pane_id}", "-c"].map(str::to_owned));
    argv.push(cwd.to_string_lossy().into_owned());
    argv.push("--".to_owned());
    argv.extend(child.iter().map(|arg| match arg.strip_suffix(';') {
        Some(rest) if !rest.ends_with('\\') => format!("{rest}\\;"),
        _ => arg.clone(),
    }));
    argv
}

fn sentinel_for(ticket: &TakeOverTicket) -> String {
    let raw = format!("argonauts-takeover-{}-{}", ticket.card, std::process::id());
    raw.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Opens the pane, has tmux signal `sentinel` when its program exits, and waits for that. An
/// error means the pane never opened; once it has, this returns when the pane is gone.
pub async fn run_in_tmux(argv: &[String], sentinel: &str) -> io::Result<()> {
    let opened = tmux_output(argv).await?;
    let pane = String::from_utf8_lossy(&opened).trim().to_owned();
    if !pane.starts_with('%') {
        return Err(io::Error::other(format!("tmux gave no pane id: {pane}")));
    }
    let signal = format!("wait-for -S {sentinel}");
    let mut hooked = true;
    for hook in ["pane-exited", "pane-died"] {
        let set = argv_of(["tmux", "set-hook", "-p", "-t", &pane, hook, &signal]);
        hooked &= tmux_output(&set).await.is_ok();
    }
    // The hooks need tmux 3.2, and a pane closed some other way never signals, so the pane is
    // also watched. A program that exited before the hooks were set is caught the same way.
    let wait = argv_of(["tmux", "wait-for", sentinel]);
    tokio::select! {
        signaled = tmux_output(&wait), if hooked => {
            if signaled.is_err() {
                watch_pane(&pane).await;
            }
        }
        () = watch_pane(&pane) => {}
    }
    Ok(())
}

/// Returns once the pane is gone or its program is dead.
async fn watch_pane(pane: &str) {
    let ask = argv_of(["tmux", "display-message", "-p", "-t", pane, "#{pane_dead}"]);
    loop {
        match tmux_output(&ask).await {
            Ok(dead) if String::from_utf8_lossy(&dead).trim() == "0" => {}
            _ => return,
        }
        tokio::time::sleep(PANE_WATCH).await;
    }
}

fn argv_of<const N: usize>(parts: [&str; N]) -> Vec<String> {
    parts.map(str::to_owned).to_vec()
}

async fn tmux_output(argv: &[String]) -> io::Result<Vec<u8>> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    // A `wait-for` given up on must not stay behind.
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        let said = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(io::Error::other(if said.is_empty() {
            format!("{program} exited with {}", output.status)
        } else {
            said
        }));
    }
    Ok(output.stdout)
}

/// Files the vendor's command expects, written before it starts.
pub fn write_spec_files(spec: &CommandSpec) -> io::Result<()> {
    for file in &spec.files {
        if let Some(parent) = file.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file.path, &file.contents)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::CardId;

    use super::*;

    fn ticket(command: Option<CommandSpec>, vendor: &str) -> TakeOverTicket {
        TakeOverTicket {
            card: CardId::new("task-002").unwrap(),
            command,
            worktree: PathBuf::from("/work/task-002"),
            vendor: vendor.to_owned(),
        }
    }

    fn claude() -> CommandSpec {
        let mut spec = CommandSpec::new("claude", "/work/task-002").args([
            "--resume",
            "abc",
            "fix it; rm -rf ~",
            "$(whoami)",
            "stop;",
        ]);
        spec.env_set = vec![("CLAUDE_CONFIG".into(), "a b".into())];
        spec.env_remove = vec!["ARGONAUTS_SESSION".into()];
        spec
    }

    #[test]
    fn take_over_plan_is_argv_only() {
        let tmux = TermEnv {
            in_tmux: true,
            shell: Some("/bin/zsh".into()),
        };
        let child = [
            "env",
            "-u",
            "ARGONAUTS_SESSION",
            "CLAUDE_CONFIG=a b",
            "claude",
            "--resume",
            "abc",
            "fix it; rm -rf ~",
            "$(whoami)",
            "stop;",
        ]
        .map(String::from)
        .to_vec();
        let with_command = ticket(Some(claude()), "claude-code");

        assert_eq!(
            plan_take_over_in(&with_command, TakeoverWhere::Here, &tmux),
            TakeOverPlan::Here {
                argv: child.clone(),
                cwd: "/work/task-002".into(),
                notice: None,
            }
        );
        let TakeOverPlan::TmuxSplit { argv, sentinel } =
            plan_take_over_in(&with_command, TakeoverWhere::TmuxSplit, &tmux)
        else {
            panic!("expected a tmux split");
        };
        assert_eq!(
            argv[..8],
            [
                "tmux",
                "split-window",
                "-h",
                "-P",
                "-F",
                "#{pane_id}",
                "-c",
                "/work/task-002"
            ]
        );
        assert_eq!(argv[8], "--");
        // Each argument stays one argument; only a trailing `;` is escaped for tmux.
        assert_eq!(argv[9..argv.len() - 1], child[..child.len() - 1]);
        assert_eq!(argv.last().unwrap(), "stop\\;");
        assert!(sentinel.starts_with("argonauts-takeover-task-002-"));
        assert!(
            sentinel
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        );

        let TakeOverPlan::NewTab { argv, .. } =
            plan_take_over_in(&with_command, TakeoverWhere::NewTab, &tmux)
        else {
            panic!("expected a tmux window");
        };
        assert_eq!(argv[1], "new-window");

        // No program anywhere is a shell given a command string.
        for place in [
            TakeoverWhere::Here,
            TakeoverWhere::TmuxSplit,
            TakeoverWhere::NewTab,
        ] {
            let plan = plan_take_over_in(&with_command, place, &tmux);
            let argv = match &plan {
                TakeOverPlan::Here { argv, .. }
                | TakeOverPlan::TmuxSplit { argv, .. }
                | TakeOverPlan::NewTab { argv, .. } => argv,
                TakeOverPlan::Refused(_) => panic!("{plan:?}"),
            };
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg == "-c" && argv[0].ends_with("sh"))
            );
            assert!(!argv.iter().any(|arg| arg == "sh" || arg == "bash"));
        }
    }

    #[test]
    fn places_the_terminal_lacks_fall_back_to_here() {
        let plain = TermEnv::default();
        let with_command = ticket(Some(claude()), "claude-code");
        for (place, notice) in [
            (TakeoverWhere::TmuxSplit, NOT_IN_TMUX),
            (TakeoverWhere::NewTab, NO_TABS),
        ] {
            let TakeOverPlan::Here {
                notice: Some(said), ..
            } = plan_take_over_in(&with_command, place, &plain)
            else {
                panic!("expected here with a notice");
            };
            assert_eq!(said, notice);
        }
    }

    #[test]
    fn an_endpoint_is_refused_and_an_out_of_quota_vendor_gets_a_shell() {
        let env = TermEnv {
            in_tmux: false,
            shell: Some("/bin/zsh".into()),
        };
        assert_eq!(
            plan_take_over_in(&ticket(None, "ollama"), TakeoverWhere::Here, &env),
            TakeOverPlan::Refused(ENDPOINT_REFUSAL.into())
        );
        assert_eq!(
            plan_take_over_in(&ticket(None, "codex"), TakeoverWhere::Here, &env),
            TakeOverPlan::Here {
                argv: vec!["/bin/zsh".into()],
                cwd: "/work/task-002".into(),
                notice: None,
            }
        );
    }

    #[test]
    fn spec_files_are_written_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = claude();
        let path = dir.path().join("nested/mcp.json");
        spec.files = vec![argonauts_core::session::SpecFile {
            path: path.clone(),
            contents: "{}".into(),
        }];
        write_spec_files(&spec).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{}");
    }
}
