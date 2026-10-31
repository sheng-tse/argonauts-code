//! Test support for Argonauts Code. Helpers may panic on impossible states, as test code does, but
//! anything that touches the file system or spawns a process returns an error saying what failed.

pub mod approved;
pub mod engine;
pub mod fake_clis;
pub mod home;
pub mod pictures;
pub mod pty;
pub mod screen;
pub mod servers;

pub use engine::runner::{FakeDirectory, FakeRunner, FakeSession, Hold};
pub use engine::services::{FakeClock, FakeServices, MemorySink, engine_context};
pub use fake_clis::FakeClis;
pub use fake_clis::fake_gh::{FakeGh, GhWorld};
pub use fake_clis::scenario::{FakeRule, FakeScenario, FakeStep, Invocation, LogLine};
pub use fake_clis::script::write_script;
pub use home::bins::{argonauts_bin, fake_cli_bin};
pub use home::repo::{EnvGuard, TempRepo};
pub use home::trace::Trace;
pub use pty::{Key, Pty, PtyCommand, Screen};
pub use servers::model_server::{FakeKind, FakeModelServer};
pub use servers::openai::{FakeOpenAi, FakeReply};
