//! Handler for the `move_symbol` command: move a top-level symbol from one file
//! to another with full import rewiring across all consumer files.
//!
//! Flow: resolve symbol → verify top-level → checkpoint → discover consumers
//! → extract symbol text → remove from source → add to destination → rewrite
//! imports in every consumer → format/validate all files → return results.

use std::path::{Path, PathBuf};

use crate::context::AppContext;
use crate::edit;
use crate::imports;
use crate::lsp_hints;
use crate::parser::{detect_language, LangId};
use crate::protocol::{RawRequest, Response};
use crate::symbols::SymbolKind;

/// Handle a `move_symbol` request.
///
/// Params:
///   - `file` (string, required) — source file containing the symbol
///   - `symbol` (string, required) — name of the symbol to move
///   - `destination` (string, required) — target file path
///   - `scope` (string, optional) — scope qualifier for disambiguation
///   - `dry_run` (bool, optional) — preview diffs without modifying disk
///
/// On success: `{ ok, files_modified, consumers_updated, checkpoint_name,
///   results: [{ file, syntax_valid, formatted }] }`
/// On dry-run: `{ ok, dry_run, diffs: [{ file, diff, syntax_valid }] }`
/// On failure after partial write: `{ error with failed_file, rolled_back }`
pub fn handle_move_symbol(req: &RawRequest, ctx: &AppContext) -> Response {
    // --- Extract and validate params ---
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "move_symbol: missing required param 'file'",
            );
        }
    };

    let symbol_name = match req.params.get("symbol").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "move_symbol: missing required param 'symbol'",
            );
        }
    };

    let destination = match req.params.get("destination").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "move_symbol: missing required param 'destination'",
            );
        }
    };

    let scope = req.params.get("scope").and_then(|v| v.as_str());
    let dry_run = edit::is_dry_run(&req.params);

    let source_path_raw = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    let dest_path_raw = match ctx.validate_path(&req.id, Path::new(destination)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };

    if !source_path_raw.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("source file not found: {}", file),
        );
    }

    // Canonicalize paths to match callgraph's canonicalized paths
    // (on macOS, /var/folders → /private/var/folders)
    let source_canon =
        std::fs::canonicalize(&source_path_raw).unwrap_or_else(|_| source_path_raw.clone());
    let dest_canon = if dest_path_raw.exists() {
        std::fs::canonicalize(&dest_path_raw).unwrap_or_else(|_| dest_path_raw.clone())
    } else if let Some(parent) = dest_path_raw.parent() {
        // Destination may not exist yet — canonicalize its parent
        let canon_parent = std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
        canon_parent.join(dest_path_raw.file_name().unwrap_or_default())
    } else {
        dest_path_raw.clone()
    };
    let source_path: &Path = &source_canon;
    let dest_path: &Path = &dest_canon;

    if source_path == dest_path {
        return Response::error(
            &req.id,
            "invalid_request",
            "move_symbol: source and destination are the same file",
        );
    }

    // --- Call graph guard (D089) ---
    let mut cg_ref = ctx.callgraph().borrow_mut();
    let graph = match cg_ref.as_mut() {
        Some(g) => g,
        None => {
            return Response::error(
                &req.id,
                "not_configured",
                "move_symbol: project not configured — send 'configure' first",
            );
        }
    };

    // --- Resolve symbol ---
    let matches = match ctx.provider().resolve_symbol(&source_path, symbol_name) {
        Ok(m) => m,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    // Disambiguation
    let filtered = if matches.len() > 1 {
        if let Some(scope_filter) = scope {
            matches
                .into_iter()
                .filter(|m| {
                    m.symbol.scope_chain.iter().any(|s| s == scope_filter)
                        || m.symbol.parent.as_deref() == Some(scope_filter)
                })
                .collect()
        } else {
            matches
        }
    } else {
        matches
    };

    // LSP-enhanced disambiguation (S03)
    let filtered = if let Some(hints) = lsp_hints::parse_lsp_hints(req) {
        lsp_hints::apply_lsp_disambiguation(filtered, &hints)
    } else {
        filtered
    };

    if filtered.is_empty() {
        return Response::error(
            &req.id,
            "symbol_not_found",
            format!("symbol '{}' not found in {}", symbol_name, file),
        );
    }

    if filtered.len() > 1 {
        let candidates: Vec<serde_json::Value> = filtered
            .iter()
            .map(|m| {
                let sym = &m.symbol;
                let qualified = if sym.scope_chain.is_empty() {
                    sym.name.clone()
                } else {
                    format!("{}::{}", sym.scope_chain.join("::"), sym.name)
                };
                let kind_str = serde_json::to_value(&sym.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| format!("{:?}", sym.kind).to_lowercase());
                serde_json::json!({
                    "name": sym.name,
                    "qualified": qualified,
                    "line": sym.range.start_line + 1,
                    "kind": kind_str,
                })
            })
            .collect();

        return Response::success(
            &req.id,
            serde_json::json!({
                "code": "ambiguous_symbol",
                "candidates": candidates,
            }),
        );
    }

    let target = &filtered[0].symbol;

    // --- Top-level guard (D100) ---
    if !target.scope_chain.is_empty() || target.kind == SymbolKind::Method {
        return Response::error(
            &req.id,
            "invalid_request",
            format!(
                "move_symbol: cannot move non-top-level symbol '{}' (kind: {:?}, scope: [{}]). Only top-level declarations can be moved.",
                symbol_name,
                target.kind,
                target.scope_chain.join(", ")
            ),
        );
    }

    // --- Read source file ---
    let source_content = match std::fs::read_to_string(source_path) {
        Ok(s) => s,
        Err(e) => {
            return Response::error(&req.id, "file_not_found", format!("{}: {}", file, e));
        }
    };

    // --- Extract symbol text from source ---
    let start_byte = edit::line_col_to_byte(
        &source_content,
        target.range.start_line,
        target.range.start_col,
    );
    let end_byte =
        edit::line_col_to_byte(&source_content, target.range.end_line, target.range.end_col);

    let symbol_text = match source_content.get(start_byte..end_byte) {
        Some(symbol_text) => symbol_text,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!(
                    "move_symbol: symbol byte range [{}..{}) is not on UTF-8 boundaries",
                    start_byte, end_byte
                ),
            );
        }
    };

    // Prepare the text to add to destination: ensure it has export prefix
    let dest_symbol_text = prepare_exported_symbol(symbol_text);

    // Prepare source with symbol removed
    let new_source = match remove_symbol_from_source(&source_content, start_byte, end_byte) {
        Ok(s) => s,
        Err(e) => return Response::error(&req.id, e.code(), e.to_string()),
    };

    // --- Read destination file (may not exist yet) ---
    let dest_content = if dest_path.exists() {
        std::fs::read_to_string(dest_path).unwrap_or_default()
    } else {
        String::new()
    };

    // Prepare new destination content
    let new_dest = append_symbol_to_dest(&dest_content, &dest_symbol_text);

    // --- Discover consumers via callers_of ---
    // Build file first to ensure it's indexed
    if let Err(e) = graph.build_file(source_path) {
        return Response::error(&req.id, e.code(), e.to_string());
    }

    let consumers = match graph.callers_of(source_path, symbol_name, 1) {
        Ok(result) => result.callers,
        Err(_) => Vec::new(), // No callers found is fine
    };

    // Collect consumer files that need import rewriting
    // CallerGroup.file is relative to project root — resolve to absolute
    let project_root = graph.project_root().to_path_buf();
    let consumer_files: Vec<PathBuf> = consumers
        .iter()
        .map(|cg| {
            let p = PathBuf::from(&cg.file);
            if p.is_absolute() {
                p
            } else {
                project_root.join(&p)
            }
        })
        .filter(|p| p != source_path && p != dest_path)
        .collect();

    // Detect language for import rewriting
    let lang = detect_language(source_path);

    // --- Compute consumer rewrites ---
    let mut consumer_rewrites: Vec<(PathBuf, String, String)> = Vec::new(); // (path, original, new)
    for consumer_file in &consumer_files {
        if !consumer_file.exists() {
            continue;
        }
        let consumer_content = match std::fs::read_to_string(consumer_file) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let new_consumer = rewrite_consumer_imports(
            &consumer_content,
            consumer_file,
            source_path,
            dest_path,
            symbol_name,
            lang,
        );

        if let Some(rewritten) = new_consumer {
            consumer_rewrites.push((consumer_file.clone(), consumer_content, rewritten));
        }
    }

    // --- Dry-run mode (D071) ---
    if dry_run {
        let mut diffs: Vec<serde_json::Value> = Vec::new();

        // Source file diff
        let source_dr = edit::dry_run_diff(&source_content, &new_source, source_path);
        diffs.push(serde_json::json!({
            "file": file,
            "diff": source_dr.diff,
            "syntax_valid": source_dr.syntax_valid,
        }));

        // Destination file diff
        let dest_dr = edit::dry_run_diff(&dest_content, &new_dest, dest_path);
        diffs.push(serde_json::json!({
            "file": destination,
            "diff": dest_dr.diff,
            "syntax_valid": dest_dr.syntax_valid,
        }));

        // Consumer file diffs
        for (path, original, new_content) in &consumer_rewrites {
            let dr = edit::dry_run_diff(original, new_content, &path);
            diffs.push(serde_json::json!({
                "file": path.display().to_string(),
                "diff": dr.diff,
                "syntax_valid": dr.syntax_valid,
            }));
        }

        return Response::success(
            &req.id,
            serde_json::json!({
                "ok": true,
                "dry_run": true,
                "diffs": diffs,
            }),
        );
    }

    // --- Create checkpoint (D105) ---
    let checkpoint_name = format!("move_symbol:{}", symbol_name);
    {
        let mut all_files: Vec<PathBuf> = vec![source_path.to_path_buf()];
        if dest_path.exists() {
            all_files.push(dest_path.to_path_buf());
        }
        for (path, _, _) in &consumer_rewrites {
            all_files.push(path.clone());
        }

        let backup_store = ctx.backup().borrow();
        let mut cp_store = ctx.checkpoint().borrow_mut();
        if let Err(e) = cp_store.create(req.session(), &checkpoint_name, all_files, &backup_store) {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    }

    // --- Apply mutations ---
    // Track files for rollback
    let mut written_files: Vec<PathBuf> = Vec::new();
    let mut new_files: Vec<PathBuf> = Vec::new();
    let mut results: Vec<serde_json::Value> = Vec::new();

    let dest_existed = dest_path.exists();

    // 1. Write source file (symbol removed)
    match edit::write_format_validate(&source_path, &new_source, &ctx.config(), &req.params) {
        Ok(wr) => {
            if let Ok(final_content) = std::fs::read_to_string(source_path) {
                ctx.lsp_notify_file_changed(source_path, &final_content);
            }

            written_files.push(source_path.to_path_buf());
            results.push(serde_json::json!({
                "file": file,
                "syntax_valid": wr.syntax_valid,
                "formatted": wr.formatted,
            }));
        }
        Err(e) => {
            restore_checkpoint(ctx, req.session(), &checkpoint_name);
            return move_error(
                &req.id,
                file,
                &written_files,
                &new_files,
                &format!("failed to write source: {}", e),
            );
        }
    }

    // 2. Write destination file (symbol added)
    match edit::write_format_validate(&dest_path, &new_dest, &ctx.config(), &req.params) {
        Ok(wr) => {
            if let Ok(final_content) = std::fs::read_to_string(dest_path) {
                ctx.lsp_notify_file_changed(dest_path, &final_content);
            }

            if dest_existed {
                written_files.push(dest_path.to_path_buf());
            } else {
                new_files.push(dest_path.to_path_buf());
            }
            results.push(serde_json::json!({
                "file": destination,
                "syntax_valid": wr.syntax_valid,
                "formatted": wr.formatted,
            }));
        }
        Err(e) => {
            restore_checkpoint(ctx, req.session(), &checkpoint_name);
            cleanup_new_files(&new_files);
            return move_error(
                &req.id,
                destination,
                &written_files,
                &new_files,
                &format!("failed to write destination: {}", e),
            );
        }
    }

    // 3. Write consumer files (imports rewritten)
    let mut consumers_updated = 0;
    for (path, _original, new_content) in &consumer_rewrites {
        match edit::write_format_validate(&path, new_content, &ctx.config(), &req.params) {
            Ok(wr) => {
                if let Ok(final_content) = std::fs::read_to_string(&path) {
                    ctx.lsp_notify_file_changed(path, &final_content);
                }

                written_files.push(path.clone());
                consumers_updated += 1;
                results.push(serde_json::json!({
                    "file": path.display().to_string(),
                    "syntax_valid": wr.syntax_valid,
                    "formatted": wr.formatted,
                }));
            }
            Err(e) => {
                restore_checkpoint(ctx, req.session(), &checkpoint_name);
                cleanup_new_files(&new_files);
                return move_error(
                    &req.id,
                    &path.display().to_string(),
                    &written_files,
                    &new_files,
                    &format!("failed to write consumer: {}", e),
                );
            }
        }
    }

    let files_modified = results.len();

    log::debug!(
        "[aft] move_symbol: {} from {} to {} ({} consumers updated)",
        symbol_name,
        file,
        destination,
        consumers_updated
    );

    Response::success(
        &req.id,
        serde_json::json!({
            "ok": true,
            "files_modified": files_modified,
            "consumers_updated": consumers_updated,
            "checkpoint_name": checkpoint_name,
            "results": results,
        }),
    )
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Compute a relative import path from `from_file` to `to_file`.
///
/// Given a consumer file and a target module file, computes the relative import
/// path suitable for TS/JS/TSX imports. Strips file extensions for TS/JS/TSX.
///
/// Examples:
/// - same dir: `./utils`
/// - parent dir: `../shared/utils`
/// - deeply nested: `../../lib/helpers`
pub fn compute_relative_import_path(from_file: &Path, to_file: &Path) -> String {
    // We want the path from from_file's directory to to_file
    let from_dir = from_file.parent().unwrap_or(Path::new(""));
    let to_dir = to_file.parent().unwrap_or(Path::new(""));
    let to_stem = to_file
        .file_stem()
        .unwrap_or_default()
        .to_str()
        .unwrap_or("");

    // Compute relative path from from_dir to to_dir
    let rel_dir = compute_relative_dir(from_dir, to_dir);

    if rel_dir.is_empty() || rel_dir == "." {
        format!("./{}", to_stem)
    } else if rel_dir.starts_with("..") {
        format!("{}/{}", rel_dir, to_stem)
    } else {
        format!("./{}/{}", rel_dir, to_stem)
    }
}

/// Compute the relative directory path from `from` to `to`.
fn compute_relative_dir(from: &Path, to: &Path) -> String {
    // Normalize to components
    let from_parts: Vec<&str> = from
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let to_parts: Vec<&str> = to
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();

    // Find common prefix length
    let common_len = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let ups = from_parts.len() - common_len;
    let downs = &to_parts[common_len..];

    let mut parts: Vec<&str> = Vec::new();
    for _ in 0..ups {
        parts.push("..");
    }
    for d in downs {
        parts.push(d);
    }

    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// Check if a path string refers to the same file as `target`, accounting for
/// relative path variations (with or without extension, ./ prefix, etc.).
fn import_path_matches_file(import_module: &str, consumer_file: &Path, target_file: &Path) -> bool {
    // Only handle relative imports
    if !import_module.starts_with('.') {
        return false;
    }

    let consumer_dir = consumer_file.parent().unwrap_or(Path::new(""));
    let resolved = consumer_dir.join(import_module);

    // Try exact match (with extension already in import path)
    if paths_equivalent(&resolved, target_file) {
        return true;
    }

    // Try adding common extensions
    for ext in &["ts", "tsx", "js", "jsx"] {
        let with_ext = resolved.with_extension(ext);
        if paths_equivalent(&with_ext, target_file) {
            return true;
        }
    }

    // Try index file pattern: import './dir' -> './dir/index.ts'
    let as_index = resolved.join("index");
    for ext in &["ts", "tsx", "js", "jsx"] {
        let with_ext = as_index.with_extension(ext);
        if paths_equivalent(&with_ext, target_file) {
            return true;
        }
    }

    false
}

/// Compare two paths for equivalence, normalizing components.
fn paths_equivalent(a: &Path, b: &Path) -> bool {
    let norm_a = normalize_path(a);
    let norm_b = normalize_path(b);
    norm_a == norm_b
}

/// Normalize a path by resolving `.` and `..` components without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    let mut parts: Vec<std::path::Component> = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::ParentDir => {
                if let Some(last) = parts.last() {
                    if matches!(last, std::path::Component::Normal(_)) {
                        parts.pop();
                        continue;
                    }
                }
                parts.push(comp);
            }
            std::path::Component::CurDir => {} // skip
            _ => parts.push(comp),
        }
    }
    parts.iter().collect()
}

