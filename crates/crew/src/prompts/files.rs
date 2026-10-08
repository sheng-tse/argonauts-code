//! Your own prompt files for a run: where they are looked for, and the text a built-in prompt
//! has once your template is taken into account.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock, PoisonError};

use argonauts_core::paths::Paths;
use argonauts_core::prompt_files::PromptDirs;

use crate::engine::host::Host;

use super::{catalog, trust};

/// Your checkout's `.argonauts`, then `~/.argonauts`. Never a task's worktree: an agent can't
/// change what another agent is told. A project file that came with the repository is passed
/// over until you trust it ([`trust`]).
pub fn dirs(host: &Host) -> PromptDirs {
    let home = Paths::from_env().ok();
    let home = home.as_ref().map(|paths| paths.home());
    let repo = host.repo();
    // `~/.argonauts/state/<repo>/runs/<run>`: the repository's state folder is two up.
    match repo.run_dir.parent().and_then(Path::parent) {
        Some(state_dir) => trust::dirs(&repo.root, state_dir, home),
        None => PromptDirs::new(Some(&repo.root), home),
    }
}

/// The project's files that came with the repository and wait for your trust: workflows,
/// their `always.toml` and guide pages as well as roles and prompts. The workflow folders pass
/// them over.
pub fn waiting(host: &Host) -> Vec<std::path::PathBuf> {
    let repo = host.repo();
    match repo.run_dir.parent().and_then(Path::parent) {
        Some(state_dir) => trust::waiting(&repo.root, state_dir).all().collect(),
        None => Vec::new(),
    }
}

/// A built-in prompt as this run sends it: your template for it when you wrote one, else the
/// built-in itself. For the prompts that aren't the head of an agent's instructions, such as
/// the report a worker ends with and the judge's rubric.
pub fn text(host: &Host, built_in: &'static str) -> &'static str {
    catalog::of(built_in)
        .and_then(|template| dirs(host).template(template.id))
        .map_or(built_in, |yours| intern(yours.text))
}

/// Prompts travel as `'static` text, so a file's text is kept for the life of the process,
/// once per distinct text: a handful of templates, never one per session.
pub(crate) fn intern(text: String) -> &'static str {
    static TEXTS: OnceLock<Mutex<BTreeMap<String, &'static str>>> = OnceLock::new();
    let mut texts = TEXTS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(kept) = texts.get(&text) {
        return kept;
    }
    let kept: &'static str = Box::leak(text.clone().into_boxed_str());
    texts.insert(text, kept);
    kept
}
