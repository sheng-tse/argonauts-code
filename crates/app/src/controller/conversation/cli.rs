//! The CLIs' own sessions: `/resume` lists every installed CLI's beside conversations and runs,
//! for this folder or for every folder, and picking one carries it on in the conversation
//! through the CLI's own resume, such as `claude -p --resume <id>`, after reprinting its last
//! turns. A session a conversation carries is listed as that conversation, never again on its
//! own.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Context as _;
use argonauts_core::Adapter;
use argonauts_core::app::{ReplayTurn, TurnReply};
use argonauts_core::cli_sessions::{
    CliScan, CliSession, CliSessionRef, CliTurn, ResumeLaunch, Speaker,
};
use argonauts_core::names::ModelRef;

use super::excerpt::Exchange;
use crate::commands::adapters_here;
use crate::controller::Ctx;

/// How many of each CLI's sessions `/resume` lists.
pub(crate) const LISTED: usize = 30;

/// How many of a session's last turns are read back: the last few print, and the rest join the
/// Ctrl+O history.
const READ_BACK: usize = 40;

/// A CLI session picked to carry on.
pub(super) struct Picked {
    pub session: CliSession,
    pub launch: ResumeLaunch,
    pub turns: Vec<CliTurn>,
    /// The CLI's name, such as "Claude Code", and its command, such as `claude`.
    pub name: &'static str,
    pub binary: &'static str,
}

impl Picked {
    /// The agent that resumes it: the main agent when it's that CLI, keeping the model you set.
    pub(super) fn worker(&self, default: &ModelRef) -> ModelRef {
        if default.agent == self.session.agent_id.0 {
            default.clone()
        } else {
            ModelRef::new(self.session.agent_id.0.clone(), None)
        }
    }

    /// The turns as a resumed conversation reprints them, oldest first.
    pub(super) fn replay(&self, worker: &ModelRef) -> Vec<ReplayTurn> {
        super::in_cli::replayed(&self.turns, worker)
    }

    /// Each request and its answer, to prime a fresh session on a fallback.
    pub(super) fn history(&self) -> Vec<Exchange> {
        self.turns
            .windows(2)
            .filter(|pair| pair[0].from == Speaker::User && pair[1].from == Speaker::Agent)
            // A turn of calls alone left no answer to carry.
            .filter(|pair| !pair[1].text.is_empty())
            .map(|pair| {
                let reply = TurnReply::Answer {
                    markdown: pair[1].text.clone(),
                };
                Exchange::new(pair[0].text.clone(), &reply)
            })
            .collect()
    }
}

/// Where the CLIs keep their sessions: under your home folder, for this repo.
fn scan(ctx: &Ctx) -> Option<CliScan> {
    let home = ctx.env.user_home.clone()?;
    Some(CliScan::new(ctx.env.repo_root.clone(), home))
}

pub(super) fn adapter(ctx: &Ctx, agent_id: &str) -> Option<Arc<dyn Adapter>> {
    adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .ok()?
        .into_iter()
        .find(|adapter| adapter.id() == agent_id)
}

/// The CLI's name as its own screen spells it, such as "Claude Code", or its id when no adapter
/// here knows it.
pub(super) fn name(ctx: &Ctx, agent_id: &str) -> String {
    adapter(ctx, agent_id).map_or_else(
        || agent_id.to_owned(),
        |adapter| adapter.display_name().to_owned(),
    )
}

/// The CLI's command, such as `claude`, or its id when no adapter here knows it.
pub(super) fn binary(ctx: &Ctx, agent_id: &str) -> String {
    adapter(ctx, agent_id).map_or_else(
        || agent_id.to_owned(),
        |adapter| adapter.binary().to_owned(),
    )
}

/// Every CLI's own sessions, newest first: this folder's, or every folder's, each once. One a
/// conversation carries is that conversation's row. A store that can't be read lists none,
/// since the rest of `/resume` still works.
pub(crate) async fn list(ctx: &Ctx, every_folder: bool) -> Vec<CliSession> {
    let Some(mut scan) = scan(ctx) else {
        return Vec::new();
    };
    if every_folder {
        scan = scan.everywhere();
    }
    let adapters = adapters_here(Some(&ctx.env), ctx.settings.resolved()).unwrap_or_default();
    let conversation_dir = ctx.env.conversation_dir();
    let listed = tokio::task::spawn_blocking(move || {
        let carried = super::crew_cli_sessions(&conversation_dir);
        let mut seen = BTreeSet::new();
        let mut sessions = Vec::new();
        for adapter in adapters {
            // A CLI can be both built in and set up again as a CLI.
            if !seen.insert(adapter.id().to_owned()) {
                continue;
            }
            match adapter.cli_sessions(&scan, LISTED) {
                Ok(own) => sessions.extend(own),
                Err(err) => tracing::warn!("listing {}'s own sessions: {err:#}", adapter.id()),
            }
        }
        sessions.retain(|session| !carried.contains(&session.id));
        newest_first(&mut sessions);
        sessions.dedup_by(|a, b| a.agent_id == b.agent_id && a.id == b.id);
        sessions
    })
    .await;
    listed.unwrap_or_else(|err| {
        tracing::warn!("listing the CLIs' own sessions: {err}");
        Vec::new()
    })
}

