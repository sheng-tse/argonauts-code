//! Refuse, don't guess. A harness session needs a working sandbox whatever its access (`None`
//! only when you set its role's `sandbox.mode` to `tool` or `none`); a build or spike session also needs
//! the approved goal's hash; and a vendor disabled for new work starts nothing. A billing cap's
//! disable only steers handoffs away, so the retry after you top up still starts.

use argonauts_core::limits::VendorHealth;
use argonauts_core::settings::approvals::SandboxSettings;
use argonauts_core::worker::{SandboxMode, SessionSpec, WorkerError};

use crate::health::{DisableScope, disable_scope};
use crate::sandbox::SelfTest;

/// `spec.sandbox.mode` is the sandbox the session will really run in, so a harness the worker
/// can't wrap reads as `None` here. `sandbox` is the session's role's.
pub fn check(
    spec: &SessionSpec,
    sandbox: &SandboxSettings,
    health: &VendorHealth,
    self_test: &SelfTest,
) -> Result<(), WorkerError> {
    match spec.sandbox.mode {
        SandboxMode::None if !sandbox.mode.ours_off() => {
            return Err(WorkerError::Refused(
                "a harness session needs a sandbox. Only sandbox.mode = \"tool\" or \"none\", \
                 which you set yourself, lets one run without ours"
                    .to_owned(),
            ));
        }
        SandboxMode::None => {}
        SandboxMode::Os | SandboxMode::Container => {
            if sandbox.fail_closed && !self_test.ok {
                return Err(WorkerError::Refused(format!(
                    "the sandbox self-test failed, so no harness session starts: {}",
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
        return Err(WorkerError::Refused(
            "a session that changes files needs the approved goal's hash, or for a direct turn \
             your message's"
                .to_owned(),
        ));
    }
    if let Some(reason) = &health.disabled
        && disable_scope(reason) == DisableScope::Every
    {
        return Err(WorkerError::Disabled {
            worker: spec.worker.clone(),
            reason: reason.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness_worker::testing::{healthy, spec_in};
    use argonauts_core::names::VendorId;
    use argonauts_core::settings::approvals::SandboxChoice;
    use argonauts_core::worker::Access;

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

    fn refused(result: Result<(), WorkerError>) -> String {
        match result {
            Err(WorkerError::Refused(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn harness_without_sandbox_is_refused() {
        let mut spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
        spec.sandbox.mode = SandboxMode::None;
        let health = healthy(&spec.worker.vendor());
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
        let health = healthy(&spec.worker.vendor());
        let mut settings = settings(SandboxChoice::Os);
        let why = refused(check(&spec, &settings, &health, &failed()));
        assert!(why.contains("a write outside the worktree worked"), "{why}");
        settings.fail_closed = false;
        check(&spec, &settings, &health, &failed()).unwrap();
    }

    #[test]
    fn a_session_that_writes_without_goal_hash_is_refused() {
        let health = healthy(&VendorId("fake".into()));
        let settings = settings(SandboxChoice::Os);
        for access in [Access::Build, Access::Spike, Access::Direct] {
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
    fn disabled_vendor_is_refused() {
        let spec = spec_in("/w".as_ref(), "/s".as_ref(), "hi");
        let mut health = healthy(&spec.worker.vendor());
        health.disabled = Some("untested version 9.9.9; pin 1.0.0".into());
        match check(&spec, &settings(SandboxChoice::Os), &health, &passed()) {
            Err(WorkerError::Disabled { worker, reason }) => {
                assert_eq!(worker, spec.worker);
                assert!(reason.contains("pin 1.0.0"));
            }
            other => panic!("expected Disabled, got {other:?}"),
        }
        // A billing cap leaves the retry on the same vendor free to start.
        health.disabled = Some(crate::health::billing_disable());
        check(&spec, &settings(SandboxChoice::Os), &health, &passed()).unwrap();
    }
}
