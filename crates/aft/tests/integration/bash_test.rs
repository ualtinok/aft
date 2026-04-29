use super::helpers::AftProcess;

#[cfg(unix)]
fn process_exists(pid: i32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    if !output.status.success() {
        return false;
    }
    !String::from_utf8_lossy(&output.stdout).contains('Z')
}

#[cfg(unix)]
fn wait_until_process_exits(pid: i32) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(2) {
        if !process_exists(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

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

#[cfg(unix)]
#[test]
fn bash_timeout_terminates_shell_process_group_grandchild() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("sleep.pid");
    let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());

    let response = aft.send(
        &serde_json::json!({
            "id": "bash-timeout-pgroup",
            "method": "bash",
            "params": { "command": command, "timeout": 200 }
        })
        .to_string(),
    );

    assert_eq!(response["success"], true, "bash failed: {response:?}");
    assert_eq!(response["timed_out"], true);
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_until_process_exits(pid),
        "grandchild sleep process {pid} survived foreground timeout"
    );

    assert!(aft.shutdown().success());
}
