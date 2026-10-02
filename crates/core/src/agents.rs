//! The record of an agent's process group. The conductor writes it to
//! `~/.argonauts/agents/<pgid>.json` while the group lives, and to the session's `process.json`.
//! The app reads it to refuse approvals from inside an agent, and to kill the groups a crashed
//! supervisor left behind.

use serde::{Deserialize, Serialize};

/// A process group an app or supervisor started, known by its leader. The leader's start time
/// tells a reused pid apart. The conductor's `agents/` files add fields of their own, which a
/// reader of this type ignores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentGroup {
    pub pgid: u32,
    pub pid: u32,
    /// Seconds since the Unix epoch, as the OS reports the leader's start.
    pub start_time: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agents_file_with_more_fields_reads_as_a_group() {
        let file = r#"{"pgid": 4242, "pid": 4242, "start_time": 1759200000,
                       "session": "s-001", "vendor": "codex"}"#;
        let group: AgentGroup = serde_json::from_str(file).unwrap();
        assert_eq!(
            group,
            AgentGroup {
                pgid: 4242,
                pid: 4242,
                start_time: 1_759_200_000,
            }
        );
    }
}
