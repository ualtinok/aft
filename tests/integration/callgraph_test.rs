//! Integration tests for `configure` and `call_tree` commands.
//!
//! Exercises multi-file call graph traversal through the binary protocol
//! using the fixtures in `tests/fixtures/callgraph/`.

use crate::helpers::{fixture_path, AftProcess};

/// `configure` sets project root and returns success.
#[test]
fn callgraph_configure_sets_project_root() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    assert_eq!(resp["ok"], true, "configure should succeed: {:?}", resp);
    assert_eq!(
        resp["project_root"].as_str().unwrap(),
        root,
        "should echo back the configured root"
    );

    aft.shutdown();
}

/// `configure` with missing param returns error.
#[test]
fn callgraph_configure_missing_param() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(r#"{"id":"1","command":"configure"}"#);

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "invalid_request");

    aft.shutdown();
}

/// `call_tree` without prior `configure` returns not_configured error.
#[test]
fn callgraph_call_tree_without_configure() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(
        r#"{"id":"1","command":"call_tree","file":"main.ts","symbol":"main"}"#,
    );

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "not_configured");

    aft.shutdown();
}

/// `call_tree` on a cross-file call chain returns nested tree.
///
/// main.ts:main → utils.ts:processData → helpers.ts:validate → helpers.ts:checkFormat
#[test]
fn callgraph_cross_file_tree() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    // Configure first
    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));
    assert_eq!(resp["ok"], true);

    // Get call tree for main
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"call_tree","file":"{}/main.ts","symbol":"main","depth":5}}"#,
        root
    ));

    assert_eq!(resp["ok"], true, "call_tree should succeed: {:?}", resp);
    assert_eq!(resp["name"], "main");
    assert_eq!(resp["resolved"], true);

    // main calls processData
    let children = resp["children"].as_array().expect("children should be array");
    let process_data = children
        .iter()
        .find(|c| c["name"] == "processData")
        .expect("main should call processData");

    // processData should be resolved to utils.ts
    assert_eq!(process_data["resolved"], true);
    assert!(
        process_data["file"].as_str().unwrap().contains("utils.ts"),
        "processData should be in utils.ts, got: {}",
        process_data["file"]
    );

    // processData calls validate
    let pd_children = process_data["children"]
        .as_array()
        .expect("processData children");
    let validate = pd_children
        .iter()
        .find(|c| c["name"] == "validate")
        .expect("processData should call validate");

    assert_eq!(validate["resolved"], true);
    assert!(
        validate["file"].as_str().unwrap().contains("helpers.ts"),
        "validate should be in helpers.ts, got: {}",
        validate["file"]
    );

    // validate calls checkFormat (local, so it might be unresolved cross-file
    // but resolved within the same file)
    let v_children = validate["children"].as_array().expect("validate children");
    let check_format = v_children
        .iter()
        .find(|c| c["name"] == "checkFormat");
    assert!(
        check_format.is_some(),
        "validate should call checkFormat, children: {:?}",
        v_children.iter().map(|c| c["name"].clone()).collect::<Vec<_>>()
    );

    aft.shutdown();
}

/// `call_tree` with depth limit truncates the tree.
#[test]
fn callgraph_depth_limit_truncates() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    // Depth 1: main → processData (no deeper)
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"call_tree","file":"{}/main.ts","symbol":"main","depth":1}}"#,
        root
    ));

    assert_eq!(resp["ok"], true);
    assert_eq!(resp["name"], "main");

    let children = resp["children"].as_array().expect("children");
    for child in children {
        let grandchildren = child["children"].as_array();
        match grandchildren {
            Some(gc) => assert!(
                gc.is_empty(),
                "At depth 1, child '{}' should have no grandchildren",
                child["name"]
            ),
            None => {} // null is fine too
        }
    }

    aft.shutdown();
}

/// `call_tree` for an unknown symbol returns error.
#[test]
fn callgraph_unknown_symbol_error() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"call_tree","file":"{}/main.ts","symbol":"nonexistent"}}"#,
        root
    ));

    assert_eq!(resp["ok"], false, "unknown symbol should fail: {:?}", resp);
    assert_eq!(resp["code"], "symbol_not_found");

    aft.shutdown();
}

/// `call_tree` resolves aliased imports (import { validate as checker }).
#[test]
fn callgraph_aliased_import_resolution() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"call_tree","file":"{}/aliased.ts","symbol":"runCheck","depth":3}}"#,
        root
    ));

    assert_eq!(resp["ok"], true, "aliased call_tree should succeed: {:?}", resp);
    assert_eq!(resp["name"], "runCheck");

    // runCheck calls checker (alias for validate)
    let children = resp["children"].as_array().expect("children");

    // The callee should resolve to validate in helpers.ts
    let resolved_child = children
        .iter()
        .find(|c| c["resolved"] == true && c["file"].as_str().unwrap_or("").contains("helpers.ts"));

    assert!(
        resolved_child.is_some(),
        "checker alias should resolve to helpers.ts, children: {:?}",
        children
    );

    aft.shutdown();
}
