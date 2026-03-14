//! Integration tests for the `transaction` command: multi-file atomicity and rollback.

use std::fs;

use super::helpers::AftProcess;

// ============================================================================
// transaction_success_three_files
// ============================================================================

#[test]
fn transaction_success_three_files() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_transaction_tests");
    fs::create_dir_all(&dir).unwrap();

    let f1 = dir.join("txn_ok_1.ts");
    let f2 = dir.join("txn_ok_2.ts");
    let f3 = dir.join("txn_ok_3.ts");

    fs::write(&f1, "const a = 1;\n").unwrap();
    fs::write(&f2, "const b = 2;\n").unwrap();
    fs::write(&f3, "const c = 3;\n").unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"txn-1","command":"transaction","operations":[{{"file":"{}","command":"write","content":"const a = 10;\n"}},{{"file":"{}","command":"write","content":"const b = 20;\n"}},{{"file":"{}","command":"write","content":"const c = 30;\n"}}]}}"#,
        f1.display(), f2.display(), f3.display()
    ));

    assert_eq!(resp["ok"], true, "transaction should succeed: {:?}", resp);
    assert_eq!(resp["files_modified"], 3);

    let results = resp["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3);

    // All three files modified on disk
    assert_eq!(fs::read_to_string(&f1).unwrap(), "const a = 10;\n");
    assert_eq!(fs::read_to_string(&f2).unwrap(), "const b = 20;\n");
    assert_eq!(fs::read_to_string(&f3).unwrap(), "const c = 30;\n");

    // Cleanup
    let _ = fs::remove_file(&f1);
    let _ = fs::remove_file(&f2);
    let _ = fs::remove_file(&f3);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// transaction_rollback_syntax_error (milestone acceptance scenario)
// ============================================================================

#[test]
fn transaction_rollback_syntax_error() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_transaction_tests");
    fs::create_dir_all(&dir).unwrap();

    let f1 = dir.join("txn_rb_1.ts");
    let f2 = dir.join("txn_rb_2.ts");
    let f3 = dir.join("txn_rb_3.ts");

    let orig1 = "const a = 1;\n";
    let orig2 = "const b = 2;\n";
    let orig3 = "const c = 3;\n";

    fs::write(&f1, orig1).unwrap();
    fs::write(&f2, orig2).unwrap();
    fs::write(&f3, orig3).unwrap();

    // Third file gets intentionally broken syntax
    let resp = aft.send(&format!(
        r#"{{"id":"txn-rb","command":"transaction","operations":[{{"file":"{}","command":"write","content":"const a = 10;\n"}},{{"file":"{}","command":"write","content":"const b = 20;\n"}},{{"file":"{}","command":"write","content":"const c = {{{{;\n"}}]}}"#,
        f1.display(), f2.display(), f3.display()
    ));

    assert_eq!(resp["ok"], false, "transaction should fail: {:?}", resp);
    assert_eq!(resp["code"], "transaction_failed");
    assert_eq!(resp["failed_operation"], 2, "third op (index 2) should fail");

    // rolled_back array should list all 3 files
    let rolled_back = resp["rolled_back"].as_array().expect("rolled_back array");
    assert_eq!(rolled_back.len(), 3, "all 3 files should be rolled back: {:?}", rolled_back);

    // All three files should be restored to original content
    assert_eq!(fs::read_to_string(&f1).unwrap(), orig1, "f1 should be restored");
    assert_eq!(fs::read_to_string(&f2).unwrap(), orig2, "f2 should be restored");
    assert_eq!(fs::read_to_string(&f3).unwrap(), orig3, "f3 should be restored");

    // Cleanup
    let _ = fs::remove_file(&f1);
    let _ = fs::remove_file(&f2);
    let _ = fs::remove_file(&f3);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// transaction_rollback_new_file
// ============================================================================

#[test]
fn transaction_rollback_new_file() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_transaction_tests");
    fs::create_dir_all(&dir).unwrap();

    let existing = dir.join("txn_new_existing.ts");
    let new_file = dir.join("txn_new_created.ts");
    let bad_file = dir.join("txn_new_bad.ts");

    let orig = "const x = 1;\n";
    fs::write(&existing, orig).unwrap();
    // new_file does not exist yet
    let _ = fs::remove_file(&new_file);
    let _ = fs::remove_file(&bad_file);

    // Op 0: modify existing, Op 1: create new file, Op 2: broken syntax triggers rollback
    let resp = aft.send(&format!(
        r#"{{"id":"txn-nf","command":"transaction","operations":[{{"file":"{}","command":"write","content":"const x = 10;\n"}},{{"file":"{}","command":"write","content":"const y = 1;\n"}},{{"file":"{}","command":"write","content":"const z = {{{{;\n"}}]}}"#,
        existing.display(), new_file.display(), bad_file.display()
    ));

    assert_eq!(resp["ok"], false, "transaction should fail: {:?}", resp);
    assert_eq!(resp["code"], "transaction_failed");

    // Existing file restored
    assert_eq!(fs::read_to_string(&existing).unwrap(), orig, "existing file should be restored");

    // New file should be deleted on rollback
    assert!(!new_file.exists(), "new file should be deleted on rollback");

    // Bad file should also be deleted (it was new)
    assert!(!bad_file.exists(), "bad file should be deleted on rollback");

    // Cleanup
    let _ = fs::remove_file(&existing);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// transaction_edit_match_operation
// ============================================================================

#[test]
fn transaction_edit_match_operation() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_transaction_tests");
    fs::create_dir_all(&dir).unwrap();

    let f1 = dir.join("txn_em_1.ts");
    let f2 = dir.join("txn_em_2.ts");

    fs::write(&f1, "const greeting = \"hello\";\n").unwrap();
    fs::write(&f2, "const name = \"world\";\n").unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"txn-em","command":"transaction","operations":[{{"file":"{}","command":"edit_match","match":"hello","replacement":"hi"}},{{"file":"{}","command":"edit_match","match":"world","replacement":"rust"}}]}}"#,
        f1.display(), f2.display()
    ));

    assert_eq!(resp["ok"], true, "transaction should succeed: {:?}", resp);
    assert_eq!(resp["files_modified"], 2);

    assert_eq!(fs::read_to_string(&f1).unwrap(), "const greeting = \"hi\";\n");
    assert_eq!(fs::read_to_string(&f2).unwrap(), "const name = \"rust\";\n");

    // Cleanup
    let _ = fs::remove_file(&f1);
    let _ = fs::remove_file(&f2);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// transaction_dry_run
