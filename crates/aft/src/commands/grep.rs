use std::collections::{BTreeMap, HashSet};
use std::env;
use std::path::Path;

use regex::RegexBuilder;

use crate::context::AppContext;
use crate::protocol::{RawRequest, Response};
use crate::search_index::{
    build_path_filters, read_searchable_text, resolve_search_scope,
    sort_grep_matches_by_mtime_desc, walk_project_files_from, GrepMatch, GrepResult, IndexStatus,
};

const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RAW_LINE_CHARS: usize = 2000;
const MAX_COMPRESSED_LINE_CHARS: usize = 200;
const MAX_MATCHES_PER_FILE: usize = 10;
const MAX_DISPLAY_MATCHES_PER_FILE: usize = 5;

pub fn handle_grep(req: &RawRequest, ctx: &AppContext) -> Response {
    let pattern = match req.params.get("pattern").and_then(|value| value.as_str()) {
        Some(pattern) => pattern,
        None => {
            return Response::error(
                &req.id,
                "invalid_request",
                "grep: missing required param 'pattern'",
            );
        }
    };

    let case_sensitive = req
        .params
        .get("case_sensitive")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    let include = string_array_param(&req.params, "include");
    let exclude = string_array_param(&req.params, "exclude");
    let max_results = req
        .params
        .get("max_results")
        .and_then(|value| value.as_u64())
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_MAX_RESULTS);
    let path = match req.params.get("path").and_then(|value| value.as_str()) {
        Some(path) => match ctx.validate_path(&req.id, Path::new(path)) {
            Ok(path) => Some(path.to_string_lossy().to_string()),
            Err(resp) => return resp,
        },
        None => None,
    };

    let mut regex_builder = RegexBuilder::new(pattern);
    regex_builder.case_insensitive(!case_sensitive);
    regex_builder.size_limit(10 * 1024 * 1024);
    if let Err(error) = regex_builder.build() {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("grep: invalid regex: {}", error),
        );
    }

    if let Err(error) = build_path_filters(&include, &exclude) {
        return Response::error(
            &req.id,
            "invalid_request",
            format!("grep: invalid include/exclude glob: {}", error),
        );
    }

    let project_root = ctx
        .config()
        .project_root
        .clone()
        .unwrap_or_else(|| env::current_dir().unwrap_or_default());
    let project_root = std::fs::canonicalize(&project_root).unwrap_or(project_root);
    let search_scope = resolve_search_scope(&project_root, path.as_deref());
    let fallback_status = if search_scope.use_index {
        current_index_status(ctx)
    } else {
        IndexStatus::Fallback
    };

    let result = {
        let search_index = ctx.search_index().borrow();
        match search_index.as_ref() {
            Some(index) if index.ready && search_scope.use_index => index.search_grep(
                pattern,
                case_sensitive,
                &include,
                &exclude,
                &search_scope.root,
                max_results,
            ),
            _ => {
                // For out-of-project paths, try ripgrep first for better performance
                if !search_scope.use_index {
                    if let Some(result) = ripgrep_grep(
                        &search_scope.root,
                        pattern,
                        case_sensitive,
                        &include,
                        &exclude,
                        max_results,
                    ) {
                        return Response::success(
                            &req.id,
                            serde_json::json!({
                                "text": format_grep_text(&result, ctx.config().compress_tool_output),
                                "matches": result.matches.iter().map(match_to_json).collect::<Vec<_>>(),
                                "total_matches": result.total_matches,
                                "files_searched": result.files_searched,
                                "files_with_matches": result.files_with_matches,
                                "index_status": result.index_status.as_str(),
                                "truncated": result.truncated,
                            }),
                        );
                    }
                }
                fallback_grep(
                    &project_root,
                    &search_scope.root,
                    pattern,
                    case_sensitive,
                    &include,
                    &exclude,
                    max_results,
                    fallback_status,
                )
            }
        }
    };
    let text = format_grep_text(&result, ctx.config().compress_tool_output);

    Response::success(
        &req.id,
        serde_json::json!({
            "text": text,
            "matches": result.matches.iter().map(match_to_json).collect::<Vec<_>>(),
            "total_matches": result.total_matches,
            "files_searched": result.files_searched,
            "files_with_matches": result.files_with_matches,
            "index_status": result.index_status.as_str(),
            "truncated": result.truncated,
        }),
    )
}

