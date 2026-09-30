//! Fake vendor CLIs on `PATH`: the `argonauts-fake-vendor` binary installed under every name
//! in [`PROGRAMS`], all playing one scenario file.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::bins::fake_vendor_bin;
use crate::scenario::{Invocation, LOG_ENV, LogLine, SCENARIO_ENV, read_log};

pub const PROGRAMS: [&str; 7] = [
    "claude", "codex", "agy", "opencode", "hermes", "openclaw", "goose",
];

/// Chat sends the fake accepts when no rule matches: it logs the invocation and exits 0.
pub const CHAT_COMMANDS: [&[&str]; 2] = [&["hermes", "send"], &["openclaw", "message", "send"]];

pub fn is_chat_command(argv0: &str, args: &[String]) -> bool {
    CHAT_COMMANDS.iter().any(|command| {
        command[0] == argv0
            && args.len() >= command.len() - 1
            && command[1..].iter().zip(args).all(|(want, arg)| want == arg)
    })
}

pub struct FakeVendors {
    bin_dir: PathBuf,
    scenario: PathBuf,
    log: PathBuf,
}

impl FakeVendors {
    /// Installs the fakes into `dir/bin`, playing `scenario`, logging to `dir/fake-log.jsonl`.
    pub fn install(dir: &Path, scenario: &Path) -> anyhow::Result<Self> {
        Self::install_with(&fake_vendor_bin()?, dir, scenario)
    }

    /// Like `install`, with the fake vendor binary given, such as a test's
    /// `CARGO_BIN_EXE_argonauts-fake-vendor`.
    pub fn install_with(binary: &Path, dir: &Path, scenario: &Path) -> anyhow::Result<Self> {
        let bin_dir = dir.join("bin");
        fs::create_dir_all(&bin_dir).with_context(|| format!("creating {}", bin_dir.display()))?;
        for program in PROGRAMS {
            let link = bin_dir.join(program);
            if link.exists() || link.is_symlink() {
                fs::remove_file(&link).with_context(|| format!("replacing {}", link.display()))?;
            }
            std::os::unix::fs::symlink(binary, &link)
                .with_context(|| format!("linking {}", link.display()))?;
        }
        let scenario = scenario
            .canonicalize()
            .with_context(|| format!("finding the scenario {}", scenario.display()))?;
        Ok(Self {
            bin_dir,
            scenario,
            log: dir.join("fake-log.jsonl"),
        })
    }

    pub fn bin_dir(&self) -> &Path {
        &self.bin_dir
    }

    pub fn log_path(&self) -> &Path {
        &self.log
    }

    /// `PATH` with the fakes first, plus the scenario and log variables.
    pub fn envs(&self) -> Vec<(String, OsString)> {
        let mut path = OsString::from(self.bin_dir.as_os_str());
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        vec![
            ("PATH".to_owned(), path),
            (
                SCENARIO_ENV.to_owned(),
                self.scenario.clone().into_os_string(),
            ),
            (LOG_ENV.to_owned(), self.log.clone().into_os_string()),
        ]
    }

    pub fn log(&self) -> anyhow::Result<Vec<LogLine>> {
        read_log(&self.log)
    }

    pub fn invocations(&self) -> anyhow::Result<Vec<Invocation>> {
        Ok(self
            .log()?
            .into_iter()
            .filter_map(|line| match line {
                LogLine::Invocation(invocation) => Some(invocation),
                _ => None,
            })
            .collect())
    }

    /// The argument vector of every chat send, program name first.
    pub fn chat_sends(&self) -> anyhow::Result<Vec<Vec<String>>> {
        Ok(self
            .invocations()?
            .into_iter()
            .filter(|invocation| is_chat_command(&invocation.argv0, &invocation.args))
            .map(|invocation| {
                std::iter::once(invocation.argv0)
                    .chain(invocation.args)
                    .collect()
            })
            .collect())
    }
}
