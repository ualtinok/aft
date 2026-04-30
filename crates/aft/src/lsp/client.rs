use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, RecvTimeoutError, Sender};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::lsp::jsonrpc::{
    Notification, Request, RequestId, Response as JsonRpcResponse, ServerMessage,
};
use crate::lsp::registry::ServerKind;
use crate::lsp::{transport, LspError};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(25);

type PendingMap = HashMap<RequestId, Sender<JsonRpcResponse>>;

/// Lifecycle state of a language server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Starting,
    Initializing,
    Ready,
    ShuttingDown,
    Exited,
}

/// Events sent from background reader threads into the main loop.
#[derive(Debug)]
pub enum LspEvent {
    /// Server sent a notification (e.g. publishDiagnostics).
    Notification {
        server_kind: ServerKind,
        root: PathBuf,
        method: String,
        params: Option<Value>,
    },
    /// Server sent a request (e.g. workspace/configuration).
    ServerRequest {
        server_kind: ServerKind,
        root: PathBuf,
        id: RequestId,
        method: String,
        params: Option<Value>,
    },
    /// Server process exited or the transport stream closed.
    ServerExited {
        server_kind: ServerKind,
        root: PathBuf,
    },
}

/// What this server told us it can do during the LSP `initialize` handshake.
///
/// We capture this once and use it to route diagnostic requests:
/// - `pull_diagnostics` → use `textDocument/diagnostic` instead of waiting for push
/// - `workspace_diagnostics` → use `workspace/diagnostic` for directory mode
///
/// Defaults are conservative: `false` means "fall back to push semantics".
#[derive(Debug, Clone, Default)]
pub struct ServerDiagnosticCapabilities {
    /// Server supports `textDocument/diagnostic` (LSP 3.17 per-file pull).
    pub pull_diagnostics: bool,
    /// Server supports `workspace/diagnostic` (LSP 3.17 workspace-wide pull).
    pub workspace_diagnostics: bool,
    /// `identifier` field from server's diagnosticProvider, if any.
    /// Used to scope previousResultId tracking when multiple servers share a file.
    pub identifier: Option<String>,
    /// Whether the server requested workspace diagnostic refresh notifications.
    /// We declare `refreshSupport: false` in our client capabilities so this
    /// should always be false in practice — kept for completeness.
    pub refresh_support: bool,
}

/// A client connected to one language server process.
pub struct LspClient {
    kind: ServerKind,
    root: PathBuf,
    state: ServerState,
    child: Child,
    writer: Arc<Mutex<BufWriter<std::process::ChildStdin>>>,

    /// Pending request responses, keyed by request ID.
    pending: Arc<Mutex<PendingMap>>,
    /// Next request ID counter.
    next_id: AtomicI64,
    /// Diagnostic capabilities reported by the server in its initialize response.
    /// `None` until `initialize()` succeeds; conservative defaults thereafter
    /// when the server doesn't advertise diagnosticProvider.
    diagnostic_caps: Option<ServerDiagnosticCapabilities>,
    /// Whether the server advertised `workspace.didChangeWatchedFiles` support
    /// during `initialize`. When `false` (or `None` pre-init), we skip sending
    /// `workspace/didChangeWatchedFiles` notifications to avoid spec violations.
    /// Intentional default: `false` (conservative — requires server opt-in).
    supports_watched_files: bool,
}

