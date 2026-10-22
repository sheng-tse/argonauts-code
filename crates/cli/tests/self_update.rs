//! `argonauts self-update` as a shell runs it. No test reaches the real manifests: each names a
//! server on this computer, or turns updates off.

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};

use argonauts_testkit::TempRepo;

fn self_update(repo: &TempRepo, args: &[&str], envs: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_argonauts"))
        .arg("self-update")
        .args(args)
        .current_dir(repo.root())
        .envs(repo.envs())
        .env_remove("ARGONAUTS_SESSION")
        .env_remove("ARGONAUTS_LOG")
        .env_remove("ARGONAUTS_DISABLE_UPDATES")
        .env_remove("ARGONAUTS_UPDATE_TEST_KEY")
        .envs(envs.iter().copied())
        .stdin(Stdio::null())
        .output()
        .expect("the argonauts binary runs")
}

fn describe(output: &Output) -> String {
    format!(
        "status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A server on this computer that answers every request with `body`, as a manifest server
/// serving something nobody signed. Its address, as `ARGONAUTS_UPDATE_BASE` takes it.
fn unsigned_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/updates/v1", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }
    });
    address
}

#[test]
fn updates_turned_off_say_so() {
    let repo = TempRepo::new().unwrap();
    let output = self_update(&repo, &["--check"], &[("ARGONAUTS_DISABLE_UPDATES", "1")]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Argonauts Code isn't updated here: updates are off: ARGONAUTS_DISABLE_UPDATES is set.\n"
    );
}

#[test]
fn a_manifest_no_trusted_key_signed_is_refused() {
    let repo = TempRepo::new().unwrap();
    let base = unsigned_server(r#"{"serial": 1, "version": "9.9.9", "platforms": {}}"#);
    let output = self_update(&repo, &["--check"], &[("ARGONAUTS_UPDATE_BASE", &base)]);
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.contains("its signature isn't from a key Argonauts Code trusts"),
        "{said}"
    );
}

#[test]
fn manifests_come_only_over_https_or_from_this_computer() {
    let repo = TempRepo::new().unwrap();
    let output = self_update(
        &repo,
        &["--check"],
        &[("ARGONAUTS_UPDATE_BASE", "http://example.com/updates/v1")],
    );
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("ARGONAUTS_UPDATE_BASE must be an https address"),
        "{}",
        describe(&output)
    );
}

/// The test's own build wasn't put in place by the installer, so it names the releases and
/// replaces nothing. Following beta from now on is written to your settings all the same.
#[test]
fn a_copy_built_from_source_points_at_the_releases_and_keeps_the_channel() {
    let repo = TempRepo::new().unwrap();
    let output = self_update(
        &repo,
        &["--channel", "beta"],
        &[("ARGONAUTS_UPDATE_BASE", "http://127.0.0.1:9/updates/v1")],
    );
    assert_eq!(output.status.code(), Some(0), "{}", describe(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.starts_with("From now on Argonauts Code follows beta.\n"),
        "{said}"
    );
    assert!(said.contains("wasn't put here by its installer"), "{said}");
    assert!(said.contains("/releases"), "{said}");
    let config = std::fs::read_to_string(repo.home().join("config.toml")).unwrap();
    assert!(config.contains("channel = \"beta\""), "{config}");
}

/// cargo's install gets cargo's releases, none of them a beta, so `--channel` is refused there
/// with the command that updates it, and nothing is kept.
#[test]
fn a_package_managers_install_has_no_channel_to_choose() {
    let repo = TempRepo::new().unwrap();
    let cargo_bin = repo.user_home().join(".cargo/bin");
    std::fs::create_dir_all(&cargo_bin).unwrap();
    let program = cargo_bin.join("argonauts");
    let built = env!("CARGO_BIN_EXE_argonauts");
    std::fs::hard_link(built, &program)
        .or_else(|_| std::fs::copy(built, &program).map(|_| ()))
        .unwrap();
    let output = Command::new(&program)
        .args(["self-update", "--channel", "beta"])
        .current_dir(repo.root())
        .envs(repo.envs())
        .env("CARGO_HOME", repo.user_home().join(".cargo"))
        .env("ARGONAUTS_UPDATE_BASE", "http://127.0.0.1:9/updates/v1")
        .env_remove("ARGONAUTS_SESSION")
        .env_remove("ARGONAUTS_DISABLE_UPDATES")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", describe(&output));
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.starts_with("cargo installed Argonauts Code, so cargo updates it:"),
        "{said}"
    );
    assert!(said.contains("so there's no channel to choose"), "{said}");
    assert!(!said.contains("From now on"), "{said}");
    let config = std::fs::read_to_string(repo.home().join("config.toml")).unwrap_or_default();
    assert!(!config.contains("channel"), "{config}");
}
