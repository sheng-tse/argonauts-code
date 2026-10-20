//! Test support for Argonauts Code. Helpers may panic on impossible states, as test code does, but
//! anything that touches the file system or spawns a process returns an error saying what failed.

pub mod acp;
pub mod bins;
pub mod fake_clis;
pub mod journal;
pub mod model_server;
pub mod openai;
pub mod pty;
pub mod repo;
pub mod runner;
pub mod scenario;
pub mod services;
pub mod trace;
pub mod wide_glyphs;

pub use bins::{argonauts_bin, fake_cli_bin};
pub use fake_clis::FakeClis;
pub use model_server::{FakeKind, FakeModelServer};
pub use openai::{FakeOpenAi, FakeReply};
pub use pty::{Key, Pty, PtyCommand, Screen};
pub use repo::{EnvGuard, TempRepo};
pub use runner::{FakeDirectory, FakeRunner, FakeSession};
pub use scenario::{FakeRule, FakeScenario, FakeStep, Invocation, LogLine};
pub use services::{FakeClock, FakeServices, MemorySink, engine_context};
pub use trace::Trace;
