//! The merge approval's packet: per-task rows, the combined diff saved to a file, and the
//! merge tree's SHA as evidence, with `EvidenceFacts.subject` naming the base, the
//! merge head, and its tree.

use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};

use argonauts_core::events::approval::{MergePacket, MergeRow};
use argonauts_core::events::run::ChangedFile;
use argonauts_core::events::session::EndReason;
use argonauts_core::events::task::{
    CheckKind, CheckOutcome, PickedBy, ReviewEvent, TaskKind, TaskState,
};
use argonauts_core::names::ModelRef;
use argonauts_core::policy::{EvidenceDocument, EvidenceFacts, EvidenceSubject, WithinProvider};
use argonauts_core::registry::provider::is_known;
use argonauts_core::steps::judgment::ReviewVerdict;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};
use crate::fallback::{merge_rule, notes};
use crate::merge::final_check::{self, FinalCheck};
use crate::merge::{branch, conductor, deliver, unborn};
use crate::review::reviewers;
use crate::roles;
use crate::state::RunState;
use crate::state::tasks::TaskEntry;
use crate::task::scheduler::left_out;
use crate::understand::materiality::one_line;

/// Paths no task may touch, whatever the settings say.
const ALWAYS_PROTECTED: &str = ".argonauts/**";

/// Everything the merge approval opens with.
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

