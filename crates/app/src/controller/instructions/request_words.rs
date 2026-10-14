//! Words people type for what Argonauts Code names otherwise. A request that says one is read
//! as if it said the name, and nothing on screen says the word back. This is the one place
//! that spells them.

/// Said for Captain.
pub(super) const FOR_CAPTAIN: [&str; 1] = ["solo"];

/// Said for a worktree.
pub(super) const FOR_A_WORKTREE: [&str; 1] = ["copy"];

/// Said for a role: the main agent, the orchestrator, the agents that draft plans, and the
/// workers.
pub(super) const FOR_ROLES: [&str; 7] = [
    "lead",
    "leader",
    "coordinator",
    "drafter",
    "drafters",
    "owner",
    "owners",
];

/// Said for what the main agent and the orchestrator do.
pub(super) const FOR_WHAT_ROLES_DO: [&str; 4] = ["lead", "leads", "coordinate", "coordinates"];

/// The ones that alone are a sign that a message says how to run it.
pub(super) const SAY_HOW: [&str; 4] = ["solo", "drafters", "copy", "coordinator"];

#[cfg(test)]
mod tests {
    use super::super::roster::Roster;
    use super::super::words::{Stance, stance, turned_around};
    use super::super::{CHECKOUT_CUES, captain_stance, might_say_how, names_a_role, worktree_cues};

    #[test]
    fn a_request_may_say_them_for_captain_and_a_worktree() {
        assert_eq!(captain_stance("do it solo"), Stance::For);
        assert_eq!(captain_stance("not solo, use the crew"), Stance::Against);
        let worktree = |says| stance(says, &worktree_cues(), &CHECKOUT_CUES);
        assert_eq!(worktree("check it in a copy first"), Stance::For);
        assert_eq!(worktree("no copy, work in my files"), Stance::Against);
    }

    #[test]
    fn a_request_may_say_them_for_roles_and_what_they_do() {
        for who in [
            "the lead",
            "leader",
            "a coordinator",
            "drafters",
            "the owners",
        ] {
            assert!(names_a_role(who), "{who}");
        }
        assert!(!names_a_role("the codex one"));
        for says in [
            "codex shouldn't lead",
            "claude never leads",
            "don't have codex coordinate",
            "gemini never coordinates",
        ] {
            assert!(turned_around(says, &[]), "{says}");
        }
        assert!(!turned_around("codex leads and gemini coordinates", &[]));
    }

    #[test]
    fn some_alone_may_say_how_to_run() {
        let nobody = Roster { agents: Vec::new() };
        for said in [
            "do it solo",
            "three drafters",
            "in a copy",
            "codex as coordinator",
        ] {
            assert!(might_say_how(said, &nobody), "{said}");
        }
        assert!(!might_say_how("this may lead to a crash", &nobody));
    }
}
