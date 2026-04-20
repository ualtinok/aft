//! Handler for the `write` command: full file write with auto-backup.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle a `write` request.
///
/// Params:
///   - `file` (string, required) — target file path
///   - `content` (string, required) — content to write
///   - `create_dirs` (bool, optional, default true) — create parent dirs if missing
///
/// Returns: `{ file, created, syntax_valid?, backup_id? }`
pub fn handle_write(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "write: missing required param 'file'",
            );
        }
    };

    let content = match req.params.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "write: missing required param 'content'",
            );
        }
    };

    let create_dirs = req
        .params
        .get("create_dirs")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    let existed = path.exists();

    // Read original content for potential dry-run diff
    let (original, read_warning) = if existed {
        match std::fs::read_to_string(path.as_path()) {
            Ok(content) => (content, None),
            Err(error) => {
                let warning = format!(
                    "write: failed to read existing file before diff for {}: {}",
                    file, error
                );
                log::warn!("{}", warning);
                (String::new(), Some(warning))
            }
        }
    } else {
        (String::new(), None)
    };

    // Dry-run: return diff without modifying disk
    if edit::is_dry_run(&req.params) {
        let dr = edit::dry_run_diff(&original, content, path.as_path());
        let mut result = serde_json::json!({
            "ok": true,
            "dry_run": true,
            "diff": dr.diff,
            "syntax_valid": dr.syntax_valid,
        });
        if let Some(ref warning) = read_warning {
            result["read_warning"] = serde_json::json!(warning);
        }
        return Response::success(&req.id, result);
    }

    // Auto-backup existing file before overwriting
    let backup_id = match edit::auto_backup(ctx, req.session(), path.as_path(), "write: pre-write backup") {
        Ok(id) => id,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Create parent directories if requested
    if create_dirs {
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Response::error(
                        &req.id,
                        "invalid_request",
                        format!("write: failed to create directories: {}", e),
                    );
                }
            }
        }
    }

    // Write, format, and validate via shared pipeline
    let mut write_result =
        match edit::write_format_validate(path.as_path(), content, &ctx.config(), &req.params) {
            Ok(r) => r,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        };

    if let Ok(final_content) = std::fs::read_to_string(path.as_path()) {
        write_result.lsp_diagnostics =
            ctx.lsp_post_write(path.as_path(), &final_content, &req.params);
    }

    log::debug!("write: {}", file);

    let mut result = serde_json::json!({
        "file": file,
        "created": !existed,
        "formatted": write_result.formatted,
    });

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

    if let Some(ref warning) = read_warning {
        result["read_warning"] = serde_json::json!(warning);
    }

    write_result.append_lsp_diagnostics_to(&mut result);

    // Include diff info if requested (for UI metadata)
    if edit::wants_diff(&req.params) {
        let final_content =
            std::fs::read_to_string(path.as_path()).unwrap_or_else(|_| content.to_string());
        result["diff"] = edit::compute_diff_info(&original, &final_content);
    }

    Response::success(&req.id, result)
}
