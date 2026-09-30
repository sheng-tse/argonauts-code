//! Agents run in their own process groups, so Ctrl+C reaches only Argonauts, which turns it
//! into a stop request that stops the agent cleanly and keeps its work.

use std::io;

use tokio::signal::unix::{Signal, SignalKind, signal};

pub struct Interrupts {
    interrupt: Signal,
    terminate: Signal,
    hangup: Signal,
}

impl Interrupts {
    /// Must run inside a tokio runtime.
    pub fn install() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    /// Cancel safe.
    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
            _ = self.hangup.recv() => {}
        }
    }
}
