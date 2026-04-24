use serde_json::json;

use super::helpers::AftProcess;

#[test]
fn configure_accepts_boolean_validate_on_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-validate-bool",
            "command": "configure",
            "project_root": dir.path(),
            "validate_on_edit": true,
        })
        .to_string(),
    );
    assert_eq!(
        configure["success"], true,
        "configure should accept boolean validate_on_edit: {configure:?}"
    );

    let status = aft.send(r#"{"id":"status-validate-bool","command":"status"}"#);
    assert_eq!(status["success"], true, "status should succeed: {status:?}");
    assert_eq!(status["features"]["validate_on_edit"], "syntax");

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}
