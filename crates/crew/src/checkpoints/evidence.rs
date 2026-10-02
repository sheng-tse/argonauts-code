//! Evidence packets: measured facts, agent outputs as data, and documents, with your approval
//! rules file (or, without one, the default rules) added so it's bound by the hash. Written to
//! `evidence/<checkpoint>.json`; a document over 64 KB goes in a file beside it with its
//! SHA-256.

use std::path::{Component, Path, PathBuf};

use argonauts_core::events::checkpoint::CheckpointKind;
use argonauts_core::names::CheckpointId;
use argonauts_core::policy::{
    EVIDENCE_INLINE_MAX, EvidenceDocument, EvidenceFacts, EvidencePacket,
};
use argonauts_core::sha256_hex;

use crate::engine::host::Host;
use crate::error::{EngineError, EngineResult};

/// The approver's rules when the repo has no file of its own, so the autonomous defaults can
/// decide in a repo nobody set up.
pub const DEFAULT_RULES: &str = include_str!("../prompts/approvals/default-rules.md");

/// The label your approval rules file goes under, `.argonauts/<model_rules>`, which is where
/// the approver looks for it.
pub fn rules_label(model_rules: &str) -> String {
    format!(".argonauts/{}", in_argonauts(model_rules))
}

/// `model_rules` as a path inside `.argonauts/`, which it may also name itself.
fn in_argonauts(model_rules: &str) -> &str {
    let name = model_rules.trim();
    name.strip_prefix(".argonauts/").unwrap_or(name)
}

pub fn packet(
    host: &Host,
    kind: CheckpointKind,
    facts: EvidenceFacts,
    agent_outputs: Vec<String>,
    documents: Vec<EvidenceDocument>,
) -> EngineResult<EvidencePacket> {
    let goal = host.read(|state| {
        state
            .goal
            .approved
            .clone()
            .or_else(|| state.goal.drafted.clone())
    });
    let mut documents = documents;
    documents.push(rules_document(
        &host.repo().root,
        &host.settings().approvals.approver.model_rules,
    )?);
    let dir = evidence_dir(&host.repo().run_dir);
    let documents = documents
        .into_iter()
        .map(|document| store_if_large(&dir, document))
        .collect::<EngineResult<Vec<_>>>()?;
    let mut facts = facts;
    // Landing, pull requests, and every other answer stay in your repo or can be closed. Only a
    // change request into migration, release, or deploy paths names outside actions, and it
    // fills them itself.
    facts.irreversible_actions.get_or_insert_with(Vec::new);
    Ok(EvidencePacket {
        kind,
        goal,
        facts,
        agent_outputs,
        documents,
    })
}

/// Returns the packet's path. Its hash is `EvidencePacket::sha256`.
pub fn write(host: &Host, id: &CheckpointId, packet: &EvidencePacket) -> EngineResult<PathBuf> {
    let dir = evidence_dir(&host.repo().run_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(packet)?)?;
    Ok(path)
}

/// The packet written for `id`, when it's there.
pub fn read(host: &Host, id: &CheckpointId) -> EngineResult<Option<EvidencePacket>> {
    let path = evidence_dir(&host.repo().run_dir).join(format!("{id}.json"));
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn evidence_dir(run_dir: &Path) -> PathBuf {
    run_dir.join("evidence")
}

/// `.argonauts/<model_rules>` from your checkout, read by the engine so the policy never reads
/// the main checkout itself. A missing file adds the default rules under the same label.
fn rules_document(root: &Path, model_rules: &str) -> EngineResult<EvidenceDocument> {
    let name = in_argonauts(model_rules);
    let relative = Path::new(name);
    // The setting names a file inside `.argonauts/`, never one elsewhere on disk.
    if name.is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(EngineError::Refused(format!(
            "approvals.approver.model_rules must name a file inside .argonauts/, not `{name}`"
        )));
    }
    let path = root.join(".argonauts").join(relative);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT_RULES.to_owned(),
        Err(err) => return Err(err.into()),
    };
    Ok(EvidenceDocument {
        label: rules_label(model_rules),
        text,
        path: None,
        sha256: None,
    })
}

/// A document over the inline limit moves to a file named by its hash, so the packet's own
/// hash still covers it.
fn store_if_large(dir: &Path, document: EvidenceDocument) -> EngineResult<EvidenceDocument> {
    if document.text.len() <= EVIDENCE_INLINE_MAX {
        return Ok(document);
    }
    let sha256 = sha256_hex(document.text.as_bytes());
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("doc-{sha256}.txt"));
    if !path.exists() {
        std::fs::write(&path, &document.text)?;
    }
    Ok(EvidenceDocument {
        label: document.label,
        text: String::new(),
        path: Some(path),
        sha256: Some(sha256),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_document_moves_beside_the_packet_with_its_hash() {
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let text = "x".repeat(EVIDENCE_INLINE_MAX + 1);
        let stored = store_if_large(
            dir.path(),
            EvidenceDocument {
                label: "combined diff".to_owned(),
                text: text.clone(),
                path: None,
                sha256: None,
            },
        )
        .unwrap_or_else(|err| panic!("{err}"));
        assert!(stored.text.is_empty());
        assert_eq!(
            stored.sha256.as_deref(),
            Some(sha256_hex(text.as_bytes()).as_str())
        );
        let path = stored.path.unwrap_or_default();
        assert_eq!(std::fs::read_to_string(path).unwrap_or_default(), text);

        let small = EvidenceDocument {
            label: "plan".to_owned(),
            text: "short".to_owned(),
            path: None,
            sha256: None,
        };
        assert_eq!(store_if_large(dir.path(), small.clone()).ok(), Some(small));
    }

    #[test]
    fn the_rules_file_is_read_from_the_argonauts_folder_only() {
        let dir = tempfile::tempdir().unwrap_or_else(|err| panic!("{err}"));
        let read = |name: &str| rules_document(dir.path(), name).ok();
        let missing = read("rules.md").map(|document| (document.label, document.text));
        assert_eq!(
            missing,
            Some((".argonauts/rules.md".to_owned(), DEFAULT_RULES.to_owned()))
        );
        std::fs::create_dir_all(dir.path().join(".argonauts")).unwrap_or_default();
        std::fs::write(dir.path().join(".argonauts/rules.md"), "merge small diffs")
            .unwrap_or_default();
        for name in ["rules.md", ".argonauts/rules.md"] {
            let found = read(name).map(|document| (document.label, document.text));
            assert_eq!(
                found,
                Some((
                    ".argonauts/rules.md".to_owned(),
                    "merge small diffs".to_owned()
                )),
                "{name}"
            );
        }
        assert!(rules_document(dir.path(), "../secrets").is_err());
        assert!(rules_document(dir.path(), "/etc/passwd").is_err());
    }
}
