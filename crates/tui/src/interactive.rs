//! The interactive app screen, fed by the app controller over two channels.

use argonauts_core::app::{AppEvent, UiCommand};
use tokio::sync::mpsc;

use crate::UiOptions;

pub struct AppIo {
    pub commands: mpsc::Sender<UiCommand>,
    pub events: mpsc::Receiver<AppEvent>,
}

/// An inline viewport like Claude Code's, with history in scrollback above the live region.
pub async fn run_app(io: AppIo, options: UiOptions) -> anyhow::Result<()> {
    let _ = (io, options);
    anyhow::bail!("argonauts-tui: the interactive screen is not implemented yet")
}

pub async fn run_screen_reader(io: AppIo, options: UiOptions) -> anyhow::Result<()> {
    let _ = (io, options);
    anyhow::bail!("argonauts-tui: the screen reader mode is not implemented yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_placeholders_say_they_are_not_built() {
        let (commands, _) = mpsc::channel(1);
        let (_, events) = mpsc::channel(1);
        let err = run_app(AppIo { commands, events }, UiOptions::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not implemented"));
    }
}
