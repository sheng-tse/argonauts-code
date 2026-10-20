//! Fills a scenario's reset times in, since an agent's reset must be relative to now, and
//! makes its fixture paths absolute so the script works from a temp folder.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use argonauts_scenarios::scenario_file;
use argonauts_testkit::FakeScenario;
use argonauts_testkit::FakeStep;
use argonauts_testkit::acp::AcpStep;
use time::OffsetDateTime;

/// Replaced by the Unix time [`SOON`] from now, in seconds, as Claude Code's `resetsAt` is.
pub const RESET_SOON: &str = "@RESET_SOON@";

/// Replaced by the Unix time two hours from now, for an agent that must stay out.
pub const RESET_LATER: &str = "@RESET_LATER@";

/// How soon an agent comes back in the scenarios that wait for it.
pub const SOON: Duration = Duration::from_secs(20);

const LATER: Duration = Duration::from_secs(2 * 60 * 60);

/// Writes the rendered `limits/<name>.toml` into `folder` and returns its path.
pub fn render(name: &str, folder: &Path) -> anyhow::Result<PathBuf> {
    let source = scenario_file(&format!("limits/{name}.toml"));
    let base = source
        .parent()
        .context("a scenario file has a folder")?
        .to_path_buf();
    let mut scenario = FakeScenario::load(&source)?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let values = [
        (RESET_SOON, now + SOON.as_secs() as i64),
        (RESET_LATER, now + LATER.as_secs() as i64),
    ];
    for step in scenario.rules.iter_mut().flat_map(|rule| &mut rule.steps) {
        match step {
            FakeStep::Emit(line) | FakeStep::Stderr(line) => *line = fill(line, &values),
            FakeStep::EmitFile(path) => *path = absolute(&base, path),
            _ => {}
        }
    }
    let acp_steps = scenario
        .acp
        .iter_mut()
        .flat_map(|rule| &mut rule.turns)
        .flat_map(|turn| &mut turn.steps);
    for step in acp_steps {
        if let AcpStep::UpdateFile(path) = step {
            *path = absolute(&base, path);
        }
    }
    let rendered = folder.join(format!("{name}.toml"));
    scenario.save(&rendered)?;
    Ok(rendered)
}

fn fill(line: &str, values: &[(&str, i64)]) -> String {
    values
        .iter()
        .fold(line.to_owned(), |line, (placeholder, value)| {
            line.replace(placeholder, &value.to_string())
        })
}

fn absolute(base: &Path, path: &str) -> String {
    base.join(path).display().to_string()
}
