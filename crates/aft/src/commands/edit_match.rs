//! Handler for the `edit_match` command: content-based string matching with
//! disambiguation for multiple occurrences.

use std::path::{Path, PathBuf};

use crate::context::AppContext;
use crate::edit::{self, validate_syntax};
use crate::protocol::{RawRequest, Response};

/// Handle an `edit_match` request.
///
/// Params:
///   - `file` (string, required) — target file path or glob pattern (e.g. `**/*.ts`)
///   - `match` (string, required, non-empty) — literal string to find
///   - `replacement` (string, required) — replacement content
///   - `occurrence` (integer, optional, 0-indexed) — select a specific occurrence (single-file only)
///   - `replace_all` (bool, optional) — replace all occurrences (default: false)
///   - `dry_run` (bool, optional) — preview changes without writing
///
/// When `file` is a glob pattern:
///   - Applies match/replace across all matching files
///   - `replace_all` is implicitly true
///   - `occurrence` is ignored
///   - Returns: `{ ok, files: [{ file, replacements, ... }], total_replacements, total_files }`
///
/// When `file` is a literal path:
///   - Original single-file behavior
///   - Returns: `{ file, replacements: 1, syntax_valid, backup_id? }`
pub fn handle_edit_match(req: &RawRequest, ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_match: missing required param 'file'",
            );
        }
    };

    let match_str = match req.params.get("match").and_then(|v| v.as_str()) {
        Some(m) => m,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_match: missing required param 'match'",
            );
        }
    };

    if match_str.is_empty() {
        return Response::error(
            &req.id,
            "invalid_request",
            "edit_match: 'match' must be a non-empty string",
        );
    }

    let replacement = match req.params.get("replacement").and_then(|v| v.as_str()) {
        Some(r) => r,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "edit_match: missing required param 'replacement'",
            );
        }
    };

    // No custom escape interpretation. JSON transport already handles escape
    // sequences before the string reaches us. Adding unescape_str on top caused
    // double-interpretation that corrupted source code with literal escapes.

    // Detect glob pattern
    if is_glob_pattern(file) {
        return handle_glob_edit_match(req, ctx, file, match_str, replacement);
    }

    // Single-file path
    handle_single_file_edit_match(req, ctx, file, match_str, replacement)
}

/// Returns true if the file path contains glob characters.
fn is_glob_pattern(path: &str) -> bool {
    path.contains('*') || path.contains('?') || path.contains('{') || path.contains('[')
}