fn fallback_grep(
    project_root: &std::path::Path,
    search_root: &std::path::Path,
    pattern: &str,
    case_sensitive: bool,
    include: &[String],
    exclude: &[String],
    max_results: usize,
    index_status: IndexStatus,
) -> GrepResult {
    let filters = build_path_filters(include, exclude).unwrap_or_default();
    let filter_root = if search_root.starts_with(project_root) {
        project_root
    } else {
        search_root
    };
    let files = walk_project_files_from(filter_root, search_root, &filters);

    let mut regex_builder = RegexBuilder::new(pattern);
    regex_builder.case_insensitive(!case_sensitive);
    regex_builder.size_limit(10 * 1024 * 1024);
    let regex = match regex_builder.build() {
        Ok(regex) => regex,
        Err(_) => {
            return GrepResult {
                matches: Vec::new(),
                total_matches: 0,
                files_searched: 0,
                files_with_matches: 0,
                index_status,
                truncated: false,
            };
        }
    };

    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut files_searched = 0usize;
    let mut files_with_matches = 0usize;
    let mut truncated = false;

    for file in files {
        let Some(content) = read_searchable_text(&file) else {
            continue;
        };
        files_searched += 1;
        let line_starts = line_starts(&content);
        let mut seen_lines = HashSet::new();
        let mut matched_this_file = false;

        for matched in regex.find_iter(&content) {
            let (line, column, line_text) = line_details(&content, &line_starts, matched.start());
            if !seen_lines.insert(line) {
                continue;
            }

            total_matches += 1;
            if matches.len() < max_results {
                matches.push(GrepMatch {
                    file: file.clone(),
                    line,
                    column,
                    line_text,
                    match_text: matched.as_str().to_string(),
                });
            } else {
                truncated = true;
            }
            matched_this_file = true;
        }

        if matched_this_file {
            files_with_matches += 1;
        }
    }

    sort_grep_matches_by_mtime_desc(&mut matches, project_root);

    GrepResult {
        total_matches,
        matches,
        files_searched,
        files_with_matches,
        index_status,
        truncated,
    }
}

/// Shell out to ripgrep for out-of-project searches.
/// Matches OpenCode's ripgrep invocation, but uses `--json` for robust parsing.
fn ripgrep_grep(
    search_root: &std::path::Path,
    pattern: &str,
    case_sensitive: bool,
    include: &[String],
    exclude: &[String],
    max_results: usize,
) -> Option<GrepResult> {
    use std::process::Command;

    let rg = which_rg()?;
    let mut cmd = Command::new(rg);
    cmd.args(["-nH", "--hidden", "--no-messages", "--json"]);
    if !case_sensitive {
        cmd.arg("-i");
    }
    for inc in include {
        cmd.args(["--glob", inc]);
    }
    for exc in exclude {
        let negated = if exc.starts_with('!') {
            exc.clone()
        } else {
            format!("!{}", exc)
        };
        cmd.args(["--glob", &negated]);
    }
    cmd.arg("--regexp").arg(pattern).arg(search_root);

    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut matches = Vec::new();
    let mut total_matches = 0usize;
    let mut files_with_matches_set: HashSet<std::path::PathBuf> = HashSet::new();
    let mut truncated = false;

    for line in stdout.lines() {
        let parsed: serde_json::Value = serde_json::from_str(line).ok()?;
        if parsed.get("type").and_then(|value| value.as_str()) != Some("match") {
            continue;
        }

        let data = parsed.get("data")?;
        let file_str = data
            .get("path")
            .and_then(|value| value.get("text"))
            .and_then(|value| value.as_str())?;
        let line_num = data
            .get("line_number")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())?;
        let line_text = data
            .get("lines")
            .and_then(|value| value.get("text"))
            .and_then(|value| value.as_str())?
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let file_path = std::path::PathBuf::from(file_str);

        total_matches += 1;
        files_with_matches_set.insert(file_path.clone());

        if matches.len() < max_results {
            matches.push(GrepMatch {
                file: file_path,
                line: line_num,
                column: 0,
                line_text,
                match_text: String::new(),
            });
        } else {
            truncated = true;
        }
    }

    Some(GrepResult {
        total_matches,
        matches,
        files_searched: 0, // rg doesn't report this
        files_with_matches: files_with_matches_set.len(),
        index_status: IndexStatus::Fallback,
        truncated,
    })
}

