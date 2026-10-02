//! Triage rules: solo or crew, computed by code from the default agent's estimate. The agent
//! supplies facts only and never picks the mode.

use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{ForcedMode, TriageMiddle};
use argonauts_core::steps::intake::ChangeEstimate;
use argonauts_learning::recipes::{LoadedRecipe, RecipeMode};

/// Triage without a recipe: the file and label rules.
pub fn decide(estimate: &ChangeEstimate, settings: &Settings) -> TriageDecision {
    decide_with(estimate, settings, None)
}

/// Triage with the recipe whose `match` fits the task, as the app found it in the main
/// checkout. With `triage.use_recipes`, a recipe that names a mode decides solo or crew.
pub fn decide_with(
    estimate: &ChangeEstimate,
    settings: &Settings,
    recipe: Option<&LoadedRecipe>,
) -> TriageDecision {
    let rules = &settings.triage;
    if let Some(forced) = rules.force {
        let mode = match forced {
            ForcedMode::Solo => RunMode::Solo,
            ForcedMode::Crew => RunMode::Crew,
        };
        return decision(
            mode,
            false,
            "force",
            format!("triage.force is {}", name(mode)),
        );
    }
    if rules.use_recipes
        && let Some(recipe) = recipe
        && let Some(mode) = recipe.recipe.mode
    {
        let mode = match mode {
            RecipeMode::Solo => RunMode::Solo,
            RecipeMode::Crew => RunMode::Crew,
        };
        let reference = recipe.reference();
        return decision(
            mode,
            false,
            &format!("recipe:{}", reference.name),
            format!("the {reference} recipe runs {}", name(mode)),
        );
    }

    let files = file_count(estimate);
    let small = files <= rules.solo_max_files;
    let tested = estimate.tests_exist || !rules.solo_requires_tests;
    if small && tested {
        let tests = if estimate.tests_exist {
            ", and tests exist"
        } else {
            ""
        };
        return decision(
            RunMode::Solo,
            false,
            "solo_max_files",
            format!(
                "{}, at most {}{tests}",
                files_phrase(files),
                rules.solo_max_files
            ),
        );
    }
    if files >= rules.crew_min_files {
        return decision(
            RunMode::Crew,
            false,
            "crew_min_files",
            format!("{}, {} or more", files_phrase(files), rules.crew_min_files),
        );
    }
    if let Some(label) = crew_label(estimate, &rules.crew_labels) {
        return decision(
            RunMode::Crew,
            false,
            "crew_labels",
            format!("labeled {label}"),
        );
    }

    let why = if small {
        format!("{}, but no tests exist", files_phrase(files))
    } else {
        format!(
            "{}, between {} and {}",
            files_phrase(files),
            rules.solo_max_files,
            rules.crew_min_files
        )
    };
    match rules.otherwise {
        TriageMiddle::SoloThenCrew => decision(
            RunMode::Solo,
            true,
            "otherwise",
            format!("{why}: solo first, then crew if the card escalates"),
        ),
        TriageMiddle::Solo => decision(RunMode::Solo, false, "otherwise", why),
        TriageMiddle::Crew => decision(RunMode::Crew, false, "otherwise", why),
    }
}

fn decision(mode: RunMode, escalate_to_crew: bool, rule: &str, reason: String) -> TriageDecision {
    TriageDecision {
        mode,
        escalate_to_crew,
        rule: rule.to_owned(),
        reason,
    }
}

/// The listed files may be only a sample, so the larger of the two counts wins.
fn file_count(estimate: &ChangeEstimate) -> u32 {
    let listed = u32::try_from(estimate.files_likely.len()).unwrap_or(u32::MAX);
    estimate.file_count.max(listed)
}

fn crew_label<'a>(estimate: &'a ChangeEstimate, crew_labels: &[String]) -> Option<&'a str> {
    estimate
        .risk_labels
        .iter()
        .map(|label| label.trim())
        .find(|label| {
            crew_labels
                .iter()
                .any(|crew| crew.trim().eq_ignore_ascii_case(label))
        })
}

fn files_phrase(files: u32) -> String {
    match files {
        1 => "1 file".to_owned(),
        n => format!("{n} files"),
    }
}

