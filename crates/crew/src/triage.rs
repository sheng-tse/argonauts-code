//! Triage rules: solo or crew, computed by code from the default agent's estimate. The agent
//! supplies facts only and never picks the mode, though your own words may, once code has
//! checked them.

use argonauts_core::events::run::{RunMode, TriageDecision};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{ForcedMode, SoloStyle, TriageMiddle, TriageSoloStyle};
use argonauts_core::steps::intake::ChangeEstimate;
use argonauts_learning::recipes::{LoadedRecipe, RecipeMode};

/// What your request's words chose, after code checked them: the mode, and where solo works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Asked {
    pub mode: Option<RunMode>,
    /// `Checked` when the words ask for a copy or a lone review, `Direct` when they ask for
    /// your checkout and `solo.style` allows it.
    pub solo: Option<SoloStyle>,
}

/// Triage without a recipe: the file and label rules.
pub fn decide(estimate: &ChangeEstimate, settings: &Settings) -> TriageDecision {
    decide_with(estimate, settings, None, Asked::default())
}

/// Triage with the recipe whose `match` fits the task, as the app found it in the main
/// checkout. With `triage.use_learned`, a recipe that names a mode decides solo or crew. Your
/// words beat every setting: All hands, a forced mode, and a recipe.
pub fn decide_with(
    estimate: &ChangeEstimate,
    settings: &Settings,
    recipe: Option<&LoadedRecipe>,
    asked: Asked,
) -> TriageDecision {
    let mut decision = match asked.mode {
        Some(mode) => decision(mode, false, TriageDecision::AS_YOU_ASKED, String::new()),
        None => by_the_rules(estimate, settings, recipe),
    };
    decision.solo = solo_style(&decision, settings, asked.solo);
    // Only a checked card can escalate, so solo first then crew run direct is plain solo.
    if decision.solo == Some(SoloStyle::Direct) && decision.escalate_to_crew {
        decision.escalate_to_crew = false;
        decision.reason = decision.reason.replace(SOLO_THEN_CREW, DIRECT_INSTEAD);
    }
    decision
}

const SOLO_THEN_CREW: &str = "solo first, then crew if the task needs you";
const DIRECT_INSTEAD: &str = "solo in your checkout, so it won't become a crew run";

/// Where a solo run works. Your words asking for a copy and solo settings that need a copy
/// make it checked. Solo you chose follows `solo.style`, and solo the rules picked follows
/// `triage.solo_style`.
fn solo_style(
    decision: &TriageDecision,
    settings: &Settings,
    asked: Option<SoloStyle>,
) -> Option<SoloStyle> {
    if decision.mode != RunMode::Solo {
        return None;
    }
    if asked == Some(SoloStyle::Checked) || settings.solo.needs_a_copy() {
        return Some(SoloStyle::Checked);
    }
    if asked == Some(SoloStyle::Direct) {
        return Some(settings.solo.style);
    }
    let chosen = decision.as_you_asked() || decision.rule == FORCE;
    Some(match settings.triage.solo_style {
        _ if chosen => settings.solo.style,
        TriageSoloStyle::Checked => SoloStyle::Checked,
        TriageSoloStyle::Direct => SoloStyle::Direct,
        TriageSoloStyle::AsSolo => settings.solo.style,
    })
}

/// The rule a preset's forced mode fires, which counts as solo you chose.
const FORCE: &str = "force";

fn by_the_rules(
    estimate: &ChangeEstimate,
    settings: &Settings,
    recipe: Option<&LoadedRecipe>,
) -> TriageDecision {
    let rules = &settings.triage;
    if let Some(crew) = ultracode(settings) {
        return crew;
    }
    if let Some(forced) = rules.force {
        let mode = match forced {
            ForcedMode::Solo => RunMode::Solo,
            ForcedMode::Crew => RunMode::Crew,
        };
        return decision(
            mode,
            false,
            FORCE,
            format!("solo_or_crew.force is {}", name(mode)),
        );
    }
    if rules.use_learned
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
            &format!("learned:{}", reference.name),
            format!("the learned workflow {reference} runs {}", name(mode)),
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
            format!("{why}: solo first, then crew if the task needs you"),
        ),
        TriageMiddle::Solo => decision(RunMode::Solo, false, "otherwise", why),
        TriageMiddle::Crew => decision(RunMode::Crew, false, "otherwise", why),
    }
}

/// The crew, while ultracode is on with a crew workflow in use, whatever else would decide:
/// the chat's triage, a task from the command line, or a follow-up. You turned it on yourself,
/// so it wins over a recipe's mode. In solo the CLI you chose keeps the work, and runs its own
/// workflow, so solo stays solo.
pub fn ultracode(settings: &Settings) -> Option<TriageDecision> {
    let solo = settings.triage.force == Some(ForcedMode::Solo);
    (settings.ultracode && !solo).then(|| {
        decision(
            RunMode::Crew,
            false,
            "ultracode",
            "Ultracode is on".to_owned(),
        )
    })
}

