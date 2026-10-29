//! Approvals and the safety boundary: the approval mode, refusals inside agent sessions and
//! their descendants, confirms for safety settings, and `config set` without a terminal.

use crate::app_support;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use anyhow::Context as _;
use app_support::*;
use argonauts_core::app::{AppEvent, ChoiceKind, UiCommand, ViewData};
use argonauts_core::events::RunEventKind;
use argonauts_core::events::approval::ApprovalKind;
use argonauts_core::events::run::{Party, RunLifecycle};
use argonauts_core::layout::{run_files, user_files};
use argonauts_core::settings::approvals::ApprovalMode;
use argonauts_scenarios::{COLS, ROWS, Scenario, WAIT};
use argonauts_testkit::{Key, Pty, PtyCommand, argonauts_bin};

const AGENT_SESSION: &str = "s-001";

/// The run is exactly as it was: nothing answered, resumed, or queued for it.
fn assert_untouched(waiting: &WaitingRun, events_before: usize) -> anyhow::Result<()> {
    assert_eq!(
        run_events(&waiting.dir)?.len(),
        events_before,
        "the run changed"
    );
    assert!(
        !waiting.dir.join(run_files::PENDING_ANSWERS).exists(),
        "an answer was queued for the run"
    );
    Ok(())
}

fn path_with_first(scenario: &Scenario, dir: &std::path::Path) -> OsString {
    let rest = scenario
        .envs()
        .into_iter()
        .find(|(name, _)| name == "PATH")
        .map(|(_, value)| value)
        .unwrap_or_default();
    let mut path = OsString::from(dir.as_os_str());
    path.push(":");
    path.push(rest);
    path
}

#[test]
fn app_approval_mode_logged_and_applied() -> anyhow::Result<()> {
    // Merges land on their own by default; this run starts with its merge waiting for you.
    let scenario = scenario_with(&format!(
        "{}\n[approvals.merge]\nmode = \"supervised\"\n",
        run_config()
    ))?;
    let mut pty = start(&scenario)?;
    // The worker holds until the mode has changed, so the merge question comes after it.
    let run_dir = start_captain(&scenario, &mut pty, requests::HELD_TASK)?;
    wait_worker_started(&scenario)?;

    check_bottom_row(&pty)?;
    pty.press(Key::ShiftTab)?;
    let mode = wait_until("the new approval mode", || {
        Ok(commands(&scenario)?
            .into_iter()
            .find_map(|command| match command {
                UiCommand::ApprovalMode { mode } => Some(mode),
                _ => None,
            }))
    })?;
    assert_ne!(mode, ApprovalMode::Supervised, "Shift+Tab kept supervised");
    wait_journal(&run_dir, "the change in the journal", |event| {
        matches!(
            &event.kind,
            RunEventKind::Run(RunLifecycle::ApprovalModeChanged { mode: logged, by: Party::User })
                if *logged == mode
        )
    })?;
    // Only auto goes ahead on its own, so only auto takes the go-ahead glyph.
    let glyph = if mode == ApprovalMode::Auto {
        "⏵⏵"
    } else {
        "ǁ"
    };
    let name = format!("{mode:?}").to_lowercase();
    pty.wait_for(&format!("{glyph} {name} mode on"), WAIT)?;

    scenario.clis.signal("worker-may-finish")?;
    let merge_question = wait_approval(&scenario, ApprovalKind::Merge)?;
    assert_eq!(
        merge_question.mode, mode,
        "the next approval ignored the change"
    );
    scenario.shot(&pty, "app_approval_mode_logged_and_applied")?;
    Ok(())
}

#[test]
fn app_refuses_approval_inside_agent_session() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let waiting = fabricate_waiting_run(&scenario, 1)?;
    let events_before = run_events(&waiting.dir)?.len();

    let mut app = launch_with(
        &scenario,
        &[],
        &[("ARGONAUTS_SESSION", AGENT_SESSION.into())],
    )?;
    let code = app.wait_for_exit(WAIT)?;
    let text = app.screen().text();
    assert_ne!(code, 0, "the app ran inside an agent session:\n{text}");
    assert!(text.to_lowercase().contains("agent"), "{text}");
    assert!(
        !events(&scenario)?
            .iter()
            .any(|event| matches!(event, AppEvent::Welcome { .. })),
        "the app opened its screen"
    );

    let resume = run_cli_with(
        &scenario,
        &["resume", waiting.run.as_str()],
        &[("ARGONAUTS_SESSION", AGENT_SESSION.into())],
    )?;
    let said = output_text(&resume);
    assert!(!resume.status.success(), "{said}");
    assert!(said.to_lowercase().contains("agent"), "{said}");
    assert_untouched(&waiting, events_before)?;
    scenario.shot(&app, "app_refuses_approval_inside_agent_session")?;
    Ok(())
}

