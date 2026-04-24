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

    assert_eq!(
        resp["success"], true,
        "configure should succeed: {:?}",
        resp
    );
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

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "invalid_request");

    aft.shutdown();
}

/// `call_tree` without prior `configure` returns not_configured error.
#[test]
fn callgraph_call_tree_without_configure() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(r#"{"id":"1","command":"call_tree","file":"main.ts","symbol":"main"}"#);

    assert_eq!(resp["success"], false);
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
    assert_eq!(resp["success"], true);

    // Get call tree for main
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"call_tree","file":"{}/main.ts","symbol":"main","depth":5}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "call_tree should succeed: {:?}",
        resp
    );
    assert_eq!(resp["name"], "main");
    assert_eq!(resp["resolved"], true);
    assert_eq!(resp["line"], 3, "main definition line should be 1-based");

    // main calls processData
    let children = resp["children"]
        .as_array()
        .expect("children should be array");
    let process_data = children
        .iter()
        .find(|c| c["name"] == "processData")
        .expect("main should call processData");

    // processData should be resolved to utils.ts
    assert_eq!(process_data["resolved"], true);
    assert_eq!(
        process_data["line"], 3,
        "processData line should be 1-based"
    );
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
    assert_eq!(validate["line"], 1, "validate line should be 1-based");
    assert!(
        validate["file"].as_str().unwrap().contains("helpers.ts"),
        "validate should be in helpers.ts, got: {}",
        validate["file"]
    );

    // validate calls checkFormat (local, so it might be unresolved cross-file
    // but resolved within the same file)
    let v_children = validate["children"].as_array().expect("validate children");
    let check_format = v_children.iter().find(|c| c["name"] == "checkFormat");
    assert!(
        check_format.is_some(),
        "validate should call checkFormat, children: {:?}",
        v_children
            .iter()
            .map(|c| c["name"].clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        check_format.unwrap()["line"],
        2,
        "checkFormat line should be 1-based (call site, not definition)"
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

    assert_eq!(resp["success"], true);
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

    assert_eq!(
        resp["success"], false,
        "unknown symbol should fail: {:?}",
        resp
    );
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

    assert_eq!(
        resp["success"], true,
        "aliased call_tree should succeed: {:?}",
        resp
    );
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

// ---------------------------------------------------------------------------
// callers command
// ---------------------------------------------------------------------------

/// `callers` without prior `configure` returns not_configured error.
#[test]
fn callgraph_callers_without_configure() {
    let mut aft = AftProcess::spawn();

    let resp =
        aft.send(r#"{"id":"1","command":"callers","file":"helpers.ts","symbol":"validate"}"#);

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "not_configured");

    aft.shutdown();
}

/// `callers` for a known cross-file call chain returns grouped results.
///
/// helpers.ts:validate is called by utils.ts:processData
#[test]
fn callgraph_callers_cross_file() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    // Configure first
    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));
    assert_eq!(resp["success"], true);

    // Get callers of validate in helpers.ts
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "callers should succeed: {:?}", resp);
    assert_eq!(resp["symbol"], "validate");
    assert!(
        resp["total_callers"].as_u64().unwrap() > 0,
        "validate should have callers"
    );
    assert!(
        resp["scanned_files"].as_u64().unwrap() > 0,
        "should report scanned files"
    );

    // Callers should include processData from utils.ts
    let callers = resp["callers"].as_array().expect("callers array");
    let utils_group = callers
        .iter()
        .find(|g| g["file"].as_str().unwrap_or("").contains("utils.ts"));
    assert!(
        utils_group.is_some(),
        "validate should be called from utils.ts, groups: {:?}",
        callers
    );

    let group = utils_group.unwrap();
    let entries = group["callers"].as_array().expect("callers entries");
    let process_data_caller = entries
        .iter()
        .find(|e| e["symbol"].as_str().unwrap_or("") == "processData");
    assert!(
        process_data_caller.is_some(),
        "validate should be called by processData, entries: {:?}",
        entries
    );
    assert_eq!(
        process_data_caller.unwrap()["line"],
        4,
        "call site line should be 1-based"
    );

    aft.shutdown();
}

