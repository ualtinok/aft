#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aft::bash_background::persistence::{session_tasks_dir, write_task, PersistedTask};
use aft::bash_background::{BgTaskRegistry, BgTaskStatus};
use serde_json::{json, Value};

use super::helpers::AftProcess;

const SESSION: &str = "persist-session";

fn spawn_storage_dir(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(name)).unwrap();
    dir
}

fn configure_background(aft: &mut AftProcess, project: &Path, storage: &Path, session: &str) {
    let response = aft.send(
        &json!({
            "id": format!("cfg-{session}"),
            "session_id": session,
            "command": "configure",
            "project_root": project,
            "storage_dir": storage,
            "experimental_bash_background": true,
            "max_background_bash_tasks": 32,
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "configure failed: {response:?}");
}

fn spawn_bg(aft: &mut AftProcess, session: &str, command: &str, timeout: Option<u64>) -> String {
    let mut params = json!({ "command": command, "background": true });
    if let Some(timeout) = timeout {
        params["timeout"] = json!(timeout);
    }
    let response = aft.send(
        &json!({
            "id": "spawn-persist-bg",
            "session_id": session,
            "command": "bash",
            "params": params,
        })
        .to_string(),
    );
    assert_eq!(response["success"], true, "spawn failed: {response:?}");
    response["task_id"].as_str().unwrap().to_string()
}

fn status(aft: &mut AftProcess, session: &str, task_id: &str) -> Value {
    aft.send(
        &json!({
            "id": format!("status-{task_id}"),
            "session_id": session,
            "command": "bash_status",
            "params": { "task_id": task_id }
        })
        .to_string(),
    )
}

fn drain(aft: &mut AftProcess, session: &str) -> Value {
    aft.send(
        &json!({
            "id": "drain-persist-bg",
            "session_id": session,
            "command": "bash_drain_completions"
        })
        .to_string(),
    )
}

fn wait_for_status(aft: &mut AftProcess, session: &str, task_id: &str, expected: &str) -> Value {
    let started = Instant::now();
    loop {
        let response = status(aft, session, task_id);
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

fn task_file(storage: &Path, session: &str, task_id: &str, suffix: &str) -> PathBuf {
    session_tasks_dir(storage, session).join(format!("{task_id}.{suffix}"))
}

fn read_json(storage: &Path, session: &str, task_id: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(task_file(storage, session, task_id, "json")).unwrap())
        .unwrap()
}

#[test]
fn spawn_detached_survives_parent_restart() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");

    let task_id = {
        let mut aft = AftProcess::spawn();
        configure_background(&mut aft, project.path(), storage.path(), SESSION);
        let task_id = spawn_bg(&mut aft, SESSION, "sleep 1", None);
        assert!(aft.shutdown().success());
        task_id
    };

    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);
    let running = status(&mut aft, SESSION, &task_id);
    assert_eq!(
        running["success"], true,
        "task was not rehydrated: {running:?}"
    );
    assert_eq!(running["status"], "running");

    let completed = wait_for_status(&mut aft, SESSION, &task_id, "completed");
    assert_eq!(completed["exit_code"], 0);
    assert!(aft.shutdown().success());
}

#[test]
fn exit_file_atomicity_many_short_tasks() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);

    let task_ids = (0..12)
        .map(|_| spawn_bg(&mut aft, SESSION, "true", None))
        .collect::<Vec<_>>();

    for task_id in &task_ids {
        let exit_path = task_file(storage.path(), SESSION, task_id, "exit");
        let started = Instant::now();
        loop {
            if exit_path.exists() {
                let content = fs::read_to_string(&exit_path).unwrap();
                assert_eq!(
                    content.trim(),
                    "0",
                    "partial exit marker for {task_id}: {content:?}"
                );
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(4));
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    assert!(aft.shutdown().success());
}

#[test]
fn pre_spawn_metadata_starting_replays_as_failed() {
    let storage = tempfile::tempdir().unwrap();
    let task_id = "bgb-starting";
    let metadata = PersistedTask::starting(
        task_id.to_string(),
        SESSION.to_string(),
        "true".to_string(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
    );
    let path = task_file(storage.path(), SESSION, task_id, "json");
    write_task(&path, &metadata).unwrap();

    let registry = BgTaskRegistry::new();
    registry.replay_session(storage.path(), SESSION).unwrap();
    let replayed = read_json(storage.path(), SESSION, task_id);
    assert_eq!(replayed["status"], "failed");
    assert_eq!(replayed["status_reason"], "spawn aborted");
    let completions = registry.drain_completions_for_session(Some(SESSION));
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].status, BgTaskStatus::Failed);
}

#[test]
fn terminal_state_monotonic_killed_wins_late_exit_file() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);
    let task_id = spawn_bg(&mut aft, SESSION, "sleep 5", None);

    let killed = aft.send(
        &json!({
            "id": "kill-monotonic",
            "session_id": SESSION,
            "command": "bash_kill",
            "params": { "task_id": task_id }
        })
        .to_string(),
    );
    assert_eq!(killed["status"], "killed");
    fs::write(task_file(storage.path(), SESSION, &task_id, "exit"), "0").unwrap();

    let after = status(&mut aft, SESSION, &task_id);
    assert_eq!(after["status"], "killed");
    assert_eq!(
        read_json(storage.path(), SESSION, &task_id)["status"],
        "killed"
    );
    assert!(aft.shutdown().success());
}

