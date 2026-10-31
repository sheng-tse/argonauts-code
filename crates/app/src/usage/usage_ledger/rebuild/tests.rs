use std::path::Path;

use argonauts_core::Paths;
use argonauts_core::behaviors::crew_stats::StatsRole;
use argonauts_core::runs::journal::read_all;
use time::macros::date;

use super::*;

/// A Captain run's log, and the two ledgers' lines of September as they were filed before each
/// session kept its own model: its Codex session and the task it built under Claude Code's
/// model, the judge's verdict as a review, and the alias `haiku` in the usage ledger. Beside
/// them a conversation turn on `haiku`, and a run whose log is gone.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/rebuild");
const RUN: &str = "run-20260930-101500-00ab";
const GONE: &str = "run-20260929-090000-0918";

/// A home with the fixtures in place: the run's log under `state`, the ledgers under `usage`.
fn home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let run = home.path().join("state/repo-id/runs").join(RUN);
    fs::create_dir_all(&run).unwrap();
    fs::copy(
        Path::new(FIXTURES).join("events.jsonl"),
        run.join("events.jsonl"),
    )
    .unwrap();
    let usage = home.path().join("usage");
    fs::create_dir_all(usage.join("work")).unwrap();
    let fixture = |name: &str| Path::new(FIXTURES).join(name);
    fs::copy(fixture("usage.jsonl"), usage.join("2026-09.jsonl")).unwrap();
    fs::copy(fixture("work.jsonl"), usage.join("work/2026-09.jsonl")).unwrap();
    home
}

fn texts(home: &Path) -> (String, String) {
    let read = |path: &str| fs::read_to_string(home.join(path)).unwrap();
    (
        read("usage/2026-09.jsonl"),
        read("usage/work/2026-09.jsonl"),
    )
}

fn model(agent: &str, model: Option<&str>) -> ModelRef {
    ModelRef::new(agent, model.map(str::to_owned))
}

/// The run is filed again from its log as a run's end files it now, the rest of the lines under
/// the catalog's spelling, and a rebuild done once is never done again.
#[test]
fn past_records_are_filed_again_from_each_runs_log_once() {
    let home = home();
    let paths = Paths::new(home.path());
    let ledger = UsageLedger::new(&paths);
    let state = home.path().join("state");
    ledger.rebuild_once().unwrap();
    assert!(home.path().join("usage").join(DONE).exists());
    assert!(!home.path().join("usage").join(BUSY).exists());

    let run = RunId::new(RUN).unwrap();
    let events = read_all(&state.join("repo-id/runs").join(RUN).join("events.jsonl"))
        .unwrap()
        .events;
    let at = ended_at(&events).unwrap();
    // The day and the hour the ledgers filed the run under stay.
    let (day, hour) = (date!(2026 - 09 - 30), Some(6));
    let work = WorkLedger::beside(&paths.usage_dir());
    let fresh = work.lines_of(Path::new("/repo"), &run, &events, day);
    let filed: Vec<WorkLine> = (work.read_since(None).into_iter())
        .filter(|line| line.run == run)
        .collect();
    assert_eq!(filed, fresh);
    assert!(
        filed
            .iter()
            .any(|line| matches!(&line.what, Work::Signoff { by, .. } if by.model.as_deref() == Some("gpt-6-luna")))
    );
    let fresh: Vec<LedgerLine> = (ledger.lines_of(Path::new("/repo"), &run, &events, at))
        .into_iter()
        .map(|line| LedgerLine { day, hour, ..line })
        .collect();
    let usage = ledger.read_since(None);
    let ran: Vec<&LedgerLine> = usage
        .iter()
        .filter(|line| line.run.as_ref() == Some(&run))
        .collect();
    assert_eq!(ran, fresh.iter().collect::<Vec<_>>());

    // The turn and the run whose log is gone keep their lines, under the catalog's spelling.
    let haiku = Some("claude-haiku-5-5".to_owned());
    let turn = usage.iter().find(|line| line.run.is_none()).unwrap();
    assert_eq!((turn.model.clone(), turn.took), (haiku.clone(), 12));
    let gone = RunId::new(GONE).unwrap();
    let kept = usage
        .iter()
        .find(|line| line.run.as_ref() == Some(&gone))
        .unwrap();
    assert_eq!(kept.model, haiku);
    let planned = work
        .read_since(None)
        .into_iter()
        .find_map(|line| match line.what {
            Work::Run { by, .. } if line.run == gone => by,
            _ => None,
        });
    assert_eq!(
        planned,
        Some(model("claude-code", Some("claude-haiku-5-5")))
    );

    // Stats has one Haiku 5.5, and the judge's verdict is its sign-off.
    let stats = crate::usage::work_ledger::stats(&work, Some(date!(2026 - 09 - 01)), &usage);
    let claude: BTreeSet<Option<&str>> = (stats.work.iter())
        .filter(|row| row.agent == "claude-code")
        .map(|row| row.model.as_deref())
        .collect();
    assert_eq!(claude, BTreeSet::from([Some("claude-haiku-5-5")]));
    let judged = stats
        .work
        .iter()
        .find(|row| row.role == StatsRole::Judge && row.done > 0);
    assert_eq!(judged.map(|row| row.agent.as_str()), Some("codex"));

    // Once is once: the files stay as they are, and a rebuild run again changes nothing.
    let once = texts(home.path());
    ledger.rebuild_once().unwrap();
    assert_eq!(texts(home.path()), once);
    ledger.rebuild().unwrap();
    assert_eq!(texts(home.path()), once);
}

/// With no ledger there's nothing to rebuild and nothing is written; while another process
/// rebuilds, this one leaves the files alone.
#[test]
fn no_ledger_or_a_rebuild_going_on_writes_nothing() {
    let home = tempfile::tempdir().unwrap();
    let ledger = UsageLedger::new(&Paths::new(home.path()));
    ledger.rebuild_once().unwrap();
    assert!(!home.path().join("usage").exists());

    let home = self::home();
    let ledger = UsageLedger::new(&Paths::new(home.path()));
    let before = texts(home.path());
    let busy = Busy::take(&home.path().join("usage")).unwrap();
    assert!(busy.is_some());
    ledger.rebuild_once().unwrap();
    assert_eq!(texts(home.path()), before);
    assert!(!home.path().join("usage").join(DONE).exists());
    drop(busy);
    ledger.rebuild_once().unwrap();
    assert!(home.path().join("usage").join(DONE).exists());
}

/// A line that doesn't read is kept as it was, where it was.
#[test]
fn a_line_that_doesnt_read_is_kept() {
    let home = home();
    let month = home.path().join("usage/2026-09.jsonl");
    let mut text = fs::read_to_string(&month).unwrap();
    text.insert_str(0, "{not a line\n");
    fs::write(&month, text).unwrap();
    let ledger = UsageLedger::new(&Paths::new(home.path()));
    ledger.rebuild().unwrap();
    let rebuilt = fs::read_to_string(&month).unwrap();
    assert!(rebuilt.starts_with("{not a line\n"), "{rebuilt}");
}
