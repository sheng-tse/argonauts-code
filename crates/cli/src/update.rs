//! `argonauts update`: each installed agent CLI updated in turn the way it was installed, with
//! its version before and after, then the doctor. Never while a run is going, so that no run
//! switches versions halfway.

use std::process::ExitCode;

use argonauts_conductor::doctor::update::{UPDATE_TIMEOUT, update_clis, update_lines};
use argonauts_core::sanitize::sanitize_for_terminal;

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
    let updates = update_clis(&adapters, &settings, UPDATE_TIMEOUT).await;
    let mut out = String::from("Update\n");
    for line in update_lines(&updates) {
        out.push_str(&format!("  {line}\n"));
    }
    println!("{}", sanitize_for_terminal(&out));
    doctor::run(&DoctorArgs::default(), set).await
}
