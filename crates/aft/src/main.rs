use std::collections::HashSet;
use std::io::{self, BufRead, BufWriter, Write};

use aft::config::Config;
use aft::context::{AppContext, SemanticIndexEvent, SemanticIndexStatus};
use aft::lsp::client::LspEvent;
use aft::parser::TreeSitterProvider;
use aft::protocol::{EchoParams, RawRequest, Response};

fn main() {
    // Handle --version flag before anything else
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("aft {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|buf, record| {
            use std::io::Write;
            let prefix = if record.target().starts_with("aft::lsp")
                || record.target().starts_with("aft_lsp")
            {
                "[aft-lsp]"
            } else {
                "[aft]"
            };
            writeln!(buf, "{} {}", prefix, record.args())
        })
        .init();

    log::info!("started, pid {}", std::process::id());

    let ctx = AppContext::new(Box::new(TreeSitterProvider::new()), Config::default());

    let stdin = io::stdin();
    let reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                log::error!("stdin read error: {}", e);
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<RawRequest>(trimmed) {
            Ok(req) => {
                // Drain search index FIRST so watcher events apply to the latest index.
                // If reversed, watcher updates applied to the old index would be lost
                // when the background-built index replaces it.
                drain_search_index_events(&ctx);
                drain_semantic_index_events(&ctx);
                drain_watcher_events(&ctx);
                drain_lsp_events(&ctx);
                dispatch(req, &ctx)
            }
            Err(e) => {
                log::error!("parse error: {} — input: {}", e, trimmed);
                Response::error(
                    "_parse_error",
                    "parse_error",
                    format!("failed to parse request: {}", e),
                )
            }
        };

        if let Err(e) = write_response(&mut writer, &response) {
            log::error!("stdout write error: {}", e);
            break;
        }
    }

    ctx.lsp().shutdown_all();
    log::info!("stdin closed, shutting down");
}

fn dispatch(req: RawRequest, ctx: &AppContext) -> Response {
    match req.command.as_str() {
        "ping" => Response::success(&req.id, serde_json::json!({ "command": "pong" })),
        "version" => Response::success(
            &req.id,
            serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
        ),
        "echo" => handle_echo(&req),
        "outline" => aft::commands::outline::handle_outline(&req, ctx),
        "zoom" => aft::commands::zoom::handle_zoom(&req, ctx),
        "read" => aft::commands::read::handle_read(&req, ctx),
        "undo" => aft::commands::undo::handle_undo(&req, ctx),
        "edit_history" => aft::commands::edit_history::handle_edit_history(&req, ctx),
        "checkpoint" => aft::commands::checkpoint::handle_checkpoint(&req, ctx),
        "restore_checkpoint" => {
            aft::commands::restore_checkpoint::handle_restore_checkpoint(&req, ctx)
        }
        "list_checkpoints" => aft::commands::list_checkpoints::handle_list_checkpoints(&req, ctx),
        "write" => aft::commands::write::handle_write(&req, ctx),
        "delete_file" => aft::commands::delete_file::handle_delete_file(&req, ctx),
        "move_file" => aft::commands::move_file::handle_move_file(&req, ctx),
        "edit_symbol" => aft::commands::edit_symbol::handle_edit_symbol(&req, ctx),
        "edit_match" => aft::commands::edit_match::handle_edit_match(&req, ctx),
        "batch" => aft::commands::batch::handle_batch(&req, ctx),
        "transaction" => aft::commands::transaction::handle_transaction(&req, ctx),
        "add_import" => aft::commands::add_import::handle_add_import(&req, ctx),
        "add_member" => aft::commands::add_member::handle_add_member(&req, ctx),
        "add_derive" => aft::commands::add_derive::handle_add_derive(&req, ctx),
        "add_decorator" => aft::commands::add_decorator::handle_add_decorator(&req, ctx),
        "add_struct_tags" => aft::commands::add_struct_tags::handle_add_struct_tags(&req, ctx),
        "wrap_try_catch" => aft::commands::wrap_try_catch::handle_wrap_try_catch(&req, ctx),
        "remove_import" => aft::commands::remove_import::handle_remove_import(&req, ctx),
        "organize_imports" => aft::commands::organize_imports::handle_organize_imports(&req, ctx),
        "configure" => aft::commands::configure::handle_configure(&req, ctx),
        "glob" => aft::commands::glob::handle_glob(&req, ctx),
        "grep" => aft::commands::grep::handle_grep(&req, ctx),
        "semantic_search" => aft::commands::semantic_search::handle_semantic_search(&req, ctx),
        "status" => aft::commands::status::handle_status(&req, ctx),
        "call_tree" => aft::commands::call_tree::handle_call_tree(&req, ctx),
        "callers" => aft::commands::callers::handle_callers(&req, ctx),
        "trace_to" => aft::commands::trace_to::handle_trace_to(&req, ctx),
        "impact" => aft::commands::impact::handle_impact(&req, ctx),
        "trace_data" => aft::commands::trace_data::handle_trace_data(&req, ctx),
        "move_symbol" => aft::commands::move_symbol::handle_move_symbol(&req, ctx),
        "extract_function" => aft::commands::extract_function::handle_extract_function(&req, ctx),
        "inline_symbol" => aft::commands::inline_symbol::handle_inline_symbol(&req, ctx),
        "git_conflicts" => aft::commands::conflicts::handle_git_conflicts(ctx, &req),
        "ast_search" => aft::commands::ast_search::handle_ast_search(&req, ctx),
        "ast_replace" => aft::commands::ast_replace::handle_ast_replace(&req, ctx),
        "lsp_diagnostics" => aft::commands::lsp_diagnostics::handle_lsp_diagnostics(&req, ctx),
        "lsp_hover" => aft::commands::lsp_hover::handle_lsp_hover(&req, ctx),
        "lsp_goto_definition" => {
            aft::commands::lsp_goto_definition::handle_lsp_goto_definition(&req, ctx)
        }
        "lsp_find_references" => {
            aft::commands::lsp_find_references::handle_lsp_find_references(&req, ctx)
        }
        "lsp_prepare_rename" => {
            aft::commands::lsp_prepare_rename::handle_lsp_prepare_rename(&req, ctx)
        }
        "lsp_rename" => aft::commands::lsp_rename::handle_lsp_rename(&req, ctx),
        // NOTE: "snapshot" must remain in the production binary because integration tests in
        // crates/aft/tests/integration/ spawn the compiled binary as a subprocess and send
        // "snapshot" commands through the stdin/stdout protocol. A #[cfg(test)] gate would
        // only affect unit-test compilation and would not exclude this arm from the binary
        // that integration tests execute. See: crates/aft/tests/integration/safety_test.rs
        "snapshot" => handle_snapshot(&req, ctx),
        _ => {
            log::warn!("unknown command: {}", req.command);
            Response::error(
                &req.id,
                "unknown_command",
                format!("unknown command: {}", req.command),
            )
        }
    }
}

