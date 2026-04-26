//! External tool runner and auto-formatter detection.
//!
//! Provides subprocess execution with timeout protection, language-to-formatter
//! mapping, and the `auto_format` entry point used by `write_format_validate`.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::parser::{detect_language, LangId};

/// Result of running an external tool subprocess.
#[derive(Debug)]
pub struct ExternalToolResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Errors from external tool execution.
#[derive(Debug)]
pub enum FormatError {
    /// The tool binary was not found on PATH.
    NotFound { tool: String },
    /// The tool exceeded its timeout and was killed.
    Timeout { tool: String, timeout_secs: u32 },
    /// The tool exited with a non-zero status.
    Failed { tool: String, stderr: String },
    /// No formatter is configured for this language.
    UnsupportedLanguage,
}

/// A configured formatter/checker that cannot be resolved for configure warnings.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MissingTool {
    pub kind: String,
    pub language: String,
    pub tool: String,
    pub hint: String,
}

#[derive(Debug, Clone)]
struct ToolCandidate {
    tool: String,
    source: String,
    args: Vec<String>,
    required: bool,
}

#[derive(Debug, Clone)]
enum ToolDetection {
    Found(String, Vec<String>),
    NotConfigured,
    NotInstalled { tool: String },
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::NotFound { tool } => write!(f, "formatter not found: {}", tool),
            FormatError::Timeout { tool, timeout_secs } => {
                write!(f, "formatter '{}' timed out after {}s", tool, timeout_secs)
            }
            FormatError::Failed { tool, stderr } => {
                write!(f, "formatter '{}' failed: {}", tool, stderr)
            }
            FormatError::UnsupportedLanguage => write!(f, "unsupported language for formatting"),
        }
    }
}

/// Spawn a subprocess and wait for completion with timeout protection.
///
/// Polls `try_wait()` at 50ms intervals. On timeout, kills the child process
/// and waits for it to exit. Returns `FormatError::NotFound` when the binary
/// isn't on PATH.
pub fn run_external_tool(
    command: &str,
    args: &[&str],
    working_dir: Option<&Path>,
    timeout_secs: u32,
) -> Result<ExternalToolResult, FormatError> {
    let mut cmd = Command::new(command);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(FormatError::NotFound {
                tool: command.to_string(),
            });
        }
        Err(e) => {
            return Err(FormatError::Failed {
                tool: command.to_string(),
                stderr: e.to_string(),
            });
        }
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs as u64);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|s| std::io::read_to_string(s).unwrap_or_default())
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|s| std::io::read_to_string(s).unwrap_or_default())
                    .unwrap_or_default();

                let exit_code = status.code().unwrap_or(-1);
                if exit_code != 0 {
                    return Err(FormatError::Failed {
                        tool: command.to_string(),
                        stderr,
                    });
                }

                return Ok(ExternalToolResult {
                    stdout,
                    stderr,
                    exit_code,
                });
            }
            Ok(None) => {
                // Still running
                if Instant::now() >= deadline {
                    // Kill the process and reap it
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(FormatError::Timeout {
                        tool: command.to_string(),
                        timeout_secs,
                    });
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(FormatError::Failed {
                    tool: command.to_string(),
                    stderr: format!("try_wait error: {}", e),
                });
            }
        }
    }
}

/// TTL for tool availability cache entries.
const TOOL_CACHE_TTL: Duration = Duration::from_secs(60);

static TOOL_CACHE: std::sync::LazyLock<Mutex<HashMap<String, (bool, Instant)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn tool_cache_key(command: &str, project_root: Option<&Path>) -> String {
    let root = project_root
        .map(|path| path.to_string_lossy())
        .unwrap_or_default();
    format!("{}\0{}", command, root)
}

pub fn clear_tool_cache() {
    if let Ok(mut cache) = TOOL_CACHE.lock() {
        cache.clear();
    }
}

/// Resolve a tool by checking node_modules/.bin relative to project_root, then PATH.
/// Returns the full path to the tool if found, otherwise None.
fn resolve_tool(command: &str, project_root: Option<&Path>) -> Option<String> {
    // 1. Check node_modules/.bin/<command> relative to project root
    if let Some(root) = project_root {
        let local_bin = root.join("node_modules").join(".bin").join(command);
        if local_bin.exists() {
            return Some(local_bin.to_string_lossy().to_string());
        }
    }

    // 2. Fall back to PATH lookup
    match Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            let start = Instant::now();
            let timeout = Duration::from_secs(2);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return if status.success() {
                            Some(command.to_string())
                        } else {
                            None
                        };
                    }
                    Ok(None) if start.elapsed() > timeout => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return None;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(50)),
                    Err(_) => return None,
                }
            }
        }
        Err(_) => None,
    }
}

/// Check if `ruff format` is available with a stable formatter.
///
/// Ruff's formatter became stable in v0.1.2. Versions before that output
/// `NOT_YET_IMPLEMENTED_*` stubs instead of formatted code. We parse the
/// version from `ruff --version` (format: "ruff X.Y.Z") and require >= 0.1.2.
/// Falls back to false if ruff is not found or version cannot be parsed.
fn ruff_format_available(project_root: Option<&Path>) -> bool {
    let key = tool_cache_key("ruff-format", project_root);
    if let Ok(cache) = TOOL_CACHE.lock() {
        if let Some((available, checked_at)) = cache.get(&key) {
            if checked_at.elapsed() < TOOL_CACHE_TTL {
                return *available;
            }
        }
    }

    let result = ruff_format_available_uncached(project_root);
    if let Ok(mut cache) = TOOL_CACHE.lock() {
        cache.insert(key, (result, Instant::now()));
    }
    result
}

fn ruff_format_available_uncached(project_root: Option<&Path>) -> bool {
    let command = match resolve_tool("ruff", project_root) {
        Some(command) => command,
        None => return false,
    };
    let output = match Command::new(&command)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) => o,
        Err(_) => return false,
    };

    let version_str = String::from_utf8_lossy(&output.stdout);
    // Parse "ruff X.Y.Z" or just "X.Y.Z"
    let version_part = version_str
        .trim()
        .strip_prefix("ruff ")
        .unwrap_or(version_str.trim());

    let parts: Vec<&str> = version_part.split('.').collect();
    if parts.len() < 3 {
        return false;
    }

    let major: u32 = match parts[0].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let minor: u32 = match parts[1].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };
    let patch: u32 = match parts[2].parse() {
        Ok(v) => v,
        Err(_) => return false,
    };

    // Require >= 0.1.2 where ruff format became stable
    (major, minor, patch) >= (0, 1, 2)
}

