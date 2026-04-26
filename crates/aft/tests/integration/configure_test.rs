use serde_json::json;

use super::helpers::AftProcess;

fn empty_path() -> std::ffi::OsString {
    std::ffi::OsString::new()
}

fn warning_with_kind<'a>(
    configure: &'a serde_json::Value,
    kind: &str,
    key: &str,
    value: &str,
) -> Option<&'a serde_json::Value> {
    configure["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| {
            warning["kind"] == kind
                && warning.get(key).and_then(|entry| entry.as_str()) == Some(value)
        })
}

#[test]
fn configure_accepts_boolean_validate_on_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-validate-bool",
            "command": "configure",
            "project_root": dir.path(),
            "validate_on_edit": true,
        })
        .to_string(),
    );
    assert_eq!(
        configure["success"], true,
        "configure should accept boolean validate_on_edit: {configure:?}"
    );
    assert!(
        configure["warnings"].as_array().is_some(),
        "configure responses should always include warnings: {configure:?}"
    );

    let status = aft.send(r#"{"id":"status-validate-bool","command":"status"}"#);
    assert_eq!(status["success"], true, "status should succeed: {status:?}");
    assert_eq!(status["features"]["validate_on_edit"], "syntax");

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_warns_for_missing_formatter_and_checker_tools() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.ts"), "const x = 1;\n").unwrap();
    std::fs::write(dir.path().join("biome.json"), "{}\n").unwrap();

    let path = empty_path();
    let mut aft = AftProcess::spawn_with_env(&[("PATH", path.as_os_str())]);

    let configure = aft.send(
        &json!({
            "id": "cfg-missing-format-check",
            "command": "configure",
            "project_root": dir.path()
        })
        .to_string(),
    );

    assert_eq!(
        configure["success"], true,
        "configure should succeed: {configure:?}"
    );
    let formatter = warning_with_kind(&configure, "formatter_not_installed", "tool", "biome")
        .expect("missing formatter warning");
    assert_eq!(formatter["language"], "typescript");
    assert!(formatter["hint"]
        .as_str()
        .unwrap()
        .contains("bun add -d --workspace-root @biomejs/biome"));

    let checker = warning_with_kind(&configure, "checker_not_installed", "tool", "biome")
        .expect("missing checker warning");
    assert_eq!(checker["language"], "typescript");

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_only_warns_for_languages_present() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.ts"), "const x = 1;\n").unwrap();
    std::fs::write(dir.path().join("pyrightconfig.json"), "{}\n").unwrap();

    let path = empty_path();
    let mut aft = AftProcess::spawn_with_env(&[("PATH", path.as_os_str())]);

    let configure = aft.send(
        &json!({
            "id": "cfg-language-present",
            "command": "configure",
            "project_root": dir.path()
        })
        .to_string(),
    );

    assert_eq!(
        configure["success"], true,
        "configure should succeed: {configure:?}"
    );
    assert!(
        warning_with_kind(&configure, "checker_not_installed", "tool", "pyright").is_none(),
        "should not warn about Python checker without Python files: {configure:?}"
    );

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_warns_for_missing_builtin_and_custom_lsp_binaries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("script.sh"), "echo hi\n").unwrap();

    let path = empty_path();
    let mut aft = AftProcess::spawn_with_env(&[("PATH", path.as_os_str())]);

    let configure = aft.send(
        &json!({
            "id": "cfg-missing-lsp",
            "command": "configure",
            "project_root": dir.path(),
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": ["typ"],
                "binary": "tinymist",
                "args": [],
                "root_markers": ["typst.toml"]
            }]
        })
        .to_string(),
    );

    assert_eq!(
        configure["success"], true,
        "configure should succeed: {configure:?}"
    );
    let bash = warning_with_kind(
        &configure,
        "lsp_binary_missing",
        "binary",
        "bash-language-server",
    )
    .expect("missing built-in bash LSP warning");
    assert_eq!(bash["server"], "bash-language-server");
    assert!(bash["hint"]
        .as_str()
        .unwrap()
        .contains("npm install -g bash-language-server"));

    let custom = warning_with_kind(&configure, "lsp_binary_missing", "binary", "tinymist")
        .expect("missing custom LSP warning");
    assert_eq!(custom["server"], "tinymist");

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_accepts_custom_lsp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-lsp-custom",
            "command": "configure",
            "project_root": dir.path(),
            "experimental_lsp_ty": true,
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": ["typ"],
                "binary": "tinymist",
                "args": [],
                "root_markers": [".git", "typst.toml"],
                "env": {
                    "TINYMIST_FONT_PATHS": "/tmp/fonts"
                },
                "initialization_options": {
                    "exportPdf": "never"
                },
                "disabled": false
            }],
            "disabled_lsp": ["Pyright"]
        })
        .to_string(),
    );

    assert_eq!(
        configure["success"], true,
        "configure should accept custom lsp server config: {configure:?}"
    );

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_rejects_lsp_server_env_with_non_string_values() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-lsp-bad-env",
            "command": "configure",
            "project_root": dir.path(),
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": ["typ"],
                "binary": "tinymist",
                "env": {
                    "TINYMIST_FONT_PATHS": 42
                }
            }]
        })
        .to_string(),
    );

    assert_eq!(configure["success"], false);
    assert_eq!(configure["code"], "invalid_request");
    assert!(configure["message"]
        .as_str()
        .unwrap()
        .contains("env.TINYMIST_FONT_PATHS must be a string"));

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}

#[test]
fn configure_rejects_malformed_lsp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let mut aft = AftProcess::spawn();

    let configure = aft.send(
        &json!({
            "id": "cfg-lsp-bad",
            "command": "configure",
            "project_root": dir.path(),
            "lsp_servers": [{
                "id": "tinymist",
                "extensions": [],
                "binary": "tinymist"
            }]
        })
        .to_string(),
    );

    assert_eq!(configure["success"], false);
    assert_eq!(configure["code"], "invalid_request");
    assert!(configure["message"]
        .as_str()
        .unwrap()
        .contains("extensions must not be empty"));

    let shutdown = aft.shutdown();
    assert!(shutdown.success());
}
