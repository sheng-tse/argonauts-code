//! OpenCode's permissions for work outside the project, sent in its inline config. OpenCode
//! 2.0.32 takes `permission` as a map from an action to patterns, each `allow`, `ask` or
//! `deny`; the last pattern that matches wins, as its own defaults (`*` allowed, then `*.env`
//! asked) show, and `*` there matches across folders. `external_directory` is what any tool
//! needs to reach a folder outside the project, so the never-list is denied there for every
//! tool, and in `read` and `edit` for the project's own files. Inside our sandbox the never-list
//! holds in the OS too.
//!
//! OpenCode 2.0.32 names a file in the session's folder by its path from there and any other
//! by its absolute path, so `/*` in `edit` is every edit outside. A session that may not write
//! outside gets that denied: `opencode run` can't ask, and an `ask` there is refused anyway, so
//! a direct turn that reads outside writes there only on the turn you allow by answering, and
//! a run's session, which can't ask, never does. The never-list is denied in `read` and `edit`
//! whatever else is allowed.

use std::path::Path;

use argonauts_core::CommandSpec;
use argonauts_core::outside::Never;
use argonauts_core::session::{SessionRequest, SessionRole};
use serde_json::{Map, Value, json};

use crate::provider::CONFIG_CONTENT_ENV;

