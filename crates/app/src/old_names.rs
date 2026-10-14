//! Your own settings files, rewritten once in today's setting names when they still use older
//! ones. The shared project file is left as it is, since it belongs to everyone who clones the
//! repo; its old names still read, and the start notices say so.

use argonauts_core::NoteLevel;
use argonauts_registry::rename_old_names;

use crate::controller::Ctx;

/// A notice for each file rewritten. The settings are read again afterwards, so the warning
/// that a file uses older names goes with the old names.
pub(crate) fn rename_in_your_files(ctx: &mut Ctx) -> Vec<(NoteLevel, String)> {
    let env = &ctx.env;
    let mut notices = Vec::new();
    for file in [env.global_config(), env.project_local_config()] {
        match rename_old_names(&file) {
            Ok(Some(renamed)) => notices.push((
                NoteLevel::Info,
                format!(
                    "{} used older setting names, so it now uses today's: {}. The file as it \
                     was is kept as {}.",
                    env.display(&file),
                    renamed.listed(),
                    env.display(&renamed.backup)
                ),
            )),
            Ok(None) => {}
            Err(err) => notices.push((
                NoteLevel::Warn,
                format!(
                    "Couldn't rewrite {} in today's setting names ({err}); its older names \
                     still read.",
                    env.display(&file)
                ),
            )),
        }
    }
    let rewrote = notices.iter().any(|(level, _)| *level == NoteLevel::Info);
    if rewrote && let Err(err) = ctx.settings.reload(&ctx.env) {
        notices.push((NoteLevel::Warn, format!("{err:#}")));
    }
    notices
}

#[cfg(test)]
mod tests {
    use argonauts_core::names::RoleName;
    use argonauts_testkit::TempRepo;

    use crate::controller::test_support::{HarnessOptions, TestCtx};
    use crate::startup::start_notices;

    /// The shape of a settings file from before the names changed, which made the app refuse
    /// to start: a role named `coordinator`, a `preset`, and a role it doesn't know at all.
    const BEFORE: &str = "crew = [\"claude-code\", \"codex\"]\n\
        # Who you chat with, and who works solo: a harness, or harness/model\n\
        default_agent = \"claude-code\"\n\
        preset = \"solo\"\n\n\
        [approvals]\nfull_auto = false\n\n\
        [roles.coordinator]\nworkers = [\"claude-code\"]\n\n\
        [roles.foreman]\nworkers = [\"codex\"]\n";

    #[test]
    fn an_old_settings_file_starts_the_app_and_is_rewritten_once() {
        let (mut parts, _) = TestCtx::new(HarnessOptions {
            prepare: Some(Box::new(|repo: &TempRepo| {
                std::fs::write(repo.global_config(), BEFORE).unwrap();
            })),
            ..HarnessOptions::default()
        });
        let settings = parts.ctx.settings.settings();
        assert_eq!(settings.workflow, "solo");
        assert!(settings.roles.contains_key(&RoleName::Orchestrator));

        let notices = start_notices(&mut parts.ctx);
        let texts: Vec<&str> = notices.iter().map(|(_, text)| text.as_str()).collect();
        let rewritten: Vec<&&str> = texts
            .iter()
            .filter(|text| text.contains("used older setting names"))
            .collect();
        assert_eq!(rewritten.len(), 1, "{texts:#?}");
        assert!(rewritten[0].contains("preset → workflow"), "{texts:#?}");
        assert!(rewritten[0].contains("config.toml.bak"), "{texts:#?}");
        // The notice replaces the warning that the file still uses older names.
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("uses older setting names")),
            "{texts:#?}"
        );
        // The role it doesn't know is named, and the rest of the file applies.
        assert!(
            texts
                .iter()
                .any(|text| text.contains("\"foreman\"") && text.contains("default is used")),
            "{texts:#?}"
        );

        let global = parts.repo.global_config();
        let now = std::fs::read_to_string(&global).unwrap();
        assert!(now.contains("[roles.orchestrator]"), "{now}");
        assert!(now.contains("main_agent = \"claude-code\""), "{now}");
        let backup = global.with_file_name("config.toml.bak");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), BEFORE);

        let again = start_notices(&mut parts.ctx);
        assert!(
            !again
                .iter()
                .any(|(_, text)| text.contains("older setting names")),
            "{again:#?}"
        );
    }
}
