use std::cell::{Ref, RefCell, RefMut};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;

use notify::RecommendedWatcher;

use crate::backup::BackupStore;
use crate::callgraph::CallGraph;
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::language::LanguageProvider;
use crate::lsp::manager::LspManager;
use crate::search_index::SearchIndex;
use crate::semantic_index::SemanticIndex;

#[derive(Debug, Clone)]
pub enum SemanticIndexStatus {
    Disabled,
    Building {
        stage: String,
        files: Option<usize>,
        entries_done: Option<usize>,
        entries_total: Option<usize>,
    },
    Ready,
    Failed(String),
}

pub enum SemanticIndexEvent {
    Progress {
        stage: String,
        files: Option<usize>,
        entries_done: Option<usize>,
        entries_total: Option<usize>,
    },
    Ready(SemanticIndex),
    Failed(String),
}

/// Normalize a path by resolving `.` and `..` components lexically,
/// without touching the filesystem. This prevents path traversal
/// attacks when `fs::canonicalize` fails (e.g. for non-existent paths).
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                // Pop the last component unless we're at root or have no components
                if !result.pop() {
                    result.push(component);
                }
            }
            Component::CurDir => {} // Skip `.`
            _ => result.push(component),
        }
    }
    result
}

fn resolve_with_existing_ancestors(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail_segments = Vec::new();

    while !existing.exists() {
        if let Some(name) = existing.file_name() {
            tail_segments.push(name.to_owned());
        } else {
            break;
        }

        existing = match existing.parent() {
            Some(parent) => parent.to_path_buf(),
            None => break,
        };
    }

    let mut resolved = std::fs::canonicalize(&existing).unwrap_or(existing);
    for segment in tail_segments.into_iter().rev() {
        resolved.push(segment);
    }

    resolved
}

/// Shared application context threaded through all command handlers.
///
/// Holds the language provider, backup/checkpoint stores, configuration,
/// and call graph engine. Constructed once at startup and passed by
/// reference to `dispatch`.
///
/// Stores use `RefCell` for interior mutability — the binary is single-threaded
/// (one request at a time on the stdin read loop) so runtime borrow checking
/// is safe and never contended.
pub struct AppContext {
    provider: Box<dyn LanguageProvider>,
    backup: RefCell<BackupStore>,
    checkpoint: RefCell<CheckpointStore>,
    config: RefCell<Config>,
    callgraph: RefCell<Option<CallGraph>>,
    search_index: RefCell<Option<SearchIndex>>,
    search_index_rx:
        RefCell<Option<crossbeam_channel::Receiver<(SearchIndex, crate::parser::SymbolCache)>>>,
    semantic_index: RefCell<Option<SemanticIndex>>,
    semantic_index_rx: RefCell<Option<crossbeam_channel::Receiver<SemanticIndexEvent>>>,
    semantic_index_status: RefCell<SemanticIndexStatus>,
    semantic_embedding_model: RefCell<Option<crate::semantic_index::EmbeddingModel>>,
    watcher: RefCell<Option<RecommendedWatcher>>,
    watcher_rx: RefCell<Option<mpsc::Receiver<notify::Result<notify::Event>>>>,
    lsp_manager: RefCell<LspManager>,
}

impl AppContext {
    pub fn new(provider: Box<dyn LanguageProvider>, config: Config) -> Self {
        AppContext {
            provider,
            backup: RefCell::new(BackupStore::new()),
            checkpoint: RefCell::new(CheckpointStore::new()),
            config: RefCell::new(config),
            callgraph: RefCell::new(None),
            search_index: RefCell::new(None),
            search_index_rx: RefCell::new(None),
            semantic_index: RefCell::new(None),
            semantic_index_rx: RefCell::new(None),
            semantic_index_status: RefCell::new(SemanticIndexStatus::Disabled),
            semantic_embedding_model: RefCell::new(None),
            watcher: RefCell::new(None),
            watcher_rx: RefCell::new(None),
            lsp_manager: RefCell::new(LspManager::new()),
        }
    }

    /// Access the language provider.
    pub fn provider(&self) -> &dyn LanguageProvider {
        self.provider.as_ref()
    }

    /// Access the backup store.
    pub fn backup(&self) -> &RefCell<BackupStore> {
        &self.backup
    }

    /// Access the checkpoint store.
    pub fn checkpoint(&self) -> &RefCell<CheckpointStore> {
        &self.checkpoint
    }

