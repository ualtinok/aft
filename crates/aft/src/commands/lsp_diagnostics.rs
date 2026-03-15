use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::Deserialize;

use crate::context::AppContext;
use crate::lsp::diagnostics::{DiagnosticSeverity, StoredDiagnostic};
use crate::protocol::{RawRequest, Response};

const MAX_WAIT_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
struct LspDiagnosticsParams {
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    wait_ms: Option<u64>,
}

pub fn handle_lsp_diagnostics(req: &RawRequest, ctx: &AppContext) -> Response {
    let params = match serde_json::from_value::<LspDiagnosticsParams>(req.params.clone()) {
        Ok(params) => params,
        Err(err) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("lsp_diagnostics: invalid params: {err}"),
            );
        }
    };

    if params.file.is_some() && params.directory.is_some() {
        return Response::error(
            &req.id,
            "invalid_request",
            "lsp_diagnostics: provide either 'file' or 'directory', not both",
        );
    }

    let wait_ms = params.wait_ms.unwrap_or(0);
    if wait_ms > MAX_WAIT_MS {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("lsp_diagnostics: wait_ms must be <= {MAX_WAIT_MS}"),
        );
    }

    let severity_filter = match parse_severity_filter(params.severity.as_deref()) {
        Ok(filter) => filter,
        Err(message) => return Response::error(&req.id, "invalid_request", message),
    };

    if wait_ms > 0 {
        thread::sleep(Duration::from_millis(wait_ms));
        let mut lsp = ctx.lsp();
        lsp.drain_events();
    }

    let diagnostics: Vec<StoredDiagnostic> = {
        let lsp = ctx.lsp();
        match (&params.file, &params.directory) {
            (Some(file), None) => {
                let path = normalize_query_path(Path::new(file));
                lsp.get_diagnostics_for_file(&path)
                    .into_iter()
                    .cloned()
                    .collect()
            }
            (None, Some(directory)) => {
                let path = normalize_query_path(Path::new(directory));
                lsp.get_diagnostics_for_directory(&path)
                    .into_iter()
                    .cloned()
                    .collect()
            }
            (None, None) => lsp.get_all_diagnostics().into_iter().cloned().collect(),
            _ => Vec::new(),
        }
    };

    let mut diagnostics: Vec<StoredDiagnostic> = diagnostics
        .into_iter()
        .filter(|diagnostic| severity_filter.matches(diagnostic.severity))
        .collect();

    diagnostics.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
            .then(left.end_line.cmp(&right.end_line))
            .then(left.end_column.cmp(&right.end_column))
            .then(left.message.cmp(&right.message))
    });

    let files_with_errors = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
        .map(|diagnostic| diagnostic.file.clone())
        .collect::<HashSet<PathBuf>>()
        .len();

    let diagnostics_json: Vec<serde_json::Value> = diagnostics
        .iter()
        .map(|diagnostic| {
            serde_json::json!({
                "file": diagnostic.file.display().to_string(),
                "line": diagnostic.line,
                "column": diagnostic.column,
                "end_line": diagnostic.end_line,
                "end_column": diagnostic.end_column,
                "severity": diagnostic.severity.as_str(),
                "message": diagnostic.message,
                "code": diagnostic.code,
                "source": diagnostic.source,
            })
        })
        .collect();

    Response::success(
        &req.id,
        serde_json::json!({
            "diagnostics": diagnostics_json,
            "total": diagnostics_json.len(),
            "files_with_errors": files_with_errors,
        }),
    )
}

#[derive(Debug, Clone, Copy)]
enum SeverityFilter {
    All,
    Only(DiagnosticSeverity),
}

impl SeverityFilter {
    fn matches(self, severity: DiagnosticSeverity) -> bool {
        match self {
            Self::All => true,
            Self::Only(expected) => expected == severity,
        }
    }
}

fn parse_severity_filter(value: Option<&str>) -> Result<SeverityFilter, String> {
    match value.unwrap_or("all") {
        "all" => Ok(SeverityFilter::All),
        "error" => Ok(SeverityFilter::Only(DiagnosticSeverity::Error)),
        "warning" => Ok(SeverityFilter::Only(DiagnosticSeverity::Warning)),
        "information" => Ok(SeverityFilter::Only(DiagnosticSeverity::Information)),
        "hint" => Ok(SeverityFilter::Only(DiagnosticSeverity::Hint)),
        other => Err(format!(
            "lsp_diagnostics: invalid severity '{other}' (expected error, warning, information, hint, or all)"
        )),
    }
}

fn normalize_query_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