/// Newest first, and by CLI then id among those used in the same moment, so the order holds.
fn newest_first(sessions: &mut [CliSession]) {
    sessions.sort_by(|a, b| {
        b.updated
            .cmp(&a.updated)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// The session `reference` names, by its whole id or the start of one, with its last turns.
pub(super) async fn pick(ctx: &Ctx, reference: &CliSessionRef) -> anyhow::Result<Picked> {
    let agent_id = reference.agent_id.0.clone();
    let adapter = adapter(ctx, &agent_id)
        .with_context(|| format!("There's no agent CLI {agent_id}. /agents lists them."))?;
    let scan = scan(ctx).with_context(|| {
        format!(
            "Can't find your home folder, where {} keeps its sessions.",
            adapter.display_name()
        )
    })?;
    let reference = reference.clone();
    tokio::task::spawn_blocking(move || {
        // This folder's first, so a short id typed here means a session listed here. A CLI
        // that keeps no list takes the id as you gave it, to carry on here.
        let session = find(adapter.as_ref(), &scan, &reference)
            .or_else(|here| {
                find(adapter.as_ref(), &scan.clone().everywhere(), &reference).map_err(|_| here)
            })
            .or_else(|missing| match adapter.lists_sessions() {
                true => Err(missing),
                false => unlisted(adapter.as_ref(), &scan, &reference),
            })?;
        let scan = CliScan::new(session.folder.clone(), scan.home.clone());
        let launch = adapter
            .resume_launch(&session)
            .with_context(|| format!("{} can't carry a session on.", adapter.display_name()))?;
        let turns = adapter.cli_session_turns(&scan, &session.id, READ_BACK)?;
        Ok(Picked {
            session,
            launch,
            turns,
            name: adapter.display_name(),
            binary: adapter.binary(),
        })
    })
    .await
    .context("reading the session")?
}

/// The session `reference` names, by its whole id or the start of one, this folder's first.
pub(super) async fn locate(ctx: &Ctx, reference: &CliSessionRef) -> anyhow::Result<CliSession> {
    let agent_id = reference.agent_id.0.clone();
    let adapter = adapter(ctx, &agent_id)
        .with_context(|| format!("There's no agent CLI {agent_id}. /agents lists them."))?;
    let scan = scan(ctx).context("Can't find your home folder, where the CLIs keep sessions.")?;
    let reference = reference.clone();
    tokio::task::spawn_blocking(move || {
        find(adapter.as_ref(), &scan, &reference).or_else(|here| {
            find(adapter.as_ref(), &scan.clone().everywhere(), &reference).map_err(|_| here)
        })
    })
    .await
    .context("reading the session")?
}

/// A session of a CLI that keeps no list, by the id you gave, carried on in this folder.
fn unlisted(
    adapter: &dyn Adapter,
    scan: &CliScan,
    reference: &CliSessionRef,
) -> anyhow::Result<CliSession> {
    anyhow::ensure!(
        argonauts_core::cli_sessions::is_session_id(&reference.id),
        "{reference} isn't a session id {} takes.",
        adapter.display_name()
    );
    Ok(CliSession {
        agent_id: reference.agent_id.clone(),
        id: reference.id.clone(),
        title: format!(
            "{} conversation {}",
            adapter.display_name(),
            short_id(&reference.id)
        ),
        prompt: None,
        folder: scan.folder.clone(),
        updated: time::OffsetDateTime::now_utc(),
        model: None,
        branch: None,
        last_message: None,
        turns: 0,
    })
}

/// The newest sessions first, then all of them, so a short id typed from the list is found
/// without reading every transcript.
pub(super) fn find(
    adapter: &dyn Adapter,
    scan: &CliScan,
    reference: &CliSessionRef,
) -> anyhow::Result<CliSession> {
    let id = reference.id.as_str();
    for limit in [LISTED, usize::MAX] {
        let sessions = adapter.cli_sessions(scan, limit)?;
        if let Some(exact) = sessions.iter().find(|session| session.id == id) {
            return Ok(exact.clone());
        }
        let starting: Vec<&CliSession> = sessions
            .iter()
            .filter(|session| session.id.starts_with(id))
            .collect();
        match starting.as_slice() {
            [one] => return Ok((*one).clone()),
            [] => {}
            many => anyhow::bail!(
                "{reference} could be {} sessions. Type more of its id, or pick it in /resume.",
                many.len()
            ),
        }
        if sessions.len() < limit {
            break;
        }
    }
    anyhow::bail!(
        "{} has no session {id} for this repo. /resume lists them.",
        adapter.display_name()
    )
}

/// The first part of a session id, enough to tell it apart in a notice.
pub(super) fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}