    /// Access the configuration (shared borrow).
    pub fn config(&self) -> Ref<'_, Config> {
        self.config.borrow()
    }

    /// Access the configuration (mutable borrow).
    pub fn config_mut(&self) -> RefMut<'_, Config> {
        self.config.borrow_mut()
    }

    /// Access the call graph engine.
    pub fn callgraph(&self) -> &RefCell<Option<CallGraph>> {
        &self.callgraph
    }

    /// Access the search index.
    pub fn search_index(&self) -> &RefCell<Option<SearchIndex>> {
        &self.search_index
    }

    /// Access the search-index build receiver (returns index + pre-warmed symbol cache).
    pub fn search_index_rx(
        &self,
    ) -> &RefCell<Option<crossbeam_channel::Receiver<(SearchIndex, crate::parser::SymbolCache)>>>
    {
        &self.search_index_rx
    }

    /// Access the semantic search index.
    pub fn semantic_index(&self) -> &RefCell<Option<SemanticIndex>> {
        &self.semantic_index
    }

    /// Access the semantic-index build receiver.
    pub fn semantic_index_rx(
        &self,
    ) -> &RefCell<Option<crossbeam_channel::Receiver<SemanticIndexEvent>>> {
        &self.semantic_index_rx
    }

    pub fn semantic_index_status(&self) -> &RefCell<SemanticIndexStatus> {
        &self.semantic_index_status
    }

    /// Access the cached semantic embedding model.
    pub fn semantic_embedding_model(&self) -> &RefCell<Option<crate::semantic_index::EmbeddingModel>> {
        &self.semantic_embedding_model
    }

    /// Access the file watcher handle (kept alive to continue watching).
    pub fn watcher(&self) -> &RefCell<Option<RecommendedWatcher>> {
        &self.watcher
    }

    /// Access the watcher event receiver.
    pub fn watcher_rx(&self) -> &RefCell<Option<mpsc::Receiver<notify::Result<notify::Event>>>> {
        &self.watcher_rx
    }

    /// Access the LSP manager.
    pub fn lsp(&self) -> RefMut<'_, LspManager> {
        self.lsp_manager.borrow_mut()
    }

    /// Notify LSP servers that a file was written.
    /// Call this after write_format_validate in command handlers.
    pub fn lsp_notify_file_changed(&self, file_path: &Path, content: &str) {
        if let Ok(mut lsp) = self.lsp_manager.try_borrow_mut() {
            if let Err(e) = lsp.notify_file_changed(file_path, content) {
                log::warn!("sync error for {}: {}", file_path.display(), e);
            }
        }
    }

    /// Notify LSP and optionally wait for diagnostics.
    ///
    /// Call this after `write_format_validate` when the request has `"diagnostics": true`.
    /// Sends didChange to the server, waits briefly for publishDiagnostics, and returns
    /// any diagnostics for the file. If no server is running, returns empty immediately.
    pub fn lsp_notify_and_collect_diagnostics(
        &self,
        file_path: &Path,
        content: &str,
        timeout: std::time::Duration,
    ) -> Vec<crate::lsp::diagnostics::StoredDiagnostic> {
        let Ok(mut lsp) = self.lsp_manager.try_borrow_mut() else {
            return Vec::new();
        };

        // Clear any queued notifications before this write so the wait loop only
        // observes diagnostics triggered by the current change.
        lsp.drain_events();

        // Send didChange/didOpen
        if let Err(e) = lsp.notify_file_changed(file_path, content) {
            log::warn!("sync error for {}: {}", file_path.display(), e);
            return Vec::new();
        }

        // Wait for diagnostics to arrive
        lsp.wait_for_diagnostics(file_path, timeout)
    }

    /// Post-write LSP hook: notify server and optionally collect diagnostics.
    ///
    /// This is the single call site for all command handlers after `write_format_validate`.
    /// When `diagnostics` is true, it notifies the server, waits until matching
    /// diagnostics arrive or the timeout expires, and returns diagnostics for the file.
    /// When false, it just notifies (fire-and-forget).
    pub fn lsp_post_write(
        &self,
        file_path: &Path,
        content: &str,
        params: &serde_json::Value,
    ) -> Vec<crate::lsp::diagnostics::StoredDiagnostic> {
        let wants_diagnostics = params
            .get("diagnostics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !wants_diagnostics {
            self.lsp_notify_file_changed(file_path, content);
            return Vec::new();
        }

        let wait_ms = params
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(1500)
            .min(10_000); // Cap at 10 seconds to prevent hangs from adversarial input

        self.lsp_notify_and_collect_diagnostics(
            file_path,
            content,
            std::time::Duration::from_millis(wait_ms),
        )
    }

    /// Validate that a file path falls within the configured project root.
    ///
    /// When `project_root` is configured (normal plugin usage), this resolves the
    /// path and checks it starts with the root. Returns the canonicalized path on
    /// success, or an error response on violation.
    ///
    /// When no `project_root` is configured (direct CLI usage), all paths pass
    /// through unrestricted for backward compatibility.
    pub fn validate_path(
        &self,
        req_id: &str,
        path: &Path,
    ) -> Result<std::path::PathBuf, crate::protocol::Response> {
        let config = self.config();
        // When restrict_to_project_root is false (default), allow all paths
        if !config.restrict_to_project_root {
            return Ok(path.to_path_buf());
        }
        let root = match &config.project_root {
            Some(r) => r.clone(),
            None => return Ok(path.to_path_buf()), // No root configured, allow all
        };
        drop(config);

        // Resolve the path (follow symlinks, normalize ..)
        let resolved = std::fs::canonicalize(path)
            .unwrap_or_else(|_| resolve_with_existing_ancestors(&normalize_path(path)));

        let resolved_root = std::fs::canonicalize(&root).unwrap_or(root);

        if !resolved.starts_with(&resolved_root) {
            return Err(crate::protocol::Response::error(
                req_id,
                "path_outside_root",
                format!(
                    "path '{}' is outside the project root '{}'",
                    path.display(),
                    resolved_root.display()
                ),
            ));
        }

        Ok(resolved)
    }

    /// Count active LSP server instances.
    pub fn lsp_server_count(&self) -> usize {
        self.lsp_manager
            .try_borrow()
            .map(|lsp| lsp.server_count())
            .unwrap_or(0)
    }

    /// Symbol cache statistics from the language provider.
    pub fn symbol_cache_stats(&self) -> serde_json::Value {
        if let Some(tsp) = self
            .provider
            .as_any()
            .downcast_ref::<crate::parser::TreeSitterProvider>()
        {
            let (local, warm) = tsp.symbol_cache_stats();
            serde_json::json!({
                "local_entries": local,
                "warm_entries": warm,
            })
        } else {
            serde_json::json!({
                "local_entries": 0,
                "warm_entries": 0,
            })
        }
    }
}
