//! The shell's learning commands: `records`, `learned`, `skills`, `sync`, `run-review`, and
//! `mcp package`. Each gives plain text and the same as JSON, so read commands can print
//! either. `--force` and promotions need you at a terminal, outside any agent's session.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use time::OffsetDateTime;

use argonauts_core::RunId;
use argonauts_core::events::shared::RunReviewEvent;
use argonauts_core::events::{RunEvent, RunEventKind};
use argonauts_core::layout::run_files;
use argonauts_core::views::LearningCommand;
use argonauts_learning::learned_workflows;
use argonauts_learning::outcomes::{self, LogLine};
use argonauts_learning::predictions;
use argonauts_learning::records::Book;
use argonauts_learning::replay::ReplayRunner;
use argonauts_learning::skills;
use argonauts_learning::sync::{self, SyncMode};

use super::act::{self, Asker};
use super::replay::{self, SupervisedReplays, Turn};
use super::view::serde_word;
use super::{or_empty, store};
use crate::app_settings::AppSettings;
use crate::env::AppEnv;

/// A learning command from the shell.
#[derive(Debug, Clone, PartialEq)]
pub enum LearningShell {
    /// Track records and prediction scores, rebuilt from the outcome log as of a moment.
    Records {
        /// A date (`2026-09-30`) or a moment in RFC 3339; now when `None`.
        as_of: Option<String>,
        rebuild: bool,
    },
    LearnedWorkflowsList,
    LearnedWorkflowsShow {
        name: String,
    },
    /// Two versions' structural diff: the current against its parent unless named.
    LearnedWorkflowsDiff {
        name: String,
        from: Option<u32>,
        to: Option<u32>,
    },
    /// Replays the oldest queued candidate now, within the month's limit.
    LearnedWorkflowsReplay,
    LearnedWorkflowsPromote {
        candidate: String,
    },
    LearnedWorkflowsRollback {
        name: String,
    },
    SkillsList,
    SkillsExport {
        learned_workflow: String,
    },
    SkillsImport {
        source: String,
    },
    Sync {
        check: bool,
        force: bool,
    },
    RunReview {
        run: String,
    },
    McpPackage {
        dir: PathBuf,
        repo: PathBuf,
    },
}

/// What a command printed, as text and as JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellOutput {
    pub text: String,
    pub json: Value,
}

impl ShellOutput {
    fn said(text: String) -> Self {
        Self {
            json: json!({ "message": text }),
            text,
        }
    }
}

