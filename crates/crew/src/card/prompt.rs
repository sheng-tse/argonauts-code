//! The owner's prompt as parts: role prompt, card, goal, notes, decisions, rules, the filtered
//! status summary, and the report schema. The session view shows exactly what was sent.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Component, Path};
use std::sync::{Mutex, OnceLock, PoisonError};

use argonauts_core::CardId;
use argonauts_core::events::RunEventKind;
use argonauts_core::events::card::{AttemptCause, CardKind, CardSpec};
use argonauts_core::events::goal::GoalFile;
use argonauts_core::events::plan::DesignDecision;
use argonauts_core::events::session::{InputPart, InputPartKind};
use argonauts_core::events::vendor::HandoffEvent;
use argonauts_core::hash::sha256_hex;
use argonauts_core::layout;
use argonauts_core::limits::HandoffPacket;
use argonauts_core::names::{RoleName, WorkerRef};
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::settings::Settings;

use crate::card::support::{card_events, owner_of, part, progress_of, record_of};
use crate::engine::host::Host;
use crate::error::EngineResult;
use crate::handoff::context;
use crate::prompts;
use crate::status::Viewer;

/// Notes are cut to this many characters each, so one long file can't crowd out the card.
const NOTE_CHARS: usize = 8_000;

pub fn owner_parts(host: &Host, card: &CardId) -> EngineResult<Vec<InputPart>> {
    let record = record_of(host, card)?;
    let (goal, decisions) =
        host.read(|state| (state.goal.approved.clone(), state.plan.decisions.clone()));
    let mut parts = vec![part(
        InputPartKind::Prompt,
        "owner",
        prompts::card::OWNER,
        None,
    )];
    parts.push(part(
        InputPartKind::Card,
        card.as_str(),
        card_text(&record.spec),
        None,
    ));
    if let Some(goal) = &goal {
        let source = Some(goal.path.display().to_string());
        parts.push(part(InputPartKind::Goal, "goal", goal_text(goal), source));
    }
    let tree = record
        .worktree
        .clone()
        .unwrap_or_else(|| host.repo().base_worktree.clone());
    parts.extend(note_parts(&tree));
    if !decisions.is_empty() {
        let ids = decisions
            .iter()
            .map(|d| d.id.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(part(
            InputPartKind::Decisions,
            "decisions",
            decisions_text(&decisions),
            Some(ids),
        ));
    }
    parts.push(part(
        InputPartKind::Rules,
        "rules",
        rules_text(&record.spec, host.settings(), &tree),
        None,
    ));
    // Where the worker has no status tool, the engine adds the summary to every start itself.
    if has_status_tool(host, &owner_of(host, &record)) {
        parts.push(status(host, card));
    }
    let build = &host.settings().prompts.build;
    parts.push(part(
        InputPartKind::Schema,
        "report",
        replaced(host, build, prompts::card::BUILD),
        (!build.trim().is_empty()).then(|| build.clone()),
    ));
    Ok(parts)
}

/// The built-in template, or the Markdown file `prompts.<key>` names (`file`): a repo path read
/// from the run's base, or the full path of a file beside the run's recipe version, which may
/// be a candidate's in the conductor's state. A file that's missing, empty, or anywhere else
/// keeps the built-in. Judge and reviewer prompts never come here: they make the evidence
/// approvals read.
pub fn replaced(host: &Host, file: &str, built_in: &'static str) -> &'static str {
    let file = file.trim();
    if file.is_empty() {
        return built_in;
    }
    let path = Path::new(file);
    let text = if path.is_absolute() {
        match crate::retro::recipe::recipe_file(&host.repo().run_dir, path) {
            Some(path) => std::fs::read_to_string(path),
            None => {
                tracing::warn!(
                    "prompts: {file} isn't a file of this run's recipe, so the built-in is used"
                );
                return built_in;
            }
        }
    } else if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        let repo = host.repo();
        std::fs::read_to_string(repo.base_worktree.join(path))
            .or_else(|_| std::fs::read_to_string(repo.root.join(path)))
    } else {
        tracing::warn!("prompts: {file} isn't a path inside the repo, so the built-in is used");
        return built_in;
    };
    match text {
        Ok(text) if !text.trim().is_empty() => intern(text),
        Ok(_) => {
            tracing::warn!("prompts: {file} is empty, so the built-in is used");
            built_in
        }
        Err(err) => {
            tracing::warn!("prompts: {file}: {err}, so the built-in is used");
            built_in
        }
    }
}

/// Asks take `'static` templates, so a replacement is kept for the life of the process, once
/// per distinct text: a recipe's few prompt files, never one per session.
fn intern(text: String) -> &'static str {
    static TEXTS: OnceLock<Mutex<BTreeMap<String, &'static str>>> = OnceLock::new();
    let mut texts = TEXTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = texts.get(&text) {
        return kept;
    }
    let kept: &'static str = Box::leak(text.clone().into_boxed_str());
    texts.insert(text, kept);
    kept
}