/// Prepare the symbol text for the destination file.
/// Ensures it has an `export` keyword prefix.
fn prepare_exported_symbol(symbol_text: &str) -> String {
    let trimmed = symbol_text.trim();

    // If it already starts with 'export', use as-is
    if trimmed.starts_with("export default")
        || trimmed.starts_with("export {")
        || trimmed.starts_with("export *")
        || trimmed.starts_with("export ")
    {
        return trimmed.to_string();
    }

    // Add export prefix
    format!("export {}", trimmed)
}

/// Remove a symbol from source content, cleaning up surrounding whitespace.
fn remove_symbol_from_source(
    source: &str,
    start_byte: usize,
    end_byte: usize,
) -> Result<String, crate::error::AftError> {
    // Extend backwards to include any preceding blank lines
    let mut actual_start = start_byte;

    // If the symbol starts with "export", we may need to look further back.
    // Walk backwards from start to find the beginning of the line
    while actual_start > 0 && source.as_bytes()[actual_start - 1] != b'\n' {
        actual_start -= 1;
    }

    // Check if the content before the symbol on this line is just whitespace
    let line_prefix = &source[actual_start..start_byte];
    if line_prefix.trim().is_empty() {
        // Use the line start
    } else {
        // There's meaningful content before it on this line, use original start
        actual_start = start_byte;
    }

    // Extend end to include trailing newline
    let mut actual_end = end_byte;
    let bytes = source.as_bytes();

    // Skip any trailing whitespace on the same line
    while actual_end < bytes.len() && (bytes[actual_end] == b' ' || bytes[actual_end] == b'\t') {
        actual_end += 1;
    }
    // Skip the newline
    if actual_end < bytes.len() && bytes[actual_end] == b'\n' {
        actual_end += 1;
    } else if actual_end < bytes.len() && bytes[actual_end] == b'\r' {
        actual_end += 1;
        if actual_end < bytes.len() && bytes[actual_end] == b'\n' {
            actual_end += 1;
        }
    }

    // Skip one additional blank line if present (to clean up double-spacing)
    let peek_end = actual_end;
    if peek_end < bytes.len() && bytes[peek_end] == b'\n' {
        actual_end = peek_end + 1;
    } else if peek_end < bytes.len() && bytes[peek_end] == b'\r' {
        actual_end = peek_end + 1;
        if actual_end < bytes.len() && bytes[actual_end] == b'\n' {
            actual_end += 1;
        }
    }

    edit::replace_byte_range(source, actual_start, actual_end, "")
}