fn resolve_candidate_tool(
    candidate: &ToolCandidate,
    project_root: Option<&Path>,
) -> Option<String> {
    if candidate.tool == "ruff" && !ruff_format_available(project_root) {
        return None;
    }

    resolve_tool(&candidate.tool, project_root)
}

fn lang_key(lang: LangId) -> &'static str {
    match lang {
        LangId::TypeScript | LangId::JavaScript | LangId::Tsx => "typescript",
        LangId::Python => "python",
        LangId::Rust => "rust",
        LangId::Go => "go",
        LangId::C => "c",
        LangId::Cpp => "cpp",
        LangId::Zig => "zig",
        LangId::CSharp => "csharp",
        LangId::Bash => "bash",
        LangId::Html => "html",
        LangId::Markdown => "markdown",
    }
}

fn has_formatter_support(lang: LangId) -> bool {
    matches!(
        lang,
        LangId::TypeScript
            | LangId::JavaScript
            | LangId::Tsx
            | LangId::Python
            | LangId::Rust
            | LangId::Go
    )
}

fn has_checker_support(lang: LangId) -> bool {
    matches!(
        lang,
        LangId::TypeScript
            | LangId::JavaScript
            | LangId::Tsx
            | LangId::Python
            | LangId::Rust
            | LangId::Go
    )
}

fn formatter_candidates(lang: LangId, config: &Config, file_str: &str) -> Vec<ToolCandidate> {
    let project_root = config.project_root.as_deref();
    if let Some(preferred) = config.formatter.get(lang_key(lang)) {
        return explicit_formatter_candidate(preferred, file_str);
    }

    match lang {
        LangId::TypeScript | LangId::JavaScript | LangId::Tsx => {
            if has_project_config(project_root, &["biome.json", "biome.jsonc"]) {
                vec![ToolCandidate {
                    tool: "biome".to_string(),
                    source: "biome.json".to_string(),
                    args: vec![
                        "format".to_string(),
                        "--write".to_string(),
                        file_str.to_string(),
                    ],
                    required: true,
                }]
            } else if has_project_config(
                project_root,
                &[
                    ".prettierrc",
                    ".prettierrc.json",
                    ".prettierrc.yml",
                    ".prettierrc.yaml",
                    ".prettierrc.js",
                    ".prettierrc.cjs",
                    ".prettierrc.mjs",
                    ".prettierrc.toml",
                    "prettier.config.js",
                    "prettier.config.cjs",
                    "prettier.config.mjs",
                ],
            ) {
                vec![ToolCandidate {
                    tool: "prettier".to_string(),
                    source: "Prettier config".to_string(),
                    args: vec!["--write".to_string(), file_str.to_string()],
                    required: true,
                }]
            } else if has_project_config(project_root, &["deno.json", "deno.jsonc"]) {
                vec![ToolCandidate {
                    tool: "deno".to_string(),
                    source: "deno.json".to_string(),
                    args: vec!["fmt".to_string(), file_str.to_string()],
                    required: true,
                }]
            } else {
                Vec::new()
            }
        }
        LangId::Python => {
            if has_project_config(project_root, &["ruff.toml", ".ruff.toml"])
                || has_pyproject_tool(project_root, "ruff")
            {
                vec![ToolCandidate {
                    tool: "ruff".to_string(),
                    source: "ruff config".to_string(),
                    args: vec!["format".to_string(), file_str.to_string()],
                    required: true,
                }]
            } else if has_pyproject_tool(project_root, "black") {
                vec![ToolCandidate {
                    tool: "black".to_string(),
                    source: "pyproject.toml".to_string(),
                    args: vec![file_str.to_string()],
                    required: true,
                }]
            } else {
                Vec::new()
            }
        }
        LangId::Rust => {
            if has_project_config(project_root, &["Cargo.toml"]) {
                vec![ToolCandidate {
                    tool: "rustfmt".to_string(),
                    source: "Cargo.toml".to_string(),
                    args: vec![file_str.to_string()],
                    required: true,
                }]
            } else {
                Vec::new()
            }
        }
        LangId::Go => {
            if has_project_config(project_root, &["go.mod"]) {
                vec![
                    ToolCandidate {
                        tool: "goimports".to_string(),
                        source: "go.mod".to_string(),
                        args: vec!["-w".to_string(), file_str.to_string()],
                        required: false,
                    },
                    ToolCandidate {
                        tool: "gofmt".to_string(),
                        source: "go.mod".to_string(),
                        args: vec!["-w".to_string(), file_str.to_string()],
                        required: true,
                    },
                ]
            } else {
                Vec::new()
            }
        }
        LangId::C | LangId::Cpp | LangId::Zig | LangId::CSharp | LangId::Bash => Vec::new(),
        LangId::Html => Vec::new(),
        LangId::Markdown => Vec::new(),
    }
}

