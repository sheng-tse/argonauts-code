//! Test support for Argonauts. Helpers may panic on impossible states, as test code does, but
//! anything that touches the file system or spawns a process returns an error saying what failed.

pub mod acp;
pub mod bins;
pub mod journal;
pub mod openai;
pub mod pty;
pub mod repo;
pub mod scenario;
pub mod services;
pub mod trace;
pub mod vendors;
pub mod worker;

pub use bins::{argonauts_bin, fake_vendor_bin};
pub use openai::{FakeOpenAi, FakeReply};
pub use pty::{Key, Pty, PtyCommand, Screen};
pub use repo::{EnvGuard, TempRepo};
pub use scenario::{FakeRule, FakeScenario, FakeStep, Invocation, LogLine};
pub use services::{FakeClock, FakeServices, MemorySink, engine_context};
pub use trace::Trace;
pub use vendors::FakeVendors;
pub use worker::{FakeDirectory, FakeSession, FakeWorker};
