use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::lsp::registry::ServerKind;

/// A single diagnostic from an LSP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDiagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

impl DiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Information => "information",
            Self::Hint => "hint",
        }
    }
}

/// Stores diagnostics from all LSP servers.
///
/// Uses replacement semantics: each publishDiagnostics notification replaces
/// all diagnostics for that (server, file) pair. An empty diagnostics array
/// clears the entry.
pub struct DiagnosticsStore {
    /// Key: (ServerKind, canonical file path)
    /// Value: list of diagnostics for that file from that server
    store: HashMap<(ServerKind, PathBuf), Vec<StoredDiagnostic>>,
}

impl DiagnosticsStore {
    pub fn new() -> Self {
        Self {
            store: HashMap::new(),
        }
    }

    /// Replace diagnostics for a (server, file) pair.
    pub fn publish(
        &mut self,
        server: ServerKind,
        file: PathBuf,
        diagnostics: Vec<StoredDiagnostic>,
    ) {
        if diagnostics.is_empty() {
            self.store.remove(&(server, file));
        } else {
            self.store.insert((server, file), diagnostics);
        }
    }

    /// Get all diagnostics for a specific file (from all servers).
    pub fn for_file(&self, file: &Path) -> Vec<&StoredDiagnostic> {
        self.store
            .iter()
            .filter(|((_, stored_file), _)| stored_file == file)
            .flat_map(|(_, diagnostics)| diagnostics.iter())
            .collect()
    }

    /// Get all diagnostics for a directory (all files under it).
    pub fn for_directory(&self, dir: &Path) -> Vec<&StoredDiagnostic> {
        self.store
            .iter()
            .filter(|((_, stored_file), _)| stored_file.starts_with(dir))
            .flat_map(|(_, diagnostics)| diagnostics.iter())
            .collect()
    }

    /// Get all stored diagnostics.
    pub fn all(&self) -> Vec<&StoredDiagnostic> {
        self.store.values().flat_map(|value| value.iter()).collect()
    }

    /// Clear all diagnostics for a server (e.g. on server restart).
    pub fn clear_server(&mut self, server: ServerKind) {
        self.store
            .retain(|(stored_server, _), _| *stored_server != server);
    }
}

impl Default for DiagnosticsStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert LSP diagnostics to our stored format.
/// LSP uses 0-based line/character; we convert to 1-based.
pub fn from_lsp_diagnostics(
    file: PathBuf,
    lsp_diagnostics: Vec<lsp_types::Diagnostic>,
) -> Vec<StoredDiagnostic> {
    lsp_diagnostics
        .into_iter()
        .map(|diagnostic| StoredDiagnostic {
            file: file.clone(),
            line: diagnostic.range.start.line + 1,
            column: diagnostic.range.start.character + 1,
            end_line: diagnostic.range.end.line + 1,
            end_column: diagnostic.range.end.character + 1,
            severity: match diagnostic.severity {
                Some(lsp_types::DiagnosticSeverity::ERROR) => DiagnosticSeverity::Error,
                Some(lsp_types::DiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
                Some(lsp_types::DiagnosticSeverity::INFORMATION) => DiagnosticSeverity::Information,
                Some(lsp_types::DiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
                _ => DiagnosticSeverity::Warning,
            },
            message: diagnostic.message,
            code: diagnostic.code.map(|code| match code {
                lsp_types::NumberOrString::Number(value) => value.to_string(),
                lsp_types::NumberOrString::String(value) => value,
            }),
            source: diagnostic.source,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lsp_types::{
        Diagnostic, DiagnosticSeverity as LspDiagnosticSeverity, NumberOrString, Position, Range,
    };

    use super::{from_lsp_diagnostics, DiagnosticSeverity, DiagnosticsStore, StoredDiagnostic};
    use crate::lsp::registry::ServerKind;

    #[test]
    fn converts_lsp_positions_to_one_based() {
        let file = PathBuf::from("/tmp/demo.rs");
        let diagnostics = from_lsp_diagnostics(
            file.clone(),
            vec![Diagnostic {
                range: Range::new(Position::new(0, 0), Position::new(1, 4)),
                severity: Some(LspDiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String("E1".into())),
                code_description: None,
                source: Some("fake".into()),
                message: "boom".into(),
                related_information: None,
                tags: None,
                data: None,
            }],
        );

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].file, file);
        assert_eq!(diagnostics[0].line, 1);
        assert_eq!(diagnostics[0].column, 1);
        assert_eq!(diagnostics[0].end_line, 2);
        assert_eq!(diagnostics[0].end_column, 5);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostics[0].code.as_deref(), Some("E1"));
    }

    #[test]
    fn publish_replaces_existing_file_diagnostics() {
        let file = PathBuf::from("/tmp/demo.rs");
        let mut store = DiagnosticsStore::new();

        store.publish(
            ServerKind::Rust,
            file.clone(),
            vec![StoredDiagnostic {
                file: file.clone(),
                line: 1,
                column: 1,
                end_line: 1,
                end_column: 2,
                severity: DiagnosticSeverity::Warning,
                message: "first".into(),
                code: None,
                source: None,
            }],
        );
        store.publish(
            ServerKind::Rust,
            file.clone(),
            vec![StoredDiagnostic {
                file: file.clone(),
                line: 2,
                column: 1,
                end_line: 2,
                end_column: 2,
                severity: DiagnosticSeverity::Error,
                message: "second".into(),
                code: None,
                source: None,
            }],
        );

        let stored = store.for_file(&file);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].message, "second");
    }
}
