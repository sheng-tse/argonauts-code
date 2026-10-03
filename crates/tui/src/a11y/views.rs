//! Views as flat lines: a table row reads as `header: value` pairs, a list as numbered items,
//! and a pager as its lines.

use argonauts_core::api::RunSummary;
use argonauts_core::app::ViewData;
use argonauts_core::display_names::DisplayNames;
use argonauts_core::views::{
    AgentRow, CheckpointRow, LearningRow, LearningView, WorkflowFile, WorkflowOrigin, WorkflowRow,
};

use crate::format;
use crate::text::sanitize_line;

use super::words::{duration, tokens, word};

pub fn view_lines(data: &ViewData, names: &DisplayNames) -> Vec<String> {
    match data {
        ViewData::Runs {
            runs,
            chats,
            sessions,
        } => {
            let mut lines = Vec::new();
            if !chats.is_empty() {
                lines.push(format!("conversations: {}", chats.len()));
                lines.extend(numbered(chats.iter().map(|chat| {
                    format!("{}: {}, turns: {}", chat.chat, one(&chat.title), chat.turns)
                })));
            }
            lines.push(format!("runs: {}", runs.len()));
            lines.extend(numbered(runs.iter().map(run_row)));
            if !sessions.is_empty() {
                lines.push(format!("agent cli sessions: {}", sessions.len()));
                lines.extend(numbered(sessions.iter().map(|session| {
                    format!(
                        "{}: {}, vendor: {}",
                        one(&session.reference().to_string()),
                        one(&session.title),
                        format::vendor_label(&one(&session.vendor.0))
                    )
                })));
            }
            if !runs.is_empty() || !chats.is_empty() || !sessions.is_empty() {
                lines.push(
                    "resume: type /resume and a conversation's, a run's, or a session's id to \
                     carry it on."
                        .to_owned(),
                );
            }
            lines
        }
        ViewData::Status { summary } => {
            let mut lines = vec!["status:".to_owned()];
            lines.extend(
                summary
                    .goal
                    .iter()
                    .map(|goal| format!("goal: {}", one(goal))),
            );
            if let Some(plan) = summary.plan.as_deref() {
                lines.push(format!("plan: {}", one(plan)));
            }
            lines.extend(summary.cards.iter().map(|card| {
                let mut line = format!(
                    "{}: {}, state: {}",
                    card.card,
                    one(&card.title),
                    word(&card.state)
                );
                if let Some(owner) = &card.owner {
                    line.push_str(&format!(", owner: {owner}"));
                }
                if !card.note.trim().is_empty() {
                    line.push_str(&format!(", {}", one(&card.note)));
                }
                line
            }));
            lines.extend(
                summary
                    .decisions
                    .iter()
                    .map(|(id, text)| format!("decision {id}: {}", one(text))),
            );
            lines.extend(
                summary
                    .recent
                    .iter()
                    .map(|line| format!("recent: {}", one(line))),
            );
            lines.extend(
                summary
                    .waiting
                    .iter()
                    .map(|line| format!("waiting: {}", one(line))),
            );
            lines
        }
        ViewData::Config { entries } => entries
            .iter()
            .map(|entry| {
                format!(
                    "{}: {}, layer: {}",
                    one(&entry.key),
                    shown_value(&entry.value),
                    one(&entry.layer)
                )
            })
            .collect(),
        ViewData::Settings { rows, .. } => {
            let mut lines: Vec<String> = rows
                .iter()
                .map(|row| {
                    let mut line = format!(
                        "{}: {}, layer: {}",
                        one(&row.key),
                        shown_value(&row.value),
                        one(&row.layer)
                    );
                    if !row.allowed.is_empty() {
                        line.push_str(&format!(", choices: {}", row.allowed.join(", ")));
                    }
                    line
                })
                .collect();
            lines.push("settings: change one with /config, the key, and the value.".to_owned());
            lines
        }
        ViewData::Models { choices, .. } => {
            let mut lines = vec![format!("models: {}", choices.len())];
            lines.extend(numbered(choices.iter().map(|choice| {
                let mut line = format!("{}, family: {}", choice.worker, one(&choice.family));
                if choice.current {
                    line.push_str(", current");
                }
                if !choice.strengths.is_empty() {
                    line.push_str(&format!(", good at: {}", choice.strengths.join(", ")));
                }
                line
            })));
            lines.push("models: pick one with /model and its name.".to_owned());
            lines
        }
        ViewData::Approvals { view } => {
            let mut lines: Vec<String> = view
                .points
                .iter()
                .map(|row| {
                    format!(
                        "{}: {}, decided with: {}, {} matches in a row, {} misses",
                        word(&row.kind),
                        word(&row.mode),
                        word(&row.decide_with),
                        row.matches_in_a_row,
                        row.misses
                    )
                })
                .collect();
            if view.spot_checks_waiting > 0 {
                lines.push(format!(
                    "approvals: {} spot checks waiting",
                    view.spot_checks_waiting
                ));
            }
            lines
        }
        ViewData::Diff { diff } => {
            let mut lines = vec![format!(
                "diff: {}, {} files, {} added, {} removed",
                diff.card, diff.stat.files_changed, diff.stat.insertions, diff.stat.deletions
            )];
            lines.extend(pager(&diff.text));
            lines
        }
        ViewData::Raw { raw } => {
            let mut lines = vec![format!("raw output: {}", raw.session)];
            lines.extend(raw.lines.iter().flat_map(|line| pager(line)));
            if raw.truncated {
                lines.push(format!(
                    "raw output: cut short, the whole log is {}",
                    raw.path.display()
                ));
            }
            lines
        }
        ViewData::Workflow { view } => {
            let mut lines = vec![format!(
                "workflow: {}, {} of {} agents done, {}, {}",
                one(&view.name),
                view.agents_done,
                view.agents_total,
                duration(view.elapsed_ms),
                format::spend(&view.spent)
            )];
            for phase in &view.phases {
                lines.push(format!(
                    "phase {}: {}, {} of {} done",
                    one(&phase.name),
                    word(&phase.status),
                    phase.done,
                    phase.total
                ));
                lines.extend(phase.rows.iter().map(|row| match row {
                    WorkflowRow::Agent(agent) => format!("  {}", agent_row(agent, names)),
                    WorkflowRow::Workflow(nested) => format!(
                        "  workflow {}: {}, {} of {} agents done",
                        one(&nested.name),
                        word(&nested.status),
                        nested.agents_done,
                        nested.agents_total
                    ),
                }));
            }
            lines
        }
        ViewData::Agent { view } => {
            let mut lines = vec![format!("agent: {}", agent_row(&view.row, names))];
            lines.extend(view.activity.iter().map(|line| {
                let outcome = match line.ok {
                    Some(true) => "done",
                    Some(false) => "failed",
                    None => "running",
                };
                format!(
                    "tool: {} {}, {outcome}",
                    one(&line.tool),
                    one(&line.summary)
                )
            }));
            if let Some(outcome) = view.outcome.as_deref() {
                lines.extend(
                    pager(outcome)
                        .into_iter()
                        .map(|line| format!("outcome: {line}")),
                );
            }
            lines
        }
        ViewData::Context { view } => view
            .rows
            .iter()
            .map(|row| {
                let cap = row
                    .cap
                    .map(|cap| format!(" of {}", tokens(cap)))
                    .unwrap_or_default();
                format!(
                    "{}: {}, {}{cap}, {} tool calls, {}",
                    one(&row.label),
                    names.worker(&row.worker),
                    tokens(row.used),
                    row.tool_calls,
                    format::spend(&row.spent)
                )
            })
            .collect(),
        ViewData::AllRepos { repos } => repos
            .iter()
            .flat_map(|repo| {
                let mut lines = vec![format!(
                    "repo {}: {} runs",
                    one(&repo.repo),
                    repo.runs.len()
                )];
                lines.extend(repo.runs.iter().map(|run| format!("  {}", run_row(run))));
                lines
            })
            .collect(),
        // A copy's notice says what changed; the list isn't read again.
        ViewData::WorkflowLibrary { refresh: true, .. } => Vec::new(),
        ViewData::WorkflowLibrary { workflows, .. } => {
            let mut lines = vec![format!("workflows: {}", workflows.len())];
            lines.extend(numbered(workflows.iter().map(workflow_row)));
            lines
        }
        ViewData::Learning { view } if view.refresh => Vec::new(),
        ViewData::Learning { view } => learning_lines(view),
        ViewData::Effort { slider } => {
            let model = one(&slider.model);
            let mut lines = match slider.level() {
                None => vec![format!(
                    "effort for {model}: none, it takes no effort setting"
                )],
                Some(level) if slider.is_fixed() => {
                    vec![format!(
                        "effort for {model}: {}, fixed by the model",
                        one(level)
                    )]
                }
                Some(level) => {
                    let levels: Vec<String> =
                        slider.stops.iter().map(|stop| one(&stop.level)).collect();
                    vec![
                        format!("effort for {model}: {}", one(level)),
                        format!("levels: {}", levels.join(", ")),
                    ]
                }
            };
            // How to flip it follows, as the view's offer.
            if let Some(toggle) = slider.all_hands {
                let state = if toggle.on { "on" } else { "off" };
                lines.push(format!("all hands: {state}"));
            }
            lines
        }
        ViewData::Crew { panel } => {
            let mut lines = vec![format!("crew: {} rows", panel.rows.len())];
            lines.extend(numbered(
                panel
                    .rows
                    .iter()
                    .map(|row| format!("{}: {}", one(&row.title), one(&row.latest))),
            ));
            lines
        }
        ViewData::Checkpoints {
            run, checkpoints, ..
        } => {
            let mut lines = vec![format!(
                "rewind: {} checkpoints in {run}",
                checkpoints.len()
            )];
            lines.extend(numbered(checkpoints.iter().map(checkpoint_row)));
            lines
        }
    }
}

