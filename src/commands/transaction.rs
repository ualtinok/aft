//! Handler for the `transaction` command: multi-file atomic edits with rollback.
//!
//! Applies edits to multiple files atomically. Phase model:
//! 1. Parse & validate all operations upfront
//! 2. Snapshot all existing files
//! 3. Apply each operation (write or edit_match)
//! 4. Validate syntax on all results
//! 5. Rollback all on any failure; success only if every file passes

use std::collections::HashSet;
use std::path::PathBuf;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// A parsed operation ready for execution.
struct ParsedOp {
    file: PathBuf,
    kind: OpKind,
}

enum OpKind {
    Write { content: String },
    EditMatch { match_str: String, replacement: String },
}

/// Per-file result after a successful apply.
struct FileResult {
    file: String,
    syntax_valid: Option<bool>,
    formatted: bool,
    format_skipped_reason: Option<String>,
}

/// Handle a `transaction` request.
///
/// Params:
///   - `operations` (array, required) — each element is an object with:
///       - `file` (string) — target file path
///       - `command` (string) — "write" or "edit_match"
///       - For "write": `content` (string)
///       - For "edit_match": `match` (string) + `replacement` (string)
///   - `dry_run` (bool, optional) — preview without modifying disk
///
/// On success: `{ ok, files_modified, results: [{ file, syntax_valid, formatted, format_skipped_reason }] }`
/// On failure: `{ error: { code: "transaction_failed", message, failed_operation, rolled_back } }`
/// On dry-run: `{ ok, dry_run, diffs: [{ file, diff, syntax_valid }] }`
pub fn handle_transaction(req: &RawRequest, ctx: &AppContext) -> Response {
    // --- Parse operations ---
    let operations = match req.params.get("operations").and_then(|v| v.as_array()) {
        Some(ops) => ops,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "transaction: missing required param 'operations' (expected array)",
            );
        }
    };

    if operations.is_empty() {
        return Response::error(
            &req.id,
            "invalid_request",
            "transaction: 'operations' array must not be empty",
        );
    }

    // Validate all operations upfront before any mutations
    let mut parsed: Vec<ParsedOp> = Vec::with_capacity(operations.len());
    for (i, op) in operations.iter().enumerate() {
        match parse_operation(op, i) {
            Ok(p) => parsed.push(p),
            Err(msg) => return Response::error(&req.id, "invalid_request", msg),
        }
    }

    let dry_run = edit::is_dry_run(&req.params);

    // --- Dry-run path: compute diffs without touching disk ---
    if dry_run {
        return handle_dry_run(req, &parsed);
    }

    // --- Snapshot phase ---
    // Track which files existed (snapshotted) vs new (will be deleted on rollback).
    let mut snapshotted_files: Vec<PathBuf> = Vec::new();
    let mut new_files: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for op in &parsed {
        if seen.contains(&op.file) {
            continue;
        }
        seen.insert(op.file.clone());

        if op.file.exists() {
            // Snapshot existing file — scoped borrow (D029)
            let snapshot_result = {
                let mut store = ctx.backup().borrow_mut();
                store.snapshot(&op.file, "transaction")
            };
            if let Err(e) = snapshot_result {
                return Response::error(&req.id, e.code(), e.to_string());
            }
            snapshotted_files.push(op.file.clone());
        } else {
            new_files.push(op.file.clone());
        }
    }

    // --- Apply phase ---
    let mut results: Vec<FileResult> = Vec::new();

    for (i, op) in parsed.iter().enumerate() {
        let new_content = match compute_new_content(op) {
            Ok(c) => c,
            Err(msg) => {
                rollback(ctx, &snapshotted_files, &new_files);
                return transaction_error(&req.id, i, &snapshotted_files, &new_files, &msg);
            }
        };

        match edit::write_format_validate(&op.file, &new_content, &ctx.config(), &req.params) {
            Ok(wr) => {
                // Track this file as new if it was created by this operation
                // (in case earlier ops in the same transaction created it)
                if !snapshotted_files.contains(&op.file) && !new_files.contains(&op.file) {
                    new_files.push(op.file.clone());
                }
                results.push(FileResult {
                    file: op.file.display().to_string(),
                    syntax_valid: wr.syntax_valid,
                    formatted: wr.formatted,
                    format_skipped_reason: wr.format_skipped_reason,
                });
            }
            Err(e) => {
                rollback(ctx, &snapshotted_files, &new_files);
                return transaction_error(
                    &req.id,
                    i,
                    &snapshotted_files,
                    &new_files,
                    &format!("write failed: {}", e),
                );
            }
        }
    }

    // --- Validate phase: check syntax_valid on all results ---
    for (i, result) in results.iter().enumerate() {
        if result.syntax_valid == Some(false) {
            rollback(ctx, &snapshotted_files, &new_files);
            return transaction_error(
                &req.id,
                i,
                &snapshotted_files,
                &new_files,
                &format!("syntax error in {}", result.file),
            );
        }
    }

    // --- Success ---
    let files_modified = results.len();
    let result_json: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let mut v = serde_json::json!({
                "file": r.file,
                "syntax_valid": r.syntax_valid,
                "formatted": r.formatted,
            });
            if let Some(ref reason) = r.format_skipped_reason {
                v["format_skipped_reason"] = serde_json::json!(reason);
            }
            v
        })
        .collect();

    eprintln!(
        "[aft] transaction: {} files modified successfully",
        files_modified
    );

    Response::success(
        &req.id,
        serde_json::json!({
            "ok": true,
            "files_modified": files_modified,
            "results": result_json,
        }),
    )
}