/// Runs `command` in the repo holding `repo`, or the current directory.
pub async fn learning(
    command: LearningShell,
    repo: Option<&Path>,
    overrides: Vec<String>,
) -> anyhow::Result<ShellOutput> {
    // The package names its repo itself, so it needs none here.
    if let LearningShell::McpPackage { dir, repo } = &command {
        return package(dir, repo);
    }
    let env = AppEnv::discover(repo)?;
    let settings = AppSettings::load(&env, overrides, false)?;
    let settings = settings.shared();
    let store = store(&env);
    let asker = Asker {
        at_terminal: env.interactive && !env.agent_session,
    };
    let needs_you = |what: &str| -> anyhow::Result<()> {
        if !asker.at_terminal {
            anyhow::bail!("{what} needs you at a terminal, outside any agent's session");
        }
        crate::refuse_inside_agent()
    };
    match command {
        LearningShell::Records { as_of, rebuild } => {
            let as_of = match as_of {
                Some(text) => moment(&text)?,
                None => OffsetDateTime::now_utc(),
            };
            records(&store, &settings, as_of, rebuild)
        }
        LearningShell::LearnedWorkflowsList => {
            let sets = or_empty(learned_workflows::load_all(&store))?;
            let evolution = argonauts_learning::evolution::Evolution::open(&store)?;
            let mut text = String::new();
            let mut rows = Vec::new();
            for set in &sets {
                let running = running(&evolution, set)?;
                let description = set
                    .version(running)
                    .map(|loaded| loaded.learned_workflow.description.clone())
                    .unwrap_or_default();
                let _ = writeln!(
                    text,
                    "{}  v{running}  {} versions  {description}",
                    set.name,
                    set.versions.len()
                );
                rows.push(json!({
                    "name": set.name,
                    "current": running,
                    "versions": set.versions.iter().map(|v| v.learned_workflow.version).collect::<Vec<_>>(),
                    "description": description,
                }));
            }
            if sets.is_empty() {
                text.push_str("No learned workflows yet.\n");
            }
            Ok(ShellOutput {
                text,
                json: json!({ "workflows": rows }),
            })
        }
        LearningShell::LearnedWorkflowsShow { name } => {
            let sets = or_empty(learned_workflows::load_all(&store))?;
            let set = sets
                .iter()
                .find(|set| set.name == name)
                .ok_or_else(|| anyhow::anyhow!("no learned workflow is named {name}"))?;
            let evolution = argonauts_learning::evolution::Evolution::open(&store)?;
            let current = set
                .version(running(&evolution, set)?)
                .ok_or_else(|| anyhow::anyhow!("{name} has no current version"))?;
            Ok(ShellOutput {
                text: current.text.clone(),
                json: json!({
                    "workflow": current.learned_workflow,
                    "path": current.path,
                    "sha256": current.sha256,
                }),
            })
        }
        LearningShell::LearnedWorkflowsDiff { name, from, to } => {
            let sets = learned_workflows::load_all(&store)?;
            let set = sets
                .iter()
                .find(|set| set.name == name)
                .ok_or_else(|| anyhow::anyhow!("no learned workflow is named {name}"))?;
            let to = to.unwrap_or(set.current);
            let from = from
                .or_else(|| {
                    set.version(to)
                        .and_then(|loaded| loaded.learned_workflow.parent)
                })
                .ok_or_else(|| {
                    anyhow::anyhow!("{name} v{to} has no parent; name one with --from")
                })?;
            let (Some(parent), Some(child)) = (set.version(from), set.version(to)) else {
                anyhow::bail!("{name} has no v{from} or no v{to}");
            };
            let changes = learned_workflows::diff(parent, child)?;
            let mut text = format!("{name} v{from} → v{to}\n");
            for change in &changes {
                let _ = writeln!(
                    text,
                    "{} ({}): {} → {}",
                    change.key,
                    serde_word(&change.class),
                    change
                        .before
                        .as_ref()
                        .map_or("unset".to_owned(), Value::to_string),
                    change
                        .after
                        .as_ref()
                        .map_or("unset".to_owned(), Value::to_string),
                );
            }
            Ok(ShellOutput {
                text,
                json: json!({ "from": from, "to": to, "changes": changes }),
            })
        }
        LearningShell::LearnedWorkflowsReplay => {
            let launcher: Arc<dyn crate::client::Launcher> =
                Arc::new(crate::client::ProcessLauncher::current()?);
            let runner: Arc<dyn ReplayRunner> = Arc::new(SupervisedReplays::new(&env, launcher));
            let checks = argonauts_conductor::check_runner(Arc::clone(&settings));
            let policy = crate::commands::policy_for(&env, Arc::clone(&settings));
            let turn =
                replay::replay_next(&store, &settings, runner, checks, policy.as_ref()).await?;
            Ok(ShellOutput::said(match turn {
                Turn::Idle(why) => format!("Nothing replayed: {why}."),
                Turn::Replayed {
                    candidate,
                    actions,
                    promotion,
                } => {
                    let mut text = format!("Replayed {candidate}.");
                    for action in &actions {
                        text.push(' ');
                        text.push_str(&argonauts_crew::run_review::action_note(action).text);
                    }
                    if let Some(promotion) = promotion {
                        text.push_str(&format!(" Promotion: {promotion:?}."));
                    }
                    text
                }
            }))
        }
        LearningShell::LearnedWorkflowsPromote { candidate } => {
            needs_you("promoting a learned workflow")?;
            let said = act::act(
                &env,
                &settings,
                &LearningCommand::Promote { candidate },
                asker,
            )?;
            Ok(ShellOutput::said(said))
        }
        LearningShell::LearnedWorkflowsRollback { name } => {
            let said = act::act(
                &env,
                &settings,
                &LearningCommand::Rollback {
                    learned_workflow: name,
                },
                asker,
            )?;
            Ok(ShellOutput::said(said))
        }
        LearningShell::SkillsList => {
            let listed = or_empty(skills::list(&store))?;
            let drift = or_empty(skills::drift(&store))?;
            let mut text = String::new();
            let mut rows = Vec::new();
            for skill in &listed {
                let front = &skill.frontmatter;
                let edited = drift.iter().any(|drift| drift.name == front.name);
                let _ = writeln!(
                    text,
                    "{}{}  {}",
                    front.name,
                    if edited { " (edited by hand)" } else { "" },
                    front.description
                );
                rows.push(json!({
                    "name": front.name,
                    "description": front.description,
                    "metadata": front.metadata,
                    "edited": edited,
                }));
            }
            if listed.is_empty() {
                text.push_str("No skills yet.\n");
            }
            Ok(ShellOutput {
                text,
                json: json!({ "skills": rows }),
            })
        }
        LearningShell::SkillsExport { learned_workflow } => Ok(ShellOutput::said(act::export(
            &store,
            &settings,
            &learned_workflow,
        )?)),
        LearningShell::SkillsImport { source } => {
            let report = skills::import(&store, &source)?;
            if !report.rejected.is_empty() {
                anyhow::bail!("refused: {}", report.rejected.join("; "));
            }
            let proposal = skills::propose_import(&store, &report, settings.skills.claude_link)?;
            let mut text = format!(
                "Proposed {} from {} on {}. Nothing in it ran.",
                report.name, report.source, proposal.branch
            );
            if !report.code.is_empty() {
                text.push_str(&format!(" Code in it: {}.", report.code.join(", ")));
            }
            if !report.findings.is_empty() {
                text.push_str(&format!(" {} findings in its scan.", report.findings.len()));
            }
            Ok(ShellOutput::said(text))
        }
        LearningShell::Sync { check, force } => {
            let mode = match (check, force) {
                (true, _) => SyncMode::Check,
                (false, true) => {
                    needs_you("argonauts sync --force")?;
                    SyncMode::Force
                }
                (false, false) => SyncMode::Write,
            };
            let report = sync::sync(&store, mode)?;
            let text = act::sync_line(&report, &store);
            Ok(ShellOutput {
                json: json!({
                    "written": report.written,
                    "unchanged": report.unchanged,
                    "edited": report.edited,
                    "warnings": report.warnings,
                    "skill_drift": report.skill_drift.iter().map(|drift| &drift.name).collect::<Vec<_>>(),
                    "readers": report.readers,
                }),
                text,
            })
        }
        LearningShell::RunReview { run } => run_review(&env, &store, &RunId::new(run)?),
        LearningShell::McpPackage { dir, repo } => package(&dir, &repo),
    }
}

