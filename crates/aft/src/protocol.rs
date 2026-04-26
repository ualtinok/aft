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
///
/// # Honest reporting convention (tri-state)
///
/// Tools that search, check, or otherwise produce results MUST follow this
/// convention so agents can distinguish "did the work, found nothing" from
/// "couldn't do the work" from "partially did the work":
///
/// 1. **`success: false`** — the requested work could not be performed.
///    Includes a `code` (e.g., `"path_not_found"`, `"no_lsp_server"`,
///    `"project_too_large"`) and a human-readable `message`. The agent
///    should treat this as an error and read the message.
///
/// 2. **`success: true` + completion signaling** — the work was performed.
///    Tools must report whether the result is *complete* OR which subset
///    was actually performed. Conventional fields:
///    - `complete: true` — full result, agent can trust absence of items
///    - `complete: false` + `pending_files: [...]` / `unchecked_files: [...]`
///      / `scope_warnings: [...]` — partial result, with named gaps
///    - `removed: true|false` (for mutations) — did the file actually change
///    - `skipped_files: [{file, reason}]` — files we couldn't process inside
///      the requested scope
///    - `no_files_matched_scope: bool` — the scope (path/glob) found zero
///      candidates (distinct from "candidates found, no matches")
///
/// 3. **Side-effect skip codes** — when the main work succeeded but a
///    non-essential side step was skipped (e.g., post-write formatting),
///    use a `<step>_skipped_reason` field. Approved values:
///    - `format_skipped_reason`: `"unsupported_language"` |
///      `"no_formatter_configured"` | `"formatter_not_installed"` |
///      `"timeout"` | `"error"`
///    - `validate_skipped_reason`: `"unsupported_language"` |
///      `"no_checker_configured"` | `"checker_not_installed"` |
///      `"timeout"` | `"error"`
///
/// **Anti-patterns to avoid:**
/// - Returning `success: true` with empty results when the scope didn't
///   resolve to any files — agent reads as "all clear" but really nothing
///   was checked. Use `no_files_matched_scope: true` or
///   `success: false, code: "path_not_found"`.
/// - Reusing `format_skipped_reason: "not_found"` for two different causes
///   ("no formatter configured" vs "configured formatter binary missing").
///   The agent can't act on the ambiguous code.
///
/// See ARCHITECTURE.md "Honest reporting convention" for the full rationale.
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