fn checker_candidates(lang: LangId, config: &Config, file_str: &str) -> Vec<ToolCandidate> {
    let project_root = config.project_root.as_deref();
    if let Some(preferred) = config.checker.get(lang_key(lang)) {
        return explicit_checker_candidate(preferred, file_str);
    }

    match lang {
        LangId::TypeScript | LangId::JavaScript | LangId::Tsx => {
            if has_project_config(project_root, &["biome.json", "biome.jsonc"]) {
                vec![ToolCandidate {
                    tool: "biome".to_string(),
                    source: "biome.json".to_string(),
                    args: vec!["check".to_string(), file_str.to_string()],
                    required: true,
                }]
            } else if has_project_config(project_root, &["tsconfig.json"]) {
                vec![ToolCandidate {
                    tool: "tsc".to_string(),
                    source: "tsconfig.json".to_string(),
                    args: vec![
                        "--noEmit".to_string(),
                        "--pretty".to_string(),
                        "false".to_string(),
                    ],
                    required: true,
                }]
            } else {
                Vec::new()
            }
        }
        LangId::Python => {
            if has_project_config(project_root, &["pyrightconfig.json"])
                || has_pyproject_tool(project_root, "pyright")
            {
                vec![ToolCandidate {
                    tool: "pyright".to_string(),
                    source: "pyright config".to_string(),
                    args: vec!["--outputjson".to_string(), file_str.to_string()],
                    required: true,
                }]
            } else if has_project_config(project_root, &["ruff.toml", ".ruff.toml"])
                || has_pyproject_tool(project_root, "ruff")
            {
                vec![ToolCandidate {
                    tool: "ruff".to_string(),
                    source: "ruff config".to_string(),
                    args: vec![
                        "check".to_string(),
                        "--output-format=json".to_string(),
                        file_str.to_string(),
                    ],
                    required: true,
                }]
            } else {
                Vec::new()
            }
        }
        LangId::Rust => {
            if has_project_config(project_root, &["Cargo.toml"]) {
                vec![ToolCandidate {
                    tool: "cargo".to_string(),
                    source: "Cargo.toml".to_string(),
                    args: vec!["check".to_string(), "--message-format=json".to_string()],
                    required: true,
                }]
            } else {
                Vec::new()
            }
        }
        LangId::Go => {
            if has_project_config(project_root, &["go.mod"]) {
                vec![
                    ToolCandidate {
                        tool: "staticcheck".to_string(),
                        source: "go.mod".to_string(),
                        args: vec![file_str.to_string()],
                        required: false,
                    },
                    ToolCandidate {
                        tool: "go".to_string(),
                        source: "go.mod".to_string(),
                        args: vec!["vet".to_string(), file_str.to_string()],
                        required: true,
                    },
                ]
            } else {
                Vec::new()
            }
        }
        LangId::C | LangId::Cpp | LangId::Zig | LangId::CSharp | LangId::Bash => Vec::new(),
        LangId::Html => Vec::new(),
        LangId::Markdown => Vec::new(),
    }
}

fn explicit_formatter_candidate(name: &str, file_str: &str) -> Vec<ToolCandidate> {
    match name {
        "none" | "off" | "false" => Vec::new(),
        "biome" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "formatter config".to_string(),
            args: vec![
                "format".to_string(),
                "--write".to_string(),
                file_str.to_string(),
            ],
            required: true,
        }],
        "prettier" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "formatter config".to_string(),
            args: vec!["--write".to_string(), file_str.to_string()],
            required: true,
        }],
        "deno" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "formatter config".to_string(),
            args: vec!["fmt".to_string(), file_str.to_string()],
            required: true,
        }],
        "ruff" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "formatter config".to_string(),
            args: vec!["format".to_string(), file_str.to_string()],
            required: true,
        }],
        "black" | "rustfmt" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "formatter config".to_string(),
            args: vec![file_str.to_string()],
            required: true,
        }],
        "goimports" | "gofmt" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "formatter config".to_string(),
            args: vec!["-w".to_string(), file_str.to_string()],
            required: true,
        }],
        _ => Vec::new(),
    }
}

fn explicit_checker_candidate(name: &str, file_str: &str) -> Vec<ToolCandidate> {
    match name {
        "none" | "off" | "false" => Vec::new(),
        "tsc" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec![
                "--noEmit".to_string(),
                "--pretty".to_string(),
                "false".to_string(),
            ],
            required: true,
        }],
        "cargo" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec!["check".to_string(), "--message-format=json".to_string()],
            required: true,
        }],
        "go" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec!["vet".to_string(), file_str.to_string()],
            required: true,
        }],
        "biome" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec!["check".to_string(), file_str.to_string()],
            required: true,
        }],
        "pyright" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec!["--outputjson".to_string(), file_str.to_string()],
            required: true,
        }],
        "ruff" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec![
                "check".to_string(),
                "--output-format=json".to_string(),
                file_str.to_string(),
            ],
            required: true,
        }],
        "staticcheck" => vec![ToolCandidate {
            tool: name.to_string(),
            source: "checker config".to_string(),
            args: vec![file_str.to_string()],
            required: true,
        }],
        _ => Vec::new(),
    }
}

fn resolve_tool_candidates(
    candidates: Vec<ToolCandidate>,
    project_root: Option<&Path>,
) -> ToolDetection {
    if candidates.is_empty() {
        return ToolDetection::NotConfigured;
    }

    let mut missing_required = None;
    for candidate in candidates {
        if let Some(command) = resolve_candidate_tool(&candidate, project_root) {
            return ToolDetection::Found(command, candidate.args);
        }
        if candidate.required && missing_required.is_none() {
            missing_required = Some(candidate.tool);
        }
    }

    match missing_required {
        Some(tool) => ToolDetection::NotInstalled { tool },
        None => ToolDetection::NotConfigured,
    }
}

fn checker_command(candidate: &ToolCandidate, resolved: String) -> String {
    match candidate.tool.as_str() {
        "tsc" => resolved,
        "cargo" => "cargo".to_string(),
        "go" => "go".to_string(),
        _ => resolved,
    }
}

fn checker_args(candidate: &ToolCandidate) -> Vec<String> {
    if candidate.tool == "tsc" {
        vec![
            "--noEmit".to_string(),
            "--pretty".to_string(),
            "false".to_string(),
        ]
    } else {
        candidate.args.clone()
    }
}

fn detect_formatter_for_path(path: &Path, lang: LangId, config: &Config) -> ToolDetection {
    let file_str = path.to_string_lossy().to_string();
    resolve_tool_candidates(
        formatter_candidates(lang, config, &file_str),
        config.project_root.as_deref(),
    )
}

fn detect_checker_for_path(path: &Path, lang: LangId, config: &Config) -> ToolDetection {
    let file_str = path.to_string_lossy().to_string();
    let candidates = checker_candidates(lang, config, &file_str);
    if candidates.is_empty() {
        return ToolDetection::NotConfigured;
    }

    let project_root = config.project_root.as_deref();
    let mut missing_required = None;
    for candidate in candidates {
        if let Some(command) = resolve_candidate_tool(&candidate, project_root) {
            return ToolDetection::Found(
                checker_command(&candidate, command),
                checker_args(&candidate),
            );
        }
        if candidate.required && missing_required.is_none() {
            missing_required = Some(candidate.tool);
        }
    }

    match missing_required {
        Some(tool) => ToolDetection::NotInstalled { tool },
        None => ToolDetection::NotConfigured,
    }
}

