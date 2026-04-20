//! Handler for the `move_file` command: rename/move a file with backup.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle a `move_file` request.
///
/// Params:
///   - `file` (string, required) — source file path
///   - `destination` (string, required) — destination file path
///
/// Returns: `{ file, destination, moved, backup_id }`
pub fn handle_move_file(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "move_file: missing required param 'file'",
            );
        }
    };

    let destination = match req.params.get("destination").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "move_file: missing required param 'destination'",
            );
        }
    };

    let src_path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    let dst_path = match ctx.validate_path(&req.id, Path::new(destination)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    if !src_path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("move_file: source file not found: {}", file),
        );
    }

    if src_path.is_dir() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("move_file: '{}' is a directory, not a file", file),
        );
    }

    if dst_path.exists() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("move_file: destination already exists: {}", destination),
        );
    }

    // Backup source before moving
    let backup_id = match edit::auto_backup(ctx, req.session(), src_path.as_path(), "move_file: pre-move backup") {
        Ok(id) => id,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Create parent directories for destination
    if let Some(parent) = dst_path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Response::error(
                    &req.id,
                    "io_error",
                    format!("move_file: failed to create directories: {}", e),
                );
            }
        }
    }

    // Move the file
    let mut source_delete_failed = false;
    if let Err(e) = std::fs::rename(&src_path, &dst_path) {
        // rename() can fail across filesystems — fallback to copy+delete
        match std::fs::copy(&src_path, &dst_path) {
            Ok(_) => {
                if let Err(e2) = std::fs::remove_file(&src_path) {
                    log::warn!(
                        "[aft] move_file: copied but failed to remove source: {}",
                        e2
                    );
                    source_delete_failed = true;
                }
            }
            Err(_) => {
                return Response::error(
                    &req.id,
                    "io_error",
                    format!("move_file: failed to move file: {}", e),
                );
            }
        }
    }

    log::debug!("move_file: {} -> {}", file, destination);

    let mut result = serde_json::json!({
        "file": file,
        "destination": destination,
        "moved": !source_delete_failed,
    });

    if source_delete_failed {
        result["warning"] = serde_json::json!("source file could not be deleted after copy");
    }

    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }

    Response::success(&req.id, result)
}