/// Shell out to ripgrep for out-of-project glob (file listing).
/// Matches OpenCode's: `rg --files --hidden --glob=!.git/* --glob=<pattern> <path>`
pub(crate) fn ripgrep_glob(
    search_root: &std::path::Path,
    pattern: &str,
    max_results: usize,
) -> Option<Vec<std::path::PathBuf>> {
    use std::process::Command;

    let rg = which_rg()?;
    let mut cmd = Command::new(rg);
    cmd.args(["--files", "--hidden", "--glob=!.git/*"])
        .arg(format!("--glob={}", pattern))
        .arg(search_root);

    let output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    let files: Vec<std::path::PathBuf> = stdout
        .lines()
        .take(max_results)
        .map(std::path::PathBuf::from)
        .collect();

    Some(files)
}

/// Find ripgrep binary on PATH.
fn which_rg() -> Option<std::path::PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(if cfg!(windows) { "rg.exe" } else { "rg" });
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn current_index_status(ctx: &AppContext) -> IndexStatus {
    if ctx
        .search_index()
        .borrow()
        .as_ref()
        .is_some_and(|index| index.ready)
    {
        IndexStatus::Ready
    } else if ctx.search_index_rx().borrow().is_some() || ctx.search_index().borrow().is_some() {
        IndexStatus::Building
    } else {
        IndexStatus::Fallback
    }
}

fn format_grep_text(result: &GrepResult, compress_tool_output: bool) -> String {
    if compress_tool_output {
        return format_compressed_grep_text(result);
    }

    format_raw_grep_text(result)
}

fn format_raw_grep_text(result: &GrepResult) -> String {
    if result.matches.is_empty() {
        return "No files found".to_string();
    }

    let mut lines = Vec::new();

    // Header: match OpenCode's "Found N matches" / "Found N matches (showing first 100)"
    if result.truncated {
        lines.push(format!(
            "Found {} matches (showing first {})",
            result.total_matches,
            result.matches.len()
        ));
    } else {
        lines.push(format!("Found {} matches", result.total_matches));
    }

    // Group matches by file, preserving the input order (already sorted by mtime)
    let mut groups: Vec<(String, Vec<&GrepMatch>)> = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for grep_match in &result.matches {
        let file_key = grep_match.file.display().to_string();
        if let Some(&idx) = seen.get(&file_key) {
            groups[idx].1.push(grep_match);
        } else {
            seen.insert(file_key.clone(), groups.len());
            groups.push((file_key, vec![grep_match]));
        }
    }

    let mut current_file = String::new();
    for (file, file_matches) in &groups {
        if current_file != *file {
            if !current_file.is_empty() {
                lines.push(String::new());
            }
            current_file = file.clone();
            lines.push(format!("{}:", file));
        }
        for grep_match in file_matches {
            lines.push(format!(
                "  Line {}: {}",
                grep_match.line,
                truncate_raw_line_text(&grep_match.line_text)
            ));
        }
    }

    // Truncation footer: match OpenCode's exact wording
    if result.truncated {
        lines.push(String::new());
        lines.push(format!(
            "(Results truncated: showing {} of {} matches ({} hidden). Consider using a more specific path or pattern.)",
            result.matches.len(),
            result.total_matches,
            result.total_matches - result.matches.len()
        ));
    }

    lines.join("\n")
}

