use std::cell::{Ref, RefCell, RefMut};
use std::path::Path;
use std::sync::mpsc;

use notify::RecommendedWatcher;

use crate::backup::BackupStore;
use crate::callgraph::CallGraph;
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::language::LanguageProvider;
use crate::lsp::manager::LspManager;

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
                eprintln!("[aft-lsp] sync error for {}: {}", file_path.display(), e);
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

        // Send didChange/didOpen
        if let Err(e) = lsp.notify_file_changed(file_path, content) {
            eprintln!("[aft-lsp] sync error for {}: {}", file_path.display(), e);
            return Vec::new();
        }

        // Wait for diagnostics to arrive
        lsp.wait_for_diagnostics(file_path, timeout)
    }

    /// Post-write LSP hook: notify server and optionally collect diagnostics.
    ///
    /// This is the single call site for all command handlers after `write_format_validate`.
    /// When `diagnostics` is true, it notifies the server, waits briefly, drains
    /// queued LSP notifications, and returns diagnostics for the file.
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

        self.lsp_notify_file_changed(file_path, content);

        if !wants_diagnostics {
            return Vec::new();
        }

        std::thread::sleep(std::time::Duration::from_millis(1500));
        let canonical_path =
            std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf());

        let mut lsp = self.lsp();
        lsp.drain_events();
        lsp.get_diagnostics_for_file(&canonical_path)
            .into_iter()
            .cloned()
            .collect()
    }
}
