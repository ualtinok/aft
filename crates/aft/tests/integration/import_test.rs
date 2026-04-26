//! Integration tests for the add_import command through the binary protocol.

use super::helpers::{fixture_path, AftProcess};
use std::fs;

/// Helper: copy a fixture to a uniquely-named temp file for mutation testing.
fn temp_copy(fixture_name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let src = fixture_path(fixture_name);
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();

    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let (stem, ext) = fixture_name.rsplit_once('.').unwrap_or((fixture_name, ""));
    let unique = if ext.is_empty() {
        format!("{}_{}", stem, n)
    } else {
        format!("{}_{}.{}", stem, n, ext)
    };
    let dest = dir.join(unique);
    fs::copy(&src, &dest).unwrap();
    dest
}

/// Helper: send an add_import request and return the response.
fn send_add_import(
    aft: &mut AftProcess,
    id: &str,
    file: &str,
    module: &str,
    names: Option<&[&str]>,
    default_import: Option<&str>,
    type_only: bool,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "id": id,
        "command": "add_import",
        "file": file,
        "module": module,
    });

    if let Some(names) = names {
        params["names"] = serde_json::json!(names);
    }
    if let Some(def) = default_import {
        params["default_import"] = serde_json::json!(def);
    }
    if type_only {
        params["type_only"] = serde_json::json!(true);
    }

    aft.send(&serde_json::to_string(&params).unwrap())
}

// --- TS tests ---

