//! A file an agent meant to create that the repository's ignore rules match. A commit never
//! force-adds it, since that's how a `.env` reaches history, so it's told instead, with the
//! rule that matched, while the file is still in the task's worktree.

use std::collections::BTreeSet;
use std::path::Path;

use super::{Claims, Git, GitError, LeftOut, LeftOutWhy, PATHS_PER_CALL, failed};

impl Git {
    /// The files `claims` names in `worktree` that your ignore rules match, each named with
    /// the rule, such as `lib/util.py (.gitignore:3:lib/)`. A folder claimed whole names
    /// nothing here: what a command left in it is no claim.
    pub async fn claimed_but_ignored(
        &self,
        worktree: &Path,
        claims: &Claims,
    ) -> Result<Vec<LeftOut>, GitError> {
        let present: BTreeSet<&str> = claims
            .files
            .iter()
            .map(String::as_str)
            .filter(|path| worktree.join(path).is_file())
            .collect();
        let present: Vec<&str> = present.into_iter().collect();
        let mut ignored = Vec::new();
        for chunk in present.chunks(PATHS_PER_CALL) {
            // Tracked files aren't subject to ignore rules, so they never show here.
            let mut args = vec!["-c", "core.quotePath=false", "check-ignore", "--verbose"];
            args.push("--");
            args.extend(chunk);
            let out = self.output(worktree, &args, &[]).await?;
            // 1 says none of them is ignored.
            if !matches!(out.status.code(), Some(0 | 1)) {
                return Err(failed(&args, &out));
            }
            // `source:line:pattern`, a tab, then the path. A name git had to quote matches
            // none of the claims, and is skipped.
            let listed = String::from_utf8_lossy(&out.stdout);
            for line in listed.lines() {
                let Some((rule, path)) = line.rsplit_once('\t') else {
                    continue;
                };
                if chunk.contains(&path) {
                    ignored.push(LeftOut {
                        path: path.to_owned(),
                        because: format!("{path} ({rule})"),
                        why: LeftOutWhy::Ignored,
                    });
                }
            }
        }
        Ok(ignored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{TestRepo, git_test_env};

    #[tokio::test]
    async fn a_claimed_file_your_ignore_rules_match_is_told_with_the_rule() {
        let repo = TestRepo::new();
        let git = Git::with_env(git_test_env());
        repo.write(".gitignore", "lib/\n.env\n");
        repo.git(&["add", ".gitignore"]);
        repo.git(&["commit", "-qm", "ignore rules"]);
        repo.write("lib/util.py", "def util(): ...\n");
        repo.write(".env", "TOKEN=1\n");
        repo.write("lib/cache.bin", "made by a command");
        repo.write("src/app.py", "app\n");
        let mut claims = Claims::default();
        for path in ["lib/util.py", ".env", "src/app.py", "lib/missing.py"] {
            claims.add(&repo.root, path);
        }

        let mut told = git.claimed_but_ignored(&repo.root, &claims).await.unwrap();
        told.sort_by(|a, b| a.path.cmp(&b.path));
        let told: Vec<&str> = told.iter().map(|left| left.because.as_str()).collect();
        assert_eq!(
            told,
            [
                ".env (.gitignore:2:.env)",
                "lib/util.py (.gitignore:1:lib/)"
            ]
        );

        // The commit takes neither, and says why.
        let committed = git
            .commit_claimed(&repo.root, "work", Some(&claims))
            .await
            .unwrap();
        let shown = repo.git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(shown, "src/app.py");
        assert!(
            committed
                .left_out
                .iter()
                .any(|left| left.why == LeftOutWhy::Ignored && left.path == ".env")
        );
    }
}
