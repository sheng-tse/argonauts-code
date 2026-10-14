//! What the four Learning pickers show, built from the learning crate's files: track records
//! and prediction scores, recipes with their candidates and the replay set, skills and the
//! branches waiting for you, and notes with the flagged ones and the conventions. A part of
//! learning that isn't built yet shows as empty, never as an error.

use time::OffsetDateTime;

use argonauts_core::app::SettingRow;
use argonauts_core::settings::Settings;
use argonauts_core::views::{
    LearningAction, LearningArea, LearningCommand, LearningRow, LearningTab, LearningView,
};
use argonauts_crew::retro::notes;
use argonauts_learning::Store;
use argonauts_learning::evolution::{self, CandidateState, Evolution};
use argonauts_learning::predictions::{self, Calibration};
use argonauts_learning::recipes::{self, RecipeSet};
use argonauts_learning::records::{Book, Record};
use argonauts_learning::replay::ReplaySet;
use argonauts_learning::skills;
use argonauts_learning::sync::{self, SyncMode};

use super::or_empty;

/// What a picker opens on: the agent `/records <agent>` names, the recipe `/learned <name>`
/// names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Focus {
    pub name: Option<String>,
}

/// The area's picker, with `settings` as its Settings tab.
pub(crate) fn build(
    area: LearningArea,
    store: &Store,
    settings: &Settings,
    focus: &Focus,
    settings_rows: Vec<SettingRow>,
) -> anyhow::Result<LearningView> {
    let now = OffsetDateTime::now_utc();
    let (tabs, summary) = match area {
        LearningArea::Records => records(store, settings, now)?,
        LearningArea::Recipes => recipes(store, settings, now)?,
        LearningArea::Skills => skill_tabs(store)?,
        LearningArea::Memory => memory(store)?,
    };
    let (tab, focus) = focused(&tabs, focus);
    Ok(LearningView {
        area,
        tabs,
        tab,
        focus,
        summary,
        settings: settings_rows,
        refresh: false,
    })
}

/// The first row whose id starts with the named agent or recipe.
fn focused(tabs: &[LearningTab], focus: &Focus) -> (usize, Option<String>) {
    let Some(name) = &focus.name else {
        return (0, None);
    };
    for (index, tab) in tabs.iter().enumerate() {
        if let Some(row) = tab
            .rows
            .iter()
            .find(|row| row.id == *name || row.id.starts_with(&format!("{name} ")))
        {
            return (index, Some(row.id.clone()));
        }
    }
    (0, None)
}

fn action(key: char, label: &str, command: LearningCommand) -> LearningAction {
    LearningAction {
        key,
        label: label.to_owned(),
        command,
    }
}

fn percent(rate: f64) -> String {
    format!("{:.0}%", rate * 100.0)
}

/// `1 skill`, `2 skills`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `1 branch waits for you`, `2 branches wait for you`.
fn waiting(n: usize, one: &str, many: &str) -> String {
    let verb = if n == 1 { "waits" } else { "wait" };
    format!("{} {verb} for you", count(n, one, many))
}

fn records(
    store: &Store,
    settings: &Settings,
    now: OffsetDateTime,
) -> anyhow::Result<(Vec<LearningTab>, String)> {
    let routing = &settings.routing;
    let book = Book::rebuild(store, routing, now)?;
    let rows: Vec<LearningRow> = book
        .rows()
        .iter()
        .map(|record| record_row(record, &book, settings))
        .collect();
    let lines = argonauts_learning::outcomes::read_all(store)?;
    let calibration = predictions::calibration(&lines, routing);
    let scores: Vec<LearningRow> = calibration.iter().map(calibration_row).collect();
    let workers = {
        let mut workers: Vec<String> = book
            .rows()
            .iter()
            .map(|record| record.worker.to_string())
            .collect();
        workers.sort();
        workers.dedup();
        workers.len()
    };
    let summary = format!(
        "{} · routing {}, explores {}",
        count(workers, "agent", "agents"),
        serde_word(&routing.algorithm),
        percent(routing.max_explore_share)
    );
    Ok((
        vec![
            LearningTab {
                title: "Agents".to_owned(),
                columns: [
                    "Agent",
                    "Kind of work",
                    "First pass",
                    "Approved",
                    "Cost",
                    "Time",
                    "Predictions",
                    "Tasks",
                ]
                .map(str::to_owned)
                .to_vec(),
                rows,
                empty: "No outcomes yet. Each finished run adds its tasks.".to_owned(),
            },
            LearningTab {
                title: "Predictions".to_owned(),
                columns: [
                    "Agent",
                    "Predicts",
                    "Count",
                    "Brier",
                    "Skill",
                    "Calibration",
                ]
                .map(str::to_owned)
                .to_vec(),
                rows: scores,
                empty: "No scored predictions yet.".to_owned(),
            },
        ],
        summary,
    ))
}

