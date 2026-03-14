//! Handler for the `edit_symbol` command: symbol-level editing with
//! resolve → backup → edit → validate cycle.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};
use crate::symbols::Range;

/// Handle an `edit_symbol` request.
///
/// Params:
///   - `file` (string, required) — target file path
///   - `symbol` (string, required) — symbol name to edit
///   - `operation` (string, required) — one of: replace, delete, insert_before, insert_after
///   - `content` (string, optional) — replacement/insertion content (required for replace/insert_*)
///   - `scope` (string, optional) — scope qualifier to disambiguate (e.g. "ClassName")
///
/// Returns on success: `{ file, symbol, operation, range, new_range?, syntax_valid, backup_id }`
/// Returns on ambiguity: `{ code: "ambiguous_symbol", candidates: [...] }`
pub fn handle_edit_symbol(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_symbol: missing required param 'file'",
            );
        }
    };

    let symbol_name = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_symbol: missing required param 'symbol'",
            );
        }
    };

    let operation = match req.params.get("operation").and_then(|v| v.as_str()) {
        Some(op) => op,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_symbol: missing required param 'operation'",
            );
        }
    };

    // Validate operation
    if !["replace", "delete", "insert_before", "insert_after"].contains(&operation) {
        return Response::error(
            &req.id,
            "invalid_request",
            format!(
                "edit_symbol: invalid operation '{}', expected: replace, delete, insert_before, insert_after",
                operation
            ),
        );
    }

    let content = req.params.get("content").and_then(|v| v.as_str());
    let scope = req.params.get("scope").and_then(|v| v.as_str());

    // Content is required for replace, insert_before, insert_after
    if operation != "delete" && content.is_none() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!(
                "edit_symbol: 'content' is required for operation '{}'",
                operation
            ),
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

    // Resolve symbol
    let matches = match ctx.provider().resolve_symbol(path, symbol_name) {
        Ok(m) => m,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Disambiguation
    let filtered = if matches.len() > 1 {
        if let Some(scope_filter) = scope {
            let narrowed: Vec<_> = matches
                .into_iter()
                .filter(|m| {
                    m.symbol.scope_chain.iter().any(|s| s == scope_filter)
                        || m.symbol.parent.as_deref() == Some(scope_filter)
                })
                .collect();
            narrowed
        } else {
            matches
        }
    } else {
        matches
    };

    if filtered.len() > 1 {
        // Return structured disambiguation response
        let candidates: Vec<serde_json::Value> = filtered
            .iter()
            .map(|m| {
                let sym = &m.symbol;
                let qualified = if sym.scope_chain.is_empty() {
                    sym.name.clone()
                } else {
                    format!("{}::{}", sym.scope_chain.join("::"), sym.name)
                };
                let kind_str = serde_json::to_value(&sym.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| format!("{:?}", sym.kind).to_lowercase());
                serde_json::json!({
                    "name": sym.name,
                    "qualified": qualified,
                    "line": sym.range.start_line,
                    "kind": kind_str,
                })
            })
            .collect();

        return Response::success(
            &req.id,
            serde_json::json!({
                "code": "ambiguous_symbol",
                "candidates": candidates,
            }),
        );
    }

    if filtered.is_empty() {
        return Response::error(
            &req.id,
            "symbol_not_found",
            format!("symbol '{}' not found in {}", symbol_name, file),
        );
    }

    let target = &filtered[0].symbol;
    let original_range = target.range.clone();

    // Read file content
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

    // Convert symbol range to byte offsets
    let start_byte = edit::line_col_to_byte(&source, target.range.start_line, target.range.start_col);
    let end_byte = edit::line_col_to_byte(&source, target.range.end_line, target.range.end_col);

    // Apply operation
    let new_source = match operation {
        "replace" => {
            let replacement = content.unwrap();
            edit::replace_byte_range(&source, start_byte, end_byte, replacement)
        }
        "delete" => edit::replace_byte_range(&source, start_byte, end_byte, ""),
        "insert_before" => {
            let insertion = content.unwrap();
            let insert_text = format!("{}\n", insertion);
            edit::replace_byte_range(&source, start_byte, start_byte, &insert_text)
        }
        "insert_after" => {
            let insertion = content.unwrap();
            let insert_text = format!("\n{}", insertion);
            edit::replace_byte_range(&source, end_byte, end_byte, &insert_text)
        }
        _ => unreachable!(),
    };

    // Dry-run: return diff without modifying disk
    if edit::is_dry_run(&req.params) {
        let dr = edit::dry_run_diff(&source, &new_source, path);
        return Response::success(&req.id, serde_json::json!({
            "ok": true, "dry_run": true, "diff": dr.diff, "syntax_valid": dr.syntax_valid,
        }));
    }

    // Auto-backup before writing
    let backup_id = match edit::auto_backup(ctx, path, &format!("edit_symbol: {} {}", operation, symbol_name)) {
        Ok(id) => id,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Write, format, and validate via shared pipeline
    let write_result = match edit::write_format_validate(path, &new_source, &ctx.config(), &req.params) {
        Ok(r) => r,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    eprintln!("[aft] edit_symbol: {} in {}", symbol_name, file);

    // Compute new range for replace and insert operations
    let new_range = match operation {
        "replace" => {
            let replacement = content.unwrap();
            let new_lines = replacement.lines().count() as u32;
            let last_line_len = replacement.lines().last().map(|l| l.len() as u32).unwrap_or(0);
            Some(Range {
                start_line: original_range.start_line,
                start_col: original_range.start_col,
                end_line: original_range.start_line + new_lines.saturating_sub(1),
                end_col: if new_lines <= 1 {
                    original_range.start_col + last_line_len
                } else {
                    last_line_len
                },
            })
        }
        _ => None,
    };

    let syntax_valid = write_result.syntax_valid.unwrap_or(true);

    let mut result = serde_json::json!({
        "file": file,
        "symbol": symbol_name,
        "operation": operation,
        "range": original_range,
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

    if let Some(nr) = new_range {
        result["new_range"] = serde_json::to_value(&nr).unwrap();
    }

    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }

    Response::success(&req.id, result)
}
