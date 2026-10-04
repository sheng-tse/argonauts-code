//! The closing answer, the last thing a run says in the conversation, the way an agent's own
//! CLI ends a task: what changed, how it was checked, and anything left. A solo run's words are
//! its owner's own final answer, from the card that landed. A crew run's lead writes them from
//! the cards' reports in one short step without tools. Code writes them when no agent can, as
//! when a run fails. Only cards that landed are what changed; the others are what's left.

use std::collections::{BTreeMap, BTreeSet};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{CardState, GateOutcome};
use argonauts_core::events::checkpoint::CheckpointPacket;
use argonauts_core::events::run::{
    ChangedFile, CheckLine, ClosingAnswer, RunLifecycle, RunMode, RunOutcome,
};
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::steps::card::{CardDigest, CloseRun, CloseRunInput};
use argonauts_core::worker::Access;

use crate::engine::host::{Ask, Host};
use crate::error::{EngineError, EngineResult};
use crate::prompts;
use crate::state::RunState;
use crate::state::cards::CardRecord;

/// The run's own records, which every run commits and nobody asked for.
const OWN_RECORDS: &str = ".argonauts/";
/// What the lead and the checks list are given at most, so the step stays short.
const MOST_TESTS: usize = 8;
const MOST_LEFT: usize = 6;

/// Writes the closing answer once the run's result is settled, after the merge, the retro, and
/// any wait to merge. Only an interruption unwinds, so the answer is written again on resume.
pub async fn record(host: &Host, outcome: RunOutcome, detail: Option<&str>) -> EngineResult<()> {
    write(host, outcome, detail, true).await
}

/// For a run that ends without finishing its workflow, as a stop or a failure does: nobody is
/// asked, so the owner's last report or code's own words close it. Nothing when the run already
/// has its answer.
pub async fn record_unasked(
    host: &Host,
    outcome: RunOutcome,
    detail: Option<&str>,
) -> EngineResult<()> {
    write(host, outcome, detail, false).await
}

async fn write(
    host: &Host,
    outcome: RunOutcome,
    detail: Option<&str>,
    ask_lead: bool,
) -> EngineResult<()> {
    if host.read(|state| state.run.closed) {
        return Ok(());
    }
    let mut answer = gather(host, outcome, detail).await;
    match host.read(|state| state.run.mode) {
        Some(RunMode::Crew) if !ask_lead => {}
        Some(RunMode::Crew) => match lead_answer(host, outcome, detail, &answer).await {
            Ok(Some((text, by))) => {
                answer.text = text;
                answer.by = Some(by);
            }
            Ok(None) => {}
            Err(EngineError::Interrupted) => return Err(EngineError::Interrupted),
            // The answer code wrote stands.
            Err(_) => {}
        },
        _ => {
            if let Some((text, by)) = host.read(owner_answer) {
                answer.text = text;
                answer.by = by;
            }
        }
    }
    host.record(
        None,
        None,
        RunEventKind::Run(RunLifecycle::ClosingAnswer(Box::new(answer))),
    )?;
    Ok(())
}

/// Code's own answer, with the files, checks, and what's left filled in.
async fn gather(host: &Host, outcome: RunOutcome, detail: Option<&str>) -> ClosingAnswer {
    let (cards, landed, gates, diff_path) = host.read(|state| {
        let cards: Vec<CardRecord> = state.cards.by_id.values().cloned().collect();
        let (gates, diff_path) = match latest_merge(state) {
            Some((gates, diff_path)) => (gates, Some(diff_path)),
            None => (cards_gates(&cards), None),
        };
        (cards, landed(state), gates, diff_path)
    });
    let files = match diff_path {
        Some(path) => tokio::fs::read_to_string(&path)
            .await
            .map(|diff| files_in_diff(&diff))
            .unwrap_or_default(),
        None => {
            let delivered: Vec<CardRecord> = cards
                .iter()
                .filter(|card| landed.contains(&card.spec.id))
                .cloned()
                .collect();
            card_files(host, &delivered).await
        }
    };
    answer_from(&cards, &landed, &gates, files, outcome, detail)
}

