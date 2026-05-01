use super::helpers::AftProcess;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_file(root: &Path, relative: &str, content: &str) -> PathBuf {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
    path
}

fn send(aft: &mut AftProcess, request: serde_json::Value) -> serde_json::Value {
    aft.send(&request.to_string())
}

fn outline_text(aft: &mut AftProcess, file: &Path) -> String {
    let resp = send(
        aft,
        json!({
            "id": format!("outline-{}", file.display()),
            "command": "outline",
            "file": file,
        }),
    );

    assert_eq!(resp["success"], true, "outline should succeed: {resp:?}");
    resp["text"].as_str().expect("outline text").to_string()
}

#[test]
fn outline_c_header_symbols_include_macros_types_and_prototypes() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "include/sample.h",
        r#"#define MAX_SIZE 128

typedef unsigned long Count;

struct Config {
    int size;
};

enum Mode {
    MODE_A,
    MODE_B,
};

int compute(int value);
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let text = outline_text(&mut aft, &file);
    for expected in ["sample.h", "MAX_SIZE", "Count", "Config", "Mode", "compute"] {
        assert!(
            text.contains(expected),
            "missing {expected} in outline: {text}"
        );
    }

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn outline_cpp_symbols_include_namespaces_templates_types_and_methods() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "include/sample.hpp",
        r#"namespace math {
class Worker {
public:
    void run();
};

struct Options {
    int count;
};

enum State {
    Ready,
    Busy,
};

template <typename T>
T identity(T value) {
    return value;
}

int add(int left, int right);
}

inline void math::Worker::run() {}
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let text = outline_text(&mut aft, &file);
    for expected in [
        "sample.hpp",
        "math",
        "Worker",
        "run",
        "Options",
        "State",
        "identity",
        "add",
    ] {
        assert!(
            text.contains(expected),
            "missing {expected} in outline: {text}"
        );
    }

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn outline_html_symbols_include_heading_hierarchy() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "index.html",
        r#"<!DOCTYPE html>
<html>
<head><title>Test Page</title></head>
<body>
  <h1>Main Title</h1>
  <p>Introduction text</p>
  <h2>First Section</h2>
  <p>Content here</p>
  <h3>Subsection A</h3>
  <p>More content</p>
  <h2>Second Section</h2>
  <article>
    <h3>Nested Article</h3>
    <p>Article content</p>
  </article>
