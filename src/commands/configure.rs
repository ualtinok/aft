use std::path::PathBuf;

use crate::callgraph::CallGraph;
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle a `configure` request.
///
/// Expects `project_root` (string, required) — absolute path to the project root.
/// Sets the project root on `Config`, initializes the `CallGraph` with that root,
/// and returns success with the configured path.
///
/// Stderr log: `[aft] project root set: <path>`
pub fn handle_configure(req: &RawRequest, ctx: &AppContext) -> Response {
    let root = match req.params.get("project_root").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "configure: missing required param 'project_root'",
            );
        }
    };

    let root_path = PathBuf::from(root);
    if !root_path.is_dir() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("configure: project_root is not a directory: {}", root),
        );
    }

    // Set project root on config
    ctx.config_mut().project_root = Some(root_path.clone());

    // Initialize call graph with the project root
    let graph = CallGraph::new(root_path.clone());
    *ctx.callgraph().borrow_mut() = Some(graph);

    eprintln!("[aft] project root set: {}", root_path.display());

    Response::success(
        &req.id,
        serde_json::json!({ "project_root": root_path.display().to_string() }),
    )
}