// ============================================================================

#[test]
fn transaction_dry_run() {
    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_transaction_tests");
    fs::create_dir_all(&dir).unwrap();

    let f1 = dir.join("txn_dr_1.ts");
    let f2 = dir.join("txn_dr_2.ts");

    let orig1 = "const a = 1;\n";
    let orig2 = "const b = 2;\n";
    fs::write(&f1, orig1).unwrap();
    fs::write(&f2, orig2).unwrap();

    let resp = aft.send(&format!(
        r#"{{"id":"txn-dr","command":"transaction","operations":[{{"file":"{}","command":"write","content":"const a = 10;\n"}},{{"file":"{}","command":"edit_match","match":"const b = 2","replacement":"const b = 20"}}],"dry_run":true}}"#,
        f1.display(), f2.display()
    ));

    assert_eq!(resp["ok"], true, "dry-run transaction should succeed: {:?}", resp);
    assert_eq!(resp["dry_run"], true);

    let diffs = resp["diffs"].as_array().expect("diffs array");
    assert_eq!(diffs.len(), 2, "should have 2 per-file diffs");

    // First diff should show change
    let diff1 = diffs[0]["diff"].as_str().expect("diff string");
    assert!(diff1.contains("-const a = 1;"), "diff1 should show removed: {}", diff1);
    assert!(diff1.contains("+const a = 10;"), "diff1 should show added: {}", diff1);

    // Second diff should show change
    let diff2 = diffs[1]["diff"].as_str().expect("diff string");
    assert!(diff2.contains("-const b = 2"), "diff2 should show removed: {}", diff2);
    assert!(diff2.contains("+const b = 20"), "diff2 should show added: {}", diff2);

    // Files unchanged on disk
    assert_eq!(fs::read_to_string(&f1).unwrap(), orig1, "f1 should not be modified by dry-run");
    assert_eq!(fs::read_to_string(&f2).unwrap(), orig2, "f2 should not be modified by dry-run");

    // Cleanup
    let _ = fs::remove_file(&f1);
    let _ = fs::remove_file(&f2);
    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// transaction_empty_operations
// ============================================================================

#[test]
fn transaction_empty_operations() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(
        r#"{"id":"txn-empty","command":"transaction","operations":[]}"#,
    );

    assert_eq!(resp["ok"], false, "empty ops should fail: {:?}", resp);
    assert_eq!(resp["code"], "invalid_request");
    assert!(
        resp["message"].as_str().unwrap().contains("must not be empty"),
        "message should mention empty: {:?}",
        resp
    );

    let status = aft.shutdown();
    assert!(status.success());
}
