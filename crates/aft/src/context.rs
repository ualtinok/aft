use std::cell::{Ref, RefCell, RefMut};
use std::path::{Component, Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};

use lsp_types::FileChangeType;
use notify::RecommendedWatcher;

use crate::backup::BackupStore;
use crate::bash_background::{BgCompletion, BgTaskRegistry};
use crate::callgraph::CallGraph;
use crate::checkpoint::CheckpointStore;
use crate::config::Config;
use crate::language::LanguageProvider;
use crate::lsp::manager::LspManager;
use crate::lsp::registry::is_config_file_path_with_custom;
use crate::protocol::{ProgressFrame, PushFrame};

pub type ProgressSender = Arc<Box<dyn Fn(PushFrame) + Send + Sync>>;
pub type SharedProgressSender = Arc<Mutex<Option<ProgressSender>>>;
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

fn path_error_response(
    req_id: &str,
    path: &Path,
    resolved_root: &Path,
) -> crate::protocol::Response {
    crate::protocol::Response::error(
        req_id,
        "path_outside_root",
        format!(
            "path '{}' is outside the project root '{}'",
            path.display(),
            resolved_root.display()
        ),
    )
}

/// Walk `candidate` component-by-component. For any component that is a
/// symlink on disk, iteratively follow the full chain (up to 40 hops) and
/// reject if any hop's resolved target lies outside `resolved_root`.
///
/// This is the fallback path used when `fs::canonicalize` fails (e.g. on
/// Linux with broken symlink chains pointing to non-existent destinations).
/// On macOS `canonicalize` also fails for broken symlinks but the returned
/// `/var/...` tempdir paths diverge from `resolved_root`'s `/private/var/...`
/// form, so we must accept either form when deciding which symlinks to check.
fn reject_escaping_symlink(
    req_id: &str,
    original_path: &Path,
    candidate: &Path,
    resolved_root: &Path,
    raw_root: &Path,
) -> Result<(), crate::protocol::Response> {
    let mut current = PathBuf::new();

    for component in candidate.components() {
        current.push(component);

        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            continue;
        };

        if !metadata.file_type().is_symlink() {
            continue;
        }

        // Only check symlinks that live inside the project root. This skips
        // OS-level prefix symlinks (macOS /var → /private/var) that are not
        // inside our project directory and whose "escaping" is harmless.
        //
        // We compare against BOTH the canonicalized root (resolved_root, e.g.
        // /private/var/.../project) AND the raw root (e.g. /var/.../project)
        // because tempdir() returns raw paths while fs::canonicalize returns
        // the resolved form — and our `current` may be in either form.
        let inside_root = current.starts_with(resolved_root) || current.starts_with(raw_root);
        if !inside_root {
            continue;
        }

        iterative_follow_chain(req_id, original_path, &current, resolved_root)?;
    }

    Ok(())
}

/// Iteratively follow a symlink chain from `link` and reject if any hop's
/// resolved target is outside `resolved_root`. Depth-capped at 40 hops.
fn iterative_follow_chain(
    req_id: &str,
    original_path: &Path,
    start: &Path,
    resolved_root: &Path,
) -> Result<(), crate::protocol::Response> {
    let mut link = start.to_path_buf();
    let mut depth = 0usize;

    loop {
        if depth > 40 {
            return Err(path_error_response(req_id, original_path, resolved_root));
        }

        let target = match std::fs::read_link(&link) {
            Ok(t) => t,
            Err(_) => {
                // Can't read the link — treat as escaping to be safe.
                return Err(path_error_response(req_id, original_path, resolved_root));
            }
        };

        let resolved_target = if target.is_absolute() {
            normalize_path(&target)
        } else {
            let parent = link.parent().unwrap_or_else(|| Path::new(""));
            normalize_path(&parent.join(&target))
        };

        // Check boundary: use canonicalized target when available (handles
        // macOS /var → /private/var aliasing), fall back to the normalized
        // path when canonicalize fails (e.g. broken symlink on Linux).
        let canonical_target =
            std::fs::canonicalize(&resolved_target).unwrap_or_else(|_| resolved_target.clone());

        if !canonical_target.starts_with(resolved_root)
            && !resolved_target.starts_with(resolved_root)
        {
            return Err(path_error_response(req_id, original_path, resolved_root));
        }

        // If the target is itself a symlink, follow the next hop.
        match std::fs::symlink_metadata(&resolved_target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                link = resolved_target;
                depth += 1;
            }
            _ => break, // Non-symlink or non-existent target — chain ends here.
        }
    }

    Ok(())
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
    progress_sender: SharedProgressSender,
    bash_background: BgTaskRegistry,
}

