use std::path::PathBuf;

use argonauts_core::look::pictures::Picture;
use argonauts_core::{SessionRequest, SessionRole};

use super::build;

#[test]
fn each_picture_goes_with_codexs_image_flag_before_resume() {
    let req = SessionRequest {
        workdir: PathBuf::from("/work/tree"),
        prompt: "what's off here? [Image #1] [Image #2]".into(),
        role: SessionRole::Direct,
        resume_session: Some("0199a000-0000-7000-8000-000000000001".into()),
        pictures: vec![
            Picture {
                number: 1,
                path: "/tmp/one.png".into(),
            },
            Picture {
                number: 2,
                path: "/tmp/two.jpg".into(),
            },
        ],
        ..SessionRequest::default()
    };
    let spec = build("codex", &req).unwrap();
    let args = &spec.args;
    let at = |needle: &str| args.iter().position(|arg| arg == needle).unwrap();
    assert_eq!(args[at("/tmp/one.png") - 1], "--image");
    assert_eq!(args[at("/tmp/two.jpg") - 1], "--image");
    assert!(at("/tmp/two.jpg") < at("resume"), "{args:?}");
    assert_eq!(spec.stdin.as_deref(), Some(req.prompt.as_str()));
}