#[test]
fn add_import_ts_external_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "imp-1",
        &file_str,
        "lodash",
        Some(&["debounce"]),
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "external");

    // Verify the import was added to the file
    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("import { debounce } from 'lodash';"),
        "should contain the new import. content:\n{}",
        content
    );

    // Verify it's in the external group (before relative imports)
    let lodash_pos = content.find("import { debounce } from 'lodash'").unwrap();
    let relative_pos = content.find("import { helper } from './utils'").unwrap();
    assert!(
        lodash_pos < relative_pos,
        "lodash import should be before relative imports"
    );

    // Syntax should be valid
    assert_eq!(
        resp["syntax_valid"], true,
        "syntax should be valid after add, resp: {:?}",
        resp
    );

    // Cleanup
    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_ts_relative_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "imp-2",
        &file_str,
        "./components",
        Some(&["Button"]),
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "internal");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("import { Button } from './components';"),
        "should contain the new relative import. content:\n{}",
        content
    );

    // Verify it's in the relative group (after external imports)
    let button_pos = content
        .find("import { Button } from './components'")
        .unwrap();
    let react_pos = content.find("import React from 'react'").unwrap();
    assert!(
        button_pos > react_pos,
        "relative import should be after external imports"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_ts_dedup() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    // Try to add useState which already exists in the fixture
    let resp = send_add_import(
        &mut aft,
        "imp-3",
        &file_str,
        "react",
        Some(&["useState"]),
        None,
        false,
    );

    assert_eq!(resp["success"], true);
    assert_eq!(resp["added"], false, "should not add duplicate");
    assert_eq!(resp["already_present"], true);

    // File should not have been modified
    let original = fs::read_to_string(fixture_path("imports_ts.ts")).unwrap();
    let current = fs::read_to_string(&file).unwrap();
    assert_eq!(
        original, current,
        "file should not have been modified for duplicate"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_ts_alphabetizes() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    // Add 'axios' which should sort before 'react' and after nothing (first external)
    let resp = send_add_import(
        &mut aft,
        "imp-4",
        &file_str,
        "axios",
        None,
        Some("axios"),
        false,
    );

    assert_eq!(resp["success"], true);
    assert_eq!(resp["added"], true);

    let content = fs::read_to_string(&file).unwrap();
    let axios_pos = content.find("import axios from 'axios'").unwrap();
    let react_pos = content.find("import React from 'react'").unwrap();
    assert!(
        axios_pos < react_pos,
        "axios should sort before react alphabetically. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

// --- JS tests ---

#[test]
fn add_import_js_works() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_js.js");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "imp-5",
        &file_str,
        "cors",
        None,
        Some("cors"),
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import on JS should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "external");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("import cors from 'cors';"),
        "should contain the new JS import. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

// --- Edge cases ---

#[test]
fn add_import_empty_file() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static EMPTY_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = EMPTY_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file = dir.join(format!("empty_{}.ts", n));
    fs::write(&file, "").unwrap();
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "imp-6",
        &file_str,
        "react",
        Some(&["useState"]),
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import on empty file should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("import { useState } from 'react';"),
        "should contain the import at top. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_missing_file_returns_error() {
    let mut aft = AftProcess::spawn();

    let resp = send_add_import(
        &mut aft,
        "imp-7",
        "/tmp/nonexistent_aft_test.ts",
        "react",
        Some(&["useState"]),
        None,
        false,
    );

    assert_eq!(resp["success"], false, "should fail for missing file");
    assert_eq!(resp["code"], "file_not_found");

    aft.shutdown();
}

#[test]
fn add_import_unsupported_language_returns_error() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static UNSUP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = UNSUP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file = dir.join(format!("test_{}.txt", n));
    fs::write(&file, "hello world").unwrap();
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "imp-8",
        &file_str,
        "react",
        Some(&["useState"]),
        None,
        false,
    );

    assert_eq!(
        resp["success"], false,
        "should fail for unsupported language"
    );
    assert_eq!(resp["code"], "invalid_request");

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_missing_params_returns_error() {
    let mut aft = AftProcess::spawn();

    // Missing 'module' param
    let resp = aft.send(r#"{"id":"imp-9","command":"add_import","file":"/tmp/test.ts"}"#);

    assert_eq!(resp["success"], false);
    assert_eq!(resp["code"], "invalid_request");
    assert!(
        resp["message"].as_str().unwrap().contains("module"),
        "error should mention missing 'module' param"
    );

    aft.shutdown();
}

// --- Python tests ---

#[test]
fn add_import_py_stdlib_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_py.py");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "py-1",
        &file_str,
        "pathlib",
        Some(&["Path"]),
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import py stdlib should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "stdlib");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("from pathlib import Path"),
        "should contain the new stdlib import. content:\n{}",
        content
    );

    // Verify it's in the stdlib group (before third-party imports)
    let pathlib_pos = content.find("from pathlib import Path").unwrap();
    let requests_pos = content.find("import requests").unwrap();
    assert!(
        pathlib_pos < requests_pos,
        "stdlib import should be before third-party imports"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_py_third_party_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_py.py");
    let file_str = file.display().to_string();

    let resp = send_add_import(&mut aft, "py-2", &file_str, "click", None, None, false);

    assert_eq!(
        resp["success"], true,
        "add_import py third-party should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "external");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("import click"),
        "should contain the new third-party import. content:\n{}",
        content
    );

    // Verify it's in the external group (after stdlib, before local)
    let click_pos = content.find("import click").unwrap();
    let os_pos = content.find("import os").unwrap();
    let utils_pos = content.find("from . import utils").unwrap();
    assert!(
        click_pos > os_pos,
        "third-party import should be after stdlib"
    );
    assert!(
        click_pos < utils_pos,
        "third-party import should be before local"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_py_local_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_py.py");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "py-3",
        &file_str,
        ".models",
        Some(&["User"]),
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import py local should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "internal");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("from .models import User"),
        "should contain the new local import. content:\n{}",
        content
    );

    // Verify it's in the internal group (after third-party)
    let models_pos = content.find("from .models import User").unwrap();
    let requests_pos = content.find("import requests").unwrap();
    assert!(
        models_pos > requests_pos,
        "local import should be after third-party"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_py_dedup() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_py.py");
    let file_str = file.display().to_string();

    // Try to add 'os' which already exists
    let resp = send_add_import(&mut aft, "py-4", &file_str, "os", None, None, false);

    assert_eq!(resp["success"], true);
    assert_eq!(resp["added"], false, "should not add duplicate");
    assert_eq!(resp["already_present"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}

// --- Rust tests ---

#[test]
fn add_import_rs_std_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_rs.rs");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "rs-1",
        &file_str,
        "std::fmt::Display",
        None,
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import rs std should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "stdlib");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("use std::fmt::Display;"),
        "should contain the new std import. content:\n{}",
        content
    );

    // Verify it's in the stdlib group (before external imports)
    let fmt_pos = content.find("use std::fmt::Display;").unwrap();
    let serde_pos = content.find("use serde").unwrap();
    assert!(
        fmt_pos < serde_pos,
        "std import should be before external imports"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_rs_external_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_rs.rs");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "rs-2",
        &file_str,
        "anyhow::Result",
        None,
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import rs external should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "external");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("use anyhow::Result;"),
        "should contain the new external import. content:\n{}",
        content
    );

    // Should be in external group (after std, before crate)
    let anyhow_pos = content.find("use anyhow::Result;").unwrap();
    let std_pos = content.find("use std::").unwrap();
    let crate_pos = content.find("use crate::").unwrap();
    assert!(anyhow_pos > std_pos, "external import should be after std");
    assert!(
        anyhow_pos < crate_pos,
        "external import should be before crate"
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_rs_dedup() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_rs.rs");
    let file_str = file.display().to_string();

    // Try to add std::collections::HashMap which already exists
    let resp = send_add_import(
        &mut aft,
        "rs-3",
        &file_str,
        "std::collections::HashMap",
        None,
        None,
        false,
    );

    assert_eq!(resp["success"], true);
    assert_eq!(resp["added"], false, "should not add duplicate");
    assert_eq!(resp["already_present"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}

// --- Go tests ---

#[test]
fn add_import_go_stdlib_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_go.go");
    let file_str = file.display().to_string();

    let resp = send_add_import(&mut aft, "go-1", &file_str, "net/http", None, None, false);

    assert_eq!(
        resp["success"], true,
        "add_import go stdlib should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "stdlib");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("\"net/http\""),
        "should contain the new stdlib import. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_go_external_group() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_go.go");
    let file_str = file.display().to_string();

    let resp = send_add_import(
        &mut aft,
        "go-2",
        &file_str,
        "golang.org/x/tools",
        None,
        None,
        false,
    );

    assert_eq!(
        resp["success"], true,
        "add_import go external should succeed: {:?}",
        resp
    );
    assert_eq!(resp["added"], true);
    assert_eq!(resp["group"], "external");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("\"golang.org/x/tools\""),
        "should contain the new external import. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn add_import_go_dedup() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_go.go");
    let file_str = file.display().to_string();

    // Try to add "fmt" which already exists
    let resp = send_add_import(&mut aft, "go-3", &file_str, "fmt", None, None, false);

    assert_eq!(resp["success"], true);
    assert_eq!(resp["added"], false, "should not add duplicate");
    assert_eq!(resp["already_present"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}

// ===========================================================================
// remove_import tests
// ===========================================================================

/// Helper: send a remove_import request and return the response.
fn send_remove_import(
    aft: &mut AftProcess,
    id: &str,
    file: &str,
    module: &str,
    name: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "id": id,
        "command": "remove_import",
        "file": file,
        "module": module,
    });

    if let Some(n) = name {
        params["name"] = serde_json::json!(n);
    }

    aft.send(&serde_json::to_string(&params).unwrap())
}

