mod approvals;
mod approve;
mod args;
mod config;
mod doctor;
mod help;
mod learning;
mod mcp;
mod runs;
mod text;
mod update;
mod usage;

use std::io::Write as _;
use std::process::ExitCode;

use argonauts_app::{FirstView, InteractiveOptions};
use argonauts_core::RunId;
use argonauts_core::agent_sessions::AgentSessionRef;
use argonauts_core::names::ChatId;
use argonauts_core::sanitize::sanitize_for_terminal;
use argonauts_core::vendor_sessions::VendorSessionRef;
use clap::FromArgMatches as _;

use crate::args::{AutoCommand, Cli, Command, McpCommand};
use crate::usage::usage;

/// Turns on logging to stderr; the value is a tracing filter.
const LOG_ENV: &str = "ARGONAUTS_LOG";

fn main() -> ExitCode {
    // Before anything else: the sandbox launcher needs a process with a single thread.
    if let Some(code) = argonauts_conductor::sandbox::launcher::launcher_main() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    let cli = Cli::from_arg_matches(&help::command().get_matches())
        .unwrap_or_else(|err| err.format(&mut help::command()).exit());
    init_logging();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("argonauts: couldn't start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(dispatch(cli));
    // A screen can be blocked reading stdin; there is nothing left to wait for.
    runtime.shutdown_background();
    code
}

async fn dispatch(cli: Cli) -> ExitCode {
    let app = |guided_setup: bool| InteractiveOptions {
        repo: None,
        overrides: cli.set.clone(),
        screen_reader: cli.screen_reader,
        guided_setup,
    };
    if cli.continue_chat && cli.command.is_some() {
        eprintln!(
            "argonauts: -c opens the app on your newest conversation, so it takes no command"
        );
        return ExitCode::from(usage::USAGE_EXIT);
    }
    let result = match cli.command {
        None if cli.continue_chat => {
            argonauts_app::run_interactive_with(app(false), FirstView::Continue)
                .await
                .map(|()| ExitCode::SUCCESS)
        }
        None => argonauts_app::run_interactive(app(false))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Init) => argonauts_app::run_interactive(app(true))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Run(ref args)) => runs::run(args, &cli.set, cli.screen_reader).await,
        Some(Command::Resume { ref id }) => resume(app(false), id.as_deref()).await,
        Some(Command::Status { json }) => runs::status(json).await,
        Some(Command::Inbox) => runs::inbox().await,
        Some(Command::Doctor(ref args)) => doctor::run(args, &cli.set).await,
        Some(Command::Update) => update::run(&cli.set).await,
        Some(Command::Config(ref command)) => config::run(command, &cli.set),
        Some(Command::Models { discover }) => config::models(discover).await,
        Some(Command::Approve {
            ref run,
            ref checkpoint,
        }) => approve::run(run, checkpoint, app(false)).await,
        Some(Command::Approvals) => approvals::show().await,
        Some(Command::Auto(AutoCommand::Off)) => approvals::auto_off(),
        Some(Command::Supervise { ref repo, ref run }) => supervise(repo, run).await,
        Some(Command::Mcp(McpCommand::Serve { ref repo })) => {
            mcp::serve(repo.as_deref(), &cli.set).await
        }
        Some(Command::Mcp(McpCommand::Status { ref file })) => argonauts_mcp::serve_status(file)
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Mcp(McpCommand::Session {
            ref status_file,
            ref report_schema,
            ref report_file,
            ref ask,
            approve_tool,
            ref tools,
            ref ask_roles,
        })) => mcp::session(
            status_file.as_deref(),
            report_schema.as_deref().zip(report_file.as_deref()),
            ask.as_deref().map(|endpoint| mcp::Asking {
                endpoint,
                approve: approve_tool,
                tools,
                roles: ask_roles,
            }),
        )
        .await
        .map(|()| ExitCode::SUCCESS),
        Some(Command::Mcp(McpCommand::Package { ref dir, ref repo })) => {
            learning::run(
                learning::package(dir.clone(), repo.clone()),
                false,
                &cli.set,
            )
            .await
        }
        Some(Command::Records {
            action,
            ref as_of,
            json,
        }) => learning::run(learning::records(action, as_of.clone()), json, &cli.set).await,
        Some(Command::Recipes { ref action, json }) => {
            learning::run(learning::recipes(action.clone()), json, &cli.set).await
        }
        Some(Command::Skills { ref action, json }) => {
            learning::run(learning::skills(action.clone()), json, &cli.set).await
        }
        Some(Command::Sync { check, force, json }) => {
            let command = argonauts_app::LearningShell::Sync { check, force };
            learning::run(command, json, &cli.set).await
        }
        Some(Command::Retro { ref run, json }) => {
            let command = argonauts_app::LearningShell::Retro { run: run.clone() };
            learning::run(command, json, &cli.set).await
        }
    };
    result.unwrap_or_else(|err| {
        // An error can quote a file you haven't trusted, before any screen cleans it up. When
        // the terminal went away there's nowhere to write it, and that's no reason to panic.
        let line = sanitize_for_terminal(&format!("{err:#}"));
        let _ = writeln!(std::io::stderr(), "argonauts: {line}");
        if err.is::<usage::UsageError>() {
            ExitCode::from(usage::USAGE_EXIT)
        } else {
            ExitCode::FAILURE
        }
    })
}