/// The version that runs now: a rollback or a promotion takes effect in the loop's state at
/// once, before its pointer change is merged.
fn running(
    evolution: &argonauts_learning::evolution::Evolution,
    set: &learned_workflows::LearnedWorkflowSet,
) -> anyhow::Result<u32> {
    Ok(or_empty(evolution.running_version(&set.name))?.unwrap_or(set.current))
}

/// `argonauts mcp package <dir> --repo <repo>`: the folder outside agents install to give work
/// to Argonauts Code, running this binary by its absolute path.
fn package(dir: &Path, repo: &Path) -> anyhow::Result<ShellOutput> {
    let binary = std::env::current_exe()?;
    let written = argonauts_learning::package::write(dir, repo, &binary)?;
    let names: Vec<String> = written
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    Ok(ShellOutput {
        text: format!("Wrote the outside agent package:\n{}", names.join("\n")),
        json: json!({ "written": names }),
    })
}

/// The end of a day (`2026-09-30` counts all of that day, UTC) or an RFC 3339 moment.
fn moment(text: &str) -> anyhow::Result<OffsetDateTime> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(moment) = OffsetDateTime::parse(text, &Rfc3339) {
        return Ok(moment);
    }
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let day = time::Date::parse(text, format)
        .map_err(|_| anyhow::anyhow!("--as-of takes a date like 2026-09-30, not {text:?}"))?;
    let next = day
        .next_day()
        .ok_or_else(|| anyhow::anyhow!("{text} is the last day there is"))?;
    Ok(next.midnight().assume_utc())
}

