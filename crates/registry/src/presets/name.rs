//! Which names a preset may have, shared by the picker, the lookup, and `preset` writes so a
//! preset you can list is one you can pick.

/// Letters, digits, `-`, `_`, and `.`, not starting with a dot. A preset's name becomes a
/// file name, so this keeps it inside the presets folder and off hidden files.
pub(crate) fn is_preset_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Why `name` was refused, with the rule it broke.
pub(crate) fn not_a_preset_name(name: &str) -> String {
    format!(
        "workflow {name:?} isn't a workflow name; a workflow's name uses letters, digits, -, _ and ., \
         and doesn't start with a dot"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_names_stay_plain_file_names() {
        for name in ["council", "my_team", "team.v2", "A1"] {
            assert!(is_preset_name(name), "{name}");
        }
        for name in [
            "", "my team", ".hidden", "..", "../up", "a/b", "a\\b", "café",
        ] {
            assert!(!is_preset_name(name), "{name:?}");
        }
    }
}