/// `callers` for a symbol with no callers returns empty result.
#[test]
fn callgraph_callers_empty_result() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    // main is an entry point — nothing calls it
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/main.ts","symbol":"main"}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "callers should succeed: {:?}", resp);
    assert_eq!(resp["total_callers"], 0, "main should have no callers");
    let callers = resp["callers"].as_array().expect("callers array");
    assert!(
        callers.is_empty(),
        "callers should be empty for entry point"
    );

    aft.shutdown();
}

/// `callers` with recursive depth finds transitive callers.
#[test]
fn callgraph_callers_recursive() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    // checkFormat is called by validate (same file), validate called by processData (utils.ts)
    // With depth 2, we should see transitive callers
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":2}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "recursive callers should succeed: {:?}",
        resp
    );

    // With depth 2, should find transitive callers from main.ts
    // (main → processData → validate)
    let total = resp["total_callers"].as_u64().unwrap();
    assert!(
        total >= 2,
        "with depth 2, validate should have >= 2 callers (direct + transitive), got {}",
        total
    );

    aft.shutdown();
}

// ---------------------------------------------------------------------------
// trace_to command
// ---------------------------------------------------------------------------

/// `trace_to` without prior `configure` returns not_configured error.
#[test]
fn callgraph_trace_to_not_configured() {
    let mut aft = AftProcess::spawn();

    let resp =
        aft.send(r#"{"id":"1","command":"trace_to","file":"helpers.ts","symbol":"checkFormat"}"#);

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "not_configured");

    aft.shutdown();
}

/// `trace_to` on a nonexistent symbol returns symbol_not_found error.
#[test]
fn callgraph_trace_to_symbol_not_found() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_to","file":"{}/helpers.ts","symbol":"nonexistent"}}"#,
        root
    ));

    assert_eq!(
        resp["success"], false,
        "unknown symbol should fail: {:?}",
        resp
    );
    assert_eq!(resp["code"], "symbol_not_found");

    aft.shutdown();
}

/// `trace_to` on a deeply-nested symbol returns a single path through the chain.
///
/// checkFormat is called by validate (helpers.ts), which is called by processData (utils.ts),
/// which is called by main (main.ts). Should return at least one path with main as entry point.
#[test]
fn callgraph_trace_to_single_path() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_to","file":"{}/helpers.ts","symbol":"checkFormat","depth":10}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "trace_to should succeed: {:?}", resp);
    assert_eq!(resp["target_symbol"], "checkFormat");
    assert!(resp["target_file"].as_str().unwrap().contains("helpers.ts"));

    let paths = resp["paths"].as_array().expect("paths should be array");
    assert!(
        !paths.is_empty(),
        "checkFormat should have at least one path to an entry point"
    );

    // At least one path should start at main and end at checkFormat
    let main_path = paths.iter().find(|p| {
        let hops = p["hops"].as_array().unwrap();
        !hops.is_empty() && hops[0]["symbol"] == "main"
    });
    assert!(
        main_path.is_some(),
        "should have a path starting from main, paths: {:?}",
        paths
    );

    // That path should end at checkFormat (last hop)
    let hops = main_path.unwrap()["hops"].as_array().unwrap();
    let last = &hops[hops.len() - 1];
    assert_eq!(
        last["symbol"], "checkFormat",
        "path should end at checkFormat"
    );
    assert_eq!(hops[0]["line"], 3, "entry point line should be 1-based");
    assert_eq!(last["line"], 5, "target line should be 1-based");

    // Verify diagnostic fields exist
    assert!(resp["total_paths"].as_u64().unwrap() >= 1);
    assert!(resp["entry_points_found"].as_u64().unwrap() >= 1);

    aft.shutdown();
}

