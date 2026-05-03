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

fn configure_path(aft: &mut AftProcess, root: &std::path::Path) {
    let response = aft.send(
        &serde_json::to_string(&json!({
            "id": "cfg",
            "command": "configure",
            "project_root": root,
            "bash_permissions": true,
        }))
        .unwrap(),
    );
    assert_eq!(response["success"], true, "configure failed: {response:?}");
}

#[cfg(unix)]
fn create_dir_symlink(src: &std::path::Path, dst: &std::path::Path) {
    std::os::unix::fs::symlink(src, dst).expect("create symlink");
}

#[cfg(windows)]
fn create_dir_symlink(src: &std::path::Path, dst: &std::path::Path) {
    std::os::windows::fs::symlink_dir(src, dst).expect("create symlink");
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
    assert!(
        response["asks"].as_array().unwrap().iter().any(|ask| {
            ask["kind"] == "external_directory"
                && ask["patterns"].as_array().unwrap().iter().any(|p| {
                    p.as_str()
                        .is_some_and(|p| p.contains("tmp/") && p.ends_with("/*"))
                })
        }),
        "response: {response:?}"
    );

    assert!(aft.shutdown().success());
}

#[test]
fn symlink_path_resolving_outside_project_requires_permission() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("project");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    create_dir_symlink(&outside, &root.join("link"));
    std::fs::write(outside.join("secret.txt"), "secret").unwrap();

    let mut aft = AftProcess::spawn();
    configure_path(&mut aft, &root);

    let response = bash(&mut aft, "symlink-outside", "rm link/secret.txt");
    assert_eq!(response["success"], false, "response: {response:?}");
    assert_eq!(response["code"], "permission_required");
    assert!(response["asks"].as_array().unwrap().iter().any(|ask| {
        ask["kind"] == "external_directory"
            && ask["patterns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p.as_str().is_some_and(|p| p.contains("outside")))
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
    assert!(
        response["asks"].as_array().unwrap().iter().any(|ask| {
            ask["kind"] == "external_directory"
                && ask["patterns"].as_array().unwrap().iter().any(|p| {
                    p.as_str()
                        .is_some_and(|p| p.contains("tmp/") && p.ends_with("/*"))
                })
        }),
        "response: {response:?}"
    );

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

#[test]
fn background_command_requires_permission_before_spawn() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = aft.send(
        &serde_json::to_string(&json!({
            "id": "bg-permission",
            "method": "bash",
            "params": {
                "command": "rm /tmp/aft-background-permission-test.txt",
                "permissions_requested": true,
                "background": true,
            },
        }))
        .unwrap(),
    );

    assert_eq!(response["success"], false, "response: {response:?}");
    assert_eq!(response["code"], "permission_required");
    assert!(response.get("task_id").is_none(), "response: {response:?}");

    assert!(aft.shutdown().success());
}

#[test]
fn malformed_bash_requires_full_permission_prompt() {
    let root = TempDir::new().unwrap();
    let mut aft = AftProcess::spawn();
    configure(&mut aft, &root);

    let response = bash(&mut aft, "malformed", "echo 'unterminated");
    assert_eq!(response["success"], false, "response: {response:?}");
    assert_eq!(response["code"], "permission_required");
    let asks = response["asks"].as_array().unwrap();
    assert!(asks.iter().any(|ask| {
        ask["kind"] == "bash" && ask["patterns"].as_array().unwrap().iter().any(|p| p == "*")
    }));

    assert!(aft.shutdown().success());
}