</body>
</html>
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let text = outline_text(&mut aft, &file);
    // Should have all headings
    for expected in [
        "Main Title",
        "First Section",
        "Subsection A",
        "Second Section",
        "Nested Article",
    ] {
        assert!(
            text.contains(expected),
            "missing {expected} in outline: {text}"
        );
    }
    // Should show heading kind abbreviation
    assert!(
        text.contains(" h "),
        "should contain heading kind 'h': {text}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn zoom_html_heading_returns_content_with_context() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "page.html",
        r#"<html>
<body>
  <h1>Welcome</h1>
  <p>Intro paragraph</p>
  <h2>Features</h2>
  <p>Feature list here</p>
  <h2>About</h2>
  <p>About section</p>
</body>
</html>
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let resp = send(
        &mut aft,
        json!({
            "id": "zoom-html",
            "command": "zoom",
            "file": file,
            "symbol": "Features",
        }),
    );

    assert_eq!(resp["success"], true, "zoom should succeed: {resp:?}");
    assert_eq!(resp["name"], "Features");
    assert_eq!(resp["kind"], "heading");
    let content = resp["content"].as_str().unwrap();
    // Section content must include the heading AND the paragraph beneath it.
    assert!(
        content.contains("Features"),
        "content should contain heading text: {content}"
    );
    assert!(
        content.contains("Feature list here"),
        "content should include section body, not just the heading line: {content}"
    );
    // The About section belongs to a different heading — must not bleed in.
    assert!(
        !content.contains("About section"),
        "content should stop before next sibling heading: {content}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

/// Regression: aft_zoom on an HTML heading must return the full section extent
/// (heading through the line before the next same-or-shallower heading), not
/// just the single heading element line.
#[test]
fn zoom_html_heading_returns_section_extent_not_just_heading_line() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "docs.html",
        r#"<html><body>
<h2>Installation</h2>
<p>Run npm install.</p>
<pre>npm install pkg</pre>
<h2>Configuration</h2>
<p>Set env vars.</p>
<h3>Advanced</h3>
<p>Advanced details here.</p>
<h2>Usage</h2>
<p>Call the API.</p>
</body></html>
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let resp = send(
        &mut aft,
        json!({ "id": "z1", "command": "zoom", "file": file, "symbol": "Configuration" }),
    );
    assert_eq!(resp["success"], true);
    let content = resp["content"].as_str().unwrap();
    // Must include the h2 itself, the <p>, and the nested h3 + its content.
    assert!(content.contains("Configuration"), "missing h2: {content}");
    assert!(
        content.contains("Set env vars"),
        "missing section body: {content}"
    );
    assert!(
        content.contains("Advanced"),
        "nested h3 should be within section: {content}"
    );
    assert!(
        content.contains("Advanced details here"),
        "nested content should be included: {content}"
    );
    // Usage is a sibling h2 — must not bleed in.
    assert!(
        !content.contains("Call the API"),
        "sibling section must not bleed in: {content}"
    );

    // h1 section: should span to EOF when it's the last heading.
    let resp2 = send(
        &mut aft,
        json!({ "id": "z2", "command": "zoom", "file": file, "symbol": "Installation" }),
    );
    assert_eq!(resp2["success"], true);
    let content2 = resp2["content"].as_str().unwrap();
    assert!(
        content2.contains("npm install pkg"),
        "Installation section body missing: {content2}"
    );
    assert!(
        !content2.contains("Set env vars"),
        "Installation must stop before Configuration: {content2}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn outline_zig_symbols_include_containers_consts_tests_and_functions() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "src/sample.zig",
        r#"const PI = 3.14;

const Payload = union {
    int: i32,
    text: []const u8,
};

const Status = enum {
    ready,
    busy,
};

const Config = struct {
    port: u16,

    pub fn init() Config {
        return .{ .port = 80 };
    }
};

fn greet(name: []const u8) void {
    _ = name;
}

test "config init" {}
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let text = outline_text(&mut aft, &file);
    for expected in [
        "sample.zig",
        "PI",
        "Payload",
        "Status",
        "Config",
        "init",
        "greet",
        "config init",
    ] {
        assert!(
            text.contains(expected),
            "missing {expected} in outline: {text}"
        );
    }

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn outline_csharp_symbols_include_namespace_types_members_and_properties() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "src/Sample.cs",
        r#"namespace Demo.Tools;

public interface IWorker
{
    string Name { get; }
}

public class Worker
{
    public string Name { get; }
    public int Count { get; set; }

    public void Run() {}
}

public struct Options
{
    public int Count { get; set; }
}

public enum Mode
{
    Fast,
    Slow,
}
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let text = outline_text(&mut aft, &file);
    for expected in [
        "Sample.cs",
        "Demo.Tools",
        "IWorker",
        "Worker",
        "Name",
        "Count",
        "Run",
        "Options",
        "Mode",
    ] {
        assert!(
            text.contains(expected),
            "missing {expected} in outline: {text}"
        );
    }

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn outline_supports_requested_new_extensions() {
    let dir = TempDir::new().unwrap();
    let files = vec![
        write_file(
            dir.path(),
            "src/sample.c",
            "int c_file(void) { return 1; }\n",
        ),
        write_file(dir.path(), "include/sample.h", "int h_file(void);\n"),
        write_file(dir.path(), "src/sample.cc", "int cc_file() { return 2; }\n"),
        write_file(
            dir.path(),
            "src/sample.cpp",
            "int cpp_file() { return 3; }\n",
        ),
        write_file(
            dir.path(),
            "include/sample.hpp",
            "struct HppType { int value; };\n",
        ),
        write_file(
            dir.path(),
            "src/sample.cs",
            "class CsType { void Run() {} }\n",
        ),
        write_file(dir.path(), "src/sample.zig", "fn zigFile() void {}\n"),
    ];

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);

    let resp = send(
        &mut aft,
        json!({
            "id": "outline-new-exts",
            "command": "outline",
            "files": files,
        }),
    );

    assert_eq!(resp["success"], true, "outline should succeed: {resp:?}");
    let text = resp["text"].as_str().expect("outline text");
    for expected in [
        "sample.c",
        "sample.h",
        "sample.cc",
        "sample.cpp",
        "sample.hpp",
        "sample.cs",
        "sample.zig",
        "c_file",
        "h_file",
        "cc_file",
        "cpp_file",
        "HppType",
        "CsType",
        "zigFile",
    ] {
        assert!(
            text.contains(expected),
            "missing {expected} in outline: {text}"
        );
    }

    let status = aft.shutdown();
    assert!(status.success());
}

#[test]
fn outline_bash_symbols_include_functions() {
    let dir = TempDir::new().unwrap();
    let file = write_file(
        dir.path(),
        "scripts/deploy.sh",
        r#"#!/bin/bash

APP_NAME="my-app"
export LOG_LEVEL="info"

function setup_environment() {
    local dir="$1"
    mkdir -p "$dir"
}

cleanup() {
    rm -rf /tmp/cache
}

main() {
    setup_environment "/tmp/app"
    echo "Starting $APP_NAME"
}

main "$@"
"#,
    );

    let mut aft = AftProcess::spawn();
    assert_eq!(aft.configure(dir.path())["success"], true);
    let text = outline_text(&mut aft, &file);

    // Should find all three function definitions
    assert!(
        text.contains("setup_environment"),
        "missing setup_environment in bash outline: {text}"
    );
    assert!(
        text.contains("cleanup"),
        "missing cleanup in bash outline: {text}"
    );
    assert!(
        text.contains("main"),
        "missing main in bash outline: {text}"
    );

    // Functions should be marked as functions
    assert!(
        text.contains("fn"),
        "bash functions should have 'fn' kind marker: {text}"
    );

    let status = aft.shutdown();
    assert!(status.success());
}