/// Append a symbol to the destination file content.
fn append_symbol_to_dest(dest_content: &str, symbol_text: &str) -> String {
    if dest_content.is_empty() {
        format!("{}\n", symbol_text)
    } else {
        let trimmed_dest = dest_content.trim_end();
        format!("{}\n\n{}\n", trimmed_dest, symbol_text)
    }
}

/// Rewrite a consumer file's imports to point to the new destination.
///
/// Finds imports from the source file that include the moved symbol,
/// and rewrites them to import from the destination instead.
/// Returns `None` if no changes needed.
fn rewrite_consumer_imports(
    consumer_content: &str,
    consumer_file: &Path,
    source_file: &Path,
    dest_file: &Path,
    symbol_name: &str,
    lang: Option<LangId>,
) -> Option<String> {
    let lang = lang?;

    // Only handle TS/JS/TSX for now (the primary use case)
    if !matches!(lang, LangId::TypeScript | LangId::Tsx | LangId::JavaScript) {
        return None;
    }

    // Parse imports
    let (_source_text, _tree, block) = match imports::parse_file_imports(consumer_file, lang) {
        Ok(r) => r,
        Err(_) => return None,
    };

    // Use the consumer_content we already read (should match source_text)
    let content = consumer_content;

    // Find imports from the source file that reference the moved symbol
    let mut result = content.to_string();
    let mut offset_delta: isize = 0; // track byte shifts from prior edits

    // Process imports in reverse order to maintain byte offsets
    let mut matching_imports: Vec<(usize, &imports::ImportStatement)> = block
        .imports
        .iter()
        .enumerate()
        .filter(|(_, imp)| import_path_matches_file(&imp.module_path, consumer_file, source_file))
        .collect();

    // Sort by byte range start descending so we edit from end to start
    matching_imports.sort_by(|a, b| b.1.byte_range.start.cmp(&a.1.byte_range.start));

    let mut made_changes = false;

    for (_, imp) in &matching_imports {
        let has_moved_symbol = imp.names.iter().any(|n| n == symbol_name)
            || imp.default_import.as_deref() == Some(symbol_name);

        if !has_moved_symbol {
            continue;
        }

        let new_import_path = compute_relative_import_path(consumer_file, dest_file);

        // Check if this import has other symbols besides the moved one
        let remaining_names: Vec<String> = imp
            .names
            .iter()
            .filter(|n| n.as_str() != symbol_name)
            .cloned()
            .collect();
        let remaining_default = if imp.default_import.as_deref() == Some(symbol_name) {
            None
        } else {
            imp.default_import.clone()
        };

        let type_only = imp.kind == imports::ImportKind::Type;

        // Build the replacement text
        let start = match (imp.byte_range.start as isize).checked_add(offset_delta) {
            Some(v) if v >= 0 => v as usize,
            _ => return None, // offset overflow — skip rewrite
        };
        let end = match (imp.byte_range.end as isize).checked_add(offset_delta) {
            Some(v) if v >= 0 => v as usize,
            _ => return None, // offset overflow — skip rewrite
        };

        if remaining_names.is_empty() && remaining_default.is_none() {
            // All symbols in this import are moving — replace entire import with new path
            // Preserve the original import structure but change the path
            let new_import = generate_import_with_alias(
                &imp.raw_text,
                symbol_name,
                &new_import_path,
                type_only,
                lang,
            );
            let old_len = end - start;
            result = format!("{}{}{}", &result[..start], new_import, &result[end..]);
            offset_delta += new_import.len() as isize - old_len as isize;
        } else {
            // Some symbols remain — keep old import for remaining, add new import for moved
            let kept_import = imports::generate_import_line(
                lang,
                &imp.module_path,
                &remaining_names,
                remaining_default.as_deref(),
                type_only,
            );

            // Generate new import for the moved symbol
            let moved_import = generate_import_with_alias(
                &imp.raw_text,
                symbol_name,
                &new_import_path,
                type_only,
                lang,
            );

            let replacement = format!("{}\n{}", kept_import, moved_import);
            let old_len = end - start;
            result = format!("{}{}{}", &result[..start], replacement, &result[end..]);
            offset_delta += replacement.len() as isize - old_len as isize;
        }

        made_changes = true;
    }

    if made_changes {
        Some(result)
    } else {
        None
    }
}

