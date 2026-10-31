//! The Captain-or-crew rules: Captain or crew, computed by code from the main agent's estimate. The
//! agent supplies facts only and never picks the mode, though your own words may, once code has
//! checked them.

use argonauts_core::runs::events::run::{CaptainOrCrewDecision, RunMode};
use argonauts_core::settings::Settings;
use argonauts_core::settings::protocol::{
    CaptainKind, CaptainOrCrewKind, CaptainOrCrewOtherwise, ForcedMode,
};
use argonauts_core::workflows::steps::intake::ChangeEstimate;
use argonauts_learning::learned_workflows::{LearnedWorkflowMode, LoadedLearnedWorkflow};

/// What your request's words chose, after code checked them: the mode, and where Captain works.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Asked {
    pub mode: Option<RunMode>,
    /// `Checked` when the words ask for a worktree or a lone review, `Direct` when they ask for
    /// your checkout and `captain.kind` allows it.
    pub captain: Option<CaptainKind>,
}

/// The choice without a learned workflow: the file and label rules.
pub fn decide(estimate: &ChangeEstimate, settings: &Settings) -> CaptainOrCrewDecision {
    decide_with(estimate, settings, None, Asked::default())
}

/// The choice with the learned workflow whose `match` fits the task, as the app found it in the
/// main checkout. With `captain_or_crew.use_learned`, a learned workflow that names a mode decides
/// Captain or crew. Your words beat every setting: Ultracode, a forced mode, and a learned
/// workflow.
pub fn decide_with(
    estimate: &ChangeEstimate,
    settings: &Settings,
    learned_workflow: Option<&LoadedLearnedWorkflow>,
    asked: Asked,
) -> CaptainOrCrewDecision {
    let mut decision = match asked.mode {
        Some(mode) => decision(
            mode,
            false,
            CaptainOrCrewDecision::AS_YOU_ASKED,
            String::new(),
        ),
        None => by_the_rules(estimate, settings, learned_workflow),
    };
    decision.captain = captain_kind(&decision, settings, asked.captain);
    // Only a checked task can need you, so Captain first then crew run direct is plain Captain.
    if decision.captain == Some(CaptainKind::Direct) && decision.move_to_crew {
        decision.move_to_crew = false;
        decision.reason = decision.reason.replace(CAPTAIN_THEN_CREW, DIRECT_INSTEAD);
    }
    decision
}

const CAPTAIN_THEN_CREW: &str = "Captain first, then crew if the task needs you";
const DIRECT_INSTEAD: &str = "Captain in your checkout, so it won't become a crew run";

/// Where a Captain run works. Your words asking for a worktree and Captain settings that need one
/// make it checked. Captain you chose follows `captain.kind`, and Captain the rules picked follows
/// `captain_or_crew.captain_kind`.
fn captain_kind(
    decision: &CaptainOrCrewDecision,
    settings: &Settings,
    asked: Option<CaptainKind>,
) -> Option<CaptainKind> {
    if decision.mode != RunMode::Captain {
        return None;
    }
    if asked == Some(CaptainKind::Checked) || settings.captain.needs_a_worktree() {
        return Some(CaptainKind::Checked);
    }
    if asked == Some(CaptainKind::Direct) {
        return Some(settings.captain.kind);
    }
    let chosen = decision.as_you_asked() || decision.rule == FORCE;
    Some(match settings.captain_or_crew.captain_kind {
        _ if chosen => settings.captain.kind,
        CaptainOrCrewKind::Checked => CaptainKind::Checked,
        CaptainOrCrewKind::Direct => CaptainKind::Direct,
        CaptainOrCrewKind::AsCaptain => settings.captain.kind,
    })
}

/// The rule a workflow's forced mode fires, which counts as Captain you chose.
const FORCE: &str = "force";