/// `trace_to` on validate (called from multiple paths) returns multiple paths.
///
/// validate is called by:
/// - main (main.ts) → processData (utils.ts) → validate
/// - handleRequest (service.ts) → processData (utils.ts) → validate
/// - testValidation (test_helpers.ts) → validate (directly)
#[test]
fn callgraph_trace_to_multi_path() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_to","file":"{}/helpers.ts","symbol":"validate","depth":10}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "trace_to should succeed: {:?}", resp);
    assert_eq!(resp["target_symbol"], "validate");

    let total_paths = resp["total_paths"].as_u64().unwrap();
    assert!(
        total_paths >= 2,
        "validate should have multiple paths to entry points, got {}",
        total_paths
    );

    let entry_points_found = resp["entry_points_found"].as_u64().unwrap();
    assert!(
        entry_points_found >= 2,
        "validate should have multiple distinct entry points, got {}",
        entry_points_found
    );

    let paths = resp["paths"].as_array().expect("paths should be array");

    // Collect entry point names (first hop of each path)
    let entry_names: Vec<&str> = paths
        .iter()
        .filter_map(|p| {
            let hops = p["hops"].as_array()?;
            hops.first().and_then(|h| h["symbol"].as_str())
        })
        .collect();

    // Should include both main and testValidation (or handleRequest) as entry points
    assert!(
        entry_names.contains(&"main") || entry_names.contains(&"handleRequest"),
        "should have main or handleRequest as entry point, got: {:?}",
        entry_names
    );

    // Each path should end at validate (the target)
    for path in paths {
        let hops = path["hops"].as_array().unwrap();
        let last = &hops[hops.len() - 1];
        assert_eq!(
            last["symbol"], "validate",
            "every path should end at validate"
        );
    }

    aft.shutdown();
}

/// `trace_to` on an entry point itself returns gracefully (no paths or self-path).
///
/// main is an entry point — there's nothing calling it, so trace_to should return
/// an empty paths array or a minimal self-path.
#[test]
fn callgraph_trace_to_no_entry_points() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_to","file":"{}/main.ts","symbol":"main","depth":10}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "trace_to on entry point should succeed: {:?}",
        resp
    );
    assert_eq!(resp["target_symbol"], "main");

    // main IS an entry point, so the result should handle it gracefully.
    // It may return a self-path (just [main]) or empty paths.
    let paths = resp["paths"].as_array().expect("paths should be array");

    // If there are paths, each should contain main
    for path in paths {
        let hops = path["hops"].as_array().unwrap();
        let has_main = hops.iter().any(|h| h["symbol"] == "main");
        assert!(has_main, "any path for main should include main");
    }

    // Diagnostic fields should be present and valid
    assert!(resp.get("total_paths").is_some());
    assert!(resp.get("entry_points_found").is_some());
    assert!(resp.get("truncated_paths").is_some());

    aft.shutdown();
}

// ---------------------------------------------------------------------------
// file watcher invalidation cycle
// ---------------------------------------------------------------------------

/// Helper: copy a fixture directory into a temp dir for watcher tests.
/// Returns the temp dir (auto-cleaned on drop) and its path as a String.
fn setup_watcher_fixture() -> (tempfile::TempDir, String) {
    let fixtures = fixture_path("callgraph");
    let tmp = tempfile::tempdir().expect("create temp dir");

    // Copy all fixture files into the temp dir
    for entry in std::fs::read_dir(&fixtures).expect("read fixtures dir") {
        let entry = entry.expect("read entry");
        let src = entry.path();
        if src.is_file() {
            let dst = tmp.path().join(entry.file_name());
            std::fs::copy(&src, &dst).expect("copy fixture file");
        }
    }

    let root = tmp.path().display().to_string();
    (tmp, root)
}

