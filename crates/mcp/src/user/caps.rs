//! What an outside agent may start: only in a repo you trusted, only while `mcp.start_runs` allows
//! it, at most `mcp.runs_per_day` a day across every repo, never with an approval mode, and
//! never with a safety key among its settings.

use std::path::Path;
use std::sync::Arc;

use argonauts_core::settings::SAFETY_KEYS;
use argonauts_core::settings::agents::StartRuns;
use argonauts_policy::limits::daily::DailyLedger;
use time::OffsetDateTime;

use crate::OutsideAgentOptions;

/// Refuses to serve in a repo you haven't set up with `argonauts setup` and trusted.
pub fn check_can_serve(options: &OutsideAgentOptions) -> anyhow::Result<()> {
    if options.repo_trusted {
        return Ok(());
    }
    anyhow::bail!(
        "argonauts mcp serve: this repo isn't trusted; run `argonauts setup` here and trust its \
         settings first"
    )
}

/// Refuses a start while `mcp.start_runs` is off.
pub fn check_can_start(options: &OutsideAgentOptions) -> Result<(), String> {
    match options.mcp.start_runs {
        StartRuns::TrustedRepos => Ok(()),
        StartRuns::Off => Err(
            "starting runs from MCP is turned off here (mcp.start_runs = \"off\"); start it in \
             the Argonauts Code app"
                .to_owned(),
        ),
    }
}

/// Refuses `key=value` overrides that are malformed or touch a safety key, naming each one.
pub fn check_overrides(overrides: &[String]) -> Result<(), String> {
    let malformed: Vec<&str> = overrides
        .iter()
        .filter(|entry| override_key(entry).is_none())
        .map(String::as_str)
        .collect();
    if let Some(entry) = malformed.first() {
        return Err(format!(
            "{} isn't a `key=value` setting with a plain dotted key",
            quoted(entry)
        ));
    }
    let safety = safety_overrides(overrides);
    if !safety.is_empty() {
        return Err(format!(
            "{} {} safety {}, which only you can change, in the Argonauts Code app",
            safety.join(", "),
            if safety.len() == 1 { "is a" } else { "are" },
            if safety.len() == 1 {
                "setting"
            } else {
                "settings"
            },
        ));
    }
    Ok(())
}

/// The safety keys among `overrides` (`key=value` each), which an outside agent may never set.
/// A key counts when it is a safety key, lies below one, or holds one, so a table value such
/// as `approvals={mode="auto"}` can't carry one in. A malformed key counts too.
pub fn safety_overrides(overrides: &[String]) -> Vec<String> {
    overrides
        .iter()
        .filter_map(|entry| match override_key(entry) {
            Some(key) => touches_safety_key(&key).then(|| key.join(".")),
            None => Some(entry.clone()),
        })
        .collect()
}

/// The key of `key=value` split into lowercase segments, or `None` when it isn't a plain
/// dotted key. Spaces and quotes around a segment are dropped, so they can't dodge the match.
fn override_key(entry: &str) -> Option<Vec<String>> {
    let (key, _value) = entry.split_once('=')?;
    key.split('.')
        .map(|segment| {
            let segment = segment.trim();
            let segment = strip_quotes(segment).trim();
            let plain = !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            plain.then(|| segment.to_ascii_lowercase())
        })
        .collect()
}

fn strip_quotes(segment: &str) -> &str {
    ['"', '\'']
        .iter()
        .find_map(|quote| {
            segment
                .strip_prefix(*quote)
                .and_then(|inner| inner.strip_suffix(*quote))
        })
        .unwrap_or(segment)
}

fn touches_safety_key(key: &[String]) -> bool {
    SAFETY_KEYS.iter().any(|pattern| {
        let pattern: Vec<&str> = pattern.split('.').collect();
        // A trailing `*` covers everything below, which the shared prefix already decides.
        let pattern = match pattern.split_last() {
            Some((&"*", above)) => above,
            _ => &pattern[..],
        };
        pattern
            .iter()
            .zip(key)
            .all(|(want, have)| *want == "*" || want == have)
    })
}

fn quoted(entry: &str) -> String {
    let clean = argonauts_core::safety::sanitize::sanitize_for_terminal(entry);
    let short: String = clean.chars().take(80).collect();
    format!("{short:?}")
}

