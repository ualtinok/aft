//! Call graph engine: cross-file call resolution and forward traversal.
//!
//! Builds a lazy, worktree-scoped call graph that resolves calls across files
//! using import chains. Supports depth-limited forward traversal with cycle
//! detection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tree_sitter::{Parser, Tree};

use crate::calls::extract_calls_full;
use crate::edit::line_col_to_byte;
use crate::error::AftError;
use crate::imports::{self, ImportBlock};
use crate::language::LanguageProvider;
use crate::parser::{detect_language, grammar_for, LangId};
use crate::symbols::SymbolKind;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

type SharedPath = Arc<PathBuf>;
type SharedStr = Arc<str>;
type ReverseIndex = HashMap<PathBuf, HashMap<String, Vec<IndexedCallerSite>>>;

/// A single call site within a function body.
#[derive(Debug, Clone)]
pub struct CallSite {
    /// The short callee name (last segment, e.g. "foo" for `utils.foo()`).
    pub callee_name: String,
    /// The full callee expression (e.g. "utils.foo" for `utils.foo()`).
    pub full_callee: String,
    /// 1-based line number of the call.
    pub line: u32,
    /// Byte range of the call expression in the source.
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Per-symbol metadata for entry point detection (avoids re-parsing).
#[derive(Debug, Clone, Serialize)]
pub struct SymbolMeta {
    /// The kind of symbol (function, class, method, etc).
    pub kind: SymbolKind,
    /// Whether this symbol is exported.
    pub exported: bool,
    /// Function/method signature if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// Per-file call data: call sites grouped by containing symbol, plus
/// exported symbol names and parsed imports.
#[derive(Debug, Clone)]
pub struct FileCallData {
    /// Map from symbol name → list of call sites within that symbol's body.
    pub calls_by_symbol: HashMap<String, Vec<CallSite>>,
    /// Names of exported symbols in this file.
    pub exported_symbols: Vec<String>,
    /// Per-symbol metadata (kind, exported, signature).
    pub symbol_metadata: HashMap<String, SymbolMeta>,
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

/// A single caller site: who calls a given symbol and from where.
#[derive(Debug, Clone, Serialize)]
pub struct CallerSite {
    /// File containing the caller.
    pub caller_file: PathBuf,
    /// Symbol that makes the call.
    pub caller_symbol: String,
    /// 1-based line number of the call.
    pub line: u32,
    /// 0-based column (byte start within file, kept for future use).
    pub col: u32,
    /// Whether the edge was resolved via import chain.
    pub resolved: bool,
}

#[derive(Debug, Clone)]
struct IndexedCallerSite {
    caller_file: SharedPath,
    caller_symbol: SharedStr,
    line: u32,
    col: u32,
    resolved: bool,
}

/// A group of callers from a single file.
#[derive(Debug, Clone, Serialize)]
pub struct CallerGroup {
    /// File path (relative to project root).
    pub file: String,
    /// Individual call sites in this file.
    pub callers: Vec<CallerEntry>,
}

/// A single caller entry within a CallerGroup.
#[derive(Debug, Clone, Serialize)]
pub struct CallerEntry {
    pub symbol: String,
    /// 1-based line number of the call.
    pub line: u32,
}

/// Result of a `callers_of` query.
#[derive(Debug, Clone, Serialize)]
pub struct CallersResult {
    /// Target symbol queried.
    pub symbol: String,
    /// Target file queried.
    pub file: String,
    /// Caller groups, one per calling file.
    pub callers: Vec<CallerGroup>,
    /// Total number of call sites found.
    pub total_callers: usize,
    /// Number of files scanned to build the reverse index.
    pub scanned_files: usize,
}

/// A node in the forward call tree.
#[derive(Debug, Clone, Serialize)]
pub struct CallTreeNode {
    /// Symbol name.
    pub name: String,
    /// File path (relative to project root when possible).
    pub file: String,
    /// 1-based line number.
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
// Entry point detection
// ---------------------------------------------------------------------------

/// Well-known main/init function names (case-insensitive exact match).
const MAIN_INIT_NAMES: &[&str] = &["main", "init", "setup", "bootstrap", "run"];

/// Determine whether a symbol is an entry point.
///
/// Entry points are:
/// - Exported standalone functions (not methods — methods are class members)
/// - Functions matching well-known main/init patterns (any language)
/// - Test functions matching language-specific patterns
pub fn is_entry_point(name: &str, kind: &SymbolKind, exported: bool, lang: LangId) -> bool {
    // Exported standalone functions
    if exported && *kind == SymbolKind::Function {
        return true;
    }

    // Main/init patterns (case-insensitive exact match, any kind)
    let lower = name.to_lowercase();
    if MAIN_INIT_NAMES.contains(&lower.as_str()) {
        return true;
    }

    // Test patterns by language
    match lang {
        LangId::TypeScript | LangId::JavaScript | LangId::Tsx => {
            // describe, it, test (exact), or starts with test/spec
            matches!(lower.as_str(), "describe" | "it" | "test")
                || lower.starts_with("test")
                || lower.starts_with("spec")
        }
        LangId::Python => {
            // starts with test_ or matches setUp/tearDown
            lower.starts_with("test_") || matches!(name, "setUp" | "tearDown")
        }
        LangId::Rust => {
            // starts with test_
            lower.starts_with("test_")
        }
        LangId::Go => {
            // starts with Test (case-sensitive)
            name.starts_with("Test")
        }
        LangId::C
        | LangId::Cpp
        | LangId::Zig
        | LangId::CSharp
        | LangId::Bash
        | LangId::Html
        | LangId::Markdown => false,
    }
}

// ---------------------------------------------------------------------------
// Trace-to types
// ---------------------------------------------------------------------------

/// A single hop in a trace path.
#[derive(Debug, Clone, Serialize)]
pub struct TraceHop {
    /// Symbol name at this hop.
    pub symbol: String,
    /// File path (relative to project root).
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// Function signature if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Whether this hop is an entry point.
    pub is_entry_point: bool,
}

/// A complete path from an entry point to the target symbol (top-down).
#[derive(Debug, Clone, Serialize)]
pub struct TracePath {
    /// Hops from entry point (first) to target (last).
    pub hops: Vec<TraceHop>,
}

/// Result of a `trace_to` query.
#[derive(Debug, Clone, Serialize)]
pub struct TraceToResult {
    /// The target symbol that was traced.
    pub target_symbol: String,
    /// The target file (relative to project root).
    pub target_file: String,
    /// Complete paths from entry points to the target.
    pub paths: Vec<TracePath>,
    /// Total number of complete paths found.
    pub total_paths: usize,
    /// Number of distinct entry points found across all paths.
    pub entry_points_found: usize,
    /// Whether any path was cut short by the depth limit.
    pub max_depth_reached: bool,
    /// Number of paths that reached a dead end (no callers, not entry point).
    pub truncated_paths: usize,
}

// ---------------------------------------------------------------------------
// Impact analysis types
// ---------------------------------------------------------------------------

/// A single caller in an impact analysis result.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactCaller {
    /// Symbol that calls the target.
    pub caller_symbol: String,
    /// File containing the caller (relative to project root).
    pub caller_file: String,
    /// 1-based line number of the call site.
    pub line: u32,
    /// Caller's function/method signature, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Whether the caller is an entry point.
    pub is_entry_point: bool,
    /// Source line at the call site (trimmed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_expression: Option<String>,
    /// Parameter names extracted from the caller's signature.
    pub parameters: Vec<String>,
}

/// Result of an `impact` query — enriched callers analysis.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactResult {
    /// The target symbol being analyzed.
    pub symbol: String,
    /// The target file (relative to project root).
    pub file: String,
    /// Target symbol's signature, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Parameter names extracted from the target's signature.
    pub parameters: Vec<String>,
    /// Total number of affected call sites.
    pub total_affected: usize,
    /// Number of distinct files containing callers.
    pub affected_files: usize,
    /// Enriched caller details.
    pub callers: Vec<ImpactCaller>,
}

// ---------------------------------------------------------------------------
// Data flow tracking types
// ---------------------------------------------------------------------------

/// A single hop in a data flow trace.
#[derive(Debug, Clone, Serialize)]
pub struct DataFlowHop {
    /// File path (relative to project root).
    pub file: String,
    /// Symbol (function/method) containing this hop.
    pub symbol: String,
    /// Variable or parameter name being tracked at this hop.
    pub variable: String,
    /// 1-based line number.
    pub line: u32,
    /// Type of data flow: "assignment", "parameter", or "return".
    pub flow_type: String,
    /// Whether this hop is an approximation (destructuring, spread, unresolved).
    pub approximate: bool,
}

/// Result of a `trace_data` query — tracks how an expression flows through
/// variable assignments and function parameters.
#[derive(Debug, Clone, Serialize)]
pub struct TraceDataResult {
    /// The expression being tracked.
    pub expression: String,
    /// The file where tracking started.
    pub origin_file: String,
    /// The symbol where tracking started.
    pub origin_symbol: String,
    /// Hops through assignments and parameters.
    pub hops: Vec<DataFlowHop>,
    /// Whether tracking stopped due to depth limit.
    pub depth_limited: bool,
}

/// Extract parameter names from a function signature string.
///
/// Strips language-specific receivers (`self`, `&self`, `&mut self` for Rust,
/// `self` for Python) and type annotations / default values. Returns just
/// the parameter names.
pub fn extract_parameters(signature: &str, lang: LangId) -> Vec<String> {
    // Find the parameter list between parentheses
    let start = match signature.find('(') {
        Some(i) => i + 1,
        None => return Vec::new(),
    };
    let end = match signature[start..].find(')') {
        Some(i) => start + i,
        None => return Vec::new(),
    };

    let params_str = &signature[start..end].trim();
    if params_str.is_empty() {
        return Vec::new();
    }

    // Split on commas, respecting nested generics/brackets
    let parts = split_params(params_str);

    let mut result = Vec::new();
    for part in parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip language-specific receivers
        match lang {
            LangId::Rust => {
                let normalized = trimmed.replace(' ', "");
                if normalized == "self"
                    || normalized == "&self"
                    || normalized == "&mutself"
                    || normalized == "mutself"
                {
                    continue;
                }
            }
            LangId::Python => {
                if trimmed == "self" || trimmed.starts_with("self:") {
                    continue;
                }
            }
            _ => {}
        }

        // Extract just the parameter name
        let name = extract_param_name(trimmed, lang);
        if !name.is_empty() {
            result.push(name);
        }
    }

