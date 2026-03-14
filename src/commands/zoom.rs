use std::path::Path;

use serde::Serialize;

use crate::context::AppContext;
use crate::parser::{FileParser, LangId};
use crate::protocol::{RawRequest, Response};
use crate::symbols::Range;

/// A reference to a called/calling function.
#[derive(Debug, Clone, Serialize)]
pub struct CallRef {
    pub name: String,
    pub line: u32,
}

/// Annotations describing file-scoped call relationships.
#[derive(Debug, Clone, Serialize)]
pub struct Annotations {
    pub calls_out: Vec<CallRef>,
    pub called_by: Vec<CallRef>,
}

/// Response payload for the zoom command.
#[derive(Debug, Clone, Serialize)]
pub struct ZoomResponse {
    pub name: String,
    pub kind: String,
    pub range: Range,
    pub content: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
    pub annotations: Annotations,
}

/// Handle a `zoom` request.
///
/// Expects `file`, `symbol` in request params, optional `context_lines` (default 3).
/// Resolves the symbol, extracts body + context, walks AST for call annotations.
pub fn handle_zoom(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "zoom: missing required param 'file'",
            );
        }
    };

    let symbol_name = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "zoom: missing required param 'symbol'",
            );
        }
    };

    let context_lines = req
        .params
        .get("context_lines")
        .and_then(|v| v.as_u64())
        .unwrap_or(3) as usize;

    let path = Path::new(file);
    if !path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("file not found: {}", file),
        );
    }

    // Resolve the target symbol
    let matches = match ctx.provider().resolve_symbol(path, symbol_name) {
        Ok(m) => m,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    if matches.len() > 1 {
        // Ambiguous — return qualified candidates
        let candidates: Vec<String> = matches
            .iter()
            .map(|m| {
                let sym = &m.symbol;
                if sym.scope_chain.is_empty() {
                    format!("{}:{}", sym.name, sym.range.start_line)
                } else {
                    format!(
                        "{}::{}:{}",
                        sym.scope_chain.join("::"),
                        sym.name,
                        sym.range.start_line
                    )
                }
            })
            .collect();
        return Response::error(
            &req.id,
            "ambiguous_symbol",
            format!(
                "symbol '{}' is ambiguous, candidates: [{}]",
                symbol_name,
                candidates.join(", ")
            ),
        );
    }

    let target = &matches[0].symbol;

    // Read source file
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Response::error(
                &req.id,
                "file_not_found",
                format!("{}: {}", file, e),
            );
        }
    };

    let lines: Vec<&str> = source.lines().collect();
    let start = target.range.start_line as usize;
    let end = target.range.end_line as usize;

    // Extract symbol body (0-based line indices)
    let content = if end < lines.len() {
        lines[start..=end].join("\n")
    } else {
        lines[start..].join("\n")
    };

    // Context before
    let ctx_start = start.saturating_sub(context_lines);
    let context_before: Vec<String> = if ctx_start < start {
        lines[ctx_start..start].iter().map(|l| l.to_string()).collect()
    } else {
        vec![]
    };

    // Context after
    let ctx_end = (end + 1 + context_lines).min(lines.len());
    let context_after: Vec<String> = if end + 1 < lines.len() {
        lines[(end + 1)..ctx_end].iter().map(|l| l.to_string()).collect()
    } else {
        vec![]
    };

    // Get all symbols in file for call matching
    let all_symbols = match ctx.provider().list_symbols(path) {
        Ok(s) => s,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    let known_names: Vec<&str> = all_symbols.iter().map(|s| s.name.as_str()).collect();

    // Parse AST for call extraction
    let mut parser = FileParser::new();
    let (tree, lang) = match parser.parse(path) {
        Ok(r) => r,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // calls_out: calls within the target symbol's byte range
    let target_byte_start = line_col_to_byte(&source, target.range.start_line, target.range.start_col);
    let target_byte_end = line_col_to_byte(&source, target.range.end_line, target.range.end_col);

    let raw_calls = extract_calls_in_range(&source, tree.root_node(), target_byte_start, target_byte_end, lang);
    let calls_out: Vec<CallRef> = raw_calls
        .into_iter()
        .filter(|(name, _)| known_names.contains(&name.as_str()) && *name != target.name)
        .map(|(name, line)| CallRef { name, line })
        .collect();

    // called_by: scan all other symbols for calls to this symbol
    let mut called_by: Vec<CallRef> = Vec::new();
    for sym in &all_symbols {
        if sym.name == target.name && sym.range.start_line == target.range.start_line {
            continue; // skip self
        }
        let sym_byte_start = line_col_to_byte(&source, sym.range.start_line, sym.range.start_col);
        let sym_byte_end = line_col_to_byte(&source, sym.range.end_line, sym.range.end_col);
        let calls = extract_calls_in_range(&source, tree.root_node(), sym_byte_start, sym_byte_end, lang);
        for (name, line) in calls {
            if name == target.name {
                called_by.push(CallRef {
                    name: sym.name.clone(),
                    line,
                });
            }
        }
    }

    // Dedup called_by by (name, line)
    called_by.sort_by(|a, b| a.name.cmp(&b.name).then(a.line.cmp(&b.line)));
    called_by.dedup_by(|a, b| a.name == b.name && a.line == b.line);

    let kind_str = serde_json::to_value(&target.kind)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", target.kind).to_lowercase());

    let resp = ZoomResponse {
        name: target.name.clone(),
        kind: kind_str,
        range: target.range.clone(),
        content,
        context_before,
        context_after,
        annotations: Annotations {
            calls_out,
            called_by,
        },
    };

    Response::success(&req.id, serde_json::to_value(&resp).unwrap())
}

/// Convert a 0-based line + column to a byte offset in the source.
fn line_col_to_byte(source: &str, line: u32, col: u32) -> usize {
    let mut byte = 0;
    for (i, l) in source.lines().enumerate() {
        if i == line as usize {
            return byte + (col as usize).min(l.len());
        }
        byte += l.len() + 1; // +1 for newline
    }
    source.len()
}

/// Extract call expression names within a byte range of the AST.
///
/// Delegates to `crate::calls::extract_calls_in_range`.
fn extract_calls_in_range(
    source: &str,
    root: tree_sitter::Node,
    byte_start: usize,
    byte_end: usize,
    lang: LangId,
) -> Vec<(String, u32)> {
    crate::calls::extract_calls_in_range(source, root, byte_start, byte_end, lang)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::context::AppContext;
    use crate::parser::TreeSitterProvider;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn make_ctx() -> AppContext {
        AppContext::new(Box::new(TreeSitterProvider::new()), Config::default())
    }

    // --- Call extraction tests ---

    #[test]
    fn extract_calls_finds_direct_calls() {
        let source = std::fs::read_to_string(fixture_path("calls.ts")).unwrap();
        let mut parser = FileParser::new();
        let path = fixture_path("calls.ts");
        let (tree, lang) = parser.parse(&path).unwrap();

        // `compute` calls `helper` — find compute's range from symbols
        let ctx = make_ctx();
        let symbols = ctx.provider().list_symbols(&path).unwrap();
        let compute = symbols.iter().find(|s| s.name == "compute").unwrap();

        let byte_start = line_col_to_byte(&source, compute.range.start_line, compute.range.start_col);
        let byte_end = line_col_to_byte(&source, compute.range.end_line, compute.range.end_col);

        let calls = extract_calls_in_range(&source, tree.root_node(), byte_start, byte_end, lang);
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();

        assert!(names.contains(&"helper"), "compute should call helper, got: {:?}", names);
    }

    #[test]
    fn extract_calls_finds_member_calls() {
        let source = std::fs::read_to_string(fixture_path("calls.ts")).unwrap();
        let mut parser = FileParser::new();
        let path = fixture_path("calls.ts");
        let (tree, lang) = parser.parse(&path).unwrap();

        let ctx = make_ctx();
        let symbols = ctx.provider().list_symbols(&path).unwrap();
        let run_all = symbols.iter().find(|s| s.name == "runAll").unwrap();

        let byte_start = line_col_to_byte(&source, run_all.range.start_line, run_all.range.start_col);
        let byte_end = line_col_to_byte(&source, run_all.range.end_line, run_all.range.end_col);

        let calls = extract_calls_in_range(&source, tree.root_node(), byte_start, byte_end, lang);
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();

        assert!(names.contains(&"add"), "runAll should call this.add, got: {:?}", names);
        assert!(names.contains(&"helper"), "runAll should call helper, got: {:?}", names);
    }

    #[test]
    fn extract_calls_unused_function_has_no_calls() {
        let source = std::fs::read_to_string(fixture_path("calls.ts")).unwrap();
        let mut parser = FileParser::new();
        let path = fixture_path("calls.ts");
        let (tree, lang) = parser.parse(&path).unwrap();

        let ctx = make_ctx();
        let symbols = ctx.provider().list_symbols(&path).unwrap();
        let unused = symbols.iter().find(|s| s.name == "unused").unwrap();

        let byte_start = line_col_to_byte(&source, unused.range.start_line, unused.range.start_col);
        let byte_end = line_col_to_byte(&source, unused.range.end_line, unused.range.end_col);

        let calls = extract_calls_in_range(&source, tree.root_node(), byte_start, byte_end, lang);
        // console.log is the only call, but "log" or "console" aren't known symbols
        let known_names = vec!["helper", "compute", "orchestrate", "unused", "format", "display"];
        let filtered: Vec<&str> = calls
            .iter()
            .map(|(n, _)| n.as_str())
            .filter(|n| known_names.contains(n))
            .collect();
        assert!(filtered.is_empty(), "unused should not call known symbols, got: {:?}", filtered);
    }

    // --- Context line tests ---

    #[test]
    fn context_lines_clamp_at_file_start() {
        // helper() is at the top of the file (line 2) — context_before should be clamped
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");
        let symbols = ctx.provider().list_symbols(&path).unwrap();
        let helper = symbols.iter().find(|s| s.name == "helper").unwrap();

        let source = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = source.lines().collect();
        let start = helper.range.start_line as usize;

        // With context_lines=5, ctx_start should clamp to 0
        let ctx_start = start.saturating_sub(5);
        let context_before: Vec<&str> = lines[ctx_start..start].to_vec();
        // Should have at most `start` lines (not panic)
        assert!(context_before.len() <= start);
    }

    #[test]
    fn context_lines_clamp_at_file_end() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");
        let symbols = ctx.provider().list_symbols(&path).unwrap();
        let display = symbols.iter().find(|s| s.name == "display").unwrap();

        let source = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = source.lines().collect();
        let end = display.range.end_line as usize;

        // With context_lines=20, should clamp to file length
        let ctx_end = (end + 1 + 20).min(lines.len());
        let context_after: Vec<&str> = if end + 1 < lines.len() {
            lines[(end + 1)..ctx_end].to_vec()
        } else {
            vec![]
        };
        // Should not panic regardless of context_lines size
        assert!(context_after.len() <= 20);
    }

    // --- Body extraction test ---

    #[test]
    fn body_extraction_matches_source() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");
        let symbols = ctx.provider().list_symbols(&path).unwrap();
        let compute = symbols.iter().find(|s| s.name == "compute").unwrap();

        let source = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = source.lines().collect();
        let start = compute.range.start_line as usize;
        let end = compute.range.end_line as usize;
        let body = lines[start..=end].join("\n");

        assert!(body.contains("function compute"), "body should contain function declaration");
        assert!(body.contains("helper(a)"), "body should contain call to helper");
        assert!(body.contains("doubled + b"), "body should contain return expression");
    }

    // --- Full zoom response tests ---

    #[test]
    fn zoom_response_has_calls_out_and_called_by() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");

        let req = make_zoom_request("z-1", path.to_str().unwrap(), "compute", None);
        let resp = handle_zoom(&req, &ctx);

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ok"], true, "zoom should succeed: {:?}", json);

        let calls_out = json["annotations"]["calls_out"]
            .as_array()
            .expect("calls_out array");
        let out_names: Vec<&str> = calls_out.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert!(out_names.contains(&"helper"), "compute calls helper: {:?}", out_names);

        let called_by = json["annotations"]["called_by"]
            .as_array()
            .expect("called_by array");
        let by_names: Vec<&str> = called_by.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert!(by_names.contains(&"orchestrate"), "orchestrate calls compute: {:?}", by_names);
    }

    #[test]
    fn zoom_response_empty_annotations_for_unused() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");

        let req = make_zoom_request("z-2", path.to_str().unwrap(), "unused", None);
        let resp = handle_zoom(&req, &ctx);

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ok"], true);

        let _calls_out = json["annotations"]["calls_out"].as_array().unwrap();
        let called_by = json["annotations"]["called_by"].as_array().unwrap();

        // calls_out exists (may contain console.log but no known symbols)
        // called_by should be empty — nobody calls unused
        assert!(called_by.is_empty(), "unused should not be called by anyone: {:?}", called_by);
    }

    #[test]
    fn zoom_symbol_not_found() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");

        let req = make_zoom_request("z-3", path.to_str().unwrap(), "nonexistent", None);
        let resp = handle_zoom(&req, &ctx);

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["code"], "symbol_not_found");
    }

    #[test]
    fn zoom_custom_context_lines() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");

        let req = make_zoom_request("z-4", path.to_str().unwrap(), "compute", Some(1));
        let resp = handle_zoom(&req, &ctx);

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ok"], true);

        let ctx_before = json["context_before"].as_array().unwrap();
        let ctx_after = json["context_after"].as_array().unwrap();
        // With context_lines=1, we get at most 1 line before and after
        assert!(ctx_before.len() <= 1, "context_before should be ≤1: {:?}", ctx_before);
        assert!(ctx_after.len() <= 1, "context_after should be ≤1: {:?}", ctx_after);
    }

    #[test]
    fn zoom_missing_file_param() {
        let ctx = make_ctx();
        let req = make_raw_request("z-5", r#"{"id":"z-5","command":"zoom","symbol":"foo"}"#);
        let resp = handle_zoom(&req, &ctx);

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["code"], "invalid_request");
    }

    #[test]
    fn zoom_missing_symbol_param() {
        let ctx = make_ctx();
        let path = fixture_path("calls.ts");
        let req_str = format!(
            r#"{{"id":"z-6","command":"zoom","file":"{}"}}"#,
            path.display()
        );
        let req: RawRequest = serde_json::from_str(&req_str).unwrap();
        let resp = handle_zoom(&req, &ctx);

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["code"], "invalid_request");
    }

    // --- Helpers ---

    fn make_zoom_request(
        id: &str,
        file: &str,
        symbol: &str,
        context_lines: Option<u64>,
    ) -> RawRequest {
        let mut json = serde_json::json!({
            "id": id,
            "command": "zoom",
            "file": file,
            "symbol": symbol,
        });
        if let Some(cl) = context_lines {
            json["context_lines"] = serde_json::json!(cl);
        }
        serde_json::from_value(json).unwrap()
    }

    fn make_raw_request(_id: &str, json_str: &str) -> RawRequest {
        serde_json::from_str(json_str).unwrap()
    }
}
