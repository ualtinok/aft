//! Handler for the `edit_match` command: content-based string matching with
//! disambiguation for multiple occurrences.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle an `edit_match` request.
///
/// Params:
///   - `file` (string, required) — target file path or glob pattern (e.g. `**/*.ts`)
///   - `match` (string, required, non-empty) — literal string to find
///   - `replacement` (string, required) — replacement content
///   - `occurrence` (integer, optional, 0-indexed) — select a specific occurrence (single-file only)
///   - `replace_all` (bool, optional) — replace all occurrences (default: false)
///   - `dry_run` (bool, optional) — preview changes without writing
///
/// When `file` is a glob pattern:
///   - Applies match/replace across all matching files
///   - `replace_all` is implicitly true
///   - `occurrence` is ignored
///   - Returns: `{ ok, files: [{ file, replacements, ... }], total_replacements, total_files }`
///
/// When `file` is a literal path:
///   - Original single-file behavior
///   - Returns: `{ file, replacements: 1, syntax_valid, backup_id? }`
pub fn handle_edit_match(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_match: missing required param 'file'",
            );
        }
    };

    let match_str = match req.params.get("match").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_match: missing required param 'match'",
            );
        }
    };

    if match_str.is_empty() {
        return Response::error(
            &req.id,
            "invalid_request",
            "edit_match: 'match' must be a non-empty string",
        );
    }

    let replacement = match req.params.get("replacement").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_match: missing required param 'replacement'",
            );
        }
    };

    // No custom escape interpretation. JSON transport already handles escape
    // sequences before the string reaches us. Adding unescape_str on top caused
    // double-interpretation that corrupted source code with literal escapes.

    // Detect glob pattern
    if is_glob_pattern(file) {
        return handle_glob_edit_match(req, ctx, file, match_str, replacement);
    }

    // Single-file path
    handle_single_file_edit_match(req, ctx, file, match_str, replacement)
}

/// Returns true if the file path contains glob characters.
fn is_glob_pattern(path: &str) -> bool {
    path.contains('*') || path.contains('?') || path.contains('{') || path.contains('[')
}

/// Handle a glob-based multi-file edit_match.
fn handle_glob_edit_match(
    req: &RawRequest,
    ctx: &AppContext,
    pattern: &str,
    match_str: &str,
    replacement: &str,
) -> Response {
    let dry_run = edit::is_dry_run(&req.params);

    // Resolve glob relative to project root (or cwd)
    let config = ctx.config();
    let root = config
        .project_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    drop(config);
    let full_pattern = if pattern.starts_with('/') {
        pattern.to_string()
    } else {
        format!("{}/{}", root.display(), pattern)
    };

    let paths: Vec<std::path::PathBuf> = match glob::glob(&full_pattern) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|p| p.is_file())
            .collect(),
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("edit_match: invalid glob pattern: {}", e),
            );
        }
    };

    if paths.is_empty() {
        return Response::error(
            &req.id,
            "match_not_found",
            format!("edit_match: no files matched glob '{}'", pattern),
        );
    }

    let config = ctx.config();
    let mut file_results: Vec<serde_json::Value> = Vec::new();
    let mut total_replacements: usize = 0;
    let mut total_files: usize = 0;
    let mut diffs: Vec<serde_json::Value> = Vec::new();

    for path in &paths {
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue, // skip unreadable files
        };

        // Find matches
        let positions: Vec<usize> = source
            .match_indices(match_str)
            .map(|(idx, _)| idx)
            .collect();

        if positions.is_empty() {
            continue; // no matches in this file
        }

        let count = positions.len();
        let new_source = source.replace(match_str, replacement);
        let file_str = path.display().to_string();

        if dry_run {
            let dr = edit::dry_run_diff(&source, &new_source, path);
            diffs.push(serde_json::json!({
                "file": file_str,
                "replacements": count,
                "diff": dr.diff,
                "syntax_valid": dr.syntax_valid,
            }));
            total_replacements += count;
            total_files += 1;
            continue;
        }

        // Backup before first mutation
        if let Err(e) = edit::auto_backup(ctx, path, &format!("glob_edit_match: {}", match_str)) {
            return Response::error(&req.id, e.code(), e.to_string());
        }

        // Write, format, validate
        let write_result =
            match edit::write_format_validate(path, &new_source, &config, &req.params) {
                Ok(r) => r,
                Err(e) => {
                    return Response::error(&req.id, e.code(), e.to_string());
                }
            };

        if let Ok(final_content) = std::fs::read_to_string(path) {
            ctx.lsp_notify_file_changed(path, &final_content);
        }

        let mut result = serde_json::json!({
            "file": file_str,
            "replacements": count,
            "syntax_valid": write_result.syntax_valid.unwrap_or(true),
            "formatted": write_result.formatted,
        });

        if let Some(ref reason) = write_result.format_skipped_reason {
            result["format_skipped_reason"] = serde_json::json!(reason);
        }

        file_results.push(result);
        total_replacements += count;
        total_files += 1;
    }

    if dry_run {
        if diffs.is_empty() {
            return Response::error(
                &req.id,
                "match_not_found",
                format!(
                    "edit_match: '{}' not found in any files matching '{}'",
                    match_str, pattern
                ),
            );
        }
        return Response::success(
            &req.id,
            serde_json::json!({
                "ok": true,
                "dry_run": true,
                "files": diffs,
                "total_replacements": total_replacements,
                "total_files": total_files,
            }),
        );
    }

    if file_results.is_empty() {
        return Response::error(
            &req.id,
            "match_not_found",
            format!(
                "edit_match: '{}' not found in any files matching '{}'",
                match_str, pattern
            ),
        );
    }

    eprintln!(
        "[aft] edit_match (glob): {} replacements across {} files",
        total_replacements, total_files
    );

    Response::success(
        &req.id,
        serde_json::json!({
            "ok": true,
            "files": file_results,
            "total_replacements": total_replacements,
            "total_files": total_files,
        }),
    )
}

