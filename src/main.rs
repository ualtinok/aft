use std::io::{self, BufRead, BufWriter, Write};

use aft::language::LanguageProvider;
use aft::parser::TreeSitterProvider;
use aft::protocol::{EchoParams, RawRequest, Response};

fn main() {
    eprintln!("[aft] started, pid {}", std::process::id());

    let provider = TreeSitterProvider::new();

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
            Ok(req) => dispatch(req, &provider),
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

fn dispatch(req: RawRequest, provider: &dyn LanguageProvider) -> Response {
    match req.command.as_str() {
        "ping" => Response::success(&req.id, serde_json::json!({ "command": "pong" })),
        "version" => Response::success(&req.id, serde_json::json!({ "version": "0.1.0" })),
        "echo" => handle_echo(&req),
        "outline" => aft::commands::outline::handle_outline(&req, provider),
        "zoom" => aft::commands::zoom::handle_zoom(&req, provider),
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

fn write_response(writer: &mut BufWriter<io::StdoutLock>, response: &Response) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, response)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}