fn format_compressed_grep_text(result: &GrepResult) -> String {
    let mut groups: BTreeMap<String, Vec<&GrepMatch>> = BTreeMap::new();

    for grep_match in &result.matches {
        groups
            .entry(grep_match.file.display().to_string())
            .or_default()
            .push(grep_match);
    }

    let mut sections = Vec::new();

    for (file, matches) in groups {
        let match_word = if matches.len() == 1 {
            "match"
        } else {
            "matches"
        };
        let mut section = format!("── {} ({} {}) ──", file, matches.len(), match_word);
        let display_count = if matches.len() > MAX_MATCHES_PER_FILE {
            MAX_DISPLAY_MATCHES_PER_FILE
        } else {
            matches.len()
        };

        for grep_match in matches.iter().take(display_count) {
            section.push_str(&format!(
                "\n  {}: {}",
                grep_match.line,
                truncate_line_text(&grep_match.line_text, MAX_COMPRESSED_LINE_CHARS)
            ));
        }

        if matches.len() > MAX_MATCHES_PER_FILE {
            section.push_str(&format!(
                "\n  ... and {} more matches",
                matches.len() - MAX_DISPLAY_MATCHES_PER_FILE
            ));
        }

        sections.push(section);
    }

    let footer = format!(
        "Found {} match(es) across {} file(s). [index: {}]",
        result.total_matches,
        result.files_with_matches,
        index_status_label(result.index_status),
    );

    if sections.is_empty() {
        footer
    } else {
        format!("{}\n\n{}", sections.join("\n\n"), footer)
    }
}

fn truncate_raw_line_text(text: &str) -> String {
    truncate_line_text_with_suffix(text, MAX_RAW_LINE_CHARS, "...")
}

fn truncate_line_text(text: &str, max_chars: usize) -> String {
    truncate_line_text_with_suffix(text, max_chars.saturating_sub(1), "…")
}

fn truncate_line_text_with_suffix(text: &str, max_chars: usize, suffix: &str) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }

    if max_chars == 0 {
        return String::new();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{}{}", truncated, suffix)
}

fn index_status_label(status: IndexStatus) -> &'static str {
    match status {
        IndexStatus::Ready => "ready",
        IndexStatus::Building => "building",
        IndexStatus::Fallback => "fallback",
    }
}

fn match_to_json(grep_match: &GrepMatch) -> serde_json::Value {
    serde_json::json!({
        "file": grep_match.file.display().to_string(),
        "line": grep_match.line,
        "column": grep_match.column,
        "line_text": grep_match.line_text,
        "match_text": grep_match.match_text,
    })
}