/// Puts the `permission` block in the inline config, beside the provider config already there.
pub(crate) fn apply(cmd: &mut CommandSpec, req: &SessionRequest) {
    let Some(permission) = permission(req) else {
        return;
    };
    let at = cmd
        .env_set
        .iter()
        .position(|(key, _)| key == CONFIG_CONTENT_ENV);
    let mut config = at
        .and_then(|at| serde_json::from_str::<Value>(&cmd.env_set[at].1).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    config["permission"] = permission;
    match at {
        Some(at) => cmd.env_set[at].1 = config.to_string(),
        None => cmd
            .env_set
            .push((CONFIG_CONTENT_ENV.to_owned(), config.to_string())),
    }
}

/// The `permission` block for `req`, or `None` where its mode keeps it to the project.
pub(crate) fn permission(req: &SessionRequest) -> Option<Value> {
    let outside = &req.outside;
    if !outside.is_open() {
        return None;
    }
    let reads = outside.reads_unasked(req.role);
    let writes = outside.writes_unasked(req.role);
    let names = name_rules(&outside.never);
    let mut closed = names.clone();
    // Paths last, so a template taken back out of a name is still closed in a folder on the
    // list.
    closed.extend(
        outside
            .never
            .paths
            .iter()
            .flat_map(|path| path_patterns(path))
            .map(|pattern| (pattern, DENY)),
    );
    let mut unwritten: Rules = outside
        .never
        .unwritten
        .iter()
        .flat_map(|path| path_patterns(path))
        .map(|pattern| (pattern, DENY))
        .collect();
    unwritten.extend(
        outside
            .project
            .iter()
            .filter(|project| writes && !req.workdir.starts_with(project))
            .flat_map(|project| path_patterns(project))
            .map(|pattern| (pattern, DENY)),
    );
    let mut permission = Map::new();
    let reaches = if reads || writes {
        allowed_but(&closed)
    } else {
        only(&closed)
    };
    permission.insert("external_directory".to_owned(), reaches);
    let read = if reads {
        allowed_but(&closed)
    } else {
        only(&closed)
    };
    permission.insert("read".to_owned(), read);
    if req.role != SessionRole::ReadOnly {
        let edit = if writes {
            let both: Rules = closed.iter().chain(&unwritten).cloned().collect();
            allowed_but(&both)
        } else {
            let mut kept = names;
            kept.push((EVERY_PATH_OUTSIDE.to_owned(), DENY));
            only(&kept)
        };
        permission.insert("edit".to_owned(), edit);
    }
    Some(Value::Object(permission))
}

/// Patterns with what each says, in the order OpenCode reads them.
type Rules = Vec<(String, &'static str)>;

const DENY: &str = "deny";
const ALLOW: &str = "allow";

/// Every file outside the session's folder, as OpenCode names one in a rule.
const EVERY_PATH_OUTSIDE: &str = "/*";

/// The never-list's names in the order the last match needs: names with `*` denied first, then
/// the templates taken back out of them allowed, then plain names denied, which a template
/// never opens.
fn name_rules(never: &Never) -> Rules {
    let in_any_folder = |name: &str| [name.to_owned(), format!("*/{name}")];
    let (globs, plain): (Vec<&String>, Vec<&String>) =
        never.names.iter().partition(|name| name.contains('*'));
    let mut rules = Rules::new();
    for glob in &globs {
        rules.extend(in_any_folder(glob).map(|pattern| (pattern, DENY)));
    }
    let mut taken_back: Vec<&str> = Vec::new();
    for name in globs.iter().flat_map(|glob| never.taken_back_from(glob)) {
        if !taken_back.contains(&name) {
            taken_back.push(name);
            rules.extend(in_any_folder(name).map(|pattern| (pattern, ALLOW)));
        }
    }
    for name in plain {
        rules.extend(in_any_folder(name).map(|pattern| (pattern, DENY)));
    }
    rules
}

/// `*` allowed first, then each rule, since the last match wins.
fn allowed_but(rules: &Rules) -> Value {
    let mut block = Map::new();
    block.insert("*".to_owned(), json!(ALLOW));
    block.extend(each(rules));
    Value::Object(block)
}

/// Each rule, after whatever OpenCode and your own config say.
fn only(rules: &Rules) -> Value {
    Value::Object(each(rules).collect())
}

fn each(rules: &Rules) -> impl Iterator<Item = (String, Value)> + '_ {
    rules
        .iter()
        .map(|(pattern, effect)| (pattern.clone(), json!(effect)))
}

fn path_patterns(path: &Path) -> Vec<String> {
    let Some(text) = path.to_str().filter(|text| text.starts_with('/')) else {
        return Vec::new();
    };
    let text = text.trim_end_matches('/');
    vec![text.to_owned(), format!("{text}/*")]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use argonauts_core::outside::{Never, Outside};
    use argonauts_core::session::SessionRole;

    use super::*;

    fn on_autopilot(role: SessionRole, workdir: &str) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from(workdir),
            role,
            outside: Outside {
                read: true,
                write: true,
                after_asking: false,
                project: Some(PathBuf::from("/home/you/app")),
                never: Never {
                    paths: vec![PathBuf::from("/home/you/.ssh")],
                    names: vec![".env".to_owned()],
                    except: Vec::new(),
                    unwritten: vec![PathBuf::from("/etc")],
                },
            },
            ..SessionRequest::default()
        }
    }

    #[test]
    fn a_mode_kept_to_the_project_sends_no_permission() {
        assert_eq!(permission(&SessionRequest::default()), None);
    }

    #[test]
    fn the_permission_joins_the_provider_config_already_sent() {
        let req = on_autopilot(SessionRole::Direct, "/home/you/app");
        let mut cmd = CommandSpec::default();
        cmd.env_set.push((
            CONFIG_CONTENT_ENV.to_owned(),
            json!({"provider": {"ollama": {}}}).to_string(),
        ));
        apply(&mut cmd, &req);
        assert_eq!(cmd.env_set.len(), 1);
        let config: Value = serde_json::from_str(&cmd.env_set[0].1).unwrap();
        assert!(config["provider"]["ollama"].is_object());
        assert_eq!(config["permission"]["read"]["*"], json!("allow"));
        let mut alone = CommandSpec::default();
        apply(&mut alone, &req);
        assert_eq!(alone.env_set[0].0, CONFIG_CONTENT_ENV);
    }

    #[test]
    fn autopilot_allows_anywhere_and_denies_the_never_list_last() {
        let permission = permission(&on_autopilot(SessionRole::Build, "/home/you/app")).unwrap();
        let external = permission["external_directory"].as_object().unwrap();
        let keys: Vec<&str> = external.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["*", ".env", "*/.env", "/home/you/.ssh", "/home/you/.ssh/*"]
        );
        assert_eq!(external["*"], json!("allow"));
        assert_eq!(external["/home/you/.ssh/*"], json!("deny"));
        assert_eq!(permission["read"]["*/.env"], json!("deny"));
        assert_eq!(permission["edit"]["/etc/*"], json!("deny"));
        assert!(
            permission["read"].get("/etc/*").is_none(),
            "system folders are read"
        );
        assert!(permission["edit"].get("/home/you/app/*").is_none());
    }

    fn in_supervised(role: SessionRole, workdir: &str) -> SessionRequest {
        let mut req = on_autopilot(role, workdir);
        req.outside.read = false;
        req.outside.write = false;
        req.outside.after_asking = true;
        req
    }

    /// OpenCode's own defaults in 2.0.32 (`*` allowed, then `*.env` asked), with the block we
    /// send after them, as OpenCode reads it: the last pattern that matches wins, and `*`
    /// matches across folders.
    fn decides(permission: &Value, action: &str, resource: &str) -> &'static str {
        let mut rules: Vec<(String, String)> = vec![("*".to_owned(), "allow".to_owned())];
        if action == "read" {
            rules.push(("*.env".to_owned(), "ask".to_owned()));
        }
        if let Some(sent) = permission[action].as_object() {
            rules.extend(
                sent.iter().map(|(pattern, effect)| {
                    (pattern.clone(), effect.as_str().unwrap().to_owned())
                }),
            );
        }
        let matches = |pattern: &str| argonauts_core::outside::name_matches(pattern, resource);
        match rules.iter().rev().find(|(pattern, _)| matches(pattern)) {
            Some((_, effect)) if effect == "allow" => "allow",
            Some((_, effect)) if effect == "deny" => "deny",
            _ => "ask",
        }
    }

    /// A turn that asks but whose CLI can't, `opencode run` refusing every `ask`, edits
    /// nothing outside, and the never-list is denied in every rule.
    #[test]
    fn a_supervised_turn_edits_nothing_outside_and_denies_the_never_list_everywhere() {
        let permission = permission(&in_supervised(SessionRole::Direct, "/home/you/app")).unwrap();
        let external = permission["external_directory"].as_object().unwrap();
        assert!(external.get("*").is_none(), "{external:?}");
        assert!(external.values().all(|rule| *rule == json!("deny")));
        assert_eq!(external["/home/you/.ssh/*"], json!("deny"));
        assert_eq!(
            decides(&permission, "edit", "/home/you/Downloads/notes.md"),
            "deny"
        );
        assert_eq!(decides(&permission, "edit", "src/main.rs"), "allow");
        assert_eq!(decides(&permission, "edit", ".env"), "deny");
        assert_eq!(decides(&permission, "read", "config/.env"), "deny");
        assert_eq!(decides(&permission, "read", "src/main.rs"), "allow");
    }

    /// The turn reads outside without asking, as Codex's do, and writes there only once you
    /// allow it by answering, on a turn that writes anywhere but the never-list.
    #[test]
    fn a_turn_that_reads_outside_edits_only_its_project_until_you_allow_it() {
        let mut reads = in_supervised(SessionRole::Direct, "/home/you/app");
        reads.outside = reads.outside.clone().without_asking();
        let permission = permission(&reads).unwrap();
        assert_eq!(
            decides(&permission, "external_directory", "/home/you/Downloads/*"),
            "allow"
        );
        assert_eq!(
            decides(&permission, "external_directory", "/home/you/.ssh/*"),
            "deny"
        );
        assert_eq!(
            decides(&permission, "read", "/home/you/Downloads/brief.txt"),
            "allow"
        );
        assert_eq!(
            decides(&permission, "read", "/home/you/.ssh/id_rsa"),
            "deny"
        );
        assert_eq!(decides(&permission, "read", ".env"), "deny");
        assert_eq!(
            decides(&permission, "edit", "/home/you/Downloads/notes.md"),
            "deny"
        );
        assert_eq!(decides(&permission, "edit", "src/main.rs"), "allow");
        assert_eq!(decides(&permission, "edit", ".env"), "deny");

        let mut allowed = reads.clone();
        allowed.outside.write = true;
        let permission = super::permission(&allowed).unwrap();
        assert_eq!(
            decides(&permission, "edit", "/home/you/Downloads/notes.md"),
            "allow"
        );
        assert_eq!(
            decides(&permission, "edit", "/home/you/.ssh/config"),
            "deny"
        );
        assert_eq!(decides(&permission, "edit", "/etc/hosts"), "deny");
        assert_eq!(decides(&permission, "edit", "config/.env"), "deny");
    }

    /// A run's session can't ask, so in supervised it reads outside and edits only its
    /// worktree.
    #[test]
    fn a_run_s_sessions_in_supervised_read_outside_and_edit_only_their_worktree() {
        let tree = "/home/you/.argonauts/worktrees/app/run-1/task-a";
        let mut worker = in_supervised(SessionRole::Build, tree);
        worker.outside = worker.outside.clone().without_asking();
        let permission = permission(&worker).unwrap();
        assert_eq!(
            decides(&permission, "read", "/home/you/Downloads/brief.txt"),
            "allow"
        );
        assert_eq!(
            decides(&permission, "edit", "/home/you/Downloads/notes.md"),
            "deny"
        );
        assert_eq!(
            decides(&permission, "edit", "/home/you/app/src/main.rs"),
            "deny"
        );
        assert_eq!(decides(&permission, "edit", "src/main.rs"), "allow");
        let mut reader = in_supervised(SessionRole::ReadOnly, tree);
        reader.outside = reader.outside.clone().without_asking();
        let permission = super::permission(&reader).unwrap();
        assert!(permission.get("edit").is_none());
        assert_eq!(
            decides(&permission, "read", "/home/you/Downloads/brief.txt"),
            "allow"
        );
    }

    /// A template a project commits, taken back out of `.env.*`, reads and edits as any file
    /// in the project, a plain name stays closed, and a folder on the list keeps even a
    /// template.
    #[test]
    fn a_template_taken_back_out_of_the_env_files_opens_and_nothing_else_does() {
        let mut req = on_autopilot(SessionRole::Direct, "/home/you/app");
        req.outside.never.names = vec![".env".to_owned(), ".env.*".to_owned()];
        req.outside.never.except = vec![".env.example".to_owned()];
        let permission = permission(&req).unwrap();
        for action in ["read", "edit"] {
            assert_eq!(
                decides(&permission, action, ".env.example"),
                "allow",
                "{action}"
            );
            assert_eq!(decides(&permission, action, "config/.env.example"), "allow");
            assert_eq!(
                decides(&permission, action, ".env.local"),
                "deny",
                "{action}"
            );
            assert_eq!(decides(&permission, action, ".env"), "deny", "{action}");
            assert_eq!(
                decides(&permission, action, "/home/you/.ssh/.env.example"),
                "deny",
                "{action}"
            );
        }
        let mut reads = req.clone();
        reads.outside.write = false;
        let permission = super::permission(&reads).unwrap();
        assert_eq!(decides(&permission, "edit", ".env.example"), "allow");
        assert_eq!(
            decides(&permission, "edit", "/home/you/Downloads/.env.example"),
            "deny"
        );
    }

    #[test]
    fn a_worker_edits_the_checkout_only_in_its_worktree_and_a_reader_edits_nothing() {
        let tree = "/home/you/.argonauts/worktrees/app/run-1/task-a";
        let worker = permission(&on_autopilot(SessionRole::Build, tree)).unwrap();
        assert_eq!(worker["edit"]["/home/you/app/*"], json!("deny"));
        let reader = permission(&on_autopilot(SessionRole::ReadOnly, tree)).unwrap();
        assert!(reader.get("edit").is_none());
        assert_eq!(reader["read"]["*"], json!("allow"));
    }
}
