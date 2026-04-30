use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
};
use lsp_types::{
    DidChangeTextDocumentParams, DidChangeWatchedFilesParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, FileChangeType, FileEvent, TextDocumentContentChangeEvent,
    TextDocumentIdentifier, TextDocumentItem, VersionedTextDocumentIdentifier,
};

use crate::config::Config;
use crate::lsp::client::{LspClient, LspEvent, ServerState};
use crate::lsp::diagnostics::{from_lsp_diagnostics, DiagnosticsStore, StoredDiagnostic};
use crate::lsp::document::DocumentStore;
use crate::lsp::registry::{resolve_lsp_binary, servers_for_file, ServerDef, ServerKind};
use crate::lsp::roots::{find_workspace_root, ServerKey};
use crate::lsp::LspError;

/// Outcome of attempting to ensure a server is running for a single matching
/// `ServerDef`. Returned per matching server so the caller can report exactly
/// what happened to the user instead of collapsing all failures into "no
/// server".
#[derive(Debug, Clone)]
pub enum ServerAttemptResult {
    /// Server is running and ready to serve requests for this file.
    Ok { server_key: ServerKey },
    /// No workspace root was found by walking up from the file looking for
    /// any of the server's configured root markers.
    NoRootMarker { looked_for: Vec<String> },
    /// The server's binary could not be found on PATH (or override was
    /// missing/invalid).
    BinaryNotInstalled { binary: String },
    /// Binary was found but spawning or initializing the server failed.
    SpawnFailed { binary: String, reason: String },
}

/// One server's attempt to handle a file.
#[derive(Debug, Clone)]
pub struct ServerAttempt {
    /// Stable server identifier (kind ID, e.g. "pyright", "rust-analyzer").
    pub server_id: String,
    /// Server display name from the registry.
    pub server_name: String,
    pub result: ServerAttemptResult,
}

/// Aggregate outcome of `ensure_server_for_file_detailed`. Distinguishes:
/// - "No server registered for this file's extension" (`attempts.is_empty()`)
/// - "Servers registered but none could start" (`successful.is_empty()` but
///   `!attempts.is_empty()`)
/// - "At least one server is ready" (`!successful.is_empty()`)
#[derive(Debug, Clone, Default)]
pub struct EnsureServerOutcomes {
    /// Server keys that are now running and ready to serve requests.
    pub successful: Vec<ServerKey>,
    /// Per-server attempt records. Empty if no server is registered for the
    /// file's extension.
    pub attempts: Vec<ServerAttempt>,
}

impl EnsureServerOutcomes {
    /// True if no server in the registry matched this file's extension.
    pub fn no_server_registered(&self) -> bool {
        self.attempts.is_empty()
    }
}

/// Outcome of a post-edit diagnostics wait. Reports the per-server status
/// alongside the fresh diagnostics, so the response layer can build an
/// honest tri-state payload (`success: true` + `complete: bool` + named
/// gap fields per `crates/aft/src/protocol.rs`).
///
/// `diagnostics` only contains entries from servers that proved freshness
/// (version-match preferred, epoch-fallback for unversioned servers).
/// Pre-edit cached entries are NEVER included — that's the whole point of
/// this type.
#[derive(Debug, Clone, Default)]
pub struct PostEditWaitOutcome {
    /// Diagnostics from servers whose response we verified is FOR the
    /// post-edit document version (or whose epoch we saw advance after our
    /// pre-edit snapshot, for unversioned servers).
    pub diagnostics: Vec<StoredDiagnostic>,
    /// Servers we expected to publish but didn't before the deadline.
    /// Reported to the agent via `pending_lsp_servers` so they understand
    /// the result is partial.
    pub pending_servers: Vec<ServerKey>,
    /// Servers whose process exited between notification and deadline.
    /// Reported separately so the agent knows the gap is unrecoverable
    /// without a server restart, not "wait longer."
    pub exited_servers: Vec<ServerKey>,
}

impl PostEditWaitOutcome {
    /// True if every expected server reported a fresh result. False means
    /// the agent should treat the diagnostics as a partial picture.
    pub fn complete(&self) -> bool {
        self.pending_servers.is_empty() && self.exited_servers.is_empty()
    }
}

/// Per-server outcome of a `textDocument/diagnostic` (per-file pull) request.
#[derive(Debug, Clone)]
pub enum PullFileOutcome {
    /// Server returned a full report; diagnostics stored.
    Full { diagnostic_count: usize },
    /// Server returned `kind: "unchanged"` — cached diagnostics still valid.
    Unchanged,
    /// Server returned a partial-result token; we don't subscribe to streamed
    /// progress so the response is treated as a soft empty until the next pull.
    PartialNotSupported,
    /// Server doesn't advertise pull capability — caller should fall back to
    /// push diagnostics for this server.
    PullNotSupported,
    /// The pull request failed (timeout, server error, etc.).
    RequestFailed { reason: String },
}