/// Counts the runs outside agents start each day, across every repo.
pub trait OutsideAgentRuns: Send + Sync {
    /// Takes one of the runs of `now`'s day unless `cap` are taken. Returns that day's count.
    fn take(&self, cap: u32, now: OffsetDateTime) -> Result<u32, String>;
    /// Gives back a run taken at `taken` whose start failed.
    fn give_back(&self, taken: OffsetDateTime) -> Result<(), String>;
}

impl OutsideAgentRuns for DailyLedger {
    fn take(&self, cap: u32, now: OffsetDateTime) -> Result<u32, String> {
        self.take_outside_agent_run(cap, now)
            .map_err(|err| err.to_string())
    }

    fn give_back(&self, taken: OffsetDateTime) -> Result<(), String> {
        self.release_outside_agent_run(taken, OffsetDateTime::now_utc())
            .map_err(|err| err.to_string())
    }
}

/// The daily ledger in `home`, which is `~/.argonauts`.
pub fn daily_ledger(home: &Path) -> Arc<dyn OutsideAgentRuns> {
    Arc::new(DailyLedger::open(home))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(list: &[&str]) -> Vec<String> {
        list.iter().map(|entry| (*entry).to_owned()).collect()
    }

    #[test]
    fn safety_keys_are_found_however_they_are_written() {
        let refused = [
            "approvals.mode=auto",
            "approvals={mode=\"auto\"}",
            "approvals=x",
            " Approvals . mode = auto",
            "\"approvals\".\"mode\"=auto",
            "sandbox.network=true",
            "mcp.start_runs=trusted-repos",
            "mcp=x",
            "agents.claude.binary=/bin/sh",
            "agents.claude={auth=\"api\"}",
            "agents=x",
            "endpoints.local.header_env.x=Y",
            "limits.spend_per_run=100",
            "limits={spend_per_run=100}",
            "limits.spend_per_day=1",
            "merge.mode=auto",
            "checks.test=x",
            "notifications.conversation.url=x",
            "fallback.allow_api_billing=true",
            "fallback.worker.allow_api_billing=true",
        ];
        for entry in refused {
            let found = safety_overrides(&entries(&[entry]));
            assert_eq!(found.len(), 1, "{entry} should be refused");
            assert!(check_overrides(&entries(&[entry])).is_err(), "{entry}");
        }
    }

    #[test]
    fn ordinary_keys_pass() {
        let allowed = [
            "agents.claude.model=haiku",
            "limit.warn_at=0.5",
            "ui.theme=dark",
            "roles.worker.agent=claude",
            "notifications.bell=false",
        ];
        for entry in allowed {
            assert!(
                safety_overrides(&entries(&[entry])).is_empty(),
                "{entry} is not a safety key"
            );
            assert_eq!(check_overrides(&entries(&[entry])), Ok(()), "{entry}");
        }
    }

    #[test]
    fn malformed_overrides_are_refused() {
        for entry in ["approvals", "", "=x", "a..b=1", "a.*.b=1", "a b=1", "ä=1"] {
            assert!(check_overrides(&entries(&[entry])).is_err(), "{entry:?}");
            assert_eq!(safety_overrides(&entries(&[entry])).len(), 1, "{entry:?}");
        }
    }

    #[test]
    fn refusals_name_every_safety_key() {
        let err = check_overrides(&entries(&[
            "ui.theme=dark",
            "sandbox.network=on",
            "mcp.pause=1",
        ]))
        .unwrap_err();
        assert!(err.contains("sandbox.network"), "{err}");
        assert!(err.contains("mcp.pause"), "{err}");
        assert!(!err.contains("ui.theme"), "{err}");
    }

    #[test]
    fn serving_needs_a_trusted_repo() {
        let err = check_can_serve(&OutsideAgentOptions::default()).unwrap_err();
        assert!(err.to_string().contains("argonauts setup"), "{err}");
        let trusted = OutsideAgentOptions {
            repo_trusted: true,
            ..OutsideAgentOptions::default()
        };
        assert!(check_can_serve(&trusted).is_ok());
    }
}
