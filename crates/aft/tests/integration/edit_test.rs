//! Integration tests for the write and edit_symbol commands.

use std::fs;
use std::path::PathBuf;

use aft::edit::replace_byte_range;

use super::helpers::{fixture_path, AftProcess};

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
            if path.exists() {
                Some(path)
            } else {
                None
            }
        })
        .expect("fake-lsp-server binary path")
}
// ============================================================================
// write command tests
// ============================================================================

#[test]
fn write_creates_new_file() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("write_new.txt");
    // Ensure file doesn't exist
    let _ = fs::remove_file(&target);

    let resp = aft.send(&format!(
        r#"{{"id":"w-1","command":"write","file":"{}","content":"hello world"}}"#,
        target.display()
    ));

    assert_eq!(resp["success"], true, "write should succeed: {:?}", resp);
    assert_eq!(resp["file"], target.display().to_string());
    assert_eq!(
        resp["created"], true,
        "file was new, created should be true"
    );
    // No backup_id for new files
    assert!(
        resp.get("backup_id").is_none() || resp["backup_id"].is_null(),
        "new file should not have backup_id"
    );

    // Verify content on disk
    let on_disk = fs::read_to_string(&target).unwrap();
    assert_eq!(on_disk, "hello world");

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn write_backups_existing_file() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("write_backup.txt");
    fs::write(&target, "original content").unwrap();

    // Overwrite via write command
    let resp = aft.send(&format!(
        r#"{{"id":"w-2","command":"write","file":"{}","content":"new content"}}"#,
        target.display()
    ));

    assert_eq!(resp["success"], true, "write should succeed: {:?}", resp);
    assert_eq!(resp["created"], false);
    assert!(
        resp["backup_id"].is_string(),
        "should have backup_id for existing file"
    );

    // Verify new content on disk
    assert_eq!(fs::read_to_string(&target).unwrap(), "new content");

    // Undo — should restore original
    let undo_resp = aft.send(&format!(
        r#"{{"id":"w-2u","command":"undo","file":"{}"}}"#,
        target.display()
    ));
    assert_eq!(
        undo_resp["success"], true,
        "undo should succeed: {:?}",
        undo_resp
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "original content");

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn write_syntax_valid() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("write_valid.ts");
    let _ = fs::remove_file(&target);

    let resp = aft.send(&format!(
        r#"{{"id":"w-3","command":"write","file":"{}","content":"export function hello(): string {{ return \"hi\"; }}"}}"#,
        target.display()
    ));

    assert_eq!(resp["success"], true, "write should succeed: {:?}", resp);
    assert_eq!(
        resp["syntax_valid"], true,
        "valid TS should have syntax_valid: true"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn write_syntax_invalid() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("write_invalid.ts");
    let _ = fs::remove_file(&target);

    let resp = aft.send(&format!(
        r#"{{"id":"w-4","command":"write","file":"{}","content":"export function {{ broken syntax"}}"#,
        target.display()
    ));

    assert_eq!(
        resp["success"], true,
        "write should succeed even with bad syntax: {:?}",
        resp
    );
    assert_eq!(
        resp["syntax_valid"], false,
        "broken TS should have syntax_valid: false"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// edit_symbol command tests
// ============================================================================

#[test]
fn edit_symbol_replace() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("edit_replace.ts");

    // Copy fixture
    let fixture = fixture_path("sample.ts");
    fs::copy(&fixture, &target).unwrap();

    // Build request with serde_json to avoid escaping issues
    // Note: symbol range starts at `function` (col 7), not at `export`
    let req = serde_json::json!({
        "id": "es-1",
        "command": "edit_symbol",
        "file": target.display().to_string(),
        "symbol": "greet",
        "operation": "replace",
        "content": "function greet(name: string): string {\n  return `Hey, ${name}!`;\n}"
    });

    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "edit_symbol replace should succeed: {:?}",
        resp
    );
    assert_eq!(resp["symbol"], "greet");
    assert_eq!(resp["operation"], "replace");
    assert_eq!(resp["syntax_valid"], true, "replacement should be valid TS");
    assert!(resp["backup_id"].is_string(), "should have backup_id");
    assert!(resp["range"].is_object(), "should have original range");

    // Verify content changed on disk
    let content = fs::read_to_string(&target).unwrap();
    assert!(
        content.contains("Hey,"),
        "should contain new greeting: {}",
        content
    );
    assert!(
        !content.contains("Hello,"),
        "should not contain old greeting"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_symbol_delete() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("edit_delete.ts");

    // Copy fixture
    let fixture = fixture_path("sample.ts");
    fs::copy(&fixture, &target).unwrap();

    // Verify internalHelper exists
    let before = fs::read_to_string(&target).unwrap();
    assert!(before.contains("internalHelper"));

    let resp = aft.send(&format!(
        r#"{{"id":"es-2","command":"edit_symbol","file":"{}","symbol":"internalHelper","operation":"delete"}}"#,
        target.display()
    ));

    assert_eq!(
        resp["success"], true,
        "edit_symbol delete should succeed: {:?}",
        resp
    );
    assert_eq!(resp["operation"], "delete");

    // Verify symbol is gone from disk
    let after = fs::read_to_string(&target).unwrap();
    assert!(
        !after.contains("function internalHelper"),
        "internalHelper should be deleted"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_symbol_ambiguous() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("edit_ambig.ts");

    // Copy ambiguous fixture
    let fixture = fixture_path("ambiguous.ts");
    fs::copy(&fixture, &target).unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"es-3","command":"edit_symbol","file":"{}","symbol":"process","operation":"replace","content":"// replaced"}}"#,
        target.display()
    ));

    assert_eq!(
        resp["success"], true,
        "ambiguous response should succeed: {:?}",
        resp
    );
    assert_eq!(resp["code"], "ambiguous_symbol");
    let candidates = resp["candidates"]
        .as_array()
        .expect("should have candidates array");
    assert!(
        candidates.len() >= 2,
        "should have at least 2 candidates: {:?}",
        candidates
    );

    // Each candidate should have name, qualified, line, kind
    for c in candidates {
        assert!(c["name"].is_string(), "candidate should have name: {:?}", c);
        assert!(
            c["qualified"].is_string(),
            "candidate should have qualified: {:?}",
            c
        );
        assert!(c["line"].is_number(), "candidate should have line: {:?}", c);
        assert!(c["kind"].is_string(), "candidate should have kind: {:?}", c);
    }

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_symbol_not_found() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("sample.ts");

    let resp = aft.send(&format!(
        r#"{{"id":"es-4","command":"edit_symbol","file":"{}","symbol":"nonexistent_symbol","operation":"replace","content":"// nope"}}"#,
        file.display()
    ));

    assert_eq!(
        resp["success"], false,
        "should fail for nonexistent symbol: {:?}",
        resp
    );
    assert_eq!(resp["code"], "symbol_not_found");
    assert!(resp["message"]
        .as_str()
        .unwrap()
        .contains("nonexistent_symbol"));

    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// edit_match command tests
