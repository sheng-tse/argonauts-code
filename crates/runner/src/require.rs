//! Steps you require in every workflow, and a task list you fixed. Another part of Argonauts Code
//! reads them from `workflows/always.toml` and from a request's words; this is what the runner
//! does with them.

use argonauts_core::settings::crew::CrewRole;

use crate::file::{Step, WorkflowFile};
use crate::script::Meta;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Required {
    pub steps: Vec<RequiredStep>,
    /// The tasks to do, exactly these. Empty leaves the list to the step that plans.
    pub tasks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredStep {
    pub name: String,
    pub role: CrewRole,
    pub does: Option<String>,
    /// Where it goes: `before <step>`, `after <step>`, `first` or `last`. Without it, before
    /// the last step the judge takes, else at the end.
    pub when: Option<String>,
}

impl Required {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.tasks.is_empty()
    }

    /// The file with each required step it lacks put in its place.
    pub fn added_to(&self, file: &WorkflowFile) -> WorkflowFile {
        let mut file = file.clone();
        for required in &self.steps {
            if file
                .steps
                .iter()
                .any(|step| same(&step.name, &required.name))
            {
                continue;
            }
            let at = place(&file, required.when.as_deref());
            file.steps.insert(
                at,
                Step {
                    name: required.name.clone(),
                    role: required.role,
                    does: required.does.clone(),
                    for_each: None,
                    count: None,
                    independent: false,
                    not_the_author: false,
                    keep: false,
                    send_back_to: None,
                    then: None,
                    until: None,
                    max_rounds: None,
                    model: None,
                    kept_by: None,
                    own_workflow: false,
                    required: true,
                },
            );
        }
        file
    }

    /// The required steps a script has no phase for, by name.
    pub fn missing_from(&self, meta: &Meta) -> Vec<String> {
        self.steps
            .iter()
            .filter(|required| {
                !meta
                    .phases
                    .iter()
                    .any(|phase| same(&phase.name, &required.name))
            })
            .map(|required| required.name.clone())
            .collect()
    }
}

/// Names match whatever their case and punctuation, so `sign-off` finds `Sign off`.
fn same(a: &str, b: &str) -> bool {
    let plain = |name: &str| -> String {
        name.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    plain(a) == plain(b)
}

fn place(file: &WorkflowFile, when: Option<&str>) -> usize {
    let find = |name: &str| file.steps.iter().position(|step| same(&step.name, name));
    let before_sign_off = || {
        file.steps
            .iter()
            .rposition(|step| step.role == CrewRole::Judge)
            .unwrap_or(file.steps.len())
    };
    let Some(when) = when.map(str::trim) else {
        return before_sign_off();
    };
    if when.eq_ignore_ascii_case("first") {
        return 0;
    }
    if when.eq_ignore_ascii_case("last") {
        return file.steps.len();
    }
    if let Some(name) = strip_word(when, "before") {
        return find(name).unwrap_or_else(before_sign_off);
    }
    if let Some(name) = strip_word(when, "after") {
        return find(name).map_or_else(before_sign_off, |index| index + 1);
    }
    before_sign_off()
}

fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let head = text.get(..word.len())?;
    head.eq_ignore_ascii_case(word)
        .then(|| text[word.len()..].trim())
        .filter(|rest| !rest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::MetaPhase;

    fn benchmark(when: Option<&str>) -> Required {
        Required {
            steps: vec![RequiredStep {
                name: "Benchmark".to_owned(),
                role: CrewRole::Worker,
                does: Some("Run the benchmark and report the numbers.".to_owned()),
                when: when.map(str::to_owned),
            }],
            tasks: Vec::new(),
        }
    }

    fn spec_first() -> WorkflowFile {
        WorkflowFile::parse(include_str!("../tests/fixtures/spec-first.toml"))
            .expect("reads")
            .expect("has steps")
    }

    fn names(file: &WorkflowFile) -> Vec<&str> {
        file.steps.iter().map(|step| step.name.as_str()).collect()
    }

    #[test]
    fn a_required_step_goes_before_sign_off_unless_it_says_where() {
        let file = spec_first();
        let with = benchmark(Some("before sign-off")).added_to(&file);
        assert_eq!(
            names(&with),
            ["Spec", "Build", "Review", "Revise", "Benchmark", "Sign off"]
        );
        assert!(with.steps[4].required);
        assert!(with.problems().is_empty());
        assert_eq!(names(&benchmark(None).added_to(&file)), names(&with));
        let after = benchmark(Some("after Spec")).added_to(&file);
        assert_eq!(names(&after)[1], "Benchmark");
        assert_eq!(
            names(&benchmark(Some("last")).added_to(&file))[5],
            "Benchmark"
        );
    }

    #[test]
    fn a_step_the_file_already_has_is_not_added_twice() {
        let file = spec_first();
        let mut required = benchmark(None);
        required.steps[0].name = "sign-off".to_owned();
        assert_eq!(required.added_to(&file), file);
    }

    #[test]
    fn a_script_must_have_a_phase_for_each_required_step() {
        let mut meta = Meta {
            name: "x".to_owned(),
            description: "y".to_owned(),
            when_to_use: None,
            phases: vec![MetaPhase {
                name: "Build".to_owned(),
                detail: String::new(),
                model: None,
            }],
        };
        assert_eq!(benchmark(None).missing_from(&meta), ["Benchmark"]);
        meta.phases[0].name = "benchmark".to_owned();
        assert!(benchmark(None).missing_from(&meta).is_empty());
    }
}
