//! The screens of a folder without git and of old runs, drawn from their pictures' own
//! examples at the pictures' width and compared with them character for character. The
//! pictures kept in `golden/` hold the rows drawn here and nothing else, but for the Merge
//! phase's panel, which the workflow view draws with the words
//! checked here.

use crate::common;
use std::path::PathBuf;

use argonauts_core::events::approval::{
    AnswerAction, AnswerOption, ApprovalKind, ApprovalOpened, ApprovalPacket, BothChanged,
    FreeText, MergePacket, MergeRow,
};
use argonauts_core::events::task::TaskKind;
use argonauts_core::names::ApprovalId;
use argonauts_core::settings::approvals::{ApprovalMode, DecideWith};
use argonauts_core::settings::asking::WhenDeclined;
use argonauts_core::steps::judgment::ReviewVerdict;
use argonauts_core::{DiffStat, Spend, TaskId};
use argonauts_tui::HistoryCell;
use argonauts_tui::cell::{Notice, Tone};
use argonauts_tui::questions::approval_question;
use common::{row_text, still, theme};
use ratatui::text::Line;

const WIDTH: u16 = 118;
const NO_GIT: &str = include_str!("../golden/no_git_fixes.txt");
const OLD_RUNS: &str = include_str!("../golden/old_runs.txt");

/// The `count` rows of `picture` from the one that starts with `first`.
fn rows<'a>(picture: &'a str, first: &str, count: usize) -> Vec<&'a str> {
    let all: Vec<&str> = picture.lines().collect();
    let at = all
        .iter()
        .position(|row| row.starts_with(first))
        .unwrap_or_else(|| panic!("the picture has no row starting with {first:?}"));
    all[at..at + count].to_vec()
}

fn drawn(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| row_text(line).trim_end().to_owned())
        .collect()
}

fn stat(files: u32, added: u32, removed: u32) -> DiffStat {
    DiffStat {
        files_changed: files,
        insertions: added,
        deletions: removed,
    }
}

#[test]
fn the_first_note_names_what_is_left_out() {
    let theme = theme();
    let note = Notice::new(
        Tone::Normal,
        "This folder has no git, so this workflow keeps its history under ~/.argonauts. Your \
         folder gets no .git, and the finished work is written back into your files. /git \
         changes that.",
    )
    .with_detail("Left out of what the agents see, and left alone in your folder:")
    .with_detail("secrets         .env              also .env.*, *.pem, *.key")
    .with_detail("packages        node_modules/     also .venv/")
    .with_detail(
        "build output    none here         __pycache__/, .next/, .gradle/ and the rest of the \
         built-in list",
    )
    .with_detail("history.leave_out adds to the list or takes from it");
    let drawn = drawn(&note.lines(&still(&theme, WIDTH)));
    let picture = rows(NO_GIT, "● This folder has no git", 7);
    // The picture breaks the note's own line four cells early, before "the", where the
    // screen fits the word on its first row. The words are the same, and every row under
    // them is the picture's.
    let unwrapped = |rows: &[&str]| format!("{} {}", rows[0], rows[1].trim_start());
    assert_eq!(unwrapped(&[&drawn[0], &drawn[1]]), unwrapped(&picture[..2]));
    assert_eq!(drawn[2..], picture[2..]);
}

/// The merge packet of the pictures' run: two tasks, four files, everything approved.
fn packet() -> MergePacket {
    let row = |n: u32, title: &str| MergeRow {
        task: TaskId::new(format!("task-{n:03}")).unwrap(),
        title: title.into(),
        kind: TaskKind::Build,
        stat: stat(2, 26, 3),
        worker: None,
        checks_passed: true,
        review: Some(ReviewVerdict::Approve),
        debt: Vec::new(),
        questions: Vec::new(),
    };
    MergePacket {
        target: Some(argonauts_core::hidden_git::BRANCH.into()),
        branch: "argonauts/run-20261010-091503-3fa2/merged".into(),
        rows: vec![row(1, "Parse --due"), row(2, "Show due dates")],
        suite_passed_on_latest_main: true,
        checks: Vec::new(),
        combined: stat(4, 52, 6),
        conductor: stat(0, 0, 0),
        diff_path: PathBuf::from("merge.diff"),
        tree_sha: "4b825dc".into(),
        spent: Spend::default(),
        limit: None,
        partial: false,
        in_the_way: Vec::new(),
        edited: Vec::new(),
        refused: None,
        both_changed: Vec::new(),
        asks_which_branch: false,
        again: None,
    }
}

fn option(key: char, label: &str, action: AnswerAction, detail: Option<&str>) -> AnswerOption {
    AnswerOption {
        key,
        label: label.into(),
        action,
        recommended: false,
        detail: detail.map(str::to_owned),
    }
}

fn opened(title: &str, packet: MergePacket, options: Vec<AnswerOption>) -> ApprovalOpened {
    ApprovalOpened {
        id: ApprovalId::numbered(1),
        kind: ApprovalKind::Merge,
        title: title.into(),
        task: None,
        packet: ApprovalPacket::Merge(packet),
        evidence_sha256: "e3b0c442".into(),
        mode: ApprovalMode::Supervised,
        decide_with: DecideWith::Rules,
        when_declined: WhenDeclined::Decide,
        options,
        free_text: FreeText::Guidance,
        needs_you: true,
    }
}

