use argonauts_core::events::session::{InputPart, InputPartKind};

use super::pictures;

fn part(kind: InputPartKind, label: &str, source: Option<&str>) -> InputPart {
    InputPart {
        kind,
        label: label.to_owned(),
        text: String::new(),
        approx_tokens: 0,
        source: source.map(str::to_owned),
    }
}

#[test]
fn the_pictures_in_the_prompt_go_to_the_cli_by_their_numbers() {
    let parts = [
        part(InputPartKind::Message, "your message", None),
        part(InputPartKind::Picture, "[Image #3]", Some("/tmp/three.png")),
        part(InputPartKind::File, "src/main.rs", Some("src/main.rs")),
        part(InputPartKind::Picture, "[Image #4]", Some("/tmp/four.jpg")),
    ];
    let sent = pictures(&parts);
    let numbers: Vec<u32> = sent.iter().map(|picture| picture.number).collect();
    assert_eq!(numbers, [3, 4]);
    assert_eq!(sent[1].path, std::path::PathBuf::from("/tmp/four.jpg"));
}

/// A picture sent with your message reaches each CLI's command as that CLI takes one, through
/// the same request the answer formats go out strict in: Codex gets its image flag beside the
/// strict format before the prompt, and Claude Code's first line holds the image block.
#[test]
fn a_picture_reaches_each_clis_command_beside_the_strict_answer_format() {
    use argonauts_adapter_claude_code::ClaudeCodeAdapter;
    use argonauts_adapter_codex::CodexAdapter;
    use argonauts_core::adapter::Adapter;
    use argonauts_core::answer_format;
    use argonauts_core::names::WorkerRef;
    use argonauts_core::settings::Settings;
    use argonauts_core::worker::Access;

    let work = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let shot = scratch.path().join("shot.png");
    std::fs::write(&shot, [0x89, b'P', b'N', b'G', 9, 9]).unwrap();
    let shot_path = shot.display().to_string();
    let mut spec = crate::harness_worker::testing::spec_in(
        work.path(),
        scratch.path(),
        "what's off on this screen? [Image #1]",
    );
    spec.prompt
        .push(part(InputPartKind::Picture, "[Image #1]", Some(&shot_path)));

    spec.worker = WorkerRef::new("codex", None);
    spec.json_schema = Some(serde_json::json!({
        "type": "object",
        "properties": {"accept": {"type": "boolean"}, "summary": {"type": "string"}},
        "required": ["accept"],
    }));
    let req = super::session_request(&spec, &CodexAdapter, &Settings::default(), None).unwrap();
    let codex = CodexAdapter.command(&req).unwrap();
    let at = |flag: &str| codex.args.iter().position(|arg| arg == flag);
    let image = at("--image").expect("the image flag");
    assert_eq!(codex.args[image + 1], shot_path);
    let format = at("--output-schema").expect("the answer format");
    let prompt = at("-").expect("the prompt from stdin");
    assert!(image < prompt && format < prompt, "{:?}", codex.args);
    let file = codex
        .files
        .iter()
        .find(|file| file.path.display().to_string() == codex.args[format + 1])
        .expect("the answer format's file");
    let written: serde_json::Value = serde_json::from_str(&file.contents).unwrap();
    assert_eq!(
        answer_format::codex_problems(&written),
        Vec::<String>::new()
    );

    spec.worker = WorkerRef::new("claude-code", None);
    spec.json_schema = None;
    spec.access = Access::Direct;
    spec.keep_input_open = true;
    let req =
        super::session_request(&spec, &ClaudeCodeAdapter, &Settings::default(), None).unwrap();
    let claude = ClaudeCodeAdapter.command(&req).unwrap();
    let stdin = claude.stdin.expect("the first message");
    let first: serde_json::Value = serde_json::from_str(stdin.lines().last().unwrap()).unwrap();
    let content = first["message"]["content"].as_array().unwrap();
    assert_eq!(content[1]["type"], "image", "{first}");
}