    result
}

/// Split parameter string on commas, respecting nested brackets/generics.
fn split_params(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0i32;

    for ch in s.chars() {
        match ch {
            '<' | '[' | '{' | '(' => {
                depth += 1;
                current.push(ch);
            }
            '>' | ']' | '}' | ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                parts.push(current.clone());
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// Extract the parameter name from a single parameter declaration.
///
/// Handles:
/// - TS/JS: `name: Type`, `name = default`, `...name`, `name?: Type`
/// - Python: `name: Type`, `name=default`, `*args`, `**kwargs`
/// - Rust: `name: Type`, `mut name: Type`
/// - Go: `name Type`, `name, name2 Type`
fn extract_param_name(param: &str, lang: LangId) -> String {
    let trimmed = param.trim();

    // Handle rest/spread params
    let working = if trimmed.starts_with("...") {
        &trimmed[3..]
    } else if trimmed.starts_with("**") {
        &trimmed[2..]
    } else if trimmed.starts_with('*') && lang == LangId::Python {
        &trimmed[1..]
    } else {
        trimmed
    };

    // Rust: `mut name: Type` → strip `mut `
    let working = if lang == LangId::Rust && working.starts_with("mut ") {
        &working[4..]
    } else {
        working
    };

    // Strip type annotation (`: Type`) and default values (`= default`)
    // Take only the name part — everything before `:`, `=`, or `?`
    let name = working
        .split(|c: char| c == ':' || c == '=')
        .next()
        .unwrap_or("")
        .trim();

    // Strip trailing `?` (optional params in TS)
    let name = name.trim_end_matches('?');

    // For Go, the name might be just `name Type` — take the first word
    if lang == LangId::Go && !name.contains(' ') {
        return name.to_string();
    }
    if lang == LangId::Go {
        return name.split_whitespace().next().unwrap_or("").to_string();
    }

    name.to_string()
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
    /// Reverse index: target_file → target_symbol → callers.
    /// Built lazily on first `callers_of` call, cleared on `invalidate_file`.
    reverse_index: Option<ReverseIndex>,
}

impl CallGraph {
    /// Create a new call graph for a project.
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            data: HashMap::new(),
            project_root,
            project_files: None,
            reverse_index: None,
        }
    }

    /// Get the project root directory.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    fn resolve_cross_file_edge_with_exports<F>(
        full_callee: &str,
        short_name: &str,
        caller_file: &Path,
        import_block: &ImportBlock,
        mut file_exports_symbol: F,
    ) -> EdgeResolution
    where
        F: FnMut(&Path, &str) -> bool,
    {
        let caller_dir = caller_file.parent().unwrap_or(Path::new("."));

        // Check namespace imports: "utils.foo" where utils is a namespace import
        if full_callee.contains('.') {
            let parts: Vec<&str> = full_callee.splitn(2, '.').collect();
            if parts.len() == 2 {
                let namespace = parts[0];
                let member = parts[1];

                for imp in &import_block.imports {
                    if imp.namespace_import.as_deref() == Some(namespace) {
                        if let Some(resolved_path) =
                            resolve_module_path(caller_dir, &imp.module_path)
                        {
                            return EdgeResolution::Resolved {
                                file: resolved_path,
                                symbol: member.to_owned(),
                            };
                        }
                    }
                }
            }
        }

        // Check named imports (direct and aliased)
        for imp in &import_block.imports {
            // Direct named import: import { foo } from './utils'
            if imp.names.iter().any(|name| name == short_name) {
                if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                    // The name in the import is the original name from the source module
                    return EdgeResolution::Resolved {
                        file: resolved_path,
                        symbol: short_name.to_owned(),
                    };
                }
            }

            // Default import: import foo from './utils'
            if imp.default_import.as_deref() == Some(short_name) {
                if let Some(resolved_path) = resolve_module_path(caller_dir, &imp.module_path) {
                    return EdgeResolution::Resolved {
                        file: resolved_path,
                        symbol: "default".to_owned(),
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
                        if file_exports_symbol(&index_path, short_name) {
                            return EdgeResolution::Resolved {
                                file: index_path,
                                symbol: short_name.to_owned(),
                            };
                        }
                    }
                } else if file_exports_symbol(&resolved_path, short_name) {
                    return EdgeResolution::Resolved {
                        file: resolved_path,
                        symbol: short_name.to_owned(),
                    };
                }
            }
        }

        EdgeResolution::Unresolved {
            callee_name: short_name.to_owned(),
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
        Self::resolve_cross_file_edge_with_exports(
            full_callee,
            short_name,
            caller_file,
            import_block,
            |path, symbol_name| self.file_exports_symbol(path, symbol_name),
        )
    }

    /// Check if a file exports a given symbol name.
    fn file_exports_symbol(&mut self, path: &Path, symbol_name: &str) -> bool {
        match self.build_file(path) {
            Ok(data) => data.exported_symbols.iter().any(|name| name == symbol_name),
            Err(_) => false,
        }
    }

    fn file_exports_symbol_cached(&self, path: &Path, symbol_name: &str) -> bool {
        self.lookup_file_data(path)
            .map(|data| data.exported_symbols.iter().any(|name| name == symbol_name))
            .unwrap_or(false)
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
            let (line, signature) = get_symbol_meta(&canon, symbol);
            return Ok(CallTreeNode {
                name: symbol.to_string(),
                file: self.relative_path(&canon),
                line,
                signature,
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
            let project_root = self.project_root.clone();
            self.project_files = Some(walk_project_files(&project_root).collect());
        }
        self.project_files.as_deref().unwrap_or(&[])
    }

    /// Build the reverse index by scanning all project files.
    ///
    /// For each file, builds the call data (if not cached), then for each
    /// (symbol, call_sites) pair, resolves cross-file edges and inserts
    /// into the reverse map: `(target_file, target_symbol) → Vec<CallerSite>`.
    fn build_reverse_index(&mut self) {
        // Discover all project files first
        let all_files = self.project_files().to_vec();

        // Build file data for all project files
        for f in &all_files {
            let _ = self.build_file(f);
        }

        // Now build the reverse map
        let mut reverse: ReverseIndex = HashMap::new();

        for caller_file in &all_files {
            // Canonicalize the caller file path for consistent lookups
            let canon_caller = Arc::new(
                std::fs::canonicalize(caller_file).unwrap_or_else(|_| caller_file.clone()),
            );
            let file_data = match self
                .data
                .get(caller_file)
                .or_else(|| self.data.get(canon_caller.as_ref()))
            {
                Some(d) => d,
                None => continue,
            };

            for (symbol_name, call_sites) in &file_data.calls_by_symbol {
                let caller_symbol: SharedStr = Arc::from(symbol_name.as_str());

                for call_site in call_sites {
                    let edge = Self::resolve_cross_file_edge_with_exports(
                        &call_site.full_callee,
                        &call_site.callee_name,
                        canon_caller.as_ref(),
                        &file_data.import_block,
                        |path, symbol_name| self.file_exports_symbol_cached(path, symbol_name),
                    );

                    let (target_file, target_symbol, resolved) = match edge {
                        EdgeResolution::Resolved { file, symbol } => (file, symbol, true),
                        EdgeResolution::Unresolved { callee_name } => {
                            (canon_caller.as_ref().clone(), callee_name, false)
                        }
                    };

                    reverse
                        .entry(target_file)
                        .or_default()
                        .entry(target_symbol)
                        .or_default()
                        .push(IndexedCallerSite {
                            caller_file: Arc::clone(&canon_caller),
                            caller_symbol: Arc::clone(&caller_symbol),
                            line: call_site.line,
                            col: 0,
                            resolved,
                        });
                }
            }
        }

        self.reverse_index = Some(reverse);
    }

    fn reverse_sites(&self, file: &Path, symbol: &str) -> Option<&[IndexedCallerSite]> {
        self.reverse_index
            .as_ref()?
            .get(file)?
            .get(symbol)
            .map(Vec::as_slice)
    }

    /// Get callers of a symbol in a file, grouped by calling file.
    ///
    /// Builds the reverse index on first call (scans all project files).
    /// Supports recursive depth expansion: depth=1 returns direct callers,
    /// depth=2 returns callers-of-callers, etc. depth=0 is treated as 1.
    pub fn callers_of(
        &mut self,
        file: &Path,
        symbol: &str,
        depth: usize,
    ) -> Result<CallersResult, AftError> {
        let canon = self.canonicalize(file)?;

        // Ensure file is built (may already be cached)
        self.build_file(&canon)?;

        // Build the reverse index if not cached
        if self.reverse_index.is_none() {
            self.build_reverse_index();
        }

        let scanned_files = self.project_files.as_ref().map(|f| f.len()).unwrap_or(0);
        let effective_depth = if depth == 0 { 1 } else { depth };

        let mut visited = HashSet::new();
        let mut all_sites: Vec<CallerSite> = Vec::new();
        self.collect_callers_recursive(
            &canon,
            symbol,
            effective_depth,
            0,
            &mut visited,
            &mut all_sites,
        );

        // Group by file

        let mut groups_map: HashMap<PathBuf, Vec<CallerEntry>> = HashMap::new();
        let total_callers = all_sites.len();
        for site in all_sites {
            let caller_file: PathBuf = site.caller_file;
            let caller_symbol: String = site.caller_symbol;
            let line = site.line;
            let entry = CallerEntry {
                symbol: caller_symbol,
                line,
            };

            if let Some(entries) = groups_map.get_mut(&caller_file) {
                entries.push(entry);
            } else {
                groups_map.insert(caller_file, vec![entry]);
            }
        }

        let mut callers: Vec<CallerGroup> = groups_map
            .into_iter()
            .map(|(file_path, entries)| CallerGroup {
                file: self.relative_path(&file_path),
                callers: entries,
            })
            .collect();

        // Sort groups by file path for deterministic output
        callers.sort_by(|a, b| a.file.cmp(&b.file));

        Ok(CallersResult {
            symbol: symbol.to_string(),
            file: self.relative_path(&canon),
            callers,
            total_callers,
            scanned_files,
        })
    }

    /// Trace backward from a symbol to all entry points.
    ///
    /// Returns complete paths (top-down: entry point first, target last).
    /// Uses BFS backward through the reverse index, with per-path cycle
    /// detection and depth limiting.
    pub fn trace_to(
        &mut self,
        file: &Path,
        symbol: &str,
        max_depth: usize,
    ) -> Result<TraceToResult, AftError> {
        let canon = self.canonicalize(file)?;

        // Ensure file is built
        self.build_file(&canon)?;

        // Build the reverse index if not cached
        if self.reverse_index.is_none() {
            self.build_reverse_index();
        }

        let target_rel = self.relative_path(&canon);
        let effective_max = if max_depth == 0 { 10 } else { max_depth };
        if self.reverse_index.is_none() {
            return Err(AftError::ParseError {
                message: format!(
                    "reverse index unavailable after building callers for {}",
                    canon.display()
                ),
            });
        }

        // Get line/signature for the target symbol
        let (target_line, target_sig) = get_symbol_meta(&canon, symbol);

        // Check if target itself is an entry point
        let target_is_entry = self
            .lookup_file_data(&canon)
            .and_then(|fd| {
                let meta = fd.symbol_metadata.get(symbol)?;
                Some(is_entry_point(symbol, &meta.kind, meta.exported, fd.lang))
            })
            .unwrap_or(false);

        // BFS state: each item is a partial path (bottom-up, will be reversed later)
        // Each path element: (canonicalized file, symbol name, line, signature)
        type PathElem = (SharedPath, SharedStr, u32, Option<String>);
        let mut complete_paths: Vec<Vec<PathElem>> = Vec::new();
        let mut max_depth_reached = false;
        let mut truncated_paths: usize = 0;

        // Initial path starts at the target
        let initial: Vec<PathElem> = vec![(
            Arc::new(canon.clone()),
            Arc::from(symbol),
            target_line,
            target_sig,
        )];

        // If the target itself is an entry point, record it as a trivial path
        if target_is_entry {
            complete_paths.push(initial.clone());
        }

        // Queue of (current_path, depth)
        let mut queue: Vec<(Vec<PathElem>, usize)> = vec![(initial, 0)];

        while let Some((path, depth)) = queue.pop() {
            if depth >= effective_max {
                max_depth_reached = true;
                continue;
            }

            let Some((current_file, current_symbol, _, _)) = path.last() else {
                continue;
            };

            // Look up callers in reverse index
            let callers = match self.reverse_sites(current_file.as_ref(), current_symbol.as_ref()) {
                Some(sites) => sites,
                None => {
                    // Dead end: no callers and not an entry point
                    // (if it were an entry point, we'd have recorded it already)
                    if path.len() > 1 {
                        // Only count as truncated if this isn't the target itself
                        // (the target with no callers is just "no paths found")
                        truncated_paths += 1;
                    }
                    continue;
                }
            };

            let mut has_new_path = false;
            for site in callers {
                // Cycle detection: skip if this caller is already in the current path
                if path.iter().any(|(file_path, sym, _, _)| {
                    file_path.as_ref() == site.caller_file.as_ref()
                        && sym.as_ref() == site.caller_symbol.as_ref()
                }) {
                    continue;
                }

                has_new_path = true;

                // Get caller's metadata
                let (caller_line, caller_sig) =
                    get_symbol_meta(site.caller_file.as_ref(), site.caller_symbol.as_ref());

                let mut new_path = path.clone();
                new_path.push((
                    Arc::clone(&site.caller_file),
                    Arc::clone(&site.caller_symbol),
                    caller_line,
                    caller_sig,
                ));

                // Check if this caller is an entry point
                // Try both canonical and non-canonical keys (build_reverse_index
                // may have stored data under the raw walker path)
                let caller_is_entry = self
                    .lookup_file_data(site.caller_file.as_ref())
                    .and_then(|fd| {
                        let meta = fd.symbol_metadata.get(site.caller_symbol.as_ref())?;
                        Some(is_entry_point(
                            site.caller_symbol.as_ref(),
                            &meta.kind,
                            meta.exported,
                            fd.lang,
                        ))
                    })
                    .unwrap_or(false);

                if caller_is_entry {
                    complete_paths.push(new_path.clone());
                }
                // Always continue searching backward — there may be longer
                // paths through other entry points beyond this one
                queue.push((new_path, depth + 1));
            }

            // If we had callers but none were new (all cycles), count as truncated
            if !has_new_path && path.len() > 1 {
                truncated_paths += 1;
            }
        }

        // Reverse each path so it reads top-down (entry point → ... → target)
        // and convert to TraceHop/TracePath
        let mut paths: Vec<TracePath> = complete_paths
            .into_iter()
            .map(|mut elems| {
                elems.reverse();
                let hops: Vec<TraceHop> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, (file_path, sym, line, sig))| {
                        let is_ep = if i == 0 {
                            // First hop (after reverse) is the entry point
                            self.lookup_file_data(file_path.as_ref())
                                .and_then(|fd| {
                                    let meta = fd.symbol_metadata.get(sym.as_ref())?;
                                    Some(is_entry_point(
                                        sym.as_ref(),
                                        &meta.kind,
                                        meta.exported,
                                        fd.lang,
                                    ))
                                })
                                .unwrap_or(false)
                        } else {
                            false
                        };
                        TraceHop {
                            symbol: sym.to_string(),
                            file: self.relative_path(file_path.as_ref()),
                            line: *line,
                            signature: sig.clone(),
                            is_entry_point: is_ep,
                        }
                    })
                    .collect();
                TracePath { hops }
            })
            .collect();

        // Sort paths for deterministic output (by entry point name, then path length)
        paths.sort_by(|a, b| {
            let a_entry = a.hops.first().map(|h| h.symbol.as_str()).unwrap_or("");
            let b_entry = b.hops.first().map(|h| h.symbol.as_str()).unwrap_or("");
            a_entry.cmp(b_entry).then(a.hops.len().cmp(&b.hops.len()))
        });

        // Count distinct entry points
        let mut entry_point_names: HashSet<String> = HashSet::new();
        for p in &paths {
            if let Some(first) = p.hops.first() {
                if first.is_entry_point {
                    entry_point_names.insert(first.symbol.clone());
                }
            }
        }

        let total_paths = paths.len();
        let entry_points_found = entry_point_names.len();

        Ok(TraceToResult {
            target_symbol: symbol.to_string(),
            target_file: target_rel,
            paths,
            total_paths,
            entry_points_found,
            max_depth_reached,
            truncated_paths,
        })
    }

