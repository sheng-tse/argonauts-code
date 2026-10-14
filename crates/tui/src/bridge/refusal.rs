//! A tool call its CLI turned down by its permission rules, before anything ran. Claude Code
//! answers such a call with an error result, which would otherwise read as a red failure,
//! though the agent only tried something its session doesn't allow.

/// What a refused call says under it.
pub const NOT_ALLOWED: &str = "Not allowed";

/// The words each CLI starts a refusal with, as its own program text writes them.
const REFUSALS: &[&str] = &[
    // Claude Code 2.1.295: "Permission to use Bash has been denied because Claude Code is
    // running in don't ask mode.", and the same with a rule's content.
    "Permission to use ",
    "Permission for this tool use was denied",
    "Claude requested permissions to ",
    // The Claude Code adapter's own words for a call its CLI denied without asking.
    "Claude Code turned this down",
];

/// Whether a failed call's output is its CLI refusing it rather than it failing.
pub fn is_refusal(output: &str) -> bool {
    let text = output.trim_start();
    let text = text.strip_prefix("Error: ").unwrap_or(text);
    REFUSALS.iter().any(|start| text.starts_with(start))
        && (text.contains("denied")
            || text.contains("haven't granted")
            || text.contains("turned this down"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_refusal_is_not_a_failure() {
        assert!(is_refusal(
            "Permission to use Bash has been denied because Claude Code is running in don't \
             ask mode. IMPORTANT: You *may* try to accomplish this action using other tools"
        ));
        assert!(is_refusal(
            "Error: Permission to use Bash with python3 -B -m unittest has been denied."
        ));
        assert!(is_refusal(
            "Claude requested permissions to use Bash, but you haven't granted it yet."
        ));
        assert!(is_refusal(
            "Claude Code turned this down: rm -rf needs approval"
        ));
        assert!(!is_refusal("Exit code 1\nFAILED (failures=1)"));
        assert!(!is_refusal("bash: ./run.sh: Permission denied"));
    }
}
