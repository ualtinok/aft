use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use aft::commands::lsp_diagnostics::handle_lsp_diagnostics;
use aft::config::Config;
use aft::context::AppContext;
use aft::lsp::client::LspEvent;
use aft::lsp::diagnostics::DiagnosticSeverity;
use aft::lsp::manager::LspManager;
use aft::lsp::registry::ServerKind;
use aft::parser::TreeSitterProvider;
use aft::protocol::RawRequest;
use tempfile::tempdir;

use super::helpers::AftProcess;

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

fn rust_workspace_with_files(names: &[&str]) -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let temp_dir = tempdir().expect("tempdir");
    let root = temp_dir.path().join("workspace");
    let src_dir = root.join("src");

    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo\"\n").expect("write Cargo.toml");

    let mut files = Vec::new();
    for name in names {
        let path = src_dir.join(name);
        fs::write(&path, "fn main() {}\n").expect("write fixture source");
        files.push(path);
    }

    (temp_dir, root, files)
}

fn collect_event<F>(manager: &mut LspManager, predicate: F) -> Option<LspEvent>
where
    F: Fn(&LspEvent) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        for event in manager.drain_events() {
            if predicate(&event) {
                return Some(event);
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    None
}

fn wait_for_publish(manager: &mut LspManager) {
    let event = collect_event(manager, |event| {
        matches!(
            event,
            LspEvent::Notification {
                method,
                ..
            } if method == "textDocument/publishDiagnostics"
        )
    });
    assert!(event.is_some(), "timed out waiting for publishDiagnostics");
}

fn manager_with_fake_server() -> LspManager {
    let mut manager = LspManager::new();
    manager.override_binary(ServerKind::Rust, fake_server_path());
    manager
}

fn app_context_with_fake_lsp() -> AppContext {
    let ctx = AppContext::new(Box::new(TreeSitterProvider::new()), Config::default());
    ctx.lsp()
        .override_binary(ServerKind::Rust, fake_server_path());
    ctx
}

#[test]
fn test_diagnostics_stored_after_did_open() {
    let (_temp_dir, _root, files) = rust_workspace_with_files(&["main.rs"]);
    let file = &files[0];
    let mut manager = manager_with_fake_server();

    manager
        .notify_file_changed(file, "fn main() { println!(\"hi\"); }\n")
        .expect("notify file changed");
    wait_for_publish(&mut manager);

    let diagnostics = manager.get_diagnostics_for_file(file);
    assert_eq!(diagnostics.len(), 2);
    assert_eq!(diagnostics[0].line, 1);
    assert_eq!(diagnostics[0].column, 1);
    assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E0001"));
    assert_eq!(diagnostics[1].line, 2);
    assert_eq!(diagnostics[1].column, 5);
    assert_eq!(diagnostics[1].severity, DiagnosticSeverity::Warning);
}

#[test]
fn test_diagnostics_replace_on_new_publish() {
    let (_temp_dir, _root, files) = rust_workspace_with_files(&["main.rs"]);
    let file = &files[0];
    let mut manager = manager_with_fake_server();

    manager
        .notify_file_changed(file, "fn main() { println!(\"one\"); }\n")
        .expect("first notify");
    wait_for_publish(&mut manager);
    assert_eq!(manager.get_diagnostics_for_file(file).len(), 2);

    manager
        .notify_file_changed(file, "fn main() { println!(\"two\"); }\n")
        .expect("second notify");
    wait_for_publish(&mut manager);

    let diagnostics = manager.get_diagnostics_for_file(file);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "test diagnostic after change");
    assert_eq!(diagnostics[0].line, 3);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E0002"));
}

#[test]
fn test_diagnostics_filter_by_severity() {
    let (_temp_dir, _root, files) = rust_workspace_with_files(&["main.rs"]);
    let file = &files[0];
    let ctx = app_context_with_fake_lsp();

    ctx.lsp()
        .notify_file_changed(file, "fn main() { println!(\"hi\"); }\n")
        .expect("notify file changed");

    let req: RawRequest = serde_json::from_value(serde_json::json!({
        "id": "diag-filter",
        "command": "lsp_diagnostics",
        "file": file.display().to_string(),
        "severity": "error",
        "wait_ms": 250
    }))
    .expect("request parses");

    let response = handle_lsp_diagnostics(&req, &ctx);
    let json = serde_json::to_value(&response).expect("response serializes");
    let diagnostics = json["diagnostics"].as_array().expect("diagnostics array");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0]["severity"], "error");
    assert_eq!(diagnostics[0]["message"], "test diagnostic error");
}

