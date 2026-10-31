//! Claude Code's rules for work outside the project. Where a mode opens it, allow rules let
//! its file tools reach anywhere without asking (`Read(//**)`, `Edit(//**)` and the tools that
//! write), and deny rules keep the never-list closed: deny rules hold in every permission mode
//! and win over any allow rule. Inside its own sandbox (`sandbox.mode = "tool"`) Claude Code
//! itself merges the paths of `Edit(...)` and `Read(...)` rules into the sandbox's
//! `allowWrite`, `denyWrite` and `denyRead` (2.1.304's settings schema says so), so its
//! commands are held to them too, except on Linux, where it leaves out a rule with a glob: a
//! never-list name such as `.env` binds only its file tools there. A worker in a worktree
//! never writes your checkout: that goes through its worktree.
//!
//! Where a mode opens outside only once asked, a direct turn gets no allow rule, so what
//! Claude Code asks about there still goes to you, and a write you approve lands, our sandbox
//! being open there. The deny rules still hold after a Yes, since Claude Code checks them before
//! it asks. A run's session can't ask, so there it reads without asking and writes only its
//! worktree.

use std::path::Path;

use argonauts_core::agents::session::SessionRole;
use argonauts_core::safety::outside::Outside;

/// The tools a read rule covers.
const READS: &[&str] = &["Read"];

/// The tools that write a file. `Edit` alone covers them in recent versions; the others are
/// named too, as the rules that keep `.argonauts/` closed name them.
const WRITES: &[&str] = &["Edit", "Write", "NotebookEdit"];

/// Every path, in Claude Code's rule syntax, where `//` starts an absolute path.
const ANYWHERE: &str = "//**";

/// Allow and deny rules, in the order Claude Code reads them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

/// With `held_by_ours`, our OS sandbox holds the session and keeps the never-list there too.
pub fn rules(
    outside: &Outside,
    role: SessionRole,
    workdir: &Path,
    held_by_ours: bool,
) -> PathRules {
    let mut rules = PathRules::default();
    if !outside.is_open() {
        return rules;
    }
    if outside.reads_unasked(role) {
        rules.allow.extend(each(READS, ANYWHERE));
    }
    if outside.writes_unasked(role) {
        rules.allow.extend(each(WRITES, ANYWHERE));
    }
    let both: Vec<&str> = READS.iter().chain(WRITES).copied().collect();
    for path in &outside.never.paths {
        for pattern in path_patterns(path) {
            rules.deny.extend(each(&both, &pattern));
        }
    }
    for name in outside.never.names.iter().filter(|name| is_plain(name)) {
        // A deny wins over every allow, so a rule can't take a template such as `.env.example`
        // back out of `.env.*`. Our sandbox can, so where it holds the session the name is left
        // to it; under Claude Code's own the templates stay closed to its file tools.
        if held_by_ours && !outside.never.taken_back_from(name).is_empty() {
            continue;
        }
        rules.deny.extend(each(&both, &format!("//**/{name}")));
    }
    for path in &outside.never.unwritten {
        for pattern in path_patterns(path) {
            rules.deny.extend(each(WRITES, &pattern));
        }
    }
    let writes = outside.writes_outside(role);
    let checkout = outside
        .project
        .as_ref()
        .filter(|project| writes && !workdir.starts_with(project));
    for pattern in checkout
        .into_iter()
        .flat_map(|project| path_patterns(project))
    {
        rules.deny.extend(each(WRITES, &pattern));
    }
    rules
}

fn each(tools: &[&str], pattern: &str) -> Vec<String> {
    tools
        .iter()
        .map(|tool| format!("{tool}({pattern})"))
        .collect()
}

/// The path itself and everything under it. A path a rule can't hold exactly, with a glob
/// character or a parenthesis, gets none: the OS sandbox still holds it.
fn path_patterns(path: &Path) -> Vec<String> {
    let Some(text) = path.to_str().filter(|text| text.starts_with('/')) else {
        return Vec::new();
    };
    if !is_plain(text) || text.contains('*') {
        return Vec::new();
    }
    let text = text.trim_end_matches('/');
    vec![format!("/{text}"), format!("/{text}/**")]
}

/// Nothing a rule reads as a glob or as its end, other than the `*` a name may hold.
fn is_plain(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(['(', ')', '?', '[', ']', '{', '}', '\\', '\n'])
        && !text.chars().any(char::is_control)
        && !text.trim_start_matches('/').contains("**")
}

