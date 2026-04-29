use std::collections::HashSet;

use crate::compress::generic::{dedup_consecutive, middle_truncate, GenericCompressor};
use crate::compress::Compressor;

const STATUS_SHORT_LIMIT: usize = 1024;
const STATUS_KEEP_PER_SECTION: usize = 10;
const DIFF_MAX_FILES: usize = 5;
const DIFF_MAX_HUNKS: usize = 20;
const HUNK_KEEP_LINES: usize = 30;
const LOG_KEEP_COMMITS: usize = 20;
const BLAME_KEEP_LINES: usize = 50;

pub struct GitCompressor;

impl Compressor for GitCompressor {
    fn matches(&self, command: &str) -> bool {
        command_head(command).is_some_and(|head| head == "git")
    }

    fn compress(&self, command: &str, output: &str) -> String {
        match git_subcommand(command).as_deref() {
            Some("status") => compress_status(output),
            Some("diff") => compress_diff(output, false),
            Some("log") => compress_log(output),
            Some("show") => compress_diff(output, true),
            Some("branch") => trim_trailing_lines(&dedup_consecutive(output)),
            Some("blame") => compress_blame(output),
            _ => GenericCompressor::compress_output(output),
        }
    }
}

fn command_head(command: &str) -> Option<&str> {
    command.split_whitespace().next()
}

fn git_subcommand(command: &str) -> Option<String> {
    let mut seen_git = false;
    for token in command.split_whitespace() {
        if !seen_git {
            if token == "git" {
                seen_git = true;
            }
            continue;
        }
        if token.starts_with('-') || token.contains('=') {
            continue;
        }
        return Some(token.to_string());
    }
    None
}

fn compress_status(output: &str) -> String {
    if output.len() <= STATUS_SHORT_LIMIT {
        return trim_trailing_lines(output);
    }

    let mut result = Vec::new();
    let mut section_entries = Vec::new();
    let mut in_section = false;

    for line in output.lines() {
        if is_status_section_header(line) {
            flush_status_entries(&mut result, &mut section_entries);
            result.push(line.to_string());
            in_section = true;
        } else if in_section && is_status_instructional(line) {
            // Lines like `  (use "git add <file>..." to include in what will be
            // committed)` come right after the section header in real git
            // output. They're informational, not entries — pass them through
            // verbatim WITHOUT resetting `in_section` so the entries that
            // follow still get aggregated and summarized.
            result.push(line.to_string());
        } else if in_section && is_status_entry(line) {
            section_entries.push(line.to_string());
        } else {
            flush_status_entries(&mut result, &mut section_entries);
            result.push(line.to_string());
            in_section = false;
        }
    }
    flush_status_entries(&mut result, &mut section_entries);

    trim_trailing_lines(&result.join("\n"))
}

fn is_status_section_header(line: &str) -> bool {
    matches!(
        line.trim_end_matches(':'),
        "Changes to be committed"
            | "Changes not staged for commit"
            | "Untracked files"
            | "Unmerged paths"
    )
}

/// Recognize the parenthesized instructional lines git emits inside a status
/// section, e.g. `  (use "git add <file>..." to include in what will be committed)`.
/// These come right after the section header and must NOT reset the
/// in-section state, otherwise the actual entries that follow are missed by
/// the entry aggregator.
fn is_status_instructional(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('(') || trimmed.starts_with("use ")
}

fn is_status_entry(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("modified:")
        || trimmed.starts_with("new file:")
        || trimmed.starts_with("deleted:")
        || trimmed.starts_with("renamed:")
        || trimmed.starts_with("copied:")
        || trimmed.starts_with("both modified:")
        || trimmed.starts_with("both added:")
        || trimmed.starts_with("deleted by us:")
        || trimmed.starts_with("deleted by them:")
        || (!trimmed.is_empty()
            && !trimmed.starts_with('(')
            && !trimmed.starts_with("use ")
            && !trimmed.starts_with("no changes"))
}

fn flush_status_entries(result: &mut Vec<String>, entries: &mut Vec<String>) {
    if entries.is_empty() {
        return;
    }

    let keep = entries.len().min(STATUS_KEEP_PER_SECTION);
    result.extend(entries.iter().take(keep).cloned());
    if entries.len() > keep {
        result.push(format!("... and {} more", entries.len() - keep));
    }
    entries.clear();
}

