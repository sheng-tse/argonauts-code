//! OpenCode's permissions for work outside the project, sent in its inline config. OpenCode
//! 2.0.32 takes `permission` as a map from an action to patterns, each `allow`, `ask` or
//! `deny`; the last pattern that matches wins, as its own defaults (`*` allowed, then `*.env`
//! asked) show, and `*` there matches across folders. `external_directory` is what any tool
//! needs to reach a folder outside the project, so the never-list is denied there for every
//! tool, and in `read` and `edit` for the project's own files. Inside our sandbox the never-list
//! holds in the OS too.

use std::path::Path;

use argonauts_core::CommandSpec;
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
    let writes = outside.write && req.role != SessionRole::ReadOnly;
    let mut closed: Vec<String> = Vec::new();
    for path in &outside.never.paths {
        closed.extend(path_patterns(path));
    }
    for name in &outside.never.names {
        closed.extend([name.clone(), format!("*/{name}")]);
    }
    let mut unwritten: Vec<String> = outside
        .never
        .unwritten
        .iter()
        .flat_map(|path| path_patterns(path))
        .collect();
    unwritten.extend(
        outside
            .project
            .iter()
            .filter(|project| writes && !req.workdir.starts_with(project))
            .flat_map(|project| path_patterns(project)),
    );
    let mut permission = Map::new();
    permission.insert("external_directory".to_owned(), allowed_but(&closed));
    if outside.read {
        permission.insert("read".to_owned(), allowed_but(&closed));
    }
    if writes {
        let both: Vec<String> = closed.iter().chain(&unwritten).cloned().collect();
        permission.insert("edit".to_owned(), allowed_but(&both));
    }
    Some(Value::Object(permission))
}

/// `*` allowed first, then each pattern denied, since the last match wins.
fn allowed_but(denied: &[String]) -> Value {
    let mut rules = Map::new();
    rules.insert("*".to_owned(), json!("allow"));
    for pattern in denied {
        rules.insert(pattern.clone(), json!("deny"));
    }
    Value::Object(rules)
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

    use super::*;

    fn on_autopilot(role: SessionRole, workdir: &str) -> SessionRequest {
        SessionRequest {
            workdir: PathBuf::from(workdir),
            role,
            outside: Outside {
                read: true,
                write: true,
                project: Some(PathBuf::from("/home/you/app")),
                never: Never {
                    paths: vec![PathBuf::from("/home/you/.ssh")],
                    names: vec![".env".to_owned()],
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
            ["*", "/home/you/.ssh", "/home/you/.ssh/*", ".env", "*/.env"]
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