/// File watcher: modify a file to add a new caller, verify it appears.
///
/// configure → callers for validate → add new caller in a new file →
/// wait for OS event delivery → send command (triggers drain) →
/// callers again → assert new caller appears.
#[test]
fn callgraph_watcher_add_caller() {
    let (_tmp, root) = setup_watcher_fixture();
    let mut aft = AftProcess::spawn();

    // Configure with temp dir
    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));
    assert_eq!(
        resp["success"], true,
        "configure should succeed: {:?}",
        resp
    );

    // Query callers of validate — should show processData from utils.ts
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));
    assert_eq!(
        resp["success"], true,
        "initial callers should succeed: {:?}",
        resp
    );
    let initial_total = resp["total_callers"].as_u64().unwrap();
    assert!(initial_total > 0, "validate should have initial callers");

    // Write a new file that calls validate
    let new_file = std::path::Path::new(&root).join("extra_caller.ts");
    std::fs::write(
        &new_file,
        r#"import { validate } from './helpers';

export function extraCheck(input: string): boolean {
    return validate(input);
}
"#,
    )
    .expect("write new caller file");

    // Wait for OS file events to be delivered
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Send ping to trigger drain_watcher_events
    aft.send(r#"{"id":"3","command":"ping"}"#);

    // Query callers again — should include the new caller
    let resp = aft.send(&format!(
        r#"{{"id":"4","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));
    assert_eq!(
        resp["success"], true,
        "callers after add should succeed: {:?}",
        resp
    );
    let new_total = resp["total_callers"].as_u64().unwrap();
    assert!(
        new_total > initial_total,
        "adding a caller should increase total_callers: initial={}, new={}",
        initial_total,
        new_total
    );

    // Verify the new caller file appears in the results
    let callers = resp["callers"].as_array().expect("callers array");
    let extra_group = callers
        .iter()
        .find(|g| g["file"].as_str().unwrap_or("").contains("extra_caller.ts"));
    assert!(
        extra_group.is_some(),
        "new caller from extra_caller.ts should appear, callers: {:?}",
        callers
    );

    aft.shutdown();
}

/// File watcher: remove a call from a file, verify it disappears.
///
/// configure → callers for validate → modify utils.ts to remove the validate
/// call → wait for OS event delivery → send command (triggers drain) →
/// callers again → assert the removed caller is gone.
#[test]
fn callgraph_watcher_remove_caller() {
    let (_tmp, root) = setup_watcher_fixture();
    let mut aft = AftProcess::spawn();

    // Configure with temp dir
    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));
    assert_eq!(
        resp["success"], true,
        "configure should succeed: {:?}",
        resp
    );

    // Query callers of validate — processData from utils.ts should be there
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));
    assert_eq!(
        resp["success"], true,
        "initial callers should succeed: {:?}",
        resp
    );
    let callers = resp["callers"].as_array().expect("callers array");
    let utils_group = callers
        .iter()
        .find(|g| g["file"].as_str().unwrap_or("").contains("utils.ts"));
    assert!(
        utils_group.is_some(),
        "validate should initially be called from utils.ts"
    );

    // Rewrite utils.ts to remove the validate() call
    let utils_path = std::path::Path::new(&root).join("utils.ts");
    std::fs::write(
        &utils_path,
        r#"export function processData(input: string): string {
    // validate call removed
    return input.toUpperCase();
}
"#,
    )
    .expect("rewrite utils.ts");

    // Wait for OS file events to be delivered
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Send ping to trigger drain_watcher_events
    aft.send(r#"{"id":"3","command":"ping"}"#);

    // Query callers again — utils.ts should no longer appear
    let resp = aft.send(&format!(
        r#"{{"id":"4","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));
    assert_eq!(
        resp["success"], true,
        "callers after remove should succeed: {:?}",
        resp
    );

    let callers = resp["callers"].as_array().expect("callers array");
    let utils_group = callers
        .iter()
        .find(|g| g["file"].as_str().unwrap_or("").contains("utils.ts"));

    // utils.ts no longer imports or calls validate, so it should not appear
    if let Some(group) = utils_group {
        let entries = group["callers"].as_array().expect("callers entries");
        let validate_caller = entries
            .iter()
            .find(|e| e["callee"].as_str().unwrap_or("") == "validate");
        assert!(
            validate_caller.is_none(),
            "validate call should be removed from utils.ts, entries: {:?}",
            entries
        );
    }

    aft.shutdown();
}

// ---------------------------------------------------------------------------
// impact command
// ---------------------------------------------------------------------------

/// `impact` without prior `configure` returns not_configured error.
#[test]
fn callgraph_impact_not_configured() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(r#"{"id":"1","command":"impact","file":"helpers.ts","symbol":"validate"}"#);

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "not_configured");

    aft.shutdown();
}

/// `impact` on a nonexistent symbol returns symbol_not_found error.
#[test]
fn callgraph_impact_symbol_not_found() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"impact","file":"{}/helpers.ts","symbol":"nonexistent"}}"#,
        root
    ));

    assert_eq!(
        resp["success"], false,
        "unknown symbol should fail: {:?}",
        resp
    );
    assert_eq!(resp["code"], "symbol_not_found");

    aft.shutdown();
}