    /// Impact analysis: enriched callers query.
    ///
    /// Returns all call sites affected by a change to the given symbol,
    /// annotated with each caller's signature, entry point status, the
    /// source line at the call site, and extracted parameter names.
    pub fn impact(
        &mut self,
        file: &Path,
        symbol: &str,
        depth: usize,
    ) -> Result<ImpactResult, AftError> {
        let canon = self.canonicalize(file)?;

        // Ensure file is built
        self.build_file(&canon)?;

        // Build the reverse index if not cached
        if self.reverse_index.is_none() {
            self.build_reverse_index();
        }

        let effective_depth = if depth == 0 { 1 } else { depth };

        // Get the target symbol's own metadata
        let (target_signature, target_parameters, target_lang) = {
            let file_data = match self.data.get(&canon) {
                Some(d) => d,
                None => {
                    return Err(AftError::InvalidRequest {
                        message: "file data missing after build".to_string(),
                    })
                }
            };
            let meta = file_data.symbol_metadata.get(symbol);
            let sig = meta.and_then(|m| m.signature.clone());
            let lang = file_data.lang;
            let params = sig
                .as_deref()
                .map(|s| extract_parameters(s, lang))
                .unwrap_or_default();
            (sig, params, lang)
        };

        // Collect all caller sites (transitive)
        let mut visited = HashSet::new();
        let mut all_sites: Vec<CallerSite> = Vec::new();
        self.collect_callers_recursive(
            &canon,
            symbol,
            effective_depth,
            0,
            &mut visited,
            &mut all_sites,
        );

        // Deduplicate sites by (file, symbol, line)
        let mut seen: HashSet<(PathBuf, String, u32)> = HashSet::new();
        all_sites.retain(|site| {
            seen.insert((
                site.caller_file.clone(),
                site.caller_symbol.clone(),
                site.line,
            ))
        });

        // Enrich each caller site
        let mut callers = Vec::new();
        let mut affected_file_set = HashSet::new();

        for site in &all_sites {
            // Build the caller's file to get metadata
            if let Err(e) = self.build_file(site.caller_file.as_path()) {
                log::debug!(
                    "callgraph: skipping caller file {}: {}",
                    site.caller_file.display(),
                    e
                );
            }

            let (sig, is_ep, params, _lang) = {
                if let Some(fd) = self.lookup_file_data(site.caller_file.as_path()) {
                    let meta = fd.symbol_metadata.get(&site.caller_symbol);
                    let sig = meta.and_then(|m| m.signature.clone());
                    let kind = meta.map(|m| m.kind.clone()).unwrap_or(SymbolKind::Function);
                    let exported = meta.map(|m| m.exported).unwrap_or(false);
                    let is_ep = is_entry_point(&site.caller_symbol, &kind, exported, fd.lang);
                    let lang = fd.lang;
                    let params = sig
                        .as_deref()
                        .map(|s| extract_parameters(s, lang))
                        .unwrap_or_default();
                    (sig, is_ep, params, lang)
                } else {
                    (None, false, Vec::new(), target_lang)
                }
            };

            // Read the source line at the call site
            let call_expression = self.read_source_line(site.caller_file.as_path(), site.line);

            let rel_file = self.relative_path(site.caller_file.as_path());
            affected_file_set.insert(rel_file.clone());

            callers.push(ImpactCaller {
                caller_symbol: site.caller_symbol.clone(),
                caller_file: rel_file,
                line: site.line,
                signature: sig,
                is_entry_point: is_ep,
                call_expression,
                parameters: params,
            });
        }

        // Sort callers by file then line for deterministic output
        callers.sort_by(|a, b| a.caller_file.cmp(&b.caller_file).then(a.line.cmp(&b.line)));

        let total_affected = callers.len();
        let affected_files = affected_file_set.len();

        Ok(ImpactResult {
            symbol: symbol.to_string(),
            file: self.relative_path(&canon),
            signature: target_signature,
            parameters: target_parameters,
            total_affected,
            affected_files,
            callers,
        })
    }