#[test]
fn test_wait_for_diagnostics_returns_after_matching_publish() {
    let (_temp_dir, _root, files) = rust_workspace_with_files(&["main.rs", "lib.rs"]);
    let main_rs = &files[0];
    let lib_rs = &files[1];
    let mut manager = manager_with_fake_server();

    manager
        .notify_file_changed(lib_rs, "pub fn answer() -> u32 { 42 }\n")
        .expect("open lib");
    wait_for_publish(&mut manager);

    manager
        .notify_file_changed(main_rs, "fn main() { println!(\"hi\"); }\n")
        .expect("open main");

    let diagnostics = manager.wait_for_diagnostics(main_rs, Duration::from_secs(2));
    let canonical_main = fs::canonicalize(main_rs).expect("canonical main");

    assert_eq!(diagnostics.len(), 2);
    assert!(diagnostics
        .iter()
        .all(|diagnostic| diagnostic.file == canonical_main));
}

#[test]
fn test_diagnostics_for_file_vs_all() {
    let (_temp_dir, _root, files) = rust_workspace_with_files(&["main.rs", "lib.rs"]);
    let main_rs = &files[0];
    let lib_rs = &files[1];
    let mut manager = manager_with_fake_server();

    manager
        .notify_file_changed(main_rs, "fn main() {}\n")
        .expect("open main");
    wait_for_publish(&mut manager);
    manager
        .notify_file_changed(lib_rs, "pub fn answer() -> u32 { 42 }\n")
        .expect("open lib");
    wait_for_publish(&mut manager);

    let file_diagnostics = manager.get_diagnostics_for_file(main_rs);
    let all_diagnostics = manager.get_all_diagnostics();
    let canonical_main = fs::canonicalize(main_rs).expect("canonical main");

    assert_eq!(file_diagnostics.len(), 2);
    assert_eq!(all_diagnostics.len(), 4);
    assert!(file_diagnostics
        .iter()
        .all(|diagnostic| diagnostic.file == canonical_main));
}

#[test]
fn test_diagnostics_clear_on_empty_array() {
    let (_temp_dir, _root, files) = rust_workspace_with_files(&["main.rs"]);
    let file = &files[0];
    let mut manager = manager_with_fake_server();

    manager
        .notify_file_changed(file, "fn main() {}\n")
        .expect("open file");
    wait_for_publish(&mut manager);
    assert_eq!(manager.get_diagnostics_for_file(file).len(), 2);

    manager.notify_file_closed(file).expect("close file");
    wait_for_publish(&mut manager);
    assert!(manager.get_diagnostics_for_file(file).is_empty());
}

#[test]
fn test_lsp_diagnostics_command_response_format() {
    let (_temp_dir, root, files) = rust_workspace_with_files(&["main.rs"]);
    let file = &files[0];
    let fake_server = fake_server_path();
    let mut aft = AftProcess::spawn_with_env(&[("AFT_LSP_RUST_BINARY", fake_server.as_os_str())]);

    let configure = aft.send(&format!(
        r#"{{"id":"cfg","command":"configure","project_root":"{}"}}"#,
        root.display()
    ));
    assert_eq!(configure["success"], true);

    let write = aft.send(&format!(
        r#"{{"id":"write-1","command":"write","file":"{}","content":"fn main() {{ println!(\"hello\"); }}\n"}}"#,
        file.display()
    ));
    assert_eq!(write["success"], true, "write failed: {write:?}");

    let resp = aft.send(&format!(
        r#"{{"id":"diag-1","command":"lsp_diagnostics","file":"{}","wait_ms":400}}"#,
        file.display()
    ));

    assert_eq!(resp["id"], "diag-1");
    assert_eq!(resp["success"], true, "response: {resp:?}");
    assert_eq!(resp["total"], 2);
    assert_eq!(resp["files_with_errors"], 1);

    let diagnostics = resp["diagnostics"].as_array().expect("diagnostics array");
    assert_eq!(diagnostics.len(), 2);
    let canonical_file = fs::canonicalize(file).expect("canonical file");
    assert_eq!(diagnostics[0]["file"], canonical_file.display().to_string());
    assert_eq!(diagnostics[0]["line"], 1);
    assert_eq!(diagnostics[0]["column"], 1);
    assert_eq!(diagnostics[0]["end_line"], 1);
    assert_eq!(diagnostics[0]["end_column"], 6);
    assert_eq!(diagnostics[0]["severity"], "error");
    assert_eq!(diagnostics[0]["message"], "test diagnostic error");
    assert_eq!(diagnostics[0]["code"], "E0001");
    assert_eq!(diagnostics[0]["source"], "fake-lsp");
    assert_eq!(diagnostics[1]["severity"], "warning");

    let status = aft.shutdown();
    assert!(status.success());
}
