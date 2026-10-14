//! Removals Claude Code asks about every time, whatever your rules say. 2.1.289 asks when an
//! empty variable would turn an rm's target into `/`, `/*`, or a folder at the top of your disk.

use std::path::Path;

/// Why Claude Code asks about `command` every time, such as `if BUILD_DIR were empty, it would
/// remove /tmp`. `None` for a command it asks about only as your rules say.
pub(crate) fn always_asked(command: &str) -> Option<String> {
    command
        .split(['\n', ';', '|', '&'])
        .find_map(|part| removal(part.split_whitespace().map(unquoted)))
}

fn removal<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut program = words.next()?;
    if program == "sudo" {
        program = words.next()?;
    }
    if program != "rm" {
        return None;
    }
    words
        .filter(|word| !word.starts_with('-'))
        .find_map(empty_variable_target)
}

/// `$VAR/rest` or `${VAR}/rest`, with no `:?` or `:-` to stop an empty value.
fn empty_variable_target(target: &str) -> Option<String> {
    let rest = target.strip_prefix('$')?;
    let (name, after) = match rest.strip_prefix('{') {
        Some(braced) => {
            let (name, after) = braced.split_once('}')?;
            (name, after)
        }
        None => {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            rest.split_at(end)
        }
    };
    let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !plain || !after.starts_with('/') {
        return None;
    }
    let top = after
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    let reaches_the_top = top.is_empty() || top == "*" || Path::new("/").join(top).is_dir();
    reaches_the_top.then(|| format!("if {name} were empty, it would remove {after}"))
}

fn unquoted(word: &str) -> &str {
    word.trim_matches(|c| c == '"' || c == '\'')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_variable_reaching_the_top_of_the_disk_is_always_asked() {
        assert_eq!(
            always_asked(r#"rm -rf "$BUILD_DIR/tmp""#).as_deref(),
            Some("if BUILD_DIR were empty, it would remove /tmp")
        );
        assert_eq!(
            always_asked("cd src && rm -r ${OUT}/*").as_deref(),
            Some("if OUT were empty, it would remove /*")
        );
        assert!(always_asked("sudo rm -rf $ROOT/").is_some());
    }

    /// 2.1.289 runs these without asking where your rules allow them.
    #[test]
    fn guarded_literal_and_unreachable_targets_follow_your_rules() {
        for command in [
            r#"rm -rf "${BUILD_DIR:?}/tmp""#,
            "rm -rf build/out",
            r#"rm -rf "$EMPTY_VAR/argonauts-probe-missing""#,
            "echo $HOME/tmp",
            "rm -rf $DIR",
        ] {
            assert_eq!(always_asked(command), None, "{command}");
        }
    }
}
