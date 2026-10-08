//! A run in a repository with no commits yet. Your branch has nothing to branch from and
//! nothing to move forward, so the run starts from a snapshot of your files, kept at a ref of
//! its own, and its work lands as changes to your files for you to commit. The app takes the
//! snapshot; the engine only needs to know where it's kept.

use crate::RunId;
use crate::settings::Settings;
use crate::settings::delivery::{DeliveryWrite, UnbornLanding};

/// The ref holding the snapshot `run` started from. Only a run that started before the
/// repository had a commit has one, so the ref also tells such a run apart.
pub fn base_ref(run: &RunId) -> String {
    format!("refs/argonauts/{run}/base")
}

/// Whether such a run is to make your branch's first commit, where by default it only writes
/// your files. `delivery.write = "files"` says never to commit for you, which holds here too.
pub fn makes_first_commit(settings: &Settings) -> bool {
    settings.merge.unborn == UnbornLanding::FirstCommit
        && settings.delivery.write == DeliveryWrite::Commit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snapshot_is_kept_out_of_your_branches_under_the_runs_name() {
        let run = RunId::new("run-07").unwrap();
        assert_eq!(base_ref(&run), "refs/argonauts/run-07/base");
    }

    #[test]
    fn a_first_commit_is_made_only_when_asked_and_never_under_files_only() {
        let mut settings = Settings::default();
        assert!(!makes_first_commit(&settings));
        settings.merge.unborn = UnbornLanding::FirstCommit;
        assert!(makes_first_commit(&settings));
        settings.delivery.write = DeliveryWrite::Files;
        assert!(!makes_first_commit(&settings));
    }
}
