//! Reading the fake agents' log: invocations and ACP frames.

use std::thread;
use std::time::{Duration, Instant};

use anyhow::bail;
use argonauts_testkit::{Invocation, LogLine};
use serde_json::Value;

use super::Limits;

/// One ACP frame from the log, with its place in the log so tests can order frames.
#[derive(Debug, Clone)]
pub struct Frame {
    pub index: usize,
    pub from_agent: bool,
    pub frame: Value,
}

impl Frame {
    pub fn method(&self) -> Option<&str> {
        self.frame.get("method").and_then(Value::as_str)
    }

    /// The `session/update` payload, if this is one.
    pub fn update(&self) -> Option<&Value> {
        self.frame.pointer("/params/update")
    }

    pub fn text(&self) -> String {
        self.frame.to_string()
    }
}

impl Limits {
    /// Every invocation of `program`, in order. The checks that ask a CLI about itself, its
    /// version, help, and model list, are left out.
    pub fn calls(&self, program: &str) -> anyhow::Result<Vec<Invocation>> {
        Ok(self
            .clis
            .invocations()?
            .into_iter()
            .filter(|call| call.argv0 == program && !is_self_check(&call.args))
            .collect())
    }

    /// Every invocation of `program` with `arg` among its arguments.
    pub fn calls_with(&self, program: &str, arg: &str) -> anyhow::Result<Vec<Invocation>> {
        Ok(self
            .calls(program)?
            .into_iter()
            .filter(|call| call.args.iter().any(|a| a == arg))
            .collect())
    }

    /// The first invocation of `program` with `arg` among its arguments.
    pub fn call_with(&self, program: &str, arg: &str) -> anyhow::Result<Option<Invocation>> {
        Ok(self.calls_with(program, arg)?.into_iter().next())
    }

    pub fn wait_for_call(
        &self,
        program: &str,
        arg: &str,
        timeout: Duration,
    ) -> anyhow::Result<Invocation> {
        let deadline = argonauts_testkit::pty::deadline::after(timeout);
        loop {
            if let Some(call) = self.call_with(program, arg)? {
                return Ok(call);
            }
            if Instant::now() >= deadline {
                bail!(
                    "no {program} call with {arg:?} in {timeout:?}; the calls were {:?}",
                    self.calls(program)?
                        .iter()
                        .map(|call| &call.args)
                        .collect::<Vec<_>>()
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn frames(&self) -> anyhow::Result<Vec<Frame>> {
        Ok(self
            .clis
            .log()?
            .into_iter()
            .enumerate()
            .filter_map(|(index, line)| match line {
                LogLine::AcpFrame {
                    from_agent, frame, ..
                } => Some(Frame {
                    index,
                    from_agent,
                    frame,
                }),
                _ => None,
            })
            .collect())
    }

    pub fn wait_for_frame(
        &self,
        what: &str,
        timeout: Duration,
        pick: impl Fn(&Frame) -> bool,
    ) -> anyhow::Result<Frame> {
        let deadline = argonauts_testkit::pty::deadline::after(timeout);
        loop {
            if let Some(frame) = self.frames()?.into_iter().find(|frame| pick(frame)) {
                return Ok(frame);
            }
            if Instant::now() >= deadline {
                bail!("no {what} frame in {timeout:?}");
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Each `argonauts_report` call an ACP agent made, with the tool's reply.
    pub fn report_calls(&self) -> anyhow::Result<Vec<(Value, Value)>> {
        Ok(self
            .clis
            .log()?
            .into_iter()
            .filter_map(|line| match line {
                LogLine::ReportCall { report, reply, .. } => Some((report, reply)),
                _ => None,
            })
            .collect())
    }

    /// Permission picks the fake agent didn't expect.
    pub fn wrong_picks(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .clis
            .log()?
            .into_iter()
            .filter_map(|line| match line {
                LogLine::WrongPick {
                    expected, picked, ..
                } => Some(format!("expected {expected}, picked {picked}")),
                _ => None,
            })
            .collect())
    }
}

/// `--version`, `--help` (the doctor's flag check), and the model lists discovery reads.
fn is_self_check(args: &[String]) -> bool {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    matches!(args.first(), Some(&"--version"))
        || args.last() == Some(&"--help")
        || args == ["debug", "models"]
        || args == ["--output-format", "json", "models"]
}

/// A tool call update for `id` with `status`, from the agent.
pub fn tool_update(frame: &Frame, id: &str, status: &str) -> bool {
    frame.from_agent
        && frame.update().is_some_and(|update| {
            update.get("toolCallId").and_then(Value::as_str) == Some(id)
                && update.get("status").and_then(Value::as_str) == Some(status)
        })
}

/// A request from the client with `method`.
pub fn client_call(frame: &Frame, method: &str) -> bool {
    !frame.from_agent && frame.method() == Some(method)
}
