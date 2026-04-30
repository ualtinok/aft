use std::fs;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use super::helpers::AftProcess;

#[test]
fn edit_match_glob_rolls_back_prior_files_when_later_write_fails() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = root.join("a.ts");
    let b = root.join("b.ts");
    let c = root.join("c.ts");

    fs::write(&a, "const a = \"OLD\";\n").unwrap();
    fs::write(&b, "const b = \"OLD\";\n").unwrap();
    fs::write(&c, "const c = \"OLD\";\n").unwrap();

    let mut readonly = fs::metadata(&b).unwrap().permissions();
    readonly.set_readonly(true);
    fs::set_permissions(&b, readonly).unwrap();

    let mut aft = AftProcess::spawn();
    let req = json!({
        "id": "glob-rollback-write-failure",
        "command": "edit_match",
        "file": format!("{}/*.ts", root.display()),
        "match": "OLD",
        "replacement": "NEW"
    });
    let resp = aft.send(&req.to_string());

    make_writable(&b);

    assert_eq!(resp["success"], false, "glob edit should fail: {resp:?}");
    assert_eq!(resp["code"], "write_error");
    assert_eq!(fs::read_to_string(&a).unwrap(), "const a = \"OLD\";\n");
    assert_eq!(fs::read_to_string(&b).unwrap(), "const b = \"OLD\";\n");
    assert_eq!(fs::read_to_string(&c).unwrap(), "const c = \"OLD\";\n");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn edit_match_glob_reports_per_file_syntax_failure_and_preserves_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = root.join("a.ts");
    let b = root.join("b.ts");
    let c = root.join("c.ts");

    let original_a = "const a = 1; // TARGET\n";
    let original_b = "const b = \"TARGET\";\n";
    let original_c = "const c = TARGET;\n";
    fs::write(&a, original_a).unwrap();
    fs::write(&b, original_b).unwrap();
    fs::write(&c, original_c).unwrap();

    let mut aft = AftProcess::spawn();
    let req = json!({
        "id": "glob-rollback-syntax-failure",
        "command": "edit_match",
        "file": format!("{}/*.ts", root.display()),
        "match": "TARGET",
        "replacement": "{;"
    });
    let resp = aft.send(&req.to_string());

    assert_eq!(
        resp["success"], true,
        "glob edit should return per-file status: {resp:?}"
    );
    let files = resp["files"].as_array().expect("files array");
    assert_eq!(files.len(), 3);
    assert!(files.iter().any(|file| file["file"]
        .as_str()
        .is_some_and(|name| name.ends_with("/c.ts"))
        && file["syntax_valid"] == false));
    assert_eq!(fs::read_to_string(&a).unwrap(), "const a = 1; // {;\n");
    assert_eq!(fs::read_to_string(&b).unwrap(), "const b = \"{;\";\n");
    assert_eq!(fs::read_to_string(&c).unwrap(), "const c = {;;\n");

    let status = aft.shutdown();
    assert!(status.success());
}

#[cfg(unix)]
fn make_writable(path: &std::path::Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(windows)]
fn make_writable(path: &std::path::Path) {
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
}