/// Where the worker has no status tool (agy, and Codex for now), the status summary goes again
/// as a `Status` part with each delivered message and each resume.
pub fn status_part(host: &Host, card: &CardId, has_status_tool: bool) -> Option<InputPart> {
    (!has_status_tool).then(|| status(host, card))
}

fn status(host: &Host, card: &CardId) -> InputPart {
    let viewer = Viewer::Session {
        role: RoleName::Owner,
        card: Some(card.clone()),
        label: None,
    };
    part(
        InputPartKind::Status,
        "status",
        host.status_text(&viewer),
        None,
    )
}

/// The gate output the owner's session resumes with.
pub fn gate_output_part(output: &str) -> InputPart {
    let text = format!(
        "Your card's gates failed. Fix the cause, not the check, then end with your report \
         again.\n\n{output}"
    );
    part(InputPartKind::GateOutput, "gate output", text, None)
}

/// A reviewer's requested changes, for the owner's session.
pub fn review_part(round: u32, comments: &[String]) -> InputPart {
    let mut text = format!(
        "Review round {round} asked for changes. Make them, then end with your report again.\n"
    );
    for comment in comments {
        let _ = write!(text, "\n- {comment}");
    }
    part(InputPartKind::Message, "review", text, None)
}

pub fn message_part(label: &str, text: impl Into<String>) -> InputPart {
    part(InputPartKind::Message, label, text, None)
}

/// What a fresh session of `to` is told after `cause`, as `handoff.context` says: nothing for
/// `fresh`, else the vendor-neutral handoff packet (the one given, or one built from the log at
/// no token cost) as a note. The note is saved, recorded as `Handoff::Packet`, and returned as
/// a part for the new session.
pub async fn packet_part(
    host: &Host,
    card: &CardId,
    to: &WorkerRef,
    cause: AttemptCause,
    why: &str,
    given: Option<HandoffPacket>,
) -> EngineResult<Option<InputPart>> {
    let record = record_of(host, card)?;
    let mut packet = match given {
        Some(packet) => packet,
        None => host
            .services()
            .handoff
            .packet(&record.spec, &card_events(host, card)?),
    };
    if packet.why_stopped.is_empty() {
        packet.why_stopped = why.to_owned();
    }
    let progress = progress_of(host, card);
    let rules = context::applied(host, &record, &progress, to, cause);
    let told_as = context::prepare(host, card, to, &rules, &mut packet).await?;
    let Some(told) = context::tell(told_as, packet)? else {
        return Ok(None);
    };
    let dir = layout::card_dir(&host.repo().run_dir, card).join("packets");
    create_private_dir_all(&dir)?;
    let path = dir.join(format!("packet-{}.json", record.attempts + 1));
    write_private_file(&path, told.json.as_bytes())?;
    let event = HandoffEvent::Packet {
        card: card.clone(),
        path: path.clone(),
        sha256: sha256_hex(told.json.as_bytes()),
    };
    host.record(Some(card), None, RunEventKind::Handoff(event))?;
    Ok(Some(part(
        InputPartKind::Handoff,
        "handoff",
        told.text,
        Some(path.display().to_string()),
    )))
}

pub(crate) fn card_text(spec: &CardSpec) -> String {
    let mut text = format!("{} · {}\n\n{}\n", spec.id, spec.title, spec.task);
    let kind = match &spec.kind {
        CardKind::Solo => "the whole change".to_owned(),
        CardKind::Interface => {
            "interfaces, with stubs that compile and fail \"not implemented\"".to_owned()
        }
        CardKind::AcceptanceTests { for_card } => {
            format!("acceptance tests for {for_card}; they must fail against the stubs")
        }
        CardKind::Build => "building".to_owned(),
        CardKind::Chore => "integration or cleanup".to_owned(),
        CardKind::Docs => "documentation".to_owned(),
        CardKind::Spike { topic, option } => format!("a spike on {topic}: {option}"),
    };
    let _ = write!(text, "\nKind: {kind}");
    list(&mut text, "Paths you own", &spec.owned_paths);
    list(&mut text, "Read-only paths", &spec.read_only_paths);
    let deps: Vec<String> = spec.depends_on.iter().map(ToString::to_string).collect();
    list(&mut text, "Already integrated before you", &deps);
    list(&mut text, "Goal criteria it serves", &spec.criteria);
    text
}

fn goal_text(goal: &GoalFile) -> String {
    let mut text = format!("{}\n", goal.summary);
    let criteria: Vec<String> = goal
        .criteria
        .iter()
        .map(|c| format!("{}: {}", c.id, c.text))
        .collect();
    list(&mut text, "Done when", &criteria);
    list(&mut text, "In scope", &goal.in_scope);
    list(&mut text, "Out of scope", &goal.out_of_scope);
    let assumptions: Vec<String> = goal.assumptions.iter().map(|a| a.text.clone()).collect();
    list(&mut text, "Assumptions", &assumptions);
    list(&mut text, "Answers", &goal.answers);
    text
}

fn decisions_text(decisions: &[DesignDecision]) -> String {
    let mut text = String::new();
    for d in decisions {
        let _ = writeln!(
            text,
            "{} · {}: {}. {}",
            d.id, d.title, d.choice, d.rationale
        );
    }
    text
}