/// Generate an import statement preserving any alias from the original import text.
///
/// If the original import has `{ X as Y }`, the new import preserves the alias.
fn generate_import_with_alias(
    original_raw: &str,
    symbol_name: &str,
    new_module_path: &str,
    type_only: bool,
    _lang: LangId,
) -> String {
    // Check if the original import uses an alias for this symbol
    // Pattern: `X as Y` inside braces
    let alias = extract_alias(original_raw, symbol_name);

    let names = if let Some(alias_name) = &alias {
        vec![format!("{} as {}", symbol_name, alias_name)]
    } else {
        vec![symbol_name.to_string()]
    };

    let type_prefix = if type_only { "type " } else { "" };
    let names_str = names.join(", ");
    format!(
        "import {}{{ {} }} from '{}';",
        type_prefix, names_str, new_module_path
    )
}

/// Extract an alias for a symbol from an import statement's raw text.
///
/// Looks for `symbol_name as alias` pattern in the import text.
fn extract_alias(raw_text: &str, symbol_name: &str) -> Option<String> {
    // Look for `symbolName as aliasName` pattern
    let pattern = format!("{} as ", symbol_name);
    if let Some(pos) = raw_text.find(&pattern) {
        let after = &raw_text[pos + pattern.len()..];
        // Extract the alias identifier (until comma, brace, whitespace, or semicolon)
        let alias: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !alias.is_empty() {
            return Some(alias);
        }
    }
    None
}

