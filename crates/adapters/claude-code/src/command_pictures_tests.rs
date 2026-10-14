use std::path::PathBuf;

use argonauts_core::pictures::Picture;
use argonauts_core::{SessionRequest, SessionRole};
use serde_json::Value;

use super::build_command;

fn picture(dir: &std::path::Path, number: u32, bytes: &[u8]) -> Picture {
    let path = dir.join(format!("shot-{number}.png"));
    std::fs::write(&path, bytes).unwrap();
    Picture { number, path }
}

fn first_line(req: &SessionRequest) -> Value {
    let spec = build_command("claude", req).unwrap();
    let stdin = spec.stdin.unwrap();
    let line = stdin.lines().last().unwrap();
    serde_json::from_str(line).unwrap()
}

#[test]
fn a_picture_goes_in_as_an_image_block_of_your_message() {
    let dir = tempfile::tempdir().unwrap();
    let shot = picture(dir.path(), 1, &[0x89, b'P', b'N', b'G', 9, 9]);
    let req = SessionRequest {
        workdir: PathBuf::from("/tmp/worktree"),
        prompt: "what's off on this screen? [Image #1]".into(),
        role: SessionRole::Direct,
        keep_input_open: true,
        typed_by_you: true,
        pictures: vec![shot],
        ..SessionRequest::default()
    };
    let line = first_line(&req);
    let content = line["message"]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "what's off on this screen? [Image #1]");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["media_type"], "image/png");
    assert_eq!(line["origin"]["kind"], "human", "still yours");
}

#[test]
fn a_picture_that_cant_go_as_an_image_goes_as_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let missing = Picture {
        number: 2,
        path: dir.path().join("gone.png"),
    };
    let req = SessionRequest {
        workdir: PathBuf::from("/tmp/worktree"),
        prompt: "and this one [Image #2]".into(),
        role: SessionRole::Direct,
        keep_input_open: true,
        pictures: vec![missing.clone()],
        ..SessionRequest::default()
    };
    let line = first_line(&req);
    let content = line["message"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    let text = content[0]["text"].as_str().unwrap();
    assert!(text.contains(&missing.path.display().to_string()), "{text}");
    // Without stream input, the text names every picture's file.
    let plain = build_command(
        "claude",
        &SessionRequest {
            keep_input_open: false,
            ..req
        },
    )
    .unwrap();
    assert!(plain.stdin.unwrap().contains("[Image #2] "));
}

#[test]
fn a_picture_typed_while_it_works_joins_the_turn_as_an_image_block() {
    let dir = tempfile::tempdir().unwrap();
    let shot = picture(dir.path(), 2, &[0x89, b'P', b'N', b'G', 7]);
    let line = super::picture_message_line_with_uuid("and this one [Image #2]", &[shot], "u-1");
    let line: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(line["type"], "user");
    assert_eq!(line["uuid"], "u-1");
    let content = line["message"]["content"].as_array().unwrap();
    assert_eq!(content[0]["text"], "and this one [Image #2]");
    assert_eq!(content[1]["type"], "image");
}
