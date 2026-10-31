//! Small pieces of the plain text the subcommands print.

use serde::Serialize;

/// An enum's name as words, such as `pull request` for `PullRequest`.
pub fn words(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(|name| name.replace('_', " ")))
        .map(|name| match name.as_str() {
            // A worker's kind on the wire. On screen it is the agent's CLI.
            "cli" => "CLI".to_owned(),
            name => argonauts_core::base::text::state_words(name).to_owned(),
        })
        .unwrap_or_default()
}

/// A size such as `14.2 MB`, in powers of 1024.
pub fn bytes(count: u64) -> String {
    const UNITS: &[&str] = &["KB", "MB", "GB", "TB"];
    if count < 1024 {
        return if count == 1 {
            "1 byte".to_owned()
        } else {
            format!("{count} bytes")
        };
    }
    // Precision past a tenth doesn't matter for a size on screen.
    #[allow(clippy::cast_precision_loss)]
    let mut size = count as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// `1 day`, `9 days`.
pub fn count(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use argonauts_core::runs::api::RunState;

    use super::*;

    #[test]
    fn sizes_read_like_a_person_wrote_them() {
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(900), "900 bytes");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(14 * 1024 * 1024 + 200 * 1024), "14.2 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn names_become_words() {
        assert_eq!(words(&RunState::PullRequest), "pull request");
        assert_eq!(words(&RunState::Waiting), "waiting");
        assert_eq!(count(1, "day", "days"), "1 day");
        assert_eq!(count(9, "day", "days"), "9 days");
    }
}