fn languages_in_project(project_root: &Path) -> HashSet<LangId> {
    crate::callgraph::walk_project_files(project_root)
        .filter_map(|path| detect_language(&path))
        .collect()
}

fn placeholder_file_for_language(project_root: &Path, lang: LangId) -> PathBuf {
    let filename = match lang {
        LangId::TypeScript => "aft-tool-detection.ts",
        LangId::Tsx => "aft-tool-detection.tsx",
        LangId::JavaScript => "aft-tool-detection.js",
        LangId::Python => "aft-tool-detection.py",
        LangId::Rust => "aft_tool_detection.rs",
        LangId::Go => "aft_tool_detection.go",
        LangId::C => "aft_tool_detection.c",
        LangId::Cpp => "aft_tool_detection.cpp",
        LangId::Zig => "aft_tool_detection.zig",
        LangId::CSharp => "aft_tool_detection.cs",
        LangId::Bash => "aft_tool_detection.sh",
        LangId::Html => "aft-tool-detection.html",
        LangId::Markdown => "aft-tool-detection.md",
    };
    project_root.join(filename)
}

pub(crate) fn install_hint(tool: &str) -> String {
    match tool {
        "biome" => {
            "Run `bun add -d --workspace-root @biomejs/biome` or install globally.".to_string()
        }
        "prettier" => "Run `npm install -D prettier` or install globally.".to_string(),
        "tsc" => "Run `npm install -D typescript` or install globally.".to_string(),
        "pyright" | "pyright-langserver" => "Install: `npm install -g pyright`".to_string(),
        "ruff" => {
            "Install: `pip install ruff` or your Python package manager equivalent.".to_string()
        }
        "black" => {
            "Install: `pip install black` or your Python package manager equivalent.".to_string()
        }
        "rustfmt" => "Install: `rustup component add rustfmt`".to_string(),
        "rust-analyzer" => "Install: `rustup component add rust-analyzer`".to_string(),
        "cargo" => "Install Rust from https://rustup.rs/.".to_string(),
        "go" => "Install Go from https://go.dev/dl/.".to_string(),
        "gopls" => "Install: `go install golang.org/x/tools/gopls@latest`".to_string(),
        "bash-language-server" => "Install: `npm install -g bash-language-server`".to_string(),
        "yaml-language-server" => "Install: `npm install -g yaml-language-server`".to_string(),
        "typescript-language-server" => {
            "Install: `npm install -g typescript-language-server typescript`".to_string()
        }
        "deno" => "Install Deno from https://deno.com/.".to_string(),
        "goimports" => "Install: `go install golang.org/x/tools/cmd/goimports@latest`".to_string(),
        "staticcheck" => {
            "Install: `go install honnef.co/go/tools/cmd/staticcheck@latest`".to_string()
        }
        other => format!("Install `{other}` and ensure it is on PATH."),
    }
}

fn configured_tool_hint(tool: &str, source: &str) -> String {
    format!(
        "{tool} is configured in {source} but not installed. {}",
        install_hint(tool)
    )
}

fn missing_tool_warning(
    kind: &str,
    language: &str,
    candidate: &ToolCandidate,
    project_root: Option<&Path>,
) -> Option<MissingTool> {
    if !candidate.required || resolve_candidate_tool(candidate, project_root).is_some() {
        return None;
    }

    Some(MissingTool {
        kind: kind.to_string(),
        language: language.to_string(),
        tool: candidate.tool.clone(),
        hint: configured_tool_hint(&candidate.tool, &candidate.source),
    })
}

/// Detect configured formatters/checkers that are missing for languages present in the project.
pub fn detect_missing_tools(project_root: &Path, config: &Config) -> Vec<MissingTool> {
    let languages = languages_in_project(project_root);
    let mut warnings = Vec::new();
    let mut seen = HashSet::new();

    for lang in languages {
        let language = lang_key(lang);
        let placeholder = placeholder_file_for_language(project_root, lang);
        let file_str = placeholder.to_string_lossy().to_string();

        for candidate in formatter_candidates(lang, config, &file_str) {
            if let Some(warning) = missing_tool_warning(
                "formatter_not_installed",
                language,
                &candidate,
                config.project_root.as_deref(),
            ) {
                if seen.insert((
                    warning.kind.clone(),
                    warning.language.clone(),
                    warning.tool.clone(),
                )) {
                    warnings.push(warning);
                }
            }
        }

        for candidate in checker_candidates(lang, config, &file_str) {
            if let Some(warning) = missing_tool_warning(
                "checker_not_installed",
                language,
                &candidate,
                config.project_root.as_deref(),
            ) {
                if seen.insert((
                    warning.kind.clone(),
                    warning.language.clone(),
                    warning.tool.clone(),
                )) {
                    warnings.push(warning);
                }
            }
        }
    }

    warnings.sort_by(|left, right| {
        (&left.kind, &left.language, &left.tool).cmp(&(&right.kind, &right.language, &right.tool))
    });
    warnings
}

/// Detect the appropriate formatter command and arguments for a file.
///
/// Priority per language:
/// - TypeScript/JavaScript/TSX: `prettier --write <file>`
/// - Python: `ruff format <file>` (fallback: `black <file>`)
/// - Rust: `rustfmt <file>`
/// - Go: `gofmt -w <file>`
///
/// Returns `None` if no formatter is available for the language.
pub fn detect_formatter(
    path: &Path,
    lang: LangId,
    config: &Config,
) -> Option<(String, Vec<String>)> {
    match detect_formatter_for_path(path, lang, config) {
        ToolDetection::Found(cmd, args) => Some((cmd, args)),
        ToolDetection::NotConfigured | ToolDetection::NotInstalled { .. } => None,
    }
}

/// Check if any of the given config file names exist in the project root.
fn has_project_config(project_root: Option<&Path>, filenames: &[&str]) -> bool {
    let root = match project_root {
        Some(r) => r,
        None => return false,
    };
    filenames.iter().any(|f| root.join(f).exists())
}

/// Check if pyproject.toml exists and contains a `[tool.<name>]` section.
fn has_pyproject_tool(project_root: Option<&Path>, tool_name: &str) -> bool {
    let root = match project_root {
        Some(r) => r,
        None => return false,
    };
    let pyproject = root.join("pyproject.toml");
    if !pyproject.exists() {
        return false;
    }
    match std::fs::read_to_string(&pyproject) {
        Ok(content) => {
            let pattern = format!("[tool.{}]", tool_name);
            content.contains(&pattern)
        }
        Err(_) => false,
    }
}

