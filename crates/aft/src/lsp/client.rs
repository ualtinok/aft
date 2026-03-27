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
}

impl LspClient {
    /// Spawn a new language server process and start the background reader thread.
    pub fn spawn(
        kind: ServerKind,
        root: PathBuf,
        binary: &Path,
        args: &[&str],
        event_tx: Sender<LspEvent>,
    ) -> io::Result<Self> {
        let mut child = Command::new(binary)
            .args(args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Use null() instead of piped() to prevent deadlock when the server
            // writes more than ~64KB to stderr (piped buffer fills, server blocks)
            .stderr(Stdio::null())
            .spawn()?;

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
        let reader_kind = kind;
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
                                server_kind: reader_kind,
                                root: reader_root.clone(),
                            });
                            break;
                        }
                    }
                    Ok(Some(ServerMessage::Notification { method, params })) => {
                        let _ = event_tx.send(LspEvent::Notification {
                            server_kind: reader_kind,
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
                        if let Ok(mut w) = reader_writer.lock() {
                            let response = super::jsonrpc::OutgoingResponse::success(
                                id.clone(),
                                serde_json::Value::Null,
                            );
                            let _ = transport::write_response(&mut *w, &response);
                        }
                        // Also forward as event for any interested handlers
                        let _ = event_tx.send(LspEvent::ServerRequest {
                            server_kind: reader_kind,
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
                            server_kind: reader_kind,
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
        })
    }

    /// Send the initialize request and wait for response. Transition to Ready.
    pub fn initialize(
        &mut self,
        workspace_root: &Path,
    ) -> Result<lsp_types::InitializeResult, LspError> {
        self.ensure_can_send()?;
        self.state = ServerState::Initializing;

        let root_uri = lsp_types::Uri::from_str(&format!("file://{}", workspace_root.display()))
            .map_err(|_| {
                LspError::NotFound(format!(
                    "failed to convert workspace root '{}' to file URI",
                    workspace_root.display()
                ))
            })?;

        let params = serde_json::from_value::<lsp_types::InitializeParams>(json!({
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
                "workspace": {
                    "workspaceFolders": true,
                    "configuration": true
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
        }))?;

        let result = self.send_request::<lsp_types::request::Initialize>(params)?;
        self.send_notification::<lsp_types::notification::Initialized>(serde_json::from_value(
            json!({}),
        )?)?;
        self.state = ServerState::Ready;
        Ok(result)
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
        self.kind
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