fn handle_echo(req: &RawRequest) -> Response {
    match serde_json::from_value::<EchoParams>(req.params.clone()) {
        Ok(params) => Response::success(&req.id, serde_json::json!({ "message": params.message })),
        Err(e) => Response::error(
            &req.id,
            "invalid_request",
            format!("echo: invalid params: {}", e),
        ),
    }
}

/// Test-only command: snapshot a file into the backup store.
///
/// Params: `file` (string, required) — path to snapshot.
/// Returns: `{ backup_id }`.
fn handle_snapshot(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "snapshot: missing required param 'file'",
            );
        }
    };

    let path = std::path::Path::new(file);
    let mut backup = ctx.backup().borrow_mut();

    match backup.snapshot(path, "manual snapshot") {
        Ok(id) => Response::success(&req.id, serde_json::json!({ "backup_id": id })),
        Err(e) => Response::error(&req.id, e.code(), e.to_string()),
    }
}

fn write_response(writer: &mut BufWriter<io::StdoutLock>, response: &Response) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Source file extensions that the call graph supports.
const SOURCE_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "py", "rs", "go"];

/// Drain pending file watcher events and invalidate changed source files
/// in the call graph.
///
/// Borrows the watcher receiver and callgraph in separate phases to avoid
/// RefCell borrow conflicts. Events are deduplicated by PathBuf — notify
/// fires multiple events per file write (Create, Modify, etc.).
fn drain_watcher_events(ctx: &AppContext) {
    // Phase 1: collect changed paths from the receiver, filtering out
    // non-project directories that cause excessive invalidation noise.
    let changed: HashSet<std::path::PathBuf> = {
        let rx_ref = ctx.watcher_rx().borrow();
        let rx = match rx_ref.as_ref() {
            Some(rx) => rx,
            None => return, // No watcher configured
        };

        let mut paths = HashSet::new();
        while let Ok(event_result) = rx.try_recv() {
            if let Ok(event) = event_result {
                // Only process events that indicate actual file content changes.
                // Skip Access events — on Linux with atime enabled, reading a file
                // during update_file triggers an access event, creating a feedback loop.
                use notify::EventKind;
                match event.kind {
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
                    _ => continue,
                }
                for path in event.paths {
                    // Skip internal directories that generate frequent non-source events.
                    let path_str = path.to_string_lossy();
                    if path_str.contains("/.git/")
                        || path_str.contains("\\.git\\")
                        || path_str.contains("/.opencode/")
                        || path_str.contains("\\.opencode\\")
                        || path_str.contains("/node_modules/")
                        || path_str.contains("\\node_modules\\")
                        || path_str.contains("/target/")
                        || path_str.contains("\\target\\")
                    {
                        continue;
                    }
                    paths.insert(path);
                }
            }
        }
        paths
    }; // receiver borrow dropped here

    if changed.is_empty() {
        return;
    }

    // Phase 2: invalidate each changed file in the call graph
    let mut graph_ref = ctx.callgraph().borrow_mut();
    if let Some(graph) = graph_ref.as_mut() {
        for path in &changed {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if SOURCE_EXTENSIONS.contains(&ext) {
                    graph.invalidate_file(path);
                }
            }
        }
    }

    let mut index_ref = ctx.search_index().borrow_mut();
    if let Some(index) = index_ref.as_mut() {
        for path in &changed {
            if path.exists() {
                index.update_file(path);
            } else {
                index.remove_file(path);
            }
        }
    }

    let mut semantic_index_ref = ctx.semantic_index().borrow_mut();
    if let Some(index) = semantic_index_ref.as_mut() {
        for path in &changed {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if SOURCE_EXTENSIONS.contains(&ext) {
                    index.invalidate_file(path);
                }
            }
        }
    }

    log::info!("invalidated {} files", changed.len());
}

