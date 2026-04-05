use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::helpers::AftProcess;

fn setup_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().expect("create temp dir");

    for (relative_path, content) in files {
        let path = temp_dir.path().join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(path, content).expect("write fixture file");
    }

    temp_dir
}

fn configure(aft: &mut AftProcess, root: &Path) {
    let resp = aft.configure(root);
    assert_eq!(resp["success"], true, "configure should succeed: {resp:?}");
}

fn configure_with_index(aft: &mut AftProcess, root: &Path) {
    let resp = aft.send(&format!(
        r#"{{"id":"cfg-index","command":"configure","project_root":"{}","experimental_search_index":true}}"#,
        root.display()
    ));
    assert_eq!(resp["success"], true, "configure should succeed: {resp:?}");
}

fn configure_with_compression(aft: &mut AftProcess, root: &Path, compress_tool_output: bool) {
    let resp = send(
        aft,
        json!({
            "id": "cfg-compression",
            "command": "configure",
            "project_root": root.display().to_string(),
            "compress_tool_output": compress_tool_output,
        }),
    );
    assert_eq!(resp["success"], true, "configure should succeed: {resp:?}");
}

fn send(aft: &mut AftProcess, request: Value) -> Value {
    aft.send(&serde_json::to_string(&request).expect("serialize request"))
}

fn canonical_path_string(path: &Path) -> String {
    fs::canonicalize(path)
        .expect("canonicalize path")
        .display()
        .to_string()
}

