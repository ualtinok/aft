//! Handler for the `edit_match` command: content-based string matching with
//! disambiguation for multiple occurrences.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle an `edit_match` request.
///
/// Params:
///   - `file` (string, required) — target file path
///   - `match` (string, required, non-empty) — literal string to find
///   - `replacement` (string, required) — replacement content
///   - `occurrence` (integer, optional, 0-indexed) — select a specific occurrence
///
/// Returns on success: `{ file, replacements: 1, syntax_valid, backup_id? }`
/// Returns on ambiguity: `{ code: "ambiguous_match", occurrences: [{ index, line, context }] }`
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

    let occurrence = req
        .params
        .get("occurrence")
        .and_then(|v| v.as_i64())
        .map(|v| v as usize);

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
            return Response::error(
                &req.id,
                "file_not_found",
                format!("{}: {}", file, e),
            );
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

    // If occurrence specified but out of range
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

    // Multiple matches without occurrence selector → disambiguation
    if positions.len() > 1 && occurrence.is_none() {
        let occurrences: Vec<serde_json::Value> = positions
            .iter()
            .enumerate()
            .map(|(idx, &byte_pos)| {
                let line = source[..byte_pos].matches('\n').count();
                let context = build_context(&source, line, 2);
                serde_json::json!({
                    "index": idx,
                    "line": line,
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

    // Single match or occurrence specified — apply the edit
    let target_idx = occurrence.unwrap_or(0);
    let byte_start = positions[target_idx];
    let byte_end = byte_start + match_str.len();

    // Auto-backup before mutation (skip for dry-run)
    let backup_id = if !edit::is_dry_run(&req.params) {
        match edit::auto_backup(ctx, path, &format!("edit_match: {}", match_str)) {
            Ok(id) => id,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        }
    } else {
        None
    };

    let new_source = edit::replace_byte_range(&source, byte_start, byte_end, replacement);

    // Dry-run: return diff without modifying disk
    if edit::is_dry_run(&req.params) {
        let dr = edit::dry_run_diff(&source, &new_source, path);
        return Response::success(&req.id, serde_json::json!({
            "ok": true, "dry_run": true, "diff": dr.diff, "syntax_valid": dr.syntax_valid,
        }));
    }

    // Write, format, and validate via shared pipeline
    let write_result = match edit::write_format_validate(path, &new_source, &ctx.config(), &req.params) {
        Ok(r) => r,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    eprintln!("[aft] edit_match: {} in {}", match_str, file);

    let syntax_valid = write_result.syntax_valid.unwrap_or(true);

    let mut result = serde_json::json!({
        "file": file,
        "replacements": 1,
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

    Response::success(&req.id, result)
}

/// Build a context string showing the target line ± `margin` lines.
fn build_context(source: &str, target_line: usize, margin: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = target_line.saturating_sub(margin);
    let end = (target_line + margin + 1).min(lines.len());
    lines[start..end].join("\n")
}
