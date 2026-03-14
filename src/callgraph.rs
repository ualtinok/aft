//! Call graph engine: cross-file call resolution and forward traversal.
//!
//! Builds a lazy, worktree-scoped call graph that resolves calls across files
//! using import chains. Supports depth-limited forward traversal with cycle
//! detection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tree_sitter::{Parser, Tree};

use crate::calls::extract_calls_full;
use crate::error::AftError;
use crate::imports::{self, ImportBlock};
use crate::language::LanguageProvider;
use crate::parser::{detect_language, grammar_for, LangId};

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// A single call site within a function body.
#[derive(Debug, Clone)]
pub struct CallSite {
    /// The short callee name (last segment, e.g. "foo" for `utils.foo()`).
    pub callee_name: String,
    /// The full callee expression (e.g. "utils.foo" for `utils.foo()`).
    pub full_callee: String,
    /// 0-based line number of the call.
    pub line: u32,
    /// Byte range of the call expression in the source.
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Per-file call data: call sites grouped by containing symbol, plus
/// exported symbol names and parsed imports.
#[derive(Debug, Clone)]
pub struct FileCallData {
    /// Map from symbol name → list of call sites within that symbol's body.
    pub calls_by_symbol: HashMap<String, Vec<CallSite>>,
    /// Names of exported symbols in this file.
    pub exported_symbols: Vec<String>,
    /// Parsed import block for cross-file resolution.
    pub import_block: ImportBlock,
    /// Language of the file.
    pub lang: LangId,
}

/// Result of resolving a cross-file call edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeResolution {
    /// Successfully resolved to a specific file and symbol.
    Resolved { file: PathBuf, symbol: String },
    /// Could not resolve — callee name preserved for diagnostics.
    Unresolved { callee_name: String },
}

/// A node in the forward call tree.
#[derive(Debug, Clone, Serialize)]
pub struct CallTreeNode {
    /// Symbol name.
    pub name: String,
    /// File path (relative to project root when possible).
    pub file: String,
    /// Line number (0-based).
    pub line: u32,
    /// Function signature if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Whether this edge was resolved cross-file.
    pub resolved: bool,
    /// Child calls (recursive).
    pub children: Vec<CallTreeNode>,
}

// ---------------------------------------------------------------------------
// CallGraph
// ---------------------------------------------------------------------------

/// Worktree-scoped call graph with lazy per-file construction.
///
/// Files are parsed and analyzed on first access, then cached. The graph
/// can resolve cross-file call edges using the import engine.
pub struct CallGraph {
    /// Cached per-file call data.
    data: HashMap<PathBuf, FileCallData>,
    /// Project root for relative path resolution.
    project_root: PathBuf,
    /// All files discovered in the worktree (lazily populated).
    project_files: Option<Vec<PathBuf>>,
}

