use std::collections::BTreeMap;
use std::env;
use std::path::Path;

use serde::Deserialize;

use crate::context::{AppContext, SemanticIndexStatus};
use crate::protocol::{RawRequest, Response};
use crate::semantic_index::{
    EmbeddingModel, is_onnx_runtime_unavailable, SemanticResult,
};
use crate::symbols::SymbolKind;

const DEFAULT_TOP_K: usize = 10;
const MAX_TOP_K: usize = 100;

#[derive(Debug, Deserialize)]
struct SemanticSearchParams {
    query: String,
    #[serde(default = "default_top_k")]
    top_k: usize,
}

pub fn handle_semantic_search(req: &RawRequest, ctx: &AppContext) -> Response {
    let params = match serde_json::from_value::<SemanticSearchParams>(req.params.clone()) {
        Ok(params) => params,
        Err(error) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("semantic_search: invalid params: {error}"),
            );
        }
    };

    match &*ctx.semantic_index_status().borrow() {
        SemanticIndexStatus::Disabled => {
            return Response::success(
                &req.id,
                serde_json::json!({
                    "status": "disabled",
                    "text": "Semantic search is not enabled.",
                }),
            );
        }
        SemanticIndexStatus::Building {
            stage,
            files,
            entries_done,
            entries_total,
        } => {
            let mut detail = format!("Semantic index is still building (stage: {}).", stage);
            if let Some(files) = files {
                detail.push_str(&format!(" files: {}", files));
            }
            if let Some(entries_done) = entries_done {
                detail.push_str(&format!(" entries done: {}", entries_done));
            }
            if let Some(entries_total) = entries_total {
                detail.push_str(&format!(" / {}", entries_total));
            }
            return Response::success(
                &req.id,
                serde_json::json!({
                    "status": "building",
                    "text": detail,
                    "stage": stage,
                    "files": files,
                    "entries_done": entries_done,
                    "entries_total": entries_total,
                }),
            );
        }
        SemanticIndexStatus::Failed(error) => {
            return semantic_error_response(&req.id, error);
        }
        SemanticIndexStatus::Ready => {}
    }

    let query_vector = match embed_query(&params.query, ctx) {
        Ok(query_vector) => query_vector,
        Err(error) => return semantic_error_response(&req.id, &error),
    };

    let project_root = ctx
        .config()
        .project_root
        .clone()
        .unwrap_or_else(|| env::current_dir().unwrap_or_default());
    let project_root = std::fs::canonicalize(&project_root).unwrap_or(project_root);

    let results = {
        let semantic_index = ctx.semantic_index().borrow();
        let Some(index) = semantic_index.as_ref() else {
            return Response::success(
                &req.id,
                serde_json::json!({
                    "status": "not_ready",
                    "text": "Semantic index is not ready yet.",
                }),
            );
        };
        index.search(&query_vector, params.top_k.min(MAX_TOP_K))
    };

    // Filter out low-relevance results below the minimum score threshold
    const MIN_SCORE: f32 = 0.35;
    let results: Vec<SemanticResult> = results
        .into_iter()
        .filter(|r| r.score >= MIN_SCORE)
        .collect();

    *ctx.semantic_index_status().borrow_mut() = SemanticIndexStatus::Ready;

    Response::success(
        &req.id,
        serde_json::json!({
            "status": "ready",
            "text": format_semantic_text(&results, &project_root),
            "results": results.iter().map(result_to_json).collect::<Vec<_>>(),
        }),
    )
}

fn default_top_k() -> usize {
    DEFAULT_TOP_K
}

fn embed_query(query: &str, ctx: &AppContext) -> Result<Vec<f32>, String> {
    let mut model_ref = ctx.semantic_embedding_model().borrow_mut();
    let semantic_config = ctx.config().semantic.clone();

    if model_ref.is_none() {
        *model_ref = Some(EmbeddingModel::from_config(&semantic_config)?);
    }

    let model = model_ref
        .as_mut()
        .ok_or_else(|| "embedding model was not initialized".to_string())?;
    let embeddings = model
        .embed(vec![query.to_string()])
        .map_err(|error| format!("failed to embed query: {error}"))?;

    let query_vector = embeddings
        .first()
        .cloned()
        .ok_or_else(|| "embedding model returned no query vector".to_string())?;

    if let Some(index) = ctx.semantic_index().borrow().as_ref() {
        if index.dimension() != query_vector.len() {
            return Err(format!(
                "semantic embedding dimension mismatch: query backend returned {}, index expects {}. Rebuild the semantic index for the active backend/model.",
                query_vector.len(),
                index.dimension()
            ));
        }
    }

    Ok(query_vector)
}

fn semantic_error_response(request_id: &str, error: &str) -> Response {
    if is_onnx_runtime_unavailable(error) {
        return Response::error(
            request_id,
            "semantic_search_unavailable",
            format!("Semantic search unavailable: {error}"),
        );
    }

    Response::error(
        request_id,
        "semantic_search_failed",
        format!("semantic_search: {error}"),
    )
}

fn format_semantic_text(results: &[SemanticResult], project_root: &Path) -> String {
    if results.is_empty() {
        return "Found 0 semantic result(s). [index: ready]".to_string();
    }

    let mut groups: BTreeMap<String, Vec<&SemanticResult>> = BTreeMap::new();

    for result in results {
        let display_path = result
            .file
            .strip_prefix(project_root)
            .unwrap_or(&result.file)
            .display()
            .to_string();
        groups.entry(display_path).or_default().push(result);
    }

    let sections = groups
        .into_iter()
        .map(|(file, file_results)| {
            let mut section = file;

            for result in file_results {
                section.push_str(&format!(
                    "\n{} [{}] lines {}-{} score {:.3}",
                    result.name,
                    symbol_kind_label(&result.kind),
                    display_line_number(result.start_line),
                    display_line_number(result.end_line),
                    result.score
                ));

                if !result.snippet.trim().is_empty() {
                    for line in result.snippet.lines() {
                        section.push_str("\n    ");
                        section.push_str(line);
                    }
                }
            }

            section
        })
        .collect::<Vec<_>>();

    format!(
        "{}\n\nFound {} semantic result(s). [index: ready]",
        sections.join("\n\n"),
        results.len()
    )
}

fn result_to_json(result: &SemanticResult) -> serde_json::Value {
    serde_json::json!({
        "file": result.file.display().to_string(),
        "name": result.name,
        "kind": result.kind,
        "start_line": display_line_number(result.start_line),
        "end_line": display_line_number(result.end_line),
        "score": result.score,
        "snippet": result.snippet,
    })
}

fn display_line_number(line: u32) -> u32 {
    line.saturating_add(1)
}

fn symbol_kind_label(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::TypeAlias => "type_alias",
        SymbolKind::Variable => "variable",
        SymbolKind::Heading => "heading",
    }
}
