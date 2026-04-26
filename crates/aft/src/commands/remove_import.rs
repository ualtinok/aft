//! Handler for the `remove_import` command: remove an import statement (or a name from one).
//!
//! Two modes:
//! - If `name` is omitted: remove the entire import statement for the given module.
//! - If `name` is given and the import has multiple names: regenerate the import without that name.
//! - If `name` is given and the import has only that name (or it's a default/side-effect import):
//!   remove the entire import statement.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::imports;
use crate::parser::{detect_language, LangId};
use crate::protocol::{RawRequest, Response};

/// Handle a `remove_import` request.
///
/// Params:
///   - `file` (string, required) — target file path
///   - `module` (string, required) — the module path to match
///   - `name` (string, optional) — specific named import to remove; if omitted, remove entire import
///
/// Returns: `{ file, removed, module, name?, syntax_valid?, backup_id? }`
pub fn handle_remove_import(req: &RawRequest, ctx: &AppContext) -> Response {
    // --- Extract params ---
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "remove_import: missing required param 'file'",
            );
        }
    };

    let module = match req.params.get("module").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "remove_import: missing required param 'module'",
            );
        }
    };

    let name = req
        .params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // --- Validate ---
    let path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    if !path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("remove_import: file not found: {}", file),
        );
    }

    let lang = match detect_language(&path) {
        Some(l) => l,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!(
                    "remove_import: unsupported file extension: {}",
                    path.extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("<none>")
                ),
            );
        }
    };

    if !imports::is_supported(lang) {
        return Response::error(
            &req.id,
            "invalid_request",
            format!(
                "remove_import: import management not yet supported for {:?}",
                lang
            ),
        );
    }

    // --- Parse file and imports ---
    let (source, _tree, block) = match imports::parse_file_imports(&path, lang) {
        Ok(result) => result,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // --- Find matching import ---
    let matching: Vec<(usize, &imports::ImportStatement)> = block
        .imports
        .iter()
        .enumerate()
        .filter(|(_, imp)| imp.module_path == module)
        .collect();

    if matching.is_empty() {
        let mut result = serde_json::json!({
            "file": file,
            "removed": false,
            "module": module,
            "reason": "module_not_found",
        });
        if let Some(ref n) = name {
            result["name"] = serde_json::json!(n);
        }
        return Response::success(&req.id, result);
    }

    // --- Determine edit ---
    let new_source = if let Some(ref target_name) = name {
        remove_name_from_imports(&source, &matching, target_name, lang)
    } else {
        remove_entire_imports(&source, &matching)
    };
    let removed = new_source != source;

    if !removed {
        let reason = if name.is_some() {
            "name_not_found"
        } else {
            "no_matching_import_removed"
        };
        let mut result = serde_json::json!({
            "file": file,
            "removed": false,
            "module": module,
            "reason": reason,
        });
        if let Some(ref n) = name {
            result["name"] = serde_json::json!(n);
        }
        return Response::success(&req.id, result);
    }

    // --- Auto-backup (skip for dry-run) ---
    let backup_id = if !edit::is_dry_run(&req.params) {
        match edit::auto_backup(ctx, req.session(), &path, "remove_import: pre-edit backup") {
            Ok(id) => id,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        }
    } else {
        None
    };

    // Dry-run: return diff without modifying disk
    if edit::is_dry_run(&req.params) {
        let dr = edit::dry_run_diff(&source, &new_source, &path);
        return Response::success(
            &req.id,
            serde_json::json!({
                "ok": true, "dry_run": true, "removed": true, "diff": dr.diff, "syntax_valid": dr.syntax_valid,
            }),
        );
    }

    // --- Write, format, and validate ---
    let mut write_result =
        match edit::write_format_validate(&path, &new_source, &ctx.config(), &req.params) {
            Ok(r) => r,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        };

    if let Ok(final_content) = std::fs::read_to_string(&path) {
        write_result.lsp_diagnostics = ctx.lsp_post_write(&path, &final_content, &req.params);
    }

    log::debug!("remove_import: {}", file);

    // --- Build response ---
    let mut result = serde_json::json!({
        "file": file,
        "removed": removed,
        "module": module,
        "formatted": write_result.formatted,
    });

    if let Some(ref n) = name {
        result["name"] = serde_json::json!(n);
    }

    if let Some(valid) = write_result.syntax_valid {
        result["syntax_valid"] = serde_json::json!(valid);
    }

    if let Some(ref reason) = write_result.format_skipped_reason {
        result["format_skipped_reason"] = serde_json::json!(reason);
    }

    if write_result.validate_requested {
        result["validation_errors"] = serde_json::json!(write_result.validation_errors);
    }
    if let Some(ref reason) = write_result.validate_skipped_reason {
        result["validate_skipped_reason"] = serde_json::json!(reason);
    }

    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }

    write_result.append_lsp_diagnostics_to(&mut result);
    Response::success(&req.id, result)
}