/// `impact` on validate (called from multiple files) returns enriched callers.
///
/// validate is called from:
/// - utils.ts (processData)
/// - test_helpers.ts (testValidation)
/// Should return callers with signatures, entry point flags, and call expressions.
#[test]
fn callgraph_impact_multi_caller() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"impact","file":"{}/helpers.ts","symbol":"validate","depth":5}}"#,
        root
    ));

    assert_eq!(resp["success"], true, "impact should succeed: {:?}", resp);
    assert_eq!(resp["symbol"], "validate");
    assert!(resp["file"].as_str().unwrap().contains("helpers.ts"));

    // Should have at least 2 affected callers (processData in utils.ts + testValidation in test_helpers.ts)
    let total_affected = resp["total_affected"].as_u64().unwrap();
    assert!(
        total_affected >= 2,
        "validate should have at least 2 affected callers, got {}",
        total_affected
    );

    // Should have at least 2 affected files
    let affected_files = resp["affected_files"].as_u64().unwrap();
    assert!(
        affected_files >= 2,
        "validate should affect at least 2 files, got {}",
        affected_files
    );

    // Callers array should exist and be non-empty
    let callers = resp["callers"].as_array().expect("callers array");
    assert!(!callers.is_empty(), "callers should not be empty");

    // Each caller should have required fields
    for caller in callers {
        assert!(
            caller.get("caller_symbol").is_some(),
            "caller should have caller_symbol: {:?}",
            caller
        );
        assert!(
            caller.get("caller_file").is_some(),
            "caller should have caller_file: {:?}",
            caller
        );
        assert!(
            caller.get("line").is_some(),
            "caller should have line: {:?}",
            caller
        );
        assert!(
            caller["line"].as_u64().unwrap_or(0) >= 1,
            "caller line should be 1-based: {:?}",
            caller
        );
        assert!(
            caller.get("is_entry_point").is_some(),
            "caller should have is_entry_point: {:?}",
            caller
        );
        assert!(
            caller.get("parameters").is_some(),
            "caller should have parameters: {:?}",
            caller
        );
    }

    // At least one caller should have is_entry_point set
    // (testValidation starts with "test", processData is called by main which is an entry point)
    // With depth 5, we get transitive callers — main should be an entry point
    let has_entry_point = callers
        .iter()
        .any(|c| c["is_entry_point"].as_bool() == Some(true));
    assert!(
        has_entry_point,
        "at least one caller should be an entry point, callers: {:?}",
        callers
    );

    // Target signature should be present
    assert!(
        resp.get("signature").is_some(),
        "target should have a signature"
    );

    // Parameters should be present (validate takes `input: string`)
    let params = resp["parameters"].as_array().expect("parameters array");
    assert!(
        params.iter().any(|p| p.as_str() == Some("input")),
        "validate parameters should include 'input', got: {:?}",
        params
    );

    aft.shutdown();
}

