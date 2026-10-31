//! The outside agent package: one folder other agents install with their own plugin commands, so
//! they can hand work to the crew. An Agent Plugins manifest, one MCP entry that runs
//! `argonauts mcp serve` in the repo by absolute path with no credentials, the
//! `delegate-to-argonauts` skill, and Claude Code's plugin layout beside them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use argonauts_core::safety::redact::find_secrets;

use crate::skills::{Frontmatter, Skill, render, validate};
use crate::store::error::{LearningError, Result};

pub const PLUGIN_NAME: &str = "argonauts";
pub const SKILL_NAME: &str = "delegate-to-argonauts";
/// The Agent Plugins 1.0.0 manifest schema, which the spec makes required.
pub const AGENT_PLUGINS_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.json";

/// The Agent Plugins layout at the root, and Claude Code's beside it. Both read `skills/`.
pub const AGENT_PLUGINS_MANIFEST: &str = "plugin.json";
pub const AGENT_PLUGINS_MCP: &str = "mcp.json";
pub const CLAUDE_MANIFEST: &str = ".claude-plugin/plugin.json";
pub const CLAUDE_MCP: &str = ".mcp.json";

const DESCRIPTION: &str = "Hand coding tasks to the Argonauts Code crew, which runs several coding \
    agents under your rules, and relay when a run needs a person.";

/// Writes the package into `dir` for `repo`, with `argonauts` at `binary`, and returns the
/// files written.
pub fn write(dir: &Path, repo: &Path, binary: &Path) -> Result<Vec<PathBuf>> {
    let repo = std::path::absolute(repo)?;
    let binary = std::path::absolute(binary)?;
    if !repo.is_dir() {
        return Err(LearningError::Refused(format!(
            "{} isn't a folder, so there's no repository to serve.",
            repo.display()
        )));
    }
    let (Some(repo_text), Some(binary_text)) = (repo.to_str(), binary.to_str()) else {
        return Err(LearningError::Refused(
            "The repository and binary paths must be UTF-8 for the MCP entry.".to_owned(),
        ));
    };

    let version = env!("CARGO_PKG_VERSION");
    let servers = json!({
        "mcpServers": {
            PLUGIN_NAME: {
                "type": "stdio",
                "command": binary_text,
                "args": ["mcp", "serve", "--repo", repo_text],
            }
        }
    });
    let agent_manifest = json!({
        "$schema": AGENT_PLUGINS_SCHEMA,
        "name": PLUGIN_NAME,
        "version": version,
        "description": DESCRIPTION,
    });
    let claude_manifest = json!({
        "name": PLUGIN_NAME,
        "version": version,
        "description": DESCRIPTION,
    });
    let skill = outside_agent_skill(version);
    let skill_text = render(&skill);
    let problems = validate(&skill_text, SKILL_NAME);
    if !problems.is_empty() {
        return Err(LearningError::Refused(format!(
            "The {SKILL_NAME} skill breaks the Agent Skills spec: {}.",
            problems.join("; ")
        )));
    }

    let files = [
        (AGENT_PLUGINS_MANIFEST.to_owned(), pretty(&agent_manifest)?),
        (AGENT_PLUGINS_MCP.to_owned(), pretty(&servers)?),
        (CLAUDE_MANIFEST.to_owned(), pretty(&claude_manifest)?),
        (CLAUDE_MCP.to_owned(), pretty(&servers)?),
        (format!("skills/{SKILL_NAME}/SKILL.md"), skill_text),
    ];
    for (path, text) in &files {
        if let Some(secret) = find_secrets(text).first() {
            return Err(LearningError::Refused(format!(
                "{path} would carry a {}, so the package isn't written.",
                secret.kind
            )));
        }
    }
    let mut written = Vec::new();
    for (path, text) in files {
        let target = dir.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, text)?;
        written.push(target);
    }
    Ok(written)
}

fn pretty(value: &Value) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

fn outside_agent_skill(version: &str) -> Skill {
    let mut metadata = BTreeMap::new();
    metadata.insert("argonauts-version".to_owned(), version.to_owned());
    Skill {
        frontmatter: Frontmatter {
            name: SKILL_NAME.to_owned(),
            description: "Use when a coding task is big enough for a crew: several files, a plan \
                worth critiquing, or work that needs review from another provider. Starts an \
                Argonauts Code run through its MCP server, follows it, and relays when it needs a \
                person, without ever approving anything itself."
                .to_owned(),
            license: None,
            compatibility: Some(
                "Needs the argonauts MCP server from this package, set up for one repository."
                    .to_owned(),
            ),
            metadata,
            allowed_tools: None,
        },
        body: SKILL_BODY.to_owned(),
    }
}

const SKILL_BODY: &str = "# Delegate to Argonauts Code

Argonauts Code runs a crew of coding agents on one repository: they plan, critique, build, and \
review each other's work under rules the person set. This skill tells you when to hand a task \
to the crew and how to follow it.

## When to delegate

- The task touches several files or modules, or needs a plan before code.
- It benefits from review by a different provider, or from tests written separately.
- The person asked for the crew, or for a run they can review later.

Keep small edits, questions about the code, and anything urgent to yourself.

## How

1. Start a run with the `argonauts` MCP server's tool for starting runs. Pass the task in the \
person's words, with the acceptance criteria you know.
2. Follow it with `argonauts_run_events`, which waits up to 30 seconds for news.
3. Relay guidance from the person with `argonauts_send_guidance`, and pause with \
`argonauts_pause_run` if they ask.
4. When a run needs a person, call `argonauts_request_decision`. Tell the person what is \
waiting and give them its link, word for word.
5. When it finishes, summarize the result and link the run.

## Never

- Never approve, answer an approval, or claim that something was approved. Approvals belong \
to the person, in Argonauts Code itself. An answer given through you never counts as one.
- Never pass credentials, tokens, or keys to the crew.
- Never start a run in a repository the server wasn't set up for.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_passes_the_validator() {
        let text = render(&outside_agent_skill("0.0.1"));
        assert_eq!(validate(&text, SKILL_NAME), Vec::<String>::new());
    }
}