#[test]
fn completion_durability_replays_undelivered_terminal_task() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let task_id = {
        let mut aft = AftProcess::spawn();
        configure_background(&mut aft, project.path(), storage.path(), SESSION);
        let task_id = spawn_bg(&mut aft, SESSION, "echo durable", None);
        let _ = wait_for_status(&mut aft, SESSION, &task_id, "completed");
        assert_eq!(
            read_json(storage.path(), SESSION, &task_id)["completion_delivered"],
            false
        );
        assert!(aft.shutdown().success());
        task_id
    };

    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);
    let drained = drain(&mut aft, SESSION);
    assert!(drained["bg_completions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|completion| completion["task_id"] == task_id));
    assert_eq!(
        read_json(storage.path(), SESSION, &task_id)["completion_delivered"],
        true
    );
    assert!(aft.shutdown().success());
}

#[test]
fn kill_marker_idempotency_terminal_and_racy_exit() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);

    let done = spawn_bg(&mut aft, SESSION, "true", None);
    let completed = wait_for_status(&mut aft, SESSION, &done, "completed");
    let killed_done = aft.send(
        &json!({"id":"kill-done","session_id":SESSION,"command":"bash_kill","params":{"task_id":done}})
            .to_string(),
    );
    assert_eq!(killed_done["status"], completed["status"]);

    let racy = spawn_bg(&mut aft, SESSION, "sleep 5", None);
    fs::write(task_file(storage.path(), SESSION, &racy, "exit"), "0").unwrap();
    let killed = aft.send(
        &json!({"id":"kill-racy","session_id":SESSION,"command":"bash_kill","params":{"task_id":racy}})
            .to_string(),
    );
    assert_eq!(killed["success"], true);
    assert_eq!(
        fs::read_to_string(task_file(storage.path(), SESSION, &racy, "exit")).unwrap(),
        "0"
    );
    assert!(aft.shutdown().success());
}

#[test]
fn disk_read_tail_does_not_truncate_live_file() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);
    let command = "for i in $(seq 1 200); do printf '%01024d' 0; sleep 0.01; done";
    let task_id = spawn_bg(&mut aft, SESSION, command, None);
    let stdout_path = task_file(storage.path(), SESSION, &task_id, "stdout");

    std::thread::sleep(Duration::from_millis(600));
    let before = fs::metadata(&stdout_path).unwrap().len();
    let snapshot = status(&mut aft, SESSION, &task_id);
    assert!(snapshot["output_preview"].as_str().unwrap().len() > 0);
    std::thread::sleep(Duration::from_millis(600));
    let after = fs::metadata(&stdout_path).unwrap().len();
    assert!(
        after > before,
        "live stdout did not keep growing after tail read: {before}->{after}"
    );
    let _ = wait_for_status(&mut aft, SESSION, &task_id, "completed");
    assert!(aft.shutdown().success());
}

#[test]
fn watchdog_deadline_enforcement_without_status_query() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let mut aft = AftProcess::spawn();
    configure_background(&mut aft, project.path(), storage.path(), SESSION);
    let task_id = spawn_bg(&mut aft, SESSION, "sleep 5", Some(1000));
    std::thread::sleep(Duration::from_millis(1800));
    let timed_out = status(&mut aft, SESSION, &task_id);
    assert_eq!(
        timed_out["status"], "timed_out",
        "watchdog did not time out task: {timed_out:?}"
    );
    assert_eq!(timed_out["exit_code"], 124);
    assert!(aft.shutdown().success());
}

#[test]
fn session_isolation_on_replay() {
    let project = tempfile::tempdir().unwrap();
    let storage = spawn_storage_dir("storage");
    let mut aft_a = AftProcess::spawn();
    configure_background(&mut aft_a, project.path(), storage.path(), "session-a");
    let task_id = spawn_bg(&mut aft_a, "session-a", "sleep 1", None);
    assert!(aft_a.shutdown().success());

    let mut aft_b = AftProcess::spawn();
    configure_background(&mut aft_b, project.path(), storage.path(), "session-b");
    let missing = status(&mut aft_b, "session-b", &task_id);
    assert_eq!(missing["success"], false);
    assert!(aft_b.shutdown().success());

    let mut aft_a2 = AftProcess::spawn();
    configure_background(&mut aft_a2, project.path(), storage.path(), "session-a");
    assert_eq!(status(&mut aft_a2, "session-a", &task_id)["success"], true);
    let _ = wait_for_status(&mut aft_a2, "session-a", &task_id, "completed");
    assert!(aft_a2.shutdown().success());
}

#[test]
fn replay_stale_running_task_marks_killed_orphaned() {
    let storage = tempfile::tempdir().unwrap();
    let task_id = "bgb-stale";
    let mut metadata = PersistedTask::starting(
        task_id.to_string(),
        SESSION.to_string(),
        "sleep 99".to_string(),
        tempfile::tempdir().unwrap().path().to_path_buf(),
        None,
    );
    metadata.status = BgTaskStatus::Running;
    metadata.started_at = metadata.started_at.saturating_sub(25 * 60 * 60 * 1000);
    metadata.child_pid = Some(999_999);
    metadata.pgid = Some(999_999);
    write_task(
        &task_file(storage.path(), SESSION, task_id, "json"),
        &metadata,
    )
    .unwrap();

    let registry = BgTaskRegistry::new();
    registry.replay_session(storage.path(), SESSION).unwrap();
    let replayed = read_json(storage.path(), SESSION, task_id);
    assert_eq!(replayed["status"], "killed");
    assert_eq!(replayed["status_reason"], "orphaned (>24h)");
}
