//! Shared edit engine: byte-offset conversion, content replacement,
//! syntax validation, and auto-backup orchestration.
//!
//! Used by `write`, `edit_symbol`, `edit_match`, and `batch` commands.

use std::path::Path;

use crate::config::Config;
use crate::context::AppContext;
use crate::error::AftError;
use crate::format;
use crate::parser::{detect_language, grammar_for, FileParser};

/// Convert 0-indexed line/col to a byte offset within `source`.
///
/// Tree-sitter columns are byte-indexed within the line, so `col` is a byte
/// offset from the start of the line (not a character offset).
///
/// Returns `source.len()` if line is beyond the end of the file.
pub fn line_col_to_byte(source: &str, line: u32, col: u32) -> usize {
    let mut byte = 0;
    for (i, l) in source.lines().enumerate() {
        if i == line as usize {
            return byte + (col as usize).min(l.len());
        }
        byte += l.len() + 1; // +1 for the newline character
    }
    // If we run out of lines, clamp to source length.
    source.len()
}

/// Replace bytes in `[start..end)` with `replacement`.
///
/// Panics if `start > end` or indices are out of bounds for the source.
pub fn replace_byte_range(source: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut result = String::with_capacity(source.len() - (end - start) + replacement.len());
    result.push_str(&source[..start]);
    result.push_str(replacement);
    result.push_str(&source[end..]);
    result
}

/// Validate syntax of a file using a fresh FileParser (D023).
///
/// Returns `Ok(Some(true))` if syntax is valid, `Ok(Some(false))` if there are
/// parse errors, and `Ok(None)` if the language is unsupported.
pub fn validate_syntax(path: &Path) -> Result<Option<bool>, AftError> {
    let mut parser = FileParser::new();
    match parser.parse(path) {
        Ok((tree, _lang)) => Ok(Some(!tree.root_node().has_error())),
        Err(AftError::InvalidRequest { .. }) => {
            // Unsupported language — not an error, just can't validate
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Validate syntax of an in-memory string without touching disk.
///
/// Uses `detect_language(path)` + `grammar_for(lang)` + `parser.parse()`
/// to validate syntax of a proposed content string. Returns `None` for
/// unsupported languages, `Some(true)` for valid, `Some(false)` for invalid.
pub fn validate_syntax_str(content: &str, path: &Path) -> Option<bool> {
    let lang = detect_language(path)?;
    let grammar = grammar_for(lang);
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&grammar).is_err() {
        return None;
    }
    let tree = parser.parse(content.as_bytes(), None)?;
    Some(!tree.root_node().has_error())
}

/// Result of a dry-run diff computation.
pub struct DryRunResult {
    /// Unified diff between original and proposed content.
    pub diff: String,
    /// Whether the proposed content has valid syntax. `None` for unsupported languages.
    pub syntax_valid: Option<bool>,
}

/// Compute a unified diff between original and proposed content, plus syntax validation.
///
/// Returns a standard unified diff with `a/` and `b/` path prefixes and 3 lines of context.
/// Also validates syntax of the proposed content via tree-sitter.
pub fn dry_run_diff(original: &str, proposed: &str, path: &Path) -> DryRunResult {
    let display_path = path.display().to_string();
    let text_diff = similar::TextDiff::from_lines(original, proposed);
    let diff = text_diff
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("a/{}", display_path),
            &format!("b/{}", display_path),
        )
        .to_string();
    let syntax_valid = validate_syntax_str(proposed, path);
    DryRunResult { diff, syntax_valid }
}

/// Extract the `dry_run` boolean from request params.
///
/// Returns `true` if `params["dry_run"]` is `true`, `false` otherwise.
pub fn is_dry_run(params: &serde_json::Value) -> bool {
    params
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Snapshot the file into the backup store before mutation.
///
/// Returns `Ok(Some(backup_id))` if the file existed and was backed up,
/// `Ok(None)` if the file doesn't exist (new file creation).
///
/// Drops the RefCell borrow before returning (D029).
pub fn auto_backup(
    ctx: &AppContext,
    path: &Path,
    description: &str,
) -> Result<Option<String>, AftError> {
    if !path.exists() {
        return Ok(None);
    }
    let backup_id = {
        let mut store = ctx.backup().borrow_mut();
        store.snapshot(path, description)?
    }; // borrow dropped here
    Ok(Some(backup_id))
}

/// Result of the write → format → validate pipeline.
///
/// Returned by `write_format_validate` to give callers a single struct
/// with all post-write signals for the response JSON.
pub struct WriteResult {
    /// Whether tree-sitter syntax validation passed. `None` if unsupported language.
    pub syntax_valid: Option<bool>,
    /// Whether the file was auto-formatted.
    pub formatted: bool,
    /// Why formatting was skipped, if it was. Values: "not_found", "timeout", "error", "unsupported_language".
    pub format_skipped_reason: Option<String>,
    /// Whether full validation was requested (controls whether validation_errors is included in response).
    pub validate_requested: bool,
    /// Structured type-checker errors (only populated when validate:"full" is requested).
    pub validation_errors: Vec<format::ValidationError>,
    /// Why validation was skipped, if it was. Values: "not_found", "timeout", "error", "unsupported_language".
    pub validate_skipped_reason: Option<String>,
    /// LSP diagnostics for the edited file. Only populated when `diagnostics: true` is
    /// passed in the edit request AND a language server is available.
    pub lsp_diagnostics: Vec<crate::lsp::diagnostics::StoredDiagnostic>,
}

impl WriteResult {
    /// Append LSP diagnostics to a response JSON object.
    /// Only adds the field when diagnostics were requested and collected.
    pub fn append_lsp_diagnostics_to(&self, result: &mut serde_json::Value) {
        if !self.lsp_diagnostics.is_empty() {
            result["lsp_diagnostics"] = serde_json::json!(self
                .lsp_diagnostics
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "file": d.file.display().to_string(),
                        "line": d.line,
                        "column": d.column,
                        "end_line": d.end_line,
                        "end_column": d.end_column,
                        "severity": d.severity.as_str(),
                        "message": d.message,
                        "code": d.code,
                        "source": d.source,
                    })
                })
                .collect::<Vec<_>>());
        }
    }
}

