//! Integration tests for the `move_file` command, focused on error-message
//! quality (BUG-7 from the dogfooding triage).

use serde_json::json;

use crate::helpers::AftProcess;

fn configure(aft: &mut AftProcess, root: &std::path::Path) {
    let resp = aft.configure(root);
    assert_eq!(resp["success"], true, "configure should succeed: {resp:?}");
}

fn send(aft: &mut AftProcess, request: serde_json::Value) -> serde_json::Value {
    aft.send(&serde_json::to_string(&request).expect("serialize request"))
}

/// Repro for the dogfooding complaint: an agent renamed `omo.png → alfonso.png`
/// earlier in a session, then later tried the same rename again. The
/// pre-existing message was just `move_file: source file not found: omo.png`,
/// which is technically correct but doesn't surface the obvious likely
/// cause (already-moved). The agent had to do an `ls` round-trip to
/// figure that out.
///
/// New behavior: when the source is missing AND the destination already
/// exists, the error message includes a hint suggesting the file may have
/// been moved earlier. Code stays `file_not_found` so the machine-readable
/// error class is unchanged.
#[test]
fn move_file_already_moved_hints_at_destination() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let root = tmp.path();

    // Simulate the "already moved" state: source missing, destination
    // present with the file the agent intended to move there.
    let dst_path = root.join("alfonso.png");
    std::fs::write(&dst_path, b"fake png bytes").expect("write destination");

    let mut aft = AftProcess::spawn();
    configure(&mut aft, root);

    let src_abs = root.join("omo.png").display().to_string();
    let dst_abs = root.join("alfonso.png").display().to_string();
    let resp = send(
        &mut aft,
        json!({
            "id": "move-already-done",
            "command": "move_file",
            "file": src_abs,
            "destination": dst_abs,
        }),
    );

    assert_eq!(resp["success"], false, "rename should fail: {resp:?}");
    assert_eq!(resp["code"], "file_not_found");

    let message = resp["message"].as_str().expect("error message string");

    // Original info still present so machine consumers don't break.
    assert!(
        message.contains("source file not found"),
        "should still say source not found: {message}"
    );
    assert!(
        message.contains("omo.png"),
        "should name the source: {message}"
    );

    // New: hints at the most likely cause without forcing the agent to
    // round-trip through `ls`/`stat` to figure it out.
    assert!(
        message.contains("alfonso.png"),
        "should mention the destination: {message}"
    );
    assert!(
        message.contains("already exists"),
        "should explicitly state the destination already exists: {message}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

/// Counterpart: when the source is missing AND the destination is ALSO
/// missing, the agent didn't pre-rename anything — they just typed the
/// wrong path. The hint must NOT be added in that case (otherwise it
/// would mislead the agent into believing they had moved a non-existent
/// file).
#[test]
fn move_file_missing_source_without_existing_destination_keeps_short_message() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let root = tmp.path();

    let mut aft = AftProcess::spawn();
    configure(&mut aft, root);

    let src_abs = root.join("nope.png").display().to_string();
    let dst_abs = root.join("also-nope.png").display().to_string();
    let resp = send(
        &mut aft,
        json!({
            "id": "move-missing",
            "command": "move_file",
            "file": src_abs,
            "destination": dst_abs,
        }),
    );

    assert_eq!(resp["success"], false, "rename should fail: {resp:?}");
    assert_eq!(resp["code"], "file_not_found");

    let message = resp["message"].as_str().expect("error message string");

    assert!(
        message.contains("source file not found"),
        "should say source not found: {message}"
    );
    assert!(
        !message.contains("already exists"),
        "must NOT add the destination-exists hint when destination doesn't exist: {message}"
    );
    assert!(
        !message.contains("already moved"),
        "must NOT speculate about prior moves when there's no destination evidence: {message}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}
