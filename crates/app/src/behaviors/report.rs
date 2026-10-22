//! What usage reports say of behaviors: counts of actions under each label from the fixed
//! list, never the action, its command, its file or its project.

use std::collections::BTreeMap;

use argonauts_core::crew_stats::StatsRole;
use serde::Serialize;
use time::Date;

use super::actions::Action;
use crate::usage_ledger::local_day;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct BehaviorCounts {
    /// Model, then role, then label, then how many actions.
    pub behaviors: BTreeMap<String, BTreeMap<&'static str, BTreeMap<String, u32>>>,
    /// Label, then how its work ended (`merged`, `sent_back` or `dropped`), then how many
    /// actions.
    pub behavior_outcomes: BTreeMap<String, BTreeMap<&'static str, u32>>,
    /// Model, then how many actions no label fits.
    pub behaviors_unlabelled: BTreeMap<String, u32>,
}

/// The counts of the actions taken on the days from `from` up to but not including `until`.
/// `model_name` says how a model may be named in a report, from the CLI or endpoint that ran
/// it and its model.
pub(crate) fn count(
    actions: &[Action],
    (from, until): (Date, Date),
    model_name: impl Fn(&str, Option<&str>) -> String,
) -> BehaviorCounts {
    let mut counts = BehaviorCounts::default();
    let within = |action: &&Action| {
        let day = local_day(action.at);
        from <= day && day < until
    };
    for action in actions.iter().filter(within) {
        let model = model_name(&action.cli, action.model.as_deref());
        if !action.behavior.is_labelled() {
            *counts.behaviors_unlabelled.entry(model).or_default() += 1;
            continue;
        }
        let label = action.behavior.id();
        *counts
            .behaviors
            .entry(model)
            .or_default()
            .entry(role_name(action.role))
            .or_default()
            .entry(label.clone())
            .or_default() += 1;
        if let Some(outcome) = action.outcome {
            *counts
                .behavior_outcomes
                .entry(label)
                .or_default()
                .entry(outcome.name())
                .or_default() += 1;
        }
    }
    counts
}

/// A role as reports and exports name it.
pub(crate) fn role_name(role: StatsRole) -> &'static str {
    match role {
        StatsRole::Main => "main",
        StatsRole::Orchestrator => "orchestrator",
        StatsRole::Worker => "worker",
        StatsRole::Reviewer => "reviewer",
        StatsRole::Judge => "judge",
        StatsRole::Merger => "merger",
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::behaviors::{Behavior, Label};
    use time::macros::datetime;

    use super::*;
    use crate::behaviors::actions::Outcome;

    /// A ledger of actions made by hand: what was done, by whom, and how it ended.
    fn action(
        model: Option<&str>,
        role: StatsRole,
        behavior: Behavior,
        outcome: Option<Outcome>,
    ) -> Action {
        Action {
            repo: "council-0123456789ab".to_owned(),
            run: "run-20261020-123036-4e50".to_owned(),
            session: "s-004".to_owned(),
            call: "item_1".to_owned(),
            agent: "build:per-api-key".to_owned(),
            cli: "codex".to_owned(),
            model: model.map(str::to_owned),
            role,
            workflow: Some("council".to_owned()),
            task: Some("task-per-api-key".to_owned()),
            outcome,
            at: datetime!(2026-10-20 12:00 UTC),
            tool: "command_execution".to_owned(),
            what: "git show ef3f187 -- ratelimit/bucket.py".to_owned(),
            ok: Some(true),
            behavior,
            ended: None,
            input: serde_json::Value::Null,
            conversation: false,
            note: None,
        }
    }

    fn day() -> (Date, Date) {
        let day = local_day(datetime!(2026-10-20 12:00 UTC));
        (day, day.next_day().unwrap())
    }

    /// Only a model on the built-in list is named; any other goes as "other".
    fn named(_cli: &str, model: Option<&str>) -> String {
        match model {
            Some(model @ ("gpt-6-luna" | "claude-haiku-5-5")) => model.to_owned(),
            Some(_) => "other".to_owned(),
            None => "default".to_owned(),
        }
    }

    #[test]
    fn the_three_fields_from_a_hand_made_ledger() {
        let history = Behavior::Two(Label::GitStatus, Label::GitHistory);
        let ledger = [
            action(
                Some("gpt-6-luna"),
                StatsRole::Worker,
                history,
                Some(Outcome::SentBack),
            ),
            action(
                Some("gpt-6-luna"),
                StatsRole::Worker,
                history,
                Some(Outcome::Merged),
            ),
            action(
                Some("gpt-6-luna"),
                StatsRole::Reviewer,
                Behavior::One(Label::RunTests),
                Some(Outcome::Merged),
            ),
            action(
                Some("claude-haiku-5-5"),
                StatsRole::Main,
                Behavior::One(Label::Workflow),
                None,
            ),
            action(
                Some("acme-prod-gpt"),
                StatsRole::Worker,
                Behavior::One(Label::EditTestAdded),
                Some(Outcome::Dropped),
            ),
            action(
                Some("gpt-6-luna"),
                StatsRole::Worker,
                Behavior::Unlabelled,
                Some(Outcome::Merged),
            ),
            action(None, StatsRole::Worker, Behavior::Unlabelled, None),
        ];
        let counts = count(&ledger, day(), named);
        let json = serde_json::to_value(&counts).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "behaviors": {
                    "claude-haiku-5-5": {"main": {"workflow": 1}},
                    "gpt-6-luna": {
                        "reviewer": {"run_tests": 1},
                        "worker": {"git_status+git_history": 2},
                    },
                    "other": {"worker": {"edit_test_added": 1}},
                },
                "behavior_outcomes": {
                    "edit_test_added": {"dropped": 1},
                    "git_status+git_history": {"merged": 1, "sent_back": 1},
                    "run_tests": {"merged": 1},
                },
                "behaviors_unlabelled": {"default": 1, "gpt-6-luna": 1},
            })
        );
    }

    #[test]
    fn nothing_that_names_the_work_is_counted() {
        let ledger = [action(
            Some("gpt-6-luna"),
            StatsRole::Worker,
            Behavior::One(Label::GitHistory),
            Some(Outcome::Merged),
        )];
        let text = serde_json::to_string(&count(&ledger, day(), named)).unwrap();
        for private in [
            "ef3f187",
            "ratelimit",
            "council",
            "per-api-key",
            "run-2026",
            "s-004",
        ] {
            assert!(!text.contains(private), "{private} in {text}");
        }
    }

    #[test]
    fn only_the_period_s_days_count() {
        let ledger = [action(
            Some("gpt-6-luna"),
            StatsRole::Worker,
            Behavior::One(Label::GitHistory),
            None,
        )];
        let (_, until) = day();
        let next = (until, until.next_day().unwrap());
        assert_eq!(count(&ledger, next, named), BehaviorCounts::default());
    }
}