#[test]
fn grep_fallback_returns_relative_paths_and_counts() {
    let project = setup_project(&[
        ("src/one.rs", "fn alpha() { println!(\"alpha\"); }\n"),
        ("src/two.rs", "fn beta() { println!(\"alpha\"); }\n"),
        ("notes.txt", "alpha beta gamma\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let response = send(
        &mut aft,
        json!({
            "id": "grep-fallback",
            "command": "grep",
            "pattern": r#""alpha""#,
            "include": ["src/**/*.rs"],
        }),
    );

    assert_eq!(
        response["success"], true,
        "grep should succeed: {response:?}"
    );
    assert_eq!(response["index_status"], "Fallback");
    assert_eq!(response["total_matches"], 2);
    assert_eq!(response["files_with_matches"], 2);
    assert_eq!(response["files_searched"], 2);

    let matches = response["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["line"], 1);
    assert!(matches[0]["column"].as_u64().unwrap_or(0) >= 1);
    // Files are returned as absolute paths
    let file_path = matches[0]["file"].as_str().expect("file path");
    assert!(file_path.contains("src/one.rs") || file_path.contains("src/two.rs"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn glob_fallback_respects_gitignore_and_returns_absolute_paths() {
    let project = setup_project(&[
        ("src/keep.rs", "fn keep() {}\n"),
        ("src/skip.ts", "const skip = true;\n"),
        ("ignored.log", "secret\n"),
        (".gitignore", "*.log\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let response = send(
        &mut aft,
        json!({
            "id": "glob-fallback",
            "command": "glob",
            "pattern": "src/**/*.rs",
        }),
    );

    assert_eq!(
        response["success"], true,
        "glob should succeed: {response:?}"
    );
    assert_eq!(response["total"], 1);
    let files = response["files"].as_array().expect("files array");
    assert_eq!(
        files,
        &vec![Value::String(canonical_path_string(
            &project.path().join("src/keep.rs")
        ))]
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn grep_uses_index_when_configured() {
    let project = setup_project(&[
        ("src/search.rs", "fn search() { println!(\"needle\"); }\n"),
        ("src/other.rs", "fn other() { println!(\"haystack\"); }\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure_with_index(&mut aft, project.path());

    let mut ready_response = None;
    for _ in 0..20 {
        let response = send(
            &mut aft,
            json!({
                "id": "grep-indexed",
                "command": "grep",
                "pattern": "needle",
                "include": ["src/**/*.rs"],
            }),
        );

        if response["index_status"] == "Ready" {
            ready_response = Some(response);
            break;
        }

        thread::sleep(Duration::from_millis(50));
    }

    let response = ready_response.expect("search index should become ready");
    assert_eq!(
        response["success"], true,
        "indexed grep should succeed: {response:?}"
    );
    assert_eq!(response["index_status"], "Ready");
    assert_eq!(response["total_matches"], 1);
    assert_eq!(response["files_with_matches"], 1);
    assert_eq!(response["files_searched"], 1);
    // Files are returned as absolute paths
    let expected_path = canonical_path_string(&project.path().join("src/search.rs"));
    assert_eq!(response["matches"][0]["file"], expected_path);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn grep_text_is_compressed_by_default() {
    let project = setup_project(&[
        ("src/one.rs", "fn alpha() { println!(\"alpha\"); }\n"),
        ("src/two.rs", "fn beta() { println!(\"alpha\"); }\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let response = send(
        &mut aft,
        json!({
            "id": "grep-compressed",
            "command": "grep",
            "pattern": r#""alpha""#,
            "include": ["src/**/*.rs"],
        }),
    );

    assert_eq!(
        response["success"], true,
        "grep should succeed: {response:?}"
    );
    let text = response["text"].as_str().expect("grep text");
    // Compressed format: decorative headers with absolute paths + summary footer
    assert!(text.contains("(1 match) ──"));
    assert!(text.contains("src/one.rs"));
    assert!(text.contains("src/two.rs"));
    assert!(text.ends_with("Found 2 match(es) across 2 file(s). [index: fallback]"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn grep_text_can_disable_compression() {
    let project = setup_project(&[("src/one.rs", "fn alpha() { println!(\"alpha\"); }\n")]);
    let mut aft = AftProcess::spawn();
    configure_with_compression(&mut aft, project.path(), false);

    let response = send(
        &mut aft,
        json!({
            "id": "grep-raw",
            "command": "grep",
            "pattern": r#""alpha""#,
            "include": ["src/**/*.rs"],
        }),
    );

    assert_eq!(
        response["success"], true,
        "grep should succeed: {response:?}"
    );
    // Raw grep format: "Found N matches" header + absolute path + plain file: format
    let expected_path = canonical_path_string(&project.path().join("src/one.rs"));
    let expected_text = format!("Found 1 matches\n{}:\n  Line 1: fn alpha() {{ println!(\"alpha\"); }}", expected_path);
    assert_eq!(response["text"], Value::String(expected_text));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn glob_text_is_compressed_by_default() {
    let project = setup_project(&[
        ("src/keep.rs", "fn keep() {}\n"),
        ("src/other.rs", "fn other() {}\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let response = send(
        &mut aft,
        json!({
            "id": "glob-compressed",
            "command": "glob",
            "pattern": "src/**/*.rs",
        }),
    );

    assert_eq!(
        response["success"], true,
        "glob should succeed: {response:?}"
    );
    assert_eq!(
        response["text"],
        Value::String(format!(
            "2 files matching src/**/*.rs\n\n{}\n{}",
            canonical_path_string(&project.path().join("src/other.rs")),
            canonical_path_string(&project.path().join("src/keep.rs"))
        ))
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn glob_text_can_disable_compression() {
    let project = setup_project(&[
        ("src/keep.rs", "fn keep() {}\n"),
        ("src/other.rs", "fn other() {}\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure_with_compression(&mut aft, project.path(), false);

    let response = send(
        &mut aft,
        json!({
            "id": "glob-raw",
            "command": "glob",
            "pattern": "src/**/*.rs",
        }),
    );

    assert_eq!(
        response["success"], true,
        "glob should succeed: {response:?}"
    );
    assert_eq!(
        response["text"],
        Value::String(format!(
            "{}\n{}",
            canonical_path_string(&project.path().join("src/other.rs")),
            canonical_path_string(&project.path().join("src/keep.rs"))
        ))
    );

    let status = aft.shutdown();
    assert!(status.success());
}
