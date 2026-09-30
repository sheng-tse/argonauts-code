mod agents;
mod args;
mod config;
mod doctor;
mod run;
mod signals;
mod usage;

use std::process::ExitCode;

use argonauts_app::InteractiveOptions;
use clap::Parser;

use crate::args::{Cli, Command};

/// Turns on logging to stderr; the value is a tracing filter.
const LOG_ENV: &str = "ARGONAUTS_LOG";

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("argonauts: could not start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(dispatch(cli));
    // The plain renderer can be blocked reading stdin; there is nothing left to wait for.
    runtime.shutdown_background();
    code
}

async fn dispatch(cli: Cli) -> ExitCode {
    let result = match cli.command {
        None => argonauts_app::run_interactive(InteractiveOptions::default())
            .await
            .map(|()| ExitCode::SUCCESS),
        Some(Command::Init) => argonauts_app::run_interactive(InteractiveOptions {
            guided_setup: true,
            ..InteractiveOptions::default()
        })
        .await
        .map(|()| ExitCode::SUCCESS),
        Some(Command::Run(args)) => run::run(args).await,
        Some(Command::Doctor(args)) => doctor::run(args).await,
        Some(Command::Config(command)) => config::run(command).await,
    };
    result.unwrap_or_else(|err| {
        eprintln!("argonauts: {err:#}");
        if err.is::<usage::UsageError>() {
            ExitCode::from(usage::USAGE_EXIT)
        } else {
            ExitCode::FAILURE
        }
    })
}

/// Logs to stderr only when asked, so nothing draws over the full screen by default.
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
