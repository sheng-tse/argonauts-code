//! The lead's brief: what it must say before anything runs, checked by
//! code. Every problem goes back to the lead at once, as the validator's errors do. The brief's
//! name and description become the run's identity; the example it started from shows only
//! once, at the end of the start item.

use std::collections::BTreeSet;

use unicode_width::UnicodeWidthStr;

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::labels;
use argonauts_core::steps::StepName;
use argonauts_core::steps::design::DesignOutput;
use argonauts_core::workflow::{Step, StepKind, Workflow};

use crate::state::RunState;

/// The widest a run's name may be, in cells.
pub const NAME_CELLS: usize = 32;
/// The widest a description may be, so it fits beside the totals at 100 columns.
pub const DESCRIPTION_CELLS: usize = 72;
pub const MAX_PHASES: usize = 6;
pub const MAX_CRITERIA: usize = 8;
pub const MAX_QUESTIONS: usize = 3;

/// What's wrong with the brief, each as one line for the lead. `workflow` is the one it names,
/// filled in and before code frames it; `examples` are the names it may start from.
pub fn problems(brief: &DesignOutput, workflow: &Workflow, examples: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    if let Some(problem) = name_problem(&brief.name, examples) {
        problems.push(problem);
    }
    let description = brief.description.trim();
    if description.is_empty() {
        problems.push("`description` is empty; say the task in one plain line".to_owned());
    } else if description.contains('\n') {
        problems.push("`description` is more than one line".to_owned());
    } else if description.width() > DESCRIPTION_CELLS {
        problems.push(format!(
            "`description` is {} cells; keep it to {DESCRIPTION_CELLS}",
            description.width()
        ));
    }
    if let Some(based_on) = &brief.based_on
        && !examples.iter().any(|example| example == based_on)
    {
        problems.push(format!("`based_on: {based_on}` names no example"));
    }
    let phases = workflow.phases.len();
    if phases == 0 || phases > MAX_PHASES {
        problems.push(format!(
            "the workflow has {phases} phases; write 1 to {MAX_PHASES}"
        ));
    }
    let mut titles = BTreeSet::new();
    for (index, phase) in workflow.phases.iter().enumerate() {
        let at = format!("phase {} ({})", index + 1, phase.name);
        if !titles.insert(phase.name.trim().to_lowercase()) {
            problems.push(format!(
                "{at} has the title another phase has; give each phase its own"
            ));
        }
        if !title_case(&phase.name) {
            problems.push(format!(
                "{at}: a title is one or two words in Title Case, such as `Fix` or `Final Check`"
            ));
        }
        if !phase.steps.is_empty() && phase.steps.iter().all(is_checkpoint) {
            problems.push(format!(
                "{at} holds only a checkpoint; a checkpoint gates the phase it comes before, so \
                 put it there"
            ));
        }
        // An example run as written keeps its own words; a workflow the lead wrote says them.
        if brief.workflow.is_some() && phase.detail.trim().is_empty() {
            problems.push(format!(
                "{at} has no `detail`; say who does what in one line"
            ));
        }
    }
    let criteria = brief.done_when.len();
    if criteria == 0 || criteria > MAX_CRITERIA {
        problems.push(format!(
            "`done_when` has {criteria} criteria; write 1 to {MAX_CRITERIA}, each one sentence a \
             check can prove"
        ));
    }
    let mut ids = BTreeSet::new();
    for criterion in &brief.done_when {
        if criterion.text.trim().is_empty() {
            problems.push(format!("criterion {} has no text", criterion.id));
        }
        if !ids.insert(criterion.id.as_str()) {
            problems.push(format!("criterion id {} is used twice", criterion.id));
        }
    }
    if brief.questions.len() > MAX_QUESTIONS {
        problems.push(format!(
            "{} questions; ask at most {MAX_QUESTIONS}, the rest are yours to decide",
            brief.questions.len()
        ));
    }
    if brief.plan.len() > 1 {
        problems.push(format!(
            "`plan` lists {} cards; a build from the goal makes one card, and a drafted plan's \
             planner names its own",
            brief.plan.len()
        ));
    }
    let mut items = BTreeSet::new();
    for card in &brief.plan {
        let item = labels::slug(&card.item, 3);
        if item.is_empty() || !items.insert(item.clone()) {
            problems.push(format!(
                "plan item `{}` is empty or repeats; each card needs its own one-token name",
                card.item
            ));
        }
    }
    problems
}

/// The brief the lead wrote for this run: the newest design asked before the workflow
/// started. A design at a phase end serves a step, so it isn't one.
pub fn of_run(state: &RunState) -> Option<DesignOutput> {
    state
        .sessions
        .in_order()
        .into_iter()
        .rev()
        .find_map(|session| {
            let started = &session.started;
            if started.step != Some(StepName::DesignWorkflow) || started.workflow_step.is_some() {
                return None;
            }
            let structured = session.end.as_ref()?.structured.clone()?;
            serde_json::from_value::<DesignOutput>(structured).ok()
        })
}

