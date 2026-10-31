//! The home folder in the header: started there, the app names it in full, as Claude Code does,
//! where a folder under it keeps `~/…`.

use crate::app_support;

use app_support::*;
use argonauts_scenarios::{Scenario, WAIT, as_set_up, launch_defaults};
use argonauts_testkit::screen::REAL_SIZES;
use argonauts_testkit::{Pty, PtyCommand, argonauts_bin};

#[test]
fn app_started_in_the_home_folder_names_it_in_full() -> anyhow::Result<()> {
    for size in &REAL_SIZES[..2] {
        let scenario = Scenario::new(AGENTS)?;
        std::fs::write(scenario.repo.global_config(), as_set_up(GLOBAL_CONFIG))?;
        let home = scenario.repo.user_home();
        let servers = no_local_servers()?;
        let args: Vec<&str> = servers.iter().map(String::as_str).collect();
        let pty = Pty::spawn(
            PtyCommand::new(argonauts_bin()?)
                .args(launch_defaults(&args))
                .cwd(home)
                .envs(scenario.envs())
                .env_remove("ARGONAUTS_SESSION")
                .size(size.rows, size.cols),
        )?;
        let screen = pty.wait_for_visible("shift\\+tab to cycle", WAIT)?;
        let text = screen.visible_text();
        // Each header line ends with its words, after the mascot's cells.
        let header: Vec<&str> = text.lines().take(5).map(str::trim_end).collect();
        let named = [home.to_path_buf(), home.canonicalize()?]
            .iter()
            .any(|path| {
                let path = format!(" {}", path.display());
                header.iter().any(|line| line.ends_with(&path))
            });
        assert!(
            named,
            "{} × {}: the header names {} in full:\n{text}",
            size.cols,
            size.rows,
            home.display()
        );
        assert!(
            !header.iter().any(|line| line.ends_with(" ~")),
            "{} × {}: the header names the home folder ~:\n{text}",
            size.cols,
            size.rows
        );
    }
    Ok(())
}
