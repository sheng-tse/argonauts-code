//! The live narrator for the attached run, hosted here because it serves the person watching.
//! Its lines go to the screen and to `narrator.jsonl`, never into an agent's context.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use argonauts_core::events::RunEvent;
use argonauts_core::layout::run_files;
use argonauts_core::policy::{
    Narrator, NarratorError, NarratorLine, NarratorLineKind, NarratorStatus,
};
use argonauts_core::settings::ui::NarratorMode;
use argonauts_core::{RunId, Spend};
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use super::Ctx;
use super::Internal;
use super::runs::RunUpdate;

pub(crate) struct NarratorHost {
    run: RunId,
    narrator: Arc<dyn Narrator>,
    internal: mpsc::UnboundedSender<Internal>,
    journal: PathBuf,
    /// Set after the first failed write, so a missing run folder says so once, not per line.
    write_failed: bool,
    forward: JoinHandle<()>,
}

impl NarratorHost {
    /// The narrator for `run`, its lines forwarded as `RunUpdate::Narrator`.
    pub(crate) fn start(ctx: &Ctx, run: &RunId) -> Self {
        let run_dir = ctx.env.run_dir(run);
        let narrator = argonauts_policy::narrator(
            ctx.settings.shared(),
            run.clone(),
            &run_dir,
            Arc::clone(&ctx.services.workers),
            Arc::clone(&ctx.services.steps),
        );
        let forward = forward_lines(run.clone(), narrator.lines(), ctx.internal.clone());
        Self {
            run: run.clone(),
            narrator,
            internal: ctx.internal.clone(),
            journal: run_dir.join(run_files::NARRATOR),
            write_failed: false,
            forward,
        }
    }

    pub(crate) fn observe(&self, event: &RunEvent) {
        self.narrator.observe(event);
    }

    /// Brings the mode and model in line with the settings in effect. True when either changed.
    pub(crate) fn apply(&self, ctx: &Ctx) -> bool {
        let settings = ctx.settings();
        let before = self.narrator.status();
        let model = settings
            .narrator
            .model
            .clone()
            .unwrap_or_else(|| settings.main_agent.clone());
        if before.mode != settings.ui.narrator {
            self.narrator.set_mode(settings.ui.narrator);
        }
        if before.model != model {
            self.narrator.set_model(model);
        }
        self.narrator.status() != before
    }

    pub(crate) fn status(&self) -> NarratorStatus {
        self.narrator.status()
    }

    pub(crate) fn is_on(&self) -> bool {
        self.status().mode != NarratorMode::Off
    }

    /// `s` in the status view. The line comes back as `RunUpdate::Narrator`, so a model call never holds up
    /// the app.
    pub(crate) fn summary(&self) {
        let narrator = Arc::clone(&self.narrator);
        self.reply(async move { narrator.summary().await }, "write a summary");
    }

    /// `/ask` during a run.
    pub(crate) fn ask(&self, question: String) {
        let narrator = Arc::clone(&self.narrator);
        self.reply(async move { narrator.ask(question).await }, "answer");
    }

    fn reply<F>(&self, call: F, what: &'static str)
    where
        F: Future<Output = Result<NarratorLine, NarratorError>> + Send + 'static,
    {
        let run = self.run.clone();
        let internal = self.internal.clone();
        let model = self.narrator.status().model;
        tokio::spawn(async move {
            let line = call.await.unwrap_or_else(|err| NarratorLine {
                ts: OffsetDateTime::now_utc(),
                run: Some(run.clone()),
                kind: NarratorLineKind::Notice,
                text: format!("The narrator couldn't {what}: {err}."),
                model,
                spend: Spend::default(),
            });
            // Only a closed app drops it, and then nobody is left to read it.
            let _ = internal.send(Internal::Run(RunUpdate::Narrator { run, line }));
        });
    }

    /// Appends the line to `narrator.jsonl`. Only the first failure is an error.
    pub(crate) fn record(&mut self, line: &NarratorLine) -> anyhow::Result<()> {
        if self.write_failed {
            return Ok(());
        }
        let written = append_line(&self.journal, line);
        if written.is_err() {
            self.write_failed = true;
        }
        written
    }
}

impl Drop for NarratorHost {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

fn append_line(path: &Path, line: &NarratorLine) -> anyhow::Result<()> {
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

fn forward_lines(
    run: RunId,
    mut lines: broadcast::Receiver<NarratorLine>,
    internal: mpsc::UnboundedSender<Internal>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match lines.recv().await {
                Ok(line) => {
                    let update = RunUpdate::Narrator {
                        run: run.clone(),
                        line,
                    };
                    if internal.send(Internal::Run(update)).is_err() {
                        return;
                    }
                }
                // Live updates replace each other, so skipping the oldest loses nothing.
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_append_to_the_journal_and_a_failure_reports_once() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let path = dir.path().join(run_files::NARRATOR);
        let line = NarratorLine {
            ts: OffsetDateTime::UNIX_EPOCH,
            run: RunId::new("run-001").ok(),
            kind: NarratorLineKind::Update,
            text: "Two workers are building.".to_owned(),
            model: argonauts_core::names::WorkerRef::new("claude-code", None),
            spend: Spend::default(),
        };
        append_line(&path, &line).expect("first");
        append_line(&path, &line).expect("second");
        let text = std::fs::read_to_string(&path).expect("the journal");
        assert_eq!(text.lines().count(), 2);
        let back: NarratorLine =
            serde_json::from_str(text.lines().next().expect("a line")).expect("json");
        assert_eq!(back, line);
        assert!(append_line(&dir.path().join("missing/narrator.jsonl"), &line).is_err());
    }
}
