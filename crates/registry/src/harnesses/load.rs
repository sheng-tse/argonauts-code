//! Reading `[[harness]]` entries from the built-ins and the two `models.toml` files.

use std::io::ErrorKind;
use std::path::Path;

use argonauts_core::registry::HarnessManifest;

use super::HarnessEntries;
use super::builtin::builtin_files;
use super::turned_on::builtin_counts;
use super::validate::{ManifestCheck, check_builtin_manifest, check_manifest};

/// Later wins by `id`. The project's entries count only when `project_trusted`, since its hash
/// is in `trusted.json`; until then they're left out with a warning that says how to trust it.
/// A built-in profile that's off by default counts only when its id is in `turned_on`, which
/// `agents_turned_on` gives.
pub fn harness_manifests(
    global_models: &Path,
    project_models: Option<&Path>,
    project_trusted: bool,
    repo_root: Option<&Path>,
    turned_on: &[String],
) -> HarnessEntries {
    let mut entries = HarnessEntries::default();
    for (name, parsed) in builtin_files() {
        let source = format!("built-in {name}");
        match parsed {
            Ok(manifests) => {
                for manifest in manifests {
                    if !builtin_counts(&manifest.id, turned_on) {
                        continue;
                    }
                    let check = check_builtin_manifest(&manifest);
                    entries.add(&source, manifest, check);
                }
            }
            Err(err) => entries
                .warnings
                .push(format!("{source} doesn't parse: {err}")),
        }
    }
    entries.read_file(global_models, repo_root);
    if let Some(project) = project_models {
        if project_trusted {
            entries.read_file(project, repo_root);
        } else {
            entries.note_untrusted(project);
        }
    }
    entries
}

/// The `id` of each `[[harness]]` table in a `models.toml`, for resolving, which only needs to
/// know the names. A missing or broken file declares none; `harness_manifests` reports it.
pub(crate) fn declared_harness_ids(path: &Path) -> Vec<String> {
    HarnessEntries::default()
        .harness_items(path)
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item.get("id")?.as_str())
        .map(str::to_owned)
        .collect()
}

impl HarnessEntries {
    fn add(&mut self, source: &str, manifest: HarnessManifest, check: ManifestCheck) {
        let id = &manifest.id;
        if !check.is_ok() {
            self.warnings.push(format!(
                "{source}: CLI {id} is left out: {}",
                check.errors.join("; ")
            ));
            return;
        }
        self.warnings.extend(
            check
                .warnings
                .iter()
                .map(|warning| format!("{source}: CLI {id}: {warning}")),
        );
        match self.manifests.iter_mut().find(|kept| kept.id == *id) {
            Some(earlier) => *earlier = manifest,
            None => self.manifests.push(manifest),
        }
    }

    fn read_file(&mut self, path: &Path, repo_root: Option<&Path>) {
        let source = path.display().to_string();
        let Some(items) = self.harness_items(path) else {
            return;
        };
        for (index, item) in items.into_iter().enumerate() {
            let id = item
                .get("id")
                .and_then(toml::Value::as_str)
                .map_or_else(|| format!("#{}", index + 1), str::to_owned);
            match item.try_into::<HarnessManifest>() {
                Ok(manifest) => {
                    let check = check_manifest(&manifest, repo_root);
                    self.add(&source, manifest, check);
                }
                Err(err) => self
                    .warnings
                    .push(format!("{source}: CLI {id} is left out: {}", err.message())),
            }
        }
    }

    fn note_untrusted(&mut self, path: &Path) {
        let Some(items) = self.harness_items(path) else {
            return;
        };
        let count = match items.len() {
            0 => return,
            1 => "its CLI entry is".to_owned(),
            n => format!("its {n} CLI entries are"),
        };
        self.warnings.push(format!(
            "{}: {count} ignored until you trust the file (/setup, or the trust question when \
             it changes)",
            path.display()
        ));
    }

    /// The file's `[[harness]]` tables. A missing file has none; one that can't be read or
    /// parsed has none and a warning.
    fn harness_items(&mut self, path: &Path) -> Option<Vec<toml::Value>> {
        let source = path.display();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return None,
            Err(err) => {
                self.warnings.push(format!("{source} can't be read: {err}"));
                return None;
            }
        };
        let mut table = match toml::from_str::<toml::Table>(&text) {
            Ok(table) => table,
            Err(err) => {
                self.warnings.push(format!(
                    "{source} doesn't parse, so its CLI entries are left out: {}",
                    err.message()
                ));
                return None;
            }
        };
        match table.remove("harness") {
            None => Some(Vec::new()),
            Some(toml::Value::Array(items)) => Some(items),
            Some(_) => {
                self.warnings.push(format!(
                    "{source}: CLI must be a list of [[harness]] tables"
                ));
                None
            }
        }
    }
}
