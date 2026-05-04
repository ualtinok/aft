use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use aft::config::{Config, UserServerDef};
use aft::lsp::client::{LspEvent, ServerState};
use aft::lsp::manager::{LspManager, ServerAttemptResult};
use aft::lsp::registry::ServerKind;
use serde_json::json;
use tempfile::tempdir;

fn fake_server_path() -> PathBuf {
    option_env!("CARGO_BIN_EXE_fake-lsp-server")
        .or(option_env!("CARGO_BIN_EXE_fake_lsp_server"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_fake-lsp-server").map(PathBuf::from))
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_fake_lsp_server").map(PathBuf::from))
        .or_else(|| {
            let mut path = std::env::current_exe().ok()?;
            path.pop();
            path.pop();
            path.push("fake-lsp-server");
            Some(path)
        })
        .filter(|path| path.exists())
        .expect("fake-lsp-server binary path not set")
}

fn rust_fixture_files() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path().join("workspace");
    let src_dir = root.join("src");
    let main_rs = src_dir.join("main.rs");
    let lib_rs = src_dir.join("lib.rs");

    fs::create_dir_all(&src_dir).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(&main_rs, "fn main() {}\n").unwrap();
    fs::write(&lib_rs, "pub fn answer() -> u32 { 42 }\n").unwrap();

    (temp_dir, main_rs, lib_rs)
}

fn collect_notification(manager: &mut LspManager, method: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        for event in manager.drain_events() {
            if let LspEvent::Notification {
                method: event_method,
                params,
                ..
            } = event
            {
                if event_method == method {
                    return params.expect("notification params");
                }
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {method}");
}

#[test]
fn test_manager_spawns_server_on_first_touch() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    let keys = manager.ensure_server_for_file_default(&main_rs);

    assert_eq!(keys.len(), 1);
    assert_eq!(manager.active_client_count(), 1);
    let client = manager
        .client_for_file_default(&main_rs)
        .expect("missing client");
    assert_eq!(client.kind(), ServerKind::Rust);
    assert_eq!(client.state(), ServerState::Ready);
}

#[test]
fn test_manager_reuses_existing_server() {
    let (_temp_dir, main_rs, lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    let first = manager.ensure_server_for_file_default(&main_rs);
    let second = manager.ensure_server_for_file_default(&lib_rs);

    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1);
    assert_eq!(first[0], second[0]);
    assert_eq!(manager.active_client_count(), 1);
}

#[test]
fn test_manager_shutdown_all() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    manager.ensure_server_for_file_default(&main_rs);
    assert_eq!(manager.active_client_count(), 1);

    manager.shutdown_all();

    assert_eq!(manager.active_client_count(), 0);
    assert!(!manager.has_active_servers());
}

#[test]
fn test_server_lifecycle_states() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    manager.ensure_server_for_file_default(&main_rs);

    let client = manager
        .client_for_file_default(&main_rs)
        .expect("missing client");
    assert_eq!(client.state(), ServerState::Ready);
}

#[test]
fn test_manager_handles_missing_binary() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(
        ServerKind::Rust,
        PathBuf::from("/definitely/missing/fake-lsp-server"),
    );

    let keys = manager.ensure_server_for_file_default(&main_rs);

    assert!(keys.is_empty());
    assert_eq!(manager.active_client_count(), 0);
    assert!(manager.client_for_file_default(&main_rs).is_none());
}

#[test]
fn test_custom_server_env_and_initialization_options_reach_spawned_server() {
    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path().join("workspace");
    let main_typ = root.join("main.typ");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("typst.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(&main_typ, "= Demo\n").unwrap();

    let mut env = HashMap::new();
    env.insert("AFT_TEST_LSP_ENV".to_string(), "from-config".to_string());
    let config = Config {
        lsp_servers: vec![UserServerDef {
            id: "tinymist".to_string(),
            extensions: vec!["typ".to_string()],
            binary: "tinymist".to_string(),
            args: Vec::new(),
            root_markers: vec!["typst.toml".to_string()],
            env,
            initialization_options: Some(json!({
                "exportPdf": "never",
                "nested": { "enabled": true }
            })),
            disabled: false,
        }],
        ..Config::default()
    };

    let mut manager = LspManager::new();
    manager.override_binary(
        ServerKind::Custom(Arc::from("tinymist")),
        fake_server_path(),
    );

    let keys = manager.ensure_server_for_file(&main_typ, &config);
    assert_eq!(keys.len(), 1);
    assert_eq!(manager.active_client_count(), 1);

    let initialized = collect_notification(&mut manager, "custom/initialized");
    assert_eq!(initialized["env"]["AFT_TEST_LSP_ENV"], "from-config");
    assert_eq!(initialized["initializationOptions"]["exportPdf"], "never");
    assert_eq!(
        initialized["initializationOptions"]["nested"]["enabled"],
        true
    );
}

#[test]
fn watched_file_capability_defaults_permissive_when_initialize_has_no_field() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let config = Config::default();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    let keys = manager.ensure_server_for_file(&main_rs, &config);
    assert_eq!(keys.len(), 1);

    let client = manager.client_for_file(&main_rs, &config).expect("client");
    assert!(
        client.supports_watched_files(),
        "missing explicit didChangeWatchedFiles capability should default to true"
    );
}

