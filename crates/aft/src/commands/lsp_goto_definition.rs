use serde::Deserialize;

use lsp_types::request::GotoDefinition;
use lsp_types::{GotoDefinitionResponse, Location, LocationLink};

use crate::context::AppContext;
use crate::lsp::position::{build_text_document_position, lsp_range_to_aft, uri_to_path};
use crate::protocol::{RawRequest, Response};

#[derive(Debug, Deserialize)]
struct LspGotoDefinitionParams {
    file: String,
    line: u32,
    character: u32,
}

pub fn handle_lsp_goto_definition(req: &RawRequest, ctx: &AppContext) -> Response {
    let params = match serde_json::from_value::<LspGotoDefinitionParams>(req.params.clone()) {
        Ok(params) => params,
        Err(err) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("lsp_goto_definition: invalid params: {err}"),
            );
        }
    };

    if params.line == 0 {
        return Response::error(
            &req.id,
            "invalid_request",
            "lsp_goto_definition: 'line' must be >= 1",
        );
    }

    let file_path = std::path::Path::new(&params.file);

    let server_keys = {
        let mut lsp = ctx.lsp();
        match lsp.ensure_file_open(file_path) {
            Ok(keys) => keys,
            Err(err) => {
                return Response::error(
                    &req.id,
                    "lsp_error",
                    format!("lsp_goto_definition: failed to open file: {err}"),
                );
            }
        }
    };

    if server_keys.is_empty() {
        return Response::error(
            &req.id,
            "no_server",
            "lsp_goto_definition: no LSP server available for this file",
        );
    }

    let canonical_path = match std::fs::canonicalize(file_path) {
        Ok(path) => path,
        Err(err) => {
            return Response::error(
                &req.id,
                "lsp_error",
                format!("lsp_goto_definition: cannot canonicalize path: {err}"),
            );
        }
    };

    let position_params =
        match build_text_document_position(&canonical_path, params.line, params.character) {
            Ok(position) => position,
            Err(err) => {
                return Response::error(
                    &req.id,
                    "lsp_error",
                    format!("lsp_goto_definition: failed to build position: {err}"),
                );
            }
        };

    let goto_params = lsp_types::GotoDefinitionParams {
        text_document_position_params: position_params,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    ctx.lsp().drain_events();

    let result = {
        let mut lsp = ctx.lsp();
        let client = match lsp.client_for_file_mut(&canonical_path) {
            Some(client) => client,
            None => {
                return Response::error(
                    &req.id,
                    "no_server",
                    "lsp_goto_definition: no active LSP client for file",
                );
            }
        };
        client.send_request::<GotoDefinition>(goto_params)
    };

    match result {
        Ok(Some(response)) => {
            let definitions = match response {
                GotoDefinitionResponse::Scalar(location) => vec![location_to_json(&location)],
                GotoDefinitionResponse::Array(locations) => {
                    locations.iter().map(location_to_json).collect()
                }
                GotoDefinitionResponse::Link(links) => {
                    links.iter().map(location_link_to_json).collect()
                }
            };

            Response::success(&req.id, serde_json::json!({ "definitions": definitions }))
        }
        Ok(None) => Response::success(&req.id, serde_json::json!({ "definitions": [] })),
        Err(err) => Response::error(
            &req.id,
            "lsp_error",
            format!("lsp_goto_definition: request failed: {err}"),
        ),
    }
}

fn location_to_json(location: &Location) -> serde_json::Value {
    let (line, column, end_line, end_column) = lsp_range_to_aft(&location.range);
    let file = uri_to_path(&location.uri)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| location.uri.as_str().to_string());

    serde_json::json!({
        "file": file,
        "line": line,
        "column": column,
        "end_line": end_line,
        "end_column": end_column,
    })
}

fn location_link_to_json(link: &LocationLink) -> serde_json::Value {
    let (line, column, end_line, end_column) = lsp_range_to_aft(&link.target_selection_range);
    let file = uri_to_path(&link.target_uri)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| link.target_uri.as_str().to_string());

    serde_json::json!({
        "file": file,
        "line": line,
        "column": column,
        "end_line": end_line,
        "end_column": end_column,
    })
}
