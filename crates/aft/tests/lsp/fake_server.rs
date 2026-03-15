use std::io::{self, BufReader, Write};

use aft::lsp::jsonrpc::{Notification, RequestId, ServerMessage};
use aft::lsp::transport::{read_message, write_notification};
use serde_json::{json, Value};

fn write_json_message(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let json = serde_json::to_string(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    write!(writer, "Content-Length: {}\r\n\r\n", json.len())?;
    writer.write_all(json.as_bytes())?;
    writer.flush()
}

fn write_response(writer: &mut impl Write, id: RequestId, result: Value) -> io::Result<()> {
    write_json_message(
        writer,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
    )
}

fn request_position(params: &Option<Value>) -> (u64, u64) {
    let line = params
        .as_ref()
        .and_then(|value| value.get("position"))
        .and_then(|value| value.get("line"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let character = params
        .as_ref()
        .and_then(|value| value.get("position"))
        .and_then(|value| value.get("character"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    (line, character)
}

fn request_document_uri(params: &Option<Value>) -> Value {
    params
        .as_ref()
        .and_then(|value| value.get("textDocument"))
        .and_then(|value| value.get("uri"))
        .cloned()
        .unwrap_or_else(|| Value::String("file:///unknown".to_string()))
}

fn request_include_declaration(params: &Option<Value>) -> bool {
    params
        .as_ref()
        .and_then(|value| value.get("context"))
        .and_then(|value| value.get("includeDeclaration"))
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

fn request_new_name(params: &Option<Value>) -> String {
    params
        .as_ref()
        .and_then(|value| value.get("newName"))
        .and_then(|value| value.as_str())
        .unwrap_or("renamed")
        .to_string()
}

fn request_document_uri_string(params: &Option<Value>) -> String {
    params
        .as_ref()
        .and_then(|value| value.get("textDocument"))
        .and_then(|value| value.get("uri"))
        .and_then(|value| value.as_str())
        .unwrap_or("file:///unknown")
        .to_string()
}

fn document_uri(params: &Option<Value>) -> Value {
    params
        .as_ref()
        .and_then(|value| value.get("textDocument"))
        .and_then(|value| value.get("uri"))
        .cloned()
        .unwrap_or_else(|| Value::String("file:///unknown".to_string()))
}

fn document_version(params: &Option<Value>) -> Value {
    params
        .as_ref()
        .and_then(|value| value.get("textDocument"))
        .and_then(|value| value.get("version"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn write_custom_notification(
    writer: &mut impl Write,
    method: &str,
    uri: Value,
    version: Value,
) -> io::Result<()> {
    write_notification(
        writer,
        &Notification::new(
            method,
            Some(json!({
                "uri": uri,
                "version": version,
            })),
        ),
    )
}

fn write_publish_diagnostics(
    writer: &mut impl Write,
    uri: Value,
    diagnostics: Value,
) -> io::Result<()> {
    write_notification(
        writer,
        &Notification::new(
            "textDocument/publishDiagnostics",
            Some(json!({
                "uri": uri,
                "diagnostics": diagnostics,
            })),
        ),
    )
}

fn opened_diagnostics() -> Value {
    json!([
        {
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 0, "character": 5 }
            },
            "severity": 1,
            "code": "E0001",
            "source": "fake-lsp",
            "message": "test diagnostic error"
        },
        {
            "range": {
                "start": { "line": 1, "character": 4 },
                "end": { "line": 1, "character": 10 }
            },
            "severity": 2,
            "source": "fake-lsp",
            "message": "test diagnostic warning"
        }
    ])
}

fn changed_diagnostics() -> Value {
    json!([
        {
            "range": {
                "start": { "line": 2, "character": 1 },
                "end": { "line": 2, "character": 8 }
            },
            "severity": 1,
            "code": "E0002",
            "source": "fake-lsp",
            "message": "test diagnostic after change"
        }
    ])
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    while let Some(message) = read_message(&mut reader)? {
        match message {
            ServerMessage::Request { id, method, params } => match method.as_str() {
                "initialize" => {
                    write_response(
                        &mut writer,
                        id,
                        json!({
                            "capabilities": {
                                "textDocumentSync": 1,
                                "hoverProvider": true,
                                "definitionProvider": true,
                                "referencesProvider": true,
                                "renameProvider": {
                                    "prepareProvider": true
                                }
                            },
                            "serverInfo": {
                                "name": "fake-lsp-server",
                                "version": "0.1.0",
                            }
                        }),
                    )?;
                }
                "shutdown" => {
                    write_response(&mut writer, id, Value::Null)?;
                }
                "textDocument/hover" => {
                    let (line, character) = request_position(&params);
                    if line == 0 && character == 0 {
                        write_response(
                            &mut writer,
                            id,
                            json!({
                                "contents": {
                                    "kind": "markdown",
                                    "value": "```typescript\nconst x: number\n```"
                                },
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 7 }
                                }
                            }),
                        )?;
                    } else {
                        write_response(&mut writer, id, Value::Null)?;
                    }
                }
                "textDocument/definition" => {
                    let uri = request_document_uri(&params);
                    write_response(
                        &mut writer,
                        id,
                        json!({
                            "uri": uri,
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": 0, "character": 10 }
                            }
                        }),
                    )?;
                }
                "textDocument/references" => {
                    let uri = request_document_uri(&params);
                    let include_declaration = request_include_declaration(&params);
                    let mut locations = vec![json!({
                        "uri": uri.clone(),
                        "range": {
                            "start": { "line": 2, "character": 0 },
                            "end": { "line": 2, "character": 5 }
                        }
                    })];
                    if include_declaration {
                        locations.insert(
                            0,
                            json!({
                                "uri": uri,
                                "range": {
                                    "start": { "line": 0, "character": 0 },
                                    "end": { "line": 0, "character": 5 }
                                }
                            }),
                        );
                    }
                    write_response(&mut writer, id, json!(locations))?;
                }
                "textDocument/prepareRename" => {
                    let (line, character) = request_position(&params);
                    if line == 0 && character == 4 {
                        write_response(
                            &mut writer,
                            id,
                            json!({
                                "range": {
                                    "start": { "line": 0, "character": 4 },
                                    "end": { "line": 0, "character": 9 }
                                },
                                "placeholder": "hello"
                            }),
                        )?;
                    } else {
                        write_response(&mut writer, id, Value::Null)?;
                    }
                }
                "textDocument/rename" => {
                    let uri_key = request_document_uri_string(&params);
                    let new_name = request_new_name(&params);
                    let edits = if new_name == "__force_failure__" {
                        vec![
                            json!({
                                "range": {
                                    "start": { "line": 0, "character": 4 },
                                    "end": { "line": 0, "character": 9 }
                                },
                                "newText": new_name
                            }),
                            json!({
                                "range": {
                                    "start": { "line": 99, "character": 0 },
                                    "end": { "line": 99, "character": 5 }
                                },
                                "newText": new_name
                            }),
                        ]
                    } else {
                        vec![
                            json!({
                                "range": {
                                    "start": { "line": 0, "character": 4 },
                                    "end": { "line": 0, "character": 9 }
                                },
                                "newText": new_name
                            }),
                            json!({
                                "range": {
                                    "start": { "line": 2, "character": 0 },
                                    "end": { "line": 2, "character": 5 }
                                },
                                "newText": new_name
                            }),
                        ]
                    };
                    let mut changes = serde_json::Map::new();
                    changes.insert(uri_key, Value::Array(edits));
                    write_response(
                        &mut writer,
                        id,
                        Value::Object(
                            [("changes".to_string(), Value::Object(changes))]
                                .into_iter()
                                .collect(),
                        ),
                    )?;
                }
                _ => {
                    write_json_message(
                        &mut writer,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": format!("method not found: {method}"),
                            }
                        }),
                    )?;
                }
            },
            ServerMessage::Notification { method, params } => match method.as_str() {
                "initialized" => {}
                "textDocument/didOpen" => {
                    let uri = document_uri(&params);
                    let version = document_version(&params);

                    write_custom_notification(
                        &mut writer,
                        "custom/documentOpened",
                        uri.clone(),
                        version,
                    )?;
                    write_publish_diagnostics(&mut writer, uri, opened_diagnostics())?;
                }
                "textDocument/didChange" => {
                    let uri = document_uri(&params);
                    let version = document_version(&params);
                    write_custom_notification(
                        &mut writer,
                        "custom/documentChanged",
                        uri.clone(),
                        version,
                    )?;
                    write_publish_diagnostics(&mut writer, uri, changed_diagnostics())?;
                }
                "textDocument/didClose" => {
                    let uri = document_uri(&params);
                    write_custom_notification(
                        &mut writer,
                        "custom/documentClosed",
                        uri.clone(),
                        Value::Null,
                    )?;
                    write_publish_diagnostics(&mut writer, uri, json!([]))?;
                }
                "exit" => break,
                _ => {}
            },
            ServerMessage::Response(_) => {}
        }
    }

    Ok(())
}
