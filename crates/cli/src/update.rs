//! `argonauts update`: each installed agent CLI's own updater in turn, with its version before
//! and after, then the doctor.

use std::process::ExitCode;

use argonauts_conductor::doctor::update::{UPDATE_TIMEOUT, update_clis, update_lines};
use argonauts_core::sanitize::sanitize_for_terminal;

use crate::args::DoctorArgs;
use crate::doctor;

pub async fn run(set: &[String]) -> anyhow::Result<ExitCode> {
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
