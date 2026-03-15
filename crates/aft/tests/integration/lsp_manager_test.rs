use std::fs;
use std::path::PathBuf;

use aft::lsp::client::ServerState;
use aft::lsp::manager::LspManager;
use aft::lsp::registry::ServerKind;
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

#[test]
fn test_manager_spawns_server_on_first_touch() {
    let (_temp_dir, main_rs, _lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    let keys = manager.ensure_server_for_file(&main_rs);

    assert_eq!(keys.len(), 1);
    assert_eq!(manager.active_client_count(), 1);
    let client = manager.client_for_file(&main_rs).expect("missing client");
    assert_eq!(client.kind(), ServerKind::Rust);
    assert_eq!(client.state(), ServerState::Ready);
}

#[test]
fn test_manager_reuses_existing_server() {
    let (_temp_dir, main_rs, lib_rs) = rust_fixture_files();
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());

    let first = manager.ensure_server_for_file(&main_rs);
    let second = manager.ensure_server_for_file(&lib_rs);

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

    manager.ensure_server_for_file(&main_rs);
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

    manager.ensure_server_for_file(&main_rs);

    let client = manager.client_for_file(&main_rs).expect("missing client");
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

    let keys = manager.ensure_server_for_file(&main_rs);

    assert!(keys.is_empty());
    assert_eq!(manager.active_client_count(), 0);
    assert!(manager.client_for_file(&main_rs).is_none());
}
