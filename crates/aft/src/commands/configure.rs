use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crossbeam_channel::unbounded;
use notify::{RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use crate::callgraph::CallGraph;
use crate::config::{SemanticBackend, SemanticBackendConfig, UserServerDef};
use crate::context::{AppContext, SemanticIndexEvent, SemanticIndexStatus};
use crate::lsp::registry::{servers_for_file, ServerKind};
use crate::protocol::{RawRequest, Response};
use crate::search_index::{
    build_path_filters, current_git_head, resolve_cache_dir, walk_project_files, SearchIndex,
};
use crate::semantic_index::SemanticIndex;

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    normalized
}

fn validate_storage_dir(raw: &str) -> Result<PathBuf, String> {
    let storage_dir = PathBuf::from(raw);
    if !storage_dir.is_absolute() {
        return Err("configure: storage_dir must be an absolute path".to_string());
    }

    let normalized = normalize_absolute_path(&storage_dir);
    if normalized
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("configure: storage_dir must not escape via '..' traversal".to_string());
    }

    Ok(normalized)
}

fn parse_semantic_config(
    value: &serde_json::Value,
    current: &SemanticBackendConfig,
) -> Result<SemanticBackendConfig, String> {
    let Some(obj) = value.as_object() else {
        return Err("configure: semantic must be an object".to_string());
    };

    let mut semantic = current.clone();

    if let Some(raw) = obj.get("backend") {
        let name = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.backend must be a string".to_string())?
            .trim();
        semantic.backend = SemanticBackend::from_name(name)
            .ok_or_else(|| format!("configure: unsupported semantic.backend '{name}'"))?;
    }
    if let Some(raw) = obj.get("model") {
        semantic.model = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.model must be a string".to_string())?
            .trim()
            .to_string();
    }
    if let Some(raw) = obj.get("base_url") {
        let base_url = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.base_url must be a string".to_string())?
            .trim()
            .to_string();
        semantic.base_url = if base_url.is_empty() {
            None
        } else {
            Some(base_url)
        };
    }
    if let Some(raw) = obj.get("api_key_env") {
        let api_key_env = raw
            .as_str()
            .ok_or_else(|| "configure: semantic.api_key_env must be a string".to_string())?
            .trim()
            .to_string();
        semantic.api_key_env = if api_key_env.is_empty() {
            None
        } else {
            Some(api_key_env)
        };
    }
    if let Some(raw) = obj.get("timeout_ms") {
        let timeout_ms = raw.as_u64().ok_or_else(|| {
            "configure: semantic.timeout_ms must be an unsigned integer".to_string()
        })?;
        semantic.timeout_ms = timeout_ms;
    }
    if let Some(raw) = obj.get("max_batch_size") {
        let max_batch_size = raw.as_u64().ok_or_else(|| {
            "configure: semantic.max_batch_size must be an unsigned integer".to_string()
        })?;
        semantic.max_batch_size = usize::try_from(max_batch_size)
            .map_err(|_| "configure: semantic.max_batch_size is too large".to_string())?;
    }

    Ok(semantic)
}

fn parse_lsp_servers(value: &Value) -> Result<Vec<UserServerDef>, String> {
    let Some(entries) = value.as_array() else {
        return Err("configure: lsp_servers must be an array".to_string());
    };

    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| parse_lsp_server(entry, index))
        .collect()
}

fn parse_lsp_server(value: &Value, index: usize) -> Result<UserServerDef, String> {
    let Some(obj) = value.as_object() else {
        return Err(format!("configure: lsp_servers[{index}] must be an object"));
    };

    let id = required_string(obj.get("id"), index, "id")?;
    let extensions = required_string_array(obj.get("extensions"), index, "extensions")?;
    let binary = required_string(obj.get("binary"), index, "binary")?;
    let args = optional_string_array(obj.get("args"), index, "args")?;
    let root_markers = optional_string_array(obj.get("root_markers"), index, "root_markers")?;
    let env = parse_lsp_server_env(obj.get("env"), index)?;
    let initialization_options = obj.get("initialization_options").cloned();
    let disabled = obj
        .get("disabled")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                format!("configure: lsp_servers[{index}].disabled must be a boolean")
            })
        })
        .transpose()?
        .unwrap_or(false);

    Ok(UserServerDef {
        id,
        extensions,
        binary,
        args,
        root_markers,
        env,
        initialization_options,
        disabled,
    })
}