// ---------------------------------------------------------------------------
// trace_data tests
// ---------------------------------------------------------------------------

/// `trace_data` without configure returns not_configured error.
#[test]
fn callgraph_trace_data_not_configured() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(
        r#"{"id":"1","command":"trace_data","file":"data_flow.ts","symbol":"transformData","expression":"rawInput"}"#,
    );

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "not_configured");

    aft.shutdown();
}

/// `trace_data` on a nonexistent symbol returns symbol_not_found error.
#[test]
fn callgraph_trace_data_symbol_not_found() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_data","file":"{}/data_flow.ts","symbol":"nonexistent","expression":"x"}}"#,
        root
    ));

    assert_eq!(
        resp["success"], false,
        "unknown symbol should fail: {:?}",
        resp
    );
    assert_eq!(resp["code"], "symbol_not_found");

    aft.shutdown();
}

/// `trace_data` tracks an expression through a local assignment within a function.
///
/// In data_flow.ts:
///   export function transformData(rawInput: string): string {
///       const cleaned = rawInput;   // assignment hop: rawInput → cleaned
///       const result = processInput(cleaned);  // parameter hop: cleaned → input in processInput
///       return result;
///   }
#[test]
fn callgraph_trace_data_assignment_tracking() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_data","file":"{}/data_flow.ts","symbol":"transformData","expression":"rawInput","depth":5}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "trace_data should succeed: {:?}",
        resp
    );
    assert_eq!(resp["expression"], "rawInput");
    assert!(
        resp["origin_file"]
            .as_str()
            .unwrap()
            .contains("data_flow.ts"),
        "origin_file should reference data_flow.ts"
    );
    assert_eq!(resp["origin_symbol"], "transformData");

    let hops = resp["hops"].as_array().expect("hops array");
    assert!(
        !hops.is_empty(),
        "should have at least one hop (assignment rawInput → cleaned)"
    );

    // First hop should be an assignment: rawInput → cleaned
    let first = &hops[0];
    assert_eq!(
        first["flow_type"], "assignment",
        "first hop should be assignment"
    );
    assert_eq!(first["variable"], "cleaned", "should track to 'cleaned'");
    assert_eq!(
        first["approximate"], false,
        "direct assignment is not approximate"
    );
    assert_eq!(first["line"], 4, "assignment line should be 1-based");

    aft.shutdown();
}

/// `trace_data` tracks across file boundaries via argument-to-parameter matching.
///
/// In data_flow.ts, `transformData` calls `processInput(cleaned)`.
/// `processInput` is defined in data_processor.ts with parameter `input`.
/// So the flow should be: rawInput → cleaned (assignment) → input (parameter in processInput).
#[test]
fn callgraph_trace_data_cross_file() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_data","file":"{}/data_flow.ts","symbol":"transformData","expression":"rawInput","depth":5}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "trace_data should succeed: {:?}",
        resp
    );

    let hops = resp["hops"].as_array().expect("hops array");
    assert!(
        hops.len() >= 2,
        "should have at least 2 hops (assignment + cross-file parameter), got {}: {:?}",
        hops.len(),
        hops
    );

    // Should have a parameter hop pointing to data_processor.ts
    let has_param_hop = hops.iter().any(|h| {
        h["flow_type"] == "parameter"
            && h["file"]
                .as_str()
                .map(|f| f.contains("data_processor.ts"))
                .unwrap_or(false)
    });
    assert!(
        has_param_hop,
        "should have a parameter hop into data_processor.ts, hops: {:?}",
        hops
    );

    // Parameter hop should map to 'input' (processInput's first parameter)
    let param_hop = hops.iter().find(|h| {
        h["flow_type"] == "parameter"
            && h["file"]
                .as_str()
                .map(|f| f.contains("data_processor.ts"))
                .unwrap_or(false)
    });
    if let Some(ph) = param_hop {
        assert_eq!(
            ph["variable"], "input",
            "parameter should be 'input' (processInput's parameter)"
        );
        assert_eq!(ph["approximate"], false);
        assert_eq!(ph["line"], 1, "parameter line should be 1-based");
    }

    aft.shutdown();
}