fn record_row(record: &Record, book: &Book, settings: &Settings) -> LearningRow {
    let worker = record.worker.to_string();
    let mut detail = Vec::new();
    if let Some(model) = &record.model_id {
        detail.push(format!("Newest model: {model}"));
    }
    if let Some(group) = &record.pooled_from {
        detail.push(format!(
            "Scored with {group} until it has {} tasks of its own",
            settings.routing.own_type_after
        ));
    }
    if record.pinned {
        detail.push("Pinned by routing.pins".to_owned());
    }
    // Claim little: better only past `routing.claim_above`.
    for other in book
        .rows()
        .iter()
        .filter(|other| other.task_type == record.task_type && other.worker != record.worker)
    {
        let verdict = match book.clearly_better(
            &record.worker,
            &other.worker,
            &record.task_type,
            &settings.routing,
        ) {
            Some(true) => "better",
            Some(false) => "not better",
            None => "not clear yet",
        };
        detail.push(format!(
            "Than {} at {}: {verdict}",
            other.worker, record.task_type
        ));
    }
    LearningRow {
        id: format!("{worker} {}", record.task_type),
        cells: vec![
            worker.clone(),
            record.task_type.clone(),
            percent(record.pass.mean()),
            percent(record.approval.mean()),
            format!("{:.2}×", record.cost_ratio),
            format!("{:.2}×", record.time_ratio),
            format!("{:+.2}", record.prediction),
            format!("{:.0}", record.evidence),
        ],
        detail,
        actions: vec![
            action(
                'p',
                "pin",
                LearningCommand::Pin {
                    worker: worker.clone(),
                    task_type: record.task_type.clone(),
                },
            ),
            action('r', "reset", LearningCommand::Reset { worker }),
            action('b', "rebuild", LearningCommand::Rebuild),
        ],
        waiting: false,
    }
}

fn calibration_row(score: &Calibration) -> LearningRow {
    let mut detail = vec![format!(
        "Calibration error {:.3}, resolution {:.3}",
        score.decomposition.reliability, score.decomposition.resolution
    )];
    for bin in &score.bins {
        detail.push(format!(
            "{:.1} to {:.1}: {} predicted {}, {} happened",
            bin.lower,
            bin.upper,
            bin.count,
            percent(bin.mean_predicted),
            percent(bin.actual_rate)
        ));
    }
    LearningRow {
        id: format!("{} {}", score.worker, score.metric),
        cells: vec![
            score.worker.to_string(),
            score.metric.clone(),
            score.count.to_string(),
            format!("{:.3}", score.brier),
            format!("{:+.2}", score.skill),
            if score.overconfident {
                "overconfident".to_owned()
            } else {
                format!(
                    "{} said, {} happened",
                    percent(score.mean_predicted),
                    percent(score.actual_rate)
                )
            },
        ],
        detail,
        actions: Vec::new(),
        waiting: false,
    }
}

