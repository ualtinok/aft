use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

mod aft {
    pub mod search_index {
        use std::fs;
        use std::path::Path;

        use sha2::{Digest, Sha256};

        pub fn project_cache_key(project_root: &Path) -> String {
            let canonical_root =
                fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
            let mut hasher = Sha256::new();
            hasher.update(canonical_root.to_string_lossy().as_bytes());
            let digest = format!("{:x}", hasher.finalize());
            digest[..16].to_string()
        }
    }
}

use aft::search_index::project_cache_key;
use serde_json::{json, Value};

use crate::helpers::AftProcess;

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

fn send(aft: &mut AftProcess, request: Value) -> Value {
    aft.send(&serde_json::to_string(&request).expect("serialize request"))
}

fn configure_semantic(
    aft: &mut AftProcess,
    root: &Path,
    storage_dir: &Path,
    enabled: bool,
) -> Value {
    send(
        aft,
        json!({
            "id": "cfg-semantic",
            "command": "configure",
            "project_root": root.display().to_string(),
            "semantic_search": enabled,
            "storage_dir": storage_dir.display().to_string(),
        }),
    )
}

fn wait_for_ready_search(aft: &mut AftProcess, query: &str) -> Value {
    for _ in 0..180 {
        let response = send(
            aft,
            json!({
                "id": "semantic-search",
                "command": "semantic_search",
                "query": query,
                "top_k": 5,
            }),
        );

        assert_eq!(
            response["success"], true,
            "semantic_search should succeed while polling: {response:?}"
        );

        if response["status"] == "ready" {
            return response;
        }

        thread::sleep(Duration::from_millis(250));
    }

    panic!("semantic index did not become ready in time");
}

#[test]
fn semantic_search_returns_not_ready_without_an_index() {
    let mut aft = AftProcess::spawn();

    let response = send(
        &mut aft,
        json!({
            "id": "semantic-not-ready",
            "command": "semantic_search",
            "query": "request handling",
        }),
    );

    assert_eq!(
        response["success"], true,
        "search should succeed: {response:?}"
    );
    assert_eq!(response["status"], "disabled");
    assert_eq!(response["text"], "Semantic search is not enabled.");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn semantic_search_returns_disabled_when_feature_is_off() {
    let project = setup_project(&[("src/lib.rs", "pub fn handle_request() -> bool { true }\n")]);
    let storage = tempfile::tempdir().expect("create storage dir");
    let mut aft = AftProcess::spawn();

    let configure = configure_semantic(&mut aft, project.path(), storage.path(), false);
    assert_eq!(
        configure["success"], true,
        "configure should succeed: {configure:?}"
    );

    let response = send(
        &mut aft,
        json!({
            "id": "semantic-disabled",
            "command": "semantic_search",
            "query": "request handling",
        }),
    );

    assert_eq!(
        response["success"], true,
        "search should succeed: {response:?}"
    );
    assert_eq!(response["status"], "disabled");
    assert_eq!(response["text"], "Semantic search is not enabled.");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
#[ignore = "requires fastembed model download (~22MB) and a full semantic index build"]
fn semantic_index_persists_across_configure_build_search_roundtrip() {
    let project = setup_project(&[
        (
            "src/lib.rs",
            "pub fn handle_request(token: &str) -> bool {\n    !token.is_empty()\n}\n\npub struct AuthService;\n",
        ),
        (
            "src/utils.rs",
            "pub fn normalize_user_id(input: &str) -> String {\n    input.trim().to_lowercase()\n}\n",
        ),
    ]);
    let storage = tempfile::tempdir().expect("create storage dir");
    let project_key = project_cache_key(project.path());
    let semantic_file = storage
        .path()
        .join("semantic")
        .join(&project_key)
        .join("semantic.bin");

    // Slow by design: this may download the embedding model on first use.
    let mut first = AftProcess::spawn();
    let configure = configure_semantic(&mut first, project.path(), storage.path(), true);
    assert_eq!(
        configure["success"], true,
        "configure should succeed: {configure:?}"
    );

    let first_response = wait_for_ready_search(&mut first, "request authentication handler");
    assert_eq!(first_response["status"], "ready");
    assert!(
        semantic_file.is_file(),
        "semantic index should persist to disk"
    );

    let first_results = first_response["results"]
        .as_array()
        .expect("semantic results array");
    assert!(
        !first_results.is_empty(),
        "expected at least one semantic result"
    );
    assert_eq!(first_results[0]["name"], "handle_request");

    let status = first.shutdown();
    assert!(status.success());

    let mut second = AftProcess::spawn();
    let configure = configure_semantic(&mut second, project.path(), storage.path(), true);
    assert_eq!(
        configure["success"], true,
        "configure should succeed: {configure:?}"
    );

    let second_response = wait_for_ready_search(&mut second, "request authentication handler");
    assert_eq!(second_response["status"], "ready");
    assert_eq!(second_response["text"], first_response["text"]);
    assert_eq!(second_response["results"], first_response["results"]);

    let status = second.shutdown();
    assert!(status.success());
}
