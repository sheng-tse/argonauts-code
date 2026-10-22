//! `argonauts behaviors`: each model's behaviors, most distinctive first, and the export for
//! Hodoscope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

use anyhow::Context as _;
use argonauts_core::Paths;
use argonauts_core::behaviors::Behavior;
use argonauts_core::display_names::DisplayNames;
use time::{Date, Duration, OffsetDateTime};
use unicode_width::UnicodeWidthStr;

use super::actions::Action;
use super::hodoscope;
use super::map;
use super::records::Records;

/// The widest a behavior's words get before they are cut.
const WORDS: usize = 56;

/// How far back to look: a span such as `7d`, `24h` or `4w`, or a day such as `2026-10-01`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BehaviorPeriod(OffsetDateTime);

impl BehaviorPeriod {
    /// The moment it starts.
    pub(crate) fn start(self) -> OffsetDateTime {
        self.0
    }

    fn at(text: &str, now: OffsetDateTime) -> Result<Self, String> {
        let text = text.trim();
        let span = |count: &str, unit: fn(i64) -> Duration| {
            count
                .parse::<u32>()
                .ok()
                .map(|count| unit(i64::from(count)))
        };
        let back = match text.char_indices().last() {
            Some((at, 'h')) => span(&text[..at], Duration::hours),
            Some((at, 'd')) => span(&text[..at], Duration::days),
            Some((at, 'w')) => span(&text[..at], Duration::weeks),
            _ => None,
        };
        if let Some(back) = back {
            return Ok(Self(now - back));
        }
        let format = time::macros::format_description!("[year]-[month]-[day]");
        Date::parse(text, format)
            .map(|day| Self(day.midnight().assume_utc()))
            .map_err(|_| {
                format!(
                    "`{text}` is neither a span such as 7d, 24h or 4w nor a day such as 2026-10-01"
                )
            })
    }
}

impl FromStr for BehaviorPeriod {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::at(text, OffsetDateTime::now_utc())
    }
}

/// Each model's behaviors over the actions taken since `since`, as `argonauts behaviors`
/// prints them.
pub fn behaviors_report(since: Option<BehaviorPeriod>) -> anyhow::Result<String> {
    let paths = Paths::from_env().context("finding your Argonauts Code folder")?;
    let actions = Records::of(&paths).actions(since.map(|since| since.0));
    Ok(report(&actions, &names()))
}

/// Writes each agent session's actions since `since` into `dir` as Hodoscope's trajectory
/// JSON, and says what it wrote.
pub fn export_hodoscope(dir: &Path, since: Option<BehaviorPeriod>) -> anyhow::Result<String> {
    let paths = Paths::from_env().context("finding your Argonauts Code folder")?;
    let actions = Records::of(&paths).actions_with_input(since.map(|since| since.0));
    let written = hodoscope::write(dir, &actions, &names())
        .with_context(|| format!("writing to {}", dir.display()))?;
    let sessions = if written == 1 { "session" } else { "sessions" };
    Ok(format!(
        "Wrote {written} {sessions} ({} actions) to {} for Hodoscope.",
        actions.len(),
        dir.display()
    ))
}

fn names() -> DisplayNames {
    DisplayNames {
        catalog: argonauts_registry::models::shipped_catalog().clone(),
        ..DisplayNames::default()
    }
}

