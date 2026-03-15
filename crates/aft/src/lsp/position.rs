use lsp_types::{Position, Range, TextDocumentIdentifier, TextDocumentPositionParams};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::LspError;

/// Convert AFT 1-based (or compatibility 0-based) line/column values to an LSP 0-based position.
pub fn to_lsp_position(line: u32, column: u32) -> Position {
    Position::new(line.saturating_sub(1), column.saturating_sub(1))
}

/// Convert an LSP 0-based position to AFT 1-based line/column values.
pub fn from_lsp_position(position: &Position) -> (u32, u32) {
    (position.line + 1, position.character + 1)
}

/// Convert an LSP Range to a 1-based AFT range as a JSON-friendly tuple.
pub fn lsp_range_to_aft(range: &Range) -> (u32, u32, u32, u32) {
    let (start_line, start_column) = from_lsp_position(&range.start);
    let (end_line, end_column) = from_lsp_position(&range.end);
    (start_line, start_column, end_line, end_column)
}

/// Build a TextDocumentPositionParams from a file path and 1-based line/column.
pub fn text_document_position(
    file_path: &Path,
    line: u32,
    column: u32,
) -> Result<TextDocumentPositionParams, LspError> {
    let uri = uri_for_path(file_path)?;
    Ok(TextDocumentPositionParams {
        text_document: TextDocumentIdentifier::new(uri),
        position: to_lsp_position(line, column),
    })
}

/// Backwards-compatible alias for existing internal call sites.
pub fn build_text_document_position(
    file_path: &Path,
    line: u32,
    column: u32,
) -> Result<TextDocumentPositionParams, LspError> {
    text_document_position(file_path, line, column)
}

/// Convert a file path to an LSP URI.
pub fn uri_for_path(path: &Path) -> Result<lsp_types::Uri, LspError> {
    let url = url::Url::from_file_path(path).map_err(|_| {
        LspError::NotFound(format!(
            "failed to convert '{}' to file URI",
            path.display()
        ))
    })?;
    lsp_types::Uri::from_str(url.as_str()).map_err(|_| {
        LspError::NotFound(format!("failed to parse file URI for '{}'", path.display()))
    })
}

fn normalize_lookup_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Convert an LSP URI to a PathBuf.
pub fn uri_to_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    url.to_file_path()
        .ok()
        .map(|path| normalize_lookup_path(&path))
}
