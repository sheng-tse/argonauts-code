//! How compaction, the subagent cap, and user hooks apply to agy.

use argonauts_core::settings::sessions::{Compaction, FanoutSwitch, NativeFanout};
use argonauts_core::{LaunchTweak, TweakEffect};

pub fn compaction(setting: Compaction) -> LaunchTweak {
    let note = match setting {
        Compaction::Inherit => return LaunchTweak::default(),
        Compaction::Auto => "agy always compacts on its own, and no flag controls it",
        Compaction::Off => "agy compacts on its own, and nothing turns that off",
    };
    not_possible(note)
}

/// `off` selects a custom agent without the subagent tools, which `build_command` writes to
/// the session's scratch directory. agy documents no count or concurrency cap, so a limit is
/// kept by Argonauts Code counting `subagent_info` children and interrupting past it.
pub fn native_fanout(setting: NativeFanout) -> LaunchTweak {
    let effect = match setting {
        NativeFanout::Limit { max } if max > 0 => TweakEffect::EnforcedByConductor,
        _ => TweakEffect::Exact,
    };
    LaunchTweak {
        effect,
        ..LaunchTweak::default()
    }
}

/// agy's hooks live in your global settings and hooks file, and no flag skips them for a run.
pub fn user_hooks_off() -> LaunchTweak {
    not_possible("agy has no flag that skips your hooks for one run")
}

/// Whether the session must not start subagents at all. A limit of zero means the same.
pub fn fanout_off(setting: Option<NativeFanout>) -> bool {
    matches!(
        setting,
        Some(NativeFanout::Switch(FanoutSwitch::Off) | NativeFanout::Limit { max: 0 })
    )
}

fn not_possible(note: &str) -> LaunchTweak {
    LaunchTweak {
        effect: TweakEffect::NotPossible {
            note: note.to_owned(),
        },
        ..LaunchTweak::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherit_passes_nothing_and_the_rest_is_not_possible() {
        assert_eq!(compaction(Compaction::Inherit), LaunchTweak::default());
        for setting in [Compaction::Auto, Compaction::Off] {
            let tweak = compaction(setting);
            assert!(tweak.args.is_empty() && tweak.env_set.is_empty());
            assert!(matches!(tweak.effect, TweakEffect::NotPossible { .. }));
        }
    }

    #[test]
    fn a_limit_is_counted_by_argonauts_code() {
        let tweak = native_fanout(NativeFanout::Limit { max: 3 });
        assert_eq!(tweak.effect, TweakEffect::EnforcedByConductor);
        assert!(tweak.args.is_empty() && tweak.env_set.is_empty());
        assert_eq!(
            native_fanout(NativeFanout::default()).effect,
            TweakEffect::EnforcedByConductor
        );
    }

    #[test]
    fn on_and_off_are_exact() {
        for setting in [
            NativeFanout::Switch(FanoutSwitch::On),
            NativeFanout::Switch(FanoutSwitch::Off),
            NativeFanout::Limit { max: 0 },
        ] {
            assert_eq!(
                native_fanout(setting).effect,
                TweakEffect::Exact,
                "{setting:?}"
            );
        }
    }

    #[test]
    fn zero_is_off() {
        assert!(fanout_off(Some(NativeFanout::Limit { max: 0 })));
        assert!(fanout_off(Some(NativeFanout::Switch(FanoutSwitch::Off))));
        assert!(!fanout_off(Some(NativeFanout::Limit { max: 1 })));
        assert!(!fanout_off(Some(NativeFanout::Switch(FanoutSwitch::On))));
        assert!(!fanout_off(None));
    }

    #[test]
    fn user_hooks_cannot_be_skipped() {
        assert!(matches!(
            user_hooks_off().effect,
            TweakEffect::NotPossible { .. }
        ));
    }
}