fn recipes(
    store: &Store,
    settings: &Settings,
    now: OffsetDateTime,
) -> anyhow::Result<(Vec<LearningTab>, String)> {
    let sets = or_empty(recipes::load_all(store))?;
    let evolution = Evolution::open(store)?;
    let candidates = or_empty(evolution.candidates())?;
    let replay = ReplaySet::open(store)?;
    let entries = or_empty(replay.entries())?;
    let proposals = or_empty(replay.proposals())?;
    let spent = or_empty(evolution.spent_this_month(now))?;
    let stage = evolution::stage(settings);
    let summary = match settings.limits.evolution_usd_per_month {
        Some(limit) => format!(
            "stage {} · ${spent:.2} of ${limit:.0}/month",
            serde_word(&stage)
        ),
        None => format!("stage {} · ${spent:.2} this month", serde_word(&stage)),
    };
    let recipe_rows = sets.iter().map(recipe_row).collect();
    let candidate_rows = candidates
        .iter()
        .map(|candidate| {
            let waiting = matches!(candidate.state, CandidateState::AwaitingPromotion);
            let mut actions = Vec::new();
            if matches!(
                candidate.state,
                CandidateState::Canary { .. } | CandidateState::Current
            ) {
                actions.push(action(
                    'c',
                    "close",
                    LearningCommand::Close {
                        candidate: candidate.id.clone(),
                    },
                ));
            }
            if waiting {
                actions.push(action(
                    'p',
                    "promote",
                    LearningCommand::Promote {
                        candidate: candidate.id.clone(),
                    },
                ));
                actions.push(action(
                    'x',
                    "reject",
                    LearningCommand::Reject {
                        candidate: candidate.id.clone(),
                    },
                ));
            }
            let thresholds = &candidate.thresholds;
            LearningRow {
                id: format!("{} {}", candidate.parent.name, candidate.id),
                cells: vec![
                    candidate.id.clone(),
                    format!("{} v{}", candidate.parent.name, candidate.version),
                    format!(
                        "{} ({})",
                        candidate.change.key,
                        serde_word(&candidate.change.class)
                    ),
                    state_word(&candidate.state),
                ],
                detail: vec![
                    format!("Why: {}", candidate.why),
                    format!(
                        "Needs +{:.0} points, cost up at most {}{}",
                        thresholds.min_gain_points,
                        percent(thresholds.max_cost_increase),
                        if thresholds.not_worse {
                            ", or not worse for its class"
                        } else {
                            ""
                        }
                    ),
                    candidate
                        .from_run
                        .as_ref()
                        .map(|run| format!("Proposed by the run review of {run}"))
                        .unwrap_or_default(),
                ],
                actions,
                waiting,
            }
        })
        .collect();
    let mut replay_rows: Vec<LearningRow> = entries
        .iter()
        .map(|entry| LearningRow {
            id: entry.id.clone(),
            cells: vec![
                entry.id.clone(),
                entry.task_type.clone(),
                if entry.holdout { "holdout" } else { "tuning" }.to_owned(),
                format!("${:.2}", entry.budget_usd),
            ],
            detail: vec![format!("From {}", entry.run)],
            actions: Vec::new(),
            waiting: false,
        })
        .collect();
    replay_rows.extend(proposals.iter().map(|proposal| {
        let entry = &proposal.entry;
        let mut actions = Vec::new();
        if proposal.valid() {
            actions.push(action(
                'a',
                "accept",
                LearningCommand::AcceptEntry {
                    id: entry.id.clone(),
                },
            ));
        }
        actions.push(action(
            'x',
            "reject",
            LearningCommand::RejectEntry {
                id: entry.id.clone(),
            },
        ));
        LearningRow {
            id: entry.id.clone(),
            cells: vec![
                entry.id.clone(),
                entry.task_type.clone(),
                "waits for you".to_owned(),
                format!("${:.2}", entry.budget_usd),
            ],
            detail: proposal
                .checks
                .iter()
                .map(|check| {
                    format!(
                        "{} {}: {}",
                        if check.passed { "passed" } else { "failed" },
                        check.name,
                        check.detail
                    )
                })
                .collect(),
            actions,
            waiting: true,
        }
    }));
    Ok((
        vec![
            LearningTab {
                title: "Workflows".to_owned(),
                columns: ["Workflow", "Runs", "Versions", "Mode", "Matches"]
                    .map(str::to_owned)
                    .to_vec(),
                rows: recipe_rows,
                empty: "No learned workflows yet. A run that merges and passes first time proposes one."
                    .to_owned(),
            },
            LearningTab {
                title: "Candidates".to_owned(),
                columns: ["Candidate", "Version", "Change", "State"]
                    .map(str::to_owned)
                    .to_vec(),
                rows: candidate_rows,
                empty: "No candidates. The run review proposes one change per run.".to_owned(),
            },
            LearningTab {
                title: "Replay set".to_owned(),
                columns: ["Task", "Kind of work", "Set", "Budget"]
                    .map(str::to_owned)
                    .to_vec(),
                rows: replay_rows,
                empty: "No replay tasks yet. Runs that merge with passing tests join it."
                    .to_owned(),
            },
        ],
        summary,
    ))
}

