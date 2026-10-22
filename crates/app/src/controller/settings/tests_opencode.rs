//! OpenCode's level in `/model`, from `opencode api model.list` on a private server: its models
//! in their groups and order, each with its window, tool use and variants as levels, and
//! Default saying what it runs. A fake `opencode` answers as OpenCode 2.0.32 does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use argonauts_core::app::ModelChoice;
use argonauts_core::registry::ModelRegistry as _;
use argonauts_core::settings::Settings;
use argonauts_registry::models::FileRegistry;
use argonauts_testkit::{FakeDirectory, FakeRunner};

use super::tests::Fixture;
use super::*;

const LISTED: &str = r#"{"location":{},"data":[
 {"id":"nemotron-3.5-super-vl-free","providerID":"opencode","name":"Nemotron 3.5 Super VL Free",
  "capabilities":{"tools":true},"variants":[{"id":"none"},{"id":"minimal"},{"id":"low"},
  {"id":"medium"},{"id":"high"},{"id":"xhigh"}],"time":{"released":1792022400000},
  "cost":[{"input":0,"output":0}],"status":"active","enabled":true,"limit":{"context":1048576}},
 {"id":"deepseek-v4.1-flash-free","providerID":"opencode","name":"DeepSeek V4.1 Flash Free",
  "capabilities":{"tools":true},"variants":[{"id":"low"},{"id":"high"},{"id":"max"}],
  "time":{"released":1788998400000},"cost":[{"input":0,"output":0}],"status":"active",
  "enabled":true,"limit":{"context":200000}},
 {"id":"gemma3:12b-32k","providerID":"ollama","name":"gemma3:12b-32k",
  "capabilities":{"tools":false},"variants":[],"time":{"released":0},"cost":[],
  "status":"active","enabled":true,"limit":{"context":131072}},
 {"id":"qwen3-coder:30b-64k","providerID":"ollama","name":"Qwen3 Coder 30B (64k)",
  "capabilities":{"tools":true},"variants":[],"time":{"released":0},"cost":[],
  "status":"active","enabled":true,"limit":{"context":65536}}
]}"#;

const DEFAULT: &str =
    r#"{"location":{},"data":{"id":"nemotron-3.5-super-vl-free","providerID":"opencode"}}"#;

/// A fake `opencode` 2.0.32 that answers `--version`, starts a server that answers
/// `api … model.list` and `api … model.default`, and refuses anything else.
fn fake_opencode(dir: &Path) -> PathBuf {
    std::fs::write(dir.join("listed.json"), LISTED).unwrap();
    std::fs::write(dir.join("default.json"), DEFAULT).unwrap();
    let path = dir.join("opencode");
    let at = dir.display();
    let script = format!(
        "#!/bin/sh\n\
         case \"$1 $2 $3 $4\" in\n\
         '--version   ') echo 'opencode v2.0.32' ;;\n\
         'serve --stdio --port 0') echo '{{\"url\":\"http://127.0.0.1:9\"}}'; cat > /dev/null ;;\n\
         'api --server http://127.0.0.1:9 model.list') cat '{at}/listed.json' ;;\n\
         'api --server http://127.0.0.1:9 model.default') cat '{at}/default.json' ;;\n\
         *) exit 2 ;;\n\
         esac\n"
    );
    argonauts_testkit::write_script(&path, &script).unwrap();
    path
}

/// Only OpenCode installed and on, listing through the fake, and no local server asked.
async fn opencode_only(fixture: &mut Fixture, dir: &Path) {
    let mut settings = Settings::default();
    for (id, agent) in settings.agents.iter_mut() {
        agent.enabled = id == "opencode";
    }
    settings.agents.get_mut("opencode").unwrap().binary = Some(fake_opencode(dir));
    settings.registry.local_servers = false;
    let registry = Arc::new(
        FileRegistry::from_files(&settings, None, None)
            .with_confine(Arc::new(argonauts_core::confine::Unconfined)),
    );
    registry.discover().await;
    assert_eq!(
        registry.discovery_skipped(),
        ["Claude Code, Codex, agy: off in your settings"]
    );
    fixture.ctx.services.registry = registry;
    let directory = Arc::new(FakeDirectory::new());
    directory.add(FakeRunner::cli("opencode", "opencode"));
    fixture.ctx.services.workers = directory;
}

fn of_opencode(choices: &[ModelChoice]) -> Vec<&ModelChoice> {
    choices
        .iter()
        .filter(|choice| choice.worker.agent == "opencode")
        .collect()
}

#[tokio::test]
async fn opencodes_models_come_in_their_groups_with_their_own_levels() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = Fixture::new(false, |_| {});
    opencode_only(&mut fixture, dir.path()).await;
    let choices = models::choices(&fixture.ctx, None);
    let opencode = of_opencode(&choices);
    let shown: Vec<String> = opencode
        .iter()
        .map(|choice| choice.worker.to_string())
        .collect();
    assert_eq!(
        shown,
        [
            "opencode",
            "opencode/ollama/qwen3-coder:30b-64k",
            "opencode/ollama/gemma3:12b-32k",
            "opencode/opencode/nemotron-3.5-super-vl-free",
            "opencode/opencode/deepseek-v4.1-flash-free",
        ],
        "Default, Ollama's with tool use first, then OpenCode Zen's newest first"
    );

    let default = opencode[0];
    assert_eq!(default.runs.as_deref(), Some("Nemotron 3.5 Super VL Free"));
    assert_eq!(
        default.efforts,
        ["none", "minimal", "low", "medium", "high", "xhigh"],
        "Default's levels are those of the model it runs"
    );

    let qwen = opencode[1];
    assert_eq!(qwen.display_name(), "Qwen3 Coder 30B (64k)");
    assert!(qwen.efforts.is_empty(), "no variants, so it takes none");
    let facts = qwen.listed.as_ref().expect("OpenCode's facts");
    assert_eq!(facts.group.as_deref(), Some("Ollama"));
    assert_eq!(facts.window, Some(65_536));
    assert_eq!(facts.hosted_by, None);
    assert!(!facts.no_tools);

    let gemma = opencode[2];
    assert_eq!(gemma.display_name(), "Gemma3 12B 32K");
    assert!(gemma.listed.as_ref().unwrap().no_tools);

    let deepseek = opencode[4];
    assert_eq!(deepseek.efforts, ["low", "high", "max"]);
    assert_eq!(
        deepseek.default_effort, None,
        "OpenCode names no default variant"
    );
    let facts = deepseek.listed.as_ref().unwrap();
    assert_eq!(facts.group.as_deref(), Some("OpenCode Zen"));
    assert_eq!(facts.hosted_by.as_deref(), Some("OpenCode Zen"));
    assert_eq!(deepseek.cost, argonauts_core::limits::CostEstimate::Free);
}