fn records(
    store: &argonauts_learning::Store,
    settings: &argonauts_core::settings::Settings,
    as_of: OffsetDateTime,
    rebuild: bool,
) -> anyhow::Result<ShellOutput> {
    let book = Book::rebuild(store, &settings.routing, as_of)?;
    let lines = outcomes::read_all(store)?;
    let scores = predictions::calibration(&lines, &settings.routing);
    let mut text = String::new();
    if rebuild {
        let _ = writeln!(
            text,
            "Rebuilt from the outcome log as of {as_of}: {} records.",
            book.rows().len()
        );
    }
    for record in book.rows() {
        let _ = writeln!(
            text,
            "{} {}: first pass {:.0}%, approved {:.0}%, cost {:.2}×, time {:.2}×, predictions {:+.2}, {:.0} tasks",
            record.worker,
            record.task_type,
            record.pass.mean() * 100.0,
            record.approval.mean() * 100.0,
            record.cost_ratio,
            record.time_ratio,
            record.prediction,
            record.evidence
        );
    }
    if book.rows().is_empty() {
        text.push_str("No outcomes yet.\n");
    }
    for score in &scores {
        let _ = writeln!(
            text,
            "{} {}: {} predictions, Brier {:.3}, skill {:+.2}{}",
            score.worker,
            score.metric,
            score.count,
            score.brier,
            score.skill,
            if score.overconfident {
                ", overconfident"
            } else {
                ""
            }
        );
    }
    Ok(ShellOutput {
        text,
        json: json!({
            "as_of": as_of.format(&time::format_description::well_known::Rfc3339)?,
            "records": book.rows(),
            "predictions": scores,
        }),
    })
}

/// `argonauts run review <run>`: the run's outcome, its predictions against what happened, its
/// memory, its routing, and the change it proposed.
#[derive(Debug, Clone, Default, Serialize)]
struct RunReviewReport {
    run: String,
    outcome: Option<Value>,
    tasks: Vec<Value>,
    predictions: Vec<Value>,
    draws: Vec<Value>,
    memory: Vec<Value>,
    change: Option<Value>,
}

