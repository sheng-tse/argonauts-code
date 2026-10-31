//! Checks a project's own settings files name, read straight from the files, for a run that
//! has none to say why: a project file you haven't trusted names them, and the run left its
//! safety settings out.

use std::path::Path;

use argonauts_core::base::layout::project_files::{CONFIG, CONFIG_LOCAL};
use argonauts_core::prompts::prompt_files::PROJECT_DIR;
use argonauts_core::runs::events::run::OverrideRecord;

/// The project file under `root` that names checks while no layer of the run's settings set
/// `checks.run` (`overrides` lists every key a layer set), so the run left them out: a file
/// you haven't trusted, whose safety settings wait for you. As the run's folder names it,
/// such as `.argonauts/config.toml`.
pub fn untrusted_checks_file(root: &Path, overrides: &[OverrideRecord]) -> Option<String> {
    if overrides.iter().any(|set| set.key == "checks.run") {
        return None;
    }
    [CONFIG, CONFIG_LOCAL].into_iter().find_map(|name| {
        let text = std::fs::read_to_string(root.join(PROJECT_DIR).join(name)).ok()?;
        let table = text.parse::<toml::Table>().ok()?;
        let named = table
            .get("checks")?
            .get("run")?
            .as_array()
            .is_some_and(|checks| !checks.is_empty());
        named.then(|| format!("{PROJECT_DIR}/{name}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks a project file names, which the run's settings don't hold, wait for you to trust
    /// that file. Checks a layer set, even to none, and a file that names none, are no reason.
    #[test]
    fn checks_in_a_file_the_run_left_out_name_that_file() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(PROJECT_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(untrusted_checks_file(repo.path(), &[]), None);
        std::fs::write(dir.join(CONFIG), "[checks]\nrun = []\n").unwrap();
        assert_eq!(untrusted_checks_file(repo.path(), &[]), None);
        std::fs::write(
            dir.join(CONFIG_LOCAL),
            "[checks]\nrun = [{ name = \"tests\", command = [\"pytest\"] }]\n",
        )
        .unwrap();
        assert_eq!(
            untrusted_checks_file(repo.path(), &[]).as_deref(),
            Some(".argonauts/config.local.toml")
        );
        let set = OverrideRecord {
            key: "checks.run".to_owned(),
            value: "[]".to_owned(),
            from: "flag".to_owned(),
        };
        assert_eq!(untrusted_checks_file(repo.path(), &[set]), None);
    }
}
