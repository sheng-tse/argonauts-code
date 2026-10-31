//! Spot-check sampling from a hash of the run id, the approval id, and a per-install salt.
//! Approval ids like `approval-003` repeat across runs, so the id alone would always or never
//! sample a Captain merge.

use std::fs;
use std::io;
use std::path::Path;

use argonauts_core::RunId;
use argonauts_core::base::names::ApprovalId;
use argonauts_core::base::paths::write_private_file;
use argonauts_core::sha256_hex;

/// In the install's state folder, `~/.argonauts/state`, beside every repo's.
pub const SALT_FILE: &str = "spot-check-salt";

/// Whether this approval joins your spot-check queue, for a sample share from 0 to 1.
pub fn sampled(salt: &[u8; 32], run: &RunId, approval: &ApprovalId, sample: f64) -> bool {
    if sample.is_nan() {
        // A share that isn't a number samples everything, the safe side.
        return true;
    }
    let mut input = Vec::with_capacity(salt.len() + run.as_str().len() + 1 + 16);
    input.extend_from_slice(salt);
    input.extend_from_slice(run.as_str().as_bytes());
    // Ids never hold a NUL, so no two (run, approval) pairs hash the same bytes.
    input.push(0);
    input.extend_from_slice(approval.as_str().as_bytes());
    let digest = sha256_hex(&input);
    let Some(first_eight_bytes) = digest
        .get(..16)
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
    else {
        return true;
    };
    let fraction = first_eight_bytes as f64 / 2f64.powi(64);
    fraction < sample
}

/// The install's salt, created with 32 random bytes the first time.
pub fn load_or_create_salt(install_state_dir: &Path) -> io::Result<[u8; 32]> {
    let path = install_state_dir.join(SALT_FILE);
    match read_salt(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        found => return found,
    }
    let mut salt = [0u8; 32];
    rand::fill(&mut salt[..]);
    // Linked into place from a unique name, so run hosts starting at once agree on one salt.
    let unique = format!(
        "{SALT_FILE}.{}.{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    );
    let temp = install_state_dir.join(unique);
    write_private_file(&temp, &salt)?;
    let linked = fs::hard_link(&temp, &path);
    let removed = fs::remove_file(&temp);
    match linked {
        Ok(()) => removed.map(|()| salt),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => read_salt(&path),
        Err(err) => Err(err),
    }
}

fn read_salt(path: &Path) -> io::Result<[u8; 32]> {
    let bytes = fs::read(path)?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} should hold 32 bytes, not {}",
                path.display(),
                bytes.len()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn run(n: u32) -> RunId {
        RunId::new(format!("run-{n}")).expect("run id")
    }

    #[test]
    fn spot_check_sampling() {
        let salt = [7u8; 32];
        let captain_merge = ApprovalId::numbered(3);

        // A fixed salt is stable.
        for n in 0..20 {
            let first = sampled(&salt, &run(n), &captain_merge, 0.5);
            assert_eq!(first, sampled(&salt, &run(n), &captain_merge, 0.5));
        }

        // The same approval id in two runs samples independently.
        let draws: Vec<bool> = (0..200)
            .map(|n| sampled(&salt, &run(n), &captain_merge, 0.5))
            .collect();
        assert!(draws.contains(&true) && draws.contains(&false));
        let other_salt = [9u8; 32];
        let moved = (0..200)
            .filter(|&n| {
                sampled(&salt, &run(n), &captain_merge, 0.5)
                    != sampled(&other_salt, &run(n), &captain_merge, 0.5)
            })
            .count();
        assert!(
            moved > 50,
            "another salt should draw other runs, moved {moved}"
        );

        // The share comes near the sample over many draws.
        for sample in [0.1, 0.5] {
            let hits = (0..10_000)
                .filter(|&n| sampled(&salt, &run(n), &captain_merge, sample))
                .count();
            let share = hits as f64 / 10_000.0;
            assert!((share - sample).abs() < 0.02, "share {share} for {sample}");
        }

        assert!(!sampled(&salt, &run(1), &captain_merge, 0.0));
        assert!(sampled(&salt, &run(1), &captain_merge, 1.0));
        assert!(sampled(&salt, &run(1), &captain_merge, f64::NAN));
    }

    #[test]
    fn the_salt_is_created_once_and_private() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = load_or_create_salt(dir.path()).expect("create");
        assert_eq!(load_or_create_salt(dir.path()).expect("load"), first);
        let path = dir.path().join(SALT_FILE);
        let mode = fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let left: Vec<_> = fs::read_dir(dir.path()).expect("list").collect();
        assert_eq!(left.len(), 1, "the temporary file is removed");

        fs::write(&path, b"short").expect("write");
        assert!(load_or_create_salt(dir.path()).is_err());
    }
}
