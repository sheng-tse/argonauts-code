//! Per-launch settings as Codex flags. Every key here was checked against codex-cli 0.158.0,
//! which validates `-c` overrides before it reads the prompt.

use argonauts_core::settings::context::ContextAsk;
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

/// `model_context_window`, the window Codex gives the session. Left alone, Codex uses the
/// model's default, 272k for most where its own model list says the model takes 872k, and
/// compacts there. The setting moves where Codex compacts and what it counts against, and
/// nothing else: it isn't what lets a large turn through. Checked live against codex-cli
/// 0.160.0 with `gpt-5.6-luna` on 2026-10-07: one turn of 290k input tokens was answered
/// with the window set to 872000, one of 282k with nothing set, and one of 53k with the
/// window set to 30000.
pub fn context_window(ask: ContextAsk) -> Vec<String> {
    let tokens = match ask {
        ContextAsk::Largest { known } => known,
        ContextAsk::Held { tokens } => Some(tokens),
        ContextAsk::AsItIs => None,
    };
    match tokens {
        Some(tokens) => config_override(&format!("model_context_window={tokens}")),
        // Codex has no word for "the largest", so a model the registry doesn't list is left
        // at its default.
        None => Vec::new(),
    }
}

pub fn native_fanout(setting: NativeFanout) -> LaunchTweak {
    let args = match setting {
        _ if subagents_off(setting) => no_subagents(),
        NativeFanout::Limit { max } => {
            // Set on every task, since OpenAI changes each model's default cap remotely.
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

/// A run's session keeps no memory of its own, so nothing one council's seat noticed reaches
/// the next council, and a worker leaves none behind. `memories` is a feature flag beside
/// `hooks` and `multi_agent` (0.158.0 to 0.163.0), and turning it off stops Codex from both
/// writing and reading memories.
pub fn memories_off() -> Vec<String> {
    vec!["--disable".to_owned(), "memories".to_owned()]
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
    fn the_window_goes_as_a_config_override_when_its_size_is_known() {
        let largest = ContextAsk::Largest {
            known: Some(872_000),
        };
        assert_eq!(
            context_window(largest),
            ["-c", "model_context_window=872000"]
        );
        assert_eq!(
            context_window(ContextAsk::Held { tokens: 200_000 }),
            ["-c", "model_context_window=200000"]
        );
        assert!(context_window(ContextAsk::Largest { known: None }).is_empty());
        assert!(context_window(ContextAsk::AsItIs).is_empty());
    }

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
