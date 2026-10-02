//! The merge checkpoint's packet: per-card rows, the combined diff saved to a file, and the
//! integration tree's SHA as evidence, with `EvidenceFacts.subject` naming the base, the
//! integration head, and its tree.

use std::path::PathBuf;

use globset::{Glob, GlobSetBuilder};

use argonauts_core::events::card::{CardKind, CardState, GateKind, GateOutcome, ReviewEvent};
use argonauts_core::events::checkpoint::{MergePacket, MergeRow};
use argonauts_core::events::session::EndReason;
use argonauts_core::names::WorkerRef;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts, EvidenceSubject};
use argonauts_core::registry::company_of;
use argonauts_core::steps::judgment::ReviewVerdict;

use crate::card::scheduler::left_out;
use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::handoff::{merge_rule, notes};
use crate::integrate::final_check::{self, FinalCheck};
use crate::integrate::{branch, conductor};
use crate::roles;
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// Paths no card may touch, whatever the settings say.
const ALWAYS_PROTECTED: &str = ".argonauts/**";

/// Everything the merge checkpoint opens with.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeEvidence {
    pub packet: MergePacket,
    pub facts: EvidenceFacts,
    pub documents: Vec<EvidenceDocument>,
    /// Review verdicts, debt, and the final check's problem, one line each.
    pub agent_outputs: Vec<String>,
}

impl MergeEvidence {
    /// What an approval covers.
    pub fn subject(&self) -> Option<&EvidenceSubject> {
        self.facts.subject.as_ref()
    }
}

/// Where the landing branch stands now.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub branch: String,
    pub worktree: PathBuf,
    pub target: Option<String>,
    /// The target's head, when the run has a target.
    pub main: Option<String>,
    /// What an approval covers: the base, the branch's head, and its tree.
    pub subject: EvidenceSubject,
}

/// A few rev-parses, so a re-entered step can check an answered checkpoint against the branch
/// before building the whole packet again.
pub async fn position(host: &Host) -> EngineResult<Position> {
    let git = host.git();
    let (branch, worktree) = landing_branch(host).await?;
    let head = git.resolve(&worktree, "HEAD").await?;
    let tree_sha = git.tree_of(&worktree, &head).await?;
    let target = final_check::target(host);
    let main = match &target {
        Some(target) => Some(git.resolve(&host.repo().root, target).await?),
        None => None,
    };
    let base = match &main {
        Some(main) => git.merge_base(&worktree, main, &head).await?,
        None => None,
    };
    let base = base
        .or_else(|| final_check::run_base(host))
        .ok_or_else(|| EngineError::Refused("the run has no base commit".to_owned()))?;
    Ok(Position {
        branch,
        worktree,
        target,
        main,
        subject: EvidenceSubject {
            base_commit: base,
            head_commit: head,
            tree_sha,
        },
    })
}

pub async fn build(host: &Host, check: &FinalCheck) -> EngineResult<MergeEvidence> {
    let git = host.git();
    let Position {
        branch,
        worktree,
        target,
        main,
        subject,
    } = position(host).await?;
    let (base, head, tree_sha) = (
        subject.base_commit.clone(),
        subject.head_commit.clone(),
        subject.tree_sha.clone(),
    );
    let main_in = match &main {
        Some(main) => git.is_ancestor(&worktree, main, &head).await?,
        None => true,
    };

    let combined = git.diff_stat(&worktree, &base, &head).await?;
    let diff = git.diff(&worktree, &base, &head).await?;
    let diff_path = host
        .repo()
        .run_dir
        .join("merge")
        .join(format!("{head}.diff"));
    if let Some(dir) = diff_path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&diff_path, &diff).await?;

    let changed = git.changed_paths(&worktree, &base, &head).await?;
    let conductor_paths = conductor::conductor_paths(host, &worktree, &base, &head).await?;
    let agent_paths: Vec<String> = changed
        .into_iter()
        .filter(|path| !conductor_paths.contains(path))
        .collect();
    let protected_paths = protected(&host.settings().limits.protected_paths, &agent_paths)?;
    let conductor = git
        .diff_stat_of(&worktree, &base, &head, &conductor_paths)
        .await?;

    let cards: Vec<CardRecord> = host.read(|state| {
        state
            .cards
            .by_id
            .values()
            .filter(|record| in_the_merge(record))
            .cloned()
            .collect()
    });
    let mut rows = Vec::new();
    let mut reviewed_by_other_company = true;
    for record in &cards {
        // Every commit that lands names its author in a note.
        notes::catch_up(host, &record.spec.id).await?;
        let row = row(host, record).await?;
        reviewed_by_other_company &= approved_elsewhere(host, record, row.owner.as_ref());
        rows.push(row);
    }

    let (budget, partial, dependencies) = host.read(|state| {
        (
            state.run.budget.clone(),
            stopped_early(state),
            state.integration.dependencies.clone(),
        )
    });
    let cards_passed = rows.iter().all(|row| row.gates_passed);
    let reviews_approved = rows
        .iter()
        .all(|row| row.review == Some(ReviewVerdict::Approve) && row.debt.is_empty());
    let suite_on_latest_main = check.passed && check.suite_ran() && main_in;
    // Debt counts where the card's merge rule is `upgrade_review`; under `none` it never blocks.
    let unreviewed_debt: Vec<String> = rows
        .iter()
        .filter(|row| !row.debt.is_empty() && merge_rule::enforced(host, &row.card))
        .map(|row| row.card.to_string())
        .collect();

    let mut agent_outputs: Vec<String> = rows.iter().map(row_line).collect();
    agent_outputs.extend(host.read(left_out_line));
    if let Some(problem) = &check.problem {
        agent_outputs.push(format!("final check: {problem}"));
    }

    let spent = budget
        .as_ref()
        .map(|budget| budget.spent)
        .unwrap_or_default();
    let packet = MergePacket {
        target,
        branch,
        rows,
        suite_passed_on_latest_main: suite_on_latest_main,
        gates: check.gates.clone(),
        combined,
        conductor,
        diff_path: diff_path.clone(),
        tree_sha: tree_sha.clone(),
        spent,
        limit: budget.and_then(|budget| budget.limit),
        partial,
    };
    let facts = EvidenceFacts {
        subject: Some(subject),
        new_dependencies: dependencies,
        agent_paths,
        conductor_paths,
        protected_paths,
        gates_passed: Some(check.passed && cards_passed),
        reviews_approved: Some(reviews_approved),
        reviewed_by_other_company: Some(reviewed_by_other_company),
        combined_diff_lines: Some(combined.insertions.saturating_add(combined.deletions)),
        suite_on_latest_main: Some(suite_on_latest_main),
        unreviewed_debt: Some(unreviewed_debt),
        spent,
        ..EvidenceFacts::default()
    };
    let documents = vec![EvidenceDocument {
        label: "combined diff".to_owned(),
        text: diff,
        path: None,
        sha256: None,
    }];
    Ok(MergeEvidence {
        packet,
        facts,
        documents,
        agent_outputs,
    })
}