fn recipe_row(set: &RecipeSet) -> LearningRow {
    let current = set.current();
    let versions: Vec<String> = set
        .versions
        .iter()
        .map(|loaded| format!("v{}", loaded.recipe.version))
        .collect();
    let (mode, matches, detail) = match current {
        Some(loaded) => {
            let recipe = &loaded.recipe;
            let mut matches = recipe.matches.task_types.clone();
            matches.extend(
                recipe
                    .matches
                    .labels_any
                    .iter()
                    .map(|label| format!("#{label}")),
            );
            (
                recipe
                    .mode
                    .map(|mode| serde_word(&mode))
                    .unwrap_or_else(|| "triage".to_owned()),
                matches.join(", "),
                vec![
                    recipe.description.clone(),
                    format!(
                        "{} · sha256 {}",
                        loaded.path.display(),
                        short(&loaded.sha256)
                    ),
                ],
            )
        }
        None => (String::new(), String::new(), Vec::new()),
    };
    LearningRow {
        id: set.name.clone(),
        cells: vec![
            set.name.clone(),
            format!("v{}", set.current),
            versions.join(" "),
            mode,
            matches,
        ],
        detail,
        actions: vec![
            action(
                'r',
                "roll back",
                LearningCommand::Rollback {
                    recipe: set.name.clone(),
                },
            ),
            action(
                'e',
                "export as a skill",
                LearningCommand::Export {
                    recipe: set.name.clone(),
                },
            ),
        ],
        waiting: false,
    }
}

fn skill_tabs(store: &Store) -> anyhow::Result<(Vec<LearningTab>, String)> {
    let listed = or_empty(skills::list(store))?;
    let drifted = or_empty(skills::drift(store))?;
    let rows: Vec<LearningRow> = listed
        .iter()
        .map(|skill| {
            let front = &skill.frontmatter;
            let meta = |key: &str| front.metadata.get(key).cloned().unwrap_or_default();
            let drift = drifted.iter().find(|drift| drift.name == front.name);
            LearningRow {
                id: front.name.clone(),
                cells: vec![
                    front.name.clone(),
                    match (meta(skills::META_RECIPE), meta(skills::META_VERSION)) {
                        (recipe, _) if recipe.is_empty() => "imported".to_owned(),
                        (recipe, version) => format!("{recipe} v{version}"),
                    },
                    if drift.is_some() {
                        "edited by hand".to_owned()
                    } else {
                        String::new()
                    },
                    front.description.clone(),
                ],
                detail: drift
                    .map(|drift| {
                        vec![format!(
                            "{} changed since its export; sync leaves it alone",
                            drift.path.display()
                        )]
                    })
                    .unwrap_or_default(),
                actions: Vec::new(),
                waiting: drift.is_some(),
            }
        })
        .collect();
    let branches = branches(store, &["argonauts/skills/", "argonauts/learned/"]);
    let summary = format!(
        "{} · {}",
        count(rows.len(), "skill", "skills"),
        waiting(branches.len(), "branch", "branches")
    );
    Ok((
        vec![
            LearningTab {
                title: "Skills".to_owned(),
                columns: ["Skill", "From", "Drift", "Description"]
                    .map(str::to_owned)
                    .to_vec(),
                rows,
                empty: "No skills yet. Import some with argonauts skills import <folder or URL>."
                    .to_owned(),
            },
            LearningTab {
                title: "Waiting branches".to_owned(),
                columns: ["Branch", "Commit"].map(str::to_owned).to_vec(),
                rows: branches,
                empty: "No branches wait for you.".to_owned(),
            },
        ],
        summary,
    ))
}

