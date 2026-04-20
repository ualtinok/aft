use std::path::Path;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle the `undo` command: restore the most recent backup for a file.
///
/// Params: `file` (string, required) — path to the file to undo.
/// Returns: `{ path, backup_id }` on success, or `no_undo_history` error.
pub fn handle_undo(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "undo: missing required param 'file'",
            );
        }
    };

    // Resolve relative paths against project_root so backup keys match
    let config = ctx.config();
    let resolved = if Path::new(file).is_relative() {
        if let Some(ref root) = config.project_root {
            root.join(file)
        } else {
            Path::new(file).to_path_buf()
        }
    } else {
        Path::new(file).to_path_buf()
    };
    drop(config);

    let mut backup = ctx.backup().borrow_mut();

    match backup.restore_latest(req.session(), &resolved) {
        Ok((entry, warning)) => {
            let mut result = serde_json::json!({
                "path": file,
                "backup_id": entry.backup_id,
            });
            if let Some(w) = warning {
                result["warning"] = serde_json::Value::String(w);
            }
            Response::success(&req.id, result)
        }
        Err(e) => Response::error(&req.id, e.code(), e.to_string()),
    }
}
