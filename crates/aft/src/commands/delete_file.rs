//! Handler for the `delete_file` command: remove a file with backup.

use std::path::Path;

use crate::context::AppContext;
use crate::edit;
use crate::protocol::{RawRequest, Response};

/// Handle a `delete_file` request.
///
/// Params:
///   - `file` (string, required) — path to the file to delete
///
/// Returns: `{ file, deleted, backup_id }`
pub fn handle_delete_file(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "delete_file: missing required param 'file'",
            );
        }
    };

    let path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    if !path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("delete_file: file not found: {}", file),
        );
    }

    if path.is_dir() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("delete_file: '{}' is a directory, not a file", file),
        );
    }

    // Backup before deletion
    let backup_id = match edit::auto_backup(ctx, req.session(), &path, "delete_file: pre-delete backup") {
        Ok(id) => id,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Delete the file
    if let Err(e) = std::fs::remove_file(path) {
        return Response::error(
            &req.id,
            "io_error",
            format!("delete_file: failed to delete: {}", e),
        );
    }

    log::debug!("delete_file: {}", file);

    let mut result = serde_json::json!({
        "file": file,
        "deleted": true,
    });

    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }

    Response::success(&req.id, result)
}