/// Proposal branches under `prefixes`, each with its pull-request action.
fn branches(store: &Store, prefixes: &[&str]) -> Vec<LearningRow> {
    let listed = argonauts_learning::git::run(
        &store.repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(objectname:short) %(subject)",
            "refs/heads/argonauts/",
        ],
    )
    .unwrap_or_default();
    listed
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let branch = parts.next()?.to_owned();
            let commit = parts.next().unwrap_or_default().to_owned();
            let subject = parts.next().unwrap_or_default().to_owned();
            prefixes
                .iter()
                .any(|prefix| branch.starts_with(prefix))
                .then(|| LearningRow {
                    id: branch.clone(),
                    cells: vec![branch.clone(), commit],
                    detail: vec![subject],
                    actions: vec![action(
                        'o',
                        "open a pull request",
                        LearningCommand::OpenPullRequest { branch },
                    )],
                    waiting: true,
                })
        })
        .collect()
}

fn memory(store: &Store) -> anyhow::Result<(Vec<LearningTab>, String)> {
    let mut note_rows = Vec::new();
    for file in notes::read_files(&store.repo_root) {
        for (note, source) in file.notes {
            note_rows.push(LearningRow {
                id: format!("{} {} {}", source.run, source.card, note.topic),
                cells: vec![
                    note.topic.clone(),
                    note.text.clone(),
                    source.worker.to_string(),
                    format!("{} {}", source.run, source.card),
                ],
                detail: vec![format!("In {}", file.relative)],
                actions: Vec::new(),
                waiting: false,
            });
        }
    }
    let flagged_rows: Vec<LearningRow> = notes::flagged(&store.state_dir)
        .into_iter()
        .filter_map(|(path, flagged)| {
            let id = path.file_stem()?.to_string_lossy().into_owned();
            Some(LearningRow {
                id: id.clone(),
                cells: vec![
                    flagged.note.topic.clone(),
                    flagged.note.text.clone(),
                    flagged.source.worker.to_string(),
                    format!("{} {}", flagged.source.run, flagged.source.card),
                ],
                detail: flagged
                    .findings
                    .iter()
                    .map(|finding| format!("Flagged: {}", finding_text(finding)))
                    .collect(),
                actions: vec![
                    action(
                        'a',
                        "accept on a branch",
                        LearningCommand::AcceptNote { id: id.clone() },
                    ),
                    action('d', "drop", LearningCommand::DropNote { id }),
                ],
                waiting: true,
            })
        })
        .collect();
    let conventions = conventions_rows(store);
    let summary = format!(
        "{} · {}",
        count(note_rows.len(), "note", "notes"),
        waiting(flagged_rows.len(), "flagged", "flagged")
    );
    let columns = ["Topic", "Note", "Agent", "From"]
        .map(str::to_owned)
        .to_vec();
    Ok((
        vec![
            LearningTab {
                title: "Notes".to_owned(),
                columns: columns.clone(),
                rows: note_rows,
                empty: "No notes yet. Notes a task's reviewer accepts merge with the run."
                    .to_owned(),
            },
            LearningTab {
                title: "Flagged".to_owned(),
                columns,
                rows: flagged_rows,
                empty: "No flagged notes wait for you.".to_owned(),
            },
            LearningTab {
                title: "Conventions".to_owned(),
                columns: ["File", "State"].map(str::to_owned).to_vec(),
                rows: conventions,
                empty: "No .argonauts/conventions.md yet.".to_owned(),
            },
        ],
        summary,
    ))
}

/// `AGENTS.md` and `CLAUDE.md` as a dry sync sees them, each with the sync action.
fn conventions_rows(store: &Store) -> Vec<LearningRow> {
    if !store.conventions().is_file() {
        return Vec::new();
    }
    let sync_action = || vec![action('s', "sync", LearningCommand::Sync)];
    let report = match sync::sync(store, SyncMode::Check) {
        Ok(report) => report,
        Err(err) => {
            return vec![LearningRow {
                id: "conventions".to_owned(),
                cells: vec![".argonauts/conventions.md".to_owned(), err.to_string()],
                detail: Vec::new(),
                actions: sync_action(),
                waiting: false,
            }];
        }
    };
    let mut rows = Vec::new();
    let mut push = |paths: &[std::path::PathBuf], state: &str, waiting: bool| {
        for path in paths {
            let name = store
                .repo_relative(path)
                .unwrap_or(path)
                .display()
                .to_string();
            rows.push(LearningRow {
                id: name.clone(),
                cells: vec![name, state.to_owned()],
                detail: report.warnings.clone(),
                actions: sync_action(),
                waiting,
            });
        }
    };
    push(&report.written, "would change", false);
    push(&report.unchanged, "in sync", false);
    push(&report.edited, "edited by hand, left alone", true);
    rows
}