/// The item the lead named for the card a build from the goal makes: its plan's first.
pub fn item(state: &RunState) -> Option<String> {
    of_run(state)?
        .plan
        .first()
        .map(|card| labels::slug(&card.item, 3))
        .filter(|item| !item.is_empty())
}

/// Why a run's name can't be used, or `None`.
pub fn name_problem(name: &str, examples: &[String]) -> Option<String> {
    let words: Vec<&str> = name.split('-').collect();
    let well_formed = !name.is_empty()
        && words.iter().all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_alphanumeric() && !c.is_uppercase())
        });
    if !well_formed {
        return Some(format!(
            "name `{name}` isn't lowercase words joined with `-`, such as `fix-slugify-median`"
        ));
    }
    if !(2..=4).contains(&words.len()) {
        return Some(format!(
            "name `{name}` has {} words; use two to four about the task",
            words.len()
        ));
    }
    if name.width() > NAME_CELLS {
        return Some(format!("name `{name}` is wider than {NAME_CELLS} cells"));
    }
    if examples.iter().any(|example| example == name) {
        return Some(format!(
            "name `{name}` is an example's; name the run for its task"
        ));
    }
    None
}

/// The brief's identity on its workflow: the run's name and description, and the example it
/// started from.
pub fn apply_identity(workflow: &mut Workflow, brief: &DesignOutput) {
    workflow.name = brief.name.trim().to_owned();
    workflow.description = brief.description.trim().to_owned();
    workflow.based_on.clone_from(&brief.based_on);
}

/// A name for a run the lead didn't name, from the first words of intake's summary or your
/// request: `add-a-json-flag` from "Add a --json flag to export".
pub fn name_from(words: &str) -> String {
    let slug = labels::slug(words, 4);
    match slug.split('-').count() {
        0 | 1 if slug.is_empty() => "untitled-run".to_owned(),
        1 => format!("{slug}-run"),
        _ => fit_name(&slug),
    }
}

/// A description that fits its line: the first line, cut with `…`.
pub fn fit_description(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if line.width() <= DESCRIPTION_CELLS {
        return line.to_owned();
    }
    let mut out = String::new();
    for c in line.chars() {
        if format!("{out}{c}…").width() > DESCRIPTION_CELLS {
            break;
        }
        out.push(c);
    }
    format!("{}…", out.trim_end())
}

/// `name-n`, within the name's width.
pub fn with_number(name: &str, n: u32) -> String {
    let suffix = format!("-{n}");
    let room = NAME_CELLS.saturating_sub(suffix.width());
    let mut base = String::new();
    for word in name.split('-') {
        let next = if base.is_empty() {
            word.to_owned()
        } else {
            format!("{base}-{word}")
        };
        if next.width() > room {
            break;
        }
        base = next;
    }
    format!("{base}{suffix}")
}

fn fit_name(slug: &str) -> String {
    let mut out = String::new();
    for word in slug.split('-') {
        let next = if out.is_empty() {
            word.to_owned()
        } else {
            format!("{out}-{word}")
        };
        if next.width() > NAME_CELLS {
            break;
        }
        out = next;
    }
    if out.contains('-') {
        out
    } else {
        format!("{out}-run")
    }
}

/// One or two words, each starting with a capital or a character with no case.
fn title_case(title: &str) -> bool {
    let words: Vec<&str> = title.split_whitespace().collect();
    (1..=2).contains(&words.len())
        && words
            .iter()
            .all(|word| word.chars().next().is_some_and(|c| !c.is_lowercase()))
}

