//! The tests that use this crate from outside, one module per file, built as one program so
//! that a change links it once instead of once per file. retired_names stays a program of its
//! own, since the checks run it by name.

mod fake_acp;
mod fake_cli;
mod openai_endpoint;
mod pictures;
mod pty;
mod runners;
mod services;
