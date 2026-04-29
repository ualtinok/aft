use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct BashKillParams {
    #[serde(default)]
    task_id: Option<String>,
}

pub fn handle(req: &RawRequest, ctx: &AppContext) -> Response {
    let raw_params = req
        .params
        .get("params")
        .cloned()
        .unwrap_or_else(|| req.params.clone());
    let params = match serde_json::from_value::<BashKillParams>(raw_params) {
        Ok(params) => params,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("bash_kill: invalid params: {e}"),
            );
        }
    };

    let Some(task_id) = params.task_id else {
        return Response::error(&req.id, "invalid_request", "bash_kill: missing task_id");
    };

    match ctx.bash_background().kill(&task_id) {
        Ok(snapshot) => Response::success(&req.id, json!(snapshot)),
        Err(message) if message.contains("not found") => {
            Response::error(&req.id, "task_not_found", message)
        }
        Err(message) => Response::error(&req.id, "kill_failed", message),
    }
}