/// Code's answer: what the landed cards reported is what changed, and every other card is
/// left, with how it ended.
fn answer_from(
    cards: &[CardRecord],
    landed: &BTreeSet<CardId>,
    gates: &[GateOutcome],
    files: Vec<ChangedFile>,
    outcome: RunOutcome,
    detail: Option<&str>,
) -> ClosingAnswer {
    let reports: Vec<_> = cards
        .iter()
        .filter(|card| landed.contains(&card.spec.id))
        .filter_map(|card| card.reports.last())
        .collect();
    let mut tests: Vec<String> = Vec::new();
    let mut left: Vec<String> = cards
        .iter()
        .filter(|card| !landed.contains(&card.spec.id))
        .filter_map(|card| {
            not_landed(card, landed).map(|how| format!("{} · {how}", card.spec.title.trim()))
        })
        .collect();
    for report in &reports {
        for test in &report.tests_run {
            if !tests.contains(test) {
                tests.push(test.clone());
            }
        }
        for concern in &report.concerns {
            if !left.contains(concern) {
                left.push(concern.clone());
            }
        }
    }
    if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
        left.insert(0, detail.to_owned());
    }
    tests.truncate(MOST_TESTS);
    left.truncate(MOST_LEFT);
    let summaries: Vec<&str> = reports
        .iter()
        .map(|report| report.summary.trim())
        .filter(|summary| !summary.is_empty())
        .collect();
    let text = if summaries.is_empty() {
        ending_sentence(outcome)
    } else {
        summaries.join(" ")
    };
    ClosingAnswer {
        text,
        by: None,
        files,
        checks: gates
            .iter()
            .filter(|gate| !gate.skipped)
            .map(|gate| CheckLine {
                what: gate.name.clone(),
                passed: gate.passed,
                result: gate.summary.trim().to_owned(),
            })
            .collect(),
        tests,
        left,
    }
}

/// The cards whose work the run delivers: the ones the newest merge card names, else the ones
/// integrated. Best-of-n's losing attempts, and cards that failed or were cancelled, aren't.
fn landed(state: &RunState) -> BTreeSet<CardId> {
    let named: BTreeSet<CardId> = state
        .checkpoints
        .by_id
        .values()
        .filter_map(|record| match &record.opened.packet {
            CheckpointPacket::Merge(packet) => Some((record.since, packet)),
            _ => None,
        })
        .max_by_key(|(since, _)| *since)
        .map(|(_, packet)| packet.rows.iter().map(|row| row.card.clone()).collect())
        .unwrap_or_default();
    if !named.is_empty() {
        return named;
    }
    state
        .cards
        .by_id
        .values()
        .filter(|record| record.state == CardState::Integrated)
        .map(|record| record.spec.id.clone())
        .collect()
}

/// How a card that didn't land ended, for what's left. `None` for one another took over.
fn not_landed(card: &CardRecord, landed: &BTreeSet<CardId>) -> Option<&'static str> {
    // Best of n keeps one attempt; the others stay on their branches.
    let attempt_of = |id: &CardId| {
        id.as_str()
            .split_once("-attempt-")
            .map(|(card, _)| card.to_owned())
    };
    if let Some(card_id) = attempt_of(&card.spec.id)
        && landed
            .iter()
            .any(|kept| attempt_of(kept).as_deref() == Some(card_id.as_str()))
    {
        return Some("not kept, left on its branch");
    }
    Some(match card.state {
        CardState::Failed => "failed",
        CardState::Cancelled => "dropped",
        CardState::Escalated | CardState::NeedsDecision => "waits for a decision",
        CardState::Blocked => "blocked on a question",
        CardState::Deferred => "waits for budget",
        CardState::Integrated => "not merged",
        CardState::Running | CardState::Gating | CardState::InReview | CardState::Integrating => {
            "stopped unfinished"
        }
        CardState::Waiting | CardState::Ready => "never started",
        CardState::TakenOver => return None,
    })
}

