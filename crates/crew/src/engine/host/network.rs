//! Losing the internet. A session that needs a provider waits for it as it starts, and one that
//! ended on the network while it was gone goes again on the same worker once it's back, with no
//! failure counted and nothing moved to another agent. Models on this machine keep working. The
//! run says once when it goes and once when it's back, and pauses when it stays gone for
//! `network.hold_minutes`.

use std::time::Duration;

use argonauts_core::base::names::ModelRef;
use argonauts_core::models::limits::{
    NETWORK_BACK, NETWORK_DOWN, NetState, may_have_lost_the_network, served_on_this_machine,
};
use argonauts_core::runs::events::RunEventKind;
use argonauts_core::runs::events::run::Party;
use argonauts_core::runs::events::session::SessionEnd;
use argonauts_core::runs::events::workflow::{RunNote, WorkflowEvent};
use argonauts_core::settings::endpoints::endpoint_settings;
use argonauts_core::{Note, NoteLevel};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::Host;
use crate::engine::commands;
use crate::engine::error::{EngineError, EngineResult};
use crate::engine::job_fallback;
use crate::engine::shared::Admission;
use crate::state::sessions::SessionRecord;

impl Host {
    /// Waits while the internet is gone, unless `worker` runs on this machine. A pause or a
    /// stop ends the wait, and [`Host::admitted`] takes it from there.
    pub(super) async fn wait_for_the_network(&self, worker: &ModelRef) -> EngineResult<()> {
        self.follow_the_network();
        if self.works_offline(worker) {
            return Ok(());
        }
        let health = self.services().health.clone();
        let mut changes = health.network_changes();
        let mut admission = self.shared().admission.subscribe();
        let halt = self.shared().halt.clone();
        loop {
            if !health.network().is_down() || *admission.borrow_and_update() != Admission::Open {
                return Ok(());
            }
            tokio::select! {
                changed = changes.changed() => if changed.is_err() {
                    return Ok(());
                },
                changed = admission.changed() => if changed.is_err() {
                    return Err(EngineError::Interrupted);
                },
                () = halt.cancelled() => return Err(EngineError::Interrupted),
            }
        }
    }

    /// Whether a session of `worker` that ended with `end` lost the internet, which is still
    /// gone. It goes again on the same worker then, with that end held against nobody.
    pub(super) async fn lost_the_network(&self, worker: &ModelRef, end: &SessionEnd) -> bool {
        may_have_lost_the_network(&end.reason)
            && !self.works_offline(worker)
            && self.services().health.check_network().await.is_down()
    }

    /// [`Host::lost_the_network`] for the session `record` holds, once it ended.
    pub(super) async fn lost_the_network_in(&self, record: &SessionRecord) -> bool {
        match &record.end {
            Some(end) => self.lost_the_network(&record.started.worker, end).await,
            None => false,
        }
    }

    fn works_offline(&self, worker: &ModelRef) -> bool {
        served_on_this_machine(&worker.agent, worker.model.as_deref())
            || endpoint_settings(self.settings(), &worker.agent).is_some_and(|ends| ends.local)
    }

    /// Starts following the internet for the rest of the run, once. Following it beside the
    /// sessions says it's gone as soon as the network check finds it, while the sessions'
    /// CLIs still try on their own.
    fn follow_the_network(&self) {
        if !job_fallback::begin_following_the_network(self) {
            return;
        }
        let host = self.clone();
        tokio::spawn(async move { host.follow().await });
    }

    async fn follow(&self) {
        let mut changes = self.services().health.network_changes();
        let halt = self.shared().halt.clone();
        // A run you resume while the internet is gone holds its full span from then, not from
        // when the internet went.
        let resumed_at = self
            .read(|state| state.run.last_resumed.is_some())
            .then(|| self.now());
        loop {
            let since = loop {
                if let NetState::Down { since } = *changes.borrow_and_update() {
                    break resumed_at.map_or(since, |resumed| since.max(resumed));
                }
                if !changed(&mut changes, &halt).await {
                    return;
                }
            };
            if !self.settled() {
                self.say(NETWORK_DOWN);
            }
            let down_for = Duration::try_from(self.now() - since).unwrap_or_default();
            let left = self.settings().network.hold().saturating_sub(down_for);
            let held_too_long = tokio::time::sleep(left);
            tokio::pin!(held_too_long);
            let back = loop {
                if !changes.borrow_and_update().is_down() {
                    break true;
                }
                tokio::select! {
                    changed = changes.changed() => if changed.is_err() {
                        return;
                    },
                    () = &mut held_too_long => break false,
                    () = halt.cancelled() => return,
                }
            };
            if back {
                if !self.settled() {
                    self.say(NETWORK_BACK);
                }
                continue;
            }
            // The pause waits for you, so the internet's return says nothing more. Whatever
            // goes on after it follows the internet afresh, with a full span to hold.
            self.pause_for_the_network().await;
            job_fallback::end_following_the_network(self);
            return;
        }
    }

    /// The run is over, pausing, paused, or stopping, so the internet is no news to it.
    fn settled(&self) -> bool {
        self.read(|state| {
            let run = &state.run;
            run.finished.is_some()
                || run.paused
                || run.pause_requested.is_some()
                || run.stop_requested
        })
    }

    /// One dim row in the conversation, and the same under each workflow's header.
    fn say(&self, text: &str) {
        let note = RunEventKind::Note(Note {
            level: NoteLevel::Info,
            text: text.to_owned(),
        });
        let in_the_views = RunEventKind::Workflow(WorkflowEvent::Note(RunNote {
            text: text.to_owned(),
            workflow: None,
            left: false,
        }));
        for kind in [note, in_the_views] {
            if let Err(err) = self.record(None, None, kind) {
                tracing::warn!(%err, "couldn't say the network changed");
            }
        }
    }

    /// Pauses the run for an internet gone `network.hold_minutes`, so it waits for you rather
    /// than hold its sessions open. Its sessions stop at once, since none of them can work.
    async fn pause_for_the_network(&self) {
        if self.settled() {
            return;
        }
        let minutes = self.settings().network.hold_minutes;
        let held = match minutes {
            1 => "a minute".to_owned(),
            minutes => format!("{minutes} minutes"),
        };
        let note = RunEventKind::Note(Note {
            level: NoteLevel::Warn,
            text: format!(
                "The network has been down for {held}, so the run pauses here with its work \
                 kept. Resume it once the internet is back."
            ),
        });
        let paused = match self.record(None, None, note) {
            Ok(_) => match commands::begin_pause_by(self, true, Party::Network) {
                Ok(true) => commands::finish_pause(self).await,
                Ok(false) => Ok(()),
                Err(err) => Err(err),
            },
            Err(err) => Err(err),
        };
        if let Err(err) = paused {
            tracing::error!(%err, "the network stayed down, and pausing the run failed");
        }
    }
}

/// Waits for the next change; false once there will be none or the run halts.
async fn changed(changes: &mut watch::Receiver<NetState>, halt: &CancellationToken) -> bool {
    tokio::select! {
        changed = changes.changed() => changed.is_ok(),
        () = halt.cancelled() => false,
    }
}