/// The question as it's drawn while it holds the keys.
fn question(opened: &ApprovalOpened) -> Vec<String> {
    let theme = theme();
    let mut question = approval_question(1, opened, None);
    question.focus();
    drawn(&question.lines(&still(&theme, WIDTH)))
}

/// How the picture's keys row ends, and how the screen's does: Esc on a run's question goes
/// back to the list everywhere, the question waiting, and Shift+Tab changes the mode from a
/// question too. The end of this row is the one place these questions are unlike the picture.
const PICTURED_ESC: &str = "Esc to set aside";
const ESC: &str = "Esc back to the list · shift+tab to cycle modes";

/// What Carry on without me says on a question that ends a run's part, under its row.
const CARRIES_ON: &str = "This part ends safely with its work kept, so nothing merges without you";

/// The question's rows against the picture's, cell for cell, but for what Esc is called on
/// the keys row, and the question's last option, Carry on without me, which the picture
/// predates, after the picture's own.
fn assert_question(drawn: &[String], picture: &[&str]) {
    let keys = picture.len() - 1;
    let options = picture
        .iter()
        .filter(|row| {
            let row = row.trim_start().trim_start_matches("❯ ");
            row.split_once(". ")
                .is_some_and(|(number, _)| number.parse::<usize>().is_ok())
        })
        .count();
    let mut wanted: Vec<String> = picture[..keys - 1]
        .iter()
        .map(|row| (*row).to_owned())
        .collect();
    let label = format!("    {}. Carry on without me", options + 1);
    // Where the picture puts each option's words beside it, these go in the same column.
    let beside = wanted.last().and_then(|row| {
        let gap = row.find(". ")? + 2;
        let words = row[gap..].find("  ")? + gap;
        let start = words + row[words..].len() - row[words..].trim_start().len();
        Some(row[..start].chars().count())
    });
    match beside {
        Some(column) => wanted.push(format!("{label:<column$}{CARRIES_ON}")),
        None => {
            wanted.push(label);
            wanted.push(format!("       {CARRIES_ON}"));
        }
    }
    wanted.push(String::new());
    assert!(picture[keys].ends_with(PICTURED_ESC), "{}", picture[keys]);
    wanted.push(picture[keys].replace(PICTURED_ESC, ESC));
    assert_eq!(drawn, wanted, "{}", drawn.join("\n"));
}

#[test]
fn the_merge_question_offers_only_what_works_without_git() {
    let choose = |key: char, label: &str, action: AnswerAction| option(key, label, action, None);
    let opened = opened(
        "Write this run into your folder?",
        packet(),
        vec![
            choose('y', "Write it into my folder", AnswerAction::Merge),
            choose(
                'b',
                "Request changes on some tasks",
                AnswerAction::RequestChanges,
            ),
            choose('x', "Abandon the run", AnswerAction::Abandon),
            choose('d', "Show the diff", AnswerAction::ShowDiff),
        ],
    );
    let picture = rows(NO_GIT, "◆ Write this run into your folder?", 9);
    let asked = question(&opened);
    // The rule above the question is the bottom area's, drawn at any width.
    let from = asked
        .iter()
        .position(|row| row.starts_with("◆ "))
        .expect("the question's title");
    assert_question(&asked[from..], &picture);
}

#[test]
fn a_file_both_changed_asks_which_version_goes_in() {
    let mut packet = packet();
    packet.both_changed = vec![BothChanged {
        path: "src/cli.js".into(),
        yours: stat(1, 3, 1),
        runs: stat(1, 24, 4),
    }];
    let choose = |key: char, label: &str, detail: &str| {
        let action = AnswerAction::Choose {
            value: label.into(),
        };
        option(key, label, action, Some(detail))
    };
    let opened = opened(
        "You and the run both changed src/cli.js. Which version goes in?",
        packet,
        vec![
            choose(
                'k',
                "Keep my version",
                "your edit stays, and the run's change to src/cli.js is left out",
            ),
            choose(
                't',
                "Take the run's version",
                "the run's src/cli.js goes in; yours stays in the history for /rewind",
            ),
            option(
                'd',
                "Show both",
                AnswerAction::ShowDiff,
                Some("your src/cli.js beside the run's, then these choices again"),
            ),
            choose(
                'l',
                "Merge later",
                "the run pauses here; /resume run-20261010-091503-3fa2 writes it in",
            ),
        ],
    );
    let picture = rows(NO_GIT, "◆ You and the run both changed src/cli.js.", 10);
    let asked = question(&opened);
    let from = asked
        .iter()
        .position(|row| row.starts_with("◆ "))
        .expect("the question's title");
    // Each choice's explanation stands beside its label, three cells after the longest.
    assert_question(&asked[from..], &picture);
}