/// A few rev-parses, so a re-entered step can check an answered approval against the branch
/// before building the whole packet again.
pub async fn position(host: &Host) -> EngineResult<Position> {
    let git = host.git();
    let (branch, worktree) = landing_branch(host).await?;
    let head = git.resolve(&worktree, "HEAD").await?;
    let tree_sha = git.tree_of(&worktree, &head).await?;
    let target = final_check::target(host);
    let main = match &target {
        Some(target) => Some(final_check::main_commit(host, target).await?),
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

/// Beside the merge's diff, `git diff --numstat` of it as `ChangedFile`s.
pub fn merged_files_path(diff_path: &Path) -> PathBuf {
    diff_path.with_extension("files.json")
}

/// The two ends of the change the merge question shows and the commit message describes:
/// from `base` to `head`, or, for a run that started from your uncommitted edits, from your
/// branch to what would land on it, which holds none of them.
pub(crate) async fn shown_range(
    host: &Host,
    worktree: &Path,
    main: Option<&str>,
    base: &str,
    head: &str,
) -> EngineResult<(String, String)> {
    let (Some(snapshot), Some(main)) = (deliver::snapshot_of_yours(host), main) else {
        return Ok((base.to_owned(), head.to_owned()));
    };
    let scratch = host.repo().run_dir.join("merge.index");
    let lands = host
        .git()
        .tree_without_yours(worktree, &scratch, &snapshot, main, head)
        .await?;
    Ok((main.to_owned(), lands))
}

pub async fn build(host: &Host, check: &FinalCheck) -> EngineResult<MergeEvidence> {
    let git = host.git();
    let Position {
        branch,
        worktree,
        main,
        subject,
        ..
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

    let (from, to) = shown_range(host, &worktree, main.as_deref(), &base, &head).await?;
    let combined = git.diff_stat(&worktree, &from, &to).await?;
    let diff = git.diff(&worktree, &from, &to).await?;
    let diff_path = host
        .repo()
        .run_dir
        .join("merge")
        .join(format!("{head}.diff"));
    if let Some(dir) = diff_path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    tokio::fs::write(&diff_path, &diff).await?;
    // Each file's lines as git counts them, for the closing answer's bars.
    let files: Vec<ChangedFile> = git
        .diff_files(&worktree, &from, &to)
        .await?
        .into_iter()
        .map(|(path, insertions, deletions)| ChangedFile {
            path,
            insertions,
            deletions,
        })
        .collect();
    let files_json = serde_json::to_vec(&files).map_err(std::io::Error::other)?;
    tokio::fs::write(merged_files_path(&diff_path), files_json).await?;

    let changed = git.changed_paths(&worktree, &from, &to).await?;
    let conductor_paths = conductor::conductor_paths(host, &worktree, &base, &head).await?;
    let agent_paths: Vec<String> = changed
        .into_iter()
        .filter(|path| !conductor_paths.contains(path))
        .collect();
    let protected_paths = protected(&host.settings().limits.protected_paths, &agent_paths)?;
    let conductor = git
        .diff_stat_of(&worktree, &from, &to, &conductor_paths)
        .await?;

    let tasks: Vec<TaskEntry> = host.read(|state| {
        state
            .tasks
            .by_id
            .values()
            .filter(|record| in_the_merge(record))
            .cloned()
            .collect()
    });
    let mut rows = Vec::new();
    let mut reviewed_by_other_provider = true;
    let mut approved_within_provider = Vec::new();
    let mut unreviewed = Vec::new();
    for record in &tasks {
        // Every commit that lands names its author in a note.
        notes::catch_up(host, &record.spec.id).await?;
        let row = row(host, record).await?;
        match approval_of(host, record, row.worker.as_ref()) {
            Approval::Counts => {}
            Approval::Neither => reviewed_by_other_provider = false,
            Approval::Unreviewed => {
                reviewed_by_other_provider = false;
                unreviewed.push(why_unreviewed(host, record));
            }
            Approval::WithinProvider { reviewer, fallback } => {
                reviewed_by_other_provider = false;
                approved_within_provider.push(WithinProvider {
                    task: host.read(|state| crate::labels::task_label(state, &record.spec.id)),
                    reviewer: roles::display_name(&reviewer),
                    fallback,
                });
            }
        }
        rows.push(row);
    }

    let (limit, partial, dependencies) = host.read(|state| {
        (
            state.run.spend.clone(),
            stopped_early(state),
            state.merge.dependencies.clone(),
        )
    });
    let tasks_passed = rows.iter().all(|row| row.checks_passed);
    let reviews_approved = rows
        .iter()
        .all(|row| row.review == Some(ReviewVerdict::Approve) && row.debt.is_empty());
    let suite_on_latest_main = check.passed && check.suite_ran() && main_in;
    // Debt counts where the task's merge rule is `upgrade_review`; under `none` it never blocks.
    // Each task goes by its label, as the merge table and its task name it.
    let unreviewed_debt: Vec<String> = rows
        .iter()
        .filter(|row| !row.debt.is_empty() && merge_rule::enforced(host, &row.task))
        .map(|row| host.read(|state| crate::labels::task_label(state, &row.task)))
        .collect();

    let mut agent_outputs: Vec<String> = rows.iter().map(row_line).collect();
    agent_outputs.extend(host.read(left_out_line));
    agent_outputs.extend(unborn::approval_line(host).await?);
    if let Some(problem) = &check.problem {
        agent_outputs.push(format!("final check: {problem}"));
    }

    let spent = limit.as_ref().map(|limit| limit.spent).unwrap_or_default();
    let packet = MergePacket {
        target: final_check::lands_on(host).await?,
        branch,
        rows,
        suite_passed_on_latest_main: suite_on_latest_main,
        checks: check.checks.clone(),
        combined,
        conductor,
        diff_path: diff_path.clone(),
        tree_sha: tree_sha.clone(),
        spent,
        limit: limit.and_then(|limit| limit.limit),
        partial,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
        both_changed: Vec::new(),
        asks_which_branch: false,
        again: None,
    };
    let facts = EvidenceFacts {
        subject: Some(subject),
        new_dependencies: dependencies,
        agent_paths,
        conductor_paths,
        protected_paths,
        checks_passed: Some(check.passed && tasks_passed),
        reviews_approved: Some(reviews_approved),
        reviewed_by_other_provider: Some(reviewed_by_other_provider),
        approved_within_provider,
        unreviewed,
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

/// The branch the run lands, and its worktree: the run branch, or the Captain task's.
pub async fn landing_branch(host: &Host) -> EngineResult<(String, PathBuf)> {
    let captain = host.read(|state| {
        let record = captain_record(state)?;
        record.branch.clone().zip(record.worktree.clone())
    });
    if let Some(captain) = captain {
        return Ok(captain);
    }
    let worktree = branch::ensure_run_branch(host).await?;
    Ok((branch::run_branch(host.run_id()), worktree))
}

/// The branch a run's work sits on, as its log tells it: a Captain run's one task branch, or the
/// run branch once there is one. Readers outside the engine, such as a replay looking
/// for what to score, use this.
pub fn landing_branch_of(state: &RunState) -> Option<String> {
    match captain_record(state) {
        Some(record) => record.branch.clone(),
        None => state.merge.branch.clone(),
    }
}

/// The run's one Captain task, while no run branch exists.
fn captain_record(state: &RunState) -> Option<&TaskEntry> {
    if state.merge.branch.is_some() {
        return None;
    }
    let mut captain = state
        .tasks
        .by_id
        .values()
        .filter(|record| record.spec.kind == TaskKind::Captain);
    match (captain.next(), captain.next()) {
        (Some(record), None) => Some(record),
        _ => None,
    }
}

/// Tasks whose work the merge carries: merged ones, but not experiments, whose work never
/// lands. A deferred task, and any task waiting on one, is left out.
pub fn in_the_merge(record: &TaskEntry) -> bool {
    record.state == TaskState::Merged && !matches!(record.spec.kind, TaskKind::Experiment { .. })
}

/// One line naming the tasks the merge leaves out, if any.
fn left_out_line(state: &RunState) -> Option<String> {
    let tasks: Vec<String> = left_out(state)
        .into_iter()
        .map(|task| match state.tasks.get(&task).map(|r| r.state) {
            Some(TaskState::Deferred) => format!("{task} (deferred)"),
            _ => format!("{task} (waits on a deferred task)"),
        })
        .collect();
    (!tasks.is_empty()).then(|| format!("left out: {}", tasks.join(", ")))
}

async fn row(host: &Host, record: &TaskEntry) -> EngineResult<MergeRow> {
    let stat = match (&record.base, &record.branch) {
        (Some(base), Some(branch)) => {
            let root = host.repo().root.clone();
            host.git().diff_stat(&root, base, branch).await?
        }
        _ => argonauts_core::DiffStat::default(),
    };
    Ok(MergeRow {
        task: record.spec.id.clone(),
        title: record.spec.title.clone(),
        kind: record.spec.kind.clone(),
        stat,
        worker: record.worker.clone().or_else(|| record.spec.worker.clone()),
        checks_passed: task_checks_passed(record),
        review: last_verdict(&record.reviews),
        debt: record.debt.iter().cloned().collect(),
        questions: host
            .read(|state| crate::blackboard::questions::for_the_merge(state, &record.spec.id)),
    })
}

/// A test task's command checks fail against the stubs by design, so its stub check's verdict
/// stands for them. Every other task goes by each check's latest outcome.
fn task_checks_passed(record: &TaskEntry) -> bool {
    if !matches!(record.spec.kind, TaskKind::AcceptanceTests { .. }) {
        return checks_passed(&record.checks);
    }
    let judged: Vec<CheckOutcome> = record
        .checks
        .iter()
        .filter(|check| !(check.kind == CheckKind::Command && check.command.is_some()))
        .cloned()
        .collect();
    checks_passed(&judged)
}

/// Each check's latest outcome passed. A task with no checks has nothing failing.
pub fn checks_passed(checks: &[CheckOutcome]) -> bool {
    let mut latest: Vec<&CheckOutcome> = Vec::new();
    for check in checks {
        latest.retain(|seen| seen.name != check.name);
        latest.push(check);
    }
    latest.iter().all(|check| check.passed)
}

/// How a task's approval stands to the merge rule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Approval {
    /// It came from a reviewer of another provider than the worker's, or from one you chose.
    Counts,
    /// It came only from reviewers code picked from the worker's own provider: who gave the
    /// verdict that stands, and whether that was a fallback in another's place.
    WithinProvider { reviewer: ModelRef, fallback: bool },
    /// The work isn't approved, or nothing says which provider made it.
    Neither,
    /// Nobody reviewed the work, though it's a task to review.
    Unreviewed,
}

/// Why a task to review came to the merge without one, naming it: nobody but its builder, or
/// its builder's provider, may review it.
fn why_unreviewed(host: &Host, record: &TaskEntry) -> String {
    crate::review::nobody_to_review(host, &record.spec.id).unwrap_or_else(|| {
        let label = host.read(|state| crate::labels::task_label(state, &record.spec.id));
        format!("Nobody reviewed {label}")
    })
}

/// The task's approval as the run's records hold it, with providers from the registry.
fn approval_of(host: &Host, record: &TaskEntry, worker: Option<&ModelRef>) -> Approval {
    if last_verdict(&record.reviews).is_none() && crate::review::needed(host, record) {
        return Approval::Unreviewed;
    }
    let provider = |model: &ModelRef| Some(roles::provider(host, model)).filter(|p| is_known(p));
    let sessions = host.read(|state| {
        let keys = &state.tasks.progress(&record.spec.id).review_sessions;
        keys.iter()
            .map(|key| {
                let session = key.as_ref().and_then(|key| state.sessions.get(key));
                session.is_some_and(|session| session.asked_worker() != &session.started.worker)
            })
            .collect::<Vec<bool>>()
    });
    let fallback = |index: usize| sessions.get(index).copied().unwrap_or(false);
    approval(&record.reviews, worker, &provider, &fallback)
}

/// Whether the task's approval counts: the verdict that stands came from another provider
/// than its worker's or from a reviewer you chose, or it approves and a review of the newest
/// round that did approved too, with nobody in that round asking for more. A provider nobody
/// knows can't be shown to be different, so it isn't. `fallback` says whether the review at an
/// index of `reviews` came from a fallback in another's place.
fn approval(
    reviews: &[ReviewEvent],
    worker: Option<&ModelRef>,
    provider: &dyn Fn(&ModelRef) -> Option<String>,
    fallback: &dyn Fn(usize) -> bool,
) -> Approval {
    let worker = worker.and_then(provider);
    let counts = |by: &ModelRef, picked_by: Option<PickedBy>| {
        reviewers::counts(picked_by, provider(by).as_deref(), worker.as_deref())
    };
    let standing = reviews
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, event)| verdict_of(event).map(|read| (index, read)));
    let Some((index, (by, _, picked_by))) = standing else {
        return Approval::Neither;
    };
    if counts(by, picked_by) {
        return Approval::Counts;
    }
    if worker.is_none() || last_verdict(reviews) != Some(ReviewVerdict::Approve) {
        return Approval::Neither;
    }
    let mut cross_approved = false;
    for event in newest_round(reviews) {
        let Some((by, verdict, picked_by)) = verdict_of(event) else {
            continue;
        };
        if verdict != ReviewVerdict::Approve {
            return Approval::Neither;
        }
        cross_approved |= counts(by, picked_by);
    }
    if cross_approved {
        return Approval::Counts;
    }
    Approval::WithinProvider {
        reviewer: by.clone(),
        fallback: fallback(index),
    }
}

/// Who gave a review or a ruling, its verdict, and who chose them.
fn verdict_of(event: &ReviewEvent) -> Option<(&ModelRef, ReviewVerdict, Option<PickedBy>)> {
    match event {
        ReviewEvent::Reviewed {
            reviewer,
            review,
            picked_by,
            ..
        } => Some((reviewer, review.verdict, *picked_by)),
        ReviewEvent::Settled {
            judge,
            ruling,
            picked_by,
        } => Some((judge, ruling.verdict, *picked_by)),
        ReviewEvent::TestsWritten { .. } => None,
    }
}

/// The reviews of the work as it stands: from the first review of the highest round on, so a
/// judge who settled that round counts with it, and an approval of an earlier build doesn't.
fn newest_round(reviews: &[ReviewEvent]) -> &[ReviewEvent] {
    let round = |event: &ReviewEvent| match event {
        ReviewEvent::Reviewed { round, .. } => Some(*round),
        _ => None,
    };
    let Some(newest) = reviews.iter().filter_map(round).max() else {
        return reviews;
    };
    let first = reviews
        .iter()
        .position(|event| round(event) == Some(newest))
        .unwrap_or_default();
    &reviews[first..]
}

/// The verdict that stands: the judge's ruling, else the latest reviewer's.
pub fn last_verdict(reviews: &[ReviewEvent]) -> Option<ReviewVerdict> {
    reviews.iter().rev().find_map(|event| match event {
        ReviewEvent::Reviewed { review, .. } => Some(review.verdict),
        ReviewEvent::Settled { ruling, .. } => Some(ruling.verdict),
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
    let checks = if row.checks_passed {
        "checks passed"
    } else {
        "checks failed"
    };
    let mut line = format!("{}: {checks}, {review}", row.task);
    for debt in &row.debt {
        line.push_str(&format!(
            ", needs review by {} (work by {} since fallback-{})",
            debt.needs, debt.by, debt.since
        ));
    }
    for question in &row.questions {
        line.push_str(&format!(", asked you \"{}\"", one_line(&question.text)));
        match (&question.answer, &question.answered_by) {
            (Some(answer), Some(by)) => {
                line.push_str(&format!(
                    " ({by} answered in your place: {})",
                    one_line(answer)
                ));
            }
            (Some(answer), None) => {
                line.push_str(&format!(" (answered in your place: {})", one_line(answer)));
            }
            (None, _) => line.push_str(" (still open)"),
        }
    }
    line
}

/// An agent was stopped before it finished, so some work may be partial.
fn stopped_early(state: &RunState) -> bool {
    state.sessions.by_key.values().any(|session| {
        matches!(
            session.end.as_ref().map(|end| &end.reason),
            Some(EndReason::Stopped | EndReason::LimitHit { .. })
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

    fn check(name: &str, passed: bool) -> CheckOutcome {
        CheckOutcome {
            name: name.to_owned(),
            kind: CheckKind::Command,
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

    /// An approval counts from another provider than the worker's, or from a reviewer or a
    /// judge you chose whatever its provider; one code picked from the worker's own provider
    /// keeps the merge back, saying who gave it and whether as a fallback.
    #[test]
    fn an_approval_counts_from_another_provider_or_from_one_you_chose() {
        use argonauts_core::steps::judgment::{ReviewOutput, SettleOutput};

        let provider = |model: &ModelRef| {
            let maker = match model.agent.as_str() {
                "claude-code" => "anthropic",
                "codex" => "openai",
                _ => "google",
            };
            Some(maker.to_owned())
        };
        let worker: ModelRef = "claude-code/haiku-5.5".parse().unwrap();
        let sonnet: ModelRef = "claude-code/sonnet-5".parse().unwrap();
        let reviewed = |by: &ModelRef, verdict, picked_by| ReviewEvent::Reviewed {
            reviewer: by.clone(),
            blind: false,
            round: 1,
            review: ReviewOutput {
                verdict,
                summary: String::new(),
                comments: Vec::new(),
                tamper_suspected: false,
            },
            picked_by,
        };
        let approves = |by: &ModelRef, picked_by| reviewed(by, ReviewVerdict::Approve, picked_by);
        let none = |_| false;
        let of = |reviews: &[ReviewEvent], fallback: &dyn Fn(usize) -> bool| {
            approval(reviews, Some(&worker), &provider, fallback)
        };
        let codex: ModelRef = "codex".parse().unwrap();
        assert_eq!(
            of(&[approves(&codex, Some(PickedBy::Code))], &none),
            Approval::Counts
        );
        let haiku_4: ModelRef = "claude-code/haiku-4.5".parse().unwrap();
        assert_eq!(
            of(&[approves(&haiku_4, Some(PickedBy::You))], &none),
            Approval::Counts
        );
        assert_eq!(
            of(&[approves(&sonnet, Some(PickedBy::Code))], &|_| true),
            Approval::WithinProvider {
                reviewer: sonnet.clone(),
                fallback: true,
            }
        );
        // Older records say nobody chose, so only another provider counts.
        assert_eq!(
            of(&[approves(&sonnet, None)], &none),
            Approval::WithinProvider {
                reviewer: sonnet.clone(),
                fallback: false,
            }
        );
        // A judge you named settles it as a reviewer you chose would.
        let opus: ModelRef = "claude-code/opus-5.5".parse().unwrap();
        let settled = ReviewEvent::Settled {
            judge: opus,
            ruling: SettleOutput {
                verdict: ReviewVerdict::Approve,
                reasons: Vec::new(),
                required_changes: Vec::new(),
            },
            picked_by: Some(PickedBy::You),
        };
        let disagreed = [
            approves(&sonnet, Some(PickedBy::Code)),
            reviewed(
                &haiku_4,
                ReviewVerdict::RequestChanges,
                Some(PickedBy::Code),
            ),
            settled,
        ];
        assert_eq!(of(&disagreed, &none), Approval::Counts);
        // Work its own provider didn't approve, or by a worker of no known provider, is
        // neither: whether it was approved is the reviews' own fact.
        let asked = [reviewed(
            &sonnet,
            ReviewVerdict::RequestChanges,
            Some(PickedBy::Code),
        )];
        assert_eq!(of(&asked, &none), Approval::Neither);
        let unknown = |_: &ModelRef| None;
        let reviews = [approves(&codex, Some(PickedBy::Code))];
        assert_eq!(
            approval(&reviews, Some(&worker), &unknown, &none),
            Approval::Neither
        );
    }

    #[test]
    fn only_each_checks_latest_outcome_counts() {
        assert!(checks_passed(&[]));
        assert!(checks_passed(&[
            check("tests", false),
            check("tests", true)
        ]));
        assert!(!checks_passed(&[
            check("tests", true),
            check("lint", false)
        ]));
        assert!(!checks_passed(&[
            check("tests", true),
            check("tests", false)
        ]));
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