fn run_review(
    env: &AppEnv,
    store: &argonauts_learning::Store,
    run: &RunId,
) -> anyhow::Result<ShellOutput> {
    let lines = outcomes::read_run(store, run)?;
    let mut report = RunReviewReport {
        run: run.to_string(),
        ..RunReviewReport::default()
    };
    for line in &lines {
        match line {
            LogLine::Run(record) => report.outcome = Some(serde_json::to_value(record)?),
            LogLine::Outcome(outcome) => report.tasks.push(serde_json::to_value(outcome)?),
            LogLine::Prediction(prediction) => {
                report.predictions.push(serde_json::to_value(prediction)?);
            }
            LogLine::Draw(draw) => report.draws.push(serde_json::to_value(draw)?),
            LogLine::Reversal(_) => {}
        }
    }
    for event in run_events(&env.run_dir(run).join(run_files::EVENTS)) {
        let RunEventKind::RunReview(run_review) = event.kind else {
            continue;
        };
        match run_review {
            RunReviewEvent::Memory {
                from,
                proposal,
                accepted,
                reason,
            } => report.memory.push(json!({
                "from": from,
                "topic": proposal.topic,
                "text": proposal.text,
                "accepted": accepted,
                "reason": reason,
            })),
            RunReviewEvent::Candidate(change) => {
                report.change = Some(serde_json::to_value(change)?)
            }
            _ => {}
        }
    }
    if lines.is_empty() && report.memory.is_empty() {
        anyhow::bail!("{run} has no run review yet");
    }
    let mut text = format!("Run review of {run}\n");
    if let Some(outcome) = &report.outcome {
        let _ = writeln!(
            text,
            "Outcome: {}, ${:.2}, {:.0} min, final checks {}",
            outcome["outcome"].as_str().unwrap_or("unknown"),
            outcome["cost_usd"].as_f64().unwrap_or(0.0),
            outcome["minutes"].as_f64().unwrap_or(0.0),
            if outcome["final_checks_passed"].as_bool() == Some(true) {
                "passed"
            } else {
                "didn't pass"
            }
        );
    }
    for task in &report.tasks {
        let _ = writeln!(
            text,
            "{} by {}: {} first try, {} tries{}",
            task["task"].as_str().unwrap_or_default(),
            task["worker"].as_str().unwrap_or_default(),
            if task["first_try"].as_bool() == Some(true) {
                "passed"
            } else {
                "didn't pass"
            },
            task["tries"],
            task["assigned"]
                .as_str()
                .map(|why| format!(", routed by {why}"))
                .unwrap_or_default()
        );
    }
    for prediction in &report.predictions {
        let _ = writeln!(
            text,
            "{} predicted {} for {}",
            prediction["worker"].as_str().unwrap_or_default(),
            prediction["p_pass_checks_first_try"],
            prediction["task"].as_str().unwrap_or_default()
        );
    }
    for note in &report.memory {
        let _ = writeln!(
            text,
            "Note [{}] {}: {}",
            note["topic"].as_str().unwrap_or_default(),
            if note["accepted"].as_bool() == Some(true) {
                "accepted"
            } else {
                "not kept"
            },
            note["text"].as_str().unwrap_or_default()
        );
    }
    if let Some(change) = &report.change {
        let _ = writeln!(
            text,
            "Proposed: {} = {} ({})",
            change["setting"].as_str().unwrap_or_default(),
            change["value"].as_str().unwrap_or_default(),
            change["why"].as_str().unwrap_or_default()
        );
    }
    Ok(ShellOutput {
        json: serde_json::to_value(&report)?,
        text,
    })
}

fn run_events(path: &Path) -> Vec<RunEvent> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What learning keeps waiting for you, one line each, for `argonauts approvals`: candidates for
/// promotion, flagged notes, replay tasks under `accept_entries = "ask"`, and proposal branches
/// for learned workflows, skills, imports, notes, and conventions.
pub fn learning_proposals(repo: Option<&Path>) -> anyhow::Result<Vec<String>> {
    let env = AppEnv::discover(repo)?;
    let store = store(&env);
    let mut lines = Vec::new();
    let evolution = argonauts_learning::evolution::Evolution::open(&store)?;
    for candidate in or_empty(evolution.candidates())? {
        if candidate.state == argonauts_learning::evolution::CandidateState::AwaitingPromotion {
            lines.push(format!(
                "{} changes {} in {}: promote or reject it in /learned",
                candidate.id, candidate.change.key, candidate.parent
            ));
        }
    }
    for (path, flagged) in argonauts_crew::run_review::notes::flagged(&env.state_dir) {
        let id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        lines.push(format!(
            "flagged note {id} from {} in {}: accept or drop it in /memory",
            flagged.source.task, flagged.source.run
        ));
    }
    let replay = argonauts_learning::replay::ReplaySet::open(&store)?;
    for proposal in or_empty(replay.proposals())? {
        lines.push(format!(
            "replay task {} from {} waits to join the replay set: accept or reject it in /learned",
            proposal.entry.id, proposal.entry.run
        ));
    }
    let branches = argonauts_learning::git::run(
        &env.repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/argonauts/learned/",
            "refs/heads/argonauts/skills/",
            "refs/heads/argonauts/notes/",
            "refs/heads/argonauts/conventions/",
        ],
    )
    .unwrap_or_default();
    lines.extend(
        branches
            .lines()
            .filter(|branch| !branch.trim().is_empty())
            .map(|branch| format!("{branch} waits for you to merge or open as a pull request")),
    );
    Ok(lines)
}
