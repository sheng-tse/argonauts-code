//! The hard limits the evidence can show at a decision point. Only `agent_paths` count;
//! `conductor_paths` are the conductor's own commits and are never a violation. Time passing
//! never is one either, since a parked run never expires.
//!
//! Irreversible actions and self-approval have no facts here: the approver checks its own
//! independence from `DecisionPoint.authors`.

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::policy::{DecisionPoint, EvidencePacket, HardLimit, Violation};
use argonauts_core::settings::Settings;

use super::deps::{allowed_at_point, is_lockfile};
use super::paths::{PathRules, held_paths, instruction_flag, sample_path};
use super::violation;

/// `settings` are the ones in force now, and `run_start` the ones the run started with.
pub fn check(
    point: &DecisionPoint,
    evidence: &EvidencePacket,
    settings: &Settings,
    run_start: &Settings,
) -> Vec<Violation> {
    let facts = &evidence.facts;
    let merge = point.kind == CheckpointKind::Merge;
    let mut violations = Vec::new();

    // The packet binds what gets approved, so one built for another kind of point covers
    // nothing here.
    if evidence.kind != point.kind {
        violations.push(violation(
            HardLimit::HeadMoved,
            format!(
                "the evidence is for a {:?} checkpoint, not this {:?} one",
                evidence.kind, point.kind
            ),
            Vec::new(),
        ));
    }
    let names_commits = matches!(point.kind, CheckpointKind::Plan | CheckpointKind::Merge);
    if names_commits && facts.subject.is_none() {
        violations.push(violation(
            HardLimit::NoSubject,
            "the evidence names no commits",
            Vec::new(),
        ));
    }
    if merge && facts.gates_passed != Some(true) {
        let detail = match facts.gates_passed {
            Some(_) => "a gate failed",
            None => "the gates didn't run",
        };
        violations.push(violation(HardLimit::GatesFailed, detail, Vec::new()));
    }

    violations.extend(protected_paths(evidence, settings));
    let off_list: Vec<&str> = facts
        .new_dependencies
        .iter()
        .map(String::as_str)
        .filter(|name| !allowed_at_point(name, &settings.limits.dependency_allowlist))
        .collect();
    if !off_list.is_empty() {
        violations.push(violation(
            HardLimit::Dependency,
            format!(
                "adds dependencies that aren't on your allowlist: {}",
                off_list.join(", ")
            ),
            Vec::new(),
        ));
    }
    let locks = agent_paths_where(evidence, |path| is_lockfile(&sample_path(path)));
    if !locks.is_empty() {
        violations.push(violation(
            HardLimit::Lockfile,
            "an agent changes lockfiles, which only the conductor updates",
            locks,
        ));
    }

    // An unreadable spend (NaN) compares as nothing, so it counts as over.
    let under = |ceiling: f64| {
        facts.spent_today_usd.partial_cmp(&ceiling) == Some(std::cmp::Ordering::Less)
    };
    if let Some(ceiling) = settings.limits.usd_per_day
        && !under(ceiling)
    {
        violations.push(violation(
            HardLimit::Ceiling,
            format!(
                "spent ${:.2} today, at or over the ${ceiling:.2} daily ceiling",
                facts.spent_today_usd
            ),
            Vec::new(),
        ));
    }
    if facts.send_backs >= 2 {
        violations.push(violation(
            HardLimit::SentBackTwice,
            format!("sent back {} times, so it goes to you", facts.send_backs),
            Vec::new(),
        ));
    }
    let cap = settings.limits.auto_merges_per_day;
    if merge && facts.auto_merges_today >= cap {
        violations.push(violation(
            HardLimit::AutoMergeCap,
            format!(
                "{} merges were auto-approved today, and the cap is {cap}",
                facts.auto_merges_today
            ),
            Vec::new(),
        ));
    }
    if facts.acceptance_tests_valid == Some(false) {
        violations.push(violation(
            HardLimit::TestPassesOnStubs,
            "an acceptance test passes on the stubs or fails on the integration branch",
            Vec::new(),
        ));
    }
    if !facts.untested_criteria.is_empty() {
        violations.push(violation(
            HardLimit::CriterionUntested,
            format!("no test covers {}", facts.untested_criteria.join(", ")),
            Vec::new(),
        ));
    }
    if let Some(flags) = facts.tamper_flags.filter(|&flags| flags > 0) {
        violations.push(violation(
            HardLimit::TamperFlag,
            format!("{flags} test-tampering flags are open"),
            Vec::new(),
        ));
    }
    if merge {
        let paths = evidence.facts.agent_paths.iter().map(String::as_str);
        let instructions = held_paths(paths, None);
        if !instructions.is_empty() {
            violations.push(instruction_flag(instructions));
        }
    }
    violations.extend(no_sandbox(settings, run_start));
    violations
}

/// Turning the sandbox back on mid-run covers only new sessions. The ones that already ran
/// had none, so every checkpoint of that run still asks.
fn no_sandbox(settings: &Settings, run_start: &Settings) -> Option<Violation> {
    let (now, started) = (settings.sandbox.mode, run_start.sandbox.mode);
    let detail = if now.ours_off() {
        format!(
            "you set sandbox.mode to {}, so this run's sessions have no sandbox of ours and \
             every checkpoint asks you",
            now.as_str()
        )
    } else if started.ours_off() {
        format!(
            "this run started with sandbox.mode {}, so its earlier sessions had no sandbox of \
             ours and every checkpoint asks you",
            started.as_str()
        )
    } else {
        return None;
    };
    Some(violation(HardLimit::NoSandbox, detail, Vec::new()))
}

/// Agent paths under `.argonauts/**` or `limits.protected_paths`, plus the protected subset the
/// engine already found.
fn protected_paths(evidence: &EvidencePacket, settings: &Settings) -> Vec<Violation> {
    let rules = PathRules::protected(settings);
    let mut violations = Vec::new();
    if !rules.invalid.is_empty() {
        violations.push(violation(
            HardLimit::ProtectedPath,
            format!(
                "these protected path patterns aren't valid globs, so what they protect can't \
                 be checked: {}",
                rules.invalid.join(", ")
            ),
            Vec::new(),
        ));
    }
    let mut paths = agent_paths_where(evidence, |path| rules.covers(path));
    paths.extend(evidence.facts.protected_paths.iter().cloned());
    paths.sort();
    paths.dedup();
    if !paths.is_empty() {
        violations.push(violation(
            HardLimit::ProtectedPath,
            "an agent changes protected paths",
            paths,
        ));
    }
    violations
}

fn agent_paths_where(evidence: &EvidencePacket, test: impl Fn(&str) -> bool) -> Vec<String> {
    let mut paths: Vec<String> = evidence
        .facts
        .agent_paths
        .iter()
        .filter(|path| test(path))
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();
    paths
}
