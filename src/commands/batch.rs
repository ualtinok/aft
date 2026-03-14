//! Handler for the `batch` command: atomic multi-edit with rollback.
//!
//! Accepts an array of edits (string-match or line-range), validates all against
//! the original content, then applies bottom-to-top to prevent line drift.
//! Takes a single auto-backup before applying. No file modification on failure.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// A validated edit ready to apply, carrying byte offsets into the original content.
struct ResolvedEdit {
    byte_start: usize,
    byte_end: usize,
    replacement: String,
}

/// Handle a `batch` request.
///
/// Params:
///   - `file` (string, required) — target file path
///   - `edits` (array, required) — each element is either:
///       - `{ "match": "...", "replacement": "..." }` — string match-replace
///       - `{ "line_start": N, "line_end": N, "content": "..." }` — line range replacement (0-indexed, inclusive)
///
/// Returns on success: `{ file, edits_applied, syntax_valid, backup_id? }`
/// Returns on failure: error with the failing edit index.
pub fn handle_batch(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "batch: missing required param 'file'",
            );
        }
    };

    let edits = match req.params.get("edits").and_then(|v| v.as_array()) {
        Some(e) => e,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "batch: missing required param 'edits' (expected array)",
            );
        }
    };

    if edits.is_empty() {
        return Response::error(
            &req.id,
            "invalid_request",
            "batch: 'edits' array must not be empty",
        );
    }

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

    // Phase 1: Validate all edits and resolve to byte offsets
    let mut resolved: Vec<ResolvedEdit> = Vec::with_capacity(edits.len());

    for (i, edit_val) in edits.iter().enumerate() {
        match resolve_edit(&source, edit_val, i) {
            Ok(r) => resolved.push(r),
            Err(resp) => return resp,
        }
    }

    // Phase 2: Auto-backup once before applying (skip for dry-run)
    let dry_run = edit::is_dry_run(&req.params);
    let backup_id = if !dry_run {
        match edit::auto_backup(ctx, path, "batch: pre-batch backup") {
            Ok(id) => id,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        }
    } else {
        None
    };

    // Phase 3: Sort edits by byte_start descending (bottom-to-top) to prevent drift
    resolved.sort_by(|a, b| b.byte_start.cmp(&a.byte_start));

    // Phase 4: Apply all edits sequentially to the content
    let mut content = source.clone();
    for r in &resolved {
        content = edit::replace_byte_range(&content, r.byte_start, r.byte_end, &r.replacement);
    }

    // Dry-run: return combined diff without modifying disk
    if dry_run {
        let dr = edit::dry_run_diff(&source, &content, path);
        return Response::success(&req.id, serde_json::json!({
            "ok": true, "dry_run": true, "diff": dr.diff, "syntax_valid": dr.syntax_valid,
        }));
    }

    // Phase 5: Write, format, and validate via shared pipeline
    let write_result = match edit::write_format_validate(path, &content, ctx.config(), &req.params) {
        Ok(r) => r,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    eprintln!("[aft] batch: {} edits in {}", edits.len(), file);

    let syntax_valid = write_result.syntax_valid.unwrap_or(true);

    let mut result = serde_json::json!({
        "file": file,
        "edits_applied": edits.len(),
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

/// Resolve a single edit object to byte offsets against the original source.
///
/// Returns `Ok(ResolvedEdit)` on success, `Err(Response)` on validation failure.
fn resolve_edit(
    source: &str,
    edit_val: &serde_json::Value,
    index: usize,
) -> Result<ResolvedEdit, Response> {
    // Detect edit type: match-replace or line-range
    if let Some(match_str) = edit_val.get("match").and_then(|v| v.as_str()) {
        // String match-replace
        let replacement = edit_val
            .get("replacement")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let positions: Vec<usize> = source
            .match_indices(match_str)
            .map(|(idx, _)| idx)
            .collect();

        if positions.is_empty() {
            return Err(Response::error(
                "_batch",
                "batch_edit_failed",
                format!(
                    "batch: edit[{}] match '{}' not found in file",
                    index, match_str
                ),
            ));
        }

        if positions.len() > 1 {
            return Err(Response::error(
                "_batch",
                "batch_edit_failed",
                format!(
                    "batch: edit[{}] match '{}' is ambiguous ({} occurrences, expected 1)",
                    index,
                    match_str,
                    positions.len()
                ),
            ));
        }

        let byte_start = positions[0];
        let byte_end = byte_start + match_str.len();

        Ok(ResolvedEdit {
            byte_start,
            byte_end,
            replacement: replacement.to_string(),
        })
    } else if edit_val.get("line_start").is_some() {
        // Line-range replacement
        let line_start = edit_val
            .get("line_start")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .ok_or_else(|| {
                Response::error(
                    "_batch",
                    "invalid_request",
                    format!("batch: edit[{}] 'line_start' must be a non-negative integer", index),
                )
            })?;

        let line_end = edit_val
            .get("line_end")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .ok_or_else(|| {
                Response::error(
                    "_batch",
                    "invalid_request",
                    format!("batch: edit[{}] 'line_end' must be a non-negative integer", index),
                )
            })?;

        let content = edit_val
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let lines: Vec<&str> = source.lines().collect();
        let total_lines = lines.len();

        if line_start >= total_lines {
            return Err(Response::error(
                "_batch",
                "batch_edit_failed",
                format!(
                    "batch: edit[{}] line_start {} out of range (file has {} lines)",
                    index, line_start, total_lines
                ),
            ));
        }

        if line_end >= total_lines {
            return Err(Response::error(
                "_batch",
                "batch_edit_failed",
                format!(
                    "batch: edit[{}] line_end {} out of range (file has {} lines)",
                    index, line_end, total_lines
                ),
            ));
        }

        if line_start > line_end {
            return Err(Response::error(
                "_batch",
                "invalid_request",
                format!(
                    "batch: edit[{}] line_start {} > line_end {}",
                    index, line_start, line_end
                ),
            ));
        }

        // Convert line range to byte offsets
        let byte_start = line_byte_offset(source, line_start);
        // line_end is inclusive: end byte is at the end of line_end (including its newline if present)
        let byte_end = line_end_byte_offset(source, line_end);

        Ok(ResolvedEdit {
            byte_start,
            byte_end,
            replacement: content.to_string(),
        })
    } else {
        Err(Response::error(
            "_batch",
            "invalid_request",
            format!(
                "batch: edit[{}] must have either 'match' or 'line_start'/'line_end'",
                index
            ),
        ))
    }
}

/// Get the byte offset of the start of a line (0-indexed).
fn line_byte_offset(source: &str, line: usize) -> usize {
    let mut offset = 0;
    for (i, l) in source.lines().enumerate() {
        if i == line {
            return offset;
        }
        offset += l.len() + 1; // +1 for newline
    }
    source.len()
}

/// Get the byte offset of the end of a line (0-indexed, inclusive).
/// Includes the trailing newline if present, so the replaced range covers the full line.
fn line_end_byte_offset(source: &str, line: usize) -> usize {
    let mut offset = 0;
    for (i, l) in source.lines().enumerate() {
        offset += l.len();
        if i == line {
            // Include trailing newline if it exists
            if offset < source.len() && source.as_bytes()[offset] == b'\n' {
                offset += 1;
            }
            return offset;
        }
        offset += 1; // newline
    }
    source.len()
}