/// `trace_data` marks destructuring as an approximate hop.
///
/// In data_flow.ts:
///   export function complexFlow(data: string): void {
///       const { name, value } = JSON.parse(data);  // destructuring — approximate
///   }
#[test]
fn callgraph_trace_data_approximation() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_data","file":"{}/data_flow.ts","symbol":"complexFlow","expression":"data","depth":5}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "trace_data should succeed: {:?}",
        resp
    );

    let hops = resp["hops"].as_array().expect("hops array");

    // Should have at least one approximate hop (the destructuring)
    let has_approximate = hops.iter().any(|h| h["approximate"] == true);
    assert!(
        has_approximate,
        "destructuring should produce an approximate hop, hops: {:?}",
        hops
    );

    aft.shutdown();
}

// ---------------------------------------------------------------------------
// max_callgraph_files guard: project_too_large error
//
// These tests configure a low cap so the guard trips deterministically. They
// verify that huge roots no longer exhaust the bridge timeout — the user
// reported this hitting ~/Work/OSS (557K files) in v0.15.0.
// ---------------------------------------------------------------------------

/// `configure` on a small repo leaves `source_file_count_exceeds_max` false so
/// plugins do not surface a spurious large-repo warning.
#[test]
fn callgraph_configure_small_repo_does_not_flag_exceeds() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    // Default cap is 20_000; the 9-file fixture is nowhere near it.
    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}"}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "configure should succeed: {:?}",
        resp
    );
    assert_eq!(
        resp["source_file_count_exceeds_max"], false,
        "small fixture with default cap should NOT be flagged as exceeding"
    );
    // Real source_file_count reported when under the cap.
    let count = resp["source_file_count"].as_u64().unwrap_or(0);
    assert!(
        count > 0 && count < 100,
        "small fixture should report a real (non-capped) count, got {}",
        count
    );

    aft.shutdown();
}

/// `configure` with `max_callgraph_files` below the project size reports the
/// large-repo condition in its response, so plugins can surface a warning.
#[test]
fn callgraph_configure_reports_source_file_count_exceeds_max() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":1}}"#,
        root
    ));

    assert_eq!(
        resp["success"], true,
        "configure should succeed: {:?}",
        resp
    );
    assert_eq!(
        resp["source_file_count_exceeds_max"], true,
        "9-file fixture with cap=1 should be flagged as exceeding max"
    );
    assert_eq!(resp["max_callgraph_files"], 1);

    aft.shutdown();
}

/// `callers` returns `project_too_large` when project exceeds `max_callgraph_files`.
#[test]
fn callgraph_callers_project_too_large() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    // Configure with cap=1 so the 9-file fixture trips the guard.
    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":1}}"#,
        root
    ));
    assert_eq!(resp["success"], true);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "project_too_large");
    // Error message should mention max_callgraph_files so users know what to tune.
    let msg = resp["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("max_callgraph_files"),
        "error message should mention max_callgraph_files: {}",
        msg
    );

    aft.shutdown();
}

/// `trace_to` returns `project_too_large` when project exceeds `max_callgraph_files`.
#[test]
fn callgraph_trace_to_project_too_large() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":1}}"#,
        root
    ));
    assert_eq!(resp["success"], true);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_to","file":"{}/helpers.ts","symbol":"validate","depth":5}}"#,
        root
    ));

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "project_too_large");

    aft.shutdown();
}

