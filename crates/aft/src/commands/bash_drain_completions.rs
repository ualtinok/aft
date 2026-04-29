use serde_json::json;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

pub fn handle(req: &RawRequest, ctx: &AppContext) -> Response {
    Response::success(
        &req.id,
        json!({
            "bg_completions": ctx.bash_background().drain_completions_for_session(Some(req.session())),
        }),
    )
}
