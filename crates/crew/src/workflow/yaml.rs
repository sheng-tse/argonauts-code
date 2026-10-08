//! Workflows as YAML text: built-in files, your own files, the lead's composed workflows, and
//! the running workflow shown to the lead at a phase end.

use argonauts_core::workflow::{Step, Workflow};

/// Unknown keys are refused, like every workflow source.
pub fn parse(text: &str) -> Result<Workflow, String> {
    serde_saphyr::from_str(text).map_err(|err| err.to_string())
}

/// One phase's steps, such as a goal attempt the lead planned.
pub fn parse_steps(text: &str) -> Result<Vec<Step>, String> {
    serde_saphyr::from_str(text).map_err(|err| err.to_string())
}

pub fn print(workflow: &Workflow) -> Result<String, String> {
    serde_saphyr::to_string(workflow).map_err(|err| err.to_string())
}

/// The running workflow for `DesignInput.current`, with each finished phase marked by a
/// `# finished` comment on its first line.
pub fn print_marked(workflow: &Workflow, finished_phases: &[u32]) -> Result<String, String> {
    let text = print(workflow)?;
    let mut out = String::with_capacity(text.len() + finished_phases.len() * 12);
    let mut in_phases = false;
    let mut phase_indent = None;
    let mut phase = 0u32;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let mut marked = false;
        if line.starts_with("phases:") {
            in_phases = true;
        } else if in_phases && indent == 0 && !line.starts_with('-') {
            in_phases = false;
        } else if in_phases && line.trim_start().starts_with("- ") {
            let first = *phase_indent.get_or_insert(indent);
            if indent == first {
                marked = finished_phases.contains(&phase);
                phase += 1;
            }
        }
        out.push_str(line);
        if marked {
            out.push_str("  # finished");
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW_PANEL: &str = r#"
name: review-panel
description: Two reviewers from other providers; an adjudicator only if they disagree
params: { target: diff, reviewers: 2 }
phases:
  - name: Review
    standard: check
    steps:
      - kind: fan_out
        role: reviewer
        count: "{{reviewers}}"
        blind: true
        different_family_from: author
  - name: Adjudicate
    standard: decide
    steps:
      - kind: judge
        role: adjudicator
        when: reviewers_disagree
"#;

    #[test]
    fn a_workflow_round_trips_through_yaml() {
        let workflow = parse(REVIEW_PANEL).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(workflow.phases.len(), 2);
        assert_eq!(
            workflow.phases[0].steps[0].count,
            Some(argonauts_core::workflow::Count::Param(
                "{{reviewers}}".to_owned()
            ))
        );
        let text = print(&workflow).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(parse(&text), Ok(workflow));
    }

    #[test]
    fn unknown_keys_are_refused() {
        let text = REVIEW_PANEL.replace("blind: true", "blindly: true");
        assert!(parse(&text).is_err());
    }

    #[test]
    fn finished_phases_are_marked() {
        let workflow = parse(REVIEW_PANEL).unwrap_or_else(|err| panic!("{err}"));
        let text = print_marked(&workflow, &[0]).unwrap_or_else(|err| panic!("{err}"));
        let marked: Vec<_> = text
            .lines()
            .filter(|line| line.ends_with("# finished"))
            .collect();
        assert_eq!(marked.len(), 1, "{text}");
        assert!(marked[0].contains("Review"), "{text}");
        // The marks are comments, so the text still parses to the same workflow.
        assert_eq!(parse(&text), Ok(workflow));
    }
}
