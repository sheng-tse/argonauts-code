//! Work outside the project (`[outside]`): the approval modes in which agents read and write
//! anywhere, without asking or once asked, and what they never reach.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::approvals::ApprovalMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OutsideSettings {
    /// The approval modes in which every agent reads anywhere without asking
    pub read: Vec<ApprovalMode>,
    /// The approval modes in which every agent writes anywhere without asking: outside the
    /// project in place, inside it in its own worktree when it works in a run
    pub write: Vec<ApprovalMode>,
    /// The approval modes in which agents may read and write anywhere once asked: what a CLI
    /// asks about still waits for you, as its own rules or `captain.asks` say, and a run's
    /// agents work there as the run you approved. Empty keeps these modes to the project
    pub ask: Vec<ApprovalMode>,
    /// What no agent reads or writes in a mode where it works outside: a folder or file (`~/`
    /// is your home), or a file name matched in every folder, such as `.env` or `.env.*`. `!`
    /// before a name takes it back out of a name with `*`, such as `!.env.example`, a template
    /// projects commit. A system folder here is still read, since every program runs from one,
    /// but never written
    pub never: Vec<String>,
}

impl Default for OutsideSettings {
    fn default() -> Self {
        let modes = vec![ApprovalMode::Auto, ApprovalMode::Autopilot];
        Self {
            read: modes.clone(),
            write: modes,
            ask: vec![ApprovalMode::Supervised, ApprovalMode::AutoPreview],
            never: NEVER.iter().map(|entry| (*entry).to_owned()).collect(),
        }
    }
}

/// Keys, keychains, `.env` files but the templates projects commit, the CLIs' logins, browser
/// profiles, and system folders.
const NEVER: &[&str] = &[
    "~/.ssh",
    "~/.aws",
    "~/.gnupg",
    "~/Library/Keychains",
    "~/.local/share/keyrings",
    ".env",
    ".env.*",
    "!.env.example",
    "!.env.sample",
    "!.env.template",
    "!.env.dist",
    "~/.claude/.credentials.json",
    "~/.codex/auth.json",
    "~/.gemini/oauth_creds.json",
    "~/.local/share/opencode/auth.json",
    "~/Library/Application Support/Google/Chrome",
    "~/Library/Application Support/Chromium",
    "~/Library/Application Support/BraveSoftware",
    "~/Library/Application Support/Microsoft Edge",
    "~/Library/Application Support/Arc",
    "~/Library/Application Support/Firefox",
    "~/Library/Safari",
    "~/Library/Cookies",
    "~/.config/google-chrome",
    "~/.config/chromium",
    "~/.config/BraveSoftware",
    "~/.config/microsoft-edge",
    "~/.mozilla",
    "/System",
    "/usr",
    "/etc",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
];

impl OutsideSettings {
    pub fn reads_in(&self, mode: ApprovalMode) -> bool {
        self.read.contains(&mode)
    }

    pub fn writes_in(&self, mode: ApprovalMode) -> bool {
        self.write.contains(&mode)
    }

    pub fn asks_in(&self, mode: ApprovalMode) -> bool {
        self.ask.contains(&mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_autopilot_work_anywhere_and_supervised_asks_first() {
        let outside = OutsideSettings::default();
        for mode in [ApprovalMode::Auto, ApprovalMode::Autopilot] {
            assert!(outside.reads_in(mode) && outside.writes_in(mode));
            assert!(!outside.asks_in(mode));
        }
        for mode in [ApprovalMode::Supervised, ApprovalMode::AutoPreview] {
            assert!(!outside.reads_in(mode) && !outside.writes_in(mode));
            assert!(outside.asks_in(mode));
        }
        assert!(outside.never.iter().any(|entry| entry == "~/.ssh"));
        assert!(outside.never.iter().any(|entry| entry == ".env"));
    }

    #[test]
    fn the_never_list_can_be_emptied_and_modes_named() {
        let outside: OutsideSettings =
            toml::from_str("read = [\"autopilot\"]\nwrite = []\nask = []\nnever = []").unwrap();
        assert!(outside.reads_in(ApprovalMode::Autopilot));
        assert!(!outside.reads_in(ApprovalMode::Auto));
        assert!(!outside.writes_in(ApprovalMode::Autopilot));
        assert!(!outside.asks_in(ApprovalMode::Supervised));
        assert!(outside.never.is_empty());
    }
}