#[test]
fn app_answer_refused_from_main_agent_descendant() -> anyhow::Result<()> {
    // The waiting run waits off screen, so the main agent answers `/ask`. The narrator is off so
    // it never could, even with a run on screen.
    let scenario = scenario_with(&GLOBAL_CONFIG.replace("[ui]\n", "[ui]\nnarrator = \"off\"\n"))?;
    let waiting = fabricate_waiting_run(&scenario, 1)?;
    let events_before = run_events(&waiting.dir)?.len();

    // A `claude` ahead of the fake that, like an agent with a shell, drops the session marker
    // and tries to take the waiting run.
    let wrap = scenario.repo.scratch().join("agent-shell");
    std::fs::create_dir_all(&wrap)?;
    let resumed = scenario.repo.scratch().join("resumed.txt");
    let script = format!(
        "#!/bin/sh\n\
         case \" $* \" in\n\
         *\" dontAsk \"*)\n\
         \x20 env -u ARGONAUTS_SESSION '{bin}' --set registry.local_servers=false resume '{run}' > '{out}' 2>&1 < /dev/null\n\
         \x20 echo \"exit $?\" >> '{out}'\n\
         \x20 ;;\n\
         esac\n\
         exec '{fake}' \"$@\"\n",
        bin = argonauts_bin()?.display(),
        run = waiting.run,
        out = resumed.display(),
        fake = scenario.clis.bin_dir().join("claude").display(),
    );
    let claude = wrap.join("claude");
    std::fs::write(&claude, script)?;
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))?;

    let mut pty = launch_with(
        &scenario,
        &[],
        &[("PATH", path_with_first(&scenario, &wrap))],
    )?;
    wait_ready(&scenario, &pty)?;
    submit(&mut pty, &format!("/ask {}", requests::ASK))?;
    wait_turns_finished(&scenario, 1)?;

    let said = read(&resumed)?;
    assert!(said.contains("exit ") && !said.contains("exit 0"), "{said}");
    assert!(said.to_lowercase().contains("agent"), "{said}");
    assert_untouched(&waiting, events_before)?;
    scenario.shot(&pty, "app_answer_refused_from_main_agent_descendant")?;
    Ok(())
}

#[test]
fn app_waiting_answer_refused_inside_agent_session() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let waiting = fabricate_waiting_run(&scenario, 1)?;
    let events_before = run_events(&waiting.dir)?.len();

    // Inside the session.
    let mut inside = launch_with(
        &scenario,
        &["resume", waiting.run.as_str()],
        &[("ARGONAUTS_SESSION", AGENT_SESSION.into())],
    )?;
    let code = inside.wait_for_exit(WAIT)?;
    let text = inside.screen().text();
    assert_ne!(code, 0, "{text}");
    assert!(text.to_lowercase().contains("agent"), "{text}");
    assert_untouched(&waiting, events_before)?;

    // Below the shell of an agent the CLI started, which registered the shell's group.
    // The group gives it away even where the shell's environment can't be read.
    let mut pgid = None;
    let mut below = resume_below_marked_shell(&scenario, &waiting, |shell| {
        pgid = Some(register_agent_group(&scenario, shell)?);
        Ok(())
    })?;
    below.wait_for_exit(WAIT)?;
    let said = String::from_utf8_lossy(&below.raw()).into_owned();
    assert!(said.contains("exit=") && !said.contains("exit=0"), "{said}");
    let pgid = pgid.context("the shell's group wasn't registered")?;
    assert!(
        said.contains(&format!("agent process group {pgid}")),
        "{said}"
    );
    assert_untouched(&waiting, events_before)?;
    scenario.shot(&below, "app_waiting_answer_refused_inside_agent_session")?;
    Ok(())
}