impl AppContext {
    pub fn new(provider: Box<dyn LanguageProvider>, config: Config) -> Self {
        let progress_sender = Arc::new(Mutex::new(None));
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
            progress_sender: Arc::clone(&progress_sender),
            bash_background: BgTaskRegistry::new(progress_sender),
        }
    }

    pub fn set_progress_sender(&self, sender: Option<ProgressSender>) {
        if let Ok(mut progress_sender) = self.progress_sender.lock() {
            *progress_sender = sender;
        }
    }

    pub fn emit_progress(&self, frame: ProgressFrame) {
        let Ok(progress_sender) = self.progress_sender.lock().map(|sender| sender.clone()) else {
            return;
        };
        if let Some(sender) = progress_sender.as_ref() {
            sender(PushFrame::Progress(frame));
        }
    }

    pub fn bash_background(&self) -> &BgTaskRegistry {
        &self.bash_background
    }

    pub fn drain_bg_completions(&self) -> Vec<BgCompletion> {
        self.bash_background.drain_completions()
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
    pub fn semantic_embedding_model(
        &self,
    ) -> &RefCell<Option<crate::semantic_index::EmbeddingModel>> {
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
            let config = self.config();
            if let Err(e) = lsp.notify_file_changed(file_path, content, &config) {
                log::warn!("sync error for {}: {}", file_path.display(), e);
            }
        }
    }

    /// Notify LSP and optionally wait for diagnostics.
    ///
    /// Call this after `write_format_validate` when the request has `"diagnostics": true`.
    /// Sends didChange to the server, waits briefly for publishDiagnostics, and returns
    /// any diagnostics for the file. If no server is running, returns empty immediately.
    ///
    /// v0.17.3: this is the version-aware path. Pre-edit cached diagnostics
    /// are NEVER returned — only entries whose `version` matches the
    /// post-edit document version (or, for unversioned servers, whose
    /// `epoch` advanced past the pre-edit snapshot).
    pub fn lsp_notify_and_collect_diagnostics(
        &self,
        file_path: &Path,
        content: &str,
        timeout: std::time::Duration,
    ) -> crate::lsp::manager::PostEditWaitOutcome {
        let Ok(mut lsp) = self.lsp_manager.try_borrow_mut() else {
            return crate::lsp::manager::PostEditWaitOutcome::default();
        };

        // Clear any queued notifications before this write so the wait loop only
        // observes diagnostics triggered by the current change.
        lsp.drain_events();

        // Snapshot per-server epochs BEFORE sending didChange so the wait
        // loop can detect freshness via epoch-delta for servers that don't
        // echo `version` on publishDiagnostics.
        let pre_snapshot = lsp.snapshot_diagnostic_epochs(file_path);

        // Send didChange/didOpen and capture per-server target version.
        let config = self.config();
        let expected_versions = match lsp.notify_file_changed_versioned(file_path, content, &config)
        {
            Ok(v) => v,
            Err(e) => {
                log::warn!("sync error for {}: {}", file_path.display(), e);
                return crate::lsp::manager::PostEditWaitOutcome::default();
            }
        };

        // No server matched this file — return an empty outcome that's
        // honestly `complete: true` (nothing to wait for).
        if expected_versions.is_empty() {
            return crate::lsp::manager::PostEditWaitOutcome::default();
        }

        lsp.wait_for_post_edit_diagnostics(
            file_path,
            &config,
            &expected_versions,
            &pre_snapshot,
            timeout,
        )
    }

    /// Collect custom server root_markers from user config for use in
    /// `is_config_file_path_with_custom` checks (#25).
    fn custom_lsp_root_markers(&self) -> Vec<String> {
        self.config()
            .lsp_servers
            .iter()
            .flat_map(|s| s.root_markers.iter().cloned())
            .collect()
    }

    fn notify_watched_config_files(&self, file_paths: &[PathBuf]) {
        let custom_markers = self.custom_lsp_root_markers();
        let config_paths: Vec<(PathBuf, FileChangeType)> = file_paths
            .iter()
            .filter(|path| is_config_file_path_with_custom(path, &custom_markers))
            .cloned()
            .map(|path| {
                let change_type = if path.exists() {
                    FileChangeType::CHANGED
                } else {
                    FileChangeType::DELETED
                };
                (path, change_type)
            })
            .collect();

        self.notify_watched_config_events(&config_paths);
    }

    fn multi_file_write_paths(params: &serde_json::Value) -> Option<Vec<PathBuf>> {
        let paths = params
            .get("multi_file_write_paths")
            .and_then(|value| value.as_array())?
            .iter()
            .filter_map(|value| value.as_str())
            .map(PathBuf::from)
            .collect::<Vec<_>>();

        (!paths.is_empty()).then_some(paths)
    }

    /// Parse config-file watched events from `multi_file_write_paths` when the
    /// array contains object entries `{ "path": "...", "type": "created|changed|deleted" }`.
    ///
    /// This handles the OBJECT variant of `multi_file_write_paths`. The STRING
    /// variant (bare path strings) is handled by `multi_file_write_paths()` and
    /// `notify_watched_config_files()`. Both variants read the same JSON key but
    /// with different per-entry schemas — they are NOT redundant.
    ///
    /// #18 note: in older code this function also existed alongside `multi_file_write_paths()`
    /// and was reachable via the `else if` branch when all entries were objects.
    /// Restoring both is correct.
    fn watched_file_events_from_params(
        params: &serde_json::Value,
        extra_markers: &[String],
    ) -> Option<Vec<(PathBuf, FileChangeType)>> {
        let events = params
            .get("multi_file_write_paths")
            .and_then(|value| value.as_array())?
            .iter()
            .filter_map(|entry| {
                // Only handle object entries — string entries go through multi_file_write_paths()
                let path = entry
                    .get("path")
                    .and_then(|value| value.as_str())
                    .map(PathBuf::from)?;

                if !is_config_file_path_with_custom(&path, extra_markers) {
                    return None;
                }

                let change_type = entry
                    .get("type")
                    .and_then(|value| value.as_str())
                    .and_then(Self::parse_file_change_type)
                    .unwrap_or_else(|| Self::change_type_from_current_state(&path));

                Some((path, change_type))
            })
            .collect::<Vec<_>>();

        (!events.is_empty()).then_some(events)
    }

    fn parse_file_change_type(value: &str) -> Option<FileChangeType> {
        match value {
            "created" | "CREATED" | "Created" => Some(FileChangeType::CREATED),
            "changed" | "CHANGED" | "Changed" => Some(FileChangeType::CHANGED),
            "deleted" | "DELETED" | "Deleted" => Some(FileChangeType::DELETED),
            _ => None,
        }
    }

    fn change_type_from_current_state(path: &Path) -> FileChangeType {
        if path.exists() {
            FileChangeType::CHANGED
        } else {
            FileChangeType::DELETED
        }
    }

    fn notify_watched_config_events(&self, config_paths: &[(PathBuf, FileChangeType)]) {
        if config_paths.is_empty() {
            return;
        }

        if let Ok(mut lsp) = self.lsp_manager.try_borrow_mut() {
            let config = self.config();
            if let Err(e) = lsp.notify_files_watched_changed(config_paths, &config) {
                log::warn!("watched-file sync error: {}", e);
            }
        }
    }

    pub fn lsp_notify_watched_config_file(&self, file_path: &Path, change_type: FileChangeType) {
        let custom_markers = self.custom_lsp_root_markers();
        if !is_config_file_path_with_custom(file_path, &custom_markers) {
            return;
        }

        self.notify_watched_config_events(&[(file_path.to_path_buf(), change_type)]);
    }

    /// Post-write LSP hook for multi-file edits. When the patch includes
    /// config-file edits, notify active workspace servers via
    /// `workspace/didChangeWatchedFiles` before sending the per-document
    /// didOpen/didChange for the current file.
    pub fn lsp_post_multi_file_write(
        &self,
        file_path: &Path,
        content: &str,
        file_paths: &[PathBuf],
        params: &serde_json::Value,
    ) -> Option<crate::lsp::manager::PostEditWaitOutcome> {
        self.notify_watched_config_files(file_paths);

        let wants_diagnostics = params
            .get("diagnostics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !wants_diagnostics {
            self.lsp_notify_file_changed(file_path, content);
            return None;
        }

        let wait_ms = params
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(3000)
            .min(10_000);

        Some(self.lsp_notify_and_collect_diagnostics(
            file_path,
            content,
            std::time::Duration::from_millis(wait_ms),
        ))
    }

    /// Post-write LSP hook: notify server and optionally collect diagnostics.
    ///
    /// This is the single call site for all command handlers after `write_format_validate`.
    /// Behavior:
    /// - When `diagnostics: true` is in `params`, notifies the server, waits
    ///   until matching diagnostics arrive or the timeout expires, and returns
    ///   `Some(outcome)` with the verified-fresh diagnostics + per-server
    ///   status.
    /// - When `diagnostics: false` (or absent), just notifies (fire-and-forget)
    ///   and returns `None`. Callers must NOT wrap this in `Some(...)`; the
    ///   `None` is what tells the response builder to omit the LSP fields
    ///   entirely (preserves the no-diagnostics-requested response shape).
    ///
    /// v0.17.3: default `wait_ms` raised from 1500 to 3000 because real-world
    /// tsserver re-analysis on monorepo files routinely takes 2-5s. Still
    /// capped at 10000ms.
    pub fn lsp_post_write(
        &self,
        file_path: &Path,
        content: &str,
        params: &serde_json::Value,
    ) -> Option<crate::lsp::manager::PostEditWaitOutcome> {
        let wants_diagnostics = params
            .get("diagnostics")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let custom_markers = self.custom_lsp_root_markers();

        if !wants_diagnostics {
            if let Some(file_paths) = Self::multi_file_write_paths(params) {
                self.notify_watched_config_files(&file_paths);
            } else if let Some(config_events) =
                Self::watched_file_events_from_params(params, &custom_markers)
            {
                self.notify_watched_config_events(&config_events);
            }
            self.lsp_notify_file_changed(file_path, content);
            return None;
        }

        let wait_ms = params
            .get("wait_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(3000)
            .min(10_000); // Cap at 10 seconds to prevent hangs from adversarial input

        if let Some(file_paths) = Self::multi_file_write_paths(params) {
            return self.lsp_post_multi_file_write(file_path, content, &file_paths, params);
        }

        if let Some(config_events) = Self::watched_file_events_from_params(params, &custom_markers)
        {
            self.notify_watched_config_events(&config_events);
        }

        Some(self.lsp_notify_and_collect_diagnostics(
            file_path,
            content,
            std::time::Duration::from_millis(wait_ms),
        ))
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

        // Keep the raw root for symlink-guard comparisons. On macOS, tempdir()
        // returns /var/... paths while canonicalize gives /private/var/...; we
        // need both forms so reject_escaping_symlink can recognise in-root
        // symlinks regardless of which prefix form `current` happens to have.
        let raw_root = root.clone();
        let resolved_root = std::fs::canonicalize(&root).unwrap_or(root);

        // Resolve the path (follow symlinks, normalize ..). If canonicalization
        // fails (e.g. path does not exist or traverses a broken symlink), inspect
        // every existing component with lstat before falling back lexically so a
        // broken in-root symlink cannot be used to write outside project_root.
        let resolved = match std::fs::canonicalize(path) {
            Ok(resolved) => resolved,
            Err(_) => {
                let normalized = normalize_path(path);
                reject_escaping_symlink(req_id, path, &normalized, &resolved_root, &raw_root)?;
                resolve_with_existing_ancestors(&normalized)
            }
        };

        if !resolved.starts_with(&resolved_root) {
            return Err(path_error_response(req_id, path, &resolved_root));
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