/// Auto-format a file using the detected formatter for its language.
///
/// Returns `(formatted, skip_reason)`:
/// - `(true, None)` — file was successfully formatted
/// - `(false, Some(reason))` — formatting was skipped, reason explains why
///
/// Skip reasons: `"unsupported_language"`, `"no_formatter_configured"`,
/// `"formatter_not_installed"`, `"timeout"`, `"error"`
pub fn auto_format(path: &Path, config: &Config) -> (bool, Option<String>) {
    // Check if formatting is disabled via plugin config
    if !config.format_on_edit {
        return (false, Some("no_formatter_configured".to_string()));
    }

    let lang = match detect_language(path) {
        Some(l) => l,
        None => {
            log::debug!(
                "[aft] format: {} (skipped: unsupported_language)",
                path.display()
            );
            return (false, Some("unsupported_language".to_string()));
        }
    };
    if !has_formatter_support(lang) {
        log::debug!(
            "[aft] format: {} (skipped: unsupported_language)",
            path.display()
        );
        return (false, Some("unsupported_language".to_string()));
    }

    let (cmd, args) = match detect_formatter_for_path(path, lang, config) {
        ToolDetection::Found(cmd, args) => (cmd, args),
        ToolDetection::NotConfigured => {
            log::debug!(
                "[aft] format: {} (skipped: no_formatter_configured)",
                path.display()
            );
            return (false, Some("no_formatter_configured".to_string()));
        }
        ToolDetection::NotInstalled { tool } => {
            log::warn!(
                "format: {} (skipped: formatter_not_installed: {})",
                path.display(),
                tool
            );
            return (false, Some("formatter_not_installed".to_string()));
        }
    };

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    // Run the formatter in the project root so tool-local config files
    // (biome.json, .prettierrc, rustfmt.toml, etc.) are discovered. The
    // type-checker path (`validate_full`) already does this via
    // `path.parent()`; formatters need the same treatment. Without it,
    // formatters silently fall back to built-in defaults when the aft
    // process CWD differs from the project root (audit #18).
    let working_dir = config.project_root.as_deref();

    match run_external_tool(&cmd, &arg_refs, working_dir, config.formatter_timeout_secs) {
        Ok(_) => {
            log::info!("format: {} ({})", path.display(), cmd);
            (true, None)
        }
        Err(FormatError::Timeout { .. }) => {
            log::warn!("format: {} (skipped: timeout)", path.display());
            (false, Some("timeout".to_string()))
        }
        Err(FormatError::NotFound { .. }) => {
            log::warn!(
                "format: {} (skipped: formatter_not_installed)",
                path.display()
            );
            (false, Some("formatter_not_installed".to_string()))
        }
        Err(FormatError::Failed { stderr, .. }) => {
            log::debug!(
                "[aft] format: {} (skipped: error: {})",
                path.display(),
                stderr.lines().next().unwrap_or("unknown")
            );
            (false, Some("error".to_string()))
        }
        Err(FormatError::UnsupportedLanguage) => {
            log::debug!(
                "[aft] format: {} (skipped: unsupported_language)",
                path.display()
            );
            (false, Some("unsupported_language".to_string()))
        }
    }
}

/// Spawn a subprocess and capture output regardless of exit code.
///
/// Unlike `run_external_tool`, this does NOT treat non-zero exit as an error —
/// type checkers return non-zero when they find issues, which is expected.
/// Returns `FormatError::NotFound` when the binary isn't on PATH, and
/// `FormatError::Timeout` if the deadline is exceeded.
pub fn run_external_tool_capture(
    command: &str,
    args: &[&str],
    working_dir: Option<&Path>,
    timeout_secs: u32,
) -> Result<ExternalToolResult, FormatError> {
    let mut cmd = Command::new(command);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return Err(FormatError::NotFound {
                tool: command.to_string(),
            });
        }
        Err(e) => {
            return Err(FormatError::Failed {
                tool: command.to_string(),
                stderr: e.to_string(),
            });
        }
    };

    let deadline = Instant::now() + Duration::from_secs(timeout_secs as u64);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = child
                    .stdout
                    .take()
                    .map(|s| std::io::read_to_string(s).unwrap_or_default())
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|s| std::io::read_to_string(s).unwrap_or_default())
                    .unwrap_or_default();

                return Ok(ExternalToolResult {
                    stdout,
                    stderr,
                    exit_code: status.code().unwrap_or(-1),
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(FormatError::Timeout {
                        tool: command.to_string(),
                        timeout_secs,
                    });
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(FormatError::Failed {
                    tool: command.to_string(),
                    stderr: format!("try_wait error: {}", e),
                });
            }
        }
    }
}

// ============================================================================
// Type-checker validation (R017)
// ============================================================================

/// A structured error from a type checker.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ValidationError {
    pub line: u32,
    pub column: u32,
    pub message: String,
    pub severity: String,
}

/// Detect the appropriate type checker command and arguments for a file.
///
/// Returns `(command, args)` for the type checker. The `--noEmit` / equivalent
/// flags ensure no output files are produced.
///
/// Supported:
/// - TypeScript/JavaScript/TSX → `npx tsc --noEmit` (fallback: `tsc --noEmit`)
/// - Python → `pyright`
/// - Rust → `cargo check`
/// - Go → `go vet`
pub fn detect_type_checker(
    path: &Path,
    lang: LangId,
    config: &Config,
) -> Option<(String, Vec<String>)> {
    match detect_checker_for_path(path, lang, config) {
        ToolDetection::Found(cmd, args) => Some((cmd, args)),
        ToolDetection::NotConfigured | ToolDetection::NotInstalled { .. } => None,
    }
}

/// Parse type checker output into structured validation errors.
///
/// Handles output formats from tsc, pyright (JSON), cargo check (JSON), and go vet.
/// Filters to errors related to the edited file where feasible.
pub fn parse_checker_output(
    stdout: &str,
    stderr: &str,
    file: &Path,
    checker: &str,
) -> Vec<ValidationError> {
    let checker_name = Path::new(checker)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(checker);
    match checker_name {
        "npx" | "tsc" => parse_tsc_output(stdout, stderr, file),
        "pyright" => parse_pyright_output(stdout, file),
        "cargo" => parse_cargo_output(stdout, stderr, file),
        "go" => parse_go_vet_output(stderr, file),
        _ => Vec::new(),
    }
}

