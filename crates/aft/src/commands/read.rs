//! Handler for the `read` command: fast file/directory reading with line numbers.
//!
//! This is the simple "give me file contents" command, designed to replace
//! opencode's built-in read tool with a faster Rust implementation.
//! For symbol-based reading and call-graph annotations, use `zoom` instead.

use std::fs;
use std::path::Path;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};

const DEFAULT_LIMIT: u32 = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_BYTES: usize = 50 * 1024; // 50KB output cap

/// Check if file content is binary by scanning for null bytes in first 8KB.
fn is_binary(content: &[u8]) -> bool {
    let check_len = content.len().min(8192);
    content[..check_len].contains(&0)
}

/// Handle a `read` request.
///
/// Params:
///   - `file` (string, required) — path to file or directory
///   - `start_line` (u32, optional) — 1-based start line (default: 1)
///   - `end_line` (u32, optional) — 1-based end line (default: start_line + limit - 1)
///   - `limit` (u32, optional) — max lines to return (default: 2000)
///
/// Returns for files:
///   `{ content, total_lines, lines_read, start_line, end_line, truncated, byte_size }`
///
/// Returns for directories:
///   `{ entries[], total_entries }`
///
/// Returns for binary files:
///   `{ binary: true, byte_size }`
pub fn handle_read(req: &RawRequest, _ctx: &AppContext) -> Response {
    let file = match req.params.get("file").and_then(|v| v.as_str()) {
        Some(f) => f,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "read: missing required param 'file'",
            );
        }
    };

    let path = Path::new(file);

    // Check existence
    if !path.exists() {
        return Response::error(
            &req.id,
            "not_found",
            format!("read: file not found: {}", file),
        );
    }

    // Directory listing
    if path.is_dir() {
        return handle_directory(req, path);
    }

    // Read raw bytes for binary detection
    let raw_bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return Response::error(
                &req.id,
                "io_error",
                format!("read: failed to read file: {}", e),
            );
        }
    };

    let byte_size = raw_bytes.len();

    // Binary detection
    if is_binary(&raw_bytes) {
        return Response::success(
            &req.id,
            serde_json::json!({
                "binary": true,
                "byte_size": byte_size,
                "message": format!("Binary file ({} bytes), cannot display as text", byte_size),
            }),
        );
    }

    // Convert to string
    let content = match String::from_utf8(raw_bytes) {
        Ok(s) => s,
        Err(_) => {
            return Response::success(
                &req.id,
                serde_json::json!({
                    "binary": true,
                    "byte_size": byte_size,
                    "message": format!("Binary file ({} bytes), not valid UTF-8", byte_size),
                }),
            );
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len() as u32;

    // Parse range parameters
    let limit = req
        .params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or(DEFAULT_LIMIT);

    let start_line = req
        .params
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|v| v.max(1) as u32)
        .unwrap_or(1);

    let end_line = req
        .params
        .get("end_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .unwrap_or_else(|| (start_line + limit - 1).min(total_lines));

    // Clamp to actual line count
    let start_idx = (start_line.saturating_sub(1) as usize).min(lines.len());
    let end_idx = (end_line as usize).min(lines.len());

    if start_idx >= lines.len() {
        return Response::success(
            &req.id,
            serde_json::json!({
                "content": "",
                "total_lines": total_lines,
                "lines_read": 0,
                "start_line": start_line,
                "end_line": start_line,
                "truncated": false,
                "byte_size": byte_size,
            }),
        );
    }

    // Build line-numbered output with truncation
    let mut output = String::new();
    let mut output_bytes = 0usize;
    let mut lines_read = 0u32;
    let mut truncated_by_size = false;

    let line_num_width = format!("{}", end_idx).len();

    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        let line_num = start_idx + i + 1; // 1-based
        let display_line = if line.len() > MAX_LINE_LENGTH {
            format!(
                "{:>width$}: {}... (truncated)\n",
                line_num,
                &line[..MAX_LINE_LENGTH],
                width = line_num_width
            )
        } else {
            format!("{:>width$}: {}\n", line_num, line, width = line_num_width)
        };

        output_bytes += display_line.len();
        if output_bytes > MAX_BYTES {
            truncated_by_size = true;
            // Add truncation notice
            output.push_str(&format!(
                "... (output truncated at {}KB, use start_line/end_line to read sections)\n",
                MAX_BYTES / 1024
            ));
            break;
        }

        output.push_str(&display_line);
        lines_read += 1;
    }

    let actual_end = start_line + lines_read - if lines_read > 0 { 1 } else { 0 };
    let has_more = (end_idx as u32) < total_lines || truncated_by_size;

    Response::success(
        &req.id,
        serde_json::json!({
            "content": output,
            "total_lines": total_lines,
            "lines_read": lines_read,
            "start_line": start_line,
            "end_line": actual_end,
            "truncated": has_more,
            "byte_size": byte_size,
        }),
    )
}

/// Handle directory listing.
fn handle_directory(req: &RawRequest, path: &Path) -> Response {
    let mut entries: Vec<String> = Vec::new();

    let read_dir = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            return Response::error(
                &req.id,
                "io_error",
                format!("read: failed to read directory: {}", e),
            );
        }
    };

    for entry_result in read_dir {
        let entry = match entry_result {
            Ok(e) => e,
            Err(_) => continue,
        };

        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);

        if is_dir {
            entries.push(format!("{}/", name));
        } else {
            entries.push(name);
        }
    }

    entries.sort();

    let total = entries.len();
    Response::success(
        &req.id,
        serde_json::json!({
            "entries": entries,
            "total_entries": total,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_binary_detects_null_bytes() {
        assert!(is_binary(&[0x48, 0x65, 0x6c, 0x00, 0x6f]));
        assert!(!is_binary(b"Hello, world!"));
        assert!(!is_binary(b""));
    }

    #[test]
    fn test_is_binary_checks_first_8kb() {
        let mut data = vec![0x41u8; 16384]; // 16KB of 'A'
        data[10000] = 0; // null byte after 8KB boundary
        assert!(!is_binary(&data)); // should not detect — null is past 8KB

        data[100] = 0; // null byte within 8KB
        assert!(is_binary(&data));
    }
}
