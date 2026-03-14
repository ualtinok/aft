use std::collections::HashSet;
use std::io::{self, BufRead, BufWriter, Write};

use aft::config::Config;
use aft::context::AppContext;
use aft::parser::TreeSitterProvider;
use aft::protocol::{EchoParams, RawRequest, Response};

fn main() {
    eprintln!("[aft] started, pid {}", std::process::id());

    let ctx = AppContext::new(Box::new(TreeSitterProvider::new()), Config::default());

    let stdin = io::stdin();
    let reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[aft] stdin read error: {}", e);
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<RawRequest>(trimmed) {
            Ok(req) => {
                drain_watcher_events(&ctx);
                dispatch(req, &ctx)
            }
            Err(e) => {
                eprintln!("[aft] parse error: {} — input: {}", e, trimmed);
                Response::error(
                    "_parse_error",
                    "parse_error",
                    format!("failed to parse request: {}", e),
                )
            }
        };

        if let Err(e) = write_response(&mut writer, &response) {
            eprintln!("[aft] stdout write error: {}", e);
            break;
        }
    }

    eprintln!("[aft] stdin closed, shutting down");
}

fn dispatch(req: RawRequest, ctx: &AppContext) -> Response {
    match req.command.as_str() {
        "ping" => Response::success(&req.id, serde_json::json!({ "command": "pong" })),
        "version" => Response::success(&req.id, serde_json::json!({ "version": "0.1.0" })),
        "echo" => handle_echo(&req),
        "outline" => aft::commands::outline::handle_outline(&req, ctx),
        "zoom" => aft::commands::zoom::handle_zoom(&req, ctx),
        "undo" => aft::commands::undo::handle_undo(&req, ctx),
        "edit_history" => aft::commands::edit_history::handle_edit_history(&req, ctx),
        "checkpoint" => aft::commands::checkpoint::handle_checkpoint(&req, ctx),
        "restore_checkpoint" => aft::commands::restore_checkpoint::handle_restore_checkpoint(&req, ctx),
        "list_checkpoints" => aft::commands::list_checkpoints::handle_list_checkpoints(&req, ctx),
        "write" => aft::commands::write::handle_write(&req, ctx),
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
        "call_tree" => aft::commands::call_tree::handle_call_tree(&req, ctx),
        "callers" => aft::commands::callers::handle_callers(&req, ctx),
        "trace_to" => aft::commands::trace_to::handle_trace_to(&req, ctx),
        // Test-only: populate the backup store through the protocol (no write/edit_symbol yet)
        "snapshot" => handle_snapshot(&req, ctx),
        _ => {
            eprintln!("[aft] unknown command: {}", req.command);
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
        Ok(params) => {
            Response::success(&req.id, serde_json::json!({ "message": params.message }))
        }
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
        Ok(id) => Response::success(
            &req.id,
            serde_json::json!({ "backup_id": id }),
        ),
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
    // Phase 1: collect changed paths from the receiver
    let changed: HashSet<std::path::PathBuf> = {
        let rx_ref = ctx.watcher_rx().borrow();
        let rx = match rx_ref.as_ref() {
            Some(rx) => rx,
            None => return, // No watcher configured
        };

        let mut paths = HashSet::new();
        while let Ok(event_result) = rx.try_recv() {
            if let Ok(event) = event_result {
                for path in event.paths {
                    // Filter to supported source extensions
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if SOURCE_EXTENSIONS.contains(&ext) {
                            paths.insert(path);
                        }
                    }
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
            graph.invalidate_file(path);
        }
    }

    eprintln!("[aft] invalidated {} files", changed.len());
}
