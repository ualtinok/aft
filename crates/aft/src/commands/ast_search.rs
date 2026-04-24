//! Handler for the `ast_search` command: AST-aware pattern search using ast-grep.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ast_grep_core::tree_sitter::LanguageExt;

use crate::ast_grep_lang::AstGrepLang;
use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

/// Handle an `ast_search` request.
///
/// Params:
///   - `pattern` (string, required) — ast-grep pattern (e.g. `console.log($MSG)`)
///   - `lang` (string, required) — target language: typescript, tsx, javascript, python, rust, go, c, cpp, zig, csharp
///   - `paths` (string[], optional) — directories/files to search (default: project root)
///   - `globs` (string[], optional) — include/exclude glob filters; prefix `!` to exclude
///   - `context` (integer, optional) — lines of context around each match (default: 0)
///
/// Returns: `{ matches: [{ file, line, column, text, meta_variables, context? }], total_matches, files_with_matches, files_searched }`
pub fn handle_ast_search(req: &RawRequest, ctx: &AppContext) -> Response {
    let pattern = match req.params.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "ast_search: missing required param 'pattern'",
            );
        }
    };

    let lang_str = match req.params.get("lang").and_then(|v| v.as_str()) {
        Some(l) => l,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "ast_search: missing required param 'lang'",
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
                    "ast_search: unsupported language '{}'. Supported: typescript, tsx, javascript, python, rust, go, c, cpp, zig, csharp",
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

    let context_lines = req
        .params
        .get("context")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    // Validate the pattern before searching. ast-grep-core can panic (via unwrap) on
    // patterns that parse to multiple AST nodes (e.g. bare `catch` or `finally`
    // clauses). Release builds use panic="unwind", so catch_unwind is effective,
    // but returning an explicit pattern error gives callers a better signal.
    use ast_grep_core::matcher::Pattern as AstPattern;
    if let Err(err) = AstPattern::try_new(&pattern, lang.clone()) {
        return Response::error(
            &req.id,
            "invalid_pattern",
            format!("invalid AST pattern: {}", err),
        );
    }

    let config = ctx.config();
    let project_root = config
        .project_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    drop(config);

    let search_roots: Vec<PathBuf> = if paths.is_empty() {
        vec![project_root.clone()]
    } else {
        paths
            .iter()
            .map(|p| {
                let pb = PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    project_root.join(p)
                }
            })
            .collect()
    };

    let extensions = lang.extensions();
    let mut all_matches: Vec<serde_json::Value> = Vec::new();
    let mut files_searched: usize = 0;
    let mut files_with_matches: usize = 0;

    for root in &search_roots {
        let files = collect_files(root, extensions, &globs);
        for file_path in files {
            files_searched += 1;
            let source = match std::fs::read_to_string(&file_path) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let matches = search_file(&source, &file_path, &pattern, &lang, context_lines);
            if !matches.is_empty() {
                files_with_matches += 1;
            }
            all_matches.extend(matches);
        }
    }

    let total_matches = all_matches.len();

    Response::success(
        &req.id,
        serde_json::json!({
            "matches": all_matches,
            "total_matches": total_matches,
            "files_with_matches": files_with_matches,
            "files_searched": files_searched,
        }),
    )
}

fn search_file(
    source: &str,
    file_path: &Path,
    pattern: &str,
    lang: &AstGrepLang,
    context_lines: usize,
) -> Vec<serde_json::Value> {
    let ast_grep = lang.ast_grep(source);
    let root = ast_grep.root();

    let source_lines: Vec<&str> = source.lines().collect();
    let file_str = file_path.display().to_string();

    // Validate the pattern before searching. ast-grep-core panics (via unwrap) on patterns
    // that parse to multiple AST nodes (e.g. bare `catch` or `finally` clauses).
    use ast_grep_core::matcher::Pattern as AstPattern;
    if AstPattern::try_new(pattern, lang.clone()).is_err() {
        return Vec::new();
    }

    let matches_iter: Vec<_> = root.find_all(pattern).collect();

    matches_iter
        .into_iter()
        .map(|node_match| {
            let start_pos = node_match.start_pos();
            // ast-grep line() is 0-based; add 1 for 1-based response
            let line_1based = start_pos.line() + 1;
            let column = start_pos.byte_point().1;

            let text = node_match.text().to_string();

            let env = node_match.get_env();
            let mut meta_vars: HashMap<String, serde_json::Value> = HashMap::new();

            for meta_var in env.get_matched_variables() {
                use ast_grep_core::meta_var::MetaVariable;
                match &meta_var {
                    MetaVariable::Capture(name, _) => {
                        if let Some(node) = env.get_match(name) {
                            meta_vars.insert(
                                format!("${}", name),
                                serde_json::Value::String(node.text().to_string()),
                            );
                        }
                    }
                    MetaVariable::MultiCapture(name) => {
                        let nodes = env.get_multiple_matches(name);
                        let texts: Vec<serde_json::Value> = nodes
                            .iter()
                            .map(|n| serde_json::Value::String(n.text().to_string()))
                            .collect();
                        meta_vars.insert(format!("${}", name), serde_json::Value::Array(texts));
                    }
                    _ => {}
                }
            }

            let mut result = serde_json::json!({
                "file": file_str,
                "line": line_1based,
                "column": column,
                "text": text,
                "meta_variables": meta_vars,
            });

            if context_lines > 0 {
                let match_line_0 = start_pos.line();
                let end_line_0 = node_match.end_pos().line();

                let ctx_start = match_line_0.saturating_sub(context_lines);
                let ctx_end = (end_line_0 + context_lines + 1).min(source_lines.len());

                let context: Vec<serde_json::Value> = (ctx_start..ctx_end)
                    .map(|i| {
                        serde_json::json!({
                            "line": i + 1,
                            "text": source_lines[i],
                            "is_match": i >= match_line_0 && i <= end_line_0,
                        })
                    })
                    .collect();

                result["context"] = serde_json::Value::Array(context);
            }

            result
        })
        .collect()
}

/// Walk `root` and collect files whose extension is in `extensions`.
///
/// Respects `.gitignore` and skips common non-source dirs (`node_modules`, `target`, etc.).
/// Use `globs` to further filter results; prefix `!` to exclude a pattern.
pub fn collect_files(root: &Path, extensions: &[&str], globs: &[String]) -> Vec<PathBuf> {
    use ignore::WalkBuilder;

    let (include_globs, exclude_globs): (Vec<&str>, Vec<&str>) = globs
        .iter()
        .map(|s| s.as_str())
        .partition(|s| !s.starts_with('!'));

    let exclude_globs: Vec<&str> = exclude_globs
        .iter()
        .map(|s| s.trim_start_matches('!'))
        .collect();

    let mut override_builder = ignore::overrides::OverrideBuilder::new(root);
    for g in &include_globs {
        let _ = override_builder.add(g);
    }
    for g in &exclude_globs {
        let _ = override_builder.add(&format!("!{}", g));
    }
    let overrides = override_builder.build().ok();

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                return !matches!(
                    name.as_ref(),
                    "node_modules"
                        | "target"
                        | "venv"
                        | ".venv"
                        | ".git"
                        | "__pycache__"
                        | ".tox"
                        | "dist"
                        | "build"
                );
            }
            true
        });

    if let Some(ov) = overrides {
        builder.overrides(ov);
    }

    builder
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map_or(false, |ext| extensions.contains(&ext))
        })
        .map(|entry| entry.into_path())
        .collect()
}
