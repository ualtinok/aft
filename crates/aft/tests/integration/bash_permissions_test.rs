use super::helpers::AftProcess;

use serde_json::json;
use tempfile::TempDir;

fn configure(aft: &mut AftProcess, root: &TempDir) {
    let response = aft.send(
        &serde_json::to_string(&json!({
            "id": "cfg",
            "command": "configure",
            "project_root": root.path(),
            "bash_permissions": true,
        }))
        .unwrap(),
    );
    assert_eq!(response["success"], true, "configure failed: {response:?}");
}

fn bash(aft: &mut AftProcess, id: &str, command: &str) -> serde_json::Value {
    aft.send(
        &serde_json::to_string(&json!({
            "id": id,
            "method": "bash",
            "params": {
                "command": command,
                "permissions_requested": true,
            },
        }))
        .unwrap(),
    )
}

#[test]
fn simple_echo_has_no_permission_asks() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let frames = aft.send_until(
        r#"{"id":"echo","method":"bash","params":{"command":"echo hello","permissions_requested":true}}"#,
        |value| value["id"] == "echo",
    );
    let response = frames.last().unwrap();
    assert_eq!(response["success"], true, "response: {response:?}");
    assert_eq!(response["output"], "hello\n");

    assert!(aft.shutdown().success());
}

#[test]
fn rm_outside_project_root_requires_external_directory() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = bash(&mut aft, "rm", "rm /tmp/foo.txt");
    assert_eq!(response["success"], false, "response: {response:?}");
    assert_eq!(response["code"], "permission_required");
    assert!(response["asks"].as_array().unwrap().iter().any(|ask| {
        ask["kind"] == "external_directory"
            && ask["patterns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "/tmp/*")
    }));

    assert!(aft.shutdown().success());
}

#[test]
fn chained_cd_then_rm_uses_subcommand_directory() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = bash(&mut aft, "chain", "cd /tmp && rm foo");
    assert_eq!(response["success"], false, "response: {response:?}");
    assert!(response["asks"].as_array().unwrap().iter().any(|ask| {
        ask["kind"] == "external_directory"
            && ask["patterns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "/tmp/*")
    }));

    assert!(aft.shutdown().success());
}

#[test]
fn git_status_returns_bash_ask_with_stable_prefix() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = bash(&mut aft, "git", "git status");
    assert_eq!(response["success"], false, "response: {response:?}");
    let asks = response["asks"].as_array().unwrap();
    assert!(asks.iter().any(|ask| {
        ask["kind"] == "bash"
            && ask["patterns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "git status")
            && ask["always"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p == "git status *")
    }));

    assert!(aft.shutdown().success());
}

#[test]
fn pipe_returns_asks_for_each_subcommand() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = bash(&mut aft, "pipe", "find . | xargs grep foo");
    assert_eq!(response["success"], false, "response: {response:?}");
    let asks = response["asks"].as_array().unwrap();
    assert!(asks.iter().any(|ask| ask["kind"] == "bash"
        && ask["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "find .")));
    assert!(asks.iter().any(|ask| ask["kind"] == "bash"
        && ask["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "grep foo")));

    assert!(aft.shutdown().success());
}

#[test]
fn granted_permission_allows_git_status_short() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = aft.send(
        &serde_json::to_string(&json!({
            "id": "grant",
            "method": "bash",
            "params": {
                "command": "git status --short",
                "permissions_requested": true,
                "permissions_granted": ["git status *"],
            },
        }))
        .unwrap(),
    );
    assert_ne!(
        response["code"], "permission_required",
        "response: {response:?}"
    );

    assert!(aft.shutdown().success());
}