// ---------------------------------------------------------------------------
// Failed-spawn dedup tests
//
// Regression: before v0.19.1, every file open / didChange / lsp_diagnostics
// call retried `spawn_server` for a (kind, root) pair that had already failed
// once. typescript-language-server failing on "Could not find a valid
// TypeScript installation" produced a fresh ERROR log per request.
//
// The fix caches the classified spawn-failure result per `(kind, root)` and
// skips re-spawn attempts. These tests pin that contract.
// ---------------------------------------------------------------------------

#[test]
fn failed_spawn_is_cached_and_not_retried_on_second_call() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    // Override with a binary that doesn't exist → spawn classifies as
    // BinaryNotInstalled.
    manager.override_binary(
        ServerKind::Rust,
        PathBuf::from("/definitely/missing/fake-lsp-server-XYZZY"),
    );

    // First call — must produce a BinaryNotInstalled attempt.
    let first = manager.ensure_server_for_file_detailed(&main_rs, &Config::default());
    assert_eq!(first.attempts.len(), 1);
    let first_result = &first.attempts[0].result;
    assert!(
        matches!(first_result, ServerAttemptResult::BinaryNotInstalled { .. })
            || matches!(first_result, ServerAttemptResult::SpawnFailed { .. }),
        "first call should classify as BinaryNotInstalled or SpawnFailed, got {first_result:?}"
    );
    assert_eq!(manager.active_client_count(), 0);

    // Second call — must return the SAME cached classification, no new spawn.
    let second = manager.ensure_server_for_file_detailed(&main_rs, &Config::default());
    assert_eq!(second.attempts.len(), 1);
    let second_result = &second.attempts[0].result;
    // The cached result is cloned, so the variant must match the first call's.
    assert_eq!(
        std::mem::discriminant(first_result),
        std::mem::discriminant(second_result),
        "cached failure must replay with the same variant"
    );
    assert_eq!(manager.active_client_count(), 0);
}

#[test]
fn failed_spawn_dedup_persists_across_many_calls() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(
        ServerKind::Rust,
        PathBuf::from("/definitely/missing/fake-lsp-server-XYZZY"),
    );

    // Simulate the production case: many file events in a row. Without dedup,
    // each one would log a fresh ERROR and try to spawn the missing binary.
    for _ in 0..10 {
        let outcome = manager.ensure_server_for_file_detailed(&main_rs, &Config::default());
        assert_eq!(outcome.attempts.len(), 1);
        assert_eq!(manager.active_client_count(), 0);
    }
}

#[test]
fn failed_spawn_for_one_root_does_not_block_a_different_root() {
    // The cache key is (ServerKind, workspace_root). A failed spawn for
    // workspace A must NOT prevent spawn attempts for an unrelated workspace
    // B — they're independent server instances.
    let (_temp_dir_a, main_rs_a, _lib_rs_a) = rust_fixture_files();
    let (_temp_dir_b, main_rs_b, _lib_rs_b) = rust_fixture_files();
    let mut manager = LspManager::new();

    // First override: missing binary → workspace A fails.
    manager.override_binary(
        ServerKind::Rust,
        PathBuf::from("/definitely/missing/fake-lsp-server-XYZZY"),
    );
    let outcome_a = manager.ensure_server_for_file_detailed(&main_rs_a, &Config::default());
    assert_eq!(outcome_a.successful.len(), 0);
    assert_eq!(manager.active_client_count(), 0);

    // Now point to a working binary. Workspace A is still cached as failed
    // (we don't auto-recover at runtime — the user has to fix env + restart),
    // but workspace B should spawn cleanly.
    manager.override_binary(ServerKind::Rust, fake_server_path());

    // Workspace A still returns the cached failure (no retry on a different
    // binary path — the cache deliberately survives override changes mid-session
    // because runtime overrides are a test-only feature).
    let outcome_a_again = manager.ensure_server_for_file_detailed(&main_rs_a, &Config::default());
    assert_eq!(outcome_a_again.successful.len(), 0);

    // Workspace B is a fresh (kind, root) pair → not in the failed cache → spawns.
    let outcome_b = manager.ensure_server_for_file_detailed(&main_rs_b, &Config::default());
    assert_eq!(outcome_b.successful.len(), 1);
    assert_eq!(manager.active_client_count(), 1);
}
