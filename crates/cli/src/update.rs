//! `argonauts update`: each installed agent CLI updated side by side the way it was installed,
//! with its version before and after, then the doctor, and a last line when Argonauts Code
//! itself is behind. Never while a run is going, so that no run switches versions halfway.

use std::io::IsTerminal as _;
use std::process::ExitCode;

use argonauts_conductor::doctor::update::{UPDATE_TIMEOUT, update_clis, update_lines};
use argonauts_core::sanitize::sanitize_for_terminal;
use tokio_util::sync::CancellationToken;

use crate::args::DoctorArgs;
use crate::doctor;

pub async fn run(set: &[String]) -> anyhow::Result<ExitCode> {
    let going = argonauts_core::Paths::from_env()
        .map(|home| argonauts_app::live_run_hosts(&home).len())
        .unwrap_or(0);
    if going > 0 {
        println!(
            "A run is going, and CLIs are updated between runs so that none switches versions \
             halfway. Run this again once it has ended."
        );
        return Ok(ExitCode::FAILURE);
    }
    // Broken settings shouldn't stop an update; the doctor says what's wrong with them.
    let settings = argonauts_app::current_settings(None, set.to_vec()).unwrap_or_default();
    let adapters = argonauts_app::doctor_adapters(None, set.to_vec())
        .unwrap_or_else(|_| argonauts_app::builtin_adapters());
    let stop = CancellationToken::new();
    let interrupts = tokio::spawn(stop_on_ctrl_c(stop.clone()));
    let updates = update_clis(&adapters, &settings, UPDATE_TIMEOUT, &stop).await;
    interrupts.abort();
    // Past the updaters there's nothing of ours to stop, so Ctrl+C ends this at once.
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            std::process::exit(i32::from(INTERRUPTED));
        }
    });
    let mut out = String::from("Update\n");
    for line in update_lines(&updates) {
        out.push_str(&format!("  {line}\n"));
    }
    println!("{}", sanitize_for_terminal(&out));
    if stop.is_cancelled() {
        println!("Stopped. Each updater it started has ended.");
        return Ok(ExitCode::from(INTERRUPTED));
    }
    let code = doctor::run(&DoctorArgs::default(), set).await?;
    // Like the check at start, only on a terminal: a script reading this gets the CLIs alone.
    if std::io::stdout().is_terminal()
        && let Some(behind) = argonauts_app::self_update::behind_line(set.to_vec()).await
    {
        println!("\n{}", sanitize_for_terminal(&behind));
    }
    Ok(code)
}

/// The exit code of a command stopped by Ctrl+C, as a shell reports one.
const INTERRUPTED: u8 = 130;

/// The updaters run in process groups of their own, which the terminal's Ctrl+C doesn't
/// reach, so Ctrl+C stops them through `stop`.
async fn stop_on_ctrl_c(stop: CancellationToken) {
    if tokio::signal::ctrl_c().await.is_ok() {
        stop.cancel();
    }
}
