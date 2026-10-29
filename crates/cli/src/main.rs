mod approvals;
mod approve;
mod args;
mod behaviors;
mod config;
mod doctor;
mod help;
mod learning;
mod mcp;
mod runs;
mod self_update;
mod text;
mod update;
mod usage;

use std::io::Write as _;
use std::process::ExitCode;

use argonauts_app::{FirstView, InteractiveOptions, TelemetryAction};
use argonauts_core::RunId;
use argonauts_core::agent_sessions::AgentSessionRef;
use argonauts_core::cli_sessions::CliSessionRef;
use argonauts_core::names::ConversationId;
use argonauts_core::sanitize::sanitize_for_terminal;
use clap::FromArgMatches as _;

use crate::args::{AutoCommand, Cli, Command, McpCommand, TelemetryCommand};
use crate::usage::usage;

/// Turns on logging to stderr; the value is a tracing filter.
const LOG_ENV: &str = "ARGONAUTS_LOG";

fn main() -> ExitCode {
    // Before anything else: the sandbox launcher needs a process with a single thread.
    if let Some(code) = argonauts_conductor::sandbox::launcher::launcher_main() {
        return ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    // Found before an update can swap the link this program was started by, so every helper
    // it starts is this version, which no update removes while it runs.
    let _ = argonauts_core::paths::this_program();
    argonauts_app::self_update::hold_this_version();
    let cli = Cli::from_arg_matches(&help::command().get_matches())
        .unwrap_or_else(|err| err.format(&mut help::command()).exit());
    init_logging();
    argonauts_app::count_crashes();
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
    if cli.continue_conversation && cli.command.is_some() {
        eprintln!(
            "argonauts: -c opens the app on your newest conversation, so it takes no command"
        );
        return ExitCode::from(usage::USAGE_EXIT);
    }
    let result = match cli.command {
        None if cli.continue_conversation => {
            argonauts_app::run_interactive_with(app(false), FirstView::Continue)
                .await
                .map(|()| ExitCode::SUCCESS)
        }
        None => argonauts_app::run_interactive(app(false))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Setup) => argonauts_app::run_interactive(app(true))
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Run(ref args)) => runs::run(args, &cli.set, cli.screen_reader).await,
        Some(Command::Resume { ref id }) => resume(app(false), id.as_deref()).await,
        Some(Command::Status { json }) => runs::status(json).await,
        Some(Command::Doctor(ref args)) => doctor::run(args, &cli.set).await,
        Some(Command::Update) => update::run(&cli.set).await,
        Some(Command::SelfUpdate(ref args)) => Ok(self_update::run(args, &cli.set).await),
        Some(Command::Welcome) => Ok(argonauts_app::welcome()),
        Some(Command::Config(ref command)) => config::run(command, &cli.set),
        Some(Command::Telemetry(command)) => telemetry(command, &cli.set).await,
        Some(Command::Models { discover }) => config::models(discover, &cli.set).await,
        Some(Command::Approve {
            ref run,
            ref approval,
        }) => approve::run(run, approval, app(false)).await,
        Some(Command::Approvals) => approvals::show().await,
        Some(Command::Auto(AutoCommand::Off)) => approvals::auto_off(),
        Some(Command::Host { ref repo, ref run }) => host(repo, run).await,
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
        Some(Command::LearnedWorkflows { ref action, json }) => {
            learning::run(learning::learned_workflows(action.clone()), json, &cli.set).await
        }
        Some(Command::Skills { ref action, json }) => {
            learning::run(learning::skills(action.clone()), json, &cli.set).await
        }
        Some(Command::Sync { check, force, json }) => {
            let command = argonauts_app::LearningShell::Sync { check, force };
            learning::run(command, json, &cli.set).await
        }
        Some(Command::Behaviors(ref args)) => behaviors::run(args, &cli.set).await,
        Some(Command::RunReview { ref run, json }) => {
            let command = argonauts_app::LearningShell::RunReview { run: run.clone() };
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

/// `argonauts telemetry show|on|off`.
async fn telemetry(command: TelemetryCommand, set: &[String]) -> anyhow::Result<ExitCode> {
    let action = match command {
        TelemetryCommand::Show => TelemetryAction::Show,
        TelemetryCommand::On => TelemetryAction::On,
        TelemetryCommand::Off => TelemetryAction::Off,
    };
    let said = argonauts_app::telemetry(action, set.to_vec()).await?;
    println!("{}", sanitize_for_terminal(&said));
    Ok(ExitCode::SUCCESS)
}

/// `argonauts resume`: a conversation, a run, a run's agent's session, or any CLI's own session
/// by id, or the list to pick from.
async fn resume(options: InteractiveOptions, id: Option<&str>) -> anyhow::Result<ExitCode> {
    argonauts_app::refuse_inside_agent()?;
    let first = match (resume_view(id)?, id) {
        (Some(first), _) => first,
        (None, Some(id)) => {
            let found = argonauts_app::find_session(&options, id)?;
            let mut terminal = Terminal::stdio();
            session_view(id, found, &mut terminal)?
        }
        (None, None) => FirstView::ResumePicker,
    };
    argonauts_app::run_interactive_with(options, first).await?;
    Ok(ExitCode::SUCCESS)
}

/// `conversation-002`, `run-03`, `run-03/s-002`, or `claude-code:0a1b2c3d`. `None` for a
/// session's id alone, which is looked for in every CLI.
fn resume_view(id: Option<&str>) -> anyhow::Result<Option<FirstView>> {
    Ok(Some(match id {
        Some(id) if id.contains('/') => {
            FirstView::Agent(AgentSessionRef::parse(id).ok_or_else(|| {
                usage(anyhow::anyhow!(
                    "{id:?} isn't an agent's session such as run-03/s-002"
                ))
            })?)
        }
        Some(id) if id.starts_with("conversation-") => FirstView::PastConversation(
            ConversationId::parse(id)
                .ok_or_else(|| usage(anyhow::anyhow!("{id:?} isn't a conversation id")))?,
        ),
        Some(id) if id.contains(':') => {
            FirstView::CliSession(CliSessionRef::parse(id).ok_or_else(|| {
                usage(anyhow::anyhow!(
                    "{id:?} isn't a session such as codex:019a2f"
                ))
            })?)
        }
        Some(run) if run.starts_with("run-") => FirstView::Resume(RunId::new(run).map_err(usage)?),
        Some(_) => return Ok(None),
        None => FirstView::ResumePicker,
    }))
}

/// Where the session `id` carries on. A CLI that keeps no list of its sessions is asked about
/// by name, since nothing else can tell its ids apart.
fn session_view(
    id: &str,
    found: argonauts_app::FoundSession,
    terminal: &mut Terminal<'_>,
) -> anyhow::Result<FirstView> {
    use argonauts_app::FoundSession;

    let unlisted = match found {
        FoundSession::Conversation(conversation) => {
            return Ok(FirstView::PastConversation(conversation));
        }
        FoundSession::CliSession(session) => return Ok(FirstView::CliSession(session)),
        FoundSession::Unlisted(unlisted) => unlisted,
    };
    let nowhere = format!("No conversation or CLI here has a session {id}.");
    if unlisted.is_empty() {
        anyhow::bail!("{nowhere} argonauts resume lists them.");
    }
    if !terminal.asks {
        let (name, session) = &unlisted[0];
        anyhow::bail!("{nowhere} If it's {name}'s, run argonauts resume {session}.");
    }
    let names: Vec<&str> = unlisted.iter().map(|(name, _)| name.as_str()).collect();
    let question = match names.as_slice() {
        [name] => format!("{nowhere} {name} keeps no list of its own. Is it {name}'s? [y/N] "),
        _ => format!("{nowhere} Which CLI is it from: {}? ", names.join(", ")),
    };
    let answer = terminal.ask(&question)?;
    let answer = answer.trim();
    let picked = match unlisted.as_slice() {
        [(_, session)]
            if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") =>
        {
            Some(session)
        }
        [_] => None,
        many => many
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(answer))
            .map(|(_, session)| session),
    };
    match picked {
        Some(session) => Ok(FirstView::CliSession(session.clone())),
        None => anyhow::bail!("Nothing opened."),
    }
}

/// Where `argonauts resume` asks, while both ends are a terminal.
struct Terminal<'a> {
    asks: bool,
    out: Box<dyn std::io::Write + 'a>,
    read: Box<dyn FnMut() -> std::io::Result<String> + 'a>,
}