// ============================================================================

#[test]
fn edit_match_single_occurrence() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("match_single.ts");

    // Copy fixture
    let fixture = fixture_path("sample.ts");
    fs::copy(&fixture, &target).unwrap();

    // "internalHelper" appears exactly once in sample.ts
    let req = serde_json::json!({
        "id": "em-1",
        "command": "edit_match",
        "file": target.display().to_string(),
        "match": "internalHelper",
        "replacement": "secretHelper"
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "edit_match should succeed: {:?}",
        resp
    );
    assert_eq!(resp["replacements"], 1);
    assert_eq!(resp["syntax_valid"], true);
    assert!(resp["backup_id"].is_string(), "should have backup_id");

    // Verify content on disk
    let content = fs::read_to_string(&target).unwrap();
    assert!(
        content.contains("secretHelper"),
        "should contain replacement"
    );
    assert!(
        !content.contains("internalHelper"),
        "should not contain original match"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_returns_inline_lsp_diagnostics_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("main.rs");
    let cargo_toml = root.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        "[package]\nname = \"inline-diag\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        &file,
        "fn main() { let value = 1; println!(\"{}\", value); }\n",
    )
    .unwrap();

    let fake_server = fake_server_path();
    let mut aft = AftProcess::spawn_with_env(&[("AFT_LSP_RUST_BINARY", fake_server.as_os_str())]);

    let configure = aft.send(&format!(
        r#"{{"id":"cfg-inline","command":"configure","project_root":"{}"}}"#,
        root.display()
    ));
    assert_eq!(
        configure["success"], true,
        "configure failed: {configure:?}"
    );

    let req = serde_json::json!({
        "id": "em-inline-diag",
        "command": "edit_match",
        "file": file.display().to_string(),
        "match": "let value = 1",
        "replacement": "let answer = 1",
        "diagnostics": true,
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], true, "edit_match should succeed: {resp:?}");
    let diagnostics = resp["lsp_diagnostics"]
        .as_array()
        .expect("lsp_diagnostics array");
    assert_eq!(
        diagnostics.len(),
        2,
        "expected inline diagnostics: {resp:?}"
    );

    let canonical_file = fs::canonicalize(&file).expect("canonical file");
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

#[test]
fn edit_match_inline_lsp_diagnostics_respects_wait_ms() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = root.join("main.rs");
    let cargo_toml = root.join("Cargo.toml");
    fs::write(
        &cargo_toml,
        "[package]\nname = \"inline-diag-fast\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        &file,
        "fn main() { let value = 1; println!(\"{}\", value); }\n",
    )
    .unwrap();

    let fake_server = fake_server_path();
    let mut aft = AftProcess::spawn_with_env(&[("AFT_LSP_RUST_BINARY", fake_server.as_os_str())]);

    let configure = aft.send(&format!(
        r#"{{"id":"cfg-inline-fast","command":"configure","project_root":"{}"}}"#,
        root.display()
    ));
    assert_eq!(
        configure["success"], true,
        "configure failed: {configure:?}"
    );

    let start = std::time::Instant::now();
    let req = serde_json::json!({
        "id": "em-inline-diag-fast",
        "command": "edit_match",
        "file": file.display().to_string(),
        "match": "let value = 1",
        "replacement": "let answer = 1",
        "diagnostics": true,
        "wait_ms": 2_000,
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());
    let elapsed = start.elapsed();

    assert_eq!(resp["success"], true, "edit_match should succeed: {resp:?}");
    let diagnostics = resp["lsp_diagnostics"]
        .as_array()
        .expect("lsp_diagnostics array");
    assert_eq!(
        diagnostics.len(),
        2,
        "expected inline diagnostics: {resp:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(3_000),
        "expected event-driven wait, elapsed: {elapsed:?}, resp: {resp:?}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_multiple_occurrences_returns_candidates() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("match_ambig.ts");

    // Write a file with a repeated string
    let content = "const a = \"hello\";\nconst b = \"hello\";\nconst c = \"world\";\n";
    fs::write(&target, content).unwrap();

    let req = serde_json::json!({
        "id": "em-2",
        "command": "edit_match",
        "file": target.display().to_string(),
        "match": "hello",
        "replacement": "bye"
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], false,
        "ambiguous response should be an error: {:?}",
        resp
    );
    assert_eq!(resp["code"], "ambiguous_match");
    let occurrences = resp["occurrences"]
        .as_array()
        .expect("should have occurrences array");
    assert_eq!(occurrences.len(), 2, "should have exactly 2 occurrences");

    // Each occurrence should have index, line, context
    for occ in occurrences {
        assert!(occ["index"].is_number(), "should have index: {:?}", occ);
        assert!(occ["line"].is_number(), "should have line: {:?}", occ);
        assert!(occ["context"].is_string(), "should have context: {:?}", occ);
    }

    // File should be unchanged
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        content,
        "file should not be modified"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_with_occurrence_selector() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("match_occ.ts");

    // Write a file with a repeated string
    let content = "const a = \"hello\";\nconst b = \"hello\";\nconst c = \"world\";\n";
    fs::write(&target, content).unwrap();

    // Select occurrence index 1 (second match)
    let req = serde_json::json!({
        "id": "em-3",
        "command": "edit_match",
        "file": target.display().to_string(),
        "match": "hello",
        "replacement": "bye",
        "occurrence": 1
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "edit_match with occurrence should succeed: {:?}",
        resp
    );
    assert_eq!(resp["replacements"], 1);

    // Verify: first "hello" untouched, second replaced
    let result = fs::read_to_string(&target).unwrap();
    assert!(
        result.contains("const a = \"hello\""),
        "first occurrence should be untouched"
    );
    assert!(
        result.contains("const b = \"bye\""),
        "second occurrence should be replaced"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_no_match() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("sample.ts");

    let req = serde_json::json!({
        "id": "em-4",
        "command": "edit_match",
        "file": file.display().to_string(),
        "match": "this_string_does_not_exist_anywhere",
        "replacement": "nope"
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], false,
        "should fail for no match: {:?}",
        resp
    );
    assert_eq!(resp["code"], "match_not_found");

    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// batch command tests
// ============================================================================

#[test]
fn batch_multiple_edits() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("batch_multi.ts");

    let content = "const greeting = \"hello\";\nconst farewell = \"goodbye\";\nconst count = 42;\n";
    fs::write(&target, content).unwrap();

    let req = serde_json::json!({
        "id": "b-1",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "match": "hello", "replacement": "hi" },
            { "match": "goodbye", "replacement": "bye" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], true, "batch should succeed: {:?}", resp);
    assert_eq!(resp["edits_applied"], 2);
    assert!(resp["backup_id"].is_string(), "should have backup_id");

    let result = fs::read_to_string(&target).unwrap();
    assert!(
        result.contains("\"hi\""),
        "first edit should apply: {}",
        result
    );
    assert!(
        result.contains("\"bye\""),
        "second edit should apply: {}",
        result
    );
    assert!(
        result.contains("count = 42"),
        "untouched line should remain"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_rollback_on_failure() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("batch_rollback.ts");

    let content = "const greeting = \"hello\";\nconst count = 42;\n";
    fs::write(&target, content).unwrap();

    // First edit is valid, second has a match that doesn't exist
    let req = serde_json::json!({
        "id": "b-2",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "match": "hello", "replacement": "hi" },
            { "match": "nonexistent_string", "replacement": "nope" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], false, "batch should fail: {:?}", resp);
    assert_eq!(resp["code"], "batch_edit_failed");

    // File should be unchanged — no partial application, no backup taken
    let on_disk = fs::read_to_string(&target).unwrap();
    assert_eq!(
        on_disk, content,
        "file should be unchanged after failed batch"
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_fuzzy_match() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("batch_fuzzy.ts");

    // Source has specific whitespace; edits use slightly different whitespace
    let content = "function greet() {\n    console.log(\"hello\");  \n    return true;\n}\n";
    fs::write(&target, content).unwrap();

    let req = serde_json::json!({
        "id": "b-fuzzy",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            // Trailing whitespace mismatch: source line has trailing spaces, match text doesn't
            { "match": "    console.log(\"hello\");", "replacement": "    console.log(\"hi\");" },
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "batch fuzzy should succeed: {:?}",
        resp
    );
    assert_eq!(resp["edits_applied"], 1);

    let result = fs::read_to_string(&target).unwrap();
    assert!(
        result.contains("\"hi\""),
        "fuzzy edit should apply: {}",
        result
    );
    // Trailing spaces from the original line should be replaced (not left behind)
    assert!(
        !result.contains("\"hello\""),
        "old text should be gone: {}",
        result
    );

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_line_range_edit() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("batch_linerange.ts");

    let content = "line zero\nline one\nline two\nline three\n";
    fs::write(&target, content).unwrap();

    // Replace line 2 (1-indexed, i.e. "line one") with new content
    let req = serde_json::json!({
        "id": "b-3",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "line_start": 2, "line_end": 2, "content": "replaced line\n" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "batch line-range should succeed: {:?}",
        resp
    );
    assert_eq!(resp["edits_applied"], 1);

    let result = fs::read_to_string(&target).unwrap();
    assert!(result.contains("line zero"), "line 0 untouched");
    assert!(
        result.contains("replaced line"),
        "line 1 should be replaced"
    );
    assert!(
        !result.contains("line one"),
        "original line 1 should be gone"
    );
    assert!(result.contains("line two"), "line 2 untouched");

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_with_undo() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_edit_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("batch_undo.ts");

    let original = "const x = 1;\nconst y = 2;\n";
    fs::write(&target, original).unwrap();

    // Apply batch
    let req = serde_json::json!({
        "id": "b-4",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "match": "const x = 1", "replacement": "const x = 100" },
            { "match": "const y = 2", "replacement": "const y = 200" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());
    assert_eq!(resp["success"], true, "batch should succeed: {:?}", resp);

    // Verify edits applied
    let modified = fs::read_to_string(&target).unwrap();
    assert!(modified.contains("x = 100"), "should have x = 100");
    assert!(modified.contains("y = 200"), "should have y = 200");

    // Undo
    let undo_resp = aft.send(&format!(
        r#"{{"id":"b-4u","command":"undo","file":"{}"}}"#,
        target.display()
    ));
    assert_eq!(
        undo_resp["success"], true,
        "undo should succeed: {:?}",
        undo_resp
    );

    // Verify original restored
    let restored = fs::read_to_string(&target).unwrap();
    assert_eq!(restored, original, "undo should restore original content");

    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_overlapping_ranges_returns_overlapping_edits_error() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("batch_overlap.txt");
    let original = "abcdefghij\n";
    fs::write(&target, original).unwrap();

    let req = serde_json::json!({
        "id": "b-overlap",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "match": "cdef", "replacement": "XXXX" },
            { "match": "defg", "replacement": "YYYY" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], false, "batch should fail: {:?}", resp);
    assert_eq!(resp["code"], "overlapping_edits");
    assert!(resp["message"].as_str().unwrap().contains("overlaps"));
    assert_eq!(fs::read_to_string(&target).unwrap(), original);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_accepts_old_string_new_string_keys() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("batch_old_new.txt");
    let original = "alpha beta gamma\n";
    fs::write(&target, original).unwrap();

    let req = serde_json::json!({
        "id": "b-old-new",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "oldString": "alpha", "newString": "ALPHA" },
            { "oldString": "gamma", "newString": "GAMMA" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], true, "batch should succeed: {:?}", resp);
    assert_eq!(resp["edits_applied"], 2);
    assert_eq!(fs::read_to_string(&target).unwrap(), "ALPHA beta GAMMA\n");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn batch_fuzzy_matching_covers_all_progressive_passes() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("batch_fuzzy_passes.txt");
    let original = concat!(
        "exact = 1\n",
        "trailing = 2   \n",
        "    trimmed = 3   \n",
        "normalized\u{00a0}space = 4\n"
    );
    fs::write(&target, original).unwrap();

    let req = serde_json::json!({
        "id": "b-fuzzy-passes",
        "command": "batch",
        "file": target.display().to_string(),
        "edits": [
            { "match": "exact = 1\n", "replacement": "exact = 10\n" },
            { "match": "trailing = 2\n", "replacement": "trailing = 20\n" },
            { "match": "trimmed = 3\n", "replacement": "trimmed = 30\n" },
            { "match": "normalized space = 4", "replacement": "normalized space = 40\n" }
        ]
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], true, "batch should succeed: {:?}", resp);
    assert_eq!(resp["edits_applied"], 4);
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        concat!(
            "exact = 10\n",
            "trailing = 20\n",
            "trimmed = 30\n",
            "normalized space = 40\n"
        )
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_replace_all_replaces_multiple_occurrences() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("match_replace_all.txt");
    let original = "hello hello hello\n";
    fs::write(&target, original).unwrap();

    let req = serde_json::json!({
        "id": "em-replace-all",
        "command": "edit_match",
        "file": target.display().to_string(),
        "match": "hello",
        "replacement": "bye",
        "replace_all": true,
        "replaceAll": true
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "edit_match should succeed: {:?}",
        resp
    );
    assert_eq!(resp["replacements"], 3);
    assert_eq!(fs::read_to_string(&target).unwrap(), "bye bye bye\n");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn replace_byte_range_invalid_ranges_return_errors() {
    let aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("replace_invalid.txt");
    fs::write(&target, "abcdef").unwrap();
    let source = fs::read_to_string(&target).unwrap();

    let start_after_end = replace_byte_range(&source, 4, 2, "x").unwrap_err();
    assert_eq!(start_after_end.code(), "invalid_request");
    assert!(start_after_end.to_string().contains("start must be <= end"));

    let end_out_of_bounds = replace_byte_range(&source, 0, source.len() + 1, "x").unwrap_err();
    assert_eq!(end_out_of_bounds.code(), "invalid_request");
    assert!(end_out_of_bounds
        .to_string()
        .contains("end exceeds source length"));

    assert_eq!(fs::read_to_string(&target).unwrap(), "abcdef");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn replace_byte_range_rejects_non_char_boundaries() {
    let aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("replace_utf8.txt");
    fs::write(&target, "aéz").unwrap();
    let source = fs::read_to_string(&target).unwrap();

    let invalid_end = replace_byte_range(&source, 1, 2, "x").unwrap_err();
    assert_eq!(invalid_end.code(), "invalid_request");
    assert!(invalid_end
        .to_string()
        .contains("end is not a char boundary"));

    let invalid_start = replace_byte_range(&source, 2, 3, "x").unwrap_err();
    assert_eq!(invalid_start.code(), "invalid_request");
    assert!(invalid_start
        .to_string()
        .contains("start is not a char boundary"));

    assert_eq!(fs::read_to_string(&target).unwrap(), "aéz");

    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// glob edit_match tests
// ============================================================================

#[test]
fn edit_match_glob_replaces_across_files() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path();

    fs::write(dir_path.join("a.ts"), "const x = \"OLD_VALUE\";\n").unwrap();
    fs::write(
        dir_path.join("b.ts"),
        "const y = \"OLD_VALUE\";\nconst z = \"OLD_VALUE\";\n",
    )
    .unwrap();
    fs::write(dir_path.join("c.json"), "{\"key\": \"OLD_VALUE\"}\n").unwrap();

    let req = serde_json::json!({
        "id": "glob-1",
        "command": "edit_match",
        "file": format!("{}/**/*.ts", dir_path.display()),
        "match": "OLD_VALUE",
        "replacement": "NEW_VALUE"
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(
        resp["success"], true,
        "glob edit should succeed: {:?}",
        resp
    );
    assert_eq!(resp["total_files"], 2, "should match 2 .ts files");
    assert_eq!(
        resp["total_replacements"], 3,
        "should replace 3 occurrences"
    );
    assert!(resp["files"].is_array(), "should have files array");
    assert_eq!(resp["files"].as_array().unwrap().len(), 2);

    let a_content = fs::read_to_string(dir_path.join("a.ts")).unwrap();
    assert!(
        a_content.contains("NEW_VALUE"),
        "a.ts should have NEW_VALUE"
    );
    assert!(
        !a_content.contains("OLD_VALUE"),
        "a.ts should not have OLD_VALUE"
    );

    let b_content = fs::read_to_string(dir_path.join("b.ts")).unwrap();
    assert_eq!(b_content.matches("NEW_VALUE").count(), 2);

    let c_content = fs::read_to_string(dir_path.join("c.json")).unwrap();
    assert!(
        c_content.contains("OLD_VALUE"),
        "c.json should be unchanged"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_glob_dry_run() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path();

    fs::write(dir_path.join("x.ts"), "const a = \"foo\";\n").unwrap();
    fs::write(
        dir_path.join("y.ts"),
        "const b = \"foo\";\nconst c = \"foo\";\n",
    )
    .unwrap();

    let req = serde_json::json!({
        "id": "glob-dry-1",
        "command": "edit_match",
        "file": format!("{}/*.ts", dir_path.display()),
        "match": "foo",
        "replacement": "bar",
        "dry_run": true
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], true);
    assert_eq!(resp["dry_run"], true);
    assert_eq!(resp["total_files"], 2);
    assert_eq!(resp["total_replacements"], 3);

    let x_content = fs::read_to_string(dir_path.join("x.ts")).unwrap();
    assert!(x_content.contains("foo"), "dry_run should not modify files");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_glob_no_matches_in_files() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path();

    fs::write(dir_path.join("a.ts"), "const x = 1;\n").unwrap();

    let req = serde_json::json!({
        "id": "glob-nomatch-1",
        "command": "edit_match",
        "file": format!("{}/*.ts", dir_path.display()),
        "match": "NONEXISTENT",
        "replacement": "whatever"
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], false);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_glob_no_files_matched() {
    let mut aft = AftProcess::spawn();
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path();

    let req = serde_json::json!({
        "id": "glob-nofiles-1",
        "command": "edit_match",
        "file": format!("{}/*.xyz", dir_path.display()),
        "match": "something",
        "replacement": "else"
    });
    let resp = aft.send(&serde_json::to_string(&req).unwrap());

    assert_eq!(resp["success"], false);

    let status = aft.shutdown();
    assert!(status.success());
}