/// Each model's behaviors in the order the Behaviors tab lists them, counted as the tab counts
/// them: an action no label fits is left out of the counts and said apart.
fn report(actions: &[Action], names: &DisplayNames) -> String {
    if actions.is_empty() {
        return "No actions yet. Every run and conversation adds the actions its agents take."
            .to_owned();
    }
    let mut groups: BTreeMap<String, Vec<Behavior>> = BTreeMap::new();
    for action in actions {
        let model = names
            .model(&action.cli, action.model.as_deref())
            .unwrap_or_else(|| names.cli(&action.cli));
        groups.entry(model).or_default().push(action.behavior);
    }
    let labelled = |behaviors: &[Behavior]| behaviors.iter().filter(|b| b.is_labelled()).count();
    let all: usize = groups.values().map(|behaviors| labelled(behaviors)).sum();
    let mut models: Vec<(&String, usize)> = groups
        .iter()
        .map(|(model, behaviors)| (model, labelled(behaviors)))
        .collect();
    models.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));

    let mut out = vec![format!("What your agents did, from {}", counted(actions))];
    for (model, count) in models {
        out.push(String::new());
        let others = all - count;
        if count < map::SMALLEST {
            out.push(format!(
                "{model} · {}: too few to compare, it needs {}",
                plural(count, "action"),
                map::SMALLEST
            ));
            continue;
        }
        out.push(format!(
            "{model} · {}; the others {others}",
            plural(count, "action")
        ));
        let rows = map::by_share(counted_apart(model, &groups), count, others);
        if rows.is_empty() {
            out.push(format!(
                "  nothing it did {} times or more yet",
                map::LISTED_FROM
            ));
        }
        for row in rows {
            let words = cut(&row.behavior, WORDS);
            let pad = WORDS.saturating_sub(words.width());
            out.push(format!(
                "  {words}{:pad$}  {:>4} ({:>4.1}%)   others {:>4} ({:>4.1}%)",
                "",
                row.mine,
                percent(row.mine, count),
                row.others,
                percent(row.others, others),
            ));
        }
        let unlabelled = groups[model].len() - count;
        if unlabelled > 0 {
            out.push(format!(
                "  and {} no label fits",
                plural(unlabelled, "action")
            ));
        }
    }
    out.join("\n")
}

/// Each labelled behavior's count in `model`'s actions and in the other models'.
fn counted_apart(model: &str, groups: &BTreeMap<String, Vec<Behavior>>) -> Vec<map::Counted> {
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (group, behaviors) in groups {
        for behavior in behaviors.iter().filter(|behavior| behavior.is_labelled()) {
            let count = counts.entry(behavior.words()).or_default();
            if group == model {
                count.0 += 1;
            } else {
                count.1 += 1;
            }
        }
    }
    counts
        .into_iter()
        .map(|(behavior, (mine, others))| map::Counted {
            behavior,
            mine,
            others,
        })
        .collect()
}

/// "562 actions in 19 runs across 6 projects", the actions no label fits left out as the
/// Behaviors tab leaves them out.
fn counted(actions: &[Action]) -> String {
    let runs: BTreeSet<&str> = actions
        .iter()
        .filter(|action| !action.conversation)
        .map(|action| action.run.as_str())
        .collect();
    let conversations: BTreeSet<(&str, &str)> = actions
        .iter()
        .filter(|action| action.conversation)
        .map(|action| (action.repo.as_str(), action.run.as_str()))
        .collect();
    let projects: BTreeSet<&str> = actions.iter().map(Action::project).collect();
    let labelled = actions
        .iter()
        .filter(|action| action.behavior.is_labelled())
        .count();
    let mut text = plural(labelled, "action");
    match (runs.len(), conversations.len()) {
        (runs, 0) => text += &format!(" in {}", plural(runs, "run")),
        (0, talks) => text += &format!(" in {}", plural(talks, "conversation")),
        (runs, talks) => {
            text += &format!(
                " in {} and {}",
                plural(runs, "run"),
                plural(talks, "conversation")
            );
        }
    }
    match projects.iter().next() {
        Some(project) if projects.len() == 1 => text += &format!(" in {project}"),
        _ => text += &format!(" across {} projects", projects.len()),
    }
    text
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

fn percent(count: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        count as f64 * 100.0 / of as f64
    }
}

fn cut(text: &str, cells: usize) -> String {
    if text.width() <= cells {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + c.to_string().width() + 1 > cells {
            break;
        }
        out.push(c);
    }
    out + "…"
}

#[cfg(test)]
mod tests {
    use argonauts_core::behaviors::Label;
    use argonauts_core::crew_stats::StatsRole;
    use time::macros::datetime;

    use super::*;

