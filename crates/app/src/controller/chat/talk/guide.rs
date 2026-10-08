//! `argonauts_guide`: one page of the guide at a time. A page is the README `/setup` writes
//! for the topic's folder, then the topic's settings as the catalog lists them, so a page is
//! as current as `/config` is.

use argonauts_core::prompt_files;
use argonauts_core::required;
use argonauts_core::settings::SettingMeta;
use argonauts_mcp::session::guide::TOPICS;
use argonauts_registry::settings_catalog;

use super::{Call, ChatState, fail};
use crate::controller::{Ctx, Effect};
use crate::prompts::catalog;

pub(crate) const NAME: &str = argonauts_mcp::session::guide::NAME;

pub(crate) const ABOUT: &str = "`argonauts_guide` opens one page of Argonauts' guide: \
    workflows, roles, prompts, goals, limits, fallbacks, mods, or an overview of the files. \
    Open the page a setup needs before you propose it, rather than guessing a file's fields.";

pub(crate) fn offered(_ctx: &Ctx, chat: &ChatState) -> bool {
    chat.place.is_none()
}

pub(crate) fn call(ctx: &mut Ctx, _chat: &mut ChatState, call: Call) -> Vec<Effect> {
    let topic = call
        .arguments
        .get("topic")
        .and_then(|topic| topic.as_str())
        .unwrap_or_default()
        .trim()
        .to_owned();
    match page(ctx, &topic) {
        Some(page) => call.answer(page),
        None => {
            let names: Vec<&str> = TOPICS.iter().map(|(name, _)| *name).collect();
            fail(
                call,
                format!(
                    "There's no page on {topic:?}. The pages are {}.",
                    names.join(", ")
                ),
            );
        }
    }
    Vec::new()
}

/// The page on `topic`, or `None` for a topic the guide doesn't have.
pub(super) fn page(ctx: &Ctx, topic: &str) -> Option<String> {
    let covers = TOPICS
        .iter()
        .find(|(name, _)| *name == topic)
        .map(|(_, covers)| *covers)?;
    let settings = settings_of(topic);
    let mut page = readme(ctx, topic).unwrap_or_else(|| format!("# {}\n\n{covers}.", title(topic)));
    // The workflows README is the runner's; the steps every workflow must contain are read here.
    if topic == "workflows" {
        page.push_str("\n\n");
        page.push_str(required::GUIDE.trim_end());
    }
    if !settings.is_empty() {
        page.push_str("\n\n## Settings\n\nSet with argonauts_setup, by dotted key.\n");
        for meta in settings {
            page.push('\n');
            page.push_str(&line(&meta));
        }
    }
    Some(page)
}

/// The README `/setup` writes for the topic's folder: the prompts page as it's written beside
/// the templates, and the others as `/setup` left them in this repository's `.argonauts/`, or
/// else in yours, so a page you edited is the one the session reads.
fn readme(ctx: &Ctx, topic: &str) -> Option<String> {
    let folder = match topic {
        "prompts" => return Some(prompt_files::readme(&catalog::all())),
        "overview" => "",
        "workflows" | "roles" | "mods" => topic,
        _ => return None,
    };
    // A page that came with the repository and waits for your trust isn't Argonauts' guide.
    let waiting = argonauts_crew::prompts::trust::waiting(&ctx.env.repo_root, &ctx.env.state_dir);
    let written = [
        ctx.env.repo_root.join(prompt_files::PROJECT_DIR),
        ctx.env.paths.home().to_path_buf(),
    ]
    .iter()
    .map(|dir| dir.join(folder).join(prompt_files::README_FILE))
    .filter(|path| !waiting.all().any(|file| file == *path))
    .find_map(|path| std::fs::read_to_string(path).ok())
    .filter(|text| !text.trim().is_empty());
    // Before `/setup` wrote it anywhere, the page is the one it would write.
    let page = written.or_else(|| {
        crate::setup::guide(if folder.is_empty() {
            "argonauts"
        } else {
            folder
        })
    })?;
    Some(without_written_by(page))
}

/// A page without the first line that says who wrote the file, which is for whoever edits it.
fn without_written_by(page: String) -> String {
    match page
        .strip_prefix("<!--")
        .and_then(|rest| rest.split_once("-->\n"))
    {
        Some((_, page)) => page.to_owned(),
        None => page,
    }
}

