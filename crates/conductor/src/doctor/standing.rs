//! How an installed CLI's version stands against the versions its adapter was recorded with.
//! Only two standings need an update: older than the oldest version supported, and a version
//! known not to work. Anything newer is judged by what the CLI offers, not by its number.

use argonauts_core::status_tab::VersionStanding;

use super::versions::numbers;

/// Where `version` stands among `tested`, the versions the adapter was recorded with, and
/// `broken`, the ones known not to work.
pub fn standing(version: Option<&str>, tested: &[&str], broken: &[&str]) -> VersionStanding {
    let Some(installed) = version.and_then(numbers) else {
        return VersionStanding::Unknown;
    };
    if broken.iter().any(|known| numbers(known) == Some(installed)) {
        return VersionStanding::Broken;
    }
    let tested: Vec<_> = tested
        .iter()
        .filter_map(|version| numbers(version))
        .collect();
    let (Some(oldest), Some(newest)) = (tested.iter().min(), tested.iter().max()) else {
        return VersionStanding::Unknown;
    };
    if installed < *oldest {
        VersionStanding::Older
    } else if installed <= *newest {
        VersionStanding::Checked
    } else if installed.0 == newest.0 {
        VersionStanding::Newer
    } else {
        VersionStanding::NewMajor
    }
}

/// The oldest of `tested`, as written there: the oldest version supported.
pub fn oldest_supported<'a>(tested: &[&'a str]) -> Option<&'a str> {
    tested
        .iter()
        .copied()
        .min_by_key(|version| numbers(version))
}

/// Whether `newest` is a later version than `installed`.
pub fn is_newer(newest: &str, installed: &str) -> bool {
    match (numbers(newest), numbers(installed)) {
        (Some(newest), Some(installed)) => newest > installed,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TESTED: &[&str] = &["0.160.0", "0.162.1"];

    #[test]
    fn a_version_stands_by_where_it_falls_among_the_checked_ones() {
        let stands = |version| standing(Some(version), TESTED, &[]);
        assert_eq!(stands("0.158.0"), VersionStanding::Older);
        assert_eq!(stands("0.160.0"), VersionStanding::Checked);
        assert_eq!(stands("0.161.4"), VersionStanding::Checked);
        assert_eq!(stands("0.162.2"), VersionStanding::Newer);
        // A later minor is still the same major: what it offers decides, not its number.
        assert_eq!(stands("0.170.0"), VersionStanding::Newer);
        assert_eq!(stands("1.0.0"), VersionStanding::NewMajor);
        assert_eq!(stands("no version here"), VersionStanding::Unknown);
        assert_eq!(standing(None, TESTED, &[]), VersionStanding::Unknown);
        assert_eq!(standing(Some("1.0.0"), &[], &[]), VersionStanding::Unknown);
    }

    #[test]
    fn a_version_known_not_to_work_is_broken_wherever_it_falls() {
        assert_eq!(
            standing(Some("0.161.0"), TESTED, &["0.161.0"]),
            VersionStanding::Broken
        );
        assert!(VersionStanding::Broken.needs_update());
        assert!(VersionStanding::Older.needs_update());
        assert!(!VersionStanding::Newer.needs_update());
        assert!(!VersionStanding::NewMajor.needs_update());
    }

    #[test]
    fn the_oldest_supported_and_a_newer_one_compare_by_number() {
        assert_eq!(oldest_supported(&["1.2.13", "1.2.9"]), Some("1.2.9"));
        assert!(is_newer("2.1.297", "2.1.296"));
        assert!(is_newer("2.1.300", "2.1.99"));
        assert!(!is_newer("2.1.296", "2.1.296"));
        assert!(!is_newer("latest", "2.1.296"));
    }
}
