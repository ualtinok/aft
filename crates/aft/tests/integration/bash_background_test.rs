use std::time::{Duration, Instant};

use serde_json::{json, Value};

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
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if !process_exists(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn configure_background(aft: &mut AftProcess) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let response = aft.send(
        &json!({
            "id": "cfg-bg",
            "command": "configure",
            "project_root": dir.path(),
            "experimental_bash_background": true,
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "configure failed: {response:?}");
    dir
}

fn spawn_bg(aft: &mut AftProcess, id: &str, command: &str) -> String {
    let response = aft.send(
        &json!({
            "id": id,
            "command": "bash",
            "params": { "command": command, "background": true }
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "spawn failed: {response:?}");
    assert_eq!(response["status"], "running");
    response["task_id"].as_str().unwrap().to_string()
}

fn spawn_bg_params(aft: &mut AftProcess, id: &str, params: Value) -> String {
    let response = aft.send(
        &json!({
            "id": id,
            "command": "bash",
            "params": params
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "spawn failed: {response:?}");
    assert_eq!(response["status"], "running");
    response["task_id"].as_str().unwrap().to_string()
}

fn status(aft: &mut AftProcess, task_id: &str) -> Value {
    aft.send(
        &json!({
            "id": format!("status-{task_id}"),
            "command": "bash_status",
            "params": { "task_id": task_id }
        })
        .to_string(),
    )
}

fn status_with_session(aft: &mut AftProcess, task_id: &str, session_id: &str) -> Value {
    aft.send(
        &json!({
            "id": format!("status-{session_id}-{task_id}"),
            "session_id": session_id,
            "command": "bash_status",
            "params": { "task_id": task_id }
        })
        .to_string(),
    )
}

fn wait_for_status(aft: &mut AftProcess, task_id: &str, expected: &str) -> Value {
    let started = Instant::now();
    loop {
        let response = status(aft, task_id);
        assert_eq!(response["success"], true, "status failed: {response:?}");
        if response["status"] == expected {
            return response;
        }
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "timed out waiting for {expected}: {response:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn background_spawn_status_running_and_completion() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(&mut aft, "spawn-running", "sleep 0.5");
    let running = status(&mut aft, &task_id);
    assert_eq!(
        running["success"], true,
        "running status failed: {running:?}"
    );
    assert_eq!(running["status"], "running");

    let completed = wait_for_status(&mut aft, &task_id, "completed");
    assert_eq!(completed["exit_code"], 0);
    assert!(completed["duration_ms"].is_u64());

    assert!(aft.shutdown().success());
}

#[test]
fn background_output_preview_updates_and_completes() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(
        &mut aft,
        "spawn-output",
        "echo hello; sleep 0.5; echo world",
    );
    let started = Instant::now();
    loop {
        let response = status(&mut aft, &task_id);
        assert_eq!(response["success"], true, "status failed: {response:?}");
        if response["output_preview"]
            .as_str()
            .unwrap_or("")
            .contains("hello")
        {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(4));
        std::thread::sleep(Duration::from_millis(50));
    }

    let completed = wait_for_status(&mut aft, &task_id, "completed");
    let output = completed["output_preview"].as_str().unwrap();
    assert!(output.contains("hello\n"));
    assert!(output.contains("world\n"));

    assert!(aft.shutdown().success());
}

#[test]
fn background_kill_running_task() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(&mut aft, "spawn-kill", "sleep 5");
    let killed = aft.send(
        &json!({
            "id": "kill-bg",
            "command": "bash_kill",
            "params": { "task_id": task_id }
        })
        .to_string(),
    );
    assert_eq!(killed["success"], true, "kill failed: {killed:?}");
    assert_eq!(killed["status"], "killed");

    let after = status(&mut aft, &task_id);
    assert_eq!(after["status"], "killed");

    assert!(aft.shutdown().success());
}

#[test]
fn background_kill_long_running_task_stays_killed_not_failed() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(&mut aft, "spawn-kill-race", "sleep 30");
    let killed = aft.send(
        &json!({
            "id": "kill-bg-race",
            "command": "bash_kill",
            "params": { "task_id": task_id }
        })
        .to_string(),
    );
    assert_eq!(killed["success"], true, "kill failed: {killed:?}");
    assert_eq!(killed["status"], "killed");

    let started = Instant::now();
    loop {
        let after = status(&mut aft, &task_id);
        assert_ne!(after["status"], "failed", "kill was overwritten: {after:?}");
        if after["status"] == "killed" {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(aft.shutdown().success());
}

#[cfg(unix)]
#[test]
fn background_kill_terminates_shell_process_group_grandchild() {
    let mut aft = AftProcess::spawn();
    let dir = configure_background(&mut aft);
    let pid_file = dir.path().join("bg-sleep.pid");
    let command = format!("sleep 30 & echo $! > {}; wait", pid_file.display());

    let task_id = spawn_bg(&mut aft, "spawn-kill-pgroup", &command);
    let started = Instant::now();
    while !pid_file.exists() {
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    let killed = aft.send(
        &json!({
            "id": "kill-bg-pgroup",
            "command": "bash_kill",
            "params": { "task_id": task_id }
        })
        .to_string(),
    );
    assert_eq!(killed["success"], true, "kill failed: {killed:?}");
    assert_eq!(killed["status"], "killed");
    assert!(
        wait_until_process_exits(pid),
        "grandchild sleep process {pid} survived background kill"
    );

    assert!(aft.shutdown().success());
}

#[test]
fn background_concurrent_task_cap_is_enforced() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let mut task_ids = Vec::new();
    for i in 0..8 {
        task_ids.push(spawn_bg(&mut aft, &format!("spawn-cap-{i}"), "sleep 2"));
    }
    let rejected = aft.send(
        &json!({
            "id": "spawn-cap-rejected",
            "command": "bash",
            "params": { "command": "sleep 1", "background": true }
        })
        .to_string(),
    );
    assert_eq!(
        rejected["success"], false,
        "9th task should fail: {rejected:?}"
    );
    assert_eq!(rejected["code"], "background_task_limit_exceeded");

    for task_id in task_ids {
        let _ = aft.send(
            &json!({
                "id": format!("kill-{task_id}"),
                "command": "bash_kill",
                "params": { "task_id": task_id }
            })
            .to_string(),
        );
    }

    assert!(aft.shutdown().success());
}

#[test]
fn background_output_spills_to_disk() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(&mut aft, "spawn-spill", "yes x | head -c 1200000");
    let completed = wait_for_status(&mut aft, &task_id, "completed");
    assert_eq!(completed["success"], true, "status failed: {completed:?}");
    let output_path = completed["output_path"].as_str().expect("spill path");
    let metadata = std::fs::metadata(output_path).expect("spill file metadata");
    assert!(
        metadata.len() >= 1_200_000,
        "spill was too small: {metadata:?}"
    );
    assert_eq!(completed["output_truncated"], true);

    assert!(aft.shutdown().success());
}

#[test]
fn background_feature_flag_disabled_rejects_spawn() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let configure = aft.send(
        &json!({
            "id": "cfg-disabled",
            "command": "configure",
            "project_root": dir.path()
        })
        .to_string(),
    );
    assert_eq!(configure["success"], true);

    let response = aft.send(
        &json!({
            "id": "spawn-disabled",
            "command": "bash",
            "params": { "command": "sleep 1", "background": true }
        })
        .to_string(),
    );
    assert_eq!(response["success"], false);
    assert_eq!(response["code"], "feature_disabled");

    assert!(aft.shutdown().success());
}

#[test]
fn background_status_unknown_task_returns_task_not_found() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let response = aft.send(
        &json!({
            "id": "status-missing",
            "command": "bash_status",
            "params": { "task_id": "missing-task" }
        })
        .to_string(),
    );
    assert_eq!(response["success"], false);
    assert_eq!(response["code"], "task_not_found");

    assert!(aft.shutdown().success());
}

#[test]
fn background_status_rejects_cross_session_task_as_not_found() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let spawn = aft.send(
        &json!({
            "id": "spawn-owned-status",
            "session_id": "session-a",
            "command": "bash",
            "params": { "command": "sleep 2", "background": true }
        })
        .to_string(),
    );
    assert_eq!(spawn["success"], true, "spawn failed: {spawn:?}");
    let task_id = spawn["task_id"].as_str().unwrap().to_string();

    let rejected = status_with_session(&mut aft, &task_id, "session-b");
    assert_eq!(
        rejected["success"], false,
        "cross-session status leaked: {rejected:?}"
    );
    assert_eq!(rejected["code"], "task_not_found");

    let owned = status_with_session(&mut aft, &task_id, "session-a");
    assert_eq!(owned["success"], true, "owner status failed: {owned:?}");

    assert!(aft.shutdown().success());
}

#[test]
fn background_kill_rejects_cross_session_task_as_not_found() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let spawn = aft.send(
        &json!({
            "id": "spawn-owned-kill",
            "session_id": "session-a",
            "command": "bash",
            "params": { "command": "sleep 2", "background": true }
        })
        .to_string(),
    );
    assert_eq!(spawn["success"], true, "spawn failed: {spawn:?}");
    let task_id = spawn["task_id"].as_str().unwrap().to_string();

    let rejected = aft.send(
        &json!({
            "id": "kill-cross-session",
            "session_id": "session-b",
            "command": "bash_kill",
            "params": { "task_id": task_id }
        })
        .to_string(),
    );
    assert_eq!(
        rejected["success"], false,
        "cross-session kill succeeded: {rejected:?}"
    );
    assert_eq!(rejected["code"], "task_not_found");

    let owned = status_with_session(&mut aft, &task_id, "session-a");
    assert_eq!(owned["success"], true, "owner status failed: {owned:?}");
    assert_eq!(owned["status"], "running");

    let killed = aft.send(
        &json!({
            "id": "kill-owner-session",
            "session_id": "session-a",
            "command": "bash_kill",
            "params": { "task_id": task_id }
        })
        .to_string(),
    );
    assert_eq!(killed["success"], true, "owner kill failed: {killed:?}");
    assert_eq!(killed["status"], "killed");

    assert!(aft.shutdown().success());
}

#[test]
fn background_completion_metadata_is_attached_to_next_response() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(&mut aft, "spawn-completion", "echo done");
    let started = Instant::now();
    loop {
        let ping = aft.send(r#"{"id":"ping-bg","command":"ping"}"#);
        if let Some(completions) = ping["bg_completions"].as_array() {
            let completion = completions
                .iter()
                .find(|completion| completion["task_id"] == task_id)
                .expect("completion for task");
            assert_eq!(completion["status"], "completed");
            assert_eq!(completion["exit_code"], 0);
            assert_eq!(completion["command"], "echo done");
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(4));
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(aft.shutdown().success());
}

#[test]
fn background_completion_delivery_is_scoped_by_session_id() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let spawn = aft.send(
        &json!({
            "id": "spawn-session-a",
            "session_id": "session-a",
            "command": "bash",
            "params": { "command": "echo session-a", "background": true }
        })
        .to_string(),
    );
    assert_eq!(spawn["success"], true, "spawn failed: {spawn:?}");
    let task_id = spawn["task_id"].as_str().unwrap().to_string();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        let session_b = aft.send(
            &json!({
                "id": "ping-session-b",
                "session_id": "session-b",
                "command": "ping"
            })
            .to_string(),
        );
        assert_eq!(session_b["success"], true);
        assert!(
            session_b["bg_completions"]
                .as_array()
                .is_none_or(|items| items.is_empty()),
            "session B drained session A completion: {session_b:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let started = Instant::now();
    let completions = loop {
        let session_a = aft.send(
            &json!({
                "id": "ping-session-a",
                "session_id": "session-a",
                "command": "ping"
            })
            .to_string(),
        );
        assert_eq!(session_a["success"], true);
        if let Some(completions) = session_a["bg_completions"].as_array() {
            break completions.clone();
        }
        assert!(started.elapsed() < Duration::from_secs(4));
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(completions
        .iter()
        .any(|completion| completion["task_id"] == task_id));

    assert!(aft.shutdown().success());
}

#[test]
fn background_spawn_honors_custom_workdir() {
    let mut aft = AftProcess::spawn();
    let dir = configure_background(&mut aft);
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();

    let task_id = spawn_bg_params(
        &mut aft,
        "spawn-bg-workdir",
        json!({ "command": "pwd", "background": true, "workdir": nested }),
    );
    let completed = wait_for_status(&mut aft, &task_id, "completed");
    let actual =
        std::fs::canonicalize(completed["output_preview"].as_str().unwrap().trim()).unwrap();
    let expected = std::fs::canonicalize(&nested).unwrap();
    assert_eq!(actual, expected);

    assert!(aft.shutdown().success());
}

#[test]
fn background_spawn_honors_env_overrides() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg_params(
        &mut aft,
        "spawn-bg-env",
        json!({
            "command": "printf '%s' \"$AFT_BG_ENV_TEST\"",
            "background": true,
            "env": { "AFT_BG_ENV_TEST": "from-bg-env" }
        }),
    );
    let completed = wait_for_status(&mut aft, &task_id, "completed");
    assert_eq!(completed["output_preview"].as_str().unwrap(), "from-bg-env");

    assert!(aft.shutdown().success());
}

#[test]
fn background_spawn_honors_timeout() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg_params(
        &mut aft,
        "spawn-bg-timeout",
        json!({ "command": "sleep 5", "background": true, "timeout": 200 }),
    );
    let started = Instant::now();
    let failed = wait_for_status(&mut aft, &task_id, "failed");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "timeout took too long: {failed:?}"
    );
    assert_eq!(failed["exit_code"], 124);

    assert!(aft.shutdown().success());
}

