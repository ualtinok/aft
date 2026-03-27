use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use lsp_types::notification::{DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    VersionedTextDocumentIdentifier,
};

use crate::lsp::client::{LspClient, LspEvent, ServerState};
use crate::lsp::diagnostics::{from_lsp_diagnostics, DiagnosticsStore, StoredDiagnostic};
use crate::lsp::document::DocumentStore;
use crate::lsp::registry::{servers_for_file, ServerDef, ServerKind};
use crate::lsp::roots::{find_workspace_root, ServerKey};
use crate::lsp::LspError;

pub struct LspManager {
    /// Active server instances, keyed by (ServerKind, workspace_root).
    clients: HashMap<ServerKey, LspClient>,
    /// Tracks opened documents and versions per active server.
    documents: HashMap<ServerKey, DocumentStore>,
    /// Stored publishDiagnostics payloads across all servers.
    diagnostics: DiagnosticsStore,
    /// Unified event channel — all server reader threads send here.
    event_tx: Sender<LspEvent>,
    event_rx: Receiver<LspEvent>,
    /// Optional binary path overrides used by integration tests.
    binary_overrides: HashMap<ServerKind, PathBuf>,
}

impl LspManager {
    pub fn new() -> Self {
        let (event_tx, event_rx) = unbounded();
        Self {
            clients: HashMap::new(),
            documents: HashMap::new(),
            diagnostics: DiagnosticsStore::new(),
            event_tx,
            event_rx,
            binary_overrides: HashMap::new(),
        }
    }

    /// For testing: override the binary for a server kind.
    pub fn override_binary(&mut self, kind: ServerKind, binary_path: PathBuf) {
        self.binary_overrides.insert(kind, binary_path);
    }