impl CallGraph {
    /// Create a new call graph for a project.
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            data: HashMap::new(),
            project_root,
            project_files: None,
        }
    }

    /// Get or build the call data for a file.
    pub fn build_file(&mut self, path: &Path) -> Result<&FileCallData, AftError> {
        let canon = self.canonicalize(path)?;

        if !self.data.contains_key(&canon) {
            let file_data = build_file_data(&canon)?;
            self.data.insert(canon.clone(), file_data);
        }

        Ok(&self.data[&canon])
    }

    /// Resolve a cross-file call edge.
    ///
    /// Given a callee expression and the calling file's import block,
    /// determines which file and symbol the call targets.
    pub fn resolve_cross_file_edge(
        &mut self,
        full_callee: &str,
        short_name: &str,
        caller_file: &Path,
        import_block: &ImportBlock,
    ) -> EdgeResolution {
        // Strategy:
        // 1. Check if the callee matches a namespace import (utils.foo → import * as utils)
        // 2. Check if the callee matches a named import (foo → import { foo } from './mod')
        // 3. Check if the callee matches an aliased import (bar → import { foo as bar } from './mod')
        // 4. Check if the callee matches a default import

        let caller_dir = caller_file.parent().unwrap_or(Path::new("."));

        // Check namespace imports: "utils.foo" where utils is a namespace import
        if full_callee.contains('.') {
            let parts: Vec<&str> = full_callee.splitn(2, '.').collect();
            if parts.len() == 2 {
                let namespace = parts[0];
                let member = parts[1];

                for imp in &import_block.imports {
                    if imp.namespace_import.as_deref() == Some(namespace) {
                        if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                            return EdgeResolution::Resolved {
                                file: resolved_path,
                                symbol: member.to_string(),
                            };
                        }
                    }
                }
            }
        }

        // Check named imports (direct and aliased)
        for imp in &import_block.imports {
            // Direct named import: import { foo } from './utils'
            if imp.names.contains(&short_name.to_string()) {
                if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                    // The name in the import is the original name from the source module
                    return EdgeResolution::Resolved {
                        file: resolved_path,
                        symbol: short_name.to_string(),
                    };
                }
            }

            // Default import: import foo from './utils'
            if imp.default_import.as_deref() == Some(short_name) {
                if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                    return EdgeResolution::Resolved {
                        file: resolved_path,
                        symbol: "default".to_string(),
                    };
                }
            }
        }

        // Check aliased imports by examining the raw import text.
        // ImportStatement.names stores the original name (foo), but the local code
        // uses the alias (bar). We need to parse `import { foo as bar }` to find
        // that `bar` maps to `foo`.
        if let Some((original_name, resolved_path)) =
            resolve_aliased_import(short_name, import_block, caller_dir)
        {
            return EdgeResolution::Resolved {
                file: resolved_path,
                symbol: original_name,
            };
        }

        // Try barrel file re-exports: if any import points to an index file,
        // check if that file re-exports the symbol
        for imp in &import_block.imports {
            if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                // Check if the resolved path is a directory (barrel file)
                if resolved_path.is_dir() {
                    if let Some(index_path) = find_index_file(&resolved_path) {
                        // Check if the index file exports this symbol
                        if self.file_exports_symbol(&index_path, short_name) {
                            return EdgeResolution::Resolved {
                                file: index_path,
                                symbol: short_name.to_string(),
                            };
                        }
                    }
                } else if self.file_exports_symbol(&resolved_path, short_name) {
                    return EdgeResolution::Resolved {
                        file: resolved_path,
                        symbol: short_name.to_string(),
                    };
                }
            }
        }

        EdgeResolution::Unresolved {
            callee_name: short_name.to_string(),
        }
    }

    /// Check if a file exports a given symbol name.
    fn file_exports_symbol(&mut self, path: &Path, symbol_name: &str) -> bool {
        match self.build_file(path) {
            Ok(data) => data.exported_symbols.contains(&symbol_name.to_string()),
            Err(_) => false,
        }
    }

    /// Depth-limited forward call tree traversal.
    ///
    /// Starting from a (file, symbol) pair, recursively follows calls
    /// up to `max_depth` levels. Uses a visited set for cycle detection.
    pub fn forward_tree(
        &mut self,
        file: &Path,
        symbol: &str,
        max_depth: usize,
    ) -> Result<CallTreeNode, AftError> {
        let mut visited = HashSet::new();
        self.forward_tree_inner(file, symbol, max_depth, 0, &mut visited)
    }

    fn forward_tree_inner(
        &mut self,
        file: &Path,
        symbol: &str,
        max_depth: usize,
        current_depth: usize,
        visited: &mut HashSet<(PathBuf, String)>,
    ) -> Result<CallTreeNode, AftError> {
        let canon = self.canonicalize(file)?;
        let visit_key = (canon.clone(), symbol.to_string());

        // Cycle detection
        if visited.contains(&visit_key) {
            return Ok(CallTreeNode {
                name: symbol.to_string(),
                file: self.relative_path(&canon),
                line: 0,
                signature: None,
                resolved: true,
                children: vec![], // cycle — stop recursion
            });
        }

        visited.insert(visit_key.clone());

        // Build file data
        let file_data = build_file_data(&canon)?;
        let import_block = file_data.import_block.clone();
        let _lang = file_data.lang;

        // Get call sites for this symbol
        let call_sites = file_data
            .calls_by_symbol
            .get(symbol)
            .cloned()
            .unwrap_or_default();

        // Get symbol metadata (line, signature)
        let (sym_line, sym_signature) = get_symbol_meta(&canon, symbol);

        // Cache file data
        self.data.insert(canon.clone(), file_data);

        // Build children
        let mut children = Vec::new();

        if current_depth < max_depth {
            for call_site in &call_sites {
                let edge = self.resolve_cross_file_edge(
                    &call_site.full_callee,
                    &call_site.callee_name,
                    &canon,
                    &import_block,
                );

                match edge {
                    EdgeResolution::Resolved {
                        file: ref target_file,
                        ref symbol,
                    } => {
                        match self.forward_tree_inner(
                            target_file,
                            symbol,
                            max_depth,
                            current_depth + 1,
                            visited,
                        ) {
                            Ok(child) => children.push(child),
                            Err(_) => {
                                // Target file can't be parsed — mark as unresolved leaf
                                children.push(CallTreeNode {
                                    name: call_site.callee_name.clone(),
                                    file: self.relative_path(target_file),
                                    line: call_site.line,
                                    signature: None,
                                    resolved: false,
                                    children: vec![],
                                });
                            }
                        }
                    }
                    EdgeResolution::Unresolved { callee_name } => {
                        children.push(CallTreeNode {
                            name: callee_name,
                            file: self.relative_path(&canon),
                            line: call_site.line,
                            signature: None,
                            resolved: false,
                            children: vec![],
                        });
                    }
                }
            }
        }

        visited.remove(&visit_key);

        Ok(CallTreeNode {
            name: symbol.to_string(),
            file: self.relative_path(&canon),
            line: sym_line,
            signature: sym_signature,
            resolved: true,
            children,
        })
    }

    /// Get all project files (lazily discovered).
    pub fn project_files(&mut self) -> &[PathBuf] {
        if self.project_files.is_none() {
            self.project_files = Some(walk_project_files(&self.project_root).collect());
        }
        self.project_files.as_ref().unwrap()
    }

    /// Return a path relative to the project root, or the absolute path if
    /// it's outside the project.
    fn relative_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.project_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    /// Canonicalize a path, falling back to the original if canonicalization fails.
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, AftError> {
        // If the path is relative, resolve it against project_root
        let full_path = if path.is_relative() {
            self.project_root.join(path)
        } else {
            path.to_path_buf()
        };

        // Try canonicalize, fall back to the full path
        Ok(std::fs::canonicalize(&full_path).unwrap_or(full_path))
    }
}