/// Runs `resume` for the waiting run under a shell that has the session marker, with the
/// marker removed, as an agent's shell could do. `before_resume` gets the shell's pid
/// first. The shell stays the parent because a command follows.
fn resume_below_marked_shell(
    scenario: &Scenario,
    waiting: &WaitingRun,
    before_resume: impl FnOnce(u32) -> anyhow::Result<()>,
) -> anyhow::Result<Pty> {
    let pid_file = scenario.repo.scratch().join("marked-shell.pid");
    let go = scenario.repo.scratch().join("marked-shell.go");
    let script = format!(
        "echo $$ > '{pid}.tmp' && mv '{pid}.tmp' '{pid}'\n\
         while [ ! -e '{go}' ]; do sleep 0.05; done\n\
         env -u ARGONAUTS_SESSION '{bin}' --set registry.local_servers=false resume '{run}'; echo \"exit=$?\"\n",
        pid = pid_file.display(),
        go = go.display(),
        bin = argonauts_bin()?.display(),
        run = waiting.run,
    );
    let shell = Pty::spawn(
        PtyCommand::new("/bin/sh")
            .args(["-c", script.as_str()])
            .cwd(scenario.repo.root())
            .envs(scenario.envs())
            .env("ARGONAUTS_SESSION", AGENT_SESSION)
            .size(ROWS, COLS),
    )?;
    let pid = wait_until("the marked shell's pid", || {
        Ok(std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse().ok()))
    })?;
    before_resume(pid)?;
    std::fs::write(&go, "")?;
    Ok(shell)
}

/// Writes `agents/<pgid>.json` for the process group `pid` heads, as the CLI runner does for
/// each agent it starts, and returns the pgid.
fn register_agent_group(scenario: &Scenario, pid: u32) -> anyhow::Result<i32> {
    let pgid: i32 = ps_field(pid, "pgid")?.parse()?;
    let entry = serde_json::json!({
        "pgid": pgid,
        "pid": pid,
        "start_time": start_time(pid)?,
    });
    let dir = scenario.repo.home().join(user_files::AGENTS);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join(format!("{pgid}.json")),
        serde_json::to_vec(&entry)?,
    )?;
    Ok(pgid)
}

