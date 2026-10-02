//! The main agent CLI's own sessions for this repo: `/resume` lists them beside conversations
//! and runs, and picking one carries it on in the chat through the CLI's own resume, such as
//! `claude -p --resume <id>`, after reprinting its last turns.

use std::sync::Arc;

use anyhow::Context as _;
use argonauts_core::Adapter;
use argonauts_core::app::{ReplayTurn, TurnReply};
use argonauts_core::events::run::Party;
use argonauts_core::names::{SessionKey, WorkerRef};
use argonauts_core::vendor_sessions::{
    ResumeLaunch, Speaker, VendorScan, VendorSession, VendorSessionRef, VendorTurn,
};

use super::excerpt::Exchange;
use crate::commands::adapters_here;
use crate::controller::Ctx;

/// How many sessions `/resume` lists.
pub(crate) const LISTED: usize = 30;

/// How many of a session's last turns are read back: the last few print, and the rest join the
/// Ctrl+O history.
const READ_BACK: usize = 40;

/// A CLI session picked to carry on.
pub(super) struct Picked {
    pub session: VendorSession,
    pub launch: ResumeLaunch,
    pub turns: Vec<VendorTurn>,
    /// The CLI's name, such as "Claude Code", and its command, such as `claude`.
    pub name: &'static str,
    pub binary: &'static str,
}

impl Picked {
    /// The agent that resumes it: the main agent when it's that CLI, keeping the model you set.
    pub(super) fn worker(&self, default: &WorkerRef) -> WorkerRef {
        if default.worker == self.session.vendor.0 {
            default.clone()
        } else {
            WorkerRef::new(self.session.vendor.0.clone(), None)
        }
    }

    /// The turns as a resumed conversation reprints them, oldest first.
    pub(super) fn replay(&self, worker: &WorkerRef) -> Vec<ReplayTurn> {
        self.turns
            .iter()
            .map(|turn| ReplayTurn {
                from: match turn.from {
                    Speaker::User => Party::User,
                    Speaker::Agent => Party::Agent {
                        session: SessionKey::numbered(0),
                        worker: worker.clone(),
                    },
                },
                text: turn.text.clone(),
            })
            .collect()
    }

    /// Each request and its answer, to prime a fresh session if another agent takes over.
    pub(super) fn history(&self) -> Vec<Exchange> {
        self.turns
            .windows(2)
            .filter(|pair| pair[0].from == Speaker::User && pair[1].from == Speaker::Agent)
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
fn scan(ctx: &Ctx) -> Option<VendorScan> {
    let home = ctx.env.user_home.clone()?;
    Some(VendorScan::new(ctx.env.repo_root.clone(), home))
}

fn adapter(ctx: &Ctx, vendor: &str) -> Option<Arc<dyn Adapter>> {
    adapters_here(Some(&ctx.env), ctx.settings.resolved())
        .ok()?
        .into_iter()
        .find(|adapter| adapter.id() == vendor)
}

/// The main agent's own sessions for this repo, newest first. A store that can't be read lists
/// none, since the rest of `/resume` still works.
pub(crate) async fn list(ctx: &Ctx) -> Vec<VendorSession> {
    let vendor = ctx.settings().default_agent.worker.clone();
    let (Some(adapter), Some(scan)) = (adapter(ctx, &vendor), scan(ctx)) else {
        return Vec::new();
    };
    let listed = tokio::task::spawn_blocking(move || adapter.vendor_sessions(&scan, LISTED)).await;
    match listed {
        Ok(Ok(sessions)) => sessions,
        Ok(Err(err)) => {
            tracing::warn!("listing {vendor}'s own sessions: {err:#}");
            Vec::new()
        }
        Err(err) => {
            tracing::warn!("listing {vendor}'s own sessions: {err}");
            Vec::new()
        }
    }
}

/// The session `reference` names, by its whole id or the start of one, with its last turns.
pub(super) async fn pick(ctx: &Ctx, reference: &VendorSessionRef) -> anyhow::Result<Picked> {
    let vendor = reference.vendor.0.clone();
    let adapter = adapter(ctx, &vendor)
        .with_context(|| format!("There's no agent CLI {vendor}. /agents lists them."))?;
    let scan = scan(ctx).with_context(|| {
        format!(
            "Can't find your home folder, where {} keeps its sessions.",
            adapter.display_name()
        )
    })?;
    let reference = reference.clone();
    tokio::task::spawn_blocking(move || {
        let session = find(adapter.as_ref(), &scan, &reference)?;
        let launch = adapter
            .resume_launch(&session)
            .with_context(|| format!("{} can't carry a session on.", adapter.display_name()))?;
        let turns = adapter.vendor_session_turns(&scan, &session.id, READ_BACK)?;
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

/// The newest sessions first, then all of them, so a short id typed from the list is found
/// without reading every transcript.
fn find(
    adapter: &dyn Adapter,
    scan: &VendorScan,
    reference: &VendorSessionRef,
) -> anyhow::Result<VendorSession> {
    let id = reference.id.as_str();
    for limit in [LISTED, usize::MAX] {
        let sessions = adapter.vendor_sessions(scan, limit)?;
        if let Some(exact) = sessions.iter().find(|session| session.id == id) {
            return Ok(exact.clone());
        }
        let starting: Vec<&VendorSession> = sessions
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