// ---------------------------------------------------------------------------
// File-level building
// ---------------------------------------------------------------------------

/// Build call data for a single file.
fn build_file_data(path: &Path) -> Result<FileCallData, AftError> {
    let lang = detect_language(path).ok_or_else(|| AftError::InvalidRequest {
        message: format!("unsupported file for call graph: {}", path.display()),
    })?;

    let source = std::fs::read_to_string(path).map_err(|e| AftError::FileNotFound {
        path: format!("{}: {}", path.display(), e),
    })?;

    let grammar = grammar_for(lang);
    let mut parser = Parser::new();
    parser.set_language(&grammar).map_err(|e| AftError::ParseError {
        message: format!("grammar init failed for {:?}: {}", lang, e),
    })?;

    let tree = parser.parse(&source, None).ok_or_else(|| AftError::ParseError {
        message: format!("parse failed for {}", path.display()),
    })?;

    // Parse imports
    let import_block = imports::parse_imports(&source, &tree, lang);

    // Get symbols (for call site extraction and export detection)
    let symbols = list_symbols_from_tree(&source, &tree, lang, path);

    // Build calls_by_symbol
    let mut calls_by_symbol: HashMap<String, Vec<CallSite>> = HashMap::new();
    let root = tree.root_node();

    for sym in &symbols {
        let byte_start = line_col_to_byte(&source, sym.start_line, sym.start_col);
        let byte_end = line_col_to_byte(&source, sym.end_line, sym.end_col);

        let raw_calls = extract_calls_full(&source, root, byte_start, byte_end, lang);

        let sites: Vec<CallSite> = raw_calls
            .into_iter()
            .filter(|(_, short, _)| *short != sym.name) // exclude self-references
            .map(|(full, short, line)| CallSite {
                callee_name: short,
                full_callee: full,
                line,
                byte_start,
                byte_end,
            })
            .collect();

        if !sites.is_empty() {
            calls_by_symbol.insert(sym.name.clone(), sites);
        }
    }

    // Collect exported symbol names
    let exported_symbols: Vec<String> = symbols
        .iter()
        .filter(|s| s.exported)
        .map(|s| s.name.clone())
        .collect();

    Ok(FileCallData {
        calls_by_symbol,
        exported_symbols,
        import_block,
        lang,
    })
}

/// Minimal symbol info needed for call graph construction.
#[derive(Debug)]
#[allow(dead_code)]
struct SymbolInfo {
    name: String,
    start_line: u32,
    start_col: u32,
    end_line: u32,
    end_col: u32,
    exported: bool,
    signature: Option<String>,
}

