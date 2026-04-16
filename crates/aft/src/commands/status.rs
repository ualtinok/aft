//! AFT status command — returns the current state of indexes, features, and configuration.

use crate::context::AppContext;
use crate::context::SemanticIndexStatus;
use crate::protocol::{RawRequest, Response};

pub fn handle_status(req: &RawRequest, ctx: &AppContext) -> Response {
    let config = ctx.config();

    // Search index status
    let search_index_info = {
        let index = ctx.search_index().borrow();
        match index.as_ref() {
            Some(idx) if idx.ready => {
                let file_count = idx.file_count();
                let trigram_count = idx.trigram_count();
                serde_json::json!({
                    "status": "ready",
                    "files": file_count,
                    "trigrams": trigram_count,
                })
            }
            Some(_) => serde_json::json!({ "status": "building" }),
            None => {
                let status = if ctx.config().experimental_search_index {
                    "loading"
                } else {
                    "disabled"
                };
                serde_json::json!({ "status": status })
            }
        }
    };

    // Semantic index status
    let semantic_index_info = {
        let index = ctx.semantic_index().borrow();
        match index.as_ref() {
            Some(idx) => {
                serde_json::json!({
                    "status": idx.status_label(),
                    "entries": idx.entry_count(),
                    "dimension": idx.dimension(),
                    "backend": idx.backend_label().unwrap_or(config.semantic_backend_label()),
                    "model": idx.model_label().unwrap_or(config.semantic.model.as_str()),
                })
            }
            None => {
                match &*ctx.semantic_index_status().borrow() {
                    SemanticIndexStatus::Disabled => serde_json::json!({
                        "status": "disabled",
                        "backend": config.semantic_backend_label(),
                        "model": config.semantic.model.as_str(),
                    }),
                    SemanticIndexStatus::Building {
                        stage,
                        files,
                        entries_done,
                        entries_total,
                    } => serde_json::json!({
                        "status": "loading",
                        "stage": stage,
                        "files": files,
                        "entries_done": entries_done,
                        "entries_total": entries_total,
                        "backend": config.semantic_backend_label(),
                        "model": config.semantic.model.as_str(),
                    }),
                    SemanticIndexStatus::Ready => serde_json::json!({
                        "status": "ready",
                        "backend": config.semantic_backend_label(),
                        "model": config.semantic.model.as_str(),
                    }),
                    SemanticIndexStatus::Failed(error) => serde_json::json!({
                        "status": "failed",
                        "error": error,
                        "backend": config.semantic_backend_label(),
                        "model": config.semantic.model.as_str(),
                    }),
                }
            }
        }
    };

    // Disk cache sizes
    let storage_dir = config.storage_dir.as_ref().map(|d| d.display().to_string());
    let disk_info = if let Some(ref dir) = config.storage_dir {
        let trigram_size = dir_size(&dir.join("index"));
        let semantic_size = dir_size(&dir.join("semantic"));
        serde_json::json!({
            "storage_dir": dir.display().to_string(),
            "trigram_disk_bytes": trigram_size,
            "semantic_disk_bytes": semantic_size,
        })
    } else {
        serde_json::json!({
            "storage_dir": null,
            "trigram_disk_bytes": 0,
            "semantic_disk_bytes": 0,
        })
    };

    // LSP servers
    let lsp_count = ctx.lsp_server_count();

    // Symbol cache stats
    let symbol_cache_stats = ctx.symbol_cache_stats();

    Response::success(
        &req.id,
        serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "project_root": config.project_root.as_ref().map(|p| p.display().to_string()),
            "features": {
                "format_on_edit": config.format_on_edit,
                "validate_on_edit": config.validate_on_edit.as_deref().unwrap_or("off"),
                "restrict_to_project_root": config.restrict_to_project_root,
                "experimental_search_index": config.experimental_search_index,
                "experimental_semantic_search": config.experimental_semantic_search,
            },
            "search_index": search_index_info,
            "semantic_index": semantic_index_info,
            "disk": disk_info,
            "lsp_servers": lsp_count,
            "symbol_cache": symbol_cache_stats,
            "storage_dir": storage_dir,
        }),
    )
}

/// Recursively compute the total size of a directory.
fn dir_size(path: &std::path::Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    dir_size_recursive(path)
}

fn dir_size_recursive(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_file() {
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
        } else if ft.is_dir() {
            total += dir_size_recursive(&entry.path());
        }
    }
    total
}
