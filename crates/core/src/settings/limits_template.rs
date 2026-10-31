//! Sections 2 and 4 of the settings file `/setup` writes, the fallbacks and the limits, with
//! every line explained. Whoever writes the file gives the fallbacks' models, since they are
//! yours.

use std::fmt::Write as _;

use crate::base::names::ModelRef;

/// `[fallback.worker]` and `[fallback.reviewer]`. A role with no models to name is written
/// commented out, as an example to fill in.
pub fn fallbacks(workers: &[ModelRef], reviewers: &[ModelRef]) -> String {
    let mut text = String::from(
        "# 2. Fallbacks: a model can't be reached, its plan ran out, or a limit of yours was \
         reached.\n",
    );
    table(
        &mut text,
        "worker",
        workers,
        (
            1,
            Some("how many work at once. 2 or more do the same task side by side"),
        ),
        (
            "first_passing",
            "first_passing · judge · merge   (matters when together is 2 or more)",
        ),
    );
    text.push('\n');
    // Two reviews are worth merging, so two review at once wherever there are two to ask. The
    // workers' table above said what `together` is.
    let (together, keeps) = match reviewers {
        [_] => (
            1,
            "keep every finding   (matters when together is 2 or more)",
        ),
        _ => (2, "keep every finding from both"),
    };
    table(
        &mut text,
        "reviewer",
        reviewers,
        (together, None),
        ("merge", keeps),
    );
    text
}

fn table(
    text: &mut String,
    role: &str,
    order: &[ModelRef],
    (together, together_says): (usize, Option<&str>),
    (pick, pick_says): (&str, &str),
) {
    // With no models of yours to name, the table is an example, and sets nothing.
    let (off, order) = match order {
        [] => ("# ", "[\"<agent>/<model>\"]".to_owned()),
        models => {
            let quoted: Vec<String> = models.iter().map(|model| format!("\"{model}\"")).collect();
            ("", format!("[{}]", quoted.join(", ")))
        }
    };
    let pick = format!("\"{pick}\"");
    let _ = writeln!(text, "{off}[fallback.{role}]");
    let _ = writeln!(text, "{off}order    = {order}");
    let _ = match together_says {
        Some(says) => writeln!(text, "{off}together = {together:<18} # {says}"),
        None => writeln!(text, "{off}together = {together}"),
    };
    let _ = writeln!(text, "{off}pick     = {pick:<18} # {pick_says}");
}

/// `[limits]`, with every limit commented out: none is set unless you set one. It ends on a
/// real key holding the default, so the commented limits stay inside the table: a table a
/// later write adds goes after that key, never between the heading and its limits.
pub fn limits() -> &'static str {
    "# 4. Limits. None unless you set one. Spend and plan usage are shown either way.\n\
     [limits]\n\
     # spend_per_run = 5.00        # dollars\n\
     # spend_per_day = 20.00\n\
     # plan_usage    = 0.80        # stop using a provider once its plan window is this full\n\
     when_reached  = \"fallback\"    # fallback: use [fallback.*] · stop\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::settings::crew::CrewRole;
    use crate::settings::fallback::Pick;
    use crate::settings::limit::OnHit;

    fn models(names: &[&str]) -> Vec<ModelRef> {
        names.iter().map(|name| name.parse().unwrap()).collect()
    }

    #[test]
    fn the_sections_read_as_settings_and_set_what_they_say() {
        let workers = models(&["claude-code/sonnet-5-5", "codex/gpt-5.6-luna"]);
        let reviewers = models(&["codex/gpt-5.6", "claude-code/haiku-4-5"]);
        let text = format!("{}\n{}", fallbacks(&workers, &reviewers), limits());
        let settings: Settings = toml::from_str(&text).unwrap();

        let own = &settings.fallback.roles;
        assert_eq!(own[&CrewRole::Worker].order.len(), 2);
        assert_eq!(own[&CrewRole::Worker].together, Some(1));
        assert_eq!(own[&CrewRole::Worker].pick, Some(Pick::FirstPassing));
        assert_eq!(own[&CrewRole::Reviewer].together, Some(2));
        assert_eq!(own[&CrewRole::Reviewer].pick, Some(Pick::Merge));
        // The limits are all commented out, so the file sets none.
        assert_eq!(settings.limits, Settings::default().limits);
        assert!(!text.contains('\u{2014}'), "no em dashes");
    }

    /// The whole file, line for line.
    #[test]
    fn the_sections_are_the_pictures() {
        let workers = models(&[
            "claude-code/sonnet-5-5",
            "codex/gpt-5.6-luna",
            "ollama/qwen3-coder",
        ]);
        let reviewers = models(&["codex/gpt-5.6", "claude-code/haiku-4-5"]);
        assert_eq!(
            fallbacks(&workers, &reviewers),
            r#"# 2. Fallbacks: a model can't be reached, its plan ran out, or a limit of yours was reached.
[fallback.worker]
order    = ["claude-code/sonnet-5-5", "codex/gpt-5.6-luna", "ollama/qwen3-coder"]
together = 1                  # how many work at once. 2 or more do the same task side by side
pick     = "first_passing"    # first_passing · judge · merge   (matters when together is 2 or more)

[fallback.reviewer]
order    = ["codex/gpt-5.6", "claude-code/haiku-4-5"]
together = 2
pick     = "merge"            # keep every finding from both
"#
        );
        assert_eq!(
            limits(),
            r#"# 4. Limits. None unless you set one. Spend and plan usage are shown either way.
[limits]
# spend_per_run = 5.00        # dollars
# spend_per_day = 20.00
# plan_usage    = 0.80        # stop using a provider once its plan window is this full
when_reached  = "fallback"    # fallback: use [fallback.*] · stop
"#
        );
    }

    /// One reviewer to ask can't review beside another, and its table says when `pick` counts.
    #[test]
    fn one_reviewer_reviews_alone() {
        let text = fallbacks(&[], &models(&["codex/gpt-5.6"]));
        let settings: Settings = toml::from_str(&text).unwrap();
        assert_eq!(
            settings.fallback.roles[&CrewRole::Reviewer].together,
            Some(1)
        );
        assert!(text.contains("# keep every finding   (matters when together is 2 or more)"));
    }

    #[test]
    fn a_role_with_no_models_is_an_example_that_sets_nothing() {
        let text = fallbacks(&[], &[]);
        assert!(
            text.lines()
                .all(|line| line.is_empty() || line.starts_with('#'))
        );
        let settings: Settings = toml::from_str(&text).unwrap();
        assert!(settings.fallback.roles.is_empty());
    }

    #[test]
    fn each_commented_limit_is_a_real_key_with_that_value() {
        let on: String = limits()
            .lines()
            .map(|line| {
                line.strip_prefix("# ")
                    .filter(|rest| rest.contains(" = "))
                    .unwrap_or(line)
            })
            .map(|line| format!("{line}\n"))
            .collect();
        let settings: Settings = toml::from_str(&on).unwrap();
        assert_eq!(settings.limits.spend.spend_per_run, Some(5.0));
        assert_eq!(settings.limits.spend_per_day, Some(20.0));
        assert_eq!(settings.limits.spend.plan_usage, Some(0.8));
        assert_eq!(settings.limits.spend.when_reached, OnHit::Fallback);
    }
}