/// Parse and validate a single operation object.
fn parse_operation(op: &serde_json::Value, index: usize) -> Result<ParsedOp, String> {
    let file = op
        .get("file")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("transaction: operation[{}] missing 'file'", index))?;

    let command = op
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("transaction: operation[{}] missing 'command'", index))?;

    let kind = match command {
        "write" => {
            let content = op
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "transaction: operation[{}] 'write' requires 'content'",
                        index
                    )
                })?;
            OpKind::Write {
                content: content.to_string(),
            }
        }
        "edit_match" => {
            let match_str = op
                .get("match")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "transaction: operation[{}] 'edit_match' requires 'match'",
                        index
                    )
                })?;
            let replacement = op
                .get("replacement")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            OpKind::EditMatch {
                match_str: match_str.to_string(),
                replacement: replacement.to_string(),
            }
        }
        other => {
            return Err(format!(
                "transaction: operation[{}] unknown command '{}' (expected 'write' or 'edit_match')",
                index, other
            ));
        }
    };

    Ok(ParsedOp {
        file: PathBuf::from(file),
        kind,
    })
}

/// Compute the new content for a single operation.
fn compute_new_content(op: &ParsedOp) -> Result<String, String> {
    match &op.kind {
        OpKind::Write { content } => Ok(content.clone()),
        OpKind::EditMatch {
            match_str,
            replacement,
        } => {
            let source = if op.file.exists() {
                std::fs::read_to_string(&op.file).map_err(|e| {
                    format!("failed to read {}: {}", op.file.display(), e)
                })?
            } else {
                String::new()
            };

            let positions: Vec<usize> = source.match_indices(match_str).map(|(i, _)| i).collect();

            if positions.is_empty() {
                return Err(format!(
                    "match '{}' not found in {}",
                    match_str,
                    op.file.display()
                ));
            }
            if positions.len() > 1 {
                return Err(format!(
                    "match '{}' is ambiguous ({} occurrences) in {}",
                    match_str,
                    positions.len(),
                    op.file.display()
                ));
            }

            let start = positions[0];
            let end = start + match_str.len();
            Ok(edit::replace_byte_range(&source, start, end, replacement))
        }
    }
}

/// Dry-run: compute per-file diffs without touching disk.
fn handle_dry_run(req: &RawRequest, ops: &[ParsedOp]) -> Response {
    let mut diffs: Vec<serde_json::Value> = Vec::with_capacity(ops.len());

    for op in ops {
        let original = if op.file.exists() {
            std::fs::read_to_string(&op.file).unwrap_or_default()
        } else {
            String::new()
        };

        let new_content = match compute_new_content_dry(op, &original) {
            Ok(c) => c,
            Err(msg) => {
                return Response::error(&req.id, "invalid_request", msg);
            }
        };

        let dr = edit::dry_run_diff(&original, &new_content, &op.file);
        diffs.push(serde_json::json!({
            "file": op.file.display().to_string(),
            "diff": dr.diff,
            "syntax_valid": dr.syntax_valid,
        }));
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "ok": true,
            "dry_run": true,
            "diffs": diffs,
        }),
    )
}

/// Compute new content for dry-run (uses provided original instead of re-reading).
fn compute_new_content_dry(op: &ParsedOp, original: &str) -> Result<String, String> {
    match &op.kind {
        OpKind::Write { content } => Ok(content.clone()),
        OpKind::EditMatch {
            match_str,
            replacement,
        } => {
            let positions: Vec<usize> = original
                .match_indices(match_str)
                .map(|(i, _)| i)
                .collect();

            if positions.is_empty() {
                return Err(format!(
                    "match '{}' not found in {}",
                    match_str,
                    op.file.display()
                ));
            }
            if positions.len() > 1 {
                return Err(format!(
                    "match '{}' is ambiguous ({} occurrences) in {}",
                    match_str,
                    positions.len(),
                    op.file.display()
                ));
            }

            let start = positions[0];
            let end = start + match_str.len();
            Ok(edit::replace_byte_range(original, start, end, replacement))
        }
    }
}

/// Rollback: restore snapshotted files (reverse order), delete new files.
fn rollback(ctx: &AppContext, snapshotted: &[PathBuf], new_files: &[PathBuf]) {
    // Restore snapshotted files in reverse order
    for path in snapshotted.iter().rev() {
        let result = {
            let mut store = ctx.backup().borrow_mut();
            store.restore_latest(path)
        };
        if let Err(e) = result {
            eprintln!(
                "[aft] transaction rollback: failed to restore {}: {}",
                path.display(),
                e
            );
        }
    }

    // Delete new files
    for path in new_files {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                eprintln!(
                    "[aft] transaction rollback: failed to delete new file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
}

/// Build the structured error response for a failed transaction.
fn transaction_error(
    req_id: &str,
    failed_index: usize,
    snapshotted: &[PathBuf],
    new_files: &[PathBuf],
    message: &str,
) -> Response {
    let mut rolled_back: Vec<serde_json::Value> = snapshotted
        .iter()
        .map(|p| serde_json::json!({ "file": p.display().to_string(), "action": "restored" }))
        .collect();

    for p in new_files {
        rolled_back.push(serde_json::json!({
            "file": p.display().to_string(),
            "action": "deleted",
        }));
    }

    eprintln!(
        "[aft] transaction failed at operation[{}]: {} — rolled back {} files",
        failed_index,
        message,
        rolled_back.len()
    );

    Response::error_with_data(
        req_id,
        "transaction_failed",
        message,
        serde_json::json!({
            "failed_operation": failed_index,
            "rolled_back": rolled_back,
        }),
    )
}