/// Parse tsc output lines like: `path(line,col): error TSxxxx: message`
fn parse_tsc_output(stdout: &str, stderr: &str, file: &Path) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let file_str = file.to_string_lossy();
    // tsc writes diagnostics to stdout (with --pretty false)
    let combined = format!("{}{}", stdout, stderr);
    for line in combined.lines() {
        // Format: path(line,col): severity TSxxxx: message
        // or: path(line,col): severity: message
        if let Some((loc, rest)) = line.split_once("): ") {
            // Check if this error is for our file (compare filename part)
            let file_part = loc.split('(').next().unwrap_or("");
            if !file_str.ends_with(file_part)
                && !file_part.ends_with(&*file_str)
                && file_part != &*file_str
            {
                continue;
            }

            // Parse (line,col) from the location part
            let coords = loc.split('(').last().unwrap_or("");
            let parts: Vec<&str> = coords.split(',').collect();
            let line_num: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let col_num: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);

            // Parse severity and message
            let (severity, message) = if let Some(msg) = rest.strip_prefix("error ") {
                ("error".to_string(), msg.to_string())
            } else if let Some(msg) = rest.strip_prefix("warning ") {
                ("warning".to_string(), msg.to_string())
            } else {
                ("error".to_string(), rest.to_string())
            };

            errors.push(ValidationError {
                line: line_num,
                column: col_num,
                message,
                severity,
            });
        }
    }
    errors
}

/// Parse pyright JSON output.
fn parse_pyright_output(stdout: &str, file: &Path) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let file_str = file.to_string_lossy();

    // pyright --outputjson emits JSON with generalDiagnostics array
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(stdout) {
        if let Some(diags) = json.get("generalDiagnostics").and_then(|d| d.as_array()) {
            for diag in diags {
                // Filter to our file
                let diag_file = diag.get("file").and_then(|f| f.as_str()).unwrap_or("");
                if !diag_file.is_empty()
                    && !file_str.ends_with(diag_file)
                    && !diag_file.ends_with(&*file_str)
                    && diag_file != &*file_str
                {
                    continue;
                }

                let line_num = diag
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("line"))
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0) as u32;
                let col_num = diag
                    .get("range")
                    .and_then(|r| r.get("start"))
                    .and_then(|s| s.get("character"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0) as u32;
                let message = diag
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                let severity = diag
                    .get("severity")
                    .and_then(|s| s.as_str())
                    .unwrap_or("error")
                    .to_lowercase();

                errors.push(ValidationError {
                    line: line_num + 1, // pyright uses 0-indexed lines
                    column: col_num,
                    message,
                    severity,
                });
            }
        }
    }
    errors
}

/// Parse cargo check JSON output, filtering to errors in the target file.
fn parse_cargo_output(stdout: &str, _stderr: &str, file: &Path) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let file_str = file.to_string_lossy();

    for line in stdout.lines() {
        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
            if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
                continue;
            }
            let message_obj = match msg.get("message") {
                Some(m) => m,
                None => continue,
            };

            let level = message_obj
                .get("level")
                .and_then(|l| l.as_str())
                .unwrap_or("error");

            // Only include errors and warnings, skip notes/help
            if level != "error" && level != "warning" {
                continue;
            }

            let text = message_obj
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string();

            // Find the primary span for our file
            if let Some(spans) = message_obj.get("spans").and_then(|s| s.as_array()) {
                for span in spans {
                    let span_file = span.get("file_name").and_then(|f| f.as_str()).unwrap_or("");
                    let is_primary = span
                        .get("is_primary")
                        .and_then(|p| p.as_bool())
                        .unwrap_or(false);

                    if !is_primary {
                        continue;
                    }

                    // Filter to our file
                    if !file_str.ends_with(span_file)
                        && !span_file.ends_with(&*file_str)
                        && span_file != &*file_str
                    {
                        continue;
                    }

                    let line_num =
                        span.get("line_start").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
                    let col_num = span
                        .get("column_start")
                        .and_then(|c| c.as_u64())
                        .unwrap_or(0) as u32;

                    errors.push(ValidationError {
                        line: line_num,
                        column: col_num,
                        message: text.clone(),
                        severity: level.to_string(),
                    });
                }
            }
        }
    }
    errors
}

/// Parse go vet output lines like: `path:line:col: message`
fn parse_go_vet_output(stderr: &str, file: &Path) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let file_str = file.to_string_lossy();

    for line in stderr.lines() {
        // Format: path:line:col: message  OR  path:line: message
        let parts: Vec<&str> = line.splitn(4, ':').collect();
        if parts.len() < 3 {
            continue;
        }

        let err_file = parts[0].trim();
        if !file_str.ends_with(err_file)
            && !err_file.ends_with(&*file_str)
            && err_file != &*file_str
        {
            continue;
        }

        let line_num: u32 = parts[1].trim().parse().unwrap_or(0);
        let (col_num, message) = if parts.len() >= 4 {
            if let Ok(col) = parts[2].trim().parse::<u32>() {
                (col, parts[3].trim().to_string())
            } else {
                // parts[2] is part of the message, not a column
                (0, format!("{}:{}", parts[2].trim(), parts[3].trim()))
            }
        } else {
            (0, parts[2].trim().to_string())
        };

        errors.push(ValidationError {
            line: line_num,
            column: col_num,
            message,
            severity: "error".to_string(),
        });
    }
    errors
}

