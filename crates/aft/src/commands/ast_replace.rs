//! Handler for the `ast_replace` command: AST-aware pattern replacement using ast-grep.
//!
//! Walks the project directory (or specified paths/globs) and replaces all nodes
//! matching the given pattern with the rewrite template.

use std::path::{Path, PathBuf};

use ast_grep_core::tree_sitter::LanguageExt;

use crate::ast_grep_lang::AstGrepLang;
use crate::commands::ast_scope::collect_ast_files;
use crate::context::AppContext;
use crate::edit::dry_run_diff;
use crate::protocol::{RawRequest, Response};

/// Handle an `ast_replace` request.
///
/// Params:
///   - `pattern` (string, required) — ast-grep pattern, e.g. `console.log($MSG)`
///   - `rewrite` (string, required) — replacement template, e.g. `logger.info($MSG)`
///   - `lang` (string, required) — language: typescript, tsx, javascript, python, rust, go, c, cpp, zig, csharp
///   - `paths` (array of strings, optional) — restrict to these paths
///   - `globs` (array of strings, optional) — include/exclude glob patterns
///   - `dry_run` (bool, optional, default true) — preview without writing
///
/// Returns (dry_run=true):
///   `{ ok: true, files: [{ file, diff, replacements }], total_replacements: N, total_files: N, files_with_matches: N, files_searched: N }`
///
/// Returns (dry_run=false):
///   `{ ok: true, files: [{ file, replacements, backup_id? }], total_replacements: N, total_files: N, files_with_matches: N, files_searched: N }`
pub fn handle_ast_replace(req: &RawRequest, ctx: &AppContext) -> Response {
    let pattern = match req.params.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "ast_replace: missing required param 'pattern'",
            );
        }
    };

    let rewrite = match req.params.get("rewrite").and_then(|v| v.as_str()) {
        Some(r) => r.to_string(),
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "ast_replace: missing required param 'rewrite'",
            );
        }
    };

    let lang_str = match req.params.get("lang").and_then(|v| v.as_str()) {
        Some(l) => l,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "ast_replace: missing required param 'lang'",
            );
        }
    };

    let lang = match AstGrepLang::from_str(lang_str) {
        Some(l) => l,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!(
                    "ast_replace: unsupported language '{}'. Supported: typescript, tsx, javascript, python, rust, go, c, cpp, zig, csharp",
                    lang_str
                ),
            );
        }
    };

    let paths: Vec<String> = req
        .params
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let globs: Vec<String> = req
        .params
        .get("globs")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let dry_run = req
        .params
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let project_root = ctx
        .config()
        .project_root
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));

    // Validate the pattern upfront. ast-grep-core can panic (via unwrap) on patterns
    // that parse to multiple AST nodes (e.g. bare `catch`/`finally` clauses).
    // Release builds use panic="unwind", so catch_unwind is effective, but returning
    // an explicit pattern error gives callers a better signal.
    {
        use ast_grep_core::matcher::Pattern as AstPattern;
        if let Err(e) = AstPattern::try_new(&pattern, lang.clone()) {
            return Response::error(
                &req.id,
                "invalid_pattern",
                format!(
                    "ast_replace: invalid pattern '{}': {}. Patterns must be complete AST nodes.",
                    pattern, e
                ),
            );
        }
    }

    let scope =
        match collect_ast_files(&req.id, "ast_replace", &project_root, &lang, &paths, &globs) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };

    let mut file_results: Vec<serde_json::Value> = Vec::new();
    let mut total_replacements = 0usize;
    let mut total_files = 0usize;
    let mut files_searched = 0usize;
    let mut files_with_matches = 0usize;

    for file_path in &scope.files {
        files_searched += 1;
        let original = match std::fs::read_to_string(file_path.as_path()) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let replacement_count = {
            let root = lang.ast_grep(&original);
            let node = root.root();
            node.find_all(pattern.as_str()).count()
        };

        if replacement_count == 0 {
            continue;
        }

        files_with_matches += 1;

        // Use replace_all to get ALL edits — root.replace() only replaces the FIRST match.
        let root = lang.ast_grep(&original);
        let mut edits = root.root().replace_all(pattern.as_str(), rewrite.as_str());
        if edits.is_empty() {
            continue;
        }
        // Apply edits in reverse byte-offset order to preserve positions
        edits.sort_by(|a, b| b.position.cmp(&a.position));
        let mut new_bytes = original.as_bytes().to_vec();
        for edit in &edits {
            let start = edit.position;
            let end = start + edit.deleted_length;
            if start <= new_bytes.len() && end <= new_bytes.len() {
                new_bytes.splice(start..end, edit.inserted_text.iter().copied());
            }
        }
        let new_content = String::from_utf8(new_bytes).unwrap_or_else(|_| original.clone());
        total_replacements += replacement_count;
        total_files += 1;

        if dry_run {
            let diff_result = dry_run_diff(&original, &new_content, file_path.as_path());
            file_results.push(serde_json::json!({
                "file": file_path.display().to_string(),
                "diff": diff_result.diff,
                "replacements": replacement_count,
            }));
        } else {
            let validated_path = match validate_matched_file_path(ctx, &req.id, file_path.as_path())
            {
                Ok(path) => path,
                Err(resp) => return resp,
            };

            let backup_id = ctx
                .backup()
                .borrow_mut()
                .snapshot(req.session(), validated_path.as_path(), "ast_replace")
                .ok();

            match std::fs::write(validated_path.as_path(), &new_content) {
                Ok(()) => {
                    let mut entry = serde_json::json!({
                        "file": file_path.display().to_string(),
                        "replacements": replacement_count,
                    });
                    if let Some(bid) = backup_id {
                        if let Some(obj) = entry.as_object_mut() {
                            obj.insert("backup_id".to_string(), serde_json::Value::String(bid));
                        }
                    }
                    file_results.push(entry);
                }
                Err(e) => {
                    file_results.push(serde_json::json!({
                        "file": file_path.display().to_string(),
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
            }
        }
    }

    Response::success(
        &req.id,
        serde_json::json!({
            "files": file_results,
            "total_replacements": total_replacements,
            "total_files": total_files,
            "files_with_matches": files_with_matches,
            "files_searched": files_searched,
            "no_files_matched_scope": scope.no_files_matched_scope,
            "scope_warnings": scope.scope_warnings,
            "dry_run": dry_run,
        }),
    )
}

fn validate_matched_file_path(
    ctx: &AppContext,
    req_id: &str,
    file_path: &Path,
) -> Result<PathBuf, Response> {
    ctx.validate_path(req_id, file_path)
}