impl LspClient {
    /// Spawn a new language server process and start the background reader thread.
    pub fn spawn(
        kind: ServerKind,
        root: PathBuf,
        binary: &Path,
        args: &[String],
        env: &HashMap<String, String>,
        event_tx: Sender<LspEvent>,
    ) -> io::Result<Self> {
        let mut command = Command::new(binary);
        command
            .args(args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Use null() instead of piped() to prevent deadlock when the server
            // writes more than ~64KB to stderr (piped buffer fills, server blocks)
            .stderr(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }

        let mut child = command.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("language server missing stdout pipe"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("language server missing stdin pipe"))?;

        let writer = Arc::new(Mutex::new(BufWriter::new(stdin)));
        let pending = Arc::new(Mutex::new(PendingMap::new()));
        let reader_pending = Arc::clone(&pending);
        let reader_writer = Arc::clone(&writer);
        let reader_kind = kind.clone();
        let reader_root = root.clone();

        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match transport::read_message(&mut reader) {
                    Ok(Some(ServerMessage::Response(response))) => {
                        if let Ok(mut guard) = reader_pending.lock() {
                            if let Some(tx) = guard.remove(&response.id) {
                                if tx.send(response).is_err() {
                                    log::debug!("[aft-lsp] response channel closed");
                                }
                            }
                        } else {
                            let _ = event_tx.send(LspEvent::ServerExited {
                                server_kind: reader_kind.clone(),
                                root: reader_root.clone(),
                            });
                            break;
                        }
                    }
                    Ok(Some(ServerMessage::Notification { method, params })) => {
                        let _ = event_tx.send(LspEvent::Notification {
                            server_kind: reader_kind.clone(),
                            root: reader_root.clone(),
                            method,
                            params,
                        });
                    }
                    Ok(Some(ServerMessage::Request { id, method, params })) => {
                        // Auto-respond to server requests to prevent deadlocks.
                        // Server requests (like client/registerCapability,
                        // window/workDoneProgress/create) block the server until
                        // we respond. If we don't respond, the server won't send
                        // responses to OUR pending requests → deadlock.
                        //
                        // Dispatch by method to return correct types:
                        // - workspace/configuration expects Vec<Value> (one per item)
                        // - Everything else gets null (safe default for registration/progress)
                        let response_value = if method == "workspace/configuration" {
                            // Return an array of null configs — one per requested item.
                            // Servers fall back to filesystem config (tsconfig, pyrightconfig, etc.)
                            let item_count = params
                                .as_ref()
                                .and_then(|p| p.get("items"))
                                .and_then(|items| items.as_array())
                                .map_or(1, |arr| arr.len());
                            serde_json::Value::Array(vec![serde_json::Value::Null; item_count])
                        } else {
                            serde_json::Value::Null
                        };
                        if let Ok(mut w) = reader_writer.lock() {
                            let response = super::jsonrpc::OutgoingResponse::success(
                                id.clone(),
                                response_value,
                            );
                            let _ = transport::write_response(&mut *w, &response);
                        }
                        // Also forward as event for any interested handlers
                        let _ = event_tx.send(LspEvent::ServerRequest {
                            server_kind: reader_kind.clone(),
                            root: reader_root.clone(),
                            id,
                            method,
                            params,
                        });
                    }
                    Ok(None) | Err(_) => {
                        if let Ok(mut guard) = reader_pending.lock() {
                            guard.clear();
                        }
                        let _ = event_tx.send(LspEvent::ServerExited {
                            server_kind: reader_kind.clone(),
                            root: reader_root.clone(),
                        });
                        break;
                    }
                }
            }
        });

        Ok(Self {
            kind,
            root,
            state: ServerState::Starting,
            child,
            writer,
            pending,
            next_id: AtomicI64::new(1),
            diagnostic_caps: None,
            supports_watched_files: false,
        })
    }

    /// Send the initialize request and wait for response. Transition to Ready.
    pub fn initialize(
        &mut self,
        workspace_root: &Path,
        initialization_options: Option<serde_json::Value>,
    ) -> Result<lsp_types::InitializeResult, LspError> {
        self.ensure_can_send()?;
        self.state = ServerState::Initializing;

        let normalized = normalize_windows_path(workspace_root);
        let root_url = url::Url::from_file_path(&normalized).map_err(|_| {
            LspError::NotFound(format!(
                "failed to convert workspace root '{}' to file URI",
                workspace_root.display()
            ))
        })?;
        let root_uri = lsp_types::Uri::from_str(root_url.as_str()).map_err(|_| {
            LspError::NotFound(format!(
                "failed to convert workspace root '{}' to file URI",
                workspace_root.display()
            ))
        })?;

        let mut params_value = json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true,
                    // LSP 3.17 workspace diagnostic pull. We declare refreshSupport=false
                    // because we drive diagnostics on-demand via pull/push and re-query
                    // when the agent calls lsp_diagnostics again — we don't need the
                    // server to proactively push refresh notifications.
                    "diagnostic": {
                        "refreshSupport": false
                    }
                },
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        "didSave": true,
                        "willSave": false,
                        "willSaveWaitUntil": false
                    },
                    "publishDiagnostics": {
                        "relatedInformation": true,
                        "versionSupport": true,
                        "codeDescriptionSupport": true,
                        "dataSupport": true
                    },
                    // LSP 3.17 textDocument diagnostic pull. dynamicRegistration=false
                    // because we use static capability discovery from the InitializeResult.
                    // relatedDocumentSupport=true to receive cascading diagnostics for
                    // files that became known while analyzing the requested one.
                    "diagnostic": {
                        "dynamicRegistration": false,
                        "relatedDocumentSupport": true
                    }
                }
            },
            "clientInfo": {
                "name": "aft",
                "version": env!("CARGO_PKG_VERSION")
            },
            "workspaceFolders": [
                {
                    "uri": root_uri,
                    "name": workspace_root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("workspace")
                }
            ]
        });
        if let Some(initialization_options) = initialization_options {
            params_value["initializationOptions"] = initialization_options;
        }

        let params = serde_json::from_value::<lsp_types::InitializeParams>(params_value)?;

        let result = self.send_request::<lsp_types::request::Initialize>(params)?;

        // Capture diagnostic capabilities from the initialize response. We parse
        // from a re-serialized JSON Value because the lsp-types crate's
        // diagnostic_provider strict variants reject some shapes real servers
        // emit (e.g. bare `true`), and we want defensive Default fallback.
        let caps_value = serde_json::to_value(&result.capabilities).unwrap_or(Value::Null);
        self.diagnostic_caps = Some(parse_diagnostic_capabilities(&caps_value));

        // Capture whether the server supports workspace/didChangeWatchedFiles (#32).
        //
        // IMPORTANT: lsp-types 0.97's WorkspaceServerCapabilities struct does NOT
        // include a `didChangeWatchedFiles` field, so `caps_value` will never have
        // it after re-serialization. We therefore default to `true` (permissive).
        //
        // Per the LSP specification, servers MUST ignore notifications for methods
        // they don't support, so sending didChangeWatchedFiles unconditionally is
        // spec-safe. The default-true matches the pre-#32 unconditional behavior
        // and avoids a regression for servers that do support it (tsserver, rust-
        // analyzer, pyright all accept it even without explicit advertising).
        //
        // If a future lsp-types version exposes the field, the pointer lookup
        // below will start returning real values and the default won't matter.
        self.supports_watched_files = caps_value
            .pointer("/workspace/didChangeWatchedFiles/dynamicRegistration")
            .and_then(|v| v.as_bool())
            .unwrap_or(true) // permissive default: spec-safe to send if server doesn't say false
            || caps_value
                .pointer("/workspace/didChangeWatchedFiles")
                .map(|v| v.is_object() || v.as_bool() == Some(true))
                .unwrap_or(true);

        self.send_notification::<lsp_types::notification::Initialized>(serde_json::from_value(
            json!({}),
        )?)?;
        self.state = ServerState::Ready;
        Ok(result)
    }

    /// Diagnostic capabilities advertised by the server. Returns `None` until
    /// `initialize()` has succeeded; returns `Some` with conservative defaults
    /// (all `false`) when the server didn't advertise diagnosticProvider.
    pub fn diagnostic_capabilities(&self) -> Option<&ServerDiagnosticCapabilities> {
        self.diagnostic_caps.as_ref()
    }

    /// Whether the server supports `workspace/didChangeWatchedFiles`.
    /// Captured from the `initialize` response. Default `false` (conservative).
    pub fn supports_watched_files(&self) -> bool {
        self.supports_watched_files
    }

    /// Send a request and wait for the response.
    pub fn send_request<R>(&mut self, params: R::Params) -> Result<R::Result, LspError>
    where
        R: lsp_types::request::Request,
        R::Params: serde::Serialize,
        R::Result: DeserializeOwned,
    {
        self.ensure_can_send()?;

        let id = RequestId::Int(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = bounded(1);
        {
            let mut pending = self.lock_pending()?;
            pending.insert(id.clone(), tx);
        }

        let request = Request::new(id.clone(), R::METHOD, Some(serde_json::to_value(params)?));
        {
            let mut writer = self
                .writer
                .lock()
                .map_err(|_| LspError::ServerNotReady("writer lock poisoned".to_string()))?;
            if let Err(err) = transport::write_request(&mut *writer, &request) {
                self.remove_pending(&id);
                return Err(err.into());
            }
        }

        let response = match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(response) => response,
            Err(RecvTimeoutError::Timeout) => {
                self.remove_pending(&id);
                return Err(LspError::Timeout(format!(
                    "timed out waiting for '{}' response from {:?}",
                    R::METHOD,
                    self.kind
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.remove_pending(&id);
                return Err(LspError::ServerNotReady(format!(
                    "language server {:?} disconnected while waiting for '{}'",
                    self.kind,
                    R::METHOD
                )));
            }
        };

        if let Some(error) = response.error {
            return Err(LspError::ServerError {
                code: error.code,
                message: error.message,
            });
        }

        serde_json::from_value(response.result.unwrap_or(Value::Null)).map_err(Into::into)
    }

    /// Send a notification (fire-and-forget).
    pub fn send_notification<N>(&mut self, params: N::Params) -> Result<(), LspError>
    where
        N: lsp_types::notification::Notification,
        N::Params: serde::Serialize,
    {
        self.ensure_can_send()?;
        let notification = Notification::new(N::METHOD, Some(serde_json::to_value(params)?));
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| LspError::ServerNotReady("writer lock poisoned".to_string()))?;
        transport::write_notification(&mut *writer, &notification)?;
        Ok(())
    }

    /// Graceful shutdown: send shutdown request, then exit notification.
    pub fn shutdown(&mut self) -> Result<(), LspError> {
        if self.state == ServerState::Exited {
            return Ok(());
        }

        if self.child.try_wait()?.is_some() {
            self.state = ServerState::Exited;
            return Ok(());
        }

        if let Err(err) = self.send_request::<lsp_types::request::Shutdown>(()) {
            self.state = ServerState::ShuttingDown;
            if self.child.try_wait()?.is_some() {
                self.state = ServerState::Exited;
                return Ok(());
            }
            return Err(err);
        }

        self.state = ServerState::ShuttingDown;

        if let Err(err) = self.send_notification::<lsp_types::notification::Exit>(()) {
            if self.child.try_wait()?.is_some() {
                self.state = ServerState::Exited;
                return Ok(());
            }
            return Err(err);
        }

        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            if self.child.try_wait()?.is_some() {
                self.state = ServerState::Exited;
                return Ok(());
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.state = ServerState::Exited;
                return Err(LspError::Timeout(format!(
                    "timed out waiting for {:?} to exit",
                    self.kind
                )));
            }
            thread::sleep(EXIT_POLL_INTERVAL);
        }
    }

    pub fn state(&self) -> ServerState {
        self.state
    }

    pub fn kind(&self) -> ServerKind {
        self.kind.clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn ensure_can_send(&self) -> Result<(), LspError> {
        if matches!(self.state, ServerState::ShuttingDown | ServerState::Exited) {
            return Err(LspError::ServerNotReady(format!(
                "language server {:?} is not ready (state: {:?})",
                self.kind, self.state
            )));
        }
        Ok(())
    }

    fn lock_pending(&self) -> Result<std::sync::MutexGuard<'_, PendingMap>, LspError> {
        self.pending
            .lock()
            .map_err(|_| io::Error::other("pending response map poisoned").into())
    }

    fn remove_pending(&self, id: &RequestId) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(id);
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Normalize a path for file URI conversion.
/// On Windows, strips the extended-length `\\?\` prefix that `Url::from_file_path` cannot handle.
/// On other platforms, returns the path unchanged.
fn normalize_windows_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

/// Parse `ServerDiagnosticCapabilities` from a re-serialized
/// `ServerCapabilities` JSON value.
///
/// LSP 3.17 spec for `diagnosticProvider`:
/// - `capabilities.diagnosticProvider` may be absent (no pull support),
///   `DiagnosticOptions`, or `DiagnosticRegistrationOptions`.
/// - If present:
///   - `interFileDependencies: bool` (we don't currently use this)
///   - `workspaceDiagnostics: bool` → workspace pull support
///   - `identifier?: string` → optional identifier scoping result IDs
///
/// We parse the raw JSON Value defensively: presence of any
/// `diagnosticProvider` value (object or `true`) means the server supports
/// at least `textDocument/diagnostic` pull.
fn parse_diagnostic_capabilities(value: &Value) -> ServerDiagnosticCapabilities {
    let mut caps = ServerDiagnosticCapabilities::default();

    if let Some(provider) = value.get("diagnosticProvider") {
        // diagnosticProvider can be `true` (rare) or an object. Treat both as
        // pull_diagnostics support.
        if provider.is_object() || provider.as_bool() == Some(true) {
            caps.pull_diagnostics = true;
        }

        if let Some(obj) = provider.as_object() {
            if obj
                .get("workspaceDiagnostics")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                caps.workspace_diagnostics = true;
            }
            if let Some(identifier) = obj.get("identifier").and_then(|v| v.as_str()) {
                caps.identifier = Some(identifier.to_string());
            }
        }
    }

    // Workspace diagnostic refresh (rare — most servers don't request this,
    // and we declared refreshSupport=false in our client capabilities anyway).
    if let Some(refresh) = value
        .get("workspace")
        .and_then(|w| w.get("diagnostic"))
        .and_then(|d| d.get("refreshSupport"))
        .and_then(|r| r.as_bool())
    {
        caps.refresh_support = refresh;
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_caps_no_diagnostic_provider() {
        let value = json!({});
        let caps = parse_diagnostic_capabilities(&value);
        assert!(!caps.pull_diagnostics);
        assert!(!caps.workspace_diagnostics);
        assert!(caps.identifier.is_none());
    }

    #[test]
    fn parse_caps_basic_pull_only() {
        let value = json!({
            "diagnosticProvider": {
                "interFileDependencies": false,
                "workspaceDiagnostics": false
            }
        });
        let caps = parse_diagnostic_capabilities(&value);
        assert!(caps.pull_diagnostics);
        assert!(!caps.workspace_diagnostics);
    }

    #[test]
    fn parse_caps_full_pull_with_workspace() {
        let value = json!({
            "diagnosticProvider": {
                "interFileDependencies": true,
                "workspaceDiagnostics": true,
                "identifier": "rust-analyzer"
            }
        });
        let caps = parse_diagnostic_capabilities(&value);
        assert!(caps.pull_diagnostics);
        assert!(caps.workspace_diagnostics);
        assert_eq!(caps.identifier.as_deref(), Some("rust-analyzer"));
    }

    #[test]
    fn parse_caps_provider_as_bare_true() {
        // LSP 3.17 allows DiagnosticOptions OR boolean — treat true as pull_diagnostics
        let value = json!({
            "diagnosticProvider": true
        });
        let caps = parse_diagnostic_capabilities(&value);
        assert!(caps.pull_diagnostics);
        assert!(!caps.workspace_diagnostics);
    }

    #[test]
    fn parse_caps_workspace_refresh_support() {
        let value = json!({
            "workspace": {
                "diagnostic": {
                    "refreshSupport": true
                }
            }
        });
        let caps = parse_diagnostic_capabilities(&value);
        assert!(caps.refresh_support);
        // No diagnosticProvider → pull still false
        assert!(!caps.pull_diagnostics);
    }
}