/// Run the project's type checker and return structured validation errors.
///
/// Returns `(errors, skip_reason)`:
/// - `(errors, None)` — checker ran, errors may be empty (= valid code)
/// - `([], Some(reason))` — checker was skipped
///
/// Skip reasons: `"unsupported_language"`, `"no_checker_configured"`,
/// `"checker_not_installed"`, `"timeout"`, `"error"`
pub fn validate_full(path: &Path, config: &Config) -> (Vec<ValidationError>, Option<String>) {
    let lang = match detect_language(path) {
        Some(l) => l,
        None => {
            log::debug!(
                "[aft] validate: {} (skipped: unsupported_language)",
                path.display()
            );
            return (Vec::new(), Some("unsupported_language".to_string()));
        }
    };
    if !has_checker_support(lang) {
        log::debug!(
            "[aft] validate: {} (skipped: unsupported_language)",
            path.display()
        );
        return (Vec::new(), Some("unsupported_language".to_string()));
    }

    let (cmd, args) = match detect_checker_for_path(path, lang, config) {
        ToolDetection::Found(cmd, args) => (cmd, args),
        ToolDetection::NotConfigured => {
            log::debug!(
                "[aft] validate: {} (skipped: no_checker_configured)",
                path.display()
            );
            return (Vec::new(), Some("no_checker_configured".to_string()));
        }
        ToolDetection::NotInstalled { tool } => {
            log::warn!(
                "validate: {} (skipped: checker_not_installed: {})",
                path.display(),
                tool
            );
            return (Vec::new(), Some("checker_not_installed".to_string()));
        }
    };

    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

    // Type checkers may need to run from the project root
    let working_dir = config.project_root.as_deref();

    match run_external_tool_capture(
        &cmd,
        &arg_refs,
        working_dir,
        config.type_checker_timeout_secs,
    ) {
        Ok(result) => {
            let errors = parse_checker_output(&result.stdout, &result.stderr, path, &cmd);
            log::debug!(
                "[aft] validate: {} ({}, {} errors)",
                path.display(),
                cmd,
                errors.len()
            );
            (errors, None)
        }
        Err(FormatError::Timeout { .. }) => {
            log::error!("validate: {} (skipped: timeout)", path.display());
            (Vec::new(), Some("timeout".to_string()))
        }
        Err(FormatError::NotFound { .. }) => {
            log::warn!(
                "validate: {} (skipped: checker_not_installed)",
                path.display()
            );
            (Vec::new(), Some("checker_not_installed".to_string()))
        }
        Err(FormatError::Failed { stderr, .. }) => {
            log::debug!(
                "[aft] validate: {} (skipped: error: {})",
                path.display(),
                stderr.lines().next().unwrap_or("unknown")
            );
            (Vec::new(), Some("error".to_string()))
        }
        Err(FormatError::UnsupportedLanguage) => {
            log::debug!(
                "[aft] validate: {} (skipped: unsupported_language)",
                path.display()
            );
            (Vec::new(), Some("unsupported_language".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn run_external_tool_not_found() {
        let result = run_external_tool("__nonexistent_tool_xyz__", &[], None, 5);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::NotFound { tool } => {
                assert_eq!(tool, "__nonexistent_tool_xyz__");
            }
            other => panic!("expected NotFound, got: {:?}", other),
        }
    }

    #[test]
    fn run_external_tool_timeout_kills_subprocess() {
        // Use `sleep 60` as a long-running process, timeout after 1 second
        let result = run_external_tool("sleep", &["60"], None, 1);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::Timeout { tool, timeout_secs } => {
                assert_eq!(tool, "sleep");
                assert_eq!(timeout_secs, 1);
            }
            other => panic!("expected Timeout, got: {:?}", other),
        }
    }

    #[test]
    fn run_external_tool_success() {
        let result = run_external_tool("echo", &["hello"], None, 5);
        assert!(result.is_ok());
        let res = result.unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.stdout.contains("hello"));
    }

    #[test]
    fn run_external_tool_nonzero_exit() {
        // `false` always exits with code 1
        let result = run_external_tool("false", &[], None, 5);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::Failed { tool, .. } => {
                assert_eq!(tool, "false");
            }
            other => panic!("expected Failed, got: {:?}", other),
        }
    }

    #[test]
    fn auto_format_unsupported_language() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "hello").unwrap();

        let config = Config::default();
        let (formatted, reason) = auto_format(&path, &config);
        assert!(!formatted);
        assert_eq!(reason.as_deref(), Some("unsupported_language"));
    }

    #[test]
    fn detect_formatter_rust_when_rustfmt_available() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let path = dir.path().join("test.rs");
        let config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let result = detect_formatter(&path, LangId::Rust, &config);
        if resolve_tool("rustfmt", config.project_root.as_deref()).is_some() {
            let (cmd, args) = result.unwrap();
            assert_eq!(cmd, "rustfmt");
            assert!(args.iter().any(|a| a.ends_with("test.rs")));
        } else {
            assert!(result.is_none());
        }
    }

    #[test]
    fn detect_formatter_go_mapping() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module test\ngo 1.21").unwrap();
        let path = dir.path().join("main.go");
        let config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let result = detect_formatter(&path, LangId::Go, &config);
        if resolve_tool("goimports", config.project_root.as_deref()).is_some() {
            let (cmd, args) = result.unwrap();
            assert_eq!(cmd, "goimports");
            assert!(args.contains(&"-w".to_string()));
        } else if resolve_tool("gofmt", config.project_root.as_deref()).is_some() {
            let (cmd, args) = result.unwrap();
            assert_eq!(cmd, "gofmt");
            assert!(args.contains(&"-w".to_string()));
        } else {
            assert!(result.is_none());
        }
    }

    #[test]
    fn detect_formatter_python_mapping() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ruff.toml"), "").unwrap();
        let path = dir.path().join("main.py");
        let config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let result = detect_formatter(&path, LangId::Python, &config);
        if ruff_format_available(config.project_root.as_deref()) {
            let (cmd, args) = result.unwrap();
            assert_eq!(cmd, "ruff");
            assert!(args.contains(&"format".to_string()));
        } else {
            assert!(result.is_none());
        }
    }

    #[test]
    fn detect_formatter_no_config_returns_none() {
        let path = Path::new("test.ts");
        let result = detect_formatter(path, LangId::TypeScript, &Config::default());
        assert!(
            result.is_none(),
            "expected no formatter without project config"
        );
    }

    #[test]
    fn detect_formatter_explicit_override() {
        // Create a temp dir with a fake node_modules/.bin/biome so resolve_tool finds it
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join("node_modules").join(".bin");
        fs::create_dir_all(&bin_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let fake = bin_dir.join("biome");
            fs::write(&fake, "#!/bin/sh\necho 1.0.0").unwrap();
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(not(unix))]
        {
            fs::write(bin_dir.join("biome.cmd"), "@echo 1.0.0").unwrap();
        }

        let path = Path::new("test.ts");
        let mut config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        config
            .formatter
            .insert("typescript".to_string(), "biome".to_string());
        let result = detect_formatter(path, LangId::TypeScript, &config);
        let (cmd, args) = result.unwrap();
        assert!(cmd.contains("biome"), "expected biome in cmd, got: {}", cmd);
        assert!(args.contains(&"format".to_string()));
        assert!(args.contains(&"--write".to_string()));
    }

    #[test]
    fn auto_format_happy_path_rustfmt() {
        if resolve_tool("rustfmt", None).is_none() {
            log::warn!("skipping: rustfmt not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let path = dir.path().join("test.rs");

        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "fn    main()   {{  println!(\"hello\");  }}").unwrap();
        drop(f);

        let config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let (formatted, reason) = auto_format(&path, &config);
        assert!(formatted, "expected formatting to succeed");
        assert!(reason.is_none());

        let content = fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("fn    main"),
            "expected rustfmt to fix spacing"
        );
    }

    #[test]
    fn parse_tsc_output_basic() {
        let stdout = "src/app.ts(10,5): error TS2322: Type 'string' is not assignable to type 'number'.\nsrc/app.ts(20,1): error TS2304: Cannot find name 'foo'.\n";
        let file = Path::new("src/app.ts");
        let errors = parse_tsc_output(stdout, "", file);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].line, 10);
        assert_eq!(errors[0].column, 5);
        assert_eq!(errors[0].severity, "error");
        assert!(errors[0].message.contains("TS2322"));
        assert_eq!(errors[1].line, 20);
    }

    #[test]
    fn parse_tsc_output_filters_other_files() {
        let stdout =
            "other.ts(1,1): error TS2322: wrong file\nsrc/app.ts(5,3): error TS1234: our file\n";
        let file = Path::new("src/app.ts");
        let errors = parse_tsc_output(stdout, "", file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 5);
    }

    #[test]
    fn parse_cargo_output_basic() {
        let json_line = r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","spans":[{"file_name":"src/main.rs","line_start":10,"column_start":5,"is_primary":true}]}}"#;
        let file = Path::new("src/main.rs");
        let errors = parse_cargo_output(json_line, "", file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 10);
        assert_eq!(errors[0].column, 5);
        assert_eq!(errors[0].severity, "error");
        assert!(errors[0].message.contains("mismatched types"));
    }

    #[test]
    fn parse_cargo_output_skips_notes() {
        // Notes and help messages should be filtered out
        let json_line = r#"{"reason":"compiler-message","message":{"level":"note","message":"expected this","spans":[{"file_name":"src/main.rs","line_start":10,"column_start":5,"is_primary":true}]}}"#;
        let file = Path::new("src/main.rs");
        let errors = parse_cargo_output(json_line, "", file);
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn parse_cargo_output_filters_other_files() {
        let json_line = r#"{"reason":"compiler-message","message":{"level":"error","message":"err","spans":[{"file_name":"src/other.rs","line_start":1,"column_start":1,"is_primary":true}]}}"#;
        let file = Path::new("src/main.rs");
        let errors = parse_cargo_output(json_line, "", file);
        assert_eq!(errors.len(), 0);
    }

    #[test]
    fn parse_go_vet_output_basic() {
        let stderr = "main.go:10:5: unreachable code\nmain.go:20: another issue\n";
        let file = Path::new("main.go");
        let errors = parse_go_vet_output(stderr, file);
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].line, 10);
        assert_eq!(errors[0].column, 5);
        assert!(errors[0].message.contains("unreachable code"));
        assert_eq!(errors[1].line, 20);
        assert_eq!(errors[1].column, 0);
    }

    #[test]
    fn parse_pyright_output_basic() {
        let stdout = r#"{"generalDiagnostics":[{"file":"test.py","range":{"start":{"line":4,"character":10}},"message":"Type error here","severity":"error"}]}"#;
        let file = Path::new("test.py");
        let errors = parse_pyright_output(stdout, file);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, 5); // 0-indexed → 1-indexed
        assert_eq!(errors[0].column, 10);
        assert_eq!(errors[0].severity, "error");
        assert!(errors[0].message.contains("Type error here"));
    }

    #[test]
    fn validate_full_unsupported_language() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.txt");
        fs::write(&path, "hello").unwrap();

        let config = Config::default();
        let (errors, reason) = validate_full(&path, &config);
        assert!(errors.is_empty());
        assert_eq!(reason.as_deref(), Some("unsupported_language"));
    }

    #[test]
    fn detect_type_checker_rust() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        let path = dir.path().join("src/main.rs");
        let config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let result = detect_type_checker(&path, LangId::Rust, &config);
        if resolve_tool("cargo", config.project_root.as_deref()).is_some() {
            let (cmd, args) = result.unwrap();
            assert_eq!(cmd, "cargo");
            assert!(args.contains(&"check".to_string()));
        } else {
            assert!(result.is_none());
        }
    }

    #[test]
    fn detect_type_checker_go() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("go.mod"), "module test\ngo 1.21").unwrap();
        let path = dir.path().join("main.go");
        let config = Config {
            project_root: Some(dir.path().to_path_buf()),
            ..Config::default()
        };
        let result = detect_type_checker(&path, LangId::Go, &config);
        if resolve_tool("go", config.project_root.as_deref()).is_some() {
            let (cmd, _args) = result.unwrap();
            // Could be staticcheck or go vet depending on what's installed
            assert!(cmd == "go" || cmd == "staticcheck");
        } else {
            assert!(result.is_none());
        }
    }
    #[test]
    fn run_external_tool_capture_nonzero_not_error() {
        // `false` exits with code 1 — capture should still return Ok
        let result = run_external_tool_capture("false", &[], None, 5);
        assert!(result.is_ok(), "capture should not error on non-zero exit");
        assert_eq!(result.unwrap().exit_code, 1);
    }

    #[test]
    fn run_external_tool_capture_not_found() {
        let result = run_external_tool_capture("__nonexistent_xyz__", &[], None, 5);
        assert!(result.is_err());
        match result.unwrap_err() {
            FormatError::NotFound { tool } => assert_eq!(tool, "__nonexistent_xyz__"),
            other => panic!("expected NotFound, got: {:?}", other),
        }
    }
}
