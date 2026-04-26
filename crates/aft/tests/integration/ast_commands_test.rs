//! Integration tests for `ast_search` and `ast_replace` through the binary protocol.

use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use super::helpers::AftProcess;

fn setup_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp_dir = tempfile::tempdir().expect("create temp dir");

    for (relative_path, content) in files {
        let path = temp_dir.path().join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(path, content).expect("write fixture file");
    }

    temp_dir
}

fn configure(aft: &mut AftProcess, root: &Path) {
    let resp = aft.configure(root);
    assert_eq!(resp["success"], true, "configure should succeed: {resp:?}");
}

fn send(aft: &mut AftProcess, request: Value) -> Value {
    aft.send(&serde_json::to_string(&request).expect("serialize request"))
}

fn read_file(root: &Path, relative_path: &str) -> String {
    fs::read_to_string(root.join(relative_path)).expect("read file")
}

fn count_occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

fn file_result<'a>(resp: &'a Value, suffix: &str) -> &'a Value {
    resp["files"]
        .as_array()
        .expect("files array")
        .iter()
        .find(|entry| entry["file"].as_str().expect("file path").ends_with(suffix))
        .unwrap_or_else(|| panic!("missing file result for suffix {suffix}: {resp:?}"))
}

#[test]
fn ast_replace_replaces_every_match_in_single_typescript_file() {
    let project = setup_project(&[(
        "sample.ts",
        "console.log(first);\nif (ready) {\n  console.log(second);\n}\nconsole.log(third);\n",
    )]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "search-single",
            "command": "ast_search",
            "pattern": "console.log($ARG)",
            "lang": "typescript",
        }),
    );

    assert_eq!(
        search["success"], true,
        "ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 3);
    assert_eq!(search["files_with_matches"], 1);

    let matched_args: Vec<&str> = search["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|m| m["meta_variables"]["$ARG"].as_str().expect("captured arg"))
        .collect();
    assert_eq!(matched_args, vec!["first", "second", "third"]);

    let replace = send(
        &mut aft,
        json!({
            "id": "replace-single",
            "command": "ast_replace",
            "pattern": "console.log($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "typescript",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["dry_run"], false);
    assert_eq!(replace["total_replacements"], 3);
    assert_eq!(replace["total_files"], 1);
    assert_eq!(replace["files_with_matches"], 1);

    let file_entry = &replace["files"].as_array().expect("files array")[0];
    assert_eq!(file_entry["replacements"], 3);
    assert!(file_entry["backup_id"].as_str().is_some());

    let updated = read_file(project.path(), "sample.ts");
    assert_eq!(count_occurrences(&updated, "logger.info("), 3);
    assert_eq!(count_occurrences(&updated, "console.log("), 0);
    assert!(updated.contains("logger.info(first);"));
    assert!(updated.contains("logger.info(second);"));
    assert!(updated.contains("logger.info(third);"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_replace_replaces_all_matches_across_multiple_files() {
    let project = setup_project(&[
        ("src/one.ts", "console.log(alpha);\n"),
        (
            "src/two.ts",
            "console.log(beta);\nconsole.log(gamma);\nconst untouched = 1;\n",
        ),
        ("src/three.ts", "const nothing_to_replace = true;\n"),
    ]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let replace = send(
        &mut aft,
        json!({
            "id": "replace-multi-file",
            "command": "ast_replace",
            "pattern": "console.log($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "typescript",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 3);
    assert_eq!(replace["total_files"], 2);
    assert_eq!(replace["files_with_matches"], 2);
    assert_eq!(replace["files_searched"], 3);

    assert_eq!(file_result(&replace, "src/one.ts")["replacements"], 1);
    assert_eq!(file_result(&replace, "src/two.ts")["replacements"], 2);

    let one = read_file(project.path(), "src/one.ts");
    let two = read_file(project.path(), "src/two.ts");
    let three = read_file(project.path(), "src/three.ts");

    assert_eq!(count_occurrences(&one, "logger.info("), 1);
    assert_eq!(count_occurrences(&two, "logger.info("), 2);
    assert_eq!(count_occurrences(&one, "console.log("), 0);
    assert_eq!(count_occurrences(&two, "console.log("), 0);
    assert_eq!(three, "const nothing_to_replace = true;\n");

    let actual_replacements =
        count_occurrences(&one, "logger.info(") + count_occurrences(&two, "logger.info(");
    assert_eq!(
        actual_replacements,
        replace["total_replacements"].as_u64().unwrap() as usize
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_replace_dry_run_reports_counts_without_writing_files() {
    let original = "console.log(first);\nconsole.log(second);\n";
    let project = setup_project(&[("sample.ts", original)]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let replace = send(
        &mut aft,
        json!({
            "id": "replace-dry-run",
            "command": "ast_replace",
            "pattern": "console.log($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "typescript",
            "dry_run": true,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "dry-run replace should succeed: {replace:?}"
    );
    assert_eq!(replace["dry_run"], true);
    assert_eq!(replace["total_replacements"], 2);
    assert_eq!(replace["total_files"], 1);

    let file_entry = &replace["files"].as_array().expect("files array")[0];
    assert_eq!(file_entry["replacements"], 2);
    let diff = file_entry["diff"].as_str().expect("diff string");
    assert!(diff.contains("-console.log(first);"));
    assert!(diff.contains("-console.log(second);"));
    assert!(diff.contains("+logger.info(first);"));
    assert!(diff.contains("+logger.info(second);"));

    let on_disk = read_file(project.path(), "sample.ts");
    assert_eq!(on_disk, original, "dry-run must not modify files on disk");
    assert_eq!(count_occurrences(&on_disk, "console.log("), 2);
    assert_eq!(count_occurrences(&on_disk, "logger.info("), 0);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_support_meta_variables_and_preserve_captures() {
    let project = setup_project(&[(
        "transform.ts",
        "function greet(name, punctuation) {\n  const message = name + punctuation;\n  return message;\n}\n",
    )]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "meta-search",
            "command": "ast_search",
            "pattern": "function $NAME($$$PARAMS) { $$$BODY }",
            "lang": "typescript",
        }),
    );

    assert_eq!(
        search["success"], true,
        "meta ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 1);

    let first_match = &search["matches"].as_array().expect("matches array")[0];
    assert_eq!(first_match["meta_variables"]["$NAME"], "greet");

    let params = first_match["meta_variables"]["$PARAMS"]
        .as_array()
        .expect("params array");
    assert!(params.contains(&Value::String("name".to_string())));
    assert!(params.contains(&Value::String("punctuation".to_string())));

    let body = first_match["meta_variables"]["$BODY"]
        .as_array()
        .expect("body array");
    assert_eq!(body.len(), 2);

    let replace = send(
        &mut aft,
        json!({
            "id": "meta-replace",
            "command": "ast_replace",
            "pattern": "function $NAME($$$PARAMS) { $$$BODY }",
            "rewrite": "const $NAME = ($$$PARAMS) => { $$$BODY }",
            "lang": "typescript",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "meta ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 1);

    let updated = read_file(project.path(), "transform.ts");
    assert!(updated.contains("const greet = (name, punctuation) =>"));
    assert!(updated.contains("const message = name + punctuation;"));
    assert!(updated.contains("return message;"));
    assert!(!updated.contains("function greet"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_replace_rejects_invalid_partial_patterns_without_crashing() {
    let original = "try { doWork(); } catch (err) { console.error(err); } finally { cleanup(); }\n";
    let project = setup_project(&[("sample.ts", original)]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    for (id, pattern) in [
        ("invalid-catch", "catch ($ERR) { $$$ }"),
        ("invalid-finally", "finally { $$$ }"),
    ] {
        let resp = send(
            &mut aft,
            json!({
                "id": id,
                "command": "ast_replace",
                "pattern": pattern,
                "rewrite": "noop()",
                "lang": "typescript",
                "dry_run": true,
            }),
        );

        assert_eq!(
            resp["success"], false,
            "invalid pattern should fail: {resp:?}"
        );
        assert_eq!(resp["code"], "invalid_pattern");
        assert!(resp["message"]
            .as_str()
            .expect("error message")
            .contains("Patterns must be complete AST nodes."));
    }

    let alive = aft.send(r#"{"id":"alive","command":"ping"}"#);
    assert_eq!(
        alive["success"], true,
        "process should stay alive after invalid patterns"
    );
    assert_eq!(read_file(project.path(), "sample.ts"), original);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_rejects_invalid_partial_patterns_without_returning_empty_matches() {
    let original = "try { doWork(); } catch (err) { console.error(err); }\n";
    let project = setup_project(&[("sample.ts", original)]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let resp = send(
        &mut aft,
        json!({
            "id": "invalid-ast-search",
            "command": "ast_search",
            "pattern": "catch ($ERR) { $$$ }",
            "lang": "typescript",
        }),
    );

    assert_eq!(
        resp["success"], false,
        "invalid ast_search pattern should fail: {resp:?}"
    );
    assert_eq!(resp["code"], "invalid_pattern");
    assert!(resp["message"]
        .as_str()
        .expect("message")
        .contains("invalid AST pattern"));

    let alive = aft.send(r#"{"id":"alive-after-ast-search","command":"ping"}"#);
    assert_eq!(alive["success"], true);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_report_empty_results_for_valid_patterns() {
    let original = "const value = compute();\n";
    let project = setup_project(&[("sample.ts", original)]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "empty-search",
            "command": "ast_search",
            "pattern": "console.log($ARG)",
            "lang": "typescript",
        }),
    );

    assert_eq!(
        search["success"], true,
        "empty ast_search should succeed: {search:?}"
    );
    assert_eq!(
        search["matches"].as_array().expect("matches array").len(),
        0
    );
    assert_eq!(search["total_matches"], 0);
    assert_eq!(search["files_with_matches"], 0);
    assert_eq!(search["files_searched"], 1);
    assert_eq!(search["no_files_matched_scope"], false);
    assert_eq!(
        search["scope_warnings"]
            .as_array()
            .expect("scope warnings")
            .len(),
        0
    );

    let replace = send(
        &mut aft,
        json!({
            "id": "empty-replace",
            "command": "ast_replace",
            "pattern": "console.log($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "typescript",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "empty ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 0);
    assert_eq!(replace["total_files"], 0);
    assert_eq!(replace["files_with_matches"], 0);
    assert_eq!(replace["files_searched"], 1);
    assert_eq!(replace["no_files_matched_scope"], false);
    assert_eq!(
        replace["scope_warnings"]
            .as_array()
            .expect("scope warnings")
            .len(),
        0
    );
    assert_eq!(replace["files"].as_array().expect("files array").len(), 0);
    assert_eq!(read_file(project.path(), "sample.ts"), original);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_reject_nonexistent_paths() {
    let project = setup_project(&[("sample.ts", "console.log(value);\n")]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let missing_absolute = project.path().join("missing.ts");
    let search = send(
        &mut aft,
        json!({
            "id": "missing-absolute-search",
            "command": "ast_search",
            "pattern": "console.log($ARG)",
            "lang": "typescript",
            "paths": [missing_absolute.display().to_string()],
        }),
    );

    assert_eq!(
        search["success"], false,
        "missing search path should fail: {search:?}"
    );
    assert_eq!(search["code"], "path_not_found");
    assert!(search["message"]
        .as_str()
        .expect("search error message")
        .contains(&missing_absolute.display().to_string()));

    let replace = send(
        &mut aft,
        json!({
            "id": "missing-relative-replace",
            "command": "ast_replace",
            "pattern": "console.log($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "typescript",
            "paths": ["does-not-exist"],
            "dry_run": true,
        }),
    );

    assert_eq!(
        replace["success"], false,
        "missing replace path should fail: {replace:?}"
    );
    assert_eq!(replace["code"], "path_not_found");
    assert!(replace["message"]
        .as_str()
        .expect("replace error message")
        .contains("does-not-exist"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_report_globs_matching_no_files() {
    let original = "console.log(value);\n";
    let project = setup_project(&[("src/sample.ts", original)]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "empty-glob-search",
            "command": "ast_search",
            "pattern": "console.log($ARG)",
            "lang": "typescript",
            "paths": ["src"],
            "globs": ["*.go"],
        }),
    );

    assert_eq!(
        search["success"], true,
        "empty glob search should succeed: {search:?}"
    );
    assert_eq!(
        search["matches"].as_array().expect("matches array").len(),
        0
    );
    assert_eq!(search["total_matches"], 0);
    assert_eq!(search["files_with_matches"], 0);
    assert_eq!(search["files_searched"], 0);
    assert_eq!(search["no_files_matched_scope"], true);
    assert_eq!(
        search["scope_warnings"].as_array().expect("scope warnings"),
        &vec![Value::String("*.go → no files".to_string())]
    );

    let replace = send(
        &mut aft,
        json!({
            "id": "empty-glob-replace",
            "command": "ast_replace",
            "pattern": "console.log($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "typescript",
            "paths": ["src"],
            "globs": ["*.go"],
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "empty glob replace should succeed: {replace:?}"
    );
    assert_eq!(replace["files"].as_array().expect("files array").len(), 0);
    assert_eq!(replace["total_replacements"], 0);
    assert_eq!(replace["total_files"], 0);
    assert_eq!(replace["files_with_matches"], 0);
    assert_eq!(replace["files_searched"], 0);
    assert_eq!(replace["no_files_matched_scope"], true);
    assert_eq!(
        replace["scope_warnings"]
            .as_array()
            .expect("scope warnings"),
        &vec![Value::String("*.go → no files".to_string())]
    );
    assert_eq!(read_file(project.path(), "src/sample.ts"), original);

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_support_python_patterns() {
    let project = setup_project(&[("sample.py", "print(alpha)\nprint(beta)\n")]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "python-search",
            "command": "ast_search",
            "pattern": "print($ARG)",
            "lang": "python",
        }),
    );

    assert_eq!(
        search["success"], true,
        "python ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 2);
    let args: Vec<&str> = search["matches"]
        .as_array()
        .expect("matches array")
        .iter()
        .map(|m| m["meta_variables"]["$ARG"].as_str().expect("python arg"))
        .collect();
    assert_eq!(args, vec!["alpha", "beta"]);

    let replace = send(
        &mut aft,
        json!({
            "id": "python-replace",
            "command": "ast_replace",
            "pattern": "print($ARG)",
            "rewrite": "logger.info($ARG)",
            "lang": "python",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "python ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 2);
    assert_eq!(replace["files_with_matches"], 1);

    let updated = read_file(project.path(), "sample.py");
    assert_eq!(count_occurrences(&updated, "logger.info("), 2);
    assert_eq!(count_occurrences(&updated, "print("), 0);
    assert!(updated.contains("logger.info(alpha)"));
    assert!(updated.contains("logger.info(beta)"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_support_c_patterns() {
    let project = setup_project(&[(
        "sample.c",
        "int add(int left, int right) {\n    return left + right;\n}\n",
    )]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "c-search",
            "command": "ast_search",
            "pattern": "int $NAME($$$PARAMS) { $$$BODY }",
            "lang": "c",
        }),
    );

    assert_eq!(
        search["success"], true,
        "c ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 1);
    assert_eq!(search["matches"][0]["meta_variables"]["$NAME"], "add");

    let replace = send(
        &mut aft,
        json!({
            "id": "c-replace",
            "command": "ast_replace",
            "pattern": "int $NAME($$$PARAMS) { $$$BODY }",
            "rewrite": "long $NAME($$$PARAMS) { $$$BODY }",
            "lang": "c",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "c ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 1);

    let updated = read_file(project.path(), "sample.c");
    assert!(updated.contains("long add(int left, int right)"));
    assert!(!updated.contains("int add(int left, int right)"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_support_cpp_patterns() {
    let project = setup_project(&[("sample.cpp", "int measure() {\n    return 42;\n}\n")]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "cpp-search",
            "command": "ast_search",
            "pattern": "int $NAME() { return 42; }",
            "lang": "cpp",
        }),
    );

    assert_eq!(
        search["success"], true,
        "cpp ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 1);
    assert_eq!(search["matches"][0]["meta_variables"]["$NAME"], "measure");

    let replace = send(
        &mut aft,
        json!({
            "id": "cpp-replace",
            "command": "ast_replace",
            "pattern": "int $NAME() { return 42; }",
            "rewrite": "long $NAME() { return 42; }",
            "lang": "cpp",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "cpp ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 1);

    let updated = read_file(project.path(), "sample.cpp");
    assert!(updated.contains("long measure()"));
    assert!(!updated.contains("int measure()"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_support_zig_patterns() {
    let project = setup_project(&[(
        "sample.zig",
        "const answer = 41;\n\nfn greet(name: []const u8) void {\n    _ = name;\n}\n",
    )]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "zig-search",
            "command": "ast_search",
            "pattern": "fn greet(name: []const u8) void { $$$ }",
            "lang": "zig",
        }),
    );

    assert_eq!(
        search["success"], true,
        "zig ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 1);
    assert!(search["matches"][0]["text"]
        .as_str()
        .expect("zig match text")
        .contains("fn greet(name: []const u8) void"));

    let replace = send(
        &mut aft,
        json!({
            "id": "zig-replace",
            "command": "ast_replace",
            "pattern": "fn greet(name: []const u8) void { _ = name; }",
            "rewrite": "pub fn greet(name: []const u8) void { _ = name; }",
            "lang": "zig",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "zig ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 1);

    let updated = read_file(project.path(), "sample.zig");
    assert!(updated.contains("pub fn greet(name: []const u8) void"));
    assert!(!updated.contains("\nfn greet(name: []const u8) void"));

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn ast_search_and_replace_support_csharp_patterns() {
    let project = setup_project(&[(
        "Sample.cs",
        "public class Worker\n{\n    private int count = 1;\n\n    public void Run()\n    {\n    }\n}\n",
    )]);
    let mut aft = AftProcess::spawn();
    configure(&mut aft, project.path());

    let search = send(
        &mut aft,
        json!({
            "id": "csharp-search",
            "command": "ast_search",
            "pattern": "public class $NAME { $$$BODY }",
            "lang": "csharp",
        }),
    );

    assert_eq!(
        search["success"], true,
        "csharp ast_search should succeed: {search:?}"
    );
    assert_eq!(search["total_matches"], 1);
    assert_eq!(search["matches"][0]["meta_variables"]["$NAME"], "Worker");

    let replace = send(
        &mut aft,
        json!({
            "id": "csharp-replace",
            "command": "ast_replace",
            "pattern": "public class $NAME { $$$BODY }",
            "rewrite": "public sealed class $NAME { $$$BODY }",
            "lang": "csharp",
            "dry_run": false,
        }),
    );

    assert_eq!(
        replace["success"], true,
        "csharp ast_replace should succeed: {replace:?}"
    );
    assert_eq!(replace["total_replacements"], 1);

    let updated = read_file(project.path(), "Sample.cs");
    assert!(updated.contains("public sealed class Worker"));
    assert!(!updated.contains("public class Worker"));

    let status = aft.shutdown();
    assert!(status.success());
}