fn compress_diff(output: &str, keep_commit_header: bool) -> String {
    let files = split_diff_files(output, keep_commit_header);
    let total_hunks: usize = files.iter().map(|file| count_hunks(&file.lines)).sum();

    if files.is_empty() || total_hunks <= 2 && output.len() <= 5 * 1024 {
        return trim_trailing_lines(output);
    }

    let max_files = if total_hunks > DIFF_MAX_HUNKS {
        DIFF_MAX_FILES
    } else {
        usize::MAX
    };

    let mut result = Vec::new();
    let mut emitted_files = 0usize;

    for file in &files {
        if file.is_diff && emitted_files >= max_files {
            continue;
        }
        result.extend(compress_diff_file(&file.lines));
        emitted_files += usize::from(file.is_diff);
    }

    let changed_files = files.iter().filter(|file| file.is_diff).count();
    if changed_files > emitted_files {
        result.push(format!(
            "... and {} more files changed",
            changed_files - emitted_files
        ));
    }

    middle_truncate(
        &trim_trailing_lines(&result.join("\n")),
        16 * 1024,
        7 * 1024,
        7 * 1024,
    )
}

struct DiffFile {
    lines: Vec<String>,
    is_diff: bool,
}

fn split_diff_files(output: &str, keep_commit_header: bool) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current = Vec::new();
    let mut current_is_diff = false;

    for line in output.lines() {
        if line.starts_with("diff --git ") {
            if !current.is_empty() {
                files.push(DiffFile {
                    lines: std::mem::take(&mut current),
                    is_diff: current_is_diff,
                });
            }
            current_is_diff = true;
        } else if !current_is_diff && !keep_commit_header && !line.starts_with("diff --git ") {
            current_is_diff = true;
        }
        current.push(line.to_string());
    }

    if !current.is_empty() {
        files.push(DiffFile {
            lines: current,
            is_diff: current_is_diff,
        });
    }

    files
}

fn compress_diff_file(lines: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let line = &lines[index];
        if !line.starts_with("@@") {
            result.push(line.clone());
            index += 1;
            continue;
        }

        let hunk_start = index;
        index += 1;
        while index < lines.len() && !lines[index].starts_with("@@") {
            index += 1;
        }
        let hunk = &lines[hunk_start..index];
        append_hunk(&mut result, hunk);
    }

    result
}

fn append_hunk(result: &mut Vec<String>, hunk: &[String]) {
    if hunk.len() <= HUNK_KEEP_LINES + 1 {
        result.extend(hunk.iter().cloned());
        return;
    }

    result.extend(hunk.iter().take(HUNK_KEEP_LINES + 1).cloned());
    let remaining = &hunk[HUNK_KEEP_LINES + 1..];
    let added = remaining
        .iter()
        .filter(|line| line.starts_with('+'))
        .count();
    let removed = remaining
        .iter()
        .filter(|line| line.starts_with('-'))
        .count();
    result.push(format!(
        "... +{} -{} in {} more lines",
        added,
        removed,
        remaining.len()
    ));
}

fn count_hunks(lines: &[String]) -> usize {
    lines.iter().filter(|line| line.starts_with("@@")).count()
}

fn compress_log(output: &str) -> String {
    let mut commits = 0usize;
    let mut omitted = 0usize;
    let mut result = Vec::new();
    let mut seen_authors = HashSet::new();

    for line in output.lines() {
        let is_commit = line.starts_with("commit ") || looks_like_oneline_commit(line);
        if is_commit {
            commits += 1;
            if commits > LOG_KEEP_COMMITS {
                omitted += 1;
                continue;
            }
        }

        if commits > LOG_KEEP_COMMITS {
            continue;
        }

        if line.starts_with("Author: ") && !seen_authors.insert(line.to_string()) {
            continue;
        }

        result.push(line.to_string());
    }

    if omitted > 0 {
        result.push(format!("... {} more commits", omitted));
    }

    trim_trailing_lines(&result.join("\n"))
}

fn looks_like_oneline_commit(line: &str) -> bool {
    let Some((hash, _message)) = line.split_once(' ') else {
        return false;
    };
    (7..=40).contains(&hash.len()) && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn compress_blame(output: &str) -> String {
    let total = output.lines().count();
    if total <= BLAME_KEEP_LINES {
        return trim_trailing_lines(output);
    }

    let mut result: Vec<String> = output
        .lines()
        .take(BLAME_KEEP_LINES)
        .map(ToString::to_string)
        .collect();
    result.push(format!("... {} more blame lines", total - BLAME_KEEP_LINES));
    result.join("\n")
}

fn trim_trailing_lines(input: &str) -> String {
    input
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}