/// Extract symbols from a parsed tree without going through the full
/// FileParser/AppContext machinery.
fn list_symbols_from_tree(
    _source: &str,
    _tree: &Tree,
    _lang: LangId,
    path: &Path,
) -> Vec<SymbolInfo> {
    // Use the parser module's symbol listing via a temporary FileParser
    let mut file_parser = crate::parser::FileParser::new();
    match file_parser.parse(path) {
        Ok(_) => {}
        Err(_) => return vec![],
    }

    // Use the tree-sitter provider to list symbols
    let provider = crate::parser::TreeSitterProvider::new();
    match provider.list_symbols(path) {
        Ok(symbols) => symbols
            .into_iter()
            .map(|s| SymbolInfo {
                name: s.name,
                start_line: s.range.start_line,
                start_col: s.range.start_col,
                end_line: s.range.end_line,
                end_col: s.range.end_col,
                exported: s.exported,
                signature: s.signature,
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// Convert a 0-based line + column to a byte offset in the source.
fn line_col_to_byte(source: &str, line: u32, col: u32) -> usize {
    let mut byte = 0;
    for (i, l) in source.lines().enumerate() {
        if i == line as usize {
            return byte + (col as usize).min(l.len());
        }
        byte += l.len() + 1;
    }
    source.len()
}

/// Get symbol metadata (line, signature) from a file.
fn get_symbol_meta(path: &Path, symbol_name: &str) -> (u32, Option<String>) {
    let provider = crate::parser::TreeSitterProvider::new();
    match provider.list_symbols(path) {
        Ok(symbols) => {
            for s in &symbols {
                if s.name == symbol_name {
                    return (s.range.start_line, s.signature.clone());
                }
            }
            (0, None)
        }
        Err(_) => (0, None),
    }
}

// ---------------------------------------------------------------------------
// Module path resolution
// ---------------------------------------------------------------------------

/// Resolve a module path (e.g. './utils') relative to a directory.
///
/// Tries common file extensions for TypeScript/JavaScript projects.
fn resolve_module_path(from_dir: &Path, module_path: &str) -> Option<PathBuf> {
    // Only handle relative imports
    if !module_path.starts_with('.') {
        return None;
    }

    let base = from_dir.join(module_path);

    // Try exact path first
    if base.is_file() {
        return Some(std::fs::canonicalize(&base).unwrap_or(base));
    }

    // Try common extensions
    let extensions = [".ts", ".tsx", ".js", ".jsx"];
    for ext in &extensions {
        let with_ext = base.with_extension(ext.trim_start_matches('.'));
        if with_ext.is_file() {
            return Some(std::fs::canonicalize(&with_ext).unwrap_or(with_ext));
        }
    }

    // Try as directory with index file
    if base.is_dir() {
        if let Some(index) = find_index_file(&base) {
            return Some(index);
        }
    }

    None
}

/// Find an index file in a directory.
fn find_index_file(dir: &Path) -> Option<PathBuf> {
    let candidates = ["index.ts", "index.tsx", "index.js", "index.jsx"];
    for name in &candidates {
        let p = dir.join(name);
        if p.is_file() {
            return Some(std::fs::canonicalize(&p).unwrap_or(p));
        }
    }
    None
}

/// Resolve an aliased import: `import { foo as bar } from './utils'`
/// where `local_name` is "bar". Returns `(original_name, resolved_file_path)`.
fn resolve_aliased_import(
    local_name: &str,
    import_block: &ImportBlock,
    caller_dir: &Path,
) -> Option<(String, PathBuf)> {
    for imp in &import_block.imports {
        // Parse the raw text to find "as <alias>" patterns
        // This handles: import { foo as bar, baz as qux } from './mod'
        if let Some(original) = find_alias_original(&imp.raw_text, local_name) {
            if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                return Some((original, resolved_path));
            }
        }
    }
    None
}

/// Parse import raw text to find the original name for an alias.
/// Given raw text like `import { foo as bar, baz } from './utils'` and
/// local_name "bar", returns Some("foo").
fn find_alias_original(raw_import: &str, local_name: &str) -> Option<String> {
    // Look for pattern: <original> as <alias>
    // This is a simple text-based search; handles the common TS/JS pattern
    let search = format!(" as {}", local_name);
    if let Some(pos) = raw_import.find(&search) {
        // Walk backwards from `pos` to find the original name
        let before = &raw_import[..pos];
        // The original name is the last word-like token before " as "
        let original = before
            .rsplit(|c: char| c == '{' || c == ',' || c.is_whitespace())
            .find(|s| !s.is_empty())?;
        return Some(original.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Worktree file discovery
// ---------------------------------------------------------------------------

/// Walk project files respecting .gitignore, excluding common non-source dirs.
///
/// Returns an iterator of file paths for supported source file types.
pub fn walk_project_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    use ignore::WalkBuilder;

    let walker = WalkBuilder::new(root)
        .hidden(true)         // skip hidden files/dirs
        .git_ignore(true)     // respect .gitignore
        .git_global(true)     // respect global gitignore
        .git_exclude(true)    // respect .git/info/exclude
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            // Always exclude these directories regardless of .gitignore
            if entry.file_type().map_or(false, |ft| ft.is_dir()) {
                return !matches!(
                    name.as_ref(),
                    "node_modules" | "target" | "venv" | ".venv" | ".git" | "__pycache__"
                        | ".tox" | "dist" | "build"
                );
            }
            true
        })
        .build();

    walker
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map_or(false, |ft| ft.is_file()))
        .filter(|entry| detect_language(entry.path()).is_some())
        .map(|entry| entry.into_path())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a temp directory with TypeScript files for testing.
    fn setup_ts_project() -> TempDir {
        let dir = TempDir::new().unwrap();

        // main.ts: imports from utils and calls functions
        fs::write(
            dir.path().join("main.ts"),
            r#"import { helper, compute } from './utils';
import * as math from './math';

export function main() {
    const a = helper(1);
    const b = compute(a, 2);
    const c = math.add(a, b);
    return c;
}
"#,
        )
        .unwrap();

        // utils.ts: defines helper and compute, imports from helpers
        fs::write(
            dir.path().join("utils.ts"),
            r#"import { double } from './helpers';

export function helper(x: number): number {
    return double(x);
}

export function compute(a: number, b: number): number {
    return a + b;
}
"#,
        )
        .unwrap();

        // helpers.ts: defines double
        fs::write(
            dir.path().join("helpers.ts"),
            r#"export function double(x: number): number {
    return x * 2;
}

export function triple(x: number): number {
    return x * 3;
}
"#,
        )
        .unwrap();

        // math.ts: defines add (for namespace import test)
        fs::write(
            dir.path().join("math.ts"),
            r#"export function add(a: number, b: number): number {
    return a + b;
}

export function subtract(a: number, b: number): number {
    return a - b;
}
"#,
        )
        .unwrap();

        dir
    }

    /// Create a project with import aliasing.
    fn setup_alias_project() -> TempDir {
        let dir = TempDir::new().unwrap();

        fs::write(
            dir.path().join("main.ts"),
            r#"import { helper as h } from './utils';

export function main() {
    return h(42);
}
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("utils.ts"),
            r#"export function helper(x: number): number {
    return x + 1;
}
"#,
        )
        .unwrap();

        dir
    }

    /// Create a project with a cycle: A → B → A.
    fn setup_cycle_project() -> TempDir {
        let dir = TempDir::new().unwrap();

        fs::write(
            dir.path().join("a.ts"),
            r#"import { funcB } from './b';

export function funcA() {
    return funcB();
}
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("b.ts"),
            r#"import { funcA } from './a';

export function funcB() {
    return funcA();
}
"#,
        )
        .unwrap();

        dir
    }

    // --- Single-file call extraction ---

    #[test]
    fn callgraph_single_file_call_extraction() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let file_data = graph.build_file(&dir.path().join("main.ts")).unwrap();
        let main_calls = &file_data.calls_by_symbol["main"];

        let callee_names: Vec<&str> = main_calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(
            callee_names.contains(&"helper"),
            "main should call helper, got: {:?}",
            callee_names
        );
        assert!(
            callee_names.contains(&"compute"),
            "main should call compute, got: {:?}",
            callee_names
        );
        assert!(
            callee_names.contains(&"add"),
            "main should call math.add (short name: add), got: {:?}",
            callee_names
        );
    }

    #[test]
    fn callgraph_file_data_has_exports() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let file_data = graph.build_file(&dir.path().join("utils.ts")).unwrap();
        assert!(
            file_data.exported_symbols.contains(&"helper".to_string()),
            "utils.ts should export helper, got: {:?}",
            file_data.exported_symbols
        );
        assert!(
            file_data.exported_symbols.contains(&"compute".to_string()),
            "utils.ts should export compute, got: {:?}",
            file_data.exported_symbols
        );
    }

    // --- Cross-file resolution ---

    #[test]
    fn callgraph_resolve_direct_import() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let main_path = dir.path().join("main.ts");
        let file_data = graph.build_file(&main_path).unwrap();
        let import_block = file_data.import_block.clone();

        let edge = graph.resolve_cross_file_edge("helper", "helper", &main_path, &import_block);
        match edge {
            EdgeResolution::Resolved { file, symbol } => {
                assert!(
                    file.ends_with("utils.ts"),
                    "helper should resolve to utils.ts, got: {:?}",
                    file
                );
                assert_eq!(symbol, "helper");
            }
            EdgeResolution::Unresolved { callee_name } => {
                panic!("Expected resolved, got unresolved: {}", callee_name);
            }
        }
    }

    #[test]
    fn callgraph_resolve_namespace_import() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let main_path = dir.path().join("main.ts");
        let file_data = graph.build_file(&main_path).unwrap();
        let import_block = file_data.import_block.clone();

        let edge = graph.resolve_cross_file_edge("math.add", "add", &main_path, &import_block);
        match edge {
            EdgeResolution::Resolved { file, symbol } => {
                assert!(
                    file.ends_with("math.ts"),
                    "math.add should resolve to math.ts, got: {:?}",
                    file
                );
                assert_eq!(symbol, "add");
            }
            EdgeResolution::Unresolved { callee_name } => {
                panic!("Expected resolved, got unresolved: {}", callee_name);
            }
        }
    }

    #[test]
    fn callgraph_resolve_aliased_import() {
        let dir = setup_alias_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let main_path = dir.path().join("main.ts");
        let file_data = graph.build_file(&main_path).unwrap();
        let import_block = file_data.import_block.clone();

        let edge = graph.resolve_cross_file_edge("h", "h", &main_path, &import_block);
        match edge {
            EdgeResolution::Resolved { file, symbol } => {
                assert!(
                    file.ends_with("utils.ts"),
                    "h (alias for helper) should resolve to utils.ts, got: {:?}",
                    file
                );
                assert_eq!(symbol, "helper");
            }
            EdgeResolution::Unresolved { callee_name } => {
                panic!("Expected resolved, got unresolved: {}", callee_name);
            }
        }
    }

    #[test]
    fn callgraph_unresolved_edge_marked() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let main_path = dir.path().join("main.ts");
        let file_data = graph.build_file(&main_path).unwrap();
        let import_block = file_data.import_block.clone();

        let edge = graph.resolve_cross_file_edge(
            "unknownFunc",
            "unknownFunc",
            &main_path,
            &import_block,
        );
        assert_eq!(
            edge,
            EdgeResolution::Unresolved {
                callee_name: "unknownFunc".to_string()
            },
            "Unknown callee should be unresolved"
        );
    }

    // --- Cycle detection ---

    #[test]
    fn callgraph_cycle_detection_stops() {
        let dir = setup_cycle_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // This should NOT infinite loop
        let tree = graph
            .forward_tree(&dir.path().join("a.ts"), "funcA", 10)
            .unwrap();

        assert_eq!(tree.name, "funcA");
        assert!(tree.resolved);

        // funcA calls funcB, funcB calls funcA (cycle), so the depth should be bounded
        // The tree should have children but not infinitely deep
        fn count_depth(node: &CallTreeNode) -> usize {
            if node.children.is_empty() {
                1
            } else {
                1 + node
                    .children
                    .iter()
                    .map(|c| count_depth(c))
                    .max()
                    .unwrap_or(0)
            }
        }

        let depth = count_depth(&tree);
        assert!(
            depth <= 4,
            "Cycle should be detected and bounded, depth was: {}",
            depth
        );
    }

    // --- Depth limiting ---

    #[test]
    fn callgraph_depth_limit_truncates() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // main → helper → double, main → compute
        // With depth 1, we should see direct callees but not their children
        let tree = graph
            .forward_tree(&dir.path().join("main.ts"), "main", 1)
            .unwrap();

        assert_eq!(tree.name, "main");

        // At depth 1, children should exist (direct calls) but their children should be empty
        for child in &tree.children {
            assert!(
                child.children.is_empty(),
                "At depth 1, child '{}' should have no children, got {:?}",
                child.name,
                child.children.len()
            );
        }
    }

    #[test]
    fn callgraph_depth_zero_no_children() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let tree = graph
            .forward_tree(&dir.path().join("main.ts"), "main", 0)
            .unwrap();

        assert_eq!(tree.name, "main");
        assert!(
            tree.children.is_empty(),
            "At depth 0, should have no children"
        );
    }

    // --- Forward tree cross-file ---

    #[test]
    fn callgraph_forward_tree_cross_file() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // main → helper (in utils.ts) → double (in helpers.ts)
        let tree = graph
            .forward_tree(&dir.path().join("main.ts"), "main", 5)
            .unwrap();

        assert_eq!(tree.name, "main");
        assert!(tree.resolved);

        // Find the helper child
        let helper_child = tree.children.iter().find(|c| c.name == "helper");
        assert!(
            helper_child.is_some(),
            "main should have helper as child, children: {:?}",
            tree.children.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        let helper = helper_child.unwrap();
        assert!(
            helper.file.ends_with("utils.ts") || helper.file == "utils.ts",
            "helper should be in utils.ts, got: {}",
            helper.file
        );

        // helper should call double (in helpers.ts)
        let double_child = helper.children.iter().find(|c| c.name == "double");
        assert!(
            double_child.is_some(),
            "helper should call double, children: {:?}",
            helper.children.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        let double = double_child.unwrap();
        assert!(
            double.file.ends_with("helpers.ts") || double.file == "helpers.ts",
            "double should be in helpers.ts, got: {}",
            double.file
        );
    }

    // --- Worktree walker ---

    #[test]
    fn callgraph_walker_excludes_gitignored() {
        let dir = TempDir::new().unwrap();

        // Create a .gitignore
        fs::write(dir.path().join(".gitignore"), "ignored_dir/\n").unwrap();

        // Create files
        fs::write(dir.path().join("main.ts"), "export function main() {}").unwrap();
        fs::create_dir(dir.path().join("ignored_dir")).unwrap();
        fs::write(
            dir.path().join("ignored_dir").join("secret.ts"),
            "export function secret() {}",
        )
        .unwrap();

        // Also create node_modules (should always be excluded)
        fs::create_dir(dir.path().join("node_modules")).unwrap();
        fs::write(
            dir.path().join("node_modules").join("dep.ts"),
            "export function dep() {}",
        )
        .unwrap();

        // Init git repo for .gitignore to work
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let files: Vec<PathBuf> = walk_project_files(dir.path()).collect();
        let file_names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(
            file_names.contains(&"main.ts".to_string()),
            "Should include main.ts, got: {:?}",
            file_names
        );
        assert!(
            !file_names.contains(&"secret.ts".to_string()),
            "Should exclude gitignored secret.ts, got: {:?}",
            file_names
        );
        assert!(
            !file_names.contains(&"dep.ts".to_string()),
            "Should exclude node_modules, got: {:?}",
            file_names
        );
    }

    #[test]
    fn callgraph_walker_only_source_files() {
        let dir = TempDir::new().unwrap();

        fs::write(dir.path().join("main.ts"), "export function main() {}").unwrap();
        fs::write(dir.path().join("readme.md"), "# Hello").unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();

        let files: Vec<PathBuf> = walk_project_files(dir.path()).collect();
        let file_names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        assert!(file_names.contains(&"main.ts".to_string()));
        assert!(
            !file_names.contains(&"readme.md".to_string()),
            "Should not include non-source files"
        );
        assert!(
            !file_names.contains(&"data.json".to_string()),
            "Should not include non-source files"
        );
    }

    // --- find_alias_original ---

    #[test]
    fn callgraph_find_alias_original_simple() {
        let raw = "import { foo as bar } from './utils';";
        assert_eq!(find_alias_original(raw, "bar"), Some("foo".to_string()));
    }

    #[test]
    fn callgraph_find_alias_original_multiple() {
        let raw = "import { foo as bar, baz as qux } from './utils';";
        assert_eq!(find_alias_original(raw, "bar"), Some("foo".to_string()));
        assert_eq!(find_alias_original(raw, "qux"), Some("baz".to_string()));
    }

    #[test]
    fn callgraph_find_alias_no_match() {
        let raw = "import { foo } from './utils';";
        assert_eq!(find_alias_original(raw, "foo"), None);
    }
}