/// The branch the run lands, and its worktree: the integration branch, or the solo card's.
pub async fn landing_branch(host: &Host) -> EngineResult<(String, PathBuf)> {
    let solo = host.read(|state| {
        let record = solo_record(state)?;
        record.branch.clone().zip(record.worktree.clone())
    });
    if let Some(solo) = solo {
        return Ok(solo);
    }
    let worktree = branch::ensure_integration(host).await?;
    Ok((branch::integration_branch(host.run_id()), worktree))
}

/// The branch a run's work sits on, as its log tells it: a solo run's one card branch, or the
/// integration branch once there is one. Readers outside the engine, such as a replay looking
/// for what to score, use this.
pub fn landing_branch_of(state: &RunState) -> Option<String> {
    match solo_record(state) {
        Some(record) => record.branch.clone(),
        None => state.integration.branch.clone(),
    }
}

/// The run's one solo card, while no integration branch exists.
fn solo_record(state: &RunState) -> Option<&CardRecord> {
    if state.integration.branch.is_some() {
        return None;
    }
    let mut solo = state
        .cards
        .by_id
        .values()
        .filter(|record| record.spec.kind == CardKind::Solo);
    match (solo.next(), solo.next()) {
        (Some(record), None) => Some(record),
        _ => None,
    }
}

/// Cards whose work the merge carries: integrated ones, but not spikes, whose experiments
/// never land. A deferred card, and any card waiting on one, is left out.
pub fn in_the_merge(record: &CardRecord) -> bool {
    record.state == CardState::Integrated && !matches!(record.spec.kind, CardKind::Spike { .. })
}

/// One line naming the cards the merge leaves out, if any.
fn left_out_line(state: &RunState) -> Option<String> {
    let cards: Vec<String> = left_out(state)
        .into_iter()
        .map(|card| match state.cards.get(&card).map(|r| r.state) {
            Some(CardState::Deferred) => format!("{card} (deferred)"),
            _ => format!("{card} (waits on a deferred card)"),
        })
        .collect();
    (!cards.is_empty()).then(|| format!("left out: {}", cards.join(", ")))
}

async fn row(host: &Host, record: &CardRecord) -> EngineResult<MergeRow> {
    let stat = match (&record.base, &record.branch) {
        (Some(base), Some(branch)) => {
            let root = host.repo().root.clone();
            host.git().diff_stat(&root, base, branch).await?
        }
        _ => argonauts_core::DiffStat::default(),
    };
    Ok(MergeRow {
        card: record.spec.id.clone(),
        title: record.spec.title.clone(),
        kind: record.spec.kind.clone(),
        stat,
        owner: record.owner.clone().or_else(|| record.spec.owner.clone()),
        gates_passed: card_gates_passed(record),
        review: last_verdict(&record.reviews),
        debt: record.debt.iter().cloned().collect(),
    })
}

