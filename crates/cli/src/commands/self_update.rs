//! `argonauts self-update`: Argonauts Code updates itself, follows another channel, installs a
//! version you name, or goes back to the one before.

use std::process::ExitCode;

use argonauts_app::self_update::Asked;
use argonauts_core::settings::updates::UpdateChannel;

use crate::args::{SelfUpdateArgs, UpdateChannelArg};

/// `set` holds the `--set` overrides, which may name the channel to follow.
pub async fn run(args: &SelfUpdateArgs, set: &[String]) -> ExitCode {
    let asked = Asked {
        check: args.check,
        channel: args.channel.map(|channel| match channel {
            UpdateChannelArg::Stable => UpdateChannel::Stable,
            UpdateChannelArg::Beta => UpdateChannel::Beta,
        }),
        version: args.version.clone(),
        rollback: args.rollback,
    };
    let result = argonauts_app::self_update::self_update(asked, set.to_vec()).await;
    result.unwrap_or_else(|err| {
        eprintln!(
            "argonauts: {}",
            argonauts_core::safety::sanitize::sanitize_for_terminal(&format!("{err:#}"))
        );
        ExitCode::FAILURE
    })
}
