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

// ---------------------------------------------------------------------------
// callers command
// ---------------------------------------------------------------------------

/// `callers` without prior `configure` returns not_configured error.
#[test]
fn callgraph_callers_without_configure() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(
        r#"{"id":"1","command":"callers","file":"helpers.ts","symbol":"validate"}"#,
    );

    assert_eq!(resp["ok"], false);
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
    assert_eq!(resp["ok"], true);

    // Get callers of validate in helpers.ts
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));

    assert_eq!(resp["ok"], true, "callers should succeed: {:?}", resp);
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

    assert_eq!(resp["ok"], true, "callers should succeed: {:?}", resp);
    assert_eq!(resp["total_callers"], 0, "main should have no callers");
    let callers = resp["callers"].as_array().expect("callers array");
    assert!(callers.is_empty(), "callers should be empty for entry point");

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

    assert_eq!(resp["ok"], true, "recursive callers should succeed: {:?}", resp);

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

    let resp = aft.send(
        r#"{"id":"1","command":"trace_to","file":"helpers.ts","symbol":"checkFormat"}"#,
    );

    assert_eq!(resp["ok"], false);
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

    assert_eq!(resp["ok"], false, "unknown symbol should fail: {:?}", resp);
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

    assert_eq!(resp["ok"], true, "trace_to should succeed: {:?}", resp);
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

    assert_eq!(resp["ok"], true, "trace_to should succeed: {:?}", resp);
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

    assert_eq!(resp["ok"], true, "trace_to on entry point should succeed: {:?}", resp);
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
    assert_eq!(resp["ok"], true, "configure should succeed: {:?}", resp);

    // Query callers of validate — should show processData from utils.ts
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));
    assert_eq!(resp["ok"], true, "initial callers should succeed: {:?}", resp);
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
    assert_eq!(resp["ok"], true, "callers after add should succeed: {:?}", resp);
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
        .find(|g| {
            g["file"]
                .as_str()
                .unwrap_or("")
                .contains("extra_caller.ts")
        });
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
    assert_eq!(resp["ok"], true, "configure should succeed: {:?}", resp);

    // Query callers of validate — processData from utils.ts should be there
    let resp = aft.send(&format!(
        r#"{{"id":"2","command":"callers","file":"{}/helpers.ts","symbol":"validate","depth":1}}"#,
        root
    ));
    assert_eq!(resp["ok"], true, "initial callers should succeed: {:?}", resp);
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
    assert_eq!(resp["ok"], true, "callers after remove should succeed: {:?}", resp);

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