fn drain_search_index_events(ctx: &AppContext) {
    let latest = {
        let rx_ref = ctx.search_index_rx().borrow();
        let Some(rx) = rx_ref.as_ref() else {
            return;
        };

        let mut latest = None;
        while let Ok(pair) = rx.try_recv() {
            latest = Some(pair);
        }
        latest
    };

    if let Some((index, symbol_cache)) = latest {
        *ctx.search_index().borrow_mut() = Some(index);
        // Merge pre-warmed symbol cache into the provider's parser
        if symbol_cache.len() > 0 {
            if let Some(tsp) = ctx
                .provider()
                .as_any()
                .downcast_ref::<aft::parser::TreeSitterProvider>()
            {
                tsp.merge_warm_cache(symbol_cache);
            }
        }
    }
}

fn drain_semantic_index_events(ctx: &AppContext) {
    let events = {
        let rx_ref = ctx.semantic_index_rx().borrow();
        let Some(rx) = rx_ref.as_ref() else {
            return;
        };

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    };

    if events.is_empty() {
        return;
    }

    let mut keep_receiver = true;
    for event in events {
        match event {
            SemanticIndexEvent::Progress {
                stage,
                files,
                entries_done,
                entries_total,
            } => {
                *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Building {
                    stage,
                    files,
                    entries_done,
                    entries_total,
                };
            }
            SemanticIndexEvent::Ready(index) => {
                *ctx.semantic_index().borrow_mut() = Some(index);
                *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Ready;
                keep_receiver = false;
            }
            SemanticIndexEvent::Failed(error) => {
                *ctx.semantic_index().borrow_mut() = None;
                *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Failed(error);
                keep_receiver = false;
            }
        }
    }

    if !keep_receiver {
        *ctx.semantic_index_rx().borrow_mut() = None;
    }
}

fn drain_lsp_events(ctx: &AppContext) {
    let events = {
        let mut lsp = ctx.lsp();
        lsp.drain_events()
    };
    for event in events {
        match event {
            LspEvent::Notification {
                server_kind,
                root,
                method,
                params,
            } => {
                log::debug!(
                    "[aft-lsp] notification {:?} {} {} {}",
                    server_kind,
                    root.display(),
                    method,
                    params.unwrap_or(serde_json::Value::Null)
                );
            }
            LspEvent::ServerRequest {
                server_kind,
                root,
                id,
                method,
                params,
            } => {
                log::debug!(
                    "[aft-lsp] request {:?} {} {:?} {} {}",
                    server_kind,
                    root.display(),
                    id,
                    method,
                    params.unwrap_or(serde_json::Value::Null)
                );
            }
            LspEvent::ServerExited { server_kind, root } => {
                log::info!("exited {:?} {}", server_kind, root.display());
            }
        }
    }
}