fn name(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Solo => "solo",
        RunMode::Crew => "crew",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(settings, files, tests exist, labels)`, then the expected `(mode, escalate, rule)`.
    type Row<'a> = (
        &'a Settings,
        u32,
        bool,
        &'a [&'a str],
        RunMode,
        bool,
        &'a str,
    );

    fn estimate(files: u32, tests: bool, labels: &[&str]) -> ChangeEstimate {
        ChangeEstimate {
            task_type: "bugfix".to_owned(),
            summary: "fix the thing".to_owned(),
            files_likely: Vec::new(),
            file_count: files,
            risk_labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            tests_exist: tests,
            unknowns: Vec::new(),
        }
    }

    #[test]
    fn triage_rules_table() {
        let defaults = Settings::default();
        let mut forced_crew = Settings::default();
        forced_crew.triage.force = Some(ForcedMode::Crew);
        let mut forced_solo = Settings::default();
        forced_solo.triage.force = Some(ForcedMode::Solo);
        let mut untested_ok = Settings::default();
        untested_ok.triage.solo_requires_tests = false;
        let mut middle_crew = Settings::default();
        middle_crew.triage.otherwise = TriageMiddle::Crew;
        let mut middle_solo = Settings::default();
        middle_solo.triage.otherwise = TriageMiddle::Solo;

        let table: Vec<Row> = vec![
            (
                &defaults,
                1,
                true,
                &[],
                RunMode::Solo,
                false,
                "solo_max_files",
            ),
            (
                &defaults,
                3,
                true,
                &[],
                RunMode::Solo,
                false,
                "solo_max_files",
            ),
            (
                &defaults,
                3,
                true,
                &["security"],
                RunMode::Solo,
                false,
                "solo_max_files",
            ),
            (&defaults, 2, false, &[], RunMode::Solo, true, "otherwise"),
            (
                &untested_ok,
                2,
                false,
                &[],
                RunMode::Solo,
                false,
                "solo_max_files",
            ),
            (&defaults, 4, true, &[], RunMode::Solo, true, "otherwise"),
            (&defaults, 7, true, &[], RunMode::Solo, true, "otherwise"),
            (
                &defaults,
                8,
                true,
                &[],
                RunMode::Crew,
                false,
                "crew_min_files",
            ),
            (
                &defaults,
                20,
                false,
                &[],
                RunMode::Crew,
                false,
                "crew_min_files",
            ),
            (
                &defaults,
                5,
                true,
                &["Migration"],
                RunMode::Crew,
                false,
                "crew_labels",
            ),
            (
                &defaults,
                5,
                true,
                &["api", "architecture"],
                RunMode::Crew,
                false,
                "crew_labels",
            ),
            (
                &defaults,
                5,
                true,
                &["api"],
                RunMode::Solo,
                true,
                "otherwise",
            ),
            (
                &middle_crew,
                5,
                true,
                &[],
                RunMode::Crew,
                false,
                "otherwise",
            ),
            (
                &middle_solo,
                5,
                true,
                &[],
                RunMode::Solo,
                false,
                "otherwise",
            ),
            (&forced_crew, 1, true, &[], RunMode::Crew, false, "force"),
            (
                &forced_solo,
                30,
                false,
                &["security"],
                RunMode::Solo,
                false,
                "force",
            ),
        ];
        for (settings, files, tests, labels, mode, escalate, rule) in table {
            let got = decide(&estimate(files, tests, labels), settings);
            let row = format!("{files} files, tests {tests}, labels {labels:?}");
            assert_eq!(got.mode, mode, "{row}: {got:?}");
            assert_eq!(got.escalate_to_crew, escalate, "{row}: {got:?}");
            assert_eq!(got.rule, rule, "{row}: {got:?}");
            assert!(
                !got.reason.is_empty() && !got.reason.contains('\n'),
                "{row}"
            );
        }
    }

    #[test]
    fn listed_files_count_when_the_count_is_lower() {
        let mut sample = estimate(1, true, &[]);
        sample.files_likely = (0..9).map(|n| format!("src/f{n}.rs")).collect();
        let got = decide(&sample, &Settings::default());
        assert_eq!(
            (got.mode, got.rule.as_str()),
            (RunMode::Crew, "crew_min_files")
        );
        assert_eq!(got.reason, "9 files, 8 or more");
    }

    #[test]
    fn reasons_read_as_one_plain_line() {
        let settings = Settings::default();
        assert_eq!(
            decide(&estimate(2, true, &[]), &settings).reason,
            "2 files, at most 3, and tests exist"
        );
        assert_eq!(
            decide(&estimate(1, false, &[]), &settings).reason,
            "1 file, but no tests exist: solo first, then crew if the card escalates"
        );
        assert_eq!(
            decide(&estimate(5, true, &["security"]), &settings).reason,
            "labeled security"
        );
    }
}