impl Terminal<'_> {
    fn stdio() -> Self {
        use std::io::{BufRead as _, IsTerminal as _};

        Self {
            asks: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            out: Box::new(std::io::stdout()),
            read: Box::new(|| {
                let mut answer = String::new();
                std::io::stdin().lock().read_line(&mut answer)?;
                Ok(answer)
            }),
        }
    }

    fn ask(&mut self, question: &str) -> anyhow::Result<String> {
        write!(self.out, "{}", sanitize_for_terminal(question))?;
        self.out.flush()?;
        Ok((self.read)()?)
    }
}

/// The run host checks its own ancestry: only Argonauts Code's own agents may not host a run.
async fn host(repo: &std::path::Path, run: &str) -> anyhow::Result<ExitCode> {
    let run = RunId::new(run).map_err(usage)?;
    argonauts_app::host(repo, &run).await?;
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
    use argonauts_core::names::AgentId;

    use super::*;

    #[test]
    fn resume_takes_a_conversation_a_run_or_a_cli_session() {
        let view = |id| resume_view(id).unwrap().unwrap();
        assert_eq!(
            view(Some("conversation-002")),
            FirstView::PastConversation(ConversationId::numbered(2))
        );
        assert_eq!(
            view(Some("run-03")),
            FirstView::Resume(RunId::new("run-03").unwrap())
        );
        assert_eq!(
            view(Some("codex:019a2f")),
            FirstView::CliSession(CliSessionRef {
                agent_id: AgentId("codex".into()),
                id: "019a2f".into(),
            })
        );
        assert_eq!(
            view(Some("run-03/s-002")),
            FirstView::Agent(AgentSessionRef::parse("run-03/s-002").unwrap())
        );
        assert_eq!(view(None), FirstView::ResumePicker);
        // A session's id alone is looked for in every CLI.
        assert_eq!(resume_view(Some("4a5b6c7d")).unwrap(), None);
        let bad = resume_view(Some("run-03/-p")).unwrap_err();
        assert!(
            bad.to_string().contains("isn't an agent's session"),
            "{bad}"
        );
        let bad = resume_view(Some("codex:-p")).unwrap_err();
        assert!(bad.to_string().contains("isn't a session"), "{bad}");
    }

    fn agy(id: &str) -> CliSessionRef {
        CliSessionRef {
            agent_id: AgentId("antigravity-cli".into()),
            id: id.into(),
        }
    }

    /// A terminal that answers `answer` and keeps what was asked.
    fn answering<'a>(answer: &'a str, asked: &'a mut Vec<u8>) -> Terminal<'a> {
        Terminal {
            asks: true,
            out: Box::new(asked),
            read: Box::new(move || Ok(format!("{answer}\n"))),
        }
    }

    #[test]
    fn a_session_id_alone_opens_where_it_was_found_and_asks_only_about_agy() {
        use argonauts_app::FoundSession;

        let mut asked = Vec::new();
        let found = FoundSession::Conversation(ConversationId::numbered(4));
        assert_eq!(
            session_view("4a5b6c7d", found, &mut answering("", &mut asked)).unwrap(),
            FirstView::PastConversation(ConversationId::numbered(4))
        );
        let codex = CliSessionRef {
            agent_id: AgentId("codex".into()),
            id: "019a5770".into(),
        };
        let found = FoundSession::CliSession(codex.clone());
        assert_eq!(
            session_view("019a5770", found, &mut answering("", &mut asked)).unwrap(),
            FirstView::CliSession(codex)
        );
        assert!(asked.is_empty(), "nothing is asked of an id a list has");

        let unlisted = || FoundSession::Unlisted(vec![("agy".to_owned(), agy("055a398f"))]);
        assert_eq!(
            session_view("055a398f", unlisted(), &mut answering("y", &mut asked)).unwrap(),
            FirstView::CliSession(agy("055a398f"))
        );
        assert_eq!(
            String::from_utf8(asked.clone()).unwrap(),
            "No conversation or CLI here has a session 055a398f. agy keeps no list of its own. \
             Is it agy's? [y/N] "
        );
        let no = session_view("055a398f", unlisted(), &mut answering("n", &mut asked));
        assert_eq!(no.unwrap_err().to_string(), "Nothing opened.");

        // Away from a terminal it says how to name agy instead.
        let mut away = answering("y", &mut asked);
        away.asks = false;
        let said = session_view("055a398f", unlisted(), &mut away).unwrap_err();
        assert_eq!(
            said.to_string(),
            "No conversation or CLI here has a session 055a398f. If it's agy's, run argonauts \
             resume antigravity-cli:055a398f."
        );
        let nowhere = session_view("055a398f", FoundSession::Unlisted(Vec::new()), &mut away);
        assert_eq!(
            nowhere.unwrap_err().to_string(),
            "No conversation or CLI here has a session 055a398f. argonauts resume lists them."
        );
    }
}
