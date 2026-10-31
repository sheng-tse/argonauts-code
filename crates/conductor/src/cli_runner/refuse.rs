//! Refuse, don't guess. A CLI session needs a working sandbox whatever its access (`None`
//! only when you set its role's `sandbox.mode` to `tool` or `none`); a build or experiment session
//! also needs the approved goal's hash; and an agent disabled for new work starts nothing. A
//! billing cap's disable only steers fallbacks away, so the retry after you top up still starts.

use argonauts_core::agents::session_runner::{RunnerError, SandboxMode, SessionSpec};
use argonauts_core::models::limits::AgentHealth;
use argonauts_core::settings::approvals::SandboxSettings;

use crate::boards::health::{DisableScope, disable_scope};
use crate::sandbox::SelfTest;

/// `spec.sandbox.mode` is the sandbox the session will really run in, so a CLI the runner
/// can't wrap reads as `None` here. `sandbox` is the session's role's.
pub fn check(
    spec: &SessionSpec,
    sandbox: &SandboxSettings,
    health: &AgentHealth,
    self_test: &SelfTest,
) -> Result<(), RunnerError> {
    match spec.sandbox.mode {
        SandboxMode::None if !sandbox.mode.ours_off() => {
            return Err(RunnerError::Refused(
                "a CLI session needs a sandbox. Only sandbox.mode = \"tool\" or \"none\", \
                 which you set yourself, lets one run without ours"
                    .to_owned(),
            ));
        }
        SandboxMode::None => {}
        SandboxMode::Os | SandboxMode::Container => {
            if sandbox.fail_closed && !self_test.ok {
                return Err(RunnerError::Refused(format!(
                    "the sandbox self-test failed, so no CLI session starts: {}",
                    self_test.detail
                )));
            }
        }
    }
    let changes_files = spec.access.writes();
    let has_goal = spec
        .goal_sha256
        .as_deref()
        .is_some_and(|hash| !hash.trim().is_empty());
    if changes_files && !has_goal {
        return Err(RunnerError::Refused(
            "a session that changes files needs the approved goal's hash, or for a direct turn \
             your message's"
                .to_owned(),
        ));
    }
    if let Some(reason) = &health.disabled
        && disable_scope(reason) == DisableScope::Every
    {
        return Err(RunnerError::Disabled {
            model: spec.model.clone(),
            reason: reason.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_runner::testing::{healthy, spec_in};
    use argonauts_core::agents::session_runner::Access;
    use argonauts_core::base::names::AgentId;
    use argonauts_core::settings::approvals::SandboxChoice;

    fn passed() -> SelfTest {
        SelfTest {
            ok: true,
            detail: "passed".into(),
        }
    }

    fn failed() -> SelfTest {
        SelfTest {
            ok: false,
            detail: "a write outside the worktree worked".into(),
        }
    }

    fn settings(mode: SandboxChoice) -> SandboxSettings {
        SandboxSettings {
            mode,
            ..SandboxSettings::default()
        }
    }

    fn refused(result: Result<(), RunnerError>) -> String {
        match result {
            Err(RunnerError::Refused(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn cli_without_sandbox_is_refused() {
        let mut spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
        spec.sandbox.mode = SandboxMode::None;
        let health = healthy(&spec.model.agent_id());
        let why = refused(check(
            &spec,
            &settings(SandboxChoice::Os),
            &health,
            &passed(),
        ));
        assert!(why.contains("sandbox"), "{why}");
        // Only your own `sandbox.mode = "tool"` or `"none"` lets it through.
        for mode in [SandboxChoice::Tool, SandboxChoice::None] {
            check(&spec, &settings(mode), &health, &passed()).unwrap();
        }
    }

    #[test]
    fn failed_self_test_refuses_when_fail_closed() {
        let spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
        let health = healthy(&spec.model.agent_id());
        let mut settings = settings(SandboxChoice::Os);
        let why = refused(check(&spec, &settings, &health, &failed()));
        assert!(why.contains("a write outside the worktree worked"), "{why}");
        settings.fail_closed = false;
        check(&spec, &settings, &health, &failed()).unwrap();
    }

    #[test]
    fn a_session_that_writes_without_goal_hash_is_refused() {
        let health = healthy(&AgentId("fake".into()));
        let settings = settings(SandboxChoice::Os);
        for access in [Access::Build, Access::Experiment, Access::Direct] {
            let mut spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
            spec.access = access;
            spec.goal_sha256 = None;
            refused(check(&spec, &settings, &health, &passed()));
            spec.goal_sha256 = Some("  ".into());
            refused(check(&spec, &settings, &health, &passed()));
            spec.goal_sha256 = Some("ab12".into());
            check(&spec, &settings, &health, &passed()).unwrap();
        }
        // Reading needs no goal.
        let spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
        check(&spec, &settings, &health, &passed()).unwrap();
    }

    #[test]
    fn disabled_agent_is_refused() {
        let spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
        let mut health = healthy(&spec.model.agent_id());
        health.disabled = Some("untested version 9.9.9; pin 1.0.0".into());
        match check(&spec, &settings(SandboxChoice::Os), &health, &passed()) {
            Err(RunnerError::Disabled {
                model: worker,
                reason,
            }) => {
                assert_eq!(worker, spec.model);
                assert!(reason.contains("pin 1.0.0"));
            }
            other => panic!("expected Disabled, got {other:?}"),
        }
        // A billing cap leaves the retry on the same agent free to start.
        health.disabled = Some(crate::boards::health::billing_disable());
        check(&spec, &settings(SandboxChoice::Os), &health, &passed()).unwrap();
    }
}