    /// Trace how an expression flows through variable assignments within a
    /// function body and across function boundaries via argument-to-parameter
    /// matching.
    ///
    /// Algorithm:
    /// 1. Parse the function body, find the expression text.
    /// 2. Walk AST for assignments that reference the tracked name.
    /// 3. When the tracked name appears as a call argument, resolve the callee,
    ///    match argument position to parameter name, recurse.
    /// 4. Destructuring, spread, and unresolved calls produce approximate hops.
    pub fn trace_data(
        &mut self,
        file: &Path,
        symbol: &str,
        expression: &str,
        max_depth: usize,
    ) -> Result<TraceDataResult, AftError> {
        let canon = self.canonicalize(file)?;
        let rel_file = self.relative_path(&canon);

        // Ensure file data is built
        self.build_file(&canon)?;

        // Verify symbol exists
        {
            let fd = match self.data.get(&canon) {
                Some(d) => d,
                None => {
                    return Err(AftError::InvalidRequest {
                        message: "file data missing after build".to_string(),
                    })
                }
            };
            let has_symbol = fd.calls_by_symbol.contains_key(symbol)
                || fd.exported_symbols.iter().any(|name| name == symbol)
                || fd.symbol_metadata.contains_key(symbol);
            if !has_symbol {
                return Err(AftError::InvalidRequest {
                    message: format!(
                        "trace_data: symbol '{}' not found in {}",
                        symbol,
                        file.display()
                    ),
                });
            }
        }

        let mut hops = Vec::new();
        let mut depth_limited = false;

        self.trace_data_inner(
            &canon,
            symbol,
            expression,
            max_depth,
            0,
            &mut hops,
            &mut depth_limited,
            &mut HashSet::new(),
        );

        Ok(TraceDataResult {
            expression: expression.to_string(),
            origin_file: rel_file,
            origin_symbol: symbol.to_string(),
            hops,
            depth_limited,
        })
    }

    /// Inner recursive data flow tracking.
    fn trace_data_inner(
        &mut self,
        file: &Path,
        symbol: &str,
        tracking_name: &str,
        max_depth: usize,
        current_depth: usize,
        hops: &mut Vec<DataFlowHop>,
        depth_limited: &mut bool,
        visited: &mut HashSet<(PathBuf, String, String)>,
    ) {
        let visit_key = (
            file.to_path_buf(),
            symbol.to_string(),
            tracking_name.to_string(),
        );
        if visited.contains(&visit_key) {
            return; // cycle
        }
        visited.insert(visit_key);

        // Read and parse the file
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(_) => return,
        };

        let lang = match detect_language(file) {
            Some(l) => l,
            None => return,
        };

        let grammar = grammar_for(lang);
        let mut parser = Parser::new();
        if parser.set_language(&grammar).is_err() {
            return;
        }
        let tree = match parser.parse(&source, None) {
            Some(t) => t,
            None => return,
        };

