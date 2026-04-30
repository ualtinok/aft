//! Background bash task management. Phase 0 stub; Phase 1 Track D fills in.

pub mod buffer;
pub mod process;
pub mod registry;

use crate::context::AppContext;
use crate::protocol::Response;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

pub use registry::{BgCompletion, BgTaskRegistry};

#[derive(Debug, Clone, Serialize)]
pub struct BgTaskInfo {
    pub task_id: String,
    pub status: BgTaskStatus,
    pub command: String,
    pub started_at: u64,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BgTaskStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

/// Spawn a bash command in the background. Returns a task_id immediately.
pub fn spawn(
    request_id: &str,
    session_id: &str,
    command: &str,
    workdir: Option<PathBuf>,
    env: Option<HashMap<String, String>>,
    timeout_ms: Option<u64>,
    ctx: &AppContext,
) -> Response {
    if !ctx.config().experimental_bash_background {
        return Response::error(
            request_id,
            "feature_disabled",
            "background bash is disabled; set experimental_bash_background=true",
        );
    }

    let workdir = workdir.unwrap_or_else(|| {
        ctx.config().project_root.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        })
    });
    let storage_dir = ctx.config().storage_dir.clone();
    let max_running = ctx.config().max_background_bash_tasks;
    let timeout = timeout_ms.map(Duration::from_millis);

    match ctx.bash_background().spawn(
        command,
        session_id.to_string(),
        workdir,
        env.unwrap_or_default(),
        timeout,
        storage_dir,
        max_running,
    ) {
        Ok(task_id) => Response::success(
            request_id,
            json!({
                "task_id": task_id,
                "status": BgTaskStatus::Running,
            }),
        ),
        Err(message) if message.contains("limit exceeded") => {
            Response::error(request_id, "background_task_limit_exceeded", message)
        }
        Err(message) => Response::error(request_id, "execution_failed", message),
    }
}