/// Remove a specific named import from the matched imports.
/// If the import only has that one name, remove the entire statement.
/// If it has multiple names, regenerate without the target name.
fn remove_name_from_imports(
    source: &str,
    matching: &[(usize, &imports::ImportStatement)],
    target_name: &str,
    lang: LangId,
) -> String {
    let mut result = source.to_string();
    // Process in reverse order to preserve byte offsets
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();

    for (_, imp) in matching {
        if imp.names.contains(&target_name.to_string()) {
            if imp.names.len() == 1 {
                // Only one named import — remove entire statement
                let range = line_range(source, &imp.byte_range);
                edits.push((range, String::new()));
            } else {
                // Multiple names — regenerate without target
                let new_names: Vec<String> = imp
                    .names
                    .iter()
                    .filter(|n| n.as_str() != target_name)
                    .cloned()
                    .collect();
                let new_line = imports::generate_import_line(
                    lang,
                    &imp.module_path,
                    &new_names,
                    imp.default_import.as_deref(),
                    imp.kind == imports::ImportKind::Type,
                );
                edits.push((imp.byte_range.clone(), new_line));
            }
        } else if imp.default_import.as_deref() == Some(target_name) {
            // Removing the default import
            if imp.names.is_empty() {
                // Only default — remove entire statement
                let range = line_range(source, &imp.byte_range);
                edits.push((range, String::new()));
            } else {
                // Has named imports too — regenerate without default
                let new_line = imports::generate_import_line(
                    lang,
                    &imp.module_path,
                    &imp.names,
                    None,
                    imp.kind == imports::ImportKind::Type,
                );
                edits.push((imp.byte_range.clone(), new_line));
            }
        }
    }

    // Apply edits in reverse order to preserve offsets
    edits.sort_by(|a, b| b.0.start.cmp(&a.0.start));
    for (range, replacement) in edits {
        result = format!(
            "{}{}{}",
            &result[..range.start],
            replacement,
            &result[range.end..]
        );
    }

    result
}

/// Remove entire import statements for all matching imports.
fn remove_entire_imports(source: &str, matching: &[(usize, &imports::ImportStatement)]) -> String {
    let mut result = source.to_string();
    // Process in reverse order to preserve byte offsets
    let mut ranges: Vec<std::ops::Range<usize>> = matching
        .iter()
        .map(|(_, imp)| line_range(source, &imp.byte_range))
        .collect();
    ranges.sort_by(|a, b| b.start.cmp(&a.start));

    for range in ranges {
        result = format!("{}{}", &result[..range.start], &result[range.end..]);
    }

    result
}

/// Expand a byte range to include the full line (including trailing newline).
fn line_range(source: &str, range: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    let start = range.start;
    let mut end = range.end;

    // Include trailing newline
    if end < source.len() {
        let bytes = source.as_bytes();
        if bytes[end] == b'\n' {
            end += 1;
        } else if bytes[end] == b'\r' {
            end += 1;
            if end < source.len() && bytes[end] == b'\n' {
                end += 1;
            }
        }
    }

    start..end
}