/// Handle a single-file edit_match (original behavior).
fn handle_single_file_edit_match(
    req: &RawRequest,
    ctx: &AppContext,
    file: &str,
    match_str: &str,
    replacement: &str,
) -> Response {
    let occurrence = req
        .params
        .get("occurrence")
        .and_then(|v| v.as_i64())
        .map(|v| v as usize);

    let replace_all = req
        .params
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = Path::new(file);
    if !path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("file not found: {}", file),
        );
    }

    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Response::error(&req.id, "file_not_found", format!("{}: {}", file, e));
        }
    };

    // Find all byte-offset positions of the match string
    let positions: Vec<usize> = source
        .match_indices(match_str)
        .map(|(idx, _)| idx)
        .collect();

    if positions.is_empty() {
        return Response::error(
            &req.id,
            "match_not_found",
            format!("edit_match: '{}' not found in {}", match_str, file),
        );
    }

    // If occurrence specified but out of range (only relevant when not replace_all)
    if !replace_all {
        if let Some(occ) = occurrence {
            if occ >= positions.len() {
                return Response::error(
                    &req.id,
                    "invalid_request",
                    format!(
                        "edit_match: occurrence {} out of range, file has {} occurrence(s)",
                        occ,
                        positions.len()
                    ),
                );
            }
        }
    }

    // Multiple matches without occurrence selector → disambiguation (unless replace_all)
    if positions.len() > 1 && occurrence.is_none() && !replace_all {
        let occurrences: Vec<serde_json::Value> = positions
            .iter()
            .enumerate()
            .map(|(idx, &byte_pos)| {
                let line = source[..byte_pos].matches('\n').count();
                let context = build_context(&source, line, 2);
                serde_json::json!({
                    "index": idx,
                    "line": line + 1,
                    "context": context,
                })
            })
            .collect();

        return Response::success(
            &req.id,
            serde_json::json!({
                "code": "ambiguous_match",
                "occurrences": occurrences,
            }),
        );
    }

    // Auto-backup before mutation (skip for dry-run)
    let backup_id = if !edit::is_dry_run(&req.params) {
        let label = if replace_all {
            format!(
                "edit_match: {} (replace_all x{})",
                match_str,
                positions.len()
            )
        } else {
            format!("edit_match: {}", match_str)
        };
        match edit::auto_backup(ctx, path, &label) {
            Ok(id) => id,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        }
    } else {
        None
    };

    // Apply edit(s)
    let (new_source, count) = if replace_all {
        let count = positions.len();
        (source.replace(match_str, replacement), count)
    } else {
        let target_idx = occurrence.unwrap_or(0);
        let byte_start = positions[target_idx];
        let byte_end = byte_start + match_str.len();
        (
            edit::replace_byte_range(&source, byte_start, byte_end, replacement),
            1,
        )
    };

    // Dry-run: return diff without modifying disk
    if edit::is_dry_run(&req.params) {
        let dr = edit::dry_run_diff(&source, &new_source, path);
        return Response::success(
            &req.id,
            serde_json::json!({
                "ok": true, "dry_run": true, "diff": dr.diff, "syntax_valid": dr.syntax_valid,
            }),
        );
    }

    // Write, format, and validate via shared pipeline
    let mut write_result =
        match edit::write_format_validate(path, &new_source, &ctx.config(), &req.params) {
            Ok(r) => r,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        };

    if let Ok(final_content) = std::fs::read_to_string(path) {
        write_result.lsp_diagnostics = ctx.lsp_post_write(path, &final_content, &req.params);
    }

    eprintln!("[aft] edit_match: {} in {}", match_str, file);

    let syntax_valid = write_result.syntax_valid.unwrap_or(true);

    let mut result = serde_json::json!({
        "file": file,
        "replacements": count,
        "syntax_valid": syntax_valid,
        "formatted": write_result.formatted,
    });

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

/// Build a context string showing the target line ± `margin` lines.
fn build_context(source: &str, target_line: usize, margin: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = target_line.saturating_sub(margin);
    let end = (target_line + margin + 1).min(lines.len());
    lines[start..end].join("\n")
}