/// Write content to disk, auto-format, then validate syntax.
///
/// This is the shared tail for all mutation commands. The pipeline order is:
/// 1. `fs::write` — persist content
/// 2. `auto_format` — run the project formatter (reads the written file, writes back)
/// 3. `validate_syntax` — parse the (potentially formatted) file
/// 4. `validate_full` — run type checker if `params.validate == "full"`
///
/// The `params` argument carries the original request parameters. When it
/// contains `"validate": "full"`, the project's type checker is invoked after
/// syntax validation and the results are included in `WriteResult`.
pub fn write_format_validate(
    path: &Path,
    content: &str,
    config: &Config,
    params: &serde_json::Value,
) -> Result<WriteResult, AftError> {
    // Step 1: Write
    std::fs::write(path, content).map_err(|e| AftError::InvalidRequest {
        message: format!("failed to write file: {}", e),
    })?;

    // Step 2: Format (before validate so we validate the formatted content)
    let (formatted, format_skipped_reason) = format::auto_format(path, config);

    // Step 3: Validate syntax
    let syntax_valid = match validate_syntax(path) {
        Ok(sv) => sv,
        Err(_) => None,
    };

    // Step 4: Full validation (type checker) — only when requested
    let validate_requested = params.get("validate").and_then(|v| v.as_str()) == Some("full");
    let (validation_errors, validate_skipped_reason) = if validate_requested {
        format::validate_full(path, config)
    } else {
        (Vec::new(), None)
    };

    Ok(WriteResult {
        syntax_valid,
        formatted,
        format_skipped_reason,
        validate_requested,
        validation_errors,
        validate_skipped_reason,
        lsp_diagnostics: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- line_col_to_byte ---

    #[test]
    fn line_col_to_byte_empty_string() {
        assert_eq!(line_col_to_byte("", 0, 0), 0);
    }

    #[test]
    fn line_col_to_byte_single_line() {
        let source = "hello";
        assert_eq!(line_col_to_byte(source, 0, 0), 0);
        assert_eq!(line_col_to_byte(source, 0, 3), 3);
        assert_eq!(line_col_to_byte(source, 0, 5), 5); // end of line
    }

    #[test]
    fn line_col_to_byte_multi_line() {
        let source = "abc\ndef\nghi\n";
        // line 0: "abc" at bytes 0..3, newline at 3
        assert_eq!(line_col_to_byte(source, 0, 0), 0);
        assert_eq!(line_col_to_byte(source, 0, 2), 2);
        // line 1: "def" at bytes 4..7, newline at 7
        assert_eq!(line_col_to_byte(source, 1, 0), 4);
        assert_eq!(line_col_to_byte(source, 1, 3), 7);
        // line 2: "ghi" at bytes 8..11, newline at 11
        assert_eq!(line_col_to_byte(source, 2, 0), 8);
        assert_eq!(line_col_to_byte(source, 2, 2), 10);
    }

    #[test]
    fn line_col_to_byte_last_line_no_trailing_newline() {
        let source = "abc\ndef";
        // line 1: "def" at bytes 4..7, no trailing newline
        assert_eq!(line_col_to_byte(source, 1, 0), 4);
        assert_eq!(line_col_to_byte(source, 1, 3), 7); // end
    }

    #[test]
    fn line_col_to_byte_multi_byte_utf8() {
        // "é" is 2 bytes in UTF-8
        let source = "café\nbar";
        // line 0: "café" is 5 bytes (c=1, a=1, f=1, é=2)
        assert_eq!(line_col_to_byte(source, 0, 0), 0);
        assert_eq!(line_col_to_byte(source, 0, 5), 5); // end of "café"
                                                       // line 1: "bar" starts at byte 6
        assert_eq!(line_col_to_byte(source, 1, 0), 6);
        assert_eq!(line_col_to_byte(source, 1, 2), 8);
    }

    #[test]
    fn line_col_to_byte_beyond_end() {
        let source = "abc";
        // Line beyond file returns source.len()
        assert_eq!(line_col_to_byte(source, 5, 0), source.len());
    }

    #[test]
    fn line_col_to_byte_col_clamped_to_line_length() {
        let source = "ab\ncd";
        // col=10 on a 2-char line should clamp to 2
        assert_eq!(line_col_to_byte(source, 0, 10), 2);
    }

    // --- replace_byte_range ---

    #[test]
    fn replace_byte_range_basic() {
        let source = "hello world";
        let result = replace_byte_range(source, 6, 11, "rust");
        assert_eq!(result, "hello rust");
    }

    #[test]
    fn replace_byte_range_delete() {
        let source = "hello world";
        let result = replace_byte_range(source, 5, 11, "");
        assert_eq!(result, "hello");
    }

    #[test]
    fn replace_byte_range_insert_at_same_position() {
        let source = "helloworld";
        let result = replace_byte_range(source, 5, 5, " ");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn replace_byte_range_replace_entire_string() {
        let source = "old content";
        let result = replace_byte_range(source, 0, source.len(), "new content");
        assert_eq!(result, "new content");
    }
}