fn parse_lsp_server_env(
    value: Option<&Value>,
    index: usize,
) -> Result<HashMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let Some(obj) = value.as_object() else {
        return Err(format!(
            "configure: lsp_servers[{index}].env must be an object"
        ));
    };

    let mut env = HashMap::with_capacity(obj.len());
    for (key, value) in obj {
        let Some(value) = value.as_str() else {
            return Err(format!(
                "configure: lsp_servers[{index}].env.{key} must be a string"
            ));
        };
        env.insert(key.clone(), value.to_string());
    }
    Ok(env)
}

fn required_string(value: Option<&Value>, index: usize, field: &str) -> Result<String, String> {
    let raw = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("configure: lsp_servers[{index}].{field} must be a string"))?
        .trim();
    if raw.is_empty() {
        return Err(format!(
            "configure: lsp_servers[{index}].{field} must not be empty"
        ));
    }
    Ok(raw.to_string())
}

fn required_string_array(
    value: Option<&Value>,
    index: usize,
    field: &str,
) -> Result<Vec<String>, String> {
    let values = optional_string_array(value, index, field)?;
    if values.is_empty() {
        return Err(format!(
            "configure: lsp_servers[{index}].{field} must not be empty"
        ));
    }
    Ok(values)
}

fn optional_string_array(
    value: Option<&Value>,
    index: usize,
    field: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(entries) = value.as_array() else {
        return Err(format!(
            "configure: lsp_servers[{index}].{field} must be an array of strings"
        ));
    };

    let mut values = Vec::with_capacity(entries.len());
    for (entry_index, entry) in entries.iter().enumerate() {
        let Some(raw) = entry.as_str() else {
            return Err(format!(
                "configure: lsp_servers[{index}].{field}[{entry_index}] must be a string"
            ));
        };
        values.push(raw.trim().trim_start_matches('.').to_string());
    }
    Ok(values)
}

fn parse_disabled_lsp(value: &Value) -> Result<std::collections::HashSet<String>, String> {
    let Some(entries) = value.as_array() else {
        return Err("configure: disabled_lsp must be an array of strings".to_string());
    };

    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            entry
                .as_str()
                .map(|value| value.to_ascii_lowercase())
                .ok_or_else(|| format!("configure: disabled_lsp[{index}] must be a string"))
        })
        .collect()
}

fn is_custom_server(kind: &ServerKind) -> bool {
    matches!(kind, ServerKind::Custom(_))
}

fn lsp_missing_hint(binary: &str) -> String {
    crate::format::install_hint(binary)
}

fn detect_missing_lsp_binaries(root_path: &Path, config: &crate::config::Config) -> Vec<Value> {
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();

    for file in crate::callgraph::walk_project_files(root_path) {
        for server in servers_for_file(&file, config) {
            if is_custom_server(&server.kind) || which::which(&server.binary).is_ok() {
                continue;
            }

            let key = (server.kind.id_str().to_string(), server.binary.clone());
            if seen.insert(key) {
                warnings.push(json!({
                    "kind": "lsp_binary_missing",
                    "server": server.binary,
                    "binary": server.binary,
                    "hint": lsp_missing_hint(&server.binary),
                }));
            }
        }
    }

    for server in &config.lsp_servers {
        if server.disabled || which::which(&server.binary).is_ok() {
            continue;
        }

        let key = (server.id.clone(), server.binary.clone());
        if seen.insert(key) {
            warnings.push(json!({
                "kind": "lsp_binary_missing",
                "server": server.id,
                "binary": server.binary,
                "hint": lsp_missing_hint(&server.binary),
            }));
        }
    }

    warnings.sort_by_key(|warning| warning.to_string());
    warnings
}