        // Find the symbol's AST node range
        let symbols = list_symbols_from_tree(&source, &tree, lang, file);
        let sym_info = match symbols.iter().find(|s| s.name == symbol) {
            Some(s) => s,
            None => return,
        };

        let body_start = line_col_to_byte(&source, sym_info.start_line, sym_info.start_col);
        let body_end = line_col_to_byte(&source, sym_info.end_line, sym_info.end_col);

        let root = tree.root_node();

        // Find the symbol's body node (the function/method definition node)
        let body_node = match find_node_covering_range(root, body_start, body_end) {
            Some(n) => n,
            None => return,
        };

        // Track names through the body
        let mut tracked_names: Vec<String> = vec![tracking_name.to_string()];
        let rel_file = self.relative_path(file);

        // Walk the body looking for assignments and calls
        self.walk_for_data_flow(
            body_node,
            &source,
            &mut tracked_names,
            file,
            symbol,
            &rel_file,
            lang,
            max_depth,
            current_depth,
            hops,
            depth_limited,
            visited,
        );
    }

    /// Walk an AST subtree looking for assignments and call expressions that
    /// reference tracked names.
    #[allow(clippy::too_many_arguments)]
    fn walk_for_data_flow(
        &mut self,
        node: tree_sitter::Node,
        source: &str,
        tracked_names: &mut Vec<String>,
        file: &Path,
        symbol: &str,
        rel_file: &str,
        lang: LangId,
        max_depth: usize,
        current_depth: usize,
        hops: &mut Vec<DataFlowHop>,
        depth_limited: &mut bool,
        visited: &mut HashSet<(PathBuf, String, String)>,
    ) {
        let kind = node.kind();

        // Check for variable declarations / assignments
        let is_var_decl = matches!(
            kind,
            "variable_declarator"
                | "assignment_expression"
                | "augmented_assignment_expression"
                | "assignment"
                | "let_declaration"
                | "short_var_declaration"
        );

        if is_var_decl {
            if let Some((new_name, init_text, line, is_approx)) =
                self.extract_assignment_info(node, source, lang, tracked_names)
            {
                // The RHS references a tracked name — add assignment hop
                if !is_approx {
                    hops.push(DataFlowHop {
                        file: rel_file.to_string(),
                        symbol: symbol.to_string(),
                        variable: new_name.clone(),
                        line,
                        flow_type: "assignment".to_string(),
                        approximate: false,
                    });
                    tracked_names.push(new_name);
                } else {
                    // Destructuring or pattern — approximate
                    hops.push(DataFlowHop {
                        file: rel_file.to_string(),
                        symbol: symbol.to_string(),
                        variable: init_text,
                        line,
                        flow_type: "assignment".to_string(),
                        approximate: true,
                    });
                    // Don't track further through this branch
                    return;
                }
            }
        }

        // Check for call expressions where tracked name is an argument
        if kind == "call_expression" || kind == "call" || kind == "macro_invocation" {
            self.check_call_for_data_flow(
                node,
                source,
                tracked_names,
                file,
                symbol,
                rel_file,
                lang,
                max_depth,
                current_depth,
                hops,
                depth_limited,
                visited,
            );
        }

        // Recurse into children
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                // Don't re-process the current node type in recursion
                self.walk_for_data_flow(
                    child,
                    source,
                    tracked_names,
                    file,
                    symbol,
                    rel_file,
                    lang,
                    max_depth,
                    current_depth,
                    hops,
                    depth_limited,
                    visited,
                );
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }

    /// Check if an assignment/declaration node assigns from a tracked name.
    /// Returns (new_name, init_text, line, is_approximate).
    fn extract_assignment_info(
        &self,
        node: tree_sitter::Node,
        source: &str,
        _lang: LangId,
        tracked_names: &[String],
    ) -> Option<(String, String, u32, bool)> {
        let kind = node.kind();
        let line = node.start_position().row as u32 + 1;

        match kind {
            "variable_declarator" => {
                // TS/JS: const x = <expr>
                let name_node = node.child_by_field_name("name")?;
                let value_node = node.child_by_field_name("value")?;
                let name_text = node_text(name_node, source);
                let value_text = node_text(value_node, source);

                // Check if name is a destructuring pattern
                if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
                    // Check if value references a tracked name
                    if tracked_names.iter().any(|t| value_text.contains(t)) {
                        return Some((name_text.clone(), name_text, line, true));
                    }
                    return None;
                }

                // Check if value references any tracked name
                if tracked_names.iter().any(|t| {
                    value_text == *t
                        || value_text.starts_with(&format!("{}.", t))
                        || value_text.starts_with(&format!("{}[", t))
                }) {
                    return Some((name_text, value_text, line, false));
                }
                None
            }
            "assignment_expression" | "augmented_assignment_expression" => {
                // TS/JS: x = <expr>
                let left = node.child_by_field_name("left")?;
                let right = node.child_by_field_name("right")?;
                let left_text = node_text(left, source);
                let right_text = node_text(right, source);

                if tracked_names.iter().any(|t| right_text == *t) {
                    return Some((left_text, right_text, line, false));
                }
                None
            }
            "assignment" => {
                // Python: x = <expr>
                let left = node.child_by_field_name("left")?;
                let right = node.child_by_field_name("right")?;
                let left_text = node_text(left, source);
                let right_text = node_text(right, source);

                if tracked_names.iter().any(|t| right_text == *t) {
                    return Some((left_text, right_text, line, false));
                }
                None
            }
            "let_declaration" | "short_var_declaration" => {
                // Rust / Go
                let left = node
                    .child_by_field_name("pattern")
                    .or_else(|| node.child_by_field_name("left"))?;
                let right = node
                    .child_by_field_name("value")
                    .or_else(|| node.child_by_field_name("right"))?;
                let left_text = node_text(left, source);
                let right_text = node_text(right, source);

                if tracked_names.iter().any(|t| right_text == *t) {
                    return Some((left_text, right_text, line, false));
                }
                None
            }
            _ => None,
        }
    }

    /// Check if a call expression uses a tracked name as an argument, and if so,
    /// resolve the callee and recurse into its body tracking the parameter name.
    #[allow(clippy::too_many_arguments)]
    fn check_call_for_data_flow(
        &mut self,
        node: tree_sitter::Node,
        source: &str,
        tracked_names: &[String],
        file: &Path,
        _symbol: &str,
        rel_file: &str,
        _lang: LangId,
        max_depth: usize,
        current_depth: usize,
        hops: &mut Vec<DataFlowHop>,
        depth_limited: &mut bool,
        visited: &mut HashSet<(PathBuf, String, String)>,
    ) {
        // Find the arguments node
        let args_node = find_child_by_kind(node, "arguments")
            .or_else(|| find_child_by_kind(node, "argument_list"));

        let args_node = match args_node {
            Some(n) => n,
            None => return,
        };

        // Collect argument texts and find which position a tracked name appears at
        let mut arg_positions: Vec<(usize, String)> = Vec::new(); // (position, tracked_name)
        let mut arg_idx = 0;

        let mut cursor = args_node.walk();
        if cursor.goto_first_child() {
            loop {
                let child = cursor.node();
                let child_kind = child.kind();

                // Skip punctuation (parentheses, commas)
                if child_kind == "(" || child_kind == ")" || child_kind == "," {
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                    continue;
                }

                let arg_text = node_text(child, source);

                // Check for spread element — approximate
                if child_kind == "spread_element" || child_kind == "dictionary_splat" {
                    if tracked_names.iter().any(|t| arg_text.contains(t)) {
                        hops.push(DataFlowHop {
                            file: rel_file.to_string(),
                            symbol: _symbol.to_string(),
                            variable: arg_text,
                            line: child.start_position().row as u32 + 1,
                            flow_type: "parameter".to_string(),
                            approximate: true,
                        });
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                    arg_idx += 1;
                    continue;
                }

                if tracked_names.iter().any(|t| arg_text == *t) {
                    arg_positions.push((arg_idx, arg_text));
                }

                arg_idx += 1;
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }

        if arg_positions.is_empty() {
            return;
        }

        // Resolve the callee
        let (full_callee, short_callee) = extract_callee_names(node, source);
        let full_callee = match full_callee {
            Some(f) => f,
            None => return,
        };
        let short_callee = match short_callee {
            Some(s) => s,
            None => return,
        };

        // Try to resolve cross-file edge
        let import_block = {
            match self.data.get(file) {
                Some(fd) => fd.import_block.clone(),
                None => return,
            }
        };

        let edge = self.resolve_cross_file_edge(&full_callee, &short_callee, file, &import_block);

        match edge {
            EdgeResolution::Resolved {
                file: target_file,
                symbol: target_symbol,
            } => {
                if current_depth + 1 > max_depth {
                    *depth_limited = true;
                    return;
                }

                // Build target file to get parameter info
                if let Err(e) = self.build_file(&target_file) {
                    log::debug!(
                        "callgraph: skipping target file {}: {}",
                        target_file.display(),
                        e
                    );
                }
                let (params, _target_lang) = {
                    match self.data.get(&target_file) {
                        Some(fd) => {
                            let meta = fd.symbol_metadata.get(&target_symbol);
                            let sig = meta.and_then(|m| m.signature.clone());
                            let params = sig
                                .as_deref()
                                .map(|s| extract_parameters(s, fd.lang))
                                .unwrap_or_default();
                            (params, fd.lang)
                        }
                        None => return,
                    }
                };

                let target_rel = self.relative_path(&target_file);

                for (pos, _tracked) in &arg_positions {
                    if let Some(param_name) = params.get(*pos) {
                        // Add parameter hop
                        hops.push(DataFlowHop {
                            file: target_rel.clone(),
                            symbol: target_symbol.clone(),
                            variable: param_name.clone(),
                            line: get_symbol_meta(&target_file, &target_symbol).0,
                            flow_type: "parameter".to_string(),
                            approximate: false,
                        });

                        // Recurse into callee's body tracking the parameter name
                        self.trace_data_inner(
                            &target_file.clone(),
                            &target_symbol.clone(),
                            param_name,
                            max_depth,
                            current_depth + 1,
                            hops,
                            depth_limited,
                            visited,
                        );
                    }
                }
            }
            EdgeResolution::Unresolved { callee_name } => {
                // Check if it's a same-file call
                let has_local = self
                    .data
                    .get(file)
                    .map(|fd| {
                        fd.calls_by_symbol.contains_key(&callee_name)
                            || fd.symbol_metadata.contains_key(&callee_name)
                    })
                    .unwrap_or(false);

                if has_local {
                    // Same-file call — get param info
                    let (params, _target_lang) = {
                        let Some(fd) = self.data.get(file) else {
                            return;
                        };
                        let meta = fd.symbol_metadata.get(&callee_name);
                        let sig = meta.and_then(|m| m.signature.clone());
                        let params = sig
                            .as_deref()
                            .map(|s| extract_parameters(s, fd.lang))
                            .unwrap_or_default();
                        (params, fd.lang)
                    };

                    let file_rel = self.relative_path(file);

                    for (pos, _tracked) in &arg_positions {
                        if let Some(param_name) = params.get(*pos) {
                            hops.push(DataFlowHop {
                                file: file_rel.clone(),
                                symbol: callee_name.clone(),
                                variable: param_name.clone(),
                                line: get_symbol_meta(file, &callee_name).0,
                                flow_type: "parameter".to_string(),
                                approximate: false,
                            });

                            // Recurse into same-file function
                            self.trace_data_inner(
                                file,
                                &callee_name.clone(),
                                param_name,
                                max_depth,
                                current_depth + 1,
                                hops,
                                depth_limited,
                                visited,
                            );
                        }
                    }
                } else {
                    // Truly unresolved — approximate hop
                    for (_pos, tracked) in &arg_positions {
                        hops.push(DataFlowHop {
                            file: self.relative_path(file),
                            symbol: callee_name.clone(),
                            variable: tracked.clone(),
                            line: node.start_position().row as u32 + 1,
                            flow_type: "parameter".to_string(),
                            approximate: true,
                        });
                    }
                }
            }
        }
    }

    /// Read a single source line (1-based) from a file, trimmed.
    fn read_source_line(&self, path: &Path, line: u32) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        content
            .lines()
            .nth(line.saturating_sub(1) as usize)
            .map(|l| l.trim().to_string())
    }

    /// Recursively collect callers up to the given depth.
    fn collect_callers_recursive(
        &self,
        file: &Path,
        symbol: &str,
        max_depth: usize,
        current_depth: usize,
        visited: &mut HashSet<(PathBuf, SharedStr)>,
        result: &mut Vec<CallerSite>,
    ) {
        if current_depth >= max_depth {
            return;
        }

        // Canonicalize for consistent reverse index lookup
        let canon = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
        let key_symbol: SharedStr = Arc::from(symbol);
        if !visited.insert((canon.clone(), Arc::clone(&key_symbol))) {
            return; // cycle detection
        }

        if let Some(sites) = self.reverse_sites(&canon, key_symbol.as_ref()) {
            for site in sites {
                result.push(CallerSite {
                    caller_file: site.caller_file.as_ref().clone(),
                    caller_symbol: site.caller_symbol.to_string(),
                    line: site.line,
                    col: site.col,
                    resolved: site.resolved,
                });
                // Recurse: find callers of the caller
                if current_depth + 1 < max_depth {
                    self.collect_callers_recursive(
                        site.caller_file.as_ref(),
                        site.caller_symbol.as_ref(),
                        max_depth,
                        current_depth + 1,
                        visited,
                        result,
                    );
                }
            }
        }
    }

    /// Invalidate a file: remove its cached data and clear the reverse index.
    ///
    /// Called by the file watcher when a file changes on disk. The reverse
    /// index is rebuilt lazily on the next `callers_of` call.
    pub fn invalidate_file(&mut self, path: &Path) {
        // Remove from data cache (try both as-is and canonicalized)
        self.data.remove(path);
        if let Ok(canon) = self.canonicalize(path) {
            self.data.remove(&canon);
        }
        // Clear the reverse index — it's stale
        self.reverse_index = None;
        // Clear project_files cache for create/remove events
        self.project_files = None;
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

    /// Look up cached file data, trying both the given path and its
    /// canonicalized form. Needed because `build_reverse_index` may store
    /// data under raw walker paths while CallerSite uses canonical paths.
    fn lookup_file_data(&self, path: &Path) -> Option<&FileCallData> {
        if let Some(fd) = self.data.get(path) {
            return Some(fd);
        }
        // Try canonical
        let canon = std::fs::canonicalize(path).ok()?;
        self.data.get(&canon).or_else(|| {
            // Try non-canonical forms stored by the walker
            self.data.iter().find_map(|(k, v)| {
                if std::fs::canonicalize(k).ok().as_ref() == Some(&canon) {
                    Some(v)
                } else {
                    None
                }
            })
        })
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
    parser
        .set_language(&grammar)
        .map_err(|e| AftError::ParseError {
            message: format!("grammar init failed for {:?}: {}", lang, e),
        })?;

    let tree = parser
        .parse(&source, None)
        .ok_or_else(|| AftError::ParseError {
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

    // Build per-symbol metadata for entry point detection
    let symbol_metadata: HashMap<String, SymbolMeta> = symbols
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                SymbolMeta {
                    kind: s.kind.clone(),
                    exported: s.exported,
                    signature: s.signature.clone(),
                },
            )
        })
        .collect();

    Ok(FileCallData {
        calls_by_symbol,
        exported_symbols,
        symbol_metadata,
        import_block,
        lang,
    })
}

/// Minimal symbol info needed for call graph construction.
#[derive(Debug)]
#[allow(dead_code)]
struct SymbolInfo {
    name: String,
    kind: SymbolKind,
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
                kind: s.kind,
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

/// Get symbol metadata (line, signature) from a file.
fn get_symbol_meta(path: &Path, symbol_name: &str) -> (u32, Option<String>) {
    let provider = crate::parser::TreeSitterProvider::new();
    match provider.list_symbols(path) {
        Ok(symbols) => {
            for s in &symbols {
                if s.name == symbol_name {
                    return (s.range.start_line + 1, s.signature.clone());
                }
            }
            (1, None)
        }
        Err(_) => (1, None),
    }
}

// ---------------------------------------------------------------------------
// Data flow tracking helpers
// ---------------------------------------------------------------------------

/// Get the text of a tree-sitter node from the source.
fn node_text(node: tree_sitter::Node, source: &str) -> String {
    source[node.start_byte()..node.end_byte()].to_string()
}

/// Find the smallest node that fully covers a byte range.
fn find_node_covering_range(
    root: tree_sitter::Node,
    start: usize,
    end: usize,
) -> Option<tree_sitter::Node> {
    let mut best = None;
    let mut cursor = root.walk();

    fn walk_covering<'a>(
        cursor: &mut tree_sitter::TreeCursor<'a>,
        start: usize,
        end: usize,
        best: &mut Option<tree_sitter::Node<'a>>,
    ) {
        let node = cursor.node();
        if node.start_byte() <= start && node.end_byte() >= end {
            *best = Some(node);
            if cursor.goto_first_child() {
                loop {
                    walk_covering(cursor, start, end, best);
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
                cursor.goto_parent();
            }
        }
    }

    walk_covering(&mut cursor, start, end, &mut best);
    best
}

/// Find a direct child node by kind name.
fn find_child_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if cursor.node().kind() == kind {
                return Some(cursor.node());
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Extract full and short callee names from a call_expression node.
fn extract_callee_names(node: tree_sitter::Node, source: &str) -> (Option<String>, Option<String>) {
    // The "function" field holds the callee
    let callee = match node.child_by_field_name("function") {
        Some(c) => c,
        None => return (None, None),
    };

    let full = node_text(callee, source);
    let short = if full.contains('.') {
        full.rsplit('.').next().unwrap_or(&full).to_string()
    } else {
        full.clone()
    };

    (Some(full), Some(short))
}

// ---------------------------------------------------------------------------
// Module path resolution
// ---------------------------------------------------------------------------

/// Resolve a module path (e.g. './utils') relative to a directory.
///
/// Tries common file extensions for TypeScript/JavaScript projects.
pub(crate) fn resolve_module_path(from_dir: &Path, module_path: &str) -> Option<PathBuf> {
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

        let edge =
            graph.resolve_cross_file_edge("unknownFunc", "unknownFunc", &main_path, &import_block);
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
            file_names.contains(&"readme.md".to_string()),
            "Markdown is now a supported source language"
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

    // --- Reverse callers ---

    #[test]
    fn callgraph_callers_of_direct() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // helpers.ts:double is called by utils.ts:helper
        let result = graph
            .callers_of(&dir.path().join("helpers.ts"), "double", 1)
            .unwrap();

        assert_eq!(result.symbol, "double");
        assert!(result.total_callers > 0, "double should have callers");
        assert!(result.scanned_files > 0, "should have scanned files");

        // Find the caller from utils.ts
        let utils_group = result.callers.iter().find(|g| g.file.contains("utils.ts"));
        assert!(
            utils_group.is_some(),
            "double should be called from utils.ts, groups: {:?}",
            result.callers.iter().map(|g| &g.file).collect::<Vec<_>>()
        );

        let group = utils_group.unwrap();
        let helper_caller = group.callers.iter().find(|c| c.symbol == "helper");
        assert!(
            helper_caller.is_some(),
            "double should be called by helper, callers: {:?}",
            group.callers.iter().map(|c| &c.symbol).collect::<Vec<_>>()
        );
    }

    #[test]
    fn callgraph_callers_of_no_callers() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // main.ts:main is the entry point — nothing calls it
        let result = graph
            .callers_of(&dir.path().join("main.ts"), "main", 1)
            .unwrap();

        assert_eq!(result.symbol, "main");
        assert_eq!(result.total_callers, 0, "main should have no callers");
        assert!(result.callers.is_empty());
    }

    #[test]
    fn callgraph_callers_recursive_depth() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // helpers.ts:double is called by utils.ts:helper
        // utils.ts:helper is called by main.ts:main
        // With depth=2, we should see both direct and transitive callers
        let result = graph
            .callers_of(&dir.path().join("helpers.ts"), "double", 2)
            .unwrap();

        assert!(
            result.total_callers >= 2,
            "with depth 2, double should have >= 2 callers (direct + transitive), got {}",
            result.total_callers
        );

        // Should include caller from main.ts (transitive: main → helper → double)
        let main_group = result.callers.iter().find(|g| g.file.contains("main.ts"));
        assert!(
            main_group.is_some(),
            "recursive callers should include main.ts, groups: {:?}",
            result.callers.iter().map(|g| &g.file).collect::<Vec<_>>()
        );
    }

    #[test]
    fn callgraph_invalidate_file_clears_reverse_index() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // Build callers to populate the reverse index
        let _ = graph
            .callers_of(&dir.path().join("helpers.ts"), "double", 1)
            .unwrap();
        assert!(
            graph.reverse_index.is_some(),
            "reverse index should be built"
        );

        // Invalidate a file
        graph.invalidate_file(&dir.path().join("utils.ts"));

        // Reverse index should be cleared
        assert!(
            graph.reverse_index.is_none(),
            "invalidate_file should clear reverse index"
        );
        // Data cache for the file should be cleared
        let canon = std::fs::canonicalize(dir.path().join("utils.ts")).unwrap();
        assert!(
            !graph.data.contains_key(&canon),
            "invalidate_file should remove file from data cache"
        );
        // Project files should be cleared
        assert!(
            graph.project_files.is_none(),
            "invalidate_file should clear project_files"
        );
    }

    // --- is_entry_point ---

    #[test]
    fn is_entry_point_exported_function() {
        assert!(is_entry_point(
            "handleRequest",
            &SymbolKind::Function,
            true,
            LangId::TypeScript
        ));
    }

    #[test]
    fn is_entry_point_exported_method_is_not_entry() {
        // Methods are class members, not standalone entry points
        assert!(!is_entry_point(
            "handleRequest",
            &SymbolKind::Method,
            true,
            LangId::TypeScript
        ));
    }

    #[test]
    fn is_entry_point_main_init_patterns() {
        for name in &["main", "Main", "MAIN", "init", "setup", "bootstrap", "run"] {
            assert!(
                is_entry_point(name, &SymbolKind::Function, false, LangId::TypeScript),
                "{} should be an entry point",
                name
            );
        }
    }

    #[test]
    fn is_entry_point_test_patterns_ts() {
        assert!(is_entry_point(
            "describe",
            &SymbolKind::Function,
            false,
            LangId::TypeScript
        ));
        assert!(is_entry_point(
            "it",
            &SymbolKind::Function,
            false,
            LangId::TypeScript
        ));
        assert!(is_entry_point(
            "test",
            &SymbolKind::Function,
            false,
            LangId::TypeScript
        ));
        assert!(is_entry_point(
            "testValidation",
            &SymbolKind::Function,
            false,
            LangId::TypeScript
        ));
        assert!(is_entry_point(
            "specHelper",
            &SymbolKind::Function,
            false,
            LangId::TypeScript
        ));
    }

    #[test]
    fn is_entry_point_test_patterns_python() {
        assert!(is_entry_point(
            "test_login",
            &SymbolKind::Function,
            false,
            LangId::Python
        ));
        assert!(is_entry_point(
            "setUp",
            &SymbolKind::Function,
            false,
            LangId::Python
        ));
        assert!(is_entry_point(
            "tearDown",
            &SymbolKind::Function,
            false,
            LangId::Python
        ));
        // "testSomething" should NOT match Python (needs test_ prefix)
        assert!(!is_entry_point(
            "testSomething",
            &SymbolKind::Function,
            false,
            LangId::Python
        ));
    }

    #[test]
    fn is_entry_point_test_patterns_rust() {
        assert!(is_entry_point(
            "test_parse",
            &SymbolKind::Function,
            false,
            LangId::Rust
        ));
        assert!(!is_entry_point(
            "TestSomething",
            &SymbolKind::Function,
            false,
            LangId::Rust
        ));
    }

    #[test]
    fn is_entry_point_test_patterns_go() {
        assert!(is_entry_point(
            "TestParsing",
            &SymbolKind::Function,
            false,
            LangId::Go
        ));
        // lowercase test should NOT match Go (needs uppercase Test prefix)
        assert!(!is_entry_point(
            "testParsing",
            &SymbolKind::Function,
            false,
            LangId::Go
        ));
    }

    #[test]
    fn is_entry_point_non_exported_non_main_is_not_entry() {
        assert!(!is_entry_point(
            "helperUtil",
            &SymbolKind::Function,
            false,
            LangId::TypeScript
        ));
    }

    // --- symbol_metadata ---

    #[test]
    fn callgraph_symbol_metadata_populated() {
        let dir = setup_ts_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let file_data = graph.build_file(&dir.path().join("utils.ts")).unwrap();
        assert!(
            file_data.symbol_metadata.contains_key("helper"),
            "symbol_metadata should contain helper"
        );
        let meta = &file_data.symbol_metadata["helper"];
        assert_eq!(meta.kind, SymbolKind::Function);
        assert!(meta.exported, "helper should be exported");
    }

    // --- trace_to ---

    /// Setup a multi-path project for trace_to tests.
    ///
    /// Structure:
    ///   main.ts: exported main() → processData (from utils)
    ///   service.ts: exported handleRequest() → processData (from utils)
    ///   utils.ts: exported processData() → validate (from helpers)
    ///   helpers.ts: exported validate() → checkFormat (local, not exported)
    ///   test_helpers.ts: testValidation() → validate (from helpers)
    ///
    /// checkFormat should have 3 paths:
    ///   main → processData → validate → checkFormat
    ///   handleRequest → processData → validate → checkFormat
    ///   testValidation → validate → checkFormat
    fn setup_trace_project() -> TempDir {
        let dir = TempDir::new().unwrap();

        fs::write(
            dir.path().join("main.ts"),
            r#"import { processData } from './utils';

export function main() {
    const result = processData("hello");
    return result;
}
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("service.ts"),
            r#"import { processData } from './utils';

export function handleRequest(input: string): string {
    return processData(input);
}
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("utils.ts"),
            r#"import { validate } from './helpers';

export function processData(input: string): string {
    const valid = validate(input);
    if (!valid) {
        throw new Error("invalid input");
    }
    return input.toUpperCase();
}
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("helpers.ts"),
            r#"export function validate(input: string): boolean {
    return checkFormat(input);
}

function checkFormat(input: string): boolean {
    return input.length > 0 && /^[a-zA-Z]+$/.test(input);
}
"#,
        )
        .unwrap();

        fs::write(
            dir.path().join("test_helpers.ts"),
            r#"import { validate } from './helpers';

function testValidation() {
    const result = validate("hello");
    console.log(result);
}
"#,
        )
        .unwrap();

        // git init so the walker works
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        dir
    }

    #[test]
    fn trace_to_multi_path() {
        let dir = setup_trace_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        let result = graph
            .trace_to(&dir.path().join("helpers.ts"), "checkFormat", 10)
            .unwrap();

        assert_eq!(result.target_symbol, "checkFormat");
        assert!(
            result.total_paths >= 2,
            "checkFormat should have at least 2 paths, got {} (paths: {:?})",
            result.total_paths,
            result
                .paths
                .iter()
                .map(|p| p.hops.iter().map(|h| h.symbol.as_str()).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );

        // Check that paths are top-down: entry point first, target last
        for path in &result.paths {
            assert!(
                path.hops.first().unwrap().is_entry_point,
                "First hop should be an entry point, got: {}",
                path.hops.first().unwrap().symbol
            );
            assert_eq!(
                path.hops.last().unwrap().symbol,
                "checkFormat",
                "Last hop should be checkFormat"
            );
        }

        // Verify entry_points_found > 0
        assert!(
            result.entry_points_found >= 2,
            "should find at least 2 entry points, got {}",
            result.entry_points_found
        );
    }

    #[test]
    fn trace_to_single_path() {
        let dir = setup_trace_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // validate is called from processData, testValidation
        // processData is called from main, handleRequest
        // So validate has paths: main→processData→validate, handleRequest→processData→validate, testValidation→validate
        let result = graph
            .trace_to(&dir.path().join("helpers.ts"), "validate", 10)
            .unwrap();

        assert_eq!(result.target_symbol, "validate");
        assert!(
            result.total_paths >= 2,
            "validate should have at least 2 paths, got {}",
            result.total_paths
        );
    }

    #[test]
    fn trace_to_cycle_detection() {
        let dir = setup_cycle_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // funcA ↔ funcB cycle — should terminate
        let result = graph
            .trace_to(&dir.path().join("a.ts"), "funcA", 10)
            .unwrap();

        // Should not hang — the fact we got here means cycle detection works
        assert_eq!(result.target_symbol, "funcA");
    }

    #[test]
    fn trace_to_depth_limit() {
        let dir = setup_trace_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // With max_depth=1, should not be able to reach entry points that are 3+ hops away
        let result = graph
            .trace_to(&dir.path().join("helpers.ts"), "checkFormat", 1)
            .unwrap();

        // testValidation→validate→checkFormat is 2 hops, which requires depth >= 2
        // main→processData→validate→checkFormat is 3 hops, which requires depth >= 3
        // With depth=1, most paths should be truncated
        assert_eq!(result.target_symbol, "checkFormat");

        // The shallow result should have fewer paths than the deep one
        let deep_result = graph
            .trace_to(&dir.path().join("helpers.ts"), "checkFormat", 10)
            .unwrap();

        assert!(
            result.total_paths <= deep_result.total_paths,
            "shallow trace should find <= paths compared to deep: {} vs {}",
            result.total_paths,
            deep_result.total_paths
        );
    }

    #[test]
    fn trace_to_entry_point_target() {
        let dir = setup_trace_project();
        let mut graph = CallGraph::new(dir.path().to_path_buf());

        // main is itself an entry point — should return a single trivial path
        let result = graph
            .trace_to(&dir.path().join("main.ts"), "main", 10)
            .unwrap();

        assert_eq!(result.target_symbol, "main");
        assert!(
            result.total_paths >= 1,
            "main should have at least 1 path (itself), got {}",
            result.total_paths
        );
        // Check the trivial path has just one hop
        let trivial = result.paths.iter().find(|p| p.hops.len() == 1);
        assert!(
            trivial.is_some(),
            "should have a trivial path with just the entry point itself"
        );
    }

    // --- extract_parameters ---

    #[test]
    fn extract_parameters_typescript() {
        let params = extract_parameters(
            "function processData(input: string, count: number): void",
            LangId::TypeScript,
        );
        assert_eq!(params, vec!["input", "count"]);
    }

    #[test]
    fn extract_parameters_typescript_optional() {
        let params = extract_parameters(
            "function fetch(url: string, options?: RequestInit): Promise<Response>",
            LangId::TypeScript,
        );
        assert_eq!(params, vec!["url", "options"]);
    }

    #[test]
    fn extract_parameters_typescript_defaults() {
        let params = extract_parameters(
            "function greet(name: string, greeting: string = \"hello\"): string",
            LangId::TypeScript,
        );
        assert_eq!(params, vec!["name", "greeting"]);
    }

    #[test]
    fn extract_parameters_typescript_rest() {
        let params = extract_parameters(
            "function sum(...numbers: number[]): number",
            LangId::TypeScript,
        );
        assert_eq!(params, vec!["numbers"]);
    }

    #[test]
    fn extract_parameters_python_self_skipped() {
        let params = extract_parameters(
            "def process(self, data: str, count: int) -> bool",
            LangId::Python,
        );
        assert_eq!(params, vec!["data", "count"]);
    }

    #[test]
    fn extract_parameters_python_no_self() {
        let params = extract_parameters("def validate(input: str) -> bool", LangId::Python);
        assert_eq!(params, vec!["input"]);
    }

    #[test]
    fn extract_parameters_python_star_args() {
        let params = extract_parameters("def func(*args, **kwargs)", LangId::Python);
        assert_eq!(params, vec!["args", "kwargs"]);
    }

    #[test]
    fn extract_parameters_rust_self_skipped() {
        let params = extract_parameters(
            "fn process(&self, data: &str, count: usize) -> bool",
            LangId::Rust,
        );
        assert_eq!(params, vec!["data", "count"]);
    }

    #[test]
    fn extract_parameters_rust_mut_self_skipped() {
        let params = extract_parameters("fn update(&mut self, value: i32)", LangId::Rust);
        assert_eq!(params, vec!["value"]);
    }

    #[test]
    fn extract_parameters_rust_no_self() {
        let params = extract_parameters("fn validate(input: &str) -> bool", LangId::Rust);
        assert_eq!(params, vec!["input"]);
    }

    #[test]
    fn extract_parameters_rust_mut_param() {
        let params = extract_parameters("fn process(mut buf: Vec<u8>, len: usize)", LangId::Rust);
        assert_eq!(params, vec!["buf", "len"]);
    }

    #[test]
    fn extract_parameters_go() {
        let params = extract_parameters(
            "func ProcessData(input string, count int) error",
            LangId::Go,
        );
        assert_eq!(params, vec!["input", "count"]);
    }

    #[test]
    fn extract_parameters_empty() {
        let params = extract_parameters("function noArgs(): void", LangId::TypeScript);
        assert!(
            params.is_empty(),
            "no-arg function should return empty params"
        );
    }

    #[test]
    fn extract_parameters_no_parens() {
        let params = extract_parameters("const x = 42", LangId::TypeScript);
        assert!(params.is_empty(), "no parens should return empty params");
    }

    #[test]
    fn extract_parameters_javascript() {
        let params = extract_parameters("function handleClick(event, target)", LangId::JavaScript);
        assert_eq!(params, vec!["event", "target"]);
    }
}