/// A Learning picker as lines: each tab's rows as `header: value` pairs with their detail and
/// what to type to act, then the settings.
fn learning_lines(view: &LearningView) -> Vec<String> {
    let area = view.area;
    let mut lines = vec![format!("{}: {}", area.command(), area.description())];
    if !view.summary.trim().is_empty() {
        lines.push(one(&view.summary));
    }
    for tab in &view.tabs {
        lines.push(format!("{}: {}", one(&tab.title), tab.rows.len()));
        if tab.rows.is_empty() {
            lines.push(one(&tab.empty));
        }
        lines.extend(numbered(
            tab.rows.iter().map(|row| learning_row(&tab.columns, row)),
        ));
    }
    lines.push(format!("settings: {}", view.settings.len()));
    lines.extend(numbered(view.settings.iter().map(|row| {
        let value = if row.value.is_empty() {
            "unset"
        } else {
            &row.value
        };
        format!(
            "{} = {}, from {}",
            one(&row.key),
            one(value),
            one(&row.layer)
        )
    })));
    lines.push(format!(
        "change a setting: type /config and its key. The shell's argonauts {} does the same as the picker's keys.",
        area.command()
    ));
    lines
}

fn learning_row(columns: &[String], row: &LearningRow) -> String {
    let mut parts: Vec<String> = columns
        .iter()
        .zip(&row.cells)
        .filter(|(_, cell)| !cell.trim().is_empty())
        .map(|(column, cell)| format!("{}: {}", one(column).to_lowercase(), one(cell)))
        .collect();
    if row.waiting {
        parts.push("waits for you".to_owned());
    }
    parts.extend(row.detail.iter().map(|line| one(line)));
    if !row.actions.is_empty() {
        let actions: Vec<String> = row
            .actions
            .iter()
            .map(|action| one(&action.label))
            .collect();
        parts.push(format!("can {}", actions.join(", ")));
    }
    parts.join(", ")
}