/// Handle a glob-based multi-file edit_match.
fn handle_glob_edit_match(
    req: &RawRequest,
    ctx: &AppContext,
    pattern: &str,
    match_str: &str,
    replacement: &str,
) -> Response {
    let dry_run = edit::is_dry_run(&req.params);

    // Resolve glob relative to project root (or cwd)
    let config = ctx.config();
    let root = config
        .project_root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    drop(config);
    let full_pattern = if pattern.starts_with('/') {
        pattern.to_string()
    } else {
        format!("{}/{}", root.display(), pattern)
    };

    let mut paths: Vec<std::path::PathBuf> = match glob::glob(&full_pattern) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|p| p.is_file())
            .collect(),
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("edit_match: invalid glob pattern: {}", e),
            );
        }
    };
    paths.sort();

    if paths.is_empty() {
        return Response::error(
            &req.id,
            "match_not_found",
            format!("edit_match: no files matched glob '{}'", pattern),
        );
    }

    let config = ctx.config();
    let mut file_results: Vec<serde_json::Value> = Vec::new();
    let mut total_replacements: usize = 0;
    let mut total_files: usize = 0;
    let mut diffs: Vec<serde_json::Value> = Vec::new();

    // --- Phase 1: Bulk edit — backup + write all files (fast) ---
    struct PendingEdit {
        path: std::path::PathBuf,
        file_str: String,
        new_source: String,
        count: usize,
    }
    let mut pending: Vec<PendingEdit> = Vec::new();

    for path in &paths {
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let positions: Vec<usize> = source
            .match_indices(match_str)
            .map(|(idx, _)| idx)
            .collect();

        if positions.is_empty() {
            continue;
        }

        let count = positions.len();
        let new_source = source.replace(match_str, replacement);
        let file_str = path.display().to_string();

        if dry_run {
            let dr = edit::dry_run_diff(&source, &new_source, &path);
            diffs.push(serde_json::json!({
                "file": file_str,
                "replacements": count,
                "diff": dr.diff,
                "syntax_valid": dr.syntax_valid,
            }));
            total_replacements += count;
            total_files += 1;
            continue;
        }

        // Backup before mutation
        let validated_path = match validate_glob_edit_path(ctx, &req.id, path) {
            Ok(validated) => validated,
            Err(resp) => return resp,
        };

        pending.push(PendingEdit {
            path: validated_path,
            file_str,
            new_source,
            count,
        });
        total_replacements += count;
        total_files += 1;
    }

    if !dry_run && pending.is_empty() {
        return Response::error(
            &req.id,
            "match_not_found",
            format!(
                "edit_match: '{}' not found in any files matching '{}'",
                match_str, pattern
            ),
        );
    }

    let checkpoint_name = if dry_run {
        None
    } else {
        let name = unique_glob_checkpoint_name();
        let files = pending
            .iter()
            .map(|edit| edit.path.clone())
            .collect::<Vec<_>>();
        let checkpoint_result = {
            let backup = ctx.backup().borrow();
            ctx.checkpoint()
                .borrow_mut()
                .create(req.session(), &name, files, &backup)
        };
        if let Err(e) = checkpoint_result {
            return Response::error(&req.id, e.code(), e.to_string());
        }
        Some(name)
    };

    if !dry_run {
        let mut written_paths: Vec<PathBuf> = Vec::new();

        for edit in &pending {
            if let Err(e) = edit::auto_backup(
                ctx,
                req.session(),
                &edit.path,
                &format!("glob_edit_match: {}", match_str),
            ) {
                if let Some(name) = &checkpoint_name {
                    delete_glob_checkpoint(ctx, req.session(), name);
                }
                return Response::error(&req.id, e.code(), e.to_string());
            }
        }

        // Write all changed files under a checkpoint-backed transaction. If any
        // write fails, restore files already written so callers never observe a
        // partially-applied glob edit.
        for edit in &pending {
            if let Err(e) = std::fs::write(&edit.path, &edit.new_source) {
                if let Some(name) = &checkpoint_name {
                    restore_glob_checkpoint(ctx, req.session(), name, &written_paths);
                    delete_glob_checkpoint(ctx, req.session(), name);
                }
                return Response::error(
                    &req.id,
                    "write_error",
                    format!("failed to write {}: {}", edit.file_str, e),
                );
            }
            written_paths.push(edit.path.clone());
        }
    }

    // --- Phase 2: Format all changed files (after all writes are done) ---
    for edit in &pending {
        let file_str = edit.path.display().to_string();
        let formatted = if !dry_run {
            match edit::write_format_only(&edit.path, &config) {
                Ok(formatted) => formatted,
                Err(e) => {
                    if let Some(name) = &checkpoint_name {
                        let paths = pending
                            .iter()
                            .map(|edit| edit.path.clone())
                            .collect::<Vec<_>>();
                        restore_glob_checkpoint(ctx, req.session(), name, &paths);
                        delete_glob_checkpoint(ctx, req.session(), name);
                    }
                    return Response::error(&req.id, e.code(), e.to_string());
                }
            }
        } else {
            false
        };
        let syntax_valid = if !dry_run {
            match validate_syntax(&edit.path) {
                Ok(valid) => valid,
                Err(e) => {
                    if let Some(name) = &checkpoint_name {
                        let paths = pending
                            .iter()
                            .map(|edit| edit.path.clone())
                            .collect::<Vec<_>>();
                        restore_glob_checkpoint(ctx, req.session(), name, &paths);
                        delete_glob_checkpoint(ctx, req.session(), name);
                    }
                    return Response::error(&req.id, e.code(), e.to_string());
                }
            }
        } else {
            None
        };

        if let Ok(final_content) = std::fs::read_to_string(&edit.path) {
            ctx.lsp_notify_file_changed(&edit.path, &final_content);
        }

        file_results.push(serde_json::json!({
            "file": file_str,
            "replacements": edit.count,
            "formatted": formatted,
            "syntax_valid": syntax_valid,
        }));
    }

    if dry_run {
        if diffs.is_empty() {
            return Response::error(
                &req.id,
                "match_not_found",
                format!(
                    "edit_match: '{}' not found in any files matching '{}'",
                    match_str, pattern
                ),
            );
        }
        return Response::success(
            &req.id,
            serde_json::json!({
                "ok": true,
                "dry_run": true,
                "files": diffs,
                "total_replacements": total_replacements,
                "total_files": total_files,
            }),
        );
    }

    if let Some(name) = &checkpoint_name {
        delete_glob_checkpoint(ctx, req.session(), name);
    }

    log::debug!(
        "[aft] edit_match (glob): {} replacements across {} files",
        total_replacements,
        total_files
    );

    Response::success(
        &req.id,
        serde_json::json!({
            "ok": true,
            "files": file_results,
            "total_replacements": total_replacements,
            "total_files": total_files,
        }),
    )
}

