//! Integration tests for dry-run support across mutation commands.

use std::fs;

use super::helpers::AftProcess;

// ============================================================================
// write dry-run
// ============================================================================

#[test]
fn write_dry_run_returns_diff() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("write_dryrun.ts");
    fs::write(&target, "const x = 1;\n").unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"wd-1","command":"write","file":"{}","content":"const x = 2;\n","dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run write should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true, "should flag dry_run: {:?}", resp);

    // Diff should be a unified diff
    let diff = resp["diff"].as_str().expect("diff should be a string");
    assert!(diff.contains("--- a/"), "diff should have a/ prefix: {}", diff);
    assert!(diff.contains("+++ b/"), "diff should have b/ prefix: {}", diff);
    assert!(diff.contains("-const x = 1;"), "diff should show removed line: {}", diff);
    assert!(diff.contains("+const x = 2;"), "diff should show added line: {}", diff);

    // File should be unchanged on disk
    let on_disk = fs::read_to_string(&target).unwrap();
    assert_eq!(on_disk, "const x = 1;\n", "file should not be modified by dry-run");

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// edit_symbol dry-run (milestone acceptance scenario)
// ============================================================================

#[test]
fn edit_symbol_dry_run() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("edit_symbol_dryrun.ts");
    let original = r#"export function greet(name: string): string {
    return `Hello, ${name}!`;
}
"#;
    fs::write(&target, original).unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"es-dr","command":"edit_symbol","file":"{}","symbol":"greet","operation":"replace","content":"export function greet(name: string): string {{\n    return `Hi, ${{name}}!`;\n}}","dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run edit_symbol should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true, "should flag dry_run: {:?}", resp);

    let diff = resp["diff"].as_str().expect("diff should be a string");
    assert!(diff.contains("--- a/"), "diff should have a/ prefix: {}", diff);
    assert!(diff.contains("+++ b/"), "diff should have b/ prefix: {}", diff);
    // The diff should show the change from Hello to Hi
    assert!(diff.contains("-    return `Hello,"), "diff should show removed line: {}", diff);
    assert!(diff.contains("+    return `Hi,"), "diff should show added line: {}", diff);

    // File should be unchanged on disk (milestone acceptance criterion)
    let on_disk = fs::read_to_string(&target).unwrap();
    assert_eq!(on_disk, original, "file MUST NOT be modified by dry-run edit_symbol");

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// edit_match dry-run
// ============================================================================

#[test]
fn edit_match_dry_run() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("edit_match_dryrun.ts");
    let original = "const greeting = \"hello\";\n";
    fs::write(&target, original).unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"em-dr","command":"edit_match","file":"{}","match":"hello","replacement":"world","dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run edit_match should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true);

    let diff = resp["diff"].as_str().expect("diff should be a string");
    assert!(diff.contains("-const greeting = \"hello\";"), "diff should show removed line: {}", diff);
    assert!(diff.contains("+const greeting = \"world\";"), "diff should show added line: {}", diff);

    // File unchanged
    assert_eq!(fs::read_to_string(&target).unwrap(), original);

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// batch dry-run
// ============================================================================

#[test]
fn batch_dry_run() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("batch_dryrun.ts");
    let original = "const a = 1;\nconst b = 2;\nconst c = 3;\n";
    fs::write(&target, original).unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"b-dr","command":"batch","file":"{}","edits":[{{"match":"const a = 1;","replacement":"const a = 10;"}},{{"match":"const c = 3;","replacement":"const c = 30;"}}],"dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run batch should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true);

    let diff = resp["diff"].as_str().expect("diff should be a string");
    // Should contain both edits in combined diff
    assert!(diff.contains("-const a = 1;"), "diff should show first edit: {}", diff);
    assert!(diff.contains("+const a = 10;"), "diff should show first replacement: {}", diff);
    assert!(diff.contains("-const c = 3;"), "diff should show second edit: {}", diff);
    assert!(diff.contains("+const c = 30;"), "diff should show second replacement: {}", diff);

    // File unchanged
    assert_eq!(fs::read_to_string(&target).unwrap(), original);

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// add_import dry-run
// ============================================================================

#[test]
fn add_import_dry_run() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("add_import_dryrun.ts");
    let original = "import { useState } from 'react';\n\nconst App = () => {};\n";
    fs::write(&target, original).unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"ai-dr","command":"add_import","file":"{}","module":"react","names":["useEffect"],"dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run add_import should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true);

    let diff = resp["diff"].as_str().expect("diff should be a string");
    assert!(diff.contains("useEffect"), "diff should mention useEffect: {}", diff);

    // File unchanged
    assert_eq!(fs::read_to_string(&target).unwrap(), original);

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// dry-run no backup
// ============================================================================

#[test]
fn dry_run_no_backup() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("no_backup_dryrun.ts");
    let original = "const x = 1;\n";
    fs::write(&target, original).unwrap();

    // Dry-run write
    let resp = aft.send(&format!(
        r#"{{"id":"nb-1","command":"write","file":"{}","content":"const x = 2;\n","dry_run":true}}"#,
        target.display()
    ));
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["dry_run"], true);

    // Check edit_history — should be empty (no backup created)
    let hist_resp = aft.send(&format!(
        r#"{{"id":"nb-2","command":"edit_history","file":"{}"}}"#,
        target.display()
    ));
    assert_eq!(hist_resp["ok"], true, "edit_history should succeed: {:?}", hist_resp);
    let entries = hist_resp["entries"].as_array().expect("entries array");
    assert!(entries.is_empty(), "dry-run should not create backup entries, got: {:?}", entries);

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// dry-run syntax validation
// ============================================================================

#[test]
fn dry_run_syntax_validation() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("syntax_dryrun.ts");
    let original = "const x = 1;\n";
    fs::write(&target, original).unwrap();

    // Dry-run write with intentionally broken syntax
    let resp = aft.send(&format!(
        r#"{{"id":"sv-1","command":"write","file":"{}","content":"const x = {{{{;\n","dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run should succeed even with bad syntax: {:?}", resp);
    assert_eq!(resp["dry_run"], true);
    assert_eq!(resp["syntax_valid"], false, "broken syntax should report syntax_valid:false: {:?}", resp);

    // File unchanged
    assert_eq!(fs::read_to_string(&target).unwrap(), original);

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// dry-run empty diff (no-op)
// ============================================================================

#[test]
fn dry_run_empty_diff() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_dryrun_tests");
    fs::create_dir_all(&dir).unwrap();
    let target = dir.join("empty_diff_dryrun.ts");
    let original = "const x = 1;\n";
    fs::write(&target, original).unwrap();

    // Dry-run write with same content = no-op
    let resp = aft.send(&format!(
        r#"{{"id":"ed-1","command":"write","file":"{}","content":"const x = 1;\n","dry_run":true}}"#,
        target.display()
    ));

    assert_eq!(resp["ok"], true, "dry_run should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true);

    let diff = resp["diff"].as_str().expect("diff should be a string");
    assert!(diff.is_empty() || !diff.contains("@@"), "no-op should produce empty or trivial diff: {}", diff);

    // File unchanged
    assert_eq!(fs::read_to_string(&target).unwrap(), original);

    // Cleanup
    let _ = fs::remove_file(&target);
    let status = aft.shutdown();
    assert!(status.success());
}