/// `argonauts resume`: a conversation, a run, a run's agent's session, or the main agent CLI's
/// own session by id, or the list to pick from.
async fn resume(options: InteractiveOptions, id: Option<&str>) -> anyhow::Result<ExitCode> {
    argonauts_app::refuse_inside_agent()?;
    let first = resume_view(id)?;
    argonauts_app::run_interactive_with(options, first).await?;
    Ok(ExitCode::SUCCESS)
}

/// `chat-002`, `run-03`, `run-03/s-002`, or `claude-code:0a1b2c3d`.
fn resume_view(id: Option<&str>) -> anyhow::Result<FirstView> {
    Ok(match id {
        Some(id) if id.contains('/') => {
            FirstView::Agent(AgentSessionRef::parse(id).ok_or_else(|| {
                usage(anyhow::anyhow!(
                    "{id:?} isn't an agent's session such as run-03/s-002"
                ))
            })?)
        }
        Some(id) if id.starts_with("chat-") => FirstView::Conversation(
            ChatId::parse(id)
                .ok_or_else(|| usage(anyhow::anyhow!("{id:?} isn't a conversation id")))?,
        ),
        Some(id) if id.contains(':') => {
            FirstView::VendorSession(VendorSessionRef::parse(id).ok_or_else(|| {
                usage(anyhow::anyhow!(
                    "{id:?} isn't a session such as codex:019a2f"
                ))
            })?)
        }
        Some(run) => FirstView::Resume(RunId::new(run).map_err(usage)?),
        None => FirstView::ResumePicker,
    })
}

/// The supervisor checks its own ancestry: only Argonauts' own agents may not host a run.
async fn supervise(repo: &std::path::Path, run: &str) -> anyhow::Result<ExitCode> {
    let run = RunId::new(run).map_err(usage)?;
    argonauts_app::supervise(repo, &run).await?;
    Ok(ExitCode::SUCCESS)
}

/// Logs to stderr only when asked, so nothing draws over the screen by default.
fn init_logging() {
    let Some(filter) = std::env::var_os(LOG_ENV) else {
        return;
    };
    let filter = tracing_subscriber::EnvFilter::new(filter.to_string_lossy());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::VendorId;

    use super::*;

    #[test]
    fn resume_takes_a_conversation_a_run_or_a_cli_session() {
        assert_eq!(
            resume_view(Some("chat-002")).unwrap(),
            FirstView::Conversation(ChatId::numbered(2))
        );
        assert_eq!(
            resume_view(Some("run-03")).unwrap(),
            FirstView::Resume(RunId::new("run-03").unwrap())
        );
        assert_eq!(
            resume_view(Some("codex:019a2f")).unwrap(),
            FirstView::VendorSession(VendorSessionRef {
                vendor: VendorId("codex".into()),
                id: "019a2f".into(),
            })
        );
        assert_eq!(
            resume_view(Some("run-03/s-002")).unwrap(),
            FirstView::Agent(AgentSessionRef::parse("run-03/s-002").unwrap())
        );
        assert_eq!(resume_view(None).unwrap(), FirstView::ResumePicker);
        let bad = resume_view(Some("run-03/-p")).unwrap_err();
        assert!(
            bad.to_string().contains("isn't an agent's session"),
            "{bad}"
        );
        let bad = resume_view(Some("codex:-p")).unwrap_err();
        assert!(bad.to_string().contains("isn't a session"), "{bad}");
    }
}
