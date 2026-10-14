//! The newest version of a CLI, asked of the place the installed one came from: Homebrew, npm
//! or the CLI's own installer. One small request, and only for an install whose source can be
//! told from where its program is. A program anywhere else is never asked about.

use std::path::Path;
use std::time::Duration;

use argonauts_core::Adapter;
use argonauts_core::adapter::InstallSources;
use serde_json::Value;

use super::command::ask;
use super::versions::numbers;
use crate::plan_usage::find_unsandboxed;

pub const NEWEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Where an installed CLI came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallSource {
    /// The formula or cask.
    Homebrew(&'static str),
    /// The package.
    Npm(&'static str),
    /// Its own installer, with the address that names the newest version where it has one.
    Own(Option<&'static str>),
}

/// The source `program` was installed from, read from the path it really is at. `None` for a
/// program anywhere `sources` doesn't account for.
pub fn install_source(program: &Path, sources: &InstallSources) -> Option<InstallSource> {
    let real = program
        .canonicalize()
        .unwrap_or_else(|_| program.to_path_buf());
    let path = real.to_string_lossy();
    let in_homebrew = ["/Cellar/", "/Caskroom/"]
        .iter()
        .any(|part| path.contains(part));
    if in_homebrew {
        return sources.homebrew.map(InstallSource::Homebrew);
    }
    if path.contains("/node_modules/") {
        return sources.npm.map(InstallSource::Npm);
    }
    let own = sources.own.filter(|own| path.contains(own.path_part))?;
    Some(InstallSource::Own(own.newest_url))
}

/// The command that updates a CLI installed from `source`, as you would type it: `brew upgrade
/// codex`, `npm i -g @openai/codex@latest`, or the CLI's own, such as `claude update`.
pub fn update_command(adapter: &dyn Adapter, source: Option<&InstallSource>) -> Option<String> {
    match source {
        Some(InstallSource::Homebrew(name)) => Some(format!("brew upgrade {name}")),
        Some(InstallSource::Npm(package)) => Some(format!("npm i -g {package}@latest")),
        Some(InstallSource::Own(_)) | None => adapter
            .update_args()
            .map(|args| format!("{} {}", adapter.binary(), args.join(" "))),
    }
}

/// The program and arguments [`update_command`] names, to run with your OK.
pub fn update_program(
    adapter: &dyn Adapter,
    source: Option<&InstallSource>,
) -> Option<(String, Vec<String>)> {
    let words = update_command(adapter, source)?;
    let mut parts = words.split(' ').map(str::to_owned);
    let program = match source {
        Some(InstallSource::Homebrew(_) | InstallSource::Npm(_)) => parts.next()?,
        // The CLI's own updater runs the program Doctor found, wherever it is.
        _ => {
            parts.next();
            String::new()
        }
    };
    Some((program, parts.collect()))
}

/// What asks `source` for the newest version: the program, by name, and its arguments.
fn request(source: &InstallSource) -> Option<(&'static str, Vec<String>)> {
    match source {
        InstallSource::Homebrew(name) => Some((
            "brew",
            vec![
                "info".to_owned(),
                "--json=v2".to_owned(),
                (*name).to_owned(),
            ],
        )),
        InstallSource::Npm(package) => Some((
            "npm",
            vec![
                "view".to_owned(),
                (*package).to_owned(),
                "version".to_owned(),
            ],
        )),
        InstallSource::Own(Some(url)) => Some((
            "curl",
            vec![
                "-fsSL".to_owned(),
                "--max-time".to_owned(),
                "10".to_owned(),
                (*url).to_owned(),
            ],
        )),
        InstallSource::Own(None) => None,
    }
}

/// Asks `source` for the newest version. `None` when it can't be asked or doesn't say.
pub async fn newest_version(source: &InstallSource, timeout: Duration) -> Option<String> {
    let (program, args) = request(source)?;
    let program = find_unsandboxed(program, None).ok()?;
    newest_from(&program, &args, source, timeout).await
}

/// Runs `program` with `args` and reads the newest version from what it prints.
pub async fn newest_from(
    program: &Path,
    args: &[String],
    source: &InstallSource,
    timeout: Duration,
) -> Option<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let answer = ask(program, &args, timeout).await.ok()?;
    if !answer.success {
        return None;
    }
    match source {
        InstallSource::Homebrew(_) => homebrew_version(&answer.text),
        InstallSource::Npm(_) | InstallSource::Own(_) => first_version(&answer.text),
    }
}

/// `versions.stable` of the formula, or `version` of the cask, in `brew info --json=v2`.
fn homebrew_version(printed: &str) -> Option<String> {
    let info: Value = serde_json::Deserializer::from_str(printed)
        .into_iter()
        .next()?
        .ok()?;
    let version = info
        .pointer("/formulae/0/versions/stable")
        .or_else(|| info.pointer("/casks/0/version"))?
        .as_str()?;
    // A cask may write `2.1.297,abc123`: the number is what comes first.
    first_version(version)
}

/// The first line that is a version number and nothing else, as `npm view` prints one.
fn first_version(printed: &str) -> Option<String> {
    printed
        .lines()
        .map(|line| line.trim().trim_start_matches('v'))
        .map(|line| line.split([',', ' ']).next().unwrap_or(line))
        .find(|line| {
            numbers(line).is_some()
                && line.contains('.')
                && line.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use argonauts_core::adapter::OwnInstaller;

    use super::*;

    const SOURCES: InstallSources = InstallSources {
        homebrew: Some("codex"),
        npm: Some("@openai/codex"),
        own: Some(OwnInstaller {
            path_part: "/claude/versions/",
            newest_url: Some("https://example.invalid/latest"),
        }),
    };

    #[test]
    fn the_source_is_read_from_where_the_program_really_is() {
        let source = |path: &str| install_source(Path::new(path), &SOURCES);
        assert_eq!(
            source("/opt/homebrew/Cellar/codex/0.160.0/bin/codex"),
            Some(InstallSource::Homebrew("codex"))
        );
        assert_eq!(
            source("/opt/homebrew/Caskroom/codex/0.160.0/codex"),
            Some(InstallSource::Homebrew("codex"))
        );
        assert_eq!(
            source("/usr/local/lib/node_modules/@openai/codex/bin/codex.js"),
            Some(InstallSource::Npm("@openai/codex"))
        );
        assert_eq!(
            source("/home/you/.local/share/claude/versions/2.1.296"),
            Some(InstallSource::Own(Some("https://example.invalid/latest")))
        );
        // A program anywhere else, such as a script of your own, is never asked about.
        assert_eq!(source("/home/you/bin/codex"), None);
        assert_eq!(source("/tmp/x/claude"), None);
    }

    #[test]
    fn a_link_is_followed_to_the_program_it_points_at() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("lib/node_modules/@openai/codex/bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("codex.js"), "").unwrap();
        let link = dir.path().join("codex");
        std::os::unix::fs::symlink(real.join("codex.js"), &link).unwrap();
        assert_eq!(
            install_source(&link, &SOURCES),
            Some(InstallSource::Npm("@openai/codex"))
        );
    }

    #[test]
    fn each_source_prints_its_newest_version_its_own_way() {
        let formula = r#"{"formulae":[{"name":"codex","versions":{"stable":"0.162.0","head":null}}],"casks":[]}"#;
        assert_eq!(homebrew_version(formula).as_deref(), Some("0.162.0"));
        let cask = "{\"formulae\":[],\"casks\":[{\"token\":\"claude-code\",\"version\":\"2.1.297,5f0c\"}]}\nWarning: something on stderr\n";
        assert_eq!(homebrew_version(cask).as_deref(), Some("2.1.297"));
        assert_eq!(homebrew_version("Error: No available formula"), None);
        assert_eq!(first_version("0.162.0\n").as_deref(), Some("0.162.0"));
        assert_eq!(
            first_version("npm warn config something\n2.1.297\n").as_deref(),
            Some("2.1.297")
        );
        assert_eq!(first_version("<html>503</html>"), None);
        assert_eq!(first_version(""), None);
    }

    /// A fake `npm`, `brew` or `curl` that prints `answer` and notes that it ran.
    fn fake_cli(dir: &Path, answer: &str) -> PathBuf {
        let path = dir.join("tool");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho ran >> '{}'\ncat <<'ANSWER'\n{answer}\nANSWER\n",
                dir.join("ran").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn the_newest_version_is_what_the_source_prints() {
        let dir = tempfile::tempdir().unwrap();
        let npm = fake_cli(dir.path(), "0.162.0");
        let source = InstallSource::Npm("@openai/codex");
        let (_, args) = request(&source).unwrap();
        assert_eq!(args, ["view", "@openai/codex", "version"]);
        let found = newest_from(&npm, &args, &source, Duration::from_secs(10)).await;
        assert_eq!(found.as_deref(), Some("0.162.0"));
        // An installer with no address for it is never asked.
        assert_eq!(request(&InstallSource::Own(None)), None);
    }

    #[test]
    fn the_update_command_is_the_sources_own() {
        let adapter = argonauts_adapter_claude_code::ClaudeCodeAdapter;
        let brew = InstallSource::Homebrew("claude-code");
        assert_eq!(
            update_command(&adapter, Some(&brew)).as_deref(),
            Some("brew upgrade claude-code")
        );
        assert_eq!(
            update_program(&adapter, Some(&brew)),
            Some((
                "brew".to_owned(),
                vec!["upgrade".to_owned(), "claude-code".to_owned()]
            ))
        );
        let npm = InstallSource::Npm("@anthropic-ai/claude-code");
        assert_eq!(
            update_command(&adapter, Some(&npm)).as_deref(),
            Some("npm i -g @anthropic-ai/claude-code@latest")
        );
        // Its own installer, or an install nobody can place, is updated by the CLI itself.
        for source in [Some(InstallSource::Own(None)), None] {
            assert_eq!(
                update_command(&adapter, source.as_ref()).as_deref(),
                Some("claude update")
            );
            assert_eq!(
                update_program(&adapter, source.as_ref()),
                Some((String::new(), vec!["update".to_owned()]))
            );
        }
    }
}