/// `impact` returns `project_too_large` when project exceeds `max_callgraph_files`.
#[test]
fn callgraph_impact_project_too_large() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":1}}"#,
        root
    ));
    assert_eq!(resp["success"], true);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"impact","file":"{}/helpers.ts","symbol":"validate"}}"#,
        root
    ));

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "project_too_large");

    aft.shutdown();
}

/// `trace_data` returns `project_too_large` when project exceeds `max_callgraph_files`.
#[test]
fn callgraph_trace_data_project_too_large() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":1}}"#,
        root
    ));
    assert_eq!(resp["success"], true);

    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"trace_data","file":"{}/data_flow.ts","symbol":"transformData","expression":"rawInput"}}"#,
        root
    ));

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "project_too_large");

    aft.shutdown();
}

/// `configure` rejects `max_callgraph_files: 0` instead of silently clamping.
/// Regression test for Oracle v0.15.1 review blocker: sub-1 values must surface
/// as `invalid_request` so user typos are visible.
#[test]
fn callgraph_configure_rejects_zero_max_callgraph_files() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":0}}"#,
        root
    ));

    assert_eq!(resp["success"], false, "configure should reject 0");
    assert_eq!(resp["code"], "invalid_request");
    let msg = resp["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("max_callgraph_files"),
        "error message should mention max_callgraph_files: {}",
        msg
    );

    aft.shutdown();
}

/// `configure` rejects negative `max_callgraph_files` (via JSON number → `as_u64` returning None).
#[test]
fn callgraph_configure_rejects_negative_max_callgraph_files() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":-5}}"#,
        root
    ));

    assert_eq!(resp["success"], false, "configure should reject -5");
    assert_eq!(resp["code"], "invalid_request");

    aft.shutdown();
}

/// `configure` accepts any positive `max_callgraph_files` and reflects it back.
/// Paired negative-cases above to prove the validator is not rejecting valid input.
#[test]
fn callgraph_configure_accepts_positive_max_callgraph_files() {
    let mut aft = AftProcess::spawn();
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    let resp = aft.send(&format!(
        r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":42}}"#,
        root
    ));

    assert_eq!(resp["success"], true);
    assert_eq!(resp["max_callgraph_files"], 42);

    aft.shutdown();
}

/// `configure` rejects non-integer `max_callgraph_files` payloads with a clear
/// `invalid_request` surface rather than silently clamping. `serde_json::Value::as_u64`
/// returns `None` for floats, strings, booleans, nulls, and compound types,
/// which are all funneled through the same rejection path.
///
/// Covers the follow-up gap Oracle flagged on v0.15.1: the predicate's truth
/// table is correct by source inspection, but explicit regression tests only
/// existed for integer cases (0, negative, positive). Added v0.15.2.
#[test]
fn callgraph_configure_rejects_non_integer_max_callgraph_files_payloads() {
    let fixtures = fixture_path("callgraph");
    let root = fixtures.display().to_string();

    // Each payload is a JSON fragment that will be inlined into the configure
    // request. All should be rejected.
    let rejected_payloads = [
        ("float", "1.5"),
        ("string", "\"twenty\""),
        ("numeric_string", "\"20000\""),
        ("bool_true", "true"),
        ("bool_false", "false"),
        ("null", "null"),
        ("array", "[]"),
        ("object", "{}"),
    ];

    for (label, payload) in rejected_payloads {
        let mut aft = AftProcess::spawn();
        let resp = aft.send(&format!(
            r#"{{"id":"1","command":"configure","project_root":"{}","max_callgraph_files":{}}}"#,
            root, payload
        ));

        assert_eq!(
            resp["success"], false,
            "configure should reject {label} payload ({payload})"
        );
        assert_eq!(
            resp["code"], "invalid_request",
            "configure should return invalid_request for {label} payload ({payload})"
        );
        let msg = resp["message"].as_str().unwrap_or("");
        assert!(
            msg.contains("max_callgraph_files"),
            "error message should mention max_callgraph_files for {label}: {msg}"
        );

        aft.shutdown();
    }
}
