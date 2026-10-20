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
use crate::views::goal_panel;

use super::words::{duration, tokens, word};

pub fn view_lines(data: &ViewData, names: &DisplayNames) -> Vec<String> {
    match data {
        ViewData::ResumeList {
            conversations,
            sessions,
        } => {
            let mut lines = Vec::new();
            if !conversations.is_empty() {
                lines.push(format!("conversations: {}", conversations.len()));
                lines.extend(numbered(conversations.iter().map(|conversation| {
                    let waits = if conversation.needs_you {
                        ", a run of it waits on you"
                    } else {
                        ""
                    };
                    format!(
                        "{}: {}, turns: {}{waits}",
                        conversation.conversation,
                        one(&conversation.title),
                        conversation.turns
                    )
                })));
            }
            if !sessions.is_empty() {
                lines.push(format!("agent cli sessions: {}", sessions.len()));
                lines.extend(numbered(sessions.iter().map(|session| {
                    format!(
                        "{}: {}, agent: {}",
                        one(&session.reference().to_string()),
                        one(&session.title),
                        format::agent_label(&one(&session.agent_id.0))
                    )
                })));
            }
            if conversations.is_empty() && sessions.is_empty() {
                lines.push("nothing to resume here yet".to_owned());
            } else {
                lines.push(
                    "resume: type /resume and a conversation's or a session's id to carry it on."
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
            lines.extend(summary.tasks.iter().map(|task| {
                let mut line = format!(
                    "{}: {}, state: {}",
                    task.task,
                    one(&task.title),
                    word(&task.state)
                );
                if let Some(worker) = &task.worker {
                    line.push_str(&format!(", worker: {worker}"));
                }
                if !task.note.trim().is_empty() {
                    line.push_str(&format!(", {}", one(&task.note)));
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
        ViewData::Mods { view } => crate::mods::spoken(view),
        ViewData::Models { choices, .. } => {
            let mut lines = vec![format!("models: {}", choices.len())];
            lines.extend(numbered(choices.iter().map(|choice| {
                let mut line = format!("{}, provider: {}", choice.worker, one(&choice.provider));
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
        ViewData::Goal { view } => {
            let mut lines = vec![format!(
                "goal: {}, {}",
                one(&view.objective),
                word(&view.status)
            )];
            lines.extend(view.criteria.iter().map(|row| {
                let state = row.state.as_ref().map(word).unwrap_or_default();
                format!(
                    "  {} {}: {}",
                    row.criterion.id,
                    one(&row.criterion.text),
                    state
                )
            }));
            lines
        }
        ViewData::Diff { diff } => {
            let mut lines = vec![format!(
                "diff: {}, {} files, {} added, {} removed",
                diff.task, diff.stat.files_changed, diff.stat.insertions, diff.stat.deletions
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
                // A step you require is said so, where the screen marks it with a star.
                let required = if view.requires(phase) {
                    ", a step you require"
                } else {
                    ""
                };
                lines.push(match phase.count {
                    Some(count) => format!(
                        "phase {}{required}: {}, {} of {} done",
                        one(&phase.name),
                        word(&phase.status),
                        count.done,
                        count.total
                    ),
                    None => format!(
                        "phase {}{required}: {}",
                        one(&phase.name),
                        word(&phase.status)
                    ),
                });
                lines.extend(phase.rows.iter().map(|row| match row {
                    WorkflowRow::Agent(agent) => {
                        let goal = view
                            .goal_of(agent)
                            .map(|goal| format!(", {}", goal_panel::row_words(goal)))
                            .unwrap_or_default();
                        format!("  {}{goal}", agent_row(agent, names))
                    }
                    WorkflowRow::Workflow(nested) => format!(
                        "  workflow {}: {}, {} of {} agents done",
                        one(&nested.name),
                        word(&nested.status),
                        nested.agents_done,
                        nested.agents_total
                    ),
                    WorkflowRow::Command(command) => format!(
                        "  {}: {}, {}",
                        one(&command.label),
                        word(&command.status),
                        one(command.result.as_deref().unwrap_or(&command.command))
                    ),
                }));
            }
            lines
        }
        ViewData::Agent { view } => {
            let mut lines = vec![format!("agent: {}", agent_row(&view.row, names))];
            if let Some(goal) = view.goal.as_deref() {
                lines.extend(goal_panel::spoken(goal));
            }
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
        ViewData::Usage { summary } => crate::overview::cards::spoken(summary, names),
        ViewData::StatusTab { view } => crate::overview::status_tab::spoken(view, names),
        ViewData::Overview { overview } => crate::overview::spoken_overview(overview, names),
        ViewData::Context { view } | ViewData::ContextPrint { view } => view
            .rows
            .iter()
            .map(|row| {
                let used = match row.window.or(row.cap).filter(|window| *window > 0) {
                    Some(window) => super::words::tokens_of_window(row.used, window),
                    None => tokens(row.used),
                };
                format!(
                    "{}: {}, {used}, {} tool calls, {}",
                    one(&row.label),
                    names.worker(&row.worker),
                    row.tool_calls,
                    format::spend(&row.spent)
                )
            })
            .collect(),
        ViewData::SessionContext {
            session, counted, ..
        } => match counted {
            Some(counted) => counted
                .usage
                .categories
                .iter()
                .map(|row| format!("{session} {}: {}", one(&row.name), tokens(row.tokens)))
                .collect(),
            None => vec![format!("{session}: not counted")],
        },
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
        ViewData::WorkflowPage { page } => workflow_page_lines(page),
        // A copy's notice says what changed; the list isn't read again.
        ViewData::Prompts { view } if view.refresh => Vec::new(),
        ViewData::Prompts { view } => {
            let mut lines = vec![format!("prompts: {} parts", view.rows.len())];
            lines.extend(view.table().lines().map(one));
            lines.extend(view.notes.iter().map(|note| one(note)));
            lines.push(
                "to change one: /prompts copy <name>, /prompts role <role>, /prompts writer \
                 [role] <who>, /prompts reset <name>"
                    .to_owned(),
            );
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
            if let Some(toggle) = slider.ultracode {
                let state = if toggle.on { "on" } else { "off" };
                lines.push(format!("ultracode: {state}"));
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

/// A workflow file's page as lines: where the file is, each step with its role, who would
/// take it and how, and the check. A change made here is typed in the file, which is checked
/// again each time it is saved.
fn workflow_page_lines(page: &argonauts_core::workflow_page::WorkflowPage) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(said) = &page.said {
        lines.push(one(said));
    }
    if !page.refresh {
        lines.push(format!("workflow {}: {}", one(&page.name), one(&page.path)));
    }
    for group in &page.groups {
        let parts = [&group.role, &group.who, &group.how];
        let said: Vec<String> = parts
            .iter()
            .filter(|part| !part.is_empty())
            .map(|part| one(part))
            .collect();
        let title = match group.step {
            Some(step) => format!("{} {}", step + 1, one(&group.title)),
            None => one(&group.title),
        };
        lines.push(format!("  {title}: {}", said.join(", ")));
    }
    let mark = if page.check.valid {
        "valid"
    } else {
        "not valid"
    };
    lines.push(format!("{mark}: {}", one(&page.check.lines.join("; "))));
    if !page.refresh {
        lines.push(
            "To change it, edit the file. It is checked again each time you save it.".to_owned(),
        );
    }
    lines
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
    if let Some(problem) = &row.problem {
        line.push_str(&format!(", problem: {}", one(problem)));
    }
    line
}

fn checkpoint_row(row: &CheckpointRow) -> String {
    let to: Vec<String> = row.to.iter().map(ToString::to_string).collect();
    let commit: String = one(&row.commit).chars().take(7).collect();
    let mut line = format!(
        "{} fallback-{}, {:04}-{:02}-{:02} {:02}:{:02} UTC, {} to {}, {}, commit {commit}",
        row.task,
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
    if row.rewound_here {
        line.push_str(", rewound here");
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
    // What the row says after its counts: a verdict, or why a fallback runs.
    let said = row
        .note
        .as_ref()
        .map(|note| format!(", {}", one(&note.text)))
        .unwrap_or_default();
    format!(
        "{}: {}{said}, {}, {}, {}",
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
    use argonauts_core::cli_sessions::CliSession;
    use argonauts_core::names::AgentId;
    use time::OffsetDateTime;

    use super::{agent_row, view_lines};

    #[test]
    fn the_resume_list_reads_cli_sessions_by_the_id_resume_takes() {
        let data = ViewData::ResumeList {
            conversations: Vec::new(),
            sessions: vec![CliSession {
                agent_id: AgentId("codex".into()),
                id: "019a5770-e3f9".into(),
                title: "Explain the\ninput loop".into(),
                prompt: None,
                folder: "/work/shop".into(),
                updated: OffsetDateTime::UNIX_EPOCH,
                model: None,
                branch: None,
                last_message: None,
                turns: 3,
            }],
        };
        assert_eq!(
            view_lines(&data, &Default::default()),
            vec![
                "agent cli sessions: 1",
                "  1. codex:019a5770-e3f9: Explain the input loop, agent: Codex",
                "resume: type /resume and a conversation's or a session's id to carry it on.",
            ]
        );
    }

    /// A reviewer's row, read aloud: who answered, what it decided, then the rest.
    #[test]
    fn a_rows_verdict_is_read_after_who_gave_it() {
        let row = argonauts_core::views::AgentRow {
            session: argonauts_core::names::SessionKey::parse("s-002").unwrap(),
            label: "review:store".into(),
            worker: "astra".parse().unwrap(),
            status: argonauts_core::views::RowStatus::Done,
            tokens: 41_200,
            spent: argonauts_core::Spend::default(),
            elapsed_ms: 58_000,
            subagents: 0,
            context: None,
            fallback_for: None,
            answered: true,
            tries: Vec::new(),
            note: Some(argonauts_core::views::RowNote::new(
                "sent it back · 2 findings",
                argonauts_core::views::Tone::Warn,
            )),
            inner: None,
            left_out: false,
        };
        let read = agent_row(&row, &Default::default());
        assert!(
            read.starts_with("review:store: astra, sent it back · 2 findings, "),
            "{read}"
        );
    }
}