// ---------------------------------------------------------------------------
// Slug format regression — task IDs must be short, agent-friendly slugs of
// the form `bgb-{8-hex}`. The earlier `{pid}-{nanos}` format produced IDs
// like `81607-1777480557085596000` which are noisy in agent output and hard
// to refer to in chat. Locked in by direct format assertion.
// ---------------------------------------------------------------------------

#[test]
fn background_task_ids_use_short_bgb_slug_format() {
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let task_id = spawn_bg(&mut aft, "slug-format", "true");

    // Format: "bgb-" + exactly 8 lowercase hex characters
    assert!(
        task_id.starts_with("bgb-"),
        "task_id must start with `bgb-` prefix; got `{task_id}`"
    );
    let suffix = &task_id["bgb-".len()..];
    assert_eq!(
        suffix.len(),
        8,
        "task_id suffix must be exactly 8 hex chars; got `{suffix}` (len={})",
        suffix.len()
    );
    assert!(
        suffix
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "task_id suffix must be lowercase hex; got `{suffix}`"
    );

    // Wait for completion and check the completion event carries the same ID
    // — important so the in-turn delivery path isn't broken by the ID change.
    let completed = wait_for_status(&mut aft, &task_id, "completed");
    assert_eq!(completed["task_id"].as_str().unwrap(), task_id);

    assert!(aft.shutdown().success());
}

#[test]
fn background_task_ids_are_unique_across_rapid_spawns() {
    // Spawn 6 short-lived tasks back-to-back and assert all IDs are distinct.
    // Catches generator regressions where the time-based seed alone collapses
    // to the same slug for spawns within the same nanosecond — happens often
    // on macOS where realtime clock resolution is microseconds. The atomic
    // counter inside `random_slug()` is the load-bearing piece this guards.
    //
    // We spawn `true` (exits instantly) and wait for completion between
    // spawns so we don't trip the running-task cap (default 8).
    let mut aft = AftProcess::spawn();
    let _dir = configure_background(&mut aft);

    let mut ids = std::collections::HashSet::new();
    for i in 0..6 {
        let id = spawn_bg(&mut aft, &format!("unique-{i}"), "true");
        assert!(
            ids.insert(id.clone()),
            "duplicate task_id allocated: `{id}` (already in {ids:?})"
        );
        // Drain to completed before the next spawn so running_count stays low.
        let _ = wait_for_status(&mut aft, &id, "completed");
    }

    assert!(aft.shutdown().success());
}