    /// Ensure a server is running for the given file. Spawns if needed.
    /// Returns the active server keys for the file, or an empty vec if none match.
    pub fn ensure_server_for_file(&mut self, file_path: &Path) -> Vec<ServerKey> {
        let defs = servers_for_file(file_path);
        let mut keys = Vec::new();

        for def in defs {
            let Some(root) = find_workspace_root(file_path, def.root_markers) else {
                continue;
            };

            let key = ServerKey {
                kind: def.kind,
                root,
            };

            if !self.clients.contains_key(&key) {
                match self.spawn_server(def, &key.root) {
                    Ok(client) => {
                        self.clients.insert(key.clone(), client);
                        self.documents.entry(key.clone()).or_default();
                    }
                    Err(err) => {
                        log::error!("failed to spawn {}: {}", def.name, err);
                        continue;
                    }
                }
            }

            keys.push(key);
        }

        keys
    }
    /// Ensure that servers are running for the file and that the document is open
    /// in each server's DocumentStore. Reads file content from disk if not already open.
    /// Returns the server keys for the file.
    pub fn ensure_file_open(&mut self, file_path: &Path) -> Result<Vec<ServerKey>, LspError> {
        let canonical_path = canonicalize_for_lsp(file_path)?;
        let server_keys = self.ensure_server_for_file(&canonical_path);
        if server_keys.is_empty() {
            return Ok(server_keys);
        }

        let uri = uri_for_path(&canonical_path)?;
        let language_id = language_id_for_extension(
            canonical_path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default(),
        )
        .to_string();

        for key in &server_keys {
            let already_open = self
                .documents
                .get(key)
                .map_or(false, |store| store.is_open(&canonical_path));

            if !already_open {
                let content = std::fs::read_to_string(&canonical_path).map_err(LspError::Io)?;
                if let Some(client) = self.clients.get_mut(key) {
                    client.send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                        text_document: TextDocumentItem::new(
                            uri.clone(),
                            language_id.clone(),
                            0,
                            content,
                        ),
                    })?;
                }
                self.documents
                    .entry(key.clone())
                    .or_default()
                    .open(canonical_path.clone());
            }
        }

        Ok(server_keys)
    }

    /// Notify relevant LSP servers that a file has been written/changed.
    /// This is the main hook called after every file write in AFT.
    ///
    /// If the file's server isn't running yet, starts it (lazy spawn).
    /// If the file isn't open in LSP yet, sends didOpen. Otherwise sends didChange.
    pub fn notify_file_changed(&mut self, file_path: &Path, content: &str) -> Result<(), LspError> {
        let canonical_path = canonicalize_for_lsp(file_path)?;
        let server_keys = self.ensure_server_for_file(&canonical_path);
        if server_keys.is_empty() {
            return Ok(());
        }

        let uri = uri_for_path(&canonical_path)?;
        let language_id = language_id_for_extension(
            canonical_path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default(),
        )
        .to_string();

        for key in server_keys {
            let current_version = self
                .documents
                .get(&key)
                .and_then(|store| store.version(&canonical_path));

            if let Some(version) = current_version {
                let next_version = version + 1;
                if let Some(client) = self.clients.get_mut(&key) {
                    client.send_notification::<DidChangeTextDocument>(
                        DidChangeTextDocumentParams {
                            text_document: VersionedTextDocumentIdentifier::new(
                                uri.clone(),
                                next_version,
                            ),
                            content_changes: vec![TextDocumentContentChangeEvent {
                                range: None,
                                range_length: None,
                                text: content.to_string(),
                            }],
                        },
                    )?;
                }
                if let Some(store) = self.documents.get_mut(&key) {
                    store.bump_version(&canonical_path);
                }
                continue;
            }

            if let Some(client) = self.clients.get_mut(&key) {
                client.send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
                    text_document: TextDocumentItem::new(
                        uri.clone(),
                        language_id.clone(),
                        0,
                        content.to_string(),
                    ),
                })?;
            }
            self.documents
                .entry(key)
                .or_default()
                .open(canonical_path.clone());
        }

        Ok(())
    }

    /// Close a document in all servers that have it open.
    pub fn notify_file_closed(&mut self, file_path: &Path) -> Result<(), LspError> {
        let canonical_path = canonicalize_for_lsp(file_path)?;
        let uri = uri_for_path(&canonical_path)?;
        let keys: Vec<ServerKey> = self.documents.keys().cloned().collect();

        for key in keys {
            let was_open = self
                .documents
                .get(&key)
                .map(|store| store.is_open(&canonical_path))
                .unwrap_or(false);
            if !was_open {
                continue;
            }

            if let Some(client) = self.clients.get_mut(&key) {
                client.send_notification::<DidCloseTextDocument>(DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier::new(uri.clone()),
                })?;
            }

            if let Some(store) = self.documents.get_mut(&key) {
                store.close(&canonical_path);
            }
        }

        Ok(())
    }

    /// Get an active client for a file path, if one exists.
    pub fn client_for_file(&self, file_path: &Path) -> Option<&LspClient> {
        let key = self.server_key_for_file(file_path)?;
        self.clients.get(&key)
    }

    /// Get a mutable active client for a file path, if one exists.
    pub fn client_for_file_mut(&mut self, file_path: &Path) -> Option<&mut LspClient> {
        let key = self.server_key_for_file(file_path)?;
        self.clients.get_mut(&key)
    }

    /// Number of tracked server clients.
    pub fn active_client_count(&self) -> usize {
        self.clients.len()
    }

    /// Drain all pending LSP events. Call from the main loop.
    pub fn drain_events(&mut self) -> Vec<LspEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            self.handle_event(&event);
            events.push(event);
        }
        events
    }

    /// Wait for diagnostics to arrive for a specific file until a timeout expires.
    pub fn wait_for_diagnostics(
        &mut self,
        file_path: &Path,
        timeout: std::time::Duration,
    ) -> Vec<StoredDiagnostic> {
        let deadline = std::time::Instant::now() + timeout;
        self.wait_for_file_diagnostics(file_path, deadline)
    }

    /// Wait for diagnostics to arrive for a specific file until a deadline.
    ///
    /// Drains already-queued events first, then blocks on the shared event
    /// channel only until either `publishDiagnostics` arrives for this file or
    /// the deadline is reached.
    pub fn wait_for_file_diagnostics(
        &mut self,
        file_path: &Path,
        deadline: std::time::Instant,
    ) -> Vec<StoredDiagnostic> {
        let lookup_path = normalize_lookup_path(file_path);

        if self.server_key_for_file(&lookup_path).is_none() {
            return Vec::new();
        }

        loop {
            if self.drain_events_for_file(&lookup_path) {
                break;
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }

            let timeout = deadline.saturating_duration_since(now);
            match self.event_rx.recv_timeout(timeout) {
                Ok(event) => {
                    if matches!(
                        self.handle_event(&event),
                        Some(ref published_file) if published_file.as_path() == lookup_path.as_path()
                    ) {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        self.get_diagnostics_for_file(&lookup_path)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Shutdown all servers gracefully.
    pub fn shutdown_all(&mut self) {
        for (key, mut client) in self.clients.drain() {
            if let Err(err) = client.shutdown() {
                log::error!("error shutting down {:?}: {}", key, err);
            }
        }
        self.documents.clear();
        self.diagnostics = DiagnosticsStore::new();
    }

    /// Check if any server is active.
    pub fn has_active_servers(&self) -> bool {
        self.clients
            .values()
            .any(|client| client.state() == ServerState::Ready)
    }

    pub fn get_diagnostics_for_file(&self, file: &Path) -> Vec<&StoredDiagnostic> {
        let normalized = normalize_lookup_path(file);
        self.diagnostics.for_file(&normalized)
    }

    pub fn get_diagnostics_for_directory(&self, dir: &Path) -> Vec<&StoredDiagnostic> {
        let normalized = normalize_lookup_path(dir);
        self.diagnostics.for_directory(&normalized)
    }

    pub fn get_all_diagnostics(&self) -> Vec<&StoredDiagnostic> {
        self.diagnostics.all()
    }

    fn drain_events_for_file(&mut self, file_path: &Path) -> bool {
        let mut saw_file_diagnostics = false;
        while let Ok(event) = self.event_rx.try_recv() {
            if matches!(
                self.handle_event(&event),
                Some(ref published_file) if published_file.as_path() == file_path
            ) {
                saw_file_diagnostics = true;
            }
        }
        saw_file_diagnostics
    }

    fn handle_event(&mut self, event: &LspEvent) -> Option<PathBuf> {
        match event {
            LspEvent::Notification {
                server_kind,
                method,
                params: Some(params),
                ..
            } if method == "textDocument/publishDiagnostics" => {
                self.handle_publish_diagnostics(*server_kind, params)
            }
            LspEvent::ServerExited { server_kind, root } => {
                let key = ServerKey {
                    kind: *server_kind,
                    root: root.clone(),
                };
                self.clients.remove(&key);
                self.documents.remove(&key);
                self.diagnostics.clear_server(*server_kind);
                None
            }
            _ => None,
        }
    }

    fn handle_publish_diagnostics(
        &mut self,
        server: ServerKind,
        params: &serde_json::Value,
    ) -> Option<PathBuf> {
        if let Ok(publish_params) =
            serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params.clone())
        {
            let Some(file) = uri_to_path(&publish_params.uri) else {
                return None;
            };
            let stored = from_lsp_diagnostics(file.clone(), publish_params.diagnostics);
            self.diagnostics.publish(server, file, stored);
            return Some(uri_to_path(&publish_params.uri)?);
        }
        None
    }

    fn spawn_server(&self, def: &ServerDef, root: &Path) -> Result<LspClient, LspError> {
        let binary = self.resolve_binary(def)?;
        let mut client = LspClient::spawn(
            def.kind,
            root.to_path_buf(),
            &binary,
            def.args,
            self.event_tx.clone(),
        )?;
        client.initialize(root)?;
        Ok(client)
    }

    fn resolve_binary(&self, def: &ServerDef) -> Result<PathBuf, LspError> {
        if let Some(path) = self.binary_overrides.get(&def.kind) {
            if path.exists() {
                return Ok(path.clone());
            }
            return Err(LspError::NotFound(format!(
                "override binary for {:?} not found: {}",
                def.kind,
                path.display()
            )));
        }

        if let Some(path) = env_binary_override(def.kind) {
            if path.exists() {
                return Ok(path);
            }
            return Err(LspError::NotFound(format!(
                "environment override binary for {:?} not found: {}",
                def.kind,
                path.display()
            )));
        }

        which::which(def.binary).map_err(|_| {
            LspError::NotFound(format!(
                "language server binary '{}' not found on PATH",
                def.binary
            ))
        })
    }

    fn server_key_for_file(&self, file_path: &Path) -> Option<ServerKey> {
        for def in servers_for_file(file_path) {
            let root = find_workspace_root(file_path, def.root_markers)?;
            let key = ServerKey {
                kind: def.kind,
                root,
            };
            if self.clients.contains_key(&key) {
                return Some(key);
            }
        }
        None
    }
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

fn canonicalize_for_lsp(file_path: &Path) -> Result<PathBuf, LspError> {
    std::fs::canonicalize(file_path).map_err(LspError::from)
}

fn uri_for_path(path: &Path) -> Result<lsp_types::Uri, LspError> {
    let url = url::Url::from_file_path(path).map_err(|_| {
        LspError::NotFound(format!(
            "failed to convert '{}' to file URI",
            path.display()
        ))
    })?;
    lsp_types::Uri::from_str(url.as_str()).map_err(|_| {
        LspError::NotFound(format!("failed to parse file URI for '{}'", path.display()))
    })
}

fn language_id_for_extension(ext: &str) -> &'static str {
    match ext {
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "rs" => "rust",
        "go" => "go",
        _ => "plaintext",
    }
}

fn normalize_lookup_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    url.to_file_path()
        .ok()
        .map(|path| normalize_lookup_path(&path))
}

fn env_binary_override(kind: ServerKind) -> Option<PathBuf> {
    let key = match kind {
        ServerKind::TypeScript => "AFT_LSP_TYPESCRIPT_BINARY",
        ServerKind::Python => "AFT_LSP_PYTHON_BINARY",
        ServerKind::Rust => "AFT_LSP_RUST_BINARY",
        ServerKind::Go => "AFT_LSP_GO_BINARY",
    };
    std::env::var_os(key).map(PathBuf::from)
}
