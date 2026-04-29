use super::helpers::AftProcess;

#[test]
fn bash_streams_progress_and_returns_final_response() {
    let mut aft = AftProcess::spawn();

    let frames = aft.send_until(
        r#"{"id":"bash-1","method":"bash","params":{"command":"echo hello"}}"#,
        |value| value["id"] == "bash-1",
    );

    let progress = frames
        .iter()
        .find(|frame| frame["type"] == "progress" && frame["request_id"] == "bash-1")
        .expect("expected at least one progress frame");
    assert_eq!(progress["kind"], "stdout");
    assert_eq!(progress["chunk"], "hello\n");

    let final_response = frames.last().expect("final response");
    assert_eq!(final_response["id"], "bash-1");
    assert_eq!(final_response["success"], true);
    assert_eq!(final_response["output"], "hello\n");
    assert_eq!(final_response["exit_code"], 0);
    assert_eq!(final_response["truncated"], false);
    assert!(final_response["duration_ms"].is_u64());

    let status = aft.shutdown();
    assert!(status.success());
}
