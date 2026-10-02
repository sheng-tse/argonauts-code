//! `live` mode: an update every `narrator.interval`, only when the digest changed since the
//! last one, and one narration when each phase ends.

use std::sync::Weak;
use std::time::Duration;

use tokio::time::{self, MissedTickBehavior};

use super::LiveNarrator;

/// Runs until the narrator is dropped. Each tick narrates only when there's something new, and
/// a phase end wakes it at once. With no interval, only phase ends are narrated.
pub async fn run(narrator: Weak<LiveNarrator>, interval: Option<Duration>) {
    let Some(wake) = narrator.upgrade().map(|narrator| narrator.wake.clone()) else {
        return;
    };
    let mut ticker = interval.filter(|every| !every.is_zero()).map(|every| {
        let mut ticker = time::interval_at(time::Instant::now() + every, every);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker
    });
    loop {
        let ticked = tokio::select! {
            () = wake.notified() => false,
            () = tick(ticker.as_mut()) => true,
        };
        // Held only while narrating, so dropping the narrator ends this task.
        let Some(narrator) = narrator.upgrade() else {
            return;
        };
        narrator.phase_ends().await;
        if ticked {
            narrator.update().await;
        }
    }
}

async fn tick(ticker: Option<&mut time::Interval>) {
    match ticker {
        Some(ticker) => {
            ticker.tick().await;
        }
        None => std::future::pending().await,
    }
}
