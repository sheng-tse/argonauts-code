//! The harness adapters beyond the built-in four: the `[[harness]]` entries the registry
//! accepts, with your `agents.<id>.binary` in place of theirs, and the built-in home templates
//! they start from.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use argonauts_adapter_acp::acp_adapter;
use argonauts_adapter_manifest::manifest_adapter_with;
use argonauts_conductor::quota::find_unsandboxed;
use argonauts_core::adapter::Transport;
use argonauts_core::layout::user_files;
use argonauts_core::paths::{create_private_dir_all, write_private_file};
use argonauts_core::registry::{HarnessManifest, ModelRegistry};
use argonauts_core::settings::Settings;
use argonauts_core::{Adapter, Paths};
use argonauts_registry::harnesses::{builtin_home_template, check_manifest};
use argonauts_registry::models::discovery::ClaudeSource;
use argonauts_registry::models::{
    CatalogSource, ProjectModels, project_models_path, registry_with_trust,
};

use crate::trust::TrustStore;

/// A repo's `.argonauts/models.toml`, and the state folder whose `trusted.json` says whether
/// its entries count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoModels {
    root: PathBuf,
    state_dir: PathBuf,
}

impl RepoModels {
    pub(crate) fn new(root: &Path, state_dir: &Path) -> Self {
        Self {
            root: root.to_owned(),
            state_dir: state_dir.to_owned(),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The hash `trusted.json` holds for the file, read fresh, since you can trust it any time.
    pub(crate) fn trusted_hashes(&self) -> Vec<String> {
        TrustStore::load(&self.state_dir)
            .recorded(&project_models_path(&self.root))
            .into_iter()
            .collect()
    }

    /// The file read once, with whether you trusted that text. `None` when it can't be read.
    fn read(&self) -> Option<ProjectModels> {
        match ProjectModels::read(&project_models_path(&self.root), &self.trusted_hashes()) {
            Ok(project) => Some(project),
            Err(err) => {
                tracing::warn!("{err}");
                None
            }
        }
    }
}

/// The model registry, with the repo's `models.toml` once you trust it, starting from the
/// model list `cache` keeps when given. With a cache, as the app keeps one, Claude Code's
/// models also come from Anthropic once a day, and the model catalog from Argonauts'
/// repository; without one nothing is fetched.
pub(crate) fn registry(
    settings: &Settings,
    repo: Option<&RepoModels>,
    cache: Option<&Path>,
) -> Arc<dyn ModelRegistry> {
    let trusted = repo.map(RepoModels::trusted_hashes).unwrap_or_default();
    let claude = cache.map(|_| ClaudeSource::for_settings(settings));
    let catalog = cache.map(|_| CatalogSource::argonauts());
    registry_with_trust(
        settings,
        repo.map(RepoModels::root),
        &trusted,
        cache,
        claude,
        catalog,
    )
}

/// Every `[[harness]]` entry that counts now: the built-in profiles (one that's off by default
/// only when it's in `turned_on`), yours, then the repo's once you trust it. Entries left out
/// are logged with why.
pub(crate) fn manifests(
    settings: &Settings,
    home: &Paths,
    repo: Option<&RepoModels>,
    turned_on: &[String],
) -> Vec<HarnessManifest> {
    let project = repo.and_then(RepoModels::read);
    let repo_root = repo.map(RepoModels::root);
    let entries = argonauts_registry::harness_manifests(
        &home.global_models(),
        project.as_ref().map(ProjectModels::path),
        project.as_ref().is_some_and(ProjectModels::trusted),
        repo_root,
        turned_on,
    );
    for warning in &entries.warnings {
        tracing::warn!("{warning}");
    }
    entries
        .manifests
        .into_iter()
        .filter_map(|manifest| with_binary(settings, manifest, repo_root))
        .collect()
}

/// Your `agents.<id>.binary` replaces the entry's, then faces the checks an entry in your own
/// `models.toml` does, so it must be an absolute path outside the repo. Without one, an entry
/// that names its binary, as the built-in profiles do, runs the one found on `PATH` the way
/// doctor finds it, since sessions start only absolute paths.
fn with_binary(
    settings: &Settings,
    mut manifest: HarnessManifest,
    repo_root: Option<&Path>,
) -> Option<HarnessManifest> {
    let Some(binary) = settings
        .agents
        .get(&manifest.id)
        .and_then(|agent| agent.binary.as_ref())
    else {
        if !manifest.binary.contains('/')
            && let Ok(found) = find_unsandboxed(&manifest.binary, repo_root)
        {
            manifest.binary = found.display().to_string();
        }
        return Some(manifest);
    };
    manifest.binary = binary.display().to_string();
    let check = check_manifest(&manifest, repo_root);
    if !check.is_ok() {
        tracing::warn!(
            "agents.{}.binary: harness {} is left out: {}",
            manifest.id,
            manifest.id,
            check.errors.join("; ")
        );
        return None;
    }
    Some(manifest)
}

/// One adapter per entry, kept while the entry stays the same: an ACP adapter leaks its names
/// once, so building one per settings change would grow without end.
#[derive(Default)]
pub(crate) struct HarnessAdapters {
    made: Vec<(HarnessManifest, Arc<dyn Adapter>)>,
}

impl HarnessAdapters {
    /// `builtins` lend their parsers to entries with `output = "parser"`.
    pub(crate) fn adapters(
        &mut self,
        manifests: Vec<HarnessManifest>,
        builtins: &[Arc<dyn Adapter>],
        home: &Paths,
    ) -> Vec<Arc<dyn Adapter>> {
        manifests
            .into_iter()
            .map(|manifest| {
                if let Err(err) = install_home_template(home, &manifest) {
                    tracing::warn!("harness {}: {err:#}", manifest.id);
                }
                if let Some((_, adapter)) = self.made.iter().find(|(made, _)| *made == manifest) {
                    return Arc::clone(adapter);
                }
                let adapter = match manifest.transport {
                    Transport::Acp => acp_adapter(manifest.clone()),
                    Transport::Lines => manifest_adapter_with(manifest.clone(), builtins),
                };
                self.made.push((manifest, Arc::clone(&adapter)));
                adapter
            })
            .collect()
    }
}

/// Writes a built-in home template, such as Hermes', into `~/.argonauts/harnesses/<name>/`,
/// where the adapters copy a fresh home from for each session. A file already there is yours
/// and stays as it is.
pub(crate) fn install_home_template(
    home: &Paths,
    manifest: &HarnessManifest,
) -> anyhow::Result<()> {
    let Some(template) = &manifest.home else {
        return Ok(());
    };
    let Some(files) = builtin_home_template(&template.template) else {
        return Ok(());
    };
    let folder = home
        .home()
        .join(user_files::HARNESSES)
        .join(&template.template);
    for (path, contents) in files {
        let file = folder.join(path);
        if file.exists() {
            continue;
        }
        if let Some(dir) = file.parent() {
            create_private_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_private_file(&file, contents.as_bytes())
            .with_context(|| format!("writing {}", file.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use argonauts_core::RepoId;
    use argonauts_core::layout::project_files;
    use argonauts_core::settings::agents::AgentSettings;

    use super::*;

    const HERMES: &str = "hermes";

    fn hermes_on() -> Vec<String> {
        vec![HERMES.to_owned()]
    }

    fn ids(manifests: &[HarnessManifest]) -> Vec<&str> {
        manifests
            .iter()
            .map(|manifest| manifest.id.as_str())
            .collect()
    }

    fn with_agent_binary(id: &str, binary: &str) -> Settings {
        let mut settings = Settings::default();
        settings.agents.insert(
            id.to_owned(),
            AgentSettings {
                binary: Some(PathBuf::from(binary)),
                ..AgentSettings::default()
            },
        );
        settings
    }

    #[test]
    fn hermes_counts_only_once_a_layer_turns_it_on() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let off = manifests(&Settings::default(), &home, None, &[]);
        assert!(!ids(&off).contains(&HERMES), "{:?}", ids(&off));
        assert!(ids(&off).contains(&"opencode-acp"), "{:?}", ids(&off));

        let on = manifests(&Settings::default(), &home, None, &hermes_on());
        assert!(ids(&on).contains(&HERMES), "{:?}", ids(&on));
    }

    #[test]
    fn your_binary_replaces_the_entrys_and_must_be_absolute_and_outside_the_repo() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let repo_root = dir.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        let repo = RepoModels::new(&repo_root, &dir.path().join("state"));

        let settings = with_agent_binary(HERMES, "/opt/hermes/bin/hermes");
        let found = manifests(&settings, &home, Some(&repo), &hermes_on());
        let hermes = found.iter().find(|manifest| manifest.id == HERMES).unwrap();
        assert_eq!(hermes.binary, "/opt/hermes/bin/hermes");

        let relative = with_agent_binary(HERMES, "bin/hermes");
        let found = manifests(&relative, &home, Some(&repo), &hermes_on());
        assert!(!ids(&found).contains(&HERMES), "{:?}", ids(&found));

        let inside = repo_root.join("hermes");
        let inside = with_agent_binary(HERMES, &inside.display().to_string());
        let found = manifests(&inside, &home, Some(&repo), &hermes_on());
        assert!(!ids(&found).contains(&HERMES), "{:?}", ids(&found));
    }

    #[test]
    fn a_profile_naming_its_binary_runs_the_one_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let found = manifests(&Settings::default(), &home, None, &hermes_on());
        let mut hermes = found
            .into_iter()
            .find(|manifest| manifest.id == HERMES)
            .unwrap();
        // A name every machine has on `PATH`, in place of Hermes'.
        hermes.binary = "sh".to_owned();
        let named = with_binary(&Settings::default(), hermes, None).unwrap();
        assert!(Path::new(&named.binary).is_absolute(), "{}", named.binary);
        assert!(named.binary.ends_with("/sh"), "{}", named.binary);
    }

    #[test]
    fn the_repos_entries_wait_for_trust_in_that_exact_text() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join(".argonauts")).unwrap();
        let path = root.join(".argonauts").join(project_files::MODELS);
        let entry = "[[harness]]\nid = \"outside\"\ndisplay_name = \"Outside\"\nbinary = \"/opt/outside\"\nfamily = \"o\"\nread_only_args = []\nbuild_args = []\n";
        std::fs::write(&path, entry).unwrap();
        let state_dir = home.state_dir(&RepoId::for_repo(&root).unwrap());
        let repo = RepoModels::new(&root, &state_dir);

        assert!(
            !ids(&manifests(&Settings::default(), &home, Some(&repo), &[])).contains(&"outside")
        );
        std::fs::create_dir_all(&state_dir).unwrap();
        TrustStore::load(&state_dir).trust(&path).unwrap();
        assert!(
            ids(&manifests(&Settings::default(), &home, Some(&repo), &[])).contains(&"outside")
        );

        std::fs::write(&path, format!("{entry}# changed\n")).unwrap();
        assert!(
            !ids(&manifests(&Settings::default(), &home, Some(&repo), &[])).contains(&"outside"),
            "a change needs trust again"
        );
    }

    #[test]
    fn hermes_home_template_is_written_once_and_yours_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let found = manifests(&Settings::default(), &home, None, &hermes_on());
        let hermes = found.iter().find(|manifest| manifest.id == HERMES).unwrap();
        install_home_template(&home, hermes).unwrap();
        let config = home
            .home()
            .join(user_files::HARNESSES)
            .join(HERMES)
            .join("config.yaml");
        let written = std::fs::read_to_string(&config).unwrap();
        assert_eq!(
            builtin_home_template(HERMES)
                .unwrap()
                .iter()
                .find(|(path, _)| *path == "config.yaml")
                .map(|(_, contents)| *contents),
            Some(written.as_str())
        );

        std::fs::write(&config, "# mine\n").unwrap();
        install_home_template(&home, hermes).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "# mine\n");
    }

    #[test]
    fn an_unchanged_entry_keeps_its_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let home = Paths::new(dir.path().join("home"));
        let mut made = HarnessAdapters::default();
        let builtins = crate::adapters::builtin_adapters();
        let first = made.adapters(
            manifests(&Settings::default(), &home, None, &hermes_on()),
            &builtins,
            &home,
        );
        let again = made.adapters(
            manifests(&Settings::default(), &home, None, &hermes_on()),
            &builtins,
            &home,
        );
        assert_eq!(first.len(), again.len());
        assert!(first.iter().zip(&again).all(|(a, b)| Arc::ptr_eq(a, b)));

        let moved = with_agent_binary(HERMES, "/opt/other/hermes");
        let changed = made.adapters(
            manifests(&moved, &home, None, &hermes_on()),
            &builtins,
            &home,
        );
        let hermes = changed
            .iter()
            .find(|adapter| adapter.id() == HERMES)
            .unwrap();
        assert_eq!(hermes.binary(), "/opt/other/hermes");
    }
}
