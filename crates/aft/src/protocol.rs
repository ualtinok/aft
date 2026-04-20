use serde::{Deserialize, Serialize};

/// Fallback session identifier used when a request arrives without one.
///
/// Introduced alongside project-shared bridges (issue #14): one `aft` process
/// can now serve many OpenCode sessions in the same project. Undo/checkpoint
/// state is partitioned by session inside Rust, but callers that haven't been
/// updated to pass `session_id` (older plugins, direct CLI usage, tests) still
/// need to work — they share this default namespace.
///
/// Also used as the migration target for legacy pre-session backups on disk.
pub const DEFAULT_SESSION_ID: &str = "__default__";

/// Inbound request envelope.
///
/// Two-stage parse: deserialize this first to get `id` + `command`, then
/// dispatch on `command` and pull specific params from the flattened `params`.
#[derive(Debug, Deserialize)]
pub struct RawRequest {
    pub id: String,
    pub command: String,
    /// Optional LSP hints from the plugin (R031 forward compatibility).
    #[serde(default)]
    pub lsp_hints: Option<serde_json::Value>,
    /// Optional session namespace for undo/checkpoint isolation.
    ///
    /// When the plugin passes `session_id`, Rust partitions backup/checkpoint
    /// state by it so concurrent OpenCode sessions sharing one bridge can't
    /// see or restore each other's snapshots. When absent, falls back to
    /// [`DEFAULT_SESSION_ID`].
    #[serde(default)]
    pub session_id: Option<String>,
    /// All remaining fields are captured here for per-command deserialization.
    #[serde(flatten)]
    pub params: serde_json::Value,
}

impl RawRequest {
    /// Session namespace for this request, falling back to [`DEFAULT_SESSION_ID`]
    /// when the plugin didn't supply one.
    pub fn session(&self) -> &str {
        self.session_id.as_deref().unwrap_or(DEFAULT_SESSION_ID)
    }
}

/// Outbound response envelope.
///
/// `data` is flattened into the top-level JSON object, so a response like
/// `Response { id: "1", success: true, data: json!({"command": "pong"}) }`
/// serializes to `{"id":"1","success":true,"command":"pong"}`.
#[derive(Debug, Serialize)]
pub struct Response {
    pub id: String,
    pub success: bool,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Parameters for the `echo` command.
#[derive(Debug, Deserialize)]
pub struct EchoParams {
    pub message: String,
}

impl Response {
    /// Build a success response with arbitrary data merged at the top level.
    pub fn success(id: impl Into<String>, data: serde_json::Value) -> Self {
        Response {
            id: id.into(),
            success: true,
            data,
        }
    }

    /// Build an error response with `code` and `message` fields.
    pub fn error(id: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Response {
            id: id.into(),
            success: false,
            data: serde_json::json!({
                "code": code,
                "message": message.into(),
            }),
        }
    }

    /// Build an error response with `code`, `message`, and additional structured data.
    ///
    /// The `extra` fields are merged into the top-level response alongside `code` and `message`.
    pub fn error_with_data(
        id: impl Into<String>,
        code: &str,
        message: impl Into<String>,
        extra: serde_json::Value,
    ) -> Self {
        let mut data = serde_json::json!({
            "code": code,
            "message": message.into(),
        });
        if let (Some(base), Some(ext)) = (data.as_object_mut(), extra.as_object()) {
            for (k, v) in ext {
                base.insert(k.clone(), v.clone());
            }
        }
        Response {
            id: id.into(),
            success: false,
            data,
        }
    }
}