/// The lines the conversation prints for the merge question and its answer name your folder,
/// never the hidden history's branch, and a question that follows the merge question doesn't
/// print the merge's evidence a second time.
#[test]
fn the_conversation_says_the_run_is_written_into_your_folder() {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::events::approval::ApprovalDecided;
    use argonauts_core::events::run::Party;
    use argonauts_tui::questions::{about_item, record_cell};
    use time::UtcOffset;

    let names = DisplayNames::default();
    let merge = opened("Write this run into your folder?", packet(), Vec::new());
    let about = about_item(&merge, None, UtcOffset::UTC, &names).expect("the merge's evidence");
    let theme = theme();
    let block = drawn(&about.lines(&still(&theme, WIDTH)));
    assert_eq!(
        block[0].trim_start_matches("● "),
        "Write the run into your folder"
    );
    assert!(
        block.iter().any(|row| row.trim() == "Suite passed"),
        "{block:#?}"
    );
    assert!(
        !block.iter().any(|row| row.contains("your-folder")),
        "{block:#?}"
    );
    assert!(!block.iter().any(|row| row.contains("main")), "{block:#?}");

    let approved = ApprovalDecided {
        id: merge.id.clone(),
        action: AnswerAction::Merge,
        text: None,
        by: Party::User,
        evidence_sha256: merge.evidence_sha256.clone(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    let record = drawn(&record_cell(&merge, &approved).lines(&still(&theme, WIDTH)));
    assert!(
        record[0].starts_with("◆ Approved writing the run into your folder · you"),
        "{record:#?}"
    );

    let mut both = packet();
    both.both_changed = vec![BothChanged {
        path: "src/cli.js".into(),
        yours: stat(1, 3, 1),
        runs: stat(1, 24, 4),
    }];
    let follows = opened("You and the run both changed src/cli.js.", both, Vec::new());
    assert!(about_item(&follows, None, UtcOffset::UTC, &names).is_none());
}

/// A run that started on no branch names none until you pick one: its question and its
/// answer say "the run", and the question asking for the branch prints no evidence again.
#[test]
fn a_run_with_no_branch_yet_names_none() {
    use argonauts_core::display_names::DisplayNames;
    use argonauts_core::events::approval::ApprovalDecided;
    use argonauts_core::events::run::Party;
    use argonauts_tui::questions::{about_item, record_cell};
    use time::UtcOffset;

    let names = DisplayNames::default();
    let mut packet = packet();
    packet.target = None;
    let merge = opened("Merge this run?", packet.clone(), Vec::new());
    let about = about_item(&merge, None, UtcOffset::UTC, &names).expect("the merge's evidence");
    let theme = theme();
    let block = drawn(&about.lines(&still(&theme, WIDTH)));
    assert_eq!(block[0].trim_start_matches("● "), "Merge the run");
    assert!(!block.iter().any(|row| row.contains("main")), "{block:#?}");
    let approved = ApprovalDecided {
        id: merge.id.clone(),
        action: AnswerAction::Merge,
        text: None,
        by: Party::Rules,
        evidence_sha256: merge.evidence_sha256.clone(),
        auto_preview: None,
        tasks: Vec::new(),
    };
    let record = drawn(&record_cell(&merge, &approved).lines(&still(&theme, WIDTH)));
    assert!(
        record[0].starts_with("◆ Approved merging the run · your rules"),
        "{record:#?}"
    );

    packet.asks_which_branch = true;
    let which = opened(
        "This checkout isn't on a branch. Which branch takes the run?",
        packet,
        Vec::new(),
    );
    assert!(about_item(&which, None, UtcOffset::UTC, &names).is_none());
    let asked = question(&which);
    assert!(!asked.iter().any(|row| row.contains("main")), "{asked:#?}");
}

#[test]
fn git_clean_says_what_it_freed() {
    let theme = theme();
    let report = Notice::new(
        Tone::Normal,
        "Cleaned this folder's hidden history · 18 MB →  7 MB",
    )
    .with_detail(
        "removed what 3 ended runs left behind and packed the rest · a paused run keeps its \
         copies for /resume",
    );
    assert_eq!(
        drawn(&report.lines(&still(&theme, WIDTH))),
        rows(NO_GIT, "● Cleaned this folder's hidden history", 2)
    );
}

#[test]
fn an_old_run_and_a_stopped_run_are_each_said_in_one_line() {
    let theme = theme();
    let ctx = still(&theme, WIDTH);
    let older = Notice::new(
        Tone::Dim,
        "\"hi\" (Oct 4) was written by an older Argonauts and can't be opened · /runs archive \
         moves it out of the way",
    );
    assert_eq!(
        drawn(&older.lines(&ctx)),
        rows(OLD_RUNS, "● \"hi\" (Oct 4)", 1)
    );
    let stopped = Notice::new(
        Tone::Warning,
        "\"Add due dates\" stopped at 3m10s when its run host exited · /resume carries it on \
         from Review",
    );
    assert_eq!(
        drawn(&stopped.lines(&ctx)),
        rows(OLD_RUNS, "● \"Add due dates\" stopped", 1)
    );
}