fn rules_text(spec: &CardSpec, settings: &Settings, tree: &Path) -> String {
    let mut never = vec![".argonauts/**".to_owned()];
    never.extend(settings.limits.protected_paths.iter().cloned());
    never.extend(spec.forbidden_paths.iter().cloned());
    let mut text = String::from("Rules for this card:");
    if spec.owned_paths.is_empty() {
        text.push_str("\n- You may change anything in the worktree except the paths below.");
    }
    list(&mut text, "Never change", &never);
    list(&mut text, "Not checked out", &spec.hidden_paths);
    let allowlist = &settings.limits.dependency_allowlist;
    if allowlist.is_empty() {
        text.push_str("\n\nAdd no new dependencies, and don't edit lockfiles.");
    } else {
        let _ = write!(
            text,
            "\n\nNew dependencies only from this list: {}. Don't edit lockfiles.",
            allowlist.join(", ")
        );
    }
    for guide in ["AGENTS.md", "CLAUDE.md"] {
        if tree.join(guide).is_file() {
            let _ = write!(text, "\n\nFollow the project's conventions in {guide}.");
        }
    }
    text
}

/// Each `.argonauts/notes/*.md` in the worktree, in name order.
/// One part per topic file. The crew's sourced notes go as a quoted block of reference data;
/// a file with none, such as one you wrote, goes as it is. With `judged`, a reviewer's or a
/// judge's parts leave out every note that agent wrote.
pub fn note_parts_for(tree: &Path, judged: Option<&WorkerRef>) -> Vec<InputPart> {
    crate::retro::notes::read_files(tree)
        .into_iter()
        .filter_map(|file| {
            let text = if file.notes.is_empty() {
                file.text
            } else {
                let kept: Vec<_> = file
                    .notes
                    .into_iter()
                    .filter(|(_, source)| judged.is_none_or(|judged| source.worker != *judged))
                    .collect();
                if kept.is_empty() {
                    return None;
                }
                argonauts_learning::memory::quoted(&kept)
            };
            let text: String = text.chars().take(NOTE_CHARS).collect();
            Some(part(
                InputPartKind::Notes,
                "notes",
                text,
                Some(file.relative),
            ))
        })
        .collect()
}

fn note_parts(tree: &Path) -> Vec<InputPart> {
    note_parts_for(tree, None)
}

fn list(text: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = write!(text, "\n\n{title}:");
    for item in items {
        let _ = write!(text, "\n- {item}");
    }
}

/// Whether the owner's worker serves the status tool.
pub(crate) fn has_status_tool(host: &Host, worker: &argonauts_core::names::WorkerRef) -> bool {
    host.services()
        .workers
        .get(worker)
        .map(|w| w.capabilities().status_tool)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::card_spec;

    #[test]
    fn the_card_part_lists_paths_and_dependencies() {
        let mut spec = card_spec("card-002", &["card-001"], None);
        spec.owned_paths = vec!["src/store/**".into()];
        spec.criteria = vec!["G1".into()];
        let text = card_text(&spec);
        assert!(text.starts_with("card-002 · Build card-002"));
        assert!(text.contains("Paths you own:\n- src/store/**"));
        assert!(text.contains("Already integrated before you:\n- card-001"));
        assert!(text.contains("Goal criteria it serves:\n- G1"));
    }

    #[test]
    fn rules_name_protected_and_forbidden_paths() {
        let mut spec = card_spec("card-001", &[], None);
        spec.forbidden_paths = vec!["migrations/**".into()];
        let mut settings = Settings::default();
        settings.limits.protected_paths = vec![".github/**".into()];
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "x").unwrap();
        let text = rules_text(&spec, &settings, dir.path());
        for path in [".argonauts/**", ".github/**", "migrations/**"] {
            assert!(text.contains(&format!("- {path}")), "{text}");
        }
        assert!(text.contains("AGENTS.md"));
        assert!(!text.contains("CLAUDE.md"));
    }

    #[test]
    fn notes_are_read_in_name_order() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join(".argonauts/notes");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("b.md"), "second").unwrap();
        std::fs::write(notes.join("a.md"), "first").unwrap();
        std::fs::write(notes.join("skip.txt"), "no").unwrap();
        let parts = note_parts(dir.path());
        let texts: Vec<&str> = parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["first", "second"]);
        assert_eq!(parts[0].source.as_deref(), Some(".argonauts/notes/a.md"));
        assert_eq!(parts[0].kind, InputPartKind::Notes);
    }

    #[test]
    fn review_and_gate_parts_carry_the_text() {
        let review = review_part(2, &["Handle the empty list".into()]);
        assert_eq!(review.kind, InputPartKind::Message);
        assert!(review.text.contains("round 2"));
        assert!(review.text.contains("- Handle the empty list"));
        let gate = gate_output_part("test: 1 failed");
        assert_eq!(gate.kind, InputPartKind::GateOutput);
        assert!(gate.text.ends_with("test: 1 failed"));
        assert_eq!(
            gate.approx_tokens as usize,
            gate.text.chars().count().div_ceil(4)
        );
    }
}
