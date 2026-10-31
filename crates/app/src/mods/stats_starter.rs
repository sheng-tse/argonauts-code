//! The stats mod's starter file, `~/.argonauts/mods/stats.toml`. Setup used to write it with
//! `period = "today"`, which kept Stats on today while every other tab opens on all. A file
//! still as setup wrote it is written again once with today's starter. One you edited is
//! yours and is never rewritten: if it still opens on today, one line says so, once.

use argonauts_core::NoteLevel;
use argonauts_core::base::hash::sha256_hex;
use argonauts_core::settings::mods::{MODS_DIR, builtin};
use argonauts_core::settings::stats::{StatsPeriod, StatsSettings};

use super::files;
use crate::startup::env::AppEnv;

/// The SHA-256 of each starter setup wrote that opened Stats on today.
const OPENED_ON_TODAY: [&str; 3] = [
    "9bed88874e94ef5065fff25f877eaeb4b7d3d0fe8757e85ecc545296e5f0d19f",
    "68d9cbf4d8ec437a0791e6b49456f267370de06a55b0b177687b46f35fe0a03f",
    "519a18d6d9bd67593062b75119aacb2e4531ad52dfc861d8c132091d94f654bd",
];

/// Beside the mods, the mark that the line about an edited file was said.
const TOLD: &str = ".stats-opens-on-today-told";

/// Brings the starter in `env`'s home up to date, or says once why Stats still opens on
/// today. `None` when there's nothing to say.
pub(crate) fn opens_on_all(env: &AppEnv) -> Option<(NoteLevel, String)> {
    let dir = env.paths.home().join(MODS_DIR);
    let path = files::mod_path(&dir, builtin::STATS);
    let text = std::fs::read_to_string(&path).ok()?;
    if OPENED_ON_TODAY.contains(&sha256_hex(text.as_bytes()).as_str()) {
        let starter = builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == builtin::STATS)
            .map(|(_, text)| *text)?;
        return files::write(&path, starter).err().map(|err| {
            let shown = env.shown(&path);
            (NoteLevel::Warn, format!("Couldn't update {shown}: {err:#}"))
        });
    }
    let told = dir.join(TOLD);
    let (settings, _) = StatsSettings::read(env.paths.home(), None);
    if settings.period != StatsPeriod::Today || told.exists() {
        return None;
    }
    // Said once: a mark that can't be written only means the line may come again.
    let _ = argonauts_core::base::paths::write_private_file(&told, b"");
    let shown = env.shown(&path);
    Some((
        NoteLevel::Info,
        format!(
            "Stats opens on today, as {shown} says. Every other tab opens on all; \
             period = \"all\" there opens Stats on all too."
        ),
    ))
}

#[cfg(test)]
mod tests {
    use argonauts_core::base::paths::Paths;
    use argonauts_testkit::TempRepo;

    use super::*;

    fn env(repo: &TempRepo) -> AppEnv {
        AppEnv::for_repo(Paths::new(repo.home()), repo.root(), true).unwrap()
    }

    fn starter() -> &'static str {
        builtin::STARTERS
            .iter()
            .find(|(name, _)| *name == builtin::STATS)
            .map(|(_, text)| *text)
            .unwrap()
    }

    /// A starter setup wrote before Stats opened on all.
    const OLD: &str = "\
# What the crew did, per CLI and model and all together. /stats opens it.
name    = \"stats\"
period  = \"today\"                 # today · 7d · 30d · all
group   = \"cli\"                   # cli · model · role
measure = \"tokens\"                # what the bars measure: tokens · tasks · spend
columns = [\"work\", \"review\", \"tokens\", \"plan\"]   # also: spend, pace, fallbacks
total   = true                    # the \"all together\" bar and line
";

    fn stats_file(repo: &TempRepo) -> std::path::PathBuf {
        repo.home().join("mods/stats.toml")
    }

    #[test]
    fn an_untouched_starter_is_written_again_once_and_opens_on_all() {
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        std::fs::create_dir_all(repo.home().join("mods")).unwrap();
        std::fs::write(stats_file(&repo), OLD).unwrap();
        assert_eq!(opens_on_all(&env), None);
        assert_eq!(
            std::fs::read_to_string(stats_file(&repo)).unwrap(),
            starter()
        );
        let (settings, _) = StatsSettings::read(repo.home(), None);
        assert_eq!(settings.period, StatsPeriod::All);
        // Again, nothing more to do.
        assert_eq!(opens_on_all(&env), None);
        assert_eq!(
            std::fs::read_to_string(stats_file(&repo)).unwrap(),
            starter()
        );
    }

    #[test]
    fn an_edited_file_on_today_is_left_alone_and_said_once() {
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        std::fs::create_dir_all(repo.home().join("mods")).unwrap();
        let edited = OLD.replace("\"tokens\"                #", "\"tasks\"                 #");
        std::fs::write(stats_file(&repo), &edited).unwrap();
        let (level, line) = opens_on_all(&env).expect("one line");
        assert_eq!(level, NoteLevel::Info);
        assert!(line.starts_with("Stats opens on today, as "), "{line}");
        assert!(line.contains("mods/stats.toml"), "{line}");
        assert_eq!(opens_on_all(&env), None, "said once");
        assert_eq!(std::fs::read_to_string(stats_file(&repo)).unwrap(), edited);
    }

    #[test]
    fn todays_starter_an_edited_file_on_all_and_no_file_say_nothing() {
        let repo = TempRepo::new().unwrap();
        let env = env(&repo);
        assert_eq!(opens_on_all(&env), None);
        std::fs::create_dir_all(repo.home().join("mods")).unwrap();
        std::fs::write(stats_file(&repo), starter()).unwrap();
        assert_eq!(opens_on_all(&env), None);
        let edited = starter().replace("\"cli\"", "\"model\"");
        std::fs::write(stats_file(&repo), &edited).unwrap();
        assert_eq!(opens_on_all(&env), None);
        assert_eq!(std::fs::read_to_string(stats_file(&repo)).unwrap(), edited);
    }
}
