//! `--help`, and `exec --help` for codex, listing the flags the doctor looks for. A fake whose
//! `--version` is in the tested range then counts as tested, as the real CLI would.

use argonauts_conductor::doctor::versions::required_flags;

/// The adapter each program stands in for.
fn adapter(argv0: &str) -> Option<&'static str> {
    match argv0 {
        "claude" => Some("claude-code"),
        "codex" => Some("codex"),
        "agy" => Some("antigravity-cli"),
        "opencode" => Some("opencode"),
        _ => None,
    }
}

/// The help text, when the arguments ask for it.
pub fn text(argv0: &str, args: &[String]) -> Option<String> {
    let asks = match args {
        [flag] => flag == "--help",
        [command, flag] => argv0 == "codex" && command == "exec" && flag == "--help",
        _ => false,
    };
    if !asks {
        return None;
    }
    let mut text = format!("Usage: {argv0} [OPTIONS]\n\nOptions:\n");
    for flag in adapter(argv0).map(required_flags).unwrap_or_default() {
        text.push_str(&format!("      {flag}\n"));
    }
    text.push_str("  -h, --help\n  -V, --version\n");
    Some(text)
}

#[cfg(test)]
mod tests {
    use argonauts_conductor::doctor::versions::find_flags;

    use super::*;

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn each_cli_lists_every_flag_the_doctor_looks_for() {
        for (program, adapter) in [
            ("claude", "claude-code"),
            ("codex", "codex"),
            ("agy", "antigravity-cli"),
        ] {
            let help = text(program, &args(&["--help"])).unwrap();
            let probe = find_flags(&help, required_flags(adapter));
            assert!(probe.missing.is_empty(), "{program}: {:?}", probe.missing);
        }
        let exec = text("codex", &args(&["exec", "--help"])).unwrap();
        assert!(
            find_flags(&exec, required_flags("codex"))
                .missing
                .is_empty()
        );
    }

    #[test]
    fn only_a_help_request_gets_help() {
        assert!(text("claude", &args(&["-p", "--help"])).is_none());
        assert!(text("claude", &args(&["exec", "--help"])).is_none());
        assert!(text("claude", &args(&["--version"])).is_none());
        assert!(
            text("hermes", &args(&["--help"]))
                .unwrap()
                .contains("--version")
        );
    }
}
