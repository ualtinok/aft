//! Integration tests for the outline command through the binary protocol.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// A handle to a running aft process with piped I/O.
struct AftProcess {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl AftProcess {
    fn spawn() -> Self {
        let binary = env!("CARGO_BIN_EXE_aft");
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn aft binary");

        let stdout = child.stdout.take().expect("stdout handle");
        let reader = BufReader::new(stdout);

        AftProcess { child, reader }
    }

    fn send(&mut self, request: &str) -> serde_json::Value {
        let stdin = self.child.stdin.as_mut().expect("stdin handle");
        writeln!(stdin, "{}", request).expect("write to stdin");
        stdin.flush().expect("flush stdin");

        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read from stdout");
        assert!(
            !line.is_empty(),
            "expected a response line but got EOF from aft"
        );
        serde_json::from_str(line.trim()).expect("parse response JSON")
    }

    fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.child.stdin.take());
        self.child.wait().expect("wait for process exit")
    }
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn test_outline_typescript_nested_structure() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("sample.ts");

    let resp = aft.send(&format!(
        r#"{{"id":"ol-1","command":"outline","file":"{}"}}"#,
        file.display()
    ));

    assert_eq!(resp["id"], "ol-1");
    assert_eq!(resp["ok"], true, "outline should succeed");

    let entries = resp["entries"].as_array().expect("entries should be array");

    // Collect top-level names
    let top_names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();

    // UserService should be at top level
    assert!(
        top_names.contains(&"UserService"),
        "UserService should be at top level, got: {:?}",
        top_names
    );

    // Methods should NOT be at top level
    assert!(
        !top_names.contains(&"getUser"),
        "getUser should NOT be at top level"
    );
    assert!(
        !top_names.contains(&"addUser"),
        "addUser should NOT be at top level"
    );

    // Methods should be nested under UserService
    let user_service = entries
        .iter()
        .find(|e| e["name"] == "UserService")
        .expect("UserService entry");
    let members = user_service["members"]
        .as_array()
        .expect("members array");
    let member_names: Vec<&str> = members
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert!(
        member_names.contains(&"getUser"),
        "getUser should be under UserService"
    );
    assert!(
        member_names.contains(&"addUser"),
        "addUser should be under UserService"
    );

    // Verify all expected symbol kinds are present
    let all_kinds: Vec<&str> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(all_kinds.contains(&"function"), "should have function kind");
    assert!(all_kinds.contains(&"class"), "should have class kind");
    assert!(
        all_kinds.contains(&"interface"),
        "should have interface kind"
    );
    assert!(all_kinds.contains(&"enum"), "should have enum kind");
    assert!(
        all_kinds.contains(&"type_alias"),
        "should have type_alias kind"
    );

    // Verify exported flag
    let greet = entries
        .iter()
        .find(|e| e["name"] == "greet")
        .expect("greet entry");
    assert_eq!(greet["exported"], true, "greet should be exported");

    let internal = entries
        .iter()
        .find(|e| e["name"] == "internalHelper")
        .expect("internalHelper entry");
    assert_eq!(
        internal["exported"], false,
        "internalHelper should not be exported"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_outline_python_multi_level_nesting() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("sample.py");

    let resp = aft.send(&format!(
        r#"{{"id":"ol-py","command":"outline","file":"{}"}}"#,
        file.display()
    ));

    assert_eq!(resp["ok"], true, "outline should succeed for Python");

    let entries = resp["entries"].as_array().expect("entries array");
    let top_names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();

    // OuterClass should be at top level
    assert!(top_names.contains(&"OuterClass"));

    // InnerClass and inner_method should NOT be at top level
    assert!(
        !top_names.contains(&"InnerClass"),
        "InnerClass should be nested"
    );
    assert!(
        !top_names.contains(&"inner_method"),
        "inner_method should be nested"
    );

    // Navigate: OuterClass → InnerClass → inner_method
    let outer = entries
        .iter()
        .find(|e| e["name"] == "OuterClass")
        .expect("OuterClass");
    let outer_members = outer["members"].as_array().expect("outer members");

    let inner = outer_members
        .iter()
        .find(|m| m["name"] == "InnerClass")
        .expect("InnerClass under OuterClass");
    let inner_members = inner["members"].as_array().expect("inner members");

    assert!(
        inner_members.iter().any(|m| m["name"] == "inner_method"),
        "inner_method should be under InnerClass"
    );

    // outer_method should be under OuterClass (not under InnerClass)
    assert!(
        outer_members
            .iter()
            .any(|m| m["name"] == "outer_method"),
        "outer_method should be under OuterClass"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_outline_missing_file() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(
        r#"{"id":"ol-miss","command":"outline","file":"/nonexistent/path/to/file.ts"}"#,
    );

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "file_not_found");
    assert!(resp["message"]
        .as_str()
        .unwrap()
        .contains("file not found"));

    // Process should still be alive
    let resp = aft.send(r#"{"id":"alive","command":"ping"}"#);
    assert_eq!(resp["ok"], true);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_outline_missing_param() {
    let mut aft = AftProcess::spawn();

    let resp = aft.send(r#"{"id":"ol-nop","command":"outline"}"#);

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "invalid_request");
    assert!(resp["message"].as_str().unwrap().contains("file"));

    let status = aft.shutdown();
    assert!(status.success());
}

// ============================================================================
// Zoom command integration tests
// ============================================================================

#[test]
fn test_zoom_success_with_annotations() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("calls.ts");

    let resp = aft.send(&format!(
        r#"{{"id":"z-1","command":"zoom","file":"{}","symbol":"compute"}}"#,
        file.display()
    ));

    assert_eq!(resp["id"], "z-1");
    assert_eq!(resp["ok"], true, "zoom should succeed: {:?}", resp);
    assert_eq!(resp["name"], "compute");
    assert_eq!(resp["kind"], "function");

    // Content should contain the function body
    let content = resp["content"].as_str().expect("content string");
    assert!(
        content.contains("function compute"),
        "content should have function declaration: {}",
        content
    );
    assert!(
        content.contains("helper(a)"),
        "content should contain call to helper: {}",
        content
    );

    // Range should be present
    assert!(resp["range"]["start_line"].is_number());
    assert!(resp["range"]["end_line"].is_number());

    // Annotations
    let calls_out = resp["annotations"]["calls_out"]
        .as_array()
        .expect("calls_out array");
    let out_names: Vec<&str> = calls_out
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        out_names.contains(&"helper"),
        "compute should call helper: {:?}",
        out_names
    );

    let called_by = resp["annotations"]["called_by"]
        .as_array()
        .expect("called_by array");
    let by_names: Vec<&str> = called_by
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert!(
        by_names.contains(&"orchestrate"),
        "orchestrate should call compute: {:?}",
        by_names
    );

    // Each CallRef should have a line number
    for cr in calls_out {
        assert!(cr["line"].is_number(), "CallRef should have line: {:?}", cr);
    }
    for cr in called_by {
        assert!(cr["line"].is_number(), "CallRef should have line: {:?}", cr);
    }

    // Context lines
    let ctx_before = resp["context_before"]
        .as_array()
        .expect("context_before array");
    let ctx_after = resp["context_after"]
        .as_array()
        .expect("context_after array");
    assert!(ctx_before.len() <= 3, "default context_lines is 3");
    assert!(ctx_after.len() <= 3, "default context_lines is 3");

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_zoom_symbol_not_found() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("calls.ts");

    let resp = aft.send(&format!(
        r#"{{"id":"z-nf","command":"zoom","file":"{}","symbol":"nonexistent_fn"}}"#,
        file.display()
    ));

    assert_eq!(resp["ok"], false);
    assert_eq!(resp["code"], "symbol_not_found");
    assert!(resp["message"]
        .as_str()
        .unwrap()
        .contains("nonexistent_fn"));

    // Process should still be alive
    let resp = aft.send(r#"{"id":"alive","command":"ping"}"#);
    assert_eq!(resp["ok"], true);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_zoom_context_lines_param() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("calls.ts");

    // Use context_lines=1
    let resp = aft.send(&format!(
        r#"{{"id":"z-cl","command":"zoom","file":"{}","symbol":"compute","context_lines":1}}"#,
        file.display()
    ));

    assert_eq!(resp["ok"], true);

    let ctx_before = resp["context_before"].as_array().unwrap();
    let ctx_after = resp["context_after"].as_array().unwrap();
    assert!(
        ctx_before.len() <= 1,
        "context_before should be ≤1 with context_lines=1: {:?}",
        ctx_before
    );
    assert!(
        ctx_after.len() <= 1,
        "context_after should be ≤1 with context_lines=1: {:?}",
        ctx_after
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn test_zoom_empty_annotations_arrays() {
    let mut aft = AftProcess::spawn();
    let file = fixture_path("calls.ts");

    // `unused` has no known callers and calls no known symbols
    let resp = aft.send(&format!(
        r#"{{"id":"z-empty","command":"zoom","file":"{}","symbol":"unused"}}"#,
        file.display()
    ));

    assert_eq!(resp["ok"], true);

    // called_by must be empty array, not null
    let called_by = resp["annotations"]["called_by"]
        .as_array()
        .expect("called_by should be array, not null");
    assert!(called_by.is_empty());

    // calls_out must be an array (possibly empty depending on known symbols)
    let calls_out = resp["annotations"]["calls_out"]
        .as_array()
        .expect("calls_out should be array, not null");
    // It's fine if calls_out has items — what matters is it's an array
    let _ = calls_out;

    let status = aft.shutdown();
    assert!(status.success());
}