fn finding_text(finding: &argonauts_learning::scan::Finding) -> String {
    use argonauts_learning::scan::Finding;
    match finding {
        Finding::HiddenText { codepoint, .. } => format!("hidden character U+{codepoint:04X}"),
        Finding::Injection { phrase } => format!("injection phrasing \"{phrase}\""),
        Finding::AddressesAgents { phrase } => format!("addresses agents: \"{phrase}\""),
        Finding::Url { url } => format!("a URL, {url}"),
        Finding::DownloadAndRun { line } => format!("download and run: {line}"),
        Finding::ShellCommand { text } => format!("a shell command: {text}"),
        Finding::Permissions { phrase } => format!("about permissions: \"{phrase}\""),
        Finding::Secret { kind } => format!("a secret ({kind})"),
    }
}

fn state_word(state: &CandidateState) -> String {
    match state {
        CandidateState::Suggested => "suggested".to_owned(),
        CandidateState::Queued => "queued for replay".to_owned(),
        CandidateState::Replaying { spent_usd } => format!("replaying, ${spent_usd:.2} so far"),
        CandidateState::Rejected { reason } => format!("rejected: {reason}"),
        CandidateState::AwaitingPromotion => "waits for you".to_owned(),
        CandidateState::Canary { runs } => format!("canary, {runs} runs in"),
        CandidateState::Current => "current".to_owned(),
        CandidateState::RolledBack { reason } => format!("rolled back: {reason}"),
    }
}

/// An enum as its settings word, such as `bandit` or `epsilon-greedy`.
pub(crate) fn serde_word<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(word)) => word,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use argonauts_testkit::TempRepo;

    use super::*;

    fn summary(area: LearningArea, store: &Store) -> String {
        let settings = Settings::default();
        build(area, store, &settings, &Focus::default(), Vec::new())
            .unwrap()
            .summary
    }

    #[test]
    fn empty_learning_files_count_nothing_and_spent_nothing() {
        let repo = TempRepo::new().unwrap();
        let store = Store::new(repo.home().join("state"), repo.root());
        let records = summary(LearningArea::Records, &store);
        assert!(
            records.starts_with("0 agents · routing bandit"),
            "{records}"
        );
        let recipes = summary(LearningArea::Recipes, &store);
        assert!(recipes.contains(" $0.00 this month"), "{recipes}");
        let skills = summary(LearningArea::Skills, &store);
        assert!(
            skills.starts_with("0 skills · 0 branches wait for you"),
            "{skills}"
        );
        let memory = summary(LearningArea::Memory, &store);
        assert!(
            memory.starts_with("0 notes · 0 flagged wait for you"),
            "{memory}"
        );
    }

    /// The header gives the summary up to half the row and the description the rest, past a
    /// two-cell gap. Each fits in 80 columns with a count that grows a digit.
    #[test]
    fn each_header_fits_in_80_columns() {
        let repo = TempRepo::new().unwrap();
        let store = Store::new(repo.home().join("state"), repo.root());
        let cells = |text: &str| text.chars().count();
        for area in [
            LearningArea::Records,
            LearningArea::Recipes,
            LearningArea::Skills,
            LearningArea::Memory,
        ] {
            let summary = summary(area, &store);
            let summary_cells = cells(&summary) + 1;
            assert!(summary_cells <= 40, "{summary}");
            assert!(
                cells(area.description()) + 2 + summary_cells <= 80,
                "{} | {summary}",
                area.description()
            );
        }
    }

    #[test]
    fn one_of_a_thing_reads_in_the_singular() {
        assert_eq!(count(1, "agent", "agents"), "1 agent");
        assert_eq!(count(2, "agent", "agents"), "2 agents");
        assert_eq!(waiting(1, "branch", "branches"), "1 branch waits for you");
        assert_eq!(waiting(3, "branch", "branches"), "3 branches wait for you");
    }
}