/// Result of `pull_file_diagnostics` for one matching server.
#[derive(Debug, Clone)]
pub struct PullFileResult {
    pub server_key: ServerKey,
    pub outcome: PullFileOutcome,
}

/// Result of `pull_workspace_diagnostics` for a single server.
#[derive(Debug, Clone)]
pub struct PullWorkspaceResult {
    pub server_key: ServerKey,
    /// Files for which a Full report was received and cached. Files that came
    /// back as `Unchanged` are NOT listed here because their cached entry was
    /// already authoritative.
    pub files_reported: Vec<PathBuf>,
    /// True if the server returned a full response within the timeout.
    pub complete: bool,
    /// True if we cancelled (request timed out before the server responded).
    pub cancelled: bool,
    /// True if the server advertised workspace pull support. When false, the
    /// other fields are empty and the caller should fall back to file-mode
    /// pull or to push semantics.
    pub supports_workspace: bool,
}

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
    /// Extra env vars merged into every spawned LSP child. Used in tests to
    /// drive the fake server's behavioral variants (`AFT_FAKE_LSP_PULL=1`,
    /// `AFT_FAKE_LSP_WORKSPACE=1`, etc.). Production code does not set this.
    extra_env: HashMap<String, String>,
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
            extra_env: HashMap::new(),
        }
    }

    /// For testing: set an extra environment variable that gets passed to
    /// every spawned LSP child process. Useful for driving fake-server
    /// behavioral variants in integration tests.
    pub fn set_extra_env(&mut self, key: &str, value: &str) {
        self.extra_env.insert(key.to_string(), value.to_string());
    }

    /// Count active LSP server instances.
    pub fn server_count(&self) -> usize {
        self.clients.len()
    }

    /// For testing: override the binary for a server kind.
    pub fn override_binary(&mut self, kind: ServerKind, binary_path: PathBuf) {
        self.binary_overrides.insert(kind, binary_path);
    }

    /// Ensure a server is running for the given file. Spawns if needed.
    /// Returns the active server keys for the file, or an empty vec if none match.
    ///
    /// This is the lightweight wrapper around [`ensure_server_for_file_detailed`]
    /// that drops failure context. Prefer the detailed variant in command
    /// handlers that need to surface honest error messages to the agent.
    pub fn ensure_server_for_file(&mut self, file_path: &Path, config: &Config) -> Vec<ServerKey> {
        self.ensure_server_for_file_detailed(file_path, config)
            .successful
    }

    /// Detailed version of [`ensure_server_for_file`] that records every
    /// matching server's outcome (`Ok` / `NoRootMarker` / `BinaryNotInstalled`
    /// / `SpawnFailed`).
    ///
    /// Use this when the caller wants to honestly report _why_ a file has no
    /// active server (e.g., to surface "bash-language-server not on PATH" to
    /// the agent instead of silently returning `total: 0`).
    pub fn ensure_server_for_file_detailed(
        &mut self,
        file_path: &Path,
        config: &Config,
    ) -> EnsureServerOutcomes {
        let defs = servers_for_file(file_path, config);
        let mut outcomes = EnsureServerOutcomes::default();

        for def in defs {
            let server_id = def.kind.id_str().to_string();
            let server_name = def.name.to_string();

            let Some(root) = find_workspace_root(file_path, &def.root_markers) else {
                outcomes.attempts.push(ServerAttempt {
                    server_id,
                    server_name,
                    result: ServerAttemptResult::NoRootMarker {
                        looked_for: def.root_markers.iter().map(|s| s.to_string()).collect(),
                    },
                });
                continue;
            };

            let key = ServerKey {
                kind: def.kind.clone(),
                root,
            };

            if !self.clients.contains_key(&key) {
                match self.spawn_server(&def, &key.root, config) {
                    Ok(client) => {
                        self.clients.insert(key.clone(), client);
                        self.documents.entry(key.clone()).or_default();
                    }
                    Err(err) => {
                        log::error!("failed to spawn {}: {}", def.name, err);
                        let result = classify_spawn_error(&def.binary, &err);
                        outcomes.attempts.push(ServerAttempt {
                            server_id,
                            server_name,
                            result,
                        });
                        continue;
                    }
                }
            }

            outcomes.attempts.push(ServerAttempt {
                server_id,
                server_name,
                result: ServerAttemptResult::Ok {
                    server_key: key.clone(),
                },
            });
            outcomes.successful.push(key);
        }

        outcomes
    }

    /// Ensure a server is running using the default LSP registry.
    /// Kept for integration tests that exercise built-in server helpers directly.
    pub fn ensure_server_for_file_default(&mut self, file_path: &Path) -> Vec<ServerKey> {
        self.ensure_server_for_file(file_path, &Config::default())
    }
    /// Ensure that servers are running for the file and that the document is open
    /// in each server's DocumentStore. Reads file content from disk if not already open.
    /// Returns the server keys for the file.
    pub fn ensure_file_open(
        &mut self,
        file_path: &Path,
        config: &Config,
    ) -> Result<Vec<ServerKey>, LspError> {
        let canonical_path = canonicalize_for_lsp(file_path)?;
        let server_keys = self.ensure_server_for_file(&canonical_path, config);
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
                .is_some_and(|store| store.is_open(&canonical_path));

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
                continue;
            }

            // Document is already open. Check disk drift — if the file has
            // been modified outside the AFT pipeline (other tool, manual
            // edit, sibling session) we MUST send a didChange before any
            // pull-diagnostic / hover query, otherwise the LSP server
            // returns results computed from stale in-memory content.
            //
            // This is the regression fix Oracle flagged in finding #6:
            // "ensure_file_open skips already-open files without checking
            // if disk content changed."
            let drifted = self
                .documents
                .get(key)
                .is_some_and(|store| store.is_stale_on_disk(&canonical_path));
            if drifted {
                let content = std::fs::read_to_string(&canonical_path).map_err(LspError::Io)?;
                let next_version = self
                    .documents
                    .get(key)
                    .and_then(|store| store.version(&canonical_path))
                    .map(|v| v + 1)
                    .unwrap_or(1);
                if let Some(client) = self.clients.get_mut(key) {
                    client.send_notification::<DidChangeTextDocument>(
                        DidChangeTextDocumentParams {
                            text_document: VersionedTextDocumentIdentifier::new(
                                uri.clone(),
                                next_version,
                            ),
                            content_changes: vec![TextDocumentContentChangeEvent {
                                range: None,
                                range_length: None,
                                text: content,
                            }],
                        },
                    )?;
                }
                if let Some(store) = self.documents.get_mut(key) {
                    store.bump_version(&canonical_path);
                }
            }
        }

        Ok(server_keys)
    }

    pub fn ensure_file_open_default(
        &mut self,
        file_path: &Path,
    ) -> Result<Vec<ServerKey>, LspError> {
        self.ensure_file_open(file_path, &Config::default())
    }

    /// Notify relevant LSP servers that a file has been written/changed.
    /// This is the main hook called after every file write in AFT.
    ///
    /// If the file's server isn't running yet, starts it (lazy spawn).
    /// If the file isn't open in LSP yet, sends didOpen. Otherwise sends didChange.
    pub fn notify_file_changed(
        &mut self,
        file_path: &Path,
        content: &str,
        config: &Config,
    ) -> Result<(), LspError> {
        self.notify_file_changed_versioned(file_path, content, config)
            .map(|_| ())
    }

    /// Like `notify_file_changed`, but returns the target document version
    /// per server so the post-edit waiter can match `publishDiagnostics`
    /// against the exact version that this notification carried.
    ///
    /// Returns: `Vec<(ServerKey, target_version)>`. `target_version` is the
    /// `version` field on the `VersionedTextDocumentIdentifier` we just sent
    /// (post-bump). For freshly-opened documents (`didOpen`) the version is
    /// `0`. Servers that don't honor versioned text document sync will not
    /// echo this back on `publishDiagnostics`; the caller is expected to
    /// fall back to the epoch-delta path for those.
    pub fn notify_file_changed_versioned(
        &mut self,
        file_path: &Path,
        content: &str,
        config: &Config,
    ) -> Result<Vec<(ServerKey, i32)>, LspError> {
        let canonical_path = canonicalize_for_lsp(file_path)?;
        let server_keys = self.ensure_server_for_file(&canonical_path, config);
        if server_keys.is_empty() {
            return Ok(Vec::new());
        }

        let uri = uri_for_path(&canonical_path)?;
        let language_id = language_id_for_extension(
            canonical_path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default(),
        )
        .to_string();

        let mut versions: Vec<(ServerKey, i32)> = Vec::with_capacity(server_keys.len());

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
                versions.push((key, next_version));
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
                .entry(key.clone())
                .or_default()
                .open(canonical_path.clone());
            // didOpen carries version 0 — that's the version the server
            // will echo on its first publishDiagnostics for this document.
            versions.push((key, 0));
        }

        Ok(versions)
    }

    pub fn notify_file_changed_default(
        &mut self,
        file_path: &Path,
        content: &str,
    ) -> Result<(), LspError> {
        self.notify_file_changed(file_path, content, &Config::default())
    }

    /// Notify every active server whose workspace contains at least one changed
    /// path that watched files changed. This is intentionally workspace-scoped
    /// rather than extension-scoped: configuration edits such as `package.json`
    /// or `tsconfig.json` affect a server's project graph even though those
    /// files may not be documents handled by the server itself.
    pub fn notify_files_watched_changed(
        &mut self,
        paths: &[(PathBuf, FileChangeType)],
        _config: &Config,
    ) -> Result<(), LspError> {
        if paths.is_empty() {
            return Ok(());
        }

        let mut canonical_events = Vec::with_capacity(paths.len());
        for (path, typ) in paths {
            let canonical_path = resolve_for_lsp_uri(path);
            canonical_events.push((canonical_path, *typ));
        }

        let keys: Vec<ServerKey> = self.clients.keys().cloned().collect();
        for key in keys {
            let mut changes = Vec::new();
            for (path, typ) in &canonical_events {
                if !path.starts_with(&key.root) {
                    continue;
                }
                changes.push(FileEvent::new(uri_for_path(path)?, *typ));
            }

            if changes.is_empty() {
                continue;
            }

            if let Some(client) = self.clients.get_mut(&key) {
                // Only send if the server advertised this capability (#32).
                // Sending didChangeWatchedFiles to a server that didn't declare
                // workspace.didChangeWatchedFiles causes spurious errors on some
                // servers (e.g. older tsserver builds) and is a spec violation.
                if !client.supports_watched_files() {
                    log::debug!(
                        "[aft-lsp] skipping didChangeWatchedFiles for {:?} (capability not declared)",
                        key
                    );
                    continue;
                }
                client.send_notification::<DidChangeWatchedFiles>(DidChangeWatchedFilesParams {
                    changes,
                })?;
            }
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
    pub fn client_for_file(&self, file_path: &Path, config: &Config) -> Option<&LspClient> {
        let key = self.server_key_for_file(file_path, config)?;
        self.clients.get(&key)
    }

    pub fn client_for_file_default(&self, file_path: &Path) -> Option<&LspClient> {
        self.client_for_file(file_path, &Config::default())
    }

    /// Get a mutable active client for a file path, if one exists.
    pub fn client_for_file_mut(
        &mut self,
        file_path: &Path,
        config: &Config,
    ) -> Option<&mut LspClient> {
        let key = self.server_key_for_file(file_path, config)?;
        self.clients.get_mut(&key)
    }

    pub fn client_for_file_mut_default(&mut self, file_path: &Path) -> Option<&mut LspClient> {
        self.client_for_file_mut(file_path, &Config::default())
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
        config: &Config,
        timeout: std::time::Duration,
    ) -> Vec<StoredDiagnostic> {
        let deadline = std::time::Instant::now() + timeout;
        self.wait_for_file_diagnostics(file_path, config, deadline)
    }

    pub fn wait_for_diagnostics_default(
        &mut self,
        file_path: &Path,
        timeout: std::time::Duration,
    ) -> Vec<StoredDiagnostic> {
        self.wait_for_diagnostics(file_path, &Config::default(), timeout)
    }

    /// Test-only accessor for the diagnostics store. Used by integration
    /// tests that need to inspect per-server entries (e.g., to verify that
    /// `ServerKey::root` is populated correctly, not the empty path that
    /// the legacy `publish_with_kind` path produced).
    #[doc(hidden)]
    pub fn diagnostics_store_for_test(&self) -> &DiagnosticsStore {
        &self.diagnostics
    }

    /// Snapshot the current per-server epoch for every entry that exists
    /// for `file_path`. Servers without an entry yet (never published)
    /// are absent from the map; for those, `pre = 0` (any first publish
    /// will be considered fresh under the epoch-fallback rule).
    pub fn snapshot_diagnostic_epochs(&self, file_path: &Path) -> HashMap<ServerKey, u64> {
        let lookup_path = normalize_lookup_path(file_path);
        self.diagnostics
            .entries_for_file(&lookup_path)
            .into_iter()
            .map(|(key, entry)| (key.clone(), entry.epoch))
            .collect()
    }

    /// Wait for FRESH per-server diagnostics that match the just-sent
    /// document version. This is the v0.17.3 post-edit path that fixes the
    /// stale-diagnostics bug: instead of returning whatever is in the cache
    /// when the deadline hits, we only return entries whose `version`
    /// matches the post-edit target version (or, for servers that don't
    /// participate in versioned sync, whose `epoch` was bumped after the
    /// pre-edit snapshot).
    ///
    /// `expected_versions` should come from `notify_file_changed_versioned`
    /// — one `(ServerKey, target_version)` per server we sent didChange/
    /// didOpen to.
    ///
    /// `pre_snapshot` is the per-server epoch BEFORE the notification was
    /// sent; it gates the epoch-fallback path so an old-version publish
    /// arriving after `drain_events` and before `didChange` cannot be
    /// mistaken for a fresh response.
    ///
    /// Returns a per-server tri-state: `Fresh` (publish matched target
    /// version OR epoch advanced past snapshot for an unversioned server),
    /// `Pending` (deadline hit before this server published anything we
    /// could verify), or `Exited` (server died between notification and
    /// deadline).
    pub fn wait_for_post_edit_diagnostics(
        &mut self,
        file_path: &Path,
        // `config` is intentionally accepted (matches sibling wait APIs and
        // future-proofs us if freshness rules need it). Currently unused
        // because expected_versions/pre_snapshot fully determine behavior.
        _config: &Config,
        expected_versions: &[(ServerKey, i32)],
        pre_snapshot: &HashMap<ServerKey, u64>,
        timeout: std::time::Duration,
    ) -> PostEditWaitOutcome {
        let lookup_path = normalize_lookup_path(file_path);
        let deadline = std::time::Instant::now() + timeout;

        // Drain any events that arrived while we were sending didChange.
        // The publishDiagnostics handler stores the version, so even
        // pre-snapshot publishes that landed late won't be mistaken for
        // fresh — the version-match check will reject them.
        let _ = self.drain_events_for_file(&lookup_path);

        let mut fresh: HashMap<ServerKey, Vec<StoredDiagnostic>> = HashMap::new();
        let mut exited: Vec<ServerKey> = Vec::new();

        loop {
            // Check freshness for every expected server. A server is fresh
            // if its current entry for this file satisfies either:
            //   1. version-match: entry.version == Some(target_version), OR
            //   2. epoch-fallback: entry.version is None AND
            //      entry.epoch > pre_snapshot.get(&key).copied().unwrap_or(0)
            // Servers whose process has exited are reported separately.
            for (key, target_version) in expected_versions {
                if fresh.contains_key(key) || exited.contains(key) {
                    continue;
                }
                if !self.clients.contains_key(key) {
                    exited.push(key.clone());
                    continue;
                }
                if let Some(entry) = self
                    .diagnostics
                    .entries_for_file(&lookup_path)
                    .into_iter()
                    .find_map(|(k, e)| if k == key { Some(e) } else { None })
                {
                    let is_fresh = match entry.version {
                        Some(v) => v == *target_version,
                        None => {
                            let pre = pre_snapshot.get(key).copied().unwrap_or(0);
                            entry.epoch > pre
                        }
                    };
                    if is_fresh {
                        fresh.insert(key.clone(), entry.diagnostics.clone());
                    }
                }
            }

            // All accounted for? Done.
            if fresh.len() + exited.len() == expected_versions.len() {
                break;
            }

            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }

            let timeout = deadline.saturating_duration_since(now);
            match self.event_rx.recv_timeout(timeout) {
                Ok(event) => {
                    self.handle_event(&event);
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        // Pending = expected but neither fresh nor exited.
        let pending: Vec<ServerKey> = expected_versions
            .iter()
            .filter(|(k, _)| !fresh.contains_key(k) && !exited.contains(k))
            .map(|(k, _)| k.clone())
            .collect();

        // Build deduplicated, sorted diagnostics from the fresh servers only.
        // Stale or pending servers contribute zero diagnostics.
        let mut diagnostics: Vec<StoredDiagnostic> = fresh
            .into_iter()
            .flat_map(|(_, diags)| diags.into_iter())
            .collect();
        diagnostics.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line.cmp(&b.line))
                .then(a.column.cmp(&b.column))
                .then(a.message.cmp(&b.message))
        });

        PostEditWaitOutcome {
            diagnostics,
            pending_servers: pending,
            exited_servers: exited,
        }
    }

    /// Wait for diagnostics to arrive for a specific file until a deadline.
    ///
    /// Drains already-queued events first, then blocks on the shared event
    /// channel only until either `publishDiagnostics` arrives for this file or
    /// the deadline is reached.
    pub fn wait_for_file_diagnostics(
        &mut self,
        file_path: &Path,
        config: &Config,
        deadline: std::time::Instant,
    ) -> Vec<StoredDiagnostic> {
        let lookup_path = normalize_lookup_path(file_path);

        if self.server_key_for_file(&lookup_path, config).is_none() {
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

    /// Default timeout for `textDocument/diagnostic` (per-file pull). Servers
    /// usually respond in under 1s for files they've already analyzed; we
    /// allow up to 10s before falling back to push semantics. Currently
    /// surfaced via [`Self::pull_file_timeout`] for callers that want to
    /// override the wait via the `wait_ms` knob.
    pub const PULL_FILE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Public accessor so command handlers can reuse the documented default.
    pub fn pull_file_timeout() -> std::time::Duration {
        Self::PULL_FILE_TIMEOUT
    }

    /// Default timeout for `workspace/diagnostic`. The LSP spec allows the
    /// server to hold this open indefinitely; we cap at 10s and report
    /// `complete: false` to the agent rather than hanging the bridge.
    const PULL_WORKSPACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Issue a `textDocument/diagnostic` (LSP 3.17 per-file pull) request to
    /// every server that supports pull diagnostics for the given file.
    ///
    /// Returns the per-server outcome. If a server reports `kind: "unchanged"`,
    /// the cached entry's diagnostics are surfaced (deterministic re-use of
    /// the previous response). If a server doesn't advertise pull capability,
    /// it's skipped here — the caller should fall back to push for those.
    ///
    /// Side effects: results are stored in `DiagnosticsStore` so directory-mode
    /// queries can aggregate them later.
    pub fn pull_file_diagnostics(
        &mut self,
        file_path: &Path,
        config: &Config,
    ) -> Result<Vec<PullFileResult>, LspError> {
        let canonical_path = canonicalize_for_lsp(file_path)?;
        // Make sure servers are running and the document is open with fresh
        // content (handles disk-drift via DocumentStore::is_stale_on_disk).
        self.ensure_file_open(&canonical_path, config)?;

        let server_keys = self.ensure_server_for_file(&canonical_path, config);
        if server_keys.is_empty() {
            return Ok(Vec::new());
        }

        let uri = uri_for_path(&canonical_path)?;
        let mut results = Vec::with_capacity(server_keys.len());

        for key in server_keys {
            let supports_pull = self
                .clients
                .get(&key)
                .and_then(|c| c.diagnostic_capabilities())
                .is_some_and(|caps| caps.pull_diagnostics);

            if !supports_pull {
                results.push(PullFileResult {
                    server_key: key.clone(),
                    outcome: PullFileOutcome::PullNotSupported,
                });
                continue;
            }

            // Look up previous resultId for incremental requests.
            let previous_result_id = self
                .diagnostics
                .entries_for_file(&canonical_path)
                .into_iter()
                .find(|(k, _)| **k == key)
                .and_then(|(_, entry)| entry.result_id.clone());

            let identifier = self
                .clients
                .get(&key)
                .and_then(|c| c.diagnostic_capabilities())
                .and_then(|caps| caps.identifier.clone());

            let params = lsp_types::DocumentDiagnosticParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                identifier,
                previous_result_id,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            };

            let outcome = match self.send_pull_request(&key, params) {
                Ok(report) => self.ingest_document_report(&key, &canonical_path, report),
                Err(err) => PullFileOutcome::RequestFailed {
                    reason: err.to_string(),
                },
            };

            results.push(PullFileResult {
                server_key: key,
                outcome,
            });
        }

        Ok(results)
    }

    /// Issue a `workspace/diagnostic` request to a specific server. Cancels
    /// internally if `timeout` elapses before the server responds. Cached
    /// entries from the response are stored so directory-mode queries pick
    /// them up.
    pub fn pull_workspace_diagnostics(
        &mut self,
        server_key: &ServerKey,
        timeout: Option<std::time::Duration>,
    ) -> Result<PullWorkspaceResult, LspError> {
        let _timeout = timeout.unwrap_or(Self::PULL_WORKSPACE_TIMEOUT);

        let supports_workspace = self
            .clients
            .get(server_key)
            .and_then(|c| c.diagnostic_capabilities())
            .is_some_and(|caps| caps.workspace_diagnostics);

        if !supports_workspace {
            return Ok(PullWorkspaceResult {
                server_key: server_key.clone(),
                files_reported: Vec::new(),
                complete: false,
                cancelled: false,
                supports_workspace: false,
            });
        }

        let identifier = self
            .clients
            .get(server_key)
            .and_then(|c| c.diagnostic_capabilities())
            .and_then(|caps| caps.identifier.clone());

        let params = lsp_types::WorkspaceDiagnosticParams {
            identifier,
            previous_result_ids: Vec::new(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        // Note: LspClient::send_request currently uses a fixed REQUEST_TIMEOUT
        // (30s, see client.rs). For workspace pull this is intentionally not
        // overridden because servers like rust-analyzer may legitimately take
        // many seconds on first request. The plugin bridge timeout (also 30s)
        // is what we ultimately defer to. In a future revision we should plumb
        // a custom timeout through send_request — for v0.16 we accept that
        // workspace pull obeys the standard request timeout.
        let result = match self
            .clients
            .get_mut(server_key)
            .ok_or_else(|| LspError::ServerNotReady("server not found".into()))?
            .send_request::<lsp_types::request::WorkspaceDiagnosticRequest>(params)
        {
            Ok(result) => result,
            Err(LspError::Timeout(_)) => {
                return Ok(PullWorkspaceResult {
                    server_key: server_key.clone(),
                    files_reported: Vec::new(),
                    complete: false,
                    cancelled: true,
                    supports_workspace: true,
                });
            }
            Err(err) => return Err(err),
        };

        // Extract the items list. Partial responses stream via $/progress
        // notifications which we don't subscribe to — treat them as soft
        // empty (caller will see complete: true with files_reported empty,
        // matching "got a partial response, no full report").
        let items = match result {
            lsp_types::WorkspaceDiagnosticReportResult::Report(report) => report.items,
            lsp_types::WorkspaceDiagnosticReportResult::Partial(_) => Vec::new(),
        };

        // Ingest each file report into the diagnostics store.
        let mut files_reported = Vec::with_capacity(items.len());
        for item in items {
            match item {
                lsp_types::WorkspaceDocumentDiagnosticReport::Full(full) => {
                    if let Some(file) = uri_to_path(&full.uri) {
                        let stored = from_lsp_diagnostics(
                            file.clone(),
                            full.full_document_diagnostic_report.items.clone(),
                        );
                        self.diagnostics.publish_with_result_id(
                            server_key.clone(),
                            file.clone(),
                            stored,
                            full.full_document_diagnostic_report.result_id.clone(),
                        );
                        files_reported.push(file);
                    }
                }
                lsp_types::WorkspaceDocumentDiagnosticReport::Unchanged(_unchanged) => {
                    // "Unchanged" means the previously cached report is still
                    // valid. We left it in place; nothing to do.
                }
            }
        }

        Ok(PullWorkspaceResult {
            server_key: server_key.clone(),
            files_reported,
            complete: true,
            cancelled: false,
            supports_workspace: true,
        })
    }

    /// Issue the per-file diagnostic request and return the report.
    fn send_pull_request(
        &mut self,
        key: &ServerKey,
        params: lsp_types::DocumentDiagnosticParams,
    ) -> Result<lsp_types::DocumentDiagnosticReportResult, LspError> {
        let client = self
            .clients
            .get_mut(key)
            .ok_or_else(|| LspError::ServerNotReady("server not found".into()))?;
        client.send_request::<lsp_types::request::DocumentDiagnosticRequest>(params)
    }

    /// Store the result of a per-file pull request and return a structured
    /// outcome the caller can inspect.
    fn ingest_document_report(
        &mut self,
        key: &ServerKey,
        canonical_path: &Path,
        result: lsp_types::DocumentDiagnosticReportResult,
    ) -> PullFileOutcome {
        let report = match result {
            lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
            lsp_types::DocumentDiagnosticReportResult::Partial(_) => {
                // Partial results stream in via $/progress notifications which
                // we don't currently subscribe to. Treat as a soft-empty
                // success — the next pull will get the full version.
                return PullFileOutcome::PartialNotSupported;
            }
        };

        match report {
            lsp_types::DocumentDiagnosticReport::Full(full) => {
                let result_id = full.full_document_diagnostic_report.result_id.clone();
                let stored = from_lsp_diagnostics(
                    canonical_path.to_path_buf(),
                    full.full_document_diagnostic_report.items.clone(),
                );
                let count = stored.len();
                self.diagnostics.publish_with_result_id(
                    key.clone(),
                    canonical_path.to_path_buf(),
                    stored,
                    result_id,
                );
                PullFileOutcome::Full {
                    diagnostic_count: count,
                }
            }
            lsp_types::DocumentDiagnosticReport::Unchanged(_unchanged) => {
                // The server says cache is still valid. We don't refresh
                // anything; the existing entry's diagnostics remain authoritative.
                PullFileOutcome::Unchanged
            }
        }
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

    /// Active server keys (running clients). Used by `lsp_diagnostics`
    /// directory mode to know which servers to ask for workspace pull.
    pub fn active_server_keys(&self) -> Vec<ServerKey> {
        self.clients.keys().cloned().collect()
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
                root,
                method,
                params: Some(params),
            } if method == "textDocument/publishDiagnostics" => {
                self.handle_publish_diagnostics(server_kind.clone(), root.clone(), params)
            }
            LspEvent::ServerExited { server_kind, root } => {
                let key = ServerKey {
                    kind: server_kind.clone(),
                    root: root.clone(),
                };
                self.clients.remove(&key);
                self.documents.remove(&key);
                self.diagnostics.clear_server(server_kind.clone());
                None
            }
            _ => None,
        }
    }

    fn handle_publish_diagnostics(
        &mut self,
        server: ServerKind,
        root: PathBuf,
        params: &serde_json::Value,
    ) -> Option<PathBuf> {
        if let Ok(publish_params) =
            serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params.clone())
        {
            let file = uri_to_path(&publish_params.uri)?;
            let stored = from_lsp_diagnostics(file.clone(), publish_params.diagnostics);
            // v0.17.3: store with real ServerKey { kind, root } and capture
            // the document `version` (when the server provided one) so the
            // post-edit waiter can reject stale publishes deterministically
            // via version-match (preferred) or epoch-delta (fallback). The
            // earlier `publish_with_kind` path silently dropped both.
            let key = ServerKey { kind: server, root };
            self.diagnostics
                .publish_full(key, file.clone(), stored, None, publish_params.version);
            return Some(file);
        }
        None
    }

    fn spawn_server(
        &self,
        def: &ServerDef,
        root: &Path,
        config: &Config,
    ) -> Result<LspClient, LspError> {
        let binary = self.resolve_binary(def, config)?;

        // Merge the server-defined env with our test-injected env.
        // `extra_env` is empty in production; tests use it to drive fake
        // server variants (AFT_FAKE_LSP_PULL=1, etc.).
        let mut merged_env = def.env.clone();
        for (key, value) in &self.extra_env {
            merged_env.insert(key.clone(), value.clone());
        }

        let mut client = LspClient::spawn(
            def.kind.clone(),
            root.to_path_buf(),
            &binary,
            &def.args,
            &merged_env,
            self.event_tx.clone(),
        )?;
        client.initialize(root, def.initialization_options.clone())?;
        Ok(client)
    }

    fn resolve_binary(&self, def: &ServerDef, config: &Config) -> Result<PathBuf, LspError> {
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

        if let Some(path) = env_binary_override(&def.kind) {
            if path.exists() {
                return Ok(path);
            }
            return Err(LspError::NotFound(format!(
                "environment override binary for {:?} not found: {}",
                def.kind,
                path.display()
            )));
        }

        // Layered resolution:
        //   1. <project_root>/node_modules/.bin/<binary>
        //   2. config.lsp_paths_extra (plugin auto-install cache, etc.)
        //   3. PATH via `which`
        resolve_lsp_binary(
            &def.binary,
            config.project_root.as_deref(),
            &config.lsp_paths_extra,
        )
        .ok_or_else(|| {
            LspError::NotFound(format!(
                "language server binary '{}' not found in node_modules/.bin, lsp_paths_extra, or PATH",
                def.binary
            ))
        })
    }

    fn server_key_for_file(&self, file_path: &Path, config: &Config) -> Option<ServerKey> {
        for def in servers_for_file(file_path, config) {
            let root = find_workspace_root(file_path, &def.root_markers)?;
            let key = ServerKey {
                kind: def.kind.clone(),
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

fn resolve_for_lsp_uri(file_path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(file_path) {
        return path;
    }

    let mut existing = file_path.to_path_buf();
    let mut missing = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            break;
        };
        missing.push(name.to_owned());
        let Some(parent) = existing.parent() else {
            break;
        };
        existing = parent.to_path_buf();
    }

    let mut resolved = std::fs::canonicalize(&existing).unwrap_or(existing);
    for segment in missing.into_iter().rev() {
        resolved.push(segment);
    }
    resolved
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
        "html" | "htm" => "html",
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

/// Classify an error returned by `spawn_server` into a structured
/// `ServerAttemptResult`. The two interesting cases for callers are:
/// - `BinaryNotInstalled` — the server's binary couldn't be resolved on PATH
///   or via override. The agent can be told "install bash-language-server".
/// - `SpawnFailed` — binary was found but spawning/initializing failed
///   (permissions, missing runtime, server crashed during initialize, etc.).
fn classify_spawn_error(binary: &str, err: &LspError) -> ServerAttemptResult {
    match err {
        // resolve_binary returns NotFound for both missing override paths and
        // missing PATH binaries. The "override missing" case is rare in
        // practice (only set in tests / env vars); we report all NotFound as
        // BinaryNotInstalled so the user sees an actionable install hint.
        LspError::NotFound(_) => ServerAttemptResult::BinaryNotInstalled {
            binary: binary.to_string(),
        },
        other => ServerAttemptResult::SpawnFailed {
            binary: binary.to_string(),
            reason: other.to_string(),
        },
    }
}

fn env_binary_override(kind: &ServerKind) -> Option<PathBuf> {
    let id = kind.id_str();
    let suffix: String = id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    let key = format!("AFT_LSP_{suffix}_BINARY");
    std::env::var_os(key).map(PathBuf::from)
}