/// Handle a `configure` request.
///
/// Expects `project_root` (string, required) — absolute path to the project root.
/// Sets the project root on `Config`, initializes the `CallGraph` with that root,
/// spawns a file watcher for live invalidation, and returns success with the
/// configured path.
///
/// Stderr log: `[aft] project root set: <path>`
/// Stderr log: `[aft] watcher started: <path>`
pub fn handle_configure(req: &RawRequest, ctx: &AppContext) -> Response {
    let root = match req.params.get("project_root").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "configure: missing required param 'project_root'",
            );
        }
    };

    let root_path = PathBuf::from(root);
    if !root_path.is_dir() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("configure: project_root is not a directory: {}", root),
        );
    }

    let previous_project_root = ctx.config().project_root.clone();
    if previous_project_root.as_ref() != Some(&root_path) {
        crate::format::clear_tool_cache();
    }

    // Set project root on config
    ctx.config_mut().project_root = Some(root_path.clone());

    // Optional feature flags from plugin config
    // Optional feature flags from plugin config
    if let Some(v) = req.params.get("format_on_edit").and_then(|v| v.as_bool()) {
        ctx.config_mut().format_on_edit = v;
    }
    if let Some(raw) = req.params.get("validate_on_edit") {
        if let Some(v) = raw.as_bool() {
            ctx.config_mut().validate_on_edit = Some(if v { "syntax" } else { "off" }.to_string());
        } else if let Some(v) = raw.as_str() {
            let value = match v {
                "true" => "syntax",
                "false" => "off",
                other => other,
            };
            ctx.config_mut().validate_on_edit = Some(value.to_string());
        }
    }
    // Per-language formatter overrides: { "typescript": "biome", "python": "ruff" }
    if let Some(v) = req.params.get("formatter").and_then(|v| v.as_object()) {
        for (lang, tool) in v {
            if let Some(tool_str) = tool.as_str() {
                ctx.config_mut()
                    .formatter
                    .insert(lang.clone(), tool_str.to_string());
            }
        }
    }
    // Restrict file operations to project root (default: false)
    if let Some(v) = req
        .params
        .get("restrict_to_project_root")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().restrict_to_project_root = v;
    }
    // Per-language checker overrides: { "typescript": "tsc", "python": "pyright" }
    if let Some(v) = req.params.get("checker").and_then(|v| v.as_object()) {
        for (lang, tool) in v {
            if let Some(tool_str) = tool.as_str() {
                ctx.config_mut()
                    .checker
                    .insert(lang.clone(), tool_str.to_string());
            }
        }
    }

    if let Some(v) = req
        .params
        .get("experimental_search_index")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().experimental_search_index = v;
    }
    if let Some(v) = req
        .params
        .get("experimental_semantic_search")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().experimental_semantic_search = v;
    }
    if let Some(v) = req
        .params
        .get("experimental_lsp_ty")
        .and_then(|v| v.as_bool())
    {
        ctx.config_mut().experimental_lsp_ty = v;
    }
    if let Some(v) = req.params.get("lsp_servers") {
        let servers = match parse_lsp_servers(v) {
            Ok(servers) => servers,
            Err(error) => return Response::error(&req.id, "invalid_request", error),
        };
        ctx.config_mut().lsp_servers = servers;
    }
    if let Some(v) = req.params.get("disabled_lsp") {
        let disabled_lsp = match parse_disabled_lsp(v) {
            Ok(disabled_lsp) => disabled_lsp,
            Err(error) => return Response::error(&req.id, "invalid_request", error),
        };
        ctx.config_mut().disabled_lsp = disabled_lsp;
    }
    if let Some(v) = req
        .params
        .get("search_index_max_file_size")
        .and_then(|v| v.as_u64())
    {
        ctx.config_mut().search_index_max_file_size = v;
    }
    if let Some(v) = req.params.get("storage_dir").and_then(|v| v.as_str()) {
        let storage_dir = match validate_storage_dir(v) {
            Ok(path) => path,
            Err(error) => {
                return Response::error(&req.id, "invalid_request", error);
            }
        };
        ctx.config_mut().storage_dir = Some(storage_dir.clone());
        let ttl_hours = ctx.config().checkpoint_ttl_hours;
        ctx.backup()
            .borrow_mut()
            .set_storage_dir(storage_dir, ttl_hours);
    }
    if let Some(v) = req.params.get("semantic") {
        let current = ctx.config().semantic.clone();
        let semantic = match parse_semantic_config(v, &current) {
            Ok(config) => config,
            Err(error) => {
                return Response::error(&req.id, "invalid_request", error);
            }
        };
        ctx.config_mut().semantic = semantic;
    }
    if let Some(raw) = req.params.get("max_callgraph_files") {
        // Reject invalid values explicitly so user typos surface instead of
        // being silently swallowed (Oracle v0.15.1 review blocker).
        // Accepts: positive integers (u64).
        // Rejects: 0, negatives, non-integers, non-numbers.
        let parsed = raw.as_u64().filter(|v| *v >= 1);
        match parsed {
            Some(v) => ctx.config_mut().max_callgraph_files = v as usize,
            None => {
                return Response::error(
                    &req.id,
                    "invalid_request",
                    format!(
                        "max_callgraph_files must be a positive integer (>= 1); got {}",
                        raw
                    ),
                );
            }
        }
    }

    // Bounded count — `.take(max + 1)` short-circuits the `ignore::Walk`
    // iterator so huge roots don't pay full walk cost here. `saturating_add`
    // guards the pathological case where a user sets `max_callgraph_files`
    // to `usize::MAX`.
    let source_file_count = crate::callgraph::walk_project_files(&root_path)
        .take(ctx.config().max_callgraph_files.saturating_add(1))
        .count();
    let exceeds = source_file_count > ctx.config().max_callgraph_files;
    if exceeds {
        log::warn!(
            "[aft] project has >{} source files (max_callgraph_files={}). Call-graph operations (callers, trace_to, trace_data, impact) will be disabled. Open a specific subdirectory for call-graph features.",
            ctx.config().max_callgraph_files,
            ctx.config().max_callgraph_files
        );
    }

    let experimental_search_index = ctx.config().experimental_search_index;
    let experimental_semantic_search = ctx.config().experimental_semantic_search;
    let search_index_max_file_size = ctx.config().search_index_max_file_size;
    let semantic_config = ctx.config().semantic.clone();

    let search_build_in_progress = ctx.search_index_rx().borrow().is_some();
    let semantic_build_in_progress = ctx.semantic_index_rx().borrow().is_some();
    // Note: We intentionally only WARN on rapid reconfigure (rather than tracking
    // JoinHandles to cancel old threads) because:
    //   1. Old thread results are dropped when ctx.search_index_rx() is reset
    //   2. Atomic tempfile writes via std::fs::rename are race-safe (last writer wins)
    //   3. Only CPU is wasted; no correctness issue
    //   4. Tracking handles would add complexity for negligible benefit
    // If reconfigure rate becomes a real problem, switch to a single
    // generation-counter + cancellation-token pattern.
    if search_build_in_progress {
        log::warn!(
            "[aft] configure called while search index build is still in progress; previous build will continue detached"
        );
    }
    if semantic_build_in_progress {
        log::warn!(
            "[aft] configure called while semantic index build is still in progress; previous build will continue detached"
        );
    }

    *ctx.search_index().borrow_mut() = None;
    *ctx.search_index_rx().borrow_mut() = None;
    *ctx.semantic_index().borrow_mut() = None;
    *ctx.semantic_index_rx().borrow_mut() = None;
    *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Disabled;
    *ctx.semantic_embedding_model().borrow_mut() = None;

    let storage_dir = ctx.config().storage_dir.clone();

    if experimental_search_index {
        let cache_dir = resolve_cache_dir(&root_path, storage_dir.as_deref());
        let current_head = current_git_head(&root_path);
        let mut baseline = SearchIndex::read_from_disk(&cache_dir);

        if let Some(index) = baseline.as_mut() {
            if current_head.is_some() && index.stored_git_head() == current_head.as_deref() {
                *ctx.search_index().borrow_mut() = Some(index.clone());
            } else {
                index.set_ready(false);
                *ctx.search_index().borrow_mut() = Some(index.clone());
            }
        }

        let (tx, rx): (
            crossbeam_channel::Sender<(SearchIndex, crate::parser::SymbolCache)>,
            crossbeam_channel::Receiver<(SearchIndex, crate::parser::SymbolCache)>,
        ) = unbounded();
        *ctx.search_index_rx().borrow_mut() = Some(rx);

        let root_clone = root_path.clone();
        thread::spawn(move || {
            let index = SearchIndex::rebuild_or_refresh(
                &root_clone,
                search_index_max_file_size,
                current_head,
                baseline,
            );
            index.write_to_disk(&cache_dir, index.stored_git_head());

            // Pre-warm symbol cache from indexed files
            let mut symbol_cache = crate::parser::SymbolCache::new();
            let mut parser = crate::parser::FileParser::new();
            for file_entry in &index.files {
                if let Ok(mtime) = std::fs::metadata(&file_entry.path).and_then(|m| m.modified()) {
                    if let Ok(symbols) = parser.extract_symbols(&file_entry.path) {
                        symbol_cache.insert(file_entry.path.clone(), mtime, symbols);
                    }
                }
            }
            log::info!(
                "[aft] pre-warmed symbol cache: {} files",
                symbol_cache.len()
            );

            let _ = tx.send((index, symbol_cache));
        });
    }

    if experimental_semantic_search {
        *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Building {
            stage: "queued".to_string(),
            files: None,
            entries_done: None,
            entries_total: None,
        };
        let (tx, rx): (
            crossbeam_channel::Sender<SemanticIndexEvent>,
            crossbeam_channel::Receiver<SemanticIndexEvent>,
        ) = unbounded();
        *ctx.semantic_index_rx().borrow_mut() = Some(rx);

        let root_clone = root_path.clone();
        let semantic_storage = storage_dir.clone();
        let semantic_project_key = crate::search_index::project_cache_key(&root_path);
        let semantic_config = semantic_config.clone();
        let tx_progress = tx.clone();
        thread::spawn(move || {
            let build_result =
                catch_unwind(AssertUnwindSafe(|| -> Result<SemanticIndex, String> {
                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "initializing_embedding_model".to_string(),
                        files: None,
                        entries_done: None,
                        entries_total: None,
                    });
                    let mut model =
                        crate::semantic_index::EmbeddingModel::from_config(&semantic_config)?;
                    let fingerprint = model.fingerprint(&semantic_config)?;
                    let fingerprint_key = fingerprint.as_string();

                    if let Some(ref dir) = semantic_storage {
                        if let Some(cached) = SemanticIndex::read_from_disk(
                            dir,
                            &semantic_project_key,
                            Some(&fingerprint_key),
                        ) {
                            let stale_count = cached.count_stale_files();
                            if stale_count == 0 {
                                let _ = tx_progress.send(SemanticIndexEvent::Progress {
                                    stage: "loaded_cached_index".to_string(),
                                    files: None,
                                    entries_done: Some(cached.entry_count()),
                                    entries_total: Some(cached.entry_count()),
                                });
                                return Ok(cached);
                            }

                            log::info!(
                                "[aft] semantic index: {} stale files, rebuilding",
                                stale_count
                            );
                        }
                    }

                    let filters = build_path_filters(&[], &[]).unwrap_or_default();
                    let files = walk_project_files(&root_clone, &filters);
                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "scanned_project_files".to_string(),
                        files: Some(files.len()),
                        entries_done: None,
                        entries_total: None,
                    });

                    // Cap file count to prevent OOM on huge project roots (e.g., /home/user).
                    // fastembed model (~200MB) + embeddings + batch buffers can exceed memory
                    // on constrained systems when indexing tens of thousands of files.
                    const MAX_SEMANTIC_FILES: usize = 10_000;
                    if files.len() > MAX_SEMANTIC_FILES {
                        log::warn!(
                            "[aft] skipping semantic index: {} files exceeds limit of {}. \
                             Open a specific project directory instead of a large root.",
                            files.len(),
                            MAX_SEMANTIC_FILES
                        );
                        return Err(format!(
                            "too many files ({}) for semantic indexing (max {})",
                            files.len(),
                            MAX_SEMANTIC_FILES
                        ));
                    }

                    let mut embed = |texts: Vec<String>| model.embed(texts);

                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "extracting_symbols".to_string(),
                        files: Some(files.len()),
                        entries_done: None,
                        entries_total: None,
                    });
                    let mut progress = |done: usize, total: usize| {
                        let _ = tx_progress.send(SemanticIndexEvent::Progress {
                            stage: "embedding_symbols".to_string(),
                            files: Some(files.len()),
                            entries_done: Some(done),
                            entries_total: Some(total),
                        });
                    };
                    let index = SemanticIndex::build_with_progress(
                        &root_clone,
                        &files,
                        &mut embed,
                        semantic_config.max_batch_size.max(1),
                        &mut progress,
                    )?;
                    let mut index = index;
                    index.set_fingerprint(fingerprint);
                    log::info!(
                        "[aft] built semantic index: {} files, {} entries",
                        files.len(),
                        index.len()
                    );
                    let _ = tx_progress.send(SemanticIndexEvent::Progress {
                        stage: "persisting_index".to_string(),
                        files: Some(files.len()),
                        entries_done: Some(index.len()),
                        entries_total: Some(index.len()),
                    });

                    if let Some(ref dir) = semantic_storage {
                        index.write_to_disk(dir, &semantic_project_key);
                    }

                    Ok(index)
                }));

            let event = match build_result {
                Ok(Ok(index)) => SemanticIndexEvent::Ready(index),
                Ok(Err(error)) => {
                    log::warn!("[aft] failed to build semantic index: {}", error);
                    SemanticIndexEvent::Failed(error)
                }
                Err(_) => {
                    let error = "semantic index build panicked".to_string();
                    log::warn!("[aft] {}", error);
                    SemanticIndexEvent::Failed(error)
                }
            };

            let _ = tx.send(event);
        });
    }

    // Initialize call graph with the project root
    let graph = CallGraph::new(root_path.clone());
    *ctx.callgraph().borrow_mut() = Some(graph);

    // Drop old watcher/receiver before creating new ones (re-configure)
    *ctx.watcher().borrow_mut() = None;
    *ctx.watcher_rx().borrow_mut() = None;

    // Spawn file watcher for live invalidation
    let (tx, rx) = mpsc::channel();
    match notify::recommended_watcher(tx) {
        Ok(mut w) => {
            if let Err(e) = w.watch(&root_path, RecursiveMode::Recursive) {
                log::debug!(
                    "[aft] watcher watch error: {} — callers will work with stale data",
                    e
                );
            } else {
                log::info!("watcher started: {}", root_path.display());
            }
            *ctx.watcher().borrow_mut() = Some(w);
            *ctx.watcher_rx().borrow_mut() = Some(rx);
        }
        Err(e) => {
            log::debug!(
                "[aft] watcher init failed: {} — callers will work with stale data",
                e
            );
        }
    }

    log::info!("project root set: {}", root_path.display());

    let config_snapshot = ctx.config().clone();
    let mut warnings = crate::format::detect_missing_tools(&root_path, &config_snapshot)
        .into_iter()
        .map(|warning| json!(warning))
        .collect::<Vec<_>>();
    warnings.extend(detect_missing_lsp_binaries(&root_path, &config_snapshot));

    Response::success(
        &req.id,
        json!({
            "project_root": root_path.display().to_string(),
            "source_file_count": source_file_count,
            "source_file_count_exceeds_max": exceeds,
            "max_callgraph_files": config_snapshot.max_callgraph_files,
            "warnings": warnings,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::validate_storage_dir;
    use std::path::PathBuf;

    #[test]
    fn validate_storage_dir_requires_absolute_paths() {
        assert!(validate_storage_dir("relative/cache").is_err());
    }

    #[test]
    fn validate_storage_dir_normalizes_safe_parents() {
        let base = std::env::temp_dir();
        let path = base.join("aft-config-test").join("..").join("cache");
        assert_eq!(
            validate_storage_dir(path.to_str().unwrap()).unwrap(),
            base.join("cache")
        );
    }

    #[test]
    fn validate_storage_dir_rejects_relative_with_dotdot() {
        // Relative paths with .. are rejected (not absolute)
        assert!(validate_storage_dir("../../../etc/passwd").is_err());
    }

    #[test]
    fn validate_storage_dir_accepts_absolute_with_dotdot_that_normalizes() {
        // /../../cache normalizes to /cache which is a valid absolute path
        let mut path = PathBuf::from(std::path::MAIN_SEPARATOR.to_string());
        path.push("..");
        path.push("..");
        path.push("cache");
        assert!(validate_storage_dir(path.to_str().unwrap()).is_ok());
    }
}