fn ps_field(pid: u32, field: &str) -> anyhow::Result<String> {
    let out = Command::new("ps")
        .args(["-o", &format!("{field}="), "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .context("running ps")?;
    anyhow::ensure!(out.status.success(), "ps found no process {pid}");
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Seconds since the epoch, as the ancestry check reads it: the kernel's start time, which
/// `ps` prints to the second.
#[cfg(not(target_os = "linux"))]
fn start_time(pid: u32) -> anyhow::Result<u64> {
    let format = time::macros::format_description!(
        "[weekday repr:short] [month repr:short] [day padding:space] \
         [hour]:[minute]:[second] [year]"
    );
    let started = time::PrimitiveDateTime::parse(&ps_field(pid, "lstart")?, &format)?;
    Ok(u64::try_from(started.assume_utc().unix_timestamp())?)
}

/// Seconds since the epoch, summed as the ancestry check's process table does: boot time
/// plus the clock ticks in `/proc/<pid>/stat`, rounded down.
#[cfg(target_os = "linux")]
fn start_time(pid: u32) -> anyhow::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    // Field 22, counted from the state that follows the parenthesized command name.
    let ticks: u64 = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .context("no start time in /proc/<pid>/stat")?
        .parse()?;
    let boot: u64 = std::fs::read_to_string("/proc/stat")?
        .lines()
        .find_map(|line| line.strip_prefix("btime "))
        .context("no btime in /proc/stat")?
        .trim()
        .parse()?;
    let hz = Command::new("getconf").arg("CLK_TCK").output()?.stdout;
    let hz: u64 = String::from_utf8(hz)?.trim().parse()?;
    Ok(boot + ticks / hz)
}

#[test]
fn app_safety_setting_confirms() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let global = scenario.repo.global_config();
    let mut pty = start(&scenario)?;

    // `/config` with an argument: `n` changes nothing, `y` writes and prints the line.
    let key = "approvals.merge.mode";
    for answer in ['n', 'y'] {
        let before = read(&global)?;
        submit(&mut pty, &format!("/config {key} auto"))?;
        answer_scope(&scenario, &mut pty, 'e', None)?;
        let confirm = wait_open_choice(&scenario, ChoiceKind::ConfirmSafetySetting)?;
        let keys: Vec<char> = confirm.options.iter().map(|o| o.key).collect();
        assert_eq!(keys, ['y', 'n'], "{confirm:?}");
        assert!(confirm.body.join("\n").contains(key), "{confirm:?}");
        answer_question(&mut pty, number_of(&confirm.options, answer))?;
        wait_choice_closed(&scenario, &confirm.id)?;
        if answer == 'n' {
            assert_eq!(read(&global)?, before, "Keep it as it is changed the file");
            assert!(!setting_changes(&scenario)?.iter().any(|(k, _, _)| k == key));
        }
    }
    let value = wait_setting_changed(&scenario, key, 1)?;
    assert!(same_toml(&value, "\"auto\""), "{value}");
    assert!(same_toml(
        &toml_value(&read(&global)?, key).unwrap_or_default(),
        "\"auto\""
    ));
    // `/config key value` says it as Claude Code's `/config` does: the key, its value, its file.
    pty.wait_for(
        r#"Set approvals\.merge\.mode to "?auto"? \(in ~/\.argonauts/config\.toml\)"#,
        WAIT,
    )?;

    // A `/permissions` pick: the TUI asks the scope, the app asks the confirm.
    let before_pick = commands(&scenario)?.len();
    submit(&mut pty, "/permissions")?;
    wait_event(&scenario, "the permissions picker", |event| {
        matches!(event, AppEvent::View { data }
            if matches!(data.as_ref(), ViewData::Settings { command: Some(c), .. } if c == "permissions"))
    })?;
    pty.press(Key::Enter)?;
    pty.press(Key::Down)?;
    pty.press(Key::Enter)?;
    // Everywhere, the fourth place to save it.
    pty.press(Key::Char('4'))?;
    let confirm = wait_open_choice(&scenario, ChoiceKind::ConfirmSafetySetting)?;
    let picked = commands(&scenario)?
        .into_iter()
        .skip(before_pick)
        .find_map(|command| match command {
            UiCommand::SetSetting { key, .. } => Some(key),
            _ => None,
        })
        .expect("the picker sent no SetSetting");
    assert!(confirm.body.join("\n").contains(&picked), "{confirm:?}");
    answer_question(&mut pty, number_of(&confirm.options, 'y'))?;
    wait_choice_closed(&scenario, &confirm.id)?;
    wait_setting_changed(&scenario, &picked, 1)?;
    assert!(
        toml_value(&read(&global)?, &picked).is_some(),
        "{picked} not written"
    );

    // Adding an endpoint names its base URL.
    submit(
        &mut pty,
        "/config endpoints.localproxy.base_url http://127.0.0.1:9/v1",
    )?;
    answer_scope(&scenario, &mut pty, 'e', None)?;
    let confirm = wait_open_choice(&scenario, ChoiceKind::ConfirmSafetySetting)?;
    assert!(
        confirm
            .body
            .join("\n")
            .contains("endpoints.localproxy.base_url"),
        "{confirm:?}"
    );
    answer_question(&mut pty, number_of(&confirm.options, 'y'))?;
    wait_choice_closed(&scenario, &confirm.id)?;
    wait_setting_changed(&scenario, "endpoints.localproxy.base_url", 1)?;
    assert!(read(&global)?.contains("http://127.0.0.1:9/v1"));
    assert!(
        read(&global)?.contains("# Written by the scenario test"),
        "a comment was lost"
    );
    scenario.shot(&pty, "app_safety_setting_confirms")?;
    Ok(())
}

#[test]
fn app_config_set_safety_key_needs_a_terminal() -> anyhow::Result<()> {
    let scenario = scenario()?;
    let global = scenario.repo.global_config();
    let before = read(&global)?;

    let refused = run_cli(&scenario, &["config", "set", "sandbox.network", "true"])?;
    let said = output_text(&refused);
    assert!(!refused.status.success(), "{said}");
    assert!(said.to_lowercase().contains("terminal"), "{said}");
    assert_eq!(read(&global)?, before, "the global file changed");
    assert!(
        !project_config(&scenario).exists(),
        "a project file was written"
    );
    assert!(!project_local_config(&scenario).exists());

    // A key that isn't a safety key saves without one, and the file keeps its comments.
    let saved = run_cli(
        &scenario,
        &["config", "set", "ui.theme", "light", "--global"],
    )?;
    assert!(saved.status.success(), "{}", output_text(&saved));
    let text = read(&global)?;
    assert!(
        same_toml(
            &toml_value(&text, "ui.theme").unwrap_or_default(),
            "\"light\""
        ),
        "{text}"
    );
    assert!(text.contains("# Written by the scenario test"), "{text}");
    Ok(())
}