fn unique_glob_checkpoint_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("__edit_match_glob_{}__", nanos)
}

fn restore_glob_checkpoint(ctx: &AppContext, session: &str, name: &str, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    if let Err(e) = ctx
        .checkpoint()
        .borrow()
        .restore_validated(session, name, paths)
    {
        log::warn!(
            "[aft] edit_match glob rollback: failed to restore checkpoint {}: {}",
            name,
            e
        );
    }
}

fn delete_glob_checkpoint(ctx: &AppContext, session: &str, name: &str) {
    ctx.checkpoint().borrow_mut().delete(session, name);
}

fn validate_glob_edit_path(
    ctx: &AppContext,
    req_id: &str,
    path: &Path,
) -> Result<std::path::PathBuf, Response> {
    ctx.validate_path(req_id, path)
}

/// Handle a single-file edit_match (original behavior).
fn handle_single_file_edit_match(
    req: &RawRequest,
    ctx: &AppContext,
    file: &str,
    match_str: &str,
    replacement: &str,
) -> Response {
    let occurrence = req
        .params
        .get("occurrence")
        .and_then(|v| v.as_i64())
        .map(|v| v as usize);

    let replace_all = req
        .params
        .get("replace_all")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let path = match ctx.validate_path(&req.id, Path::new(file)) {
        Ok(path) => path,
        Err(resp) => return resp,
    };
    if !path.exists() {
        return Response::error(
            &req.id,
            "file_not_found",
            format!("file not found: {}", file),
        );
    }

    let source = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return Response::error(&req.id, "file_not_found", format!("{}: {}", file, e));
        }
    };

    // Find all positions using progressive fuzzy matching:
    // Pass 1: exact, Pass 2: rstrip, Pass 3: trim, Pass 4: normalized Unicode
    let fuzzy_matches = crate::fuzzy_match::find_all_fuzzy(&source, match_str);

    if fuzzy_matches.is_empty() {
        return Response::error(
            &req.id,
            "match_not_found",
            format!("edit_match: '{}' not found in {}", match_str, file),
        );
    }

    // Log if fuzzy match was needed (not exact)
    if fuzzy_matches[0].pass > 1 {
        log::debug!(
            "[aft] edit_match: fuzzy match (pass {}) for '{}' in {}",
            fuzzy_matches[0].pass,
            match_str,
            file
        );
    }

    let positions: Vec<usize> = fuzzy_matches.iter().map(|m| m.byte_start).collect();

    // If occurrence specified but out of range (only relevant when not replace_all)
    if !replace_all {
        if let Some(occ) = occurrence {
            if occ >= positions.len() {
                return Response::error(
                    &req.id,
                    "invalid_request",
                    format!(
                        "edit_match: occurrence {} out of range, file has {} occurrence(s)",
                        occ,
                        positions.len()
                    ),
                );
            }
        }
    }

    // Multiple matches without occurrence selector → disambiguation (unless replace_all)
    if positions.len() > 1 && occurrence.is_none() && !replace_all {
        let occurrences: Vec<serde_json::Value> = positions
            .iter()
            .enumerate()
            .map(|(idx, &byte_pos)| {
                let line = source[..byte_pos].matches('\n').count();
                let context = build_context(&source, line, 2);
                serde_json::json!({
                    "index": idx,
                    "line": line + 1,
                    "context": context,
                })
            })
            .collect();

        return Response::error_with_data(
            &req.id,
            "ambiguous_match",
            format!(
                "Found {} matches. Use 'occurrence' (0-indexed) to select one, or 'replaceAll: true' to replace all.",
                occurrences.len()
            ),
            serde_json::json!({
                "occurrences": occurrences,
            }),
        );
    }

    // Auto-backup before mutation (skip for dry-run)
    let backup_id = if !edit::is_dry_run(&req.params) {
        let label = if replace_all {
            format!(
                "edit_match: {} (replace_all x{})",
                match_str,
                positions.len()
            )
        } else {
            format!("edit_match: {}", match_str)
        };
        match edit::auto_backup(ctx, req.session(), path.as_path(), &label) {
            Ok(id) => id,
            Err(e) => {
                return Response::error(&req.id, e.code(), e.to_string());
            }
        }
    } else {
        None
    };

    // Apply edit(s) — use fuzzy match byte lengths (may differ from match_str.len())
    let (new_source, count) = if replace_all {
        let count = fuzzy_matches.len();
        // Apply replacements in reverse order to preserve byte offsets
        let mut result = source.clone();
        for m in fuzzy_matches.iter().rev() {
            result = match edit::replace_byte_range(
                &result,
                m.byte_start,
                m.byte_start + m.byte_len,
                replacement,
            ) {
                Ok(updated) => updated,
                Err(e) => {
                    return Response::error(&req.id, e.code(), e.to_string());
                }
            };
        }
        (result, count)
    } else {
        let target_idx = occurrence.unwrap_or(0);
        let m = &fuzzy_matches[target_idx];
        (
            match edit::replace_byte_range(
                &source,
                m.byte_start,
                m.byte_start + m.byte_len,
                replacement,
            ) {
                Ok(updated) => updated,
                Err(e) => {
                    return Response::error(&req.id, e.code(), e.to_string());
                }
            },
            1,
        )
    };

    // Dry-run: return diff without modifying disk
    if edit::is_dry_run(&req.params) {
        let dr = edit::dry_run_diff(&source, &new_source, path.as_path());
        return Response::success(
            &req.id,
            serde_json::json!({
                "ok": true, "dry_run": true, "diff": dr.diff, "syntax_valid": dr.syntax_valid,
            }),
        );
    }

    // Write, format, and validate via shared pipeline
    let mut write_result = match edit::write_format_validate(
        path.as_path(),
        &new_source,
        &ctx.config(),
        &req.params,
    ) {
        Ok(r) => r,
        Err(e) => {
            return Response::error(&req.id, e.code(), e.to_string());
        }
    };

    if let Ok(final_content) = std::fs::read_to_string(path.as_path()) {
        write_result.lsp_diagnostics =
            ctx.lsp_post_write(path.as_path(), &final_content, &req.params);
    }

    log::debug!("edit_match: {} in {}", match_str, file);

    let syntax_valid = write_result.syntax_valid.unwrap_or(true);

    let mut result = serde_json::json!({
        "file": file,
        "replacements": count,
        "syntax_valid": syntax_valid,
        "formatted": write_result.formatted,
    });

    if let Some(ref reason) = write_result.format_skipped_reason {
        result["format_skipped_reason"] = serde_json::json!(reason);
    }

    if write_result.validate_requested {
        result["validation_errors"] = serde_json::json!(write_result.validation_errors);
    }
    if let Some(ref reason) = write_result.validate_skipped_reason {
        result["validate_skipped_reason"] = serde_json::json!(reason);
    }

    if let Some(ref id) = backup_id {
        result["backup_id"] = serde_json::json!(id);
    }

    write_result.append_lsp_diagnostics_to(&mut result);

    // Include diff info if requested (for UI metadata)
    if edit::wants_diff(&req.params) {
        let final_content = std::fs::read_to_string(&path).unwrap_or_else(|_| new_source);
        result["diff"] = edit::compute_diff_info(&source, &final_content);
    }

    Response::success(&req.id, result)
}

/// Build a context string showing the target line ± `margin` lines.
fn build_context(source: &str, target_line: usize, margin: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = target_line.saturating_sub(margin);
    let end = (target_line + margin + 1).min(lines.len());
    lines[start..end].join("\n")
}