/// A test card's command gates fail against the stubs by design, so its stub check's verdict
/// stands for them. Every other card goes by each gate's latest outcome.
fn card_gates_passed(record: &CardRecord) -> bool {
    if !matches!(record.spec.kind, CardKind::AcceptanceTests { .. }) {
        return gates_passed(&record.gates);
    }
    let judged: Vec<GateOutcome> = record
        .gates
        .iter()
        .filter(|gate| !(gate.kind == GateKind::Command && gate.command.is_some()))
        .cloned()
        .collect();
    gates_passed(&judged)
}

/// Each gate's latest outcome passed. A card with no gates has nothing failing.
pub fn gates_passed(gates: &[GateOutcome]) -> bool {
    let mut latest: Vec<&GateOutcome> = Vec::new();
    for gate in gates {
        latest.retain(|seen| seen.name != gate.name);
        latest.push(gate);
    }
    latest.iter().all(|gate| gate.passed)
}

/// Whether the verdict that stands on the card came from another company than its owner's. A
/// family nobody knows can't be shown to differ, so it doesn't.
fn approved_elsewhere(host: &Host, record: &CardRecord, owner: Option<&WorkerRef>) -> bool {
    let company = |worker: &WorkerRef| {
        let family = roles::family(host, worker);
        (!family.is_empty() && family != "unknown").then(|| company_of(&family))
    };
    let judge = record.reviews.iter().rev().find_map(|event| match event {
        ReviewEvent::Reviewed { reviewer, .. } => Some(reviewer),
        ReviewEvent::Adjudicated { adjudicator, .. } => Some(adjudicator),
        ReviewEvent::TestsWritten { .. } => None,
    });
    match (judge.and_then(company), owner.and_then(company)) {
        (Some(judge), Some(owner)) => judge != owner,
        _ => false,
    }
}

/// The verdict that stands: an adjudication, else the latest reviewer's.
pub fn last_verdict(reviews: &[ReviewEvent]) -> Option<ReviewVerdict> {
    reviews.iter().rev().find_map(|event| match event {
        ReviewEvent::Reviewed { review, .. } => Some(review.verdict),
        ReviewEvent::Adjudicated { ruling, .. } => Some(ruling.verdict),
        ReviewEvent::TestsWritten { .. } => None,
    })
}

fn row_line(row: &MergeRow) -> String {
    let review = match row.review {
        Some(ReviewVerdict::Approve) => "approved",
        Some(ReviewVerdict::RequestChanges) => "changes requested",
        Some(ReviewVerdict::Reject) => "rejected",
        None => "not reviewed",
    };
    let gates = if row.gates_passed {
        "gates passed"
    } else {
        "gates failed"
    };
    let mut line = format!("{}: {gates}, {review}", row.card);
    for debt in &row.debt {
        line.push_str(&format!(
            ", needs review by {} (work by {} since handoff-{})",
            debt.needs, debt.by, debt.since
        ));
    }
    line
}

/// An agent was stopped before it finished, so some work may be partial.
fn stopped_early(state: &RunState) -> bool {
    state.sessions.by_key.values().any(|session| {
        matches!(
            session.end.as_ref().map(|end| &end.reason),
            Some(EndReason::Stopped | EndReason::BudgetHit { .. })
        )
    })
}

/// The protected subset of `paths`.
pub fn protected(patterns: &[String], paths: &[String]) -> EngineResult<Vec<String>> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns
        .iter()
        .map(String::as_str)
        .chain([ALWAYS_PROTECTED])
    {
        let glob = Glob::new(pattern)
            .map_err(|err| EngineError::Refused(format!("protected path {pattern}: {err}")))?;
        builder.add(glob);
    }
    let set = builder
        .build()
        .map_err(|err| EngineError::Refused(format!("protected paths: {err}")))?;
    Ok(paths
        .iter()
        .filter(|path| set.is_match(path.as_str()))
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(name: &str, passed: bool) -> GateOutcome {
        GateOutcome {
            name: name.to_owned(),
            kind: GateKind::Command,
            passed,
            command: None,
            exit_code: Some(i32::from(!passed)),
            duration_ms: 1,
            summary: String::new(),
            output_tail: String::new(),
            violations: Vec::new(),
            sandboxed: true,
            skipped: false,
        }
    }

    #[test]
    fn only_each_gates_latest_outcome_counts() {
        assert!(gates_passed(&[]));
        assert!(gates_passed(&[gate("tests", false), gate("tests", true)]));
        assert!(!gates_passed(&[gate("tests", true), gate("lint", false)]));
        assert!(!gates_passed(&[gate("tests", true), gate("tests", false)]));
    }

    #[test]
    fn protected_paths_include_the_argonauts_folder() {
        let paths = vec![
            "src/lib.rs".to_owned(),
            ".argonauts/goal.md".to_owned(),
            ".github/workflows/ci.yml".to_owned(),
        ];
        let found = protected(&[".github/**".to_owned()], &paths).unwrap();
        assert_eq!(found, vec![paths[1].clone(), paths[2].clone()]);
        assert!(protected(&["[".to_owned()], &paths).is_err());
    }
}
