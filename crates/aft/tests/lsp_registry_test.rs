use std::fs;

use aft::lsp::registry::{servers_for_file, ServerKind};
use aft::lsp::roots::find_workspace_root;
use tempfile::tempdir;

#[test]
fn test_deepest_root_wins_in_monorepo() {
    let temp_dir = tempdir().unwrap();
    let monorepo_root = temp_dir.path().join("monorepo");
    let crate_root = monorepo_root.join("crates").join("my-crate");
    let src_dir = crate_root.join("src");
    let main_rs = src_dir.join("main.rs");

    fs::create_dir_all(&src_dir).unwrap();
    fs::write(monorepo_root.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(
        crate_root.join("Cargo.toml"),
        "[package]\nname = \"my-crate\"\n",
    )
    .unwrap();
    fs::write(&main_rs, "fn main() {}\n").unwrap();

    let expected_root = fs::canonicalize(&crate_root).unwrap();
    assert_eq!(
        find_workspace_root(&main_rs, &["Cargo.toml"]),
        Some(expected_root)
    );
}

#[test]
fn test_typescript_root_with_tsconfig() {
    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path().join("web");
    let src_dir = root.join("src");
    let file = src_dir.join("index.ts");

    fs::create_dir_all(&src_dir).unwrap();
    fs::write(root.join("tsconfig.json"), "{}\n").unwrap();
    fs::write(&file, "export const value = 1;\n").unwrap();

    let expected_root = fs::canonicalize(&root).unwrap();
    assert_eq!(
        find_workspace_root(&file, &["tsconfig.json", "package.json"]),
        Some(expected_root)
    );
}

#[test]
fn test_registry_and_root_combined() {
    let temp_dir = tempdir().unwrap();
    let root = temp_dir.path().join("crate");
    let src_dir = root.join("src");
    let file = src_dir.join("main.rs");

    fs::create_dir_all(&src_dir).unwrap();
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(&file, "fn main() {}\n").unwrap();

    let servers = servers_for_file(&file);
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].kind, ServerKind::Rust);
    let expected_root = fs::canonicalize(&root).unwrap();
    assert_eq!(
        find_workspace_root(&file, servers[0].root_markers),
        Some(expected_root)
    );
}
