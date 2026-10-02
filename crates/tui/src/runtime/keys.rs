//! Terminal input as a stream the loop can stop reading while a child program has the
//! terminal. Crossterm's reader keeps reading stdin even when nobody polls it, so pausing drops
//! the stream, as Codex's `event_stream.rs` explains.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use crossterm::event::{Event, EventStream};
use futures::{Stream, StreamExt};

pub(crate) trait KeySource: Stream<Item = io::Result<Event>> + Unpin {
    fn pause(&mut self) {}
    fn resume(&mut self) {}
}

/// The real terminal's events.
pub(crate) struct TerminalKeys {
    stream: Option<EventStream>,
}

impl TerminalKeys {
    pub(crate) fn new() -> Self {
        Self {
            stream: Some(EventStream::new()),
        }
    }
}

impl Stream for TerminalKeys {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().stream.as_mut() {
            Some(stream) => stream.poll_next_unpin(cx),
            // Nothing is polled while paused; resuming makes a new stream.
            None => Poll::Pending,
        }
    }
}

impl KeySource for TerminalKeys {
    fn pause(&mut self) {
        self.stream = None;
    }

    fn resume(&mut self) {
        if self.stream.is_none() {
            self.stream = Some(EventStream::new());
        }
    }
}

/// Events from anywhere, such as a test's script. Nothing to pause.
pub(crate) struct GivenKeys<S> {
    stream: Pin<Box<S>>,
}

impl<S: Stream<Item = io::Result<Event>>> GivenKeys<S> {
    pub(crate) fn new(stream: S) -> Self {
        Self {
            stream: Box::pin(stream),
        }
    }
}

impl<S: Stream<Item = io::Result<Event>>> Stream for GivenKeys<S> {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().stream.as_mut().poll_next(cx)
    }
}

impl<S: Stream<Item = io::Result<Event>>> KeySource for GivenKeys<S> {}