fn title(topic: &str) -> String {
    let mut chars = topic.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The settings a topic covers, by the keys they start with.
fn settings_of(topic: &str) -> Vec<SettingMeta> {
    let prefixes: &[&str] = match topic {
        "workflows" => &[
            "workflow",
            "workflow_size",
            "ultracode",
            "workflows",
            "phases",
        ],
        "roles" => &["main_agent", "crew", "roles"],
        "prompts" => &["prompts"],
        "goals" => &["goal"],
        "limits" => &["limits"],
        "fallbacks" => &["fallback"],
        "mods" => &["mods", "ui.mods", "ui.above_input"],
        _ => &[],
    };
    settings_catalog()
        .into_iter()
        .filter(|meta| {
            prefixes.iter().any(|prefix| {
                meta.key == *prefix
                    || meta
                        .key
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
        .collect()
}

/// `- limits.spend_per_day: what it does. One of: …`
fn line(meta: &SettingMeta) -> String {
    let mut line = format!("- `{}`", meta.key);
    if !meta.description.is_empty() {
        line.push_str(&format!(": {}.", meta.description));
    }
    if !meta.allowed.is_empty() {
        line.push_str(&format!(" One of {}.", meta.allowed.join(", ")));
    }
    if meta.safety {
        line.push_str(" A safety setting: the user confirms it.");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::test_support::{HarnessOptions, TestCtx};

    fn ctx() -> TestCtx {
        TestCtx::new(HarnessOptions::default()).0
    }

    #[test]
    fn a_call_answers_with_the_page_or_names_the_pages() {
        use argonauts_core::names::TurnId;
        use serde_json::json;
        use tokio::sync::oneshot;

        let mut test = ctx();
        let mut chat = ChatState::new(&test.ctx).expect("a conversation");
        let mut ask = |topic: &str| {
            let (reply, mut answered) = oneshot::channel();
            let arguments = json!({ "topic": topic });
            let turn = TurnId::numbered(1);
            call(
                &mut test.ctx,
                &mut chat,
                Call {
                    turn,
                    arguments,
                    reply,
                },
            );
            answered.try_recv().expect("an answer")
        };
        let goals = ask("goals");
        assert!(!goals.is_error);
        assert!(goals.text.starts_with("# Goals"), "{}", goals.text);
        let unknown = ask("everything");
        assert!(unknown.is_error);
        assert!(
            unknown
                .text
                .contains("The pages are overview, workflows, roles"),
            "{}",
            unknown.text
        );
    }

    #[test]
    fn every_topic_has_a_page() {
        let test = ctx();
        for (topic, _) in TOPICS {
            let page = page(&test.ctx, topic).unwrap_or_else(|| panic!("{topic}"));
            assert!(page.starts_with("# "), "{topic}: {page}");
        }
        assert_eq!(page(&test.ctx, "everything"), None);
    }

    /// Before `/setup` wrote a README anywhere, the page is the one it would write.
    #[test]
    fn a_page_setup_hasnt_written_yet_is_the_one_it_would_write() {
        let test = ctx();
        for (topic, readme) in [
            ("overview", "argonauts"),
            ("workflows", "workflows"),
            ("roles", "roles"),
            ("mods", "mods"),
        ] {
            let page = page(&test.ctx, topic).expect("a page");
            let written = without_written_by(crate::setup::guide(readme).expect("a README"));
            assert!(page.starts_with(written.trim_end()), "{topic}: {page}");
        }
        for topic in crate::setup::guide_topics() {
            assert!(crate::setup::guide(topic).is_some(), "{topic}");
        }
    }

    #[test]
    fn the_prompts_page_is_the_readme_setup_writes() {
        let test = ctx();
        let page = page(&test.ctx, "prompts").expect("a page");
        let readme = prompt_files::readme(&catalog::all());
        assert!(page.starts_with(&readme), "{page}");
        assert!(page.contains("## Settings"), "{page}");
        assert!(page.contains("`prompts.writer`"), "{page}");
    }

    #[test]
    fn a_readme_in_the_repository_is_the_page() {
        let test = ctx();
        let home = test.ctx.env.paths.home().join("workflows");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("README.md"), "# Workflows, yours\n").unwrap();
        let page = page(&test.ctx, "workflows").expect("a page");
        assert!(page.starts_with("# Workflows, yours"), "{page}");
        let project = test.repo.root().join(".argonauts/workflows");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("README.md"), "# Workflows here\n").unwrap();
        let page = super::page(&test.ctx, "workflows").expect("a page");
        assert!(
            page.starts_with("# Workflows here"),
            "the project's first: {page}"
        );
    }

    #[test]
    fn the_workflows_page_says_what_every_workflow_must_contain() {
        let test = ctx();
        let page = page(&test.ctx, "workflows").expect("a page");
        assert!(page.contains("workflows/always.toml"), "{page}");
        assert!(page.contains("do exactly these three tasks"), "{page}");
        let settings = page.find("## Settings").expect("settings");
        assert!(page.find("## Steps every workflow").expect("steps") < settings);
    }

    #[test]
    fn a_topic_lists_its_settings_from_the_catalog() {
        let test = ctx();
        let limits = page(&test.ctx, "limits").expect("a page");
        let keys: Vec<String> = settings_catalog()
            .into_iter()
            .map(|meta| meta.key)
            .filter(|key| key.starts_with("limits."))
            .collect();
        assert!(!keys.is_empty());
        for key in keys {
            assert!(limits.contains(&format!("`{key}`")), "{key}");
        }
        assert!(!limits.contains("`goal."), "only its own topic");
        let goals = page(&test.ctx, "goals").expect("a page");
        assert!(goals.contains("`goal."), "{goals}");
    }
}