/// The words a card's state has for the lead: `landed`, else how it ended.
fn digest_state(card: &CardRecord, landed: &BTreeSet<CardId>) -> String {
    if landed.contains(&card.spec.id) {
        "landed".to_owned()
    } else {
        not_landed(card, landed).unwrap_or("taken over").to_owned()
    }
}

/// What the lead is told of each card, the landed ones first.
fn digests(state: &RunState) -> Vec<CardDigest> {
    let landed = landed(state);
    let mut records: Vec<&CardRecord> = state.cards.by_id.values().collect();
    records.sort_by_key(|record| !landed.contains(&record.spec.id));
    records
        .into_iter()
        .filter(|record| record.state != CardState::TakenOver)
        .map(|record| {
            let report = record.reports.last();
            CardDigest {
                title: record.spec.title.clone(),
                state: digest_state(record, &landed),
                summary: report
                    .map(|report| report.summary.clone())
                    .unwrap_or_default(),
                tests_run: report
                    .map(|report| report.tests_run.clone())
                    .unwrap_or_default(),
                concerns: report
                    .map(|report| report.concerns.clone())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

/// A solo run's owner already said it all in its last report: its answer, else its summary.
/// The card that landed speaks, never a losing attempt; with none landed, the last to report.
fn owner_answer(state: &RunState) -> Option<(String, Option<WorkerRef>)> {
    let landed = landed(state);
    let reported = |record: &&CardRecord| !record.reports.is_empty();
    let record = state
        .cards
        .by_id
        .values()
        .filter(reported)
        .find(|record| landed.contains(&record.spec.id))
        .or_else(|| {
            landed
                .is_empty()
                .then(|| state.cards.by_id.values().rev().find(reported))
                .flatten()
        })?;
    let report = record.reports.last()?;
    let text = [&report.answer, &report.summary]
        .into_iter()
        .map(|text| text.trim())
        .find(|text| !text.is_empty())?;
    let by = record.owner.clone().or_else(|| record.spec.owner.clone());
    Some((text.to_owned(), by))
}

/// The lead, the agent you chat with, writes a crew run's answer from what code gathered.
/// `None` when no card reported, so there's nothing for it to say beyond code's words.
async fn lead_answer(
    host: &Host,
    outcome: RunOutcome,
    detail: Option<&str>,
    gathered: &ClosingAnswer,
) -> EngineResult<Option<(String, WorkerRef)>> {
    let (request, cards) = host.read(|state| {
        let request = state
            .run
            .started
            .as_ref()
            .map(|started| started.request.clone())
            .unwrap_or_default();
        (request, digests(state))
    });
    if cards.iter().all(|card| card.summary.trim().is_empty()) {
        return Ok(None);
    }
    let mut ending = ending_words(outcome).to_owned();
    if let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) {
        ending.push_str(": ");
        ending.push_str(detail.trim());
    }
    let input = CloseRunInput {
        request,
        ending,
        cards,
        files: gathered
            .files
            .iter()
            .map(|file| format!("{} +{} -{}", file.path, file.insertions, file.deletions))
            .collect(),
        checks: gathered
            .checks
            .iter()
            .map(|check| {
                let verdict = if check.passed { "passed" } else { "failed" };
                match check.result.as_str() {
                    "" => format!("{}: {verdict}", check.what),
                    result => format!("{}: {verdict}, {result}", check.what),
                }
            })
            .collect(),
    };
    let worker = host.settings().default_agent.clone();
    let mut ask = Ask::new(
        None,
        RoleName::Default,
        worker,
        prompts::divide::CLOSE_RUN,
        input,
    );
    ask.access = Access::NoTools;
    let answered = host.ask::<CloseRun>(ask).await?;
    let text = answered.output.answer.trim().to_owned();
    Ok((!text.is_empty()).then_some((text, answered.worker)))
}

/// The merge card's checks and where its diff was written, from the newest one put up.
fn latest_merge(state: &RunState) -> Option<(Vec<GateOutcome>, std::path::PathBuf)> {
    state
        .checkpoints
        .by_id
        .values()
        .filter_map(|record| match &record.opened.packet {
            CheckpointPacket::Merge(packet) => Some((record.since, packet)),
            _ => None,
        })
        .max_by_key(|(since, _)| *since)
        .map(|(_, packet)| (packet.gates.clone(), packet.diff_path.clone()))
}

/// Each gate's latest result over every card, for a run that never reached the merge.
fn cards_gates(cards: &[CardRecord]) -> Vec<GateOutcome> {
    let mut latest: BTreeMap<String, GateOutcome> = BTreeMap::new();
    for gate in cards.iter().flat_map(|card| &card.gates) {
        latest.insert(gate.name.clone(), gate.clone());
    }
    latest.into_values().collect()
}

/// What the cards changed on their branches, for a run that never reached the merge.
async fn card_files(host: &Host, cards: &[CardRecord]) -> Vec<ChangedFile> {
    let root = host.repo().root.clone();
    let mut files: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for card in cards {
        let (Some(base), Some(branch)) = (&card.base, &card.branch) else {
            continue;
        };
        let Ok(changed) = host.git().diff_files(&root, base, branch).await else {
            continue;
        };
        for (path, added, removed) in changed {
            let counts = files.entry(path).or_default();
            counts.0 += added;
            counts.1 += removed;
        }
    }
    files
        .into_iter()
        .filter(|(path, _)| !path.starts_with(OWN_RECORDS))
        .map(|(path, (insertions, deletions))| ChangedFile {
            path,
            insertions,
            deletions,
        })
        .collect()
}

/// Each file of a unified diff, as `git diff` writes it, with its lines added and removed. The
/// run's own records are left out.
fn files_in_diff(diff: &str) -> Vec<ChangedFile> {
    let mut files: Vec<ChangedFile> = Vec::new();
    let mut in_hunk = false;
    for line in diff.lines() {
        if let Some(names) = line.strip_prefix("diff --git ") {
            in_hunk = false;
            // `a/<path> b/<path>`; the b side names a renamed or new file as it is now.
            let path = names.rsplit_once(" b/").map_or(names, |(_, path)| path);
            files.push(ChangedFile {
                path: path.to_owned(),
                insertions: 0,
                deletions: 0,
            });
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk && let Some(file) = files.last_mut() {
            if line.starts_with('+') {
                file.insertions += 1;
            } else if line.starts_with('-') {
                file.deletions += 1;
            }
        }
    }
    files.retain(|file| !file.path.starts_with(OWN_RECORDS));
    files
}

/// For the lead: how the run ended.
fn ending_words(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Merged => "merged into your branch",
        RunOutcome::PullRequest => "waiting in a pull request",
        RunOutcome::BranchLeft => "left on its branches, not merged",
        RunOutcome::Abandoned => "abandoned at the merge, nothing merged",
        RunOutcome::Stopped => "stopped before it finished",
        RunOutcome::Failed => "failed",
    }
}

/// Code's words when no agent reported anything.
fn ending_sentence(outcome: RunOutcome) -> String {
    match outcome {
        RunOutcome::Merged => "The run merged.",
        RunOutcome::PullRequest => "The run's work waits in a pull request.",
        RunOutcome::BranchLeft => "The run's work is on its branches, not merged.",
        RunOutcome::Abandoned => "The run was abandoned, so nothing merged.",
        RunOutcome::Stopped => "The run stopped before any agent reported.",
        RunOutcome::Failed => "The run failed before any agent reported.",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::steps::card::{CardReport, ReportStatus};

    fn card(id: &str, title: &str, state: CardState, said: &str) -> CardRecord {
        let mut spec = crate::testutil::card_spec(id, &[], Some("claude-code/opus"));
        spec.title = title.to_owned();
        let mut record = CardRecord::new(spec);
        record.state = state;
        record.reports.push(CardReport {
            status: ReportStatus::Done,
            summary: format!("{said} summary"),
            files_changed: Vec::new(),
            tests_run: vec![format!("{said} tests")],
            concerns: vec![format!("{said} concern")],
            questions: Vec::new(),
            change_requests: Vec::new(),
            memory_proposals: Vec::new(),
            answer: format!("{said} answer"),
        });
        record
    }

    fn state_of(cards: Vec<CardRecord>) -> RunState {
        let mut state = RunState::default();
        for record in cards {
            state.cards.by_id.insert(record.spec.id.clone(), record);
        }
        state
    }

    #[test]
    fn only_a_card_that_landed_is_what_changed() {
        // A crew run whose second card was dropped: its claims are left, never what changed.
        let cards = vec![
            card("card-001", "Add the flag", CardState::Integrated, "FLAG"),
            card("card-002", "Write the docs", CardState::Cancelled, "DOCS"),
        ];
        let state = state_of(cards.clone());
        let landed = landed(&state);
        let answer = answer_from(&cards, &landed, &[], Vec::new(), RunOutcome::Merged, None);
        assert_eq!(answer.text, "FLAG summary");
        assert_eq!(answer.tests, ["FLAG tests"]);
        assert_eq!(answer.left, ["Write the docs · dropped", "FLAG concern"]);
        let told: Vec<(String, String)> = digests(&state)
            .into_iter()
            .map(|digest| (digest.title, digest.state))
            .collect();
        assert_eq!(
            told,
            [
                ("Add the flag".to_owned(), "landed".to_owned()),
                ("Write the docs".to_owned(), "dropped".to_owned())
            ]
        );
    }

    #[test]
    fn best_of_n_speaks_for_the_attempt_it_kept() {
        let mut state = state_of(vec![
            card(
                "card-001-attempt-1",
                "Fix (attempt 1)",
                CardState::Integrated,
                "FIRST",
            ),
            card(
                "card-001-attempt-2",
                "Fix (attempt 2)",
                CardState::Gating,
                "SECOND",
            ),
        ]);
        let (text, _) = owner_answer(&state).unwrap();
        assert_eq!(text, "FIRST answer");
        let cards: Vec<CardRecord> = state.cards.by_id.values().cloned().collect();
        let answer = answer_from(
            &cards,
            &landed(&state),
            &[],
            Vec::new(),
            RunOutcome::Merged,
            None,
        );
        assert_eq!(
            answer.left[0],
            "Fix (attempt 2) · not kept, left on its branch"
        );
        // With nothing landed, as in a failed run, the last to report still speaks.
        for record in state.cards.by_id.values_mut() {
            record.state = CardState::Failed;
        }
        assert_eq!(owner_answer(&state).unwrap().0, "SECOND answer");
    }

    #[test]
    fn a_diff_gives_each_file_its_lines_without_the_runs_own_records() {
        let diff = "\
diff --git a/invoice/money.py b/invoice/money.py
index 1111111..2222222 100644
--- a/invoice/money.py
+++ b/invoice/money.py
@@ -1,4 +1,5 @@
-def parse(text):
+def parse(text: str) -> int:
+    text = text.strip()
--- a line that starts with dashes
 unchanged
diff --git a/.argonauts/goal/run-1.md b/.argonauts/goal/run-1.md
new file mode 100644
--- /dev/null
+++ b/.argonauts/goal/run-1.md
@@ -0,0 +1 @@
+The goal.
diff --git a/my notes.md b/my notes.md
--- a/my notes.md
+++ b/my notes.md
@@ -3 +3 @@
-old
+new
";
        let files = files_in_diff(diff);
        assert_eq!(
            files,
            vec![
                ChangedFile {
                    path: "invoice/money.py".into(),
                    insertions: 2,
                    deletions: 2,
                },
                ChangedFile {
                    path: "my notes.md".into(),
                    insertions: 1,
                    deletions: 1,
                },
            ]
        );
    }

    #[test]
    fn every_ending_has_words_for_the_lead_and_for_code() {
        for outcome in [
            RunOutcome::Merged,
            RunOutcome::PullRequest,
            RunOutcome::BranchLeft,
            RunOutcome::Abandoned,
            RunOutcome::Stopped,
            RunOutcome::Failed,
        ] {
            assert!(!ending_words(outcome).is_empty());
            assert!(ending_sentence(outcome).ends_with('.'));
        }
    }
}
