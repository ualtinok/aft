use crate::compress::generic::GenericCompressor;
use crate::compress::Compressor;

pub struct BunCompressor;

impl Compressor for BunCompressor {
    fn matches(&self, command: &str) -> bool {
        command
            .split_whitespace()
            .next()
            .is_some_and(|head| head == "bun")
    }

    fn compress(&self, command: &str, output: &str) -> String {
        match bun_subcommand(command).as_deref() {
            Some("install" | "add" | "remove") => compress_package(output),
            Some("run" | "test") => GenericCompressor::compress_output(output),
            Some("build") => compress_build(output),
            _ => GenericCompressor::compress_output(output),
        }
    }
}

fn bun_subcommand(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .skip_while(|token| *token != "bun")
        .skip(1)
        .find(|token| !token.starts_with('-'))
        .map(ToString::to_string)
}

fn compress_package(output: &str) -> String {
    let mut result = Vec::new();
    for line in output.lines() {
        if is_bun_progress(line) {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.contains("packages installed")
            || trimmed.contains("package installed")
            || trimmed.starts_with("error:")
            || trimmed.starts_with("bun install error:")
            || trimmed.starts_with("Saved lockfile")
        {
            result.push(line.to_string());
        }
    }
    trim_trailing_lines(&result.join("\n"))
}

fn compress_build(output: &str) -> String {
    let mut result = Vec::new();
    let mut timing_seen = 0usize;
    let mut timing_omitted = 0usize;
    for line in output.lines() {
        if is_timing_line(line) {
            timing_seen += 1;
            if timing_seen > 10 {
                timing_omitted += 1;
                continue;
            }
        }
        result.push(line.to_string());
    }
    if timing_omitted > 0 {
        result.push(format!("... and {timing_omitted} more timing lines"));
    }
    trim_trailing_lines(&result.join("\n"))
}

fn is_bun_progress(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "."
        || trimmed.chars().all(|char| char == '.')
        || trimmed.starts_with("Resolving")
        || trimmed.starts_with("Resolved")
        || trimmed.starts_with("Downloaded")
        || trimmed.starts_with("Extracted")
}

fn is_timing_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('[') && trimmed.contains(" ms]")
}

fn trim_trailing_lines(input: &str) -> String {
    input
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}
