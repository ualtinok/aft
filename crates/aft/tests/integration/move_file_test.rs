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

#[cfg(target_os = "linux")]
#[test]
fn move_file_cross_fs_copy_delete_failure_reports_partial_success() {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    if !Path::new("/dev/shm").exists() {
        return;
    }

    let src_tmp = tempfile::tempdir().expect("create source temp dir");
    let dst_tmp = tempfile::tempdir_in("/dev/shm").expect("create destination temp dir");
    let src_path = src_tmp.path().join("source.txt");
    let dst_path = dst_tmp.path().join("destination.txt");
    std::fs::write(&src_path, "content\n").expect("write source");

    let src_parent = src_path.parent().expect("source parent");
    let original_mode = std::fs::metadata(src_parent)
        .expect("source parent metadata")
        .permissions()
        .mode();
    std::fs::set_permissions(src_parent, std::fs::Permissions::from_mode(0o555))
        .expect("make source parent undeletable");

    let mut aft = AftProcess::spawn();
    configure(&mut aft, Path::new("/"));

    let resp = send(
        &mut aft,
        json!({
            "id": "move-partial-delete",
            "command": "move_file",
            "file": src_path.display().to_string(),
            "destination": dst_path.display().to_string(),
        }),
    );

    std::fs::set_permissions(src_parent, std::fs::Permissions::from_mode(original_mode))
        .expect("restore source parent permissions");

    assert_eq!(
        resp["success"], true,
        "copy succeeded, delete failed: {resp:?}"
    );
    assert_eq!(resp["moved"], true);
    assert_eq!(resp["complete"], false);
    assert_eq!(resp["source_delete_failed"], true);
    assert!(resp["warning"]
        .as_str()
        .is_some_and(|warning| warning.contains("Both paths now exist")));
    assert!(src_path.exists(), "source remains after partial move");
    assert!(dst_path.exists(), "destination was written");

    let status = aft.shutdown();
    assert!(status.success());
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