fn by_the_rules(
    estimate: &ChangeEstimate,
    settings: &Settings,
    learned_workflow: Option<&LoadedLearnedWorkflow>,
) -> CaptainOrCrewDecision {
    let rules = &settings.captain_or_crew;
    if let Some(crew) = ultracode(settings) {
        return crew;
    }
    if let Some(forced) = rules.force {
        let mode = match forced {
            ForcedMode::Captain => RunMode::Captain,
            ForcedMode::Crew => RunMode::Crew,
        };
        return decision(
            mode,
            false,
            FORCE,
            format!("captain_or_crew.force is {}", name(mode)),
        );
    }
    if rules.use_learned
        && let Some(learned_workflow) = learned_workflow
        && let Some(mode) = learned_workflow.learned_workflow.mode
    {
        let mode = match mode {
            LearnedWorkflowMode::Captain => RunMode::Captain,
            LearnedWorkflowMode::Crew => RunMode::Crew,
        };
        let reference = learned_workflow.reference();
        return decision(
            mode,
            false,
            &format!("learned:{}", reference.name),
            format!("the learned workflow {reference} runs {}", name(mode)),
        );
    }

    let files = file_count(estimate);
    let small = files <= rules.captain_max_files;
    let tested = estimate.tests_exist || !rules.captain_requires_tests;
    if small && tested {
        let tests = if estimate.tests_exist {
            ", and tests exist"
        } else {
            ""
        };
        return decision(
            RunMode::Captain,
            false,
            "captain_max_files",
            format!(
                "{}, at most {}{tests}",
                files_phrase(files),
                rules.captain_max_files
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
            rules.captain_max_files,
            rules.crew_min_files
        )
    };
    match rules.otherwise {
        CaptainOrCrewOtherwise::CaptainThenCrew => decision(
            RunMode::Captain,
            true,
            "otherwise",
            format!("{why}: Captain first, then crew if the task needs you"),
        ),
        CaptainOrCrewOtherwise::Captain => decision(RunMode::Captain, false, "otherwise", why),
        CaptainOrCrewOtherwise::Crew => decision(RunMode::Crew, false, "otherwise", why),
    }
}

/// The crew, while ultracode is on with a crew workflow in use, whatever else would decide: the
/// conversation's estimate, a task from the command line, or a follow-up. You turned it on
/// yourself, so it wins over a learned workflow's mode. In Captain the CLI you chose keeps the
/// work, and runs its own workflow, so Captain stays as Captain.
pub fn ultracode(settings: &Settings) -> Option<CaptainOrCrewDecision> {
    let captain = settings.captain_or_crew.force == Some(ForcedMode::Captain);
    (settings.ultracode && !captain).then(|| {
        decision(
            RunMode::Crew,
            false,
            "ultracode",
            "Ultracode is on".to_owned(),
        )
    })
}

fn decision(
    mode: RunMode,
    move_to_crew: bool,
    rule: &str,
    reason: String,
) -> CaptainOrCrewDecision {
    CaptainOrCrewDecision {
        mode,
        move_to_crew,
        rule: rule.to_owned(),
        reason,
        captain: None,
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
        RunMode::Captain => "captain",
        RunMode::Crew => "crew",
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::settings::workflows::{OrchestratorDesign, OrchestratorDesigns};

    use super::*;

    /// `(settings, files, tests exist, labels)`, then the expected `(mode, move_to_crew, rule)`.
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
    fn captain_or_crew_rules_table() {
        let defaults = Settings::default();
        let mut forced_crew = Settings::default();
        forced_crew.captain_or_crew.force = Some(ForcedMode::Crew);
        let mut forced_captain = Settings::default();
        forced_captain.captain_or_crew.force = Some(ForcedMode::Captain);
        let mut untested_ok = Settings::default();
        untested_ok.captain_or_crew.captain_requires_tests = false;
        let mut middle_crew = Settings::default();
        middle_crew.captain_or_crew.otherwise = CaptainOrCrewOtherwise::Crew;
        let mut middle_captain = Settings::default();
        middle_captain.captain_or_crew.otherwise = CaptainOrCrewOtherwise::Captain;
        let ultracode = Settings {
            ultracode: true,
            ..Settings::default()
        };
        let mut ultracode_over_captain = forced_captain.clone();
        ultracode_over_captain.ultracode = true;

        let table: Vec<Row> = vec![
            (
                &defaults,
                1,
                true,
                &[],
                RunMode::Captain,
                false,
                "captain_max_files",
            ),
            (
                &defaults,
                3,
                true,
                &[],
                RunMode::Captain,
                false,
                "captain_max_files",
            ),
            (
                &defaults,
                3,
                true,
                &["security"],
                RunMode::Captain,
                false,
                "captain_max_files",
            ),
            (
                &defaults,
                2,
                false,
                &[],
                RunMode::Captain,
                true,
                "otherwise",
            ),
            (
                &untested_ok,
                2,
                false,
                &[],
                RunMode::Captain,
                false,
                "captain_max_files",
            ),
            (&defaults, 4, true, &[], RunMode::Captain, true, "otherwise"),
            (&defaults, 7, true, &[], RunMode::Captain, true, "otherwise"),
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
                RunMode::Captain,
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
                &middle_captain,
                5,
                true,
                &[],
                RunMode::Captain,
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
                &ultracode_over_captain,
                1,
                true,
                &[],
                RunMode::Captain,
                false,
                "force",
            ),
            (
                &forced_captain,
                30,
                false,
                &["security"],
                RunMode::Captain,
                false,
                "force",
            ),
        ];
        for (settings, files, tests, labels, mode, ask_you, rule) in table {
            let got = decide(&estimate(files, tests, labels), settings);
            let row = format!("{files} files, tests {tests}, labels {labels:?}");
            assert_eq!(got.mode, mode, "{row}: {got:?}");
            assert_eq!(got.move_to_crew, ask_you, "{row}: {got:?}");
            assert_eq!(got.rule, rule, "{row}: {got:?}");
            assert!(
                !got.reason.is_empty() && !got.reason.contains('\n'),
                "{row}"
            );
        }
    }

    /// Your words beat every setting that picks a mode: Ultracode, a forced mode, a learned
    /// workflow.
    #[test]
    fn words_beat_ultracode_a_forced_mode_and_a_learned_workflow() {
        let ultracode = Settings {
            ultracode: true,
            ..Settings::default()
        };
        let mut forced_crew = Settings::default();
        forced_crew.captain_or_crew.force = Some(ForcedMode::Crew);
        let text = "name: api-feature\nversion: 1\ndescription: HTTP endpoints\nmode: crew\n\
                    match: { task_types: [bugfix] }\n";
        let path = std::path::Path::new("v1.yaml");
        let learned_workflow = LoadedLearnedWorkflow {
            learned_workflow: argonauts_learning::learned_workflows::parse(path, text).unwrap(),
            path: path.to_owned(),
            text: text.to_owned(),
            sha256: String::new(),
        };
        let captain = Asked {
            mode: Some(RunMode::Captain),
            captain: None,
        };
        let defaults = Settings::default();
        let small = estimate(1, true, &[]);
        for (settings, learned_workflow) in [
            (&ultracode, None),
            (&forced_crew, None),
            (&defaults, Some(&learned_workflow)),
        ] {
            assert_eq!(
                decide_with(&small, settings, learned_workflow, Asked::default()).mode,
                RunMode::Crew
            );
            let asked = decide_with(&small, settings, learned_workflow, captain);
            assert_eq!(asked.mode, RunMode::Captain);
            assert!(asked.as_you_asked(), "{asked:?}");
            assert_eq!(asked.reason, "");
        }
    }

    /// Captain you chose works where `captain.kind` says, Captain the rules picked where
    /// `captain_or_crew.captain_kind` says, and anything that needs a worktree is checked.
    #[test]
    fn where_captain_works() {
        use CaptainKind::{Checked, Direct};
        let defaults = Settings::default();
        let mut captain_checked = Settings::default();
        captain_checked.captain.kind = Checked;
        let mut rules_direct = Settings::default();
        rules_direct.captain_or_crew.captain_kind = CaptainOrCrewKind::Direct;
        let mut as_captain = Settings::default();
        as_captain.captain_or_crew.captain_kind = CaptainOrCrewKind::AsCaptain;
        let mut reviewed = Settings::default();
        reviewed.captain.review = true;
        let mut forced_captain = Settings::default();
        forced_captain.captain_or_crew.force = Some(ForcedMode::Captain);
        let words = |captain| Asked {
            mode: Some(RunMode::Captain),
            captain,
        };
        let rules = |captain| Asked {
            mode: None,
            captain,
        };
        let small = estimate(1, true, &[]);
        let untested = estimate(1, false, &[]);
        let large = estimate(20, true, &[]);
        let table: Vec<(&Settings, &ChangeEstimate, Asked, Option<CaptainKind>)> = vec![
            (&defaults, &small, words(None), Some(Direct)),
            (&captain_checked, &small, words(None), Some(Checked)),
            (&defaults, &small, words(Some(Checked)), Some(Checked)),
            (&captain_checked, &small, words(Some(Direct)), Some(Checked)),
            (&defaults, &small, rules(None), Some(Checked)),
            (&rules_direct, &small, rules(None), Some(Direct)),
            (&as_captain, &small, rules(None), Some(Direct)),
            (&rules_direct, &small, rules(Some(Checked)), Some(Checked)),
            // Captain first then crew is checked by default, and plain Captain when picks run
            // direct.
            (&defaults, &untested, rules(None), Some(Checked)),
            (&rules_direct, &untested, rules(None), Some(Direct)),
            (&reviewed, &small, words(None), Some(Checked)),
            (&forced_captain, &small, rules(None), Some(Direct)),
            (&defaults, &large, rules(None), None),
        ];
        for (settings, estimate, asked, captain) in table {
            let got = decide_with(estimate, settings, None, asked);
            assert_eq!(got.captain, captain, "{asked:?} {got:?}");
        }
        let plain = decide_with(&untested, &rules_direct, None, rules(None));
        assert!(!plain.move_to_crew, "{plain:?}");
        assert!(plain.reason.ends_with(DIRECT_INSTEAD), "{}", plain.reason);
    }

    #[test]
    fn ultracode_has_the_main_agent_design_the_crews_workflow() {
        let mut settings = Settings::default();
        settings.workflows.orchestrator_designs = false.into();
        assert_eq!(settings.orchestrator_designs(), OrchestratorDesign::Off);
        settings.ultracode = true;
        assert_eq!(settings.orchestrator_designs(), OrchestratorDesign::Size);
        settings.workflows.orchestrator_designs = OrchestratorDesigns::ANY;
        assert_eq!(settings.orchestrator_designs(), OrchestratorDesign::Any);
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
            "1 file, but no tests exist: Captain first, then crew if the task needs you"
        );
        assert_eq!(
            decide(&estimate(5, true, &["security"]), &settings).reason,
            "labeled security"
        );
    }
}