fn workflow_row(row: &WorkflowFile) -> String {
    let from = match row.origin {
        WorkflowOrigin::BuiltIn => "built in".to_owned(),
        WorkflowOrigin::Yours | WorkflowOrigin::Repo => format!("from {}", one(&row.source)),
    };
    let mut line = format!("{}, {from}", one(&row.name));
    if row.in_use {
        line.push_str(", in use");
    }
    if row.edited == Some(true) {
        line.push_str(", edited");
    }
    if !row.description.trim().is_empty() {
        line.push_str(&format!(", {}", one(&row.description)));
    }
    line
}

fn checkpoint_row(row: &CheckpointRow) -> String {
    let to: Vec<String> = row.to.iter().map(ToString::to_string).collect();
    let commit: String = one(&row.commit).chars().take(7).collect();
    let mut line = format!(
        "{} handoff-{}, {:04}-{:02}-{:02} {:02}:{:02} UTC, {} to {}, {}, commit {commit}",
        row.card,
        row.n,
        row.at.year(),
        u8::from(row.at.month()),
        row.at.day(),
        row.at.hour(),
        row.at.minute(),
        row.from,
        to.join(" and "),
        one(&row.reason),
    );
    if row.reverted_here {
        line.push_str(", reverted here");
    }
    line
}

fn run_row(run: &RunSummary) -> String {
    format!(
        "{}: {}, state: {}, {}",
        run.run,
        one(&run.title),
        word(&run.state),
        format::spend(&run.spent)
    )
}

fn agent_row(row: &AgentRow, names: &DisplayNames) -> String {
    format!(
        "{}: {}, {}, {}, {}",
        one(&row.label),
        names.worker(&row.worker),
        word(&row.status),
        tokens(row.tokens),
        duration(row.elapsed_ms)
    )
}

fn numbered(rows: impl Iterator<Item = String>) -> Vec<String> {
    rows.enumerate()
        .map(|(n, row)| format!("  {}. {row}", n + 1))
        .collect()
}

fn pager(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| sanitize_line(line).trim_end().to_owned())
        .filter(|line| !line.trim().is_empty())
        .collect()
}

fn shown_value(value: &str) -> String {
    match one(value) {
        empty if empty.is_empty() => "unset".to_owned(),
        value => value,
    }
}

fn one(text: &str) -> String {
    sanitize_line(text).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use argonauts_core::app::ViewData;
    use argonauts_core::names::VendorId;
    use argonauts_core::vendor_sessions::VendorSession;
    use time::OffsetDateTime;

    use super::view_lines;

    #[test]
    fn the_resume_list_reads_cli_sessions_by_the_id_resume_takes() {
        let data = ViewData::Runs {
            runs: Vec::new(),
            chats: Vec::new(),
            sessions: vec![VendorSession {
                vendor: VendorId("codex".into()),
                id: "019a5770-e3f9".into(),
                title: "Explain the\ninput loop".into(),
                prompt: None,
                folder: "/work/shop".into(),
                updated: OffsetDateTime::UNIX_EPOCH,
                model: None,
                branch: None,
                last_message: None,
            }],
        };
        assert_eq!(
            view_lines(&data, &Default::default()),
            vec![
                "runs: 0",
                "agent cli sessions: 1",
                "  1. codex:019a5770-e3f9: Explain the input loop, vendor: Codex",
                "resume: type /resume and a conversation's, a run's, or a session's id to carry \
                 it on.",
            ]
        );
    }
}
