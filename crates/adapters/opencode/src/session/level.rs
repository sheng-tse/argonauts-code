//! How an effort level reaches OpenCode. OpenCode 2 takes it inside the model, as
//! `--model opencode/deepseek-v4.1-flash-free#high`: its `run` has no `--variant` (2.0.26
//! answers "Unrecognized flag: --variant" and exits 1, and 2.0.32's help lists none). OpenCode
//! 1.18 takes `--variant <level>`, as its `run.ts` reads it. A version that can't be read gets
//! OpenCode 2's form, as today's OpenCode is 2.x.

use argonauts_core::extract_version;

/// What `opencode run` is given for a model and a level: the `--model` value, and the flags
/// after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelArgs {
    pub model: Option<String>,
    pub flags: Vec<String>,
}

/// `model` with `level` where `version` takes it. A model whose levels are known has had
/// its level moved to one it takes, or dropped when it takes none, before this. With no
/// model named, OpenCode 2 has nowhere to put a level, so it goes.
pub(crate) fn model_args(
    model: Option<&str>,
    level: Option<&str>,
    version: Option<&str>,
) -> ModelArgs {
    let level = level.filter(|level| !level.is_empty());
    if takes_variant_flag(version) {
        return ModelArgs {
            model: model.map(str::to_owned),
            flags: level
                .map(|level| vec!["--variant".to_owned(), level.to_owned()])
                .unwrap_or_default(),
        };
    }
    let model = model.map(|model| match level {
        // A model typed with its variant already carries one.
        Some(level) if !model.contains('#') => format!("{model}#{level}"),
        _ => model.to_owned(),
    });
    ModelArgs {
        model,
        flags: Vec::new(),
    }
}

/// OpenCode 1.x reads `--variant`; 2.x doesn't have it.
fn takes_variant_flag(version: Option<&str>) -> bool {
    version
        .and_then(extract_version)
        .and_then(|version| version.split('.').next()?.parse::<u64>().ok())
        .is_some_and(|major| major < 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEEPSEEK: &str = "opencode/deepseek-v4.1-flash-free";

    #[test]
    fn opencode_2_takes_the_level_inside_the_model() {
        let args = model_args(Some(DEEPSEEK), Some("high"), Some("2.0.32"));
        assert_eq!(
            args.model.as_deref(),
            Some("opencode/deepseek-v4.1-flash-free#high")
        );
        assert!(args.flags.is_empty(), "never --variant on OpenCode 2");
    }

    #[test]
    fn an_unknown_version_is_taken_as_opencode_2() {
        let args = model_args(Some(DEEPSEEK), Some("max"), None);
        assert_eq!(
            args.model.as_deref(),
            Some("opencode/deepseek-v4.1-flash-free#max")
        );
        assert!(args.flags.is_empty());
    }

    #[test]
    fn opencode_1_18_keeps_its_variant_flag() {
        let args = model_args(Some(DEEPSEEK), Some("high"), Some("opencode 1.18.33"));
        assert_eq!(args.model.as_deref(), Some(DEEPSEEK));
        assert_eq!(args.flags, ["--variant", "high"]);
    }

    #[test]
    fn a_model_with_no_level_gets_no_mark() {
        let args = model_args(Some("ollama/qwen3-coder:30b-64k"), None, Some("2.0.32"));
        assert_eq!(args.model.as_deref(), Some("ollama/qwen3-coder:30b-64k"));
        assert!(args.flags.is_empty());
    }

    #[test]
    fn a_typed_variant_is_kept_and_no_model_drops_the_level() {
        let typed = "opencode/beam-free#low";
        let args = model_args(Some(typed), Some("high"), Some("2.0.32"));
        assert_eq!(args.model.as_deref(), Some(typed));
        let bare = model_args(None, Some("high"), Some("2.0.32"));
        assert_eq!(
            bare,
            ModelArgs {
                model: None,
                flags: Vec::new()
            }
        );
    }
}