/// Restore a checkpoint by name, scoped to the caller's session.
fn restore_checkpoint(ctx: &AppContext, session: &str, name: &str) {
    let cp_store = ctx.checkpoint().borrow();
    if let Err(e) = cp_store.restore(session, name) {
        log::debug!(
            "[aft] move_symbol rollback: failed to restore checkpoint '{}': {}",
            name,
            e
        );
    }
}

/// Delete new files that were created during the operation.
fn cleanup_new_files(new_files: &[PathBuf]) {
    for path in new_files {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(path) {
                log::debug!(
                    "[aft] move_symbol rollback: failed to delete new file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
}

/// Build a structured error response for a failed move operation.
fn move_error(
    req_id: &str,
    failed_file: &str,
    written_files: &[PathBuf],
    new_files: &[PathBuf],
    message: &str,
) -> Response {
    let mut rolled_back: Vec<serde_json::Value> = written_files
        .iter()
        .map(|p| {
            serde_json::json!({
                "file": p.display().to_string(),
                "action": "restored",
            })
        })
        .collect();

    for p in new_files {
        rolled_back.push(serde_json::json!({
            "file": p.display().to_string(),
            "action": "deleted",
        }));
    }

    log::debug!(
        "[aft] move_symbol failed at {}: {} — rolled back {} files",
        failed_file,
        message,
        rolled_back.len()
    );

    Response::error_with_data(
        req_id,
        "move_symbol_failed",
        message,
        serde_json::json!({
            "failed_file": failed_file,
            "rolled_back": rolled_back,
        }),
    )
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_same_directory() {
        let from = Path::new("src/components/Button.ts");
        let to = Path::new("src/components/utils.ts");
        assert_eq!(compute_relative_import_path(from, to), "./utils");
    }

    #[test]
    fn relative_path_parent_directory() {
        let from = Path::new("src/components/Button.ts");
        let to = Path::new("src/utils.ts");
        assert_eq!(compute_relative_import_path(from, to), "../utils");
    }

    #[test]
    fn relative_path_sibling_directory() {
        let from = Path::new("src/components/Button.ts");
        let to = Path::new("src/shared/utils.ts");
        assert_eq!(compute_relative_import_path(from, to), "../shared/utils");
    }

    #[test]
    fn relative_path_deeply_nested() {
        let from = Path::new("src/features/auth/components/Login.ts");
        let to = Path::new("src/lib/helpers.ts");
        assert_eq!(
            compute_relative_import_path(from, to),
            "../../../lib/helpers"
        );
    }

    #[test]
    fn relative_path_child_directory() {
        let from = Path::new("src/index.ts");
        let to = Path::new("src/utils/helpers.ts");
        assert_eq!(compute_relative_import_path(from, to), "./utils/helpers");
    }

    #[test]
    fn relative_path_strips_extension() {
        let from = Path::new("src/app.tsx");
        let to = Path::new("src/components/Header.tsx");
        assert_eq!(
            compute_relative_import_path(from, to),
            "./components/Header"
        );
    }

    #[test]
    fn prepare_exported_adds_export() {
        let text = "function doStuff() { return 42; }";
        assert_eq!(
            prepare_exported_symbol(text),
            "export function doStuff() { return 42; }"
        );
    }

    #[test]
    fn prepare_exported_preserves_existing() {
        let text = "export function doStuff() { return 42; }";
        assert_eq!(
            prepare_exported_symbol(text),
            "export function doStuff() { return 42; }"
        );
    }

    #[test]
    fn prepare_exported_preserves_export_reexports_and_default() {
        assert_eq!(
            prepare_exported_symbol("export default function doStuff() { return 42; }"),
            "export default function doStuff() { return 42; }"
        );
        assert_eq!(
            prepare_exported_symbol("export { doStuff } from './other';"),
            "export { doStuff } from './other';"
        );
        assert_eq!(
            prepare_exported_symbol("export * from './other';"),
            "export * from './other';"
        );
    }

    #[test]
    fn extract_alias_found() {
        let raw = "import { formatDate as fmtDate, other } from './utils';";
        assert_eq!(
            extract_alias(raw, "formatDate"),
            Some("fmtDate".to_string())
        );
    }

    #[test]
    fn extract_alias_not_found() {
        let raw = "import { formatDate, other } from './utils';";
        assert_eq!(extract_alias(raw, "formatDate"), None);
    }

    #[test]
    fn import_path_matches_same_dir() {
        let consumer = Path::new("src/components/Button.ts");
        let target = Path::new("src/components/utils.ts");
        assert!(import_path_matches_file("./utils", consumer, target));
    }

    #[test]
    fn import_path_matches_parent_dir() {
        let consumer = Path::new("src/components/Button.ts");
        let target = Path::new("src/service.ts");
        assert!(import_path_matches_file("../service", consumer, target));
    }

    #[test]
    fn import_path_no_match_different_file() {
        let consumer = Path::new("src/components/Button.ts");
        let target = Path::new("src/components/utils.ts");
        assert!(!import_path_matches_file("./other", consumer, target));
    }

    #[test]
    fn import_path_no_match_external() {
        let consumer = Path::new("src/components/Button.ts");
        let target = Path::new("src/components/utils.ts");
        assert!(!import_path_matches_file("react", consumer, target));
    }

    #[test]
    fn normalize_path_with_parent() {
        let p = Path::new("src/components/../utils.ts");
        assert_eq!(normalize_path(p), PathBuf::from("src/utils.ts"));
    }

    #[test]
    fn normalize_path_with_current() {
        let p = Path::new("src/./components/Button.ts");
        assert_eq!(normalize_path(p), PathBuf::from("src/components/Button.ts"));
    }

    #[test]
    fn remove_symbol_cleans_whitespace() {
        let source = "export function keep() {}\n\nexport function remove() {}\n\nexport function alsoKeep() {}\n";
        let start = source.find("export function remove").unwrap();
        let end = start + "export function remove() {}".len();
        let result = remove_symbol_from_source(source, start, end).unwrap();
        assert!(result.contains("export function keep()"));
        assert!(!result.contains("remove"));
        assert!(result.contains("export function alsoKeep()"));
    }

    #[test]
    fn append_to_empty_dest() {
        let result = append_symbol_to_dest("", "export function foo() {}");
        assert_eq!(result, "export function foo() {}\n");
    }

    #[test]
    fn append_to_existing_dest() {
        let result =
            append_symbol_to_dest("export function bar() {}\n", "export function foo() {}");
        assert_eq!(
            result,
            "export function bar() {}\n\nexport function foo() {}\n"
        );
    }
}
