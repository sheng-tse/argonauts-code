use std::path::PathBuf;

use argonauts_core::pictures::Picture;
use argonauts_core::{SessionRequest, SessionRole};

use super::build_command;

#[test]
fn a_picture_goes_to_agy_as_its_files_path() {
    let req = SessionRequest {
        workdir: PathBuf::from("/work/tree"),
        prompt: "what's off here? [Image #1]".into(),
        role: SessionRole::Direct,
        pictures: vec![Picture {
            number: 1,
            path: "/tmp/shot.png".into(),
        }],
        ..SessionRequest::default()
    };
    let spec = build_command("agy", &req).unwrap();
    let line: serde_json::Value = serde_json::from_str(spec.stdin.as_deref().unwrap()).unwrap();
    let text = line["message"]["content"].as_str().unwrap();
    assert!(text.starts_with("what's off here? [Image #1]"), "{text}");
    assert!(text.ends_with("[Image #1] /tmp/shot.png"), "{text}");
    assert!(!spec.args.iter().any(|arg| arg.contains("shot.png")));
}