/// Whether `rule` is one [`rules`] writes: a file tool with an absolute pattern.
pub fn is_path_rule(rule: &str) -> bool {
    let Some((tool, rest)) = rule.split_once('(') else {
        return false;
    };
    let Some(pattern) = rest.strip_suffix(')') else {
        return false;
    };
    let known = READS.contains(&tool) || WRITES.contains(&tool);
    let body = pattern.strip_suffix("/**").unwrap_or(pattern);
    known
        && pattern.starts_with("//")
        && !body.contains(['(', ')', '\n'])
        && (pattern == ANYWHERE || is_plain(body.trim_start_matches("//**/")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::safety::outside::Never;

    use super::*;

    fn autopilot() -> Outside {
        Outside {
            read: true,
            write: true,
            after_asking: false,
            project: Some(PathBuf::from("/home/you/app")),
            never: Never {
                paths: vec![PathBuf::from("/home/you/.ssh")],
                names: vec![".env".to_owned()],
                except: Vec::new(),
                unwritten: vec![PathBuf::from("/etc")],
            },
        }
    }

    #[test]
    fn a_closed_mode_adds_no_rule() {
        let rules = rules(
            &Outside::default(),
            SessionRole::Direct,
            Path::new("/w"),
            true,
        );
        assert_eq!(rules, PathRules::default());
    }

    #[test]
    fn a_direct_turn_reads_and_writes_anywhere_but_the_never_list() {
        let rules = rules(
            &autopilot(),
            SessionRole::Direct,
            Path::new("/home/you/app"),
            true,
        );
        assert_eq!(
            rules.allow,
            [
                "Read(//**)",
                "Edit(//**)",
                "Write(//**)",
                "NotebookEdit(//**)"
            ]
        );
        for denied in [
            "Read(//home/you/.ssh)",
            "Read(//home/you/.ssh/**)",
            "Edit(//home/you/.ssh/**)",
            "Write(//home/you/.ssh/**)",
            "Read(//**/.env)",
            "Edit(//**/.env)",
            "Edit(//etc/**)",
        ] {
            assert!(rules.deny.iter().any(|rule| rule == denied), "{denied}");
        }
        // System folders are read; the project is the turn's own folder.
        assert!(!rules.deny.iter().any(|rule| rule.starts_with("Read(//etc")));
        assert!(
            !rules
                .deny
                .iter()
                .any(|rule| rule.contains("//home/you/app"))
        );
        assert!(
            rules
                .allow
                .iter()
                .chain(&rules.deny)
                .all(|rule| is_path_rule(rule))
        );
    }

    fn supervised() -> Outside {
        Outside {
            read: false,
            write: false,
            after_asking: true,
            ..autopilot()
        }
    }

    /// Nothing is allowed without asking, so Claude Code asks about a write outside as it does
    /// on its own, and the never-list is denied before any question.
    #[test]
    fn a_supervised_turn_asks_outside_and_the_never_list_is_denied_even_after_yes() {
        let rules = rules(
            &supervised(),
            SessionRole::Direct,
            Path::new("/home/you/app"),
            true,
        );
        assert!(rules.allow.is_empty(), "{:?}", rules.allow);
        for denied in [
            "Read(//home/you/.ssh/**)",
            "Edit(//home/you/.ssh/**)",
            "Write(//home/you/.ssh/**)",
            "Read(//**/.env)",
            "Edit(//**/.env)",
            "Edit(//etc/**)",
        ] {
            assert!(rules.deny.iter().any(|rule| rule == denied), "{denied}");
        }
        assert!(
            !rules
                .deny
                .iter()
                .any(|rule| rule.contains("//home/you/app"))
        );
    }

    /// A run's session can't ask, so nothing it writes outside would wait for you: it reads
    /// there without asking and writes only its worktree, and the never-list holds.
    #[test]
    fn a_run_s_sessions_in_supervised_read_outside_and_write_only_their_worktree() {
        let tree = Path::new("/home/you/.argonauts/worktrees/app/run-1/task-a");
        let worker = rules(&supervised(), SessionRole::Build, tree, true);
        assert_eq!(worker.allow, ["Read(//**)"]);
        assert!(worker.deny.iter().any(|rule| rule == "Read(//**/.env)"));
        let reader = rules(&supervised(), SessionRole::ReadOnly, tree, true);
        assert_eq!(reader.allow, ["Read(//**)"]);
    }

    /// A rule can't take `.env.example` back out of `.env.*`, a deny winning over every allow.
    /// Where our sandbox holds the session, which can, the name is left to it; under Claude
    /// Code's own sandbox the templates stay closed to its file tools.
    #[test]
    fn a_name_a_template_is_taken_back_from_is_left_to_our_sandbox() {
        let mut outside = autopilot();
        outside.never.names = vec![".env".to_owned(), ".env.*".to_owned()];
        outside.never.except = vec![".env.example".to_owned()];
        let workdir = Path::new("/home/you/app");
        let ours = rules(&outside, SessionRole::Direct, workdir, true);
        assert!(ours.deny.iter().any(|rule| rule == "Read(//**/.env)"));
        assert!(
            !ours.deny.iter().any(|rule| rule.contains(".env.*")),
            "{:?}",
            ours.deny
        );
        let own = rules(&outside, SessionRole::Direct, workdir, false);
        assert!(own.deny.iter().any(|rule| rule == "Read(//**/.env.*)"));
        assert!(own.deny.iter().any(|rule| rule == "Edit(//**/.env.*)"));
    }

    #[test]
    fn a_worker_writes_the_checkout_only_through_its_worktree() {
        let tree = Path::new("/home/you/.argonauts/worktrees/app/run-1/task-a");
        let rules = rules(&autopilot(), SessionRole::Build, tree, true);
        assert!(
            rules
                .deny
                .iter()
                .any(|rule| rule == "Edit(//home/you/app/**)")
        );
        assert!(
            !rules
                .deny
                .iter()
                .any(|rule| rule == "Read(//home/you/app/**)")
        );
    }

    #[test]
    fn a_reader_reads_anywhere_and_writes_nothing_outside() {
        let rules = rules(&autopilot(), SessionRole::ReadOnly, Path::new("/w"), true);
        assert_eq!(rules.allow, ["Read(//**)"]);
        assert!(
            rules
                .deny
                .iter()
                .any(|rule| rule == "Read(//home/you/.ssh/**)")
        );
    }

    #[test]
    fn a_path_a_rule_cant_hold_exactly_is_left_to_the_os_sandbox() {
        assert!(path_patterns(Path::new("/home/you/odd(name)")).is_empty());
        assert!(path_patterns(Path::new("/home/you/[x]")).is_empty());
        assert!(!is_path_rule("Read(//a/b))"));
        assert!(!is_path_rule("Bash(//**)"));
        assert!(!is_path_rule("Read(relative/**)"));
        assert!(is_path_rule(
            "Read(//Users/you/Library/Application Support/Arc/**)"
        ));
    }
}