fn decision(mode: RunMode, escalate_to_crew: bool, rule: &str, reason: String) -> TriageDecision {
    TriageDecision {
        mode,
        escalate_to_crew,
        rule: rule.to_owned(),
        reason,
        solo: None,
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
    use argonauts_core::settings::workflows::{LeadDesign, LeadDesigns};

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
        let ultracode = Settings {
            ultracode: true,
            ..Settings::default()
        };
        let mut ultracode_over_solo = forced_solo.clone();
        ultracode_over_solo.ultracode = true;

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
            (&ultracode, 1, true, &[], RunMode::Crew, false, "ultracode"),
            (
                &ultracode,
                30,
                false,
                &["security"],
                RunMode::Crew,
                false,
                "ultracode",
            ),
            (
                &ultracode_over_solo,
                1,
                true,
                &[],
                RunMode::Solo,
                false,
                "force",
            ),
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

    /// Your words beat every setting that picks a mode: All hands, a forced mode, a recipe.
    #[test]
    fn words_beat_ultracode_a_forced_mode_and_a_recipe() {
        let ultracode = Settings {
            ultracode: true,
            ..Settings::default()
        };
        let mut forced_crew = Settings::default();
        forced_crew.triage.force = Some(ForcedMode::Crew);
        let text = "name: api-feature\nversion: 1\ndescription: HTTP endpoints\nmode: crew\n\
                    match: { task_types: [bugfix] }\n";
        let path = std::path::Path::new("v1.yaml");
        let recipe = LoadedRecipe {
            recipe: argonauts_learning::recipes::parse(path, text).unwrap(),
            path: path.to_owned(),
            text: text.to_owned(),
            sha256: String::new(),
        };
        let solo = Asked {
            mode: Some(RunMode::Solo),
            solo: None,
        };
        let defaults = Settings::default();
        let small = estimate(1, true, &[]);
        for (settings, recipe) in [
            (&ultracode, None),
            (&forced_crew, None),
            (&defaults, Some(&recipe)),
        ] {
            assert_eq!(
                decide_with(&small, settings, recipe, Asked::default()).mode,
                RunMode::Crew
            );
            let asked = decide_with(&small, settings, recipe, solo);
            assert_eq!(asked.mode, RunMode::Solo);
            assert!(asked.as_you_asked(), "{asked:?}");
            assert_eq!(asked.reason, "");
        }
    }

    /// Solo you chose works where `solo.style` says, solo the rules picked where
    /// `triage.solo_style` says, and anything that needs a copy is checked.
    #[test]
    fn where_solo_works() {
        use SoloStyle::{Checked, Direct};
        let defaults = Settings::default();
        let mut solo_checked = Settings::default();
        solo_checked.solo.style = Checked;
        let mut rules_direct = Settings::default();
        rules_direct.triage.solo_style = TriageSoloStyle::Direct;
        let mut as_solo = Settings::default();
        as_solo.triage.solo_style = TriageSoloStyle::AsSolo;
        let mut reviewed = Settings::default();
        reviewed.solo.review = true;
        let mut forced_solo = Settings::default();
        forced_solo.triage.force = Some(ForcedMode::Solo);
        let words = |solo| Asked {
            mode: Some(RunMode::Solo),
            solo,
        };
        let rules = |solo| Asked { mode: None, solo };
        let small = estimate(1, true, &[]);
        let untested = estimate(1, false, &[]);
        let large = estimate(20, true, &[]);
        let table: Vec<(&Settings, &ChangeEstimate, Asked, Option<SoloStyle>)> = vec![
            (&defaults, &small, words(None), Some(Direct)),
            (&solo_checked, &small, words(None), Some(Checked)),
            (&defaults, &small, words(Some(Checked)), Some(Checked)),
            (&solo_checked, &small, words(Some(Direct)), Some(Checked)),
            (&defaults, &small, rules(None), Some(Checked)),
            (&rules_direct, &small, rules(None), Some(Direct)),
            (&as_solo, &small, rules(None), Some(Direct)),
            (&rules_direct, &small, rules(Some(Checked)), Some(Checked)),
            // Solo first then crew is checked by default, and plain solo when picks run direct.
            (&defaults, &untested, rules(None), Some(Checked)),
            (&rules_direct, &untested, rules(None), Some(Direct)),
            (&reviewed, &small, words(None), Some(Checked)),
            (&forced_solo, &small, rules(None), Some(Direct)),
            (&defaults, &large, rules(None), None),
        ];
        for (settings, estimate, asked, solo) in table {
            let got = decide_with(estimate, settings, None, asked);
            assert_eq!(got.solo, solo, "{asked:?} {got:?}");
        }
        let plain = decide_with(&untested, &rules_direct, None, rules(None));
        assert!(!plain.escalate_to_crew, "{plain:?}");
        assert!(plain.reason.ends_with(DIRECT_INSTEAD), "{}", plain.reason);
    }

    #[test]
    fn ultracode_has_the_lead_design_the_crews_workflow() {
        let mut settings = Settings::default();
        settings.workflows.lead_designs = false.into();
        assert_eq!(settings.lead_designs(), LeadDesign::Off);
        settings.ultracode = true;
        assert_eq!(settings.lead_designs(), LeadDesign::Size);
        settings.workflows.lead_designs = LeadDesigns::ANY;
        assert_eq!(settings.lead_designs(), LeadDesign::Any);
        assert_eq!(
            decide(&estimate(1, true, &[]), &settings).reason,
            "Ultracode is on"
        );
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
            "1 file, but no tests exist: solo first, then crew if the task needs you"
        );
        assert_eq!(
            decide(&estimate(5, true, &["security"]), &settings).reason,
            "labeled security"
        );
    }
}