fn string_array_param(params: &serde_json::Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn line_details(content: &str, line_starts: &[usize], offset: usize) -> (u32, u32, String) {
    let line_index = match line_starts.binary_search(&offset) {
        Ok(index) => index,
        Err(index) => index.saturating_sub(1),
    };
    let line_start = line_starts.get(line_index).copied().unwrap_or(0);
    let line_end = content[line_start..]
        .find('\n')
        .map(|length| line_start + length)
        .unwrap_or(content.len());
    let line_text = content[line_start..line_end]
        .trim_end_matches('\r')
        .to_string();
    let column = content[line_start..offset].chars().count() as u32 + 1;
    (line_index as u32 + 1, column, line_text)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn grep_match(file: &str, line: u32, line_text: &str) -> GrepMatch {
        GrepMatch {
            file: PathBuf::from(file),
            line,
            column: 1,
            line_text: line_text.to_string(),
            match_text: "needle".to_string(),
        }
    }

    #[test]
    fn compressed_grep_groups_truncates_and_adds_footer() {
        let long_line = format!("{}xyz", "a".repeat(220));
        let result = GrepResult {
            matches: vec![
                grep_match(
                    "crates/aft/src/commands/grep.rs",
                    14,
                    "pub fn handle_grep(req: &RawRequest, ctx: &AppContext) -> Response {",
                ),
                grep_match("crates/aft/src/commands/grep.rs", 116, &long_line),
                grep_match(
                    "crates/aft/src/main.rs",
                    116,
                    "        \"grep\" => aft::commands::grep::handle_grep(&req, ctx),",
                ),
            ],
            total_matches: 3,
            files_searched: 2,
            files_with_matches: 2,
            index_status: IndexStatus::Ready,
            truncated: false,
        };

        let text = format_grep_text(&result, true);

        assert!(text.contains("── crates/aft/src/commands/grep.rs (2 matches) ──"));
        assert!(text.contains(
            "  14: pub fn handle_grep(req: &RawRequest, ctx: &AppContext) -> Response {"
        ));
        assert!(text.contains("  116: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa…"));
        assert!(text.contains("── crates/aft/src/main.rs (1 match) ──"));
        assert!(text.ends_with("Found 3 match(es) across 2 file(s). [index: ready]"));
    }

    #[test]
    fn compressed_grep_caps_large_file_sections() {
        let matches = (1..=11)
            .map(|line| grep_match("src/large.rs", line, &format!("line {line}")))
            .collect::<Vec<_>>();
        let result = GrepResult {
            matches,
            total_matches: 11,
            files_searched: 1,
            files_with_matches: 1,
            index_status: IndexStatus::Fallback,
            truncated: false,
        };

        let text = format_grep_text(&result, true);

        assert!(text.contains("── src/large.rs (11 matches) ──"));
        assert!(text.contains("  1: line 1"));
        assert!(text.contains("  5: line 5"));
        assert!(!text.contains("  6: line 6"));
        assert!(text.contains("  ... and 6 more matches"));
    }

    #[test]
    fn raw_grep_matches_opencode_format() {
        let result = GrepResult {
            matches: vec![
                grep_match(
                    "/absolute/path/to/file.rs",
                    14,
                    "pub fn handle_grep(req: &RawRequest, ctx: &AppContext) -> Response {",
                ),
                grep_match("/absolute/path/to/file.rs", 116, "another match"),
                grep_match("/absolute/path/to/main.rs", 42, "dispatch call"),
            ],
            total_matches: 3,
            files_searched: 2,
            files_with_matches: 2,
            index_status: IndexStatus::Fallback,
            truncated: false,
        };

        let text = format_grep_text(&result, false);

        // Must match OpenCode's exact format: header, filepath:, indented Line N: text
        assert_eq!(
            text,
            "Found 3 matches\n/absolute/path/to/file.rs:\n  Line 14: pub fn handle_grep(req: &RawRequest, ctx: &AppContext) -> Response {\n  Line 116: another match\n\n/absolute/path/to/main.rs:\n  Line 42: dispatch call"
        );
    }

    #[test]
    fn raw_grep_truncates_lines_at_two_thousand_chars() {
        let result = GrepResult {
            matches: vec![grep_match("src/main.rs", 42, &"a".repeat(2005))],
            total_matches: 1,
            files_searched: 1,
            files_with_matches: 1,
            index_status: IndexStatus::Fallback,
            truncated: false,
        };

        let text = format_grep_text(&result, false);

        let expected = format!(
            "Found 1 matches\nsrc/main.rs:\n  Line 42: {}...",
            "a".repeat(2000)
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn raw_grep_returns_no_files_found_for_zero_results() {
        let result = GrepResult {
            matches: Vec::new(),
            total_matches: 0,
            files_searched: 0,
            files_with_matches: 0,
            index_status: IndexStatus::Fallback,
            truncated: false,
        };

        let text = format_grep_text(&result, false);

        assert_eq!(text, "No files found");
    }
}