/// Helper: send an organize_imports request and return the response.
fn send_organize_imports(aft: &mut AftProcess, id: &str, file: &str) -> serde_json::Value {
    let params = serde_json::json!({
        "id": id,
        "command": "organize_imports",
        "file": file,
    });

    aft.send(&serde_json::to_string(&params).unwrap())
}

#[test]
fn remove_import_entire_statement_ts() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    // Remove the 'zod' import entirely
    let resp = send_remove_import(&mut aft, "rm-1", &file_str, "zod", None);

    assert_eq!(
        resp["success"], true,
        "remove_import should succeed: {:?}",
        resp
    );
    assert_eq!(resp["removed"], true);
    assert_eq!(resp["module"], "zod");

    let content = fs::read_to_string(&file).unwrap();
    assert!(
        !content.contains("from 'zod'"),
        "zod import should be removed. content:\n{}",
        content
    );
    // Other imports should remain
    assert!(
        content.contains("from 'react'"),
        "react imports should remain"
    );

    assert_eq!(resp["syntax_valid"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn remove_import_specific_name_from_multi_ts() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    // Remove 'useState' from `import { useState, useEffect } from 'react';`
    let resp = send_remove_import(&mut aft, "rm-2", &file_str, "react", Some("useState"));

    assert_eq!(
        resp["success"], true,
        "remove_import should succeed: {:?}",
        resp
    );
    assert_eq!(resp["removed"], true);
    assert_eq!(resp["name"], "useState");

    let content = fs::read_to_string(&file).unwrap();
    // Should still have useEffect from react
    assert!(
        content.contains("useEffect") && content.contains("react"),
        "useEffect import from react should remain. content:\n{}",
        content
    );
    // useState should not appear in that specific import anymore
    assert!(
        !content.contains("import { useState, useEffect }"),
        "the original multi-name import should be modified. content:\n{}",
        content
    );

    assert_eq!(resp["syntax_valid"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn remove_import_missing_module_reports_not_removed() {
    let mut aft = AftProcess::spawn();
    let file = temp_copy("imports_ts.ts");
    let file_str = file.display().to_string();

    let resp = send_remove_import(&mut aft, "rm-3", &file_str, "nonexistent-module", None);

    assert_eq!(resp["success"], true, "request should complete: {resp:?}");
    assert_eq!(resp["removed"], false, "nothing should be removed");
    assert_eq!(resp["reason"], "module_not_found");

    fs::remove_file(&file).ok();
    aft.shutdown();
}

// ===========================================================================
// organize_imports tests
// ===========================================================================

#[test]
fn organize_imports_ts_regroups_and_sorts() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ORG_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = ORG_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file = dir.join(format!("organize_ts_{}.ts", n));

    // Write a scrambled import file
    fs::write(
        &file,
        "\
import { helper } from './utils';
import { z } from 'zod';
import React from 'react';
import { Config } from '../config';
import { useState } from 'react';

export function App() {}
",
    )
    .unwrap();

    let file_str = file.display().to_string();
    let resp = send_organize_imports(&mut aft, "org-1", &file_str);

    assert_eq!(
        resp["success"], true,
        "organize_imports should succeed: {:?}",
        resp
    );

    let content = fs::read_to_string(&file).unwrap();

    // External imports should come before internal
    let react_pos = content.find("react").unwrap();
    let utils_pos = content.find("./utils").unwrap();
    assert!(
        react_pos < utils_pos,
        "external imports should come before internal. content:\n{}",
        content
    );

    // Within external group, should be alphabetical: react before zod
    let zod_pos = content.find("zod").unwrap();
    assert!(
        react_pos < zod_pos,
        "react should come before zod (alphabetical). content:\n{}",
        content
    );

    assert_eq!(resp["syntax_valid"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn organize_imports_ts_deduplicates() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static DEDUP_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = DEDUP_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file = dir.join(format!("organize_dedup_{}.ts", n));

    // Write a file with duplicate imports
    fs::write(
        &file,
        "\
import { z } from 'zod';
import { z } from 'zod';
import React from 'react';
import React from 'react';

export function App() {}
",
    )
    .unwrap();

    let file_str = file.display().to_string();
    let resp = send_organize_imports(&mut aft, "org-2", &file_str);

    assert_eq!(
        resp["success"], true,
        "organize_imports should succeed: {:?}",
        resp
    );
    assert!(
        resp["removed_duplicates"].as_u64().unwrap() >= 2,
        "should remove at least 2 duplicates: {:?}",
        resp
    );

    let content = fs::read_to_string(&file).unwrap();
    // Count occurrences of 'zod' — should appear exactly once
    let zod_count = content.matches("'zod'").count();
    assert_eq!(
        zod_count, 1,
        "should have exactly one zod import. content:\n{}",
        content
    );

    let react_count = content.matches("from 'react'").count();
    assert_eq!(
        react_count, 1,
        "should have exactly one react import. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn organize_imports_py_isort_grouping() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static PY_ORG_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = PY_ORG_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file = dir.join(format!("organize_py_{}.py", n));

    // Write a scrambled Python import file (wrong order: local, external, stdlib)
    fs::write(
        &file,
        "\
from . import utils
import requests
import os
import sys
from ..config import Settings

def main():
    pass
",
    )
    .unwrap();

    let file_str = file.display().to_string();
    let resp = send_organize_imports(&mut aft, "org-3", &file_str);

    assert_eq!(
        resp["success"], true,
        "organize_imports should succeed: {:?}",
        resp
    );

    // Check groups: should be stdlib, external, internal
    let groups = resp["groups"].as_array().unwrap();
    assert!(
        groups.len() >= 2,
        "should have at least 2 groups: {:?}",
        groups
    );
    assert_eq!(groups[0]["name"], "stdlib", "first group should be stdlib");

    let content = fs::read_to_string(&file).unwrap();

    // Stdlib (os, sys) should come before external (requests)
    let os_pos = content.find("import os").unwrap();
    let requests_pos = content.find("import requests").unwrap();
    assert!(
        os_pos < requests_pos,
        "stdlib should come before external. content:\n{}",
        content
    );

    // External should come before internal
    let utils_pos = content.find("utils").unwrap();
    assert!(
        requests_pos < utils_pos,
        "external should come before internal. content:\n{}",
        content
    );

    fs::remove_file(&file).ok();
    aft.shutdown();
}

#[test]
fn organize_imports_rs_merges_common_prefix() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static RS_ORG_COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut aft = AftProcess::spawn();
    let dir = std::env::temp_dir().join("aft_import_tests");
    fs::create_dir_all(&dir).unwrap();
    let n = RS_ORG_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file = dir.join(format!("organize_rs_{}.rs", n));

    // Write Rust file with separate use declarations that share a common prefix
    fs::write(
        &file,
        "\
use std::path::PathBuf;
use std::path::Path;
use std::collections::HashMap;
use serde::Deserialize;
use serde::Serialize;
use crate::config::Settings;

fn main() {}
",
    )
    .unwrap();

    let file_str = file.display().to_string();
    let resp = send_organize_imports(&mut aft, "org-4", &file_str);

    assert_eq!(
        resp["success"], true,
        "organize_imports should succeed: {:?}",
        resp
    );

    let content = fs::read_to_string(&file).unwrap();

    // std::path::Path and std::path::PathBuf should be merged
    assert!(
        content.contains("use std::path::{Path, PathBuf};"),
        "should merge std::path imports into a use tree. content:\n{}",
        content
    );

    // serde::Deserialize and serde::Serialize should be merged
    assert!(
        content.contains("use serde::{Deserialize, Serialize};"),
        "should merge serde imports into a use tree. content:\n{}",
        content
    );

    // Groups should be in order: stdlib, external, internal
    let std_pos = content.find("use std::").unwrap();
    let serde_pos = content.find("use serde::").unwrap();
    let crate_pos = content.find("use crate::").unwrap();
    assert!(std_pos < serde_pos, "stdlib before external");
    assert!(serde_pos < crate_pos, "external before internal");

    assert_eq!(resp["syntax_valid"], true);

    fs::remove_file(&file).ok();
    aft.shutdown();
}