fn is_checkpoint(step: &Step) -> bool {
    step.kind == StepKind::Checkpoint && step.checkpoint != Some(CheckpointKind::ChangeRequest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use argonauts_core::events::goal::Criterion;
    use argonauts_core::steps::design::BriefCard;

    use crate::workflow::yaml;

    const FIX: &str = r#"name: fix-slugify-median
description: Fix slugify's dashes and median's even and empty lists, with tests
phases:
  - title: Fix
    detail: one owner per function, each in its own worktree, tests as the gate
    standard: do
    steps:
      - { kind: build, input: plan, label: "fix:{item}", review_in: Review }
  - title: Review
    detail: a reviewer from another company reads each fix
    standard: check
    steps: []
  - title: Merge
    detail: the merge into main
    standard: decide
    steps:
      - { kind: merge, label: "merge:main" }
"#;

    fn examples() -> Vec<String> {
        ["peer-crew", "solo"].map(str::to_owned).to_vec()
    }

    fn brief() -> DesignOutput {
        DesignOutput {
            name: "fix-slugify-median".to_owned(),
            description: "Fix slugify's dashes and median's even and empty lists, with tests"
                .to_owned(),
            based_on: Some("peer-crew".to_owned()),
            why: "each bug has one clear fix, so no drafting".to_owned(),
            params: Vec::new(),
            workflow: Some(FIX.to_owned()),
            done_when: vec![Criterion::new("G1", "median([]) raises ValueError")],
            plan: vec![BriefCard {
                item: "slugify".to_owned(),
                task: "collapse punctuation into one dash".to_owned(),
            }],
            questions: Vec::new(),
            first_attempt: None,
        }
    }

    fn workflow() -> Workflow {
        yaml::parse(FIX).unwrap()
    }

    #[test]
    fn a_good_brief_passes() {
        assert_eq!(
            problems(&brief(), &workflow(), &examples()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_name_and_description_are_checked() {
        let mut missing = brief();
        missing.name = String::new();
        assert!(problems(&missing, &workflow(), &examples())[0].contains("name ``"));
        let mut template = brief();
        template.name = "peer-crew".to_owned();
        assert!(problems(&template, &workflow(), &examples())[0].contains("an example's"));
        let mut long = brief();
        long.description = "x".repeat(73);
        assert!(problems(&long, &workflow(), &examples())[0].contains("72"));
        let mut unknown = brief();
        unknown.based_on = Some("nonesuch".to_owned());
        assert!(problems(&unknown, &workflow(), &examples())[0].contains("names no example"));
    }

    #[test]
    fn seven_phases_and_a_lone_checkpoint_are_refused() {
        let mut seven = workflow();
        let merge = seven.phases[2].clone();
        for n in 0..4 {
            let mut more = merge.clone();
            more.name = format!("More {n}");
            seven.phases.push(more);
        }
        assert!(
            problems(&brief(), &seven, &examples())
                .iter()
                .any(|problem| problem.contains("7 phases"))
        );

        let lone: Workflow = yaml::parse(
            "name: x\nphases:\n  - title: Plan\n    detail: you approve the plan\n    \
             standard: plan\n    steps:\n      - { kind: checkpoint, checkpoint: plan }\n",
        )
        .unwrap();
        assert!(
            problems(&brief(), &lone, &examples())
                .iter()
                .any(|problem| problem.contains("only a checkpoint"))
        );
    }

    #[test]
    fn the_done_when_list_has_one_to_eight_criteria() {
        let mut none = brief();
        none.done_when.clear();
        assert!(problems(&none, &workflow(), &examples())[0].contains("0 criteria"));
        let mut twice = brief();
        twice.done_when.push(Criterion::new("G1", "again"));
        assert!(problems(&twice, &workflow(), &examples())[0].contains("used twice"));
    }

    #[test]
    fn phase_titles_are_their_own_and_in_title_case() {
        let mut twice = workflow();
        twice.phases[2].name = "review".to_owned();
        let found = problems(&brief(), &twice, &examples());
        assert!(
            found
                .iter()
                .any(|problem| problem.contains("title another phase has")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|problem| problem.contains("Title Case")),
            "{found:?}"
        );
        let mut long = workflow();
        long.phases[0].name = "Fix The Bugs".to_owned();
        assert!(
            problems(&brief(), &long, &examples())
                .iter()
                .any(|problem| problem.contains("Title Case"))
        );
        let mut two_words = workflow();
        two_words.phases[1].name = "Final Check".to_owned();
        assert_eq!(
            problems(&brief(), &two_words, &examples()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_plan_names_the_one_card_a_goal_build_makes() {
        let mut two = brief();
        two.plan.push(BriefCard {
            item: "median".to_owned(),
            task: "average the middle pair".to_owned(),
        });
        assert!(
            problems(&two, &workflow(), &examples())
                .iter()
                .any(|problem| problem.contains("lists 2 cards"))
        );
    }

    #[test]
    fn a_repeated_name_takes_a_number_and_keeps_its_width() {
        assert_eq!(with_number("fix-slugify-median", 2), "fix-slugify-median-2");
        let long = "speed-up-the-csv-export-for-big-tables";
        let numbered = with_number(long, 12);
        assert!(
            numbered.ends_with("-12") && numbered.width() <= NAME_CELLS,
            "{numbered}"
        );
    }

    #[test]
    fn names_come_from_the_first_words_when_the_lead_named_nothing() {
        assert_eq!(name_from("Add a --json flag to export"), "add-a-json-flag");
        assert_eq!(name_from("refactor"), "refactor-run");
        assert_eq!(name_from("   "), "untitled-run");
        // Whatever it's made from, it passes the rule a lead's name must.
        assert_eq!(name_problem(&name_from("Fix Slugify's Dashes"), &[]), None);
        let long = fit_description(&"word ".repeat(30));
        assert!(
            long.width() <= DESCRIPTION_CELLS && long.ends_with('…'),
            "{long}"
        );
    }
}