    fn action(repo: &str, run: &str, model: &str, behavior: Behavior) -> Action {
        Action {
            repo: repo.to_owned(),
            run: run.to_owned(),
            session: "s-001".to_owned(),
            call: "item_1".to_owned(),
            agent: "build:x".to_owned(),
            cli: if model.starts_with("gpt") {
                "codex"
            } else {
                "claude-code"
            }
            .to_owned(),
            model: Some(model.to_owned()),
            role: StatsRole::Worker,
            workflow: None,
            task: None,
            outcome: None,
            at: datetime!(2026-10-20 12:00 UTC),
            tool: "Bash".to_owned(),
            what: String::new(),
            ok: Some(true),
            behavior,
            ended: None,
            input: serde_json::Value::Null,
            conversation: !run.starts_with("run-"),
            note: None,
        }
    }

    #[test]
    fn each_model_s_behaviors_in_the_order_and_counts_the_tab_lists_them() {
        let history = Behavior::Two(Label::GitStatus, Label::GitHistory);
        let answer = Behavior::One(Label::Answer);
        let mut actions = Vec::new();
        for _ in 0..4 {
            actions.push(action(
                "council-0123456789ab",
                "run-1",
                "gpt-6-luna",
                history,
            ));
        }
        for _ in 0..6 {
            actions.push(action(
                "council-0123456789ab",
                "run-1",
                "gpt-6-luna",
                answer,
            ));
        }
        for _ in 0..9 {
            actions.push(action(
                "captain-0123456789ab",
                "run-2",
                "claude-haiku-5-5",
                answer,
            ));
        }
        actions.push(action(
            "captain-0123456789ab",
            "conversation-001",
            "claude-haiku-5-5",
            Behavior::Unlabelled,
        ));
        actions.push(action(
            "captain-0123456789ab",
            "run-2",
            "claude-haiku-4-5-20251001",
            answer,
        ));
        let text = report(&actions, &names());
        let expected = [
            "What your agents did, from 20 actions in 2 runs and 1 conversation across 2 projects",
            "",
            "GPT 6 Luna · 10 actions; the others 10",
            "  checked git status or diff and read git history              4 (40.0%)   others    0 ( 0.0%)",
            "  returned its structured answer                               6 (60.0%)   others   10 (100.0%)",
            "",
            "Haiku 5.5 · 9 actions; the others 11",
            "  returned its structured answer                               9 (100.0%)   others    7 (63.6%)",
            "  and 1 action no label fits",
            "",
            "Haiku 4.5 · 1 action: too few to compare, it needs 5",
        ];
        assert_eq!(text, expected.join("\n"));
    }

    #[test]
    fn a_model_that_did_nothing_three_times_says_so() {
        let actions: Vec<Action> = [
            Label::ReadSource,
            Label::SearchCode,
            Label::ListFiles,
            Label::RunTests,
            Label::Build,
        ]
        .into_iter()
        .map(|label| {
            action(
                "council-0123456789ab",
                "run-1",
                "gpt-6-luna",
                Behavior::One(label),
            )
        })
        .collect();
        let text = report(&actions, &names());
        assert!(
            text.ends_with(
                "GPT 6 Luna · 5 actions; the others 0\n  nothing it did 3 times or more yet"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_period_is_a_span_or_a_day() {
        let now = datetime!(2026-10-20 12:00 UTC);
        let at = |text: &str| BehaviorPeriod::at(text, now).map(|since| since.0);
        assert_eq!(at("7d"), Ok(datetime!(2026-10-13 12:00 UTC)));
        assert_eq!(at("24h"), Ok(datetime!(2026-10-19 12:00 UTC)));
        assert_eq!(at("2w"), Ok(datetime!(2026-10-06 12:00 UTC)));
        assert_eq!(at("2026-10-01"), Ok(datetime!(2026-10-01 0:00 UTC)));
        assert!(at("soon").is_err());
        assert!(at("-3d").is_err());
        assert!(at("d").is_err());
    }

    #[test]
    fn with_nothing_recorded_it_says_so() {
        assert!(report(&[], &names()).starts_with("No actions yet."));
    }
}
