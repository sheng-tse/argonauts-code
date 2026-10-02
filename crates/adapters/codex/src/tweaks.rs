//! Per-launch settings as Codex flags. Every key here was checked against codex-cli 0.158.0,
//! which validates `-c` overrides before it reads the prompt.

use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout};
use argonauts_core::{LaunchTweak, TweakEffect};

/// Tokens, more than any model's window. Codex caps the threshold at 90% of the window, so this
/// makes it compact at that cap whatever your `config.toml` says.
pub const LATEST_AUTO_COMPACT_TOKENS: u64 = 10_000_000;

pub fn compaction(setting: Compaction) -> LaunchTweak {
    let latest = config_override(&format!(
        "model_auto_compact_token_limit={LATEST_AUTO_COMPACT_TOKENS}"
    ));
    match setting {
        Compaction::Inherit => LaunchTweak::default(),
        Compaction::Auto => LaunchTweak {
            args: latest,
            ..LaunchTweak::default()
        },
        // Nothing turns Codex's auto-compaction off, so the closest is compacting as late as it
        // allows.
        Compaction::Off => LaunchTweak {
            args: latest,
            effect: TweakEffect::Approximated {
                note: "Codex can't turn compaction off; it compacts by 90% of the window at the \
                       latest"
                    .to_owned(),
            },
            ..LaunchTweak::default()
        },
    }
}

pub fn native_fanout(setting: NativeFanout) -> LaunchTweak {
    let args = match setting {
        _ if subagents_off(setting) => no_subagents(),
        NativeFanout::Limit { max } => {
            // Set on every card, since OpenAI changes each model's default cap remotely.
            config_override(&format!("agents.max_concurrent_threads_per_session={max}"))
        }
        NativeFanout::Switch(_) => Vec::new(),
    };
    LaunchTweak {
        args,
        ..LaunchTweak::default()
    }
}

pub fn subagents_off(setting: NativeFanout) -> bool {
    matches!(
        setting,
        NativeFanout::Switch(FanoutSwitch::Off) | NativeFanout::Limit { max: 0 }
    )
}

/// `hooks` is a feature flag, so this turns off every hook, the repository's too.
pub fn user_hooks_off() -> LaunchTweak {
    LaunchTweak {
        args: vec!["--disable".to_owned(), "hooks".to_owned()],
        effect: TweakEffect::Approximated {
            note: "Codex turns off every hook, the repository's too".to_owned(),
        },
        ..LaunchTweak::default()
    }
}

/// What a read-only session that must not fan out, such as a judge, gets on top of
/// [`no_subagents`]: the documented `[agents]` switch.
pub fn read_only_no_subagents() -> Vec<String> {
    config_override("agents.enabled=false")
}

fn no_subagents() -> Vec<String> {
    vec!["--disable".to_owned(), "multi_agent".to_owned()]
}

fn config_override(key_value: &str) -> Vec<String> {
    vec!["-c".to_owned(), key_value.to_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherit_passes_nothing() {
        assert_eq!(compaction(Compaction::Inherit), LaunchTweak::default());
    }

    #[test]
    fn fanout_on_passes_nothing_and_zero_means_off() {
        let on = native_fanout(NativeFanout::Switch(FanoutSwitch::On));
        assert!(on.args.is_empty());
        assert_eq!(on.effect, TweakEffect::Exact);
        assert_eq!(
            native_fanout(NativeFanout::Limit { max: 0 }).args,
            ["--disable", "multi_agent"]
        );
    }
}
