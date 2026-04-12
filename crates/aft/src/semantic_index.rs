use crate::parser::FileParser;
use crate::symbols::{Symbol, SymbolKind};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const DEFAULT_DIMENSION: usize = 384;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_DIMENSION: usize = 1024;
const F32_BYTES: usize = std::mem::size_of::<f32>();
const HEADER_BYTES: usize = 9;
const ONNX_RUNTIME_INSTALL_HINT: &str =
    "ONNX Runtime not found. Install via: brew install onnxruntime (macOS) or apt install libonnxruntime (Linux).";

/// Pre-validate ONNX Runtime by attempting a raw dlopen before ort touches it.
/// This catches broken/incompatible .so files without risking a panic in the ort crate.
/// Also checks the runtime version via OrtGetApiBase if available.
pub fn pre_validate_onnx_runtime() -> Result<(), String> {
    let dylib_path = std::env::var("ORT_DYLIB_PATH").ok();

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        let default_name = "libonnxruntime.so";
        #[cfg(target_os = "macos")]
        let default_name = "libonnxruntime.dylib";

        let lib_name = dylib_path.as_deref().unwrap_or(default_name);

        unsafe {
            let c_name = std::ffi::CString::new(lib_name)
                .map_err(|e| format!("invalid library path: {}", e))?;
            let handle = libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW);
            if handle.is_null() {
                let err = libc::dlerror();
                let msg = if err.is_null() {
                    "unknown dlopen error".to_string()
                } else {
                    std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned()
                };
                return Err(format!(
                    "ONNX Runtime not found. dlopen('{}') failed: {}. \
                     Run `bunx @cortexkit/aft-opencode@latest doctor` to diagnose.",
                    lib_name, msg
                ));
            }

            // Try to detect the runtime version from the file path or soname.
            // libonnxruntime.so.1.19.0, libonnxruntime.1.24.4.dylib, etc.
            let detected_version = detect_ort_version_from_path(lib_name);

            libc::dlclose(handle);

            // Check version compatibility — we need 1.24.x
            if let Some(ref version) = detected_version {
                let parts: Vec<&str> = version.split('.').collect();
                if let (Some(major), Some(minor)) = (
                    parts.first().and_then(|s| s.parse::<u32>().ok()),
                    parts.get(1).and_then(|s| s.parse::<u32>().ok()),
                ) {
                    if major != 1 || minor < 20 {
                        return Err(format!(
                            "ONNX Runtime version mismatch: found v{} at '{}', but AFT requires v1.20+. \
                             Solutions:\n\
                             1. Remove the old library and restart (AFT auto-downloads the correct version):\n\
                             {}\n\
                             2. Or install ONNX Runtime 1.24: https://github.com/microsoft/onnxruntime/releases/tag/v1.24.0\n\
                             3. Run `bunx @cortexkit/aft-opencode@latest doctor` for full diagnostics.",
                            version,
                            lib_name,
                            suggest_removal_command(lib_name),
                        ));
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, skip pre-validation — let ort handle LoadLibrary
        let _ = dylib_path;
    }

    Ok(())
}

/// Try to extract the ORT version from the library filename or resolved symlink.
/// Examples: "libonnxruntime.so.1.19.0" → "1.19.0", "libonnxruntime.1.24.4.dylib" → "1.24.4"
fn detect_ort_version_from_path(lib_path: &str) -> Option<String> {
    let path = std::path::Path::new(lib_path);

    // Try the path as given, then follow symlinks
    for candidate in [Some(path.to_path_buf()), std::fs::canonicalize(path).ok()]
        .into_iter()
        .flatten()
    {
        if let Some(name) = candidate.file_name().and_then(|n| n.to_str()) {
            if let Some(version) = extract_version_from_filename(name) {
                return Some(version);
            }
        }
    }

    // Also check for versioned siblings in the same directory
    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with("libonnxruntime") {
                        if let Some(version) = extract_version_from_filename(name) {
                            return Some(version);
                        }
                    }
                }
            }
        }
    }

    None
}

/// Extract version from filenames like "libonnxruntime.so.1.19.0" or "libonnxruntime.1.24.4.dylib"
fn extract_version_from_filename(name: &str) -> Option<String> {
    // Match patterns: .so.X.Y.Z or .X.Y.Z.dylib or .X.Y.Z.so
    let re = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(name).map(|m| m.as_str().to_string())
}

fn suggest_removal_command(lib_path: &str) -> String {
    if lib_path.starts_with("/usr/local/lib")
        || lib_path == "libonnxruntime.so"
        || lib_path == "libonnxruntime.dylib"
    {
        #[cfg(target_os = "linux")]
        return "   sudo rm /usr/local/lib/libonnxruntime* && sudo ldconfig".to_string();
        #[cfg(target_os = "macos")]
        return "   sudo rm /usr/local/lib/libonnxruntime*".to_string();
        #[cfg(target_os = "windows")]
        return "   Delete the ONNX Runtime DLL from your PATH".to_string();
    }
    format!("   rm '{}'", lib_path)
}

pub fn initialize_text_embedding() -> Result<TextEmbedding, String> {
    // Pre-validate before ort can panic on a bad library
    pre_validate_onnx_runtime()?;

    TextEmbedding::try_new(InitOptions::new(EmbeddingModel::AllMiniLML6V2))
        .map_err(format_embedding_init_error)
}

pub fn is_onnx_runtime_unavailable(message: &str) -> bool {
    if message.trim_start().starts_with("ONNX Runtime not found.") {
        return true;
    }

    let message = message.to_ascii_lowercase();
    let mentions_onnx_runtime = ["onnx runtime", "onnxruntime", "libonnxruntime"]
        .iter()
        .any(|pattern| message.contains(pattern));
    let mentions_dynamic_load_failure = [
        "shared library",
        "dynamic library",
        "failed to load",
        "could not load",
        "unable to load",
        "dlopen",
        "loadlibrary",
        "no such file",
        "not found",
    ]
    .iter()
    .any(|pattern| message.contains(pattern));

    mentions_onnx_runtime && mentions_dynamic_load_failure
}

fn format_embedding_init_error(error: impl Display) -> String {
    let message = error.to_string();

    if is_onnx_runtime_unavailable(&message) {
        return format!("{ONNX_RUNTIME_INSTALL_HINT} Original error: {message}");
    }

    format!("failed to initialize semantic embedding model: {message}")
}

/// A chunk of code ready for embedding — derived from a Symbol with context enrichment
#[derive(Debug, Clone)]
pub struct SemanticChunk {
    /// Absolute file path
    pub file: PathBuf,
    /// Symbol name
    pub name: String,
    /// Symbol kind (function, class, struct, etc.)
    pub kind: SymbolKind,
    /// Line range (0-based internally, inclusive)
    pub start_line: u32,
    pub end_line: u32,
    /// Whether the symbol is exported
    pub exported: bool,
    /// The enriched text that gets embedded (scope + signature + body snippet)
    pub embed_text: String,
    /// Short code snippet for display in results
    pub snippet: String,
}

/// A stored embedding entry — chunk metadata + vector
struct EmbeddingEntry {
    chunk: SemanticChunk,
    vector: Vec<f32>,
}

/// The semantic index — stores embeddings for all symbols in a project
pub struct SemanticIndex {
    entries: Vec<EmbeddingEntry>,
    /// Track which files are indexed and their mtime for staleness detection
    file_mtimes: HashMap<PathBuf, SystemTime>,
    /// Embedding dimension (384 for MiniLM-L6-v2)
    dimension: usize,
}

/// Search result from a semantic query
#[derive(Debug)]
pub struct SemanticResult {
    pub file: PathBuf,
    pub name: String,
    pub kind: SymbolKind,
    pub start_line: u32,
    pub end_line: u32,
    pub exported: bool,
    pub snippet: String,
    pub score: f32,
}

impl SemanticIndex {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            file_mtimes: HashMap::new(),
            dimension: DEFAULT_DIMENSION, // MiniLM-L6-v2 default
        }
    }

    /// Number of embedded symbol entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Human-readable status label for the index.
    pub fn status_label(&self) -> &'static str {
        if self.entries.is_empty() {
            "empty"
        } else {
            "ready"
        }
    }

    /// Build the semantic index from a set of files using the provided embedding function.
    /// `embed_fn` takes a batch of texts and returns a batch of embedding vectors.
    pub fn build<F>(
        project_root: &Path,
        files: &[PathBuf],
        embed_fn: &mut F,
        max_batch_size: usize,
    ) -> Result<Self, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
    {
        let mut parser = FileParser::new();
        let mut chunks: Vec<SemanticChunk> = Vec::new();
        let mut file_mtimes: HashMap<PathBuf, SystemTime> = HashMap::new();

        // Extract chunks from all files
        for file in files {
            let mtime = std::fs::metadata(file)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            file_mtimes.insert(file.clone(), mtime);

            let source = match std::fs::read_to_string(file) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let symbols = match parser.extract_symbols(file) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let file_chunks = symbols_to_chunks(file, &symbols, &source, project_root);
            chunks.extend(file_chunks);
        }

        if chunks.is_empty() {
            return Ok(Self {
                entries: Vec::new(),
                file_mtimes,
                dimension: DEFAULT_DIMENSION,
            });
        }

        // Embed in batches
        let mut entries: Vec<EmbeddingEntry> = Vec::with_capacity(chunks.len());
        let batch_size = max_batch_size.max(1);
        for batch_start in (0..chunks.len()).step_by(batch_size) {
            let batch_end = (batch_start + batch_size).min(chunks.len());
            let batch_texts: Vec<String> = chunks[batch_start..batch_end]
                .iter()
                .map(|c| c.embed_text.clone())
                .collect();

            let vectors = embed_fn(batch_texts)?;

            for (i, vector) in vectors.into_iter().enumerate() {
                let chunk_idx = batch_start + i;
                entries.push(EmbeddingEntry {
                    chunk: chunks[chunk_idx].clone(),
                    vector,
                });
            }
        }

        let dimension = entries
            .first()
            .map(|e| e.vector.len())
            .unwrap_or(DEFAULT_DIMENSION);

        Ok(Self {
            entries,
            file_mtimes,
            dimension,
        })
    }

    /// Search the index with a query embedding, returning top-K results sorted by relevance
    pub fn search(&self, query_vector: &[f32], top_k: usize) -> Vec<SemanticResult> {
        if self.entries.is_empty() || query_vector.len() != self.dimension {
            return Vec::new();
        }

        let mut scored: Vec<(f32, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| (cosine_similarity(query_vector, &entry.vector), i))
            .collect();

        // Sort descending by score
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        scored
            .into_iter()
            .take(top_k)
            .filter(|(score, _)| *score > 0.0)
            .map(|(score, idx)| {
                let entry = &self.entries[idx];
                SemanticResult {
                    file: entry.chunk.file.clone(),
                    name: entry.chunk.name.clone(),
                    kind: entry.chunk.kind.clone(),
                    start_line: entry.chunk.start_line,
                    end_line: entry.chunk.end_line,
                    exported: entry.chunk.exported,
                    snippet: entry.chunk.snippet.clone(),
                    score,
                }
            })
            .collect()
    }

    /// Number of indexed entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if a file needs re-indexing based on mtime
    pub fn is_file_stale(&self, file: &Path) -> bool {
        match self.file_mtimes.get(file) {
            None => true,
            Some(stored_mtime) => {
                let current_mtime = std::fs::metadata(file)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                *stored_mtime != current_mtime
            }
        }
    }

    /// Remove entries for a specific file
    pub fn remove_file(&mut self, file: &Path) {
        self.invalidate_file(file);
    }

    pub fn invalidate_file(&mut self, file: &Path) {
        self.entries.retain(|e| e.chunk.file != file);
        self.file_mtimes.remove(file);
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Write the semantic index to disk using atomic temp+rename pattern
    pub fn write_to_disk(&self, storage_dir: &Path, project_key: &str) {
        // Don't persist empty indexes — they would be loaded on next startup
        // and prevent a fresh build that might find files.
        if self.entries.is_empty() {
            log::info!("[aft] skipping semantic index persistence (0 entries)");
            return;
        }
        let dir = storage_dir.join("semantic").join(project_key);
        if let Err(e) = fs::create_dir_all(&dir) {
            log::warn!("[aft] failed to create semantic cache dir: {}", e);
            return;
        }
        let data_path = dir.join("semantic.bin");
        let tmp_path = dir.join("semantic.bin.tmp");
        let bytes = self.to_bytes();
        if let Err(e) = fs::write(&tmp_path, &bytes) {
            log::warn!("[aft] failed to write semantic index: {}", e);
            let _ = fs::remove_file(&tmp_path);
            return;
        }
        if let Err(e) = fs::rename(&tmp_path, &data_path) {
            log::warn!("[aft] failed to rename semantic index: {}", e);
            let _ = fs::remove_file(&tmp_path);
            return;
        }
        log::info!(
            "[aft] semantic index persisted: {} entries, {:.1} KB",
            self.entries.len(),
            bytes.len() as f64 / 1024.0
        );
    }

    /// Read the semantic index from disk
    pub fn read_from_disk(storage_dir: &Path, project_key: &str) -> Option<Self> {
        let data_path = storage_dir
            .join("semantic")
            .join(project_key)
            .join("semantic.bin");
        let file_len = usize::try_from(fs::metadata(&data_path).ok()?.len()).ok()?;
        if file_len < HEADER_BYTES {
            log::warn!(
                "[aft] corrupt semantic index (too small: {} bytes), removing",
                file_len
            );
            let _ = fs::remove_file(&data_path);
            return None;
        }

        let mut header = [0u8; HEADER_BYTES];
        fs::File::open(&data_path)
            .ok()?
            .read_exact(&mut header)
            .ok()?;
        let version = header[0];
        let dimension = u32::from_le_bytes(header[1..5].try_into().ok()?) as usize;
        let entry_count = u32::from_le_bytes(header[5..9].try_into().ok()?) as usize;
        if version != 1 || dimension == 0 || dimension > MAX_DIMENSION || entry_count > MAX_ENTRIES
        {
            let _ = fs::remove_file(&data_path);
            return None;
        }
        let minimum_vector_bytes = entry_count
            .checked_mul(dimension)
            .and_then(|count| count.checked_mul(F32_BYTES))?;
        if minimum_vector_bytes > file_len.saturating_sub(HEADER_BYTES) {
            let _ = fs::remove_file(&data_path);
            return None;
        }

        let bytes = fs::read(&data_path).ok()?;
        match Self::from_bytes(&bytes) {
            Ok(index) => {
                if index.entries.is_empty() {
                    log::info!("[aft] cached semantic index is empty, will rebuild");
                    let _ = fs::remove_file(&data_path);
                    return None;
                }
                log::info!(
                    "[aft] loaded semantic index from disk: {} entries",
                    index.entries.len()
                );
                Some(index)
            }
            Err(e) => {
                log::warn!("[aft] corrupt semantic index, rebuilding: {}", e);
                let _ = fs::remove_file(&data_path);
                None
            }
        }
    }

    /// Serialize the index to bytes for disk persistence
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Header: version(1) + dimension(4) + entry_count(4)
        buf.push(1u8); // version
        buf.extend_from_slice(&(self.dimension as u32).to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());

        // File mtime table: count(4) + entries
        buf.extend_from_slice(&(self.file_mtimes.len() as u32).to_le_bytes());
        for (path, mtime) in &self.file_mtimes {
            let path_bytes = path.to_string_lossy().as_bytes().to_vec();
            buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(&path_bytes);
            let duration = mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default();
            buf.extend_from_slice(&duration.as_secs().to_le_bytes());
        }

        // Entries: each is metadata + vector
        for entry in &self.entries {
            let c = &entry.chunk;

            // File path
            let file_bytes = c.file.to_string_lossy().as_bytes().to_vec();
            buf.extend_from_slice(&(file_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(&file_bytes);

            // Name
            let name_bytes = c.name.as_bytes();
            buf.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(name_bytes);

            // Kind (1 byte)
            buf.push(symbol_kind_to_u8(&c.kind));

            // Lines + exported
            buf.extend_from_slice(&(c.start_line as u32).to_le_bytes());
            buf.extend_from_slice(&(c.end_line as u32).to_le_bytes());
            buf.push(c.exported as u8);

            // Snippet
            let snippet_bytes = c.snippet.as_bytes();
            buf.extend_from_slice(&(snippet_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(snippet_bytes);

            // Embed text
            let embed_bytes = c.embed_text.as_bytes();
            buf.extend_from_slice(&(embed_bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(embed_bytes);

            // Vector (f32 array)
            for &val in &entry.vector {
                buf.extend_from_slice(&val.to_le_bytes());
            }
        }

        buf
    }

    /// Deserialize the index from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let mut pos = 0;

        if data.len() < 9 {
            return Err("data too short".to_string());
        }

        let version = data[pos];
        pos += 1;
        if version != 1 {
            return Err(format!("unsupported version: {}", version));
        }

        let dimension = read_u32(data, &mut pos)? as usize;
        let entry_count = read_u32(data, &mut pos)? as usize;
        if dimension == 0 || dimension > MAX_DIMENSION {
            return Err(format!("invalid embedding dimension: {}", dimension));
        }
        if entry_count > MAX_ENTRIES {
            return Err(format!("too many semantic index entries: {}", entry_count));
        }

        // File mtimes
        let mtime_count = read_u32(data, &mut pos)? as usize;
        if mtime_count > MAX_ENTRIES {
            return Err(format!("too many semantic file mtimes: {}", mtime_count));
        }

        let vector_bytes = entry_count
            .checked_mul(dimension)
            .and_then(|count| count.checked_mul(F32_BYTES))
            .ok_or_else(|| "semantic vector allocation overflow".to_string())?;
        if vector_bytes > data.len().saturating_sub(pos) {
            return Err("semantic index vectors exceed available data".to_string());
        }

        let mut file_mtimes = HashMap::with_capacity(mtime_count);
        for _ in 0..mtime_count {
            let path = read_string(data, &mut pos)?;
            let secs = read_u64(data, &mut pos)?;
            let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            file_mtimes.insert(PathBuf::from(path), mtime);
        }

        // Entries
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let file = PathBuf::from(read_string(data, &mut pos)?);
            let name = read_string(data, &mut pos)?;

            if pos >= data.len() {
                return Err("unexpected end of data".to_string());
            }
            let kind = u8_to_symbol_kind(data[pos]);
            pos += 1;

            let start_line = read_u32(data, &mut pos)?;
            let end_line = read_u32(data, &mut pos)?;

            if pos >= data.len() {
                return Err("unexpected end of data".to_string());
            }
            let exported = data[pos] != 0;
            pos += 1;

            let snippet = read_string(data, &mut pos)?;
            let embed_text = read_string(data, &mut pos)?;

            // Vector
            let vec_bytes = dimension
                .checked_mul(F32_BYTES)
                .ok_or_else(|| "semantic vector allocation overflow".to_string())?;
            if pos + vec_bytes > data.len() {
                return Err("unexpected end of data reading vector".to_string());
            }
            let mut vector = Vec::with_capacity(dimension);
            for _ in 0..dimension {
                let bytes = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
                vector.push(f32::from_le_bytes(bytes));
                pos += 4;
            }

            entries.push(EmbeddingEntry {
                chunk: SemanticChunk {
                    file,
                    name,
                    kind,
                    start_line,
                    end_line,
                    exported,
                    embed_text,
                    snippet,
                },
                vector,
            });
        }

        Ok(Self {
            entries,
            file_mtimes,
            dimension,
        })
    }
}

/// Build enriched embedding text from a symbol with cAST-style context
fn build_embed_text(symbol: &Symbol, source: &str, file: &Path, project_root: &Path) -> String {
    let relative = file
        .strip_prefix(project_root)
        .unwrap_or(file)
        .to_string_lossy();

    let kind_label = match &symbol.kind {
        SymbolKind::Function => "function",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
        SymbolKind::Struct => "struct",
        SymbolKind::Interface => "interface",
        SymbolKind::Enum => "enum",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Variable => "variable",
        SymbolKind::Heading => "heading",
    };

    // Build: "file:relative/path kind:function name:validateAuth signature:fn validateAuth(token: &str) -> bool"
    let mut text = format!("file:{} kind:{} name:{}", relative, kind_label, symbol.name);

    if let Some(sig) = &symbol.signature {
        text.push_str(&format!(" signature:{}", sig));
    }

    // Add body snippet (first ~300 chars of symbol body)
    let lines: Vec<&str> = source.lines().collect();
    let start = (symbol.range.start_line.saturating_sub(1) as usize).min(lines.len()); // 1-based to 0-based
    let end = (symbol.range.end_line as usize).min(lines.len()); // 1-based inclusive
    if start < end {
        let body: String = lines[start..end]
            .iter()
            .take(15) // max 15 lines
            .copied()
            .collect::<Vec<&str>>()
            .join("\n");
        let snippet = if body.len() > 300 {
            format!("{}...", &body[..body.floor_char_boundary(300)])
        } else {
            body
        };
        text.push_str(&format!(" body:{}", snippet));
    }

    text
}

/// Build a display snippet from a symbol's source
fn build_snippet(symbol: &Symbol, source: &str) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let start = (symbol.range.start_line.saturating_sub(1) as usize).min(lines.len());
    let end = (symbol.range.end_line as usize).min(lines.len());
    if start < end {
        let snippet_lines: Vec<&str> = lines[start..end].iter().take(5).copied().collect();
        let mut snippet = snippet_lines.join("\n");
        if end - start > 5 {
            snippet.push_str("\n  ...");
        }
        if snippet.len() > 300 {
            snippet = format!("{}...", &snippet[..snippet.floor_char_boundary(300)]);
        }
        snippet
    } else {
        String::new()
    }
}

/// Convert symbols to semantic chunks with enriched context
fn symbols_to_chunks(
    file: &Path,
    symbols: &[Symbol],
    source: &str,
    project_root: &Path,
) -> Vec<SemanticChunk> {
    let mut chunks = Vec::new();

    for symbol in symbols {
        // Skip very small symbols (single-line variables, etc.)
        let line_count = symbol
            .range
            .end_line
            .saturating_sub(symbol.range.start_line)
            + 1;
        if line_count < 2 && !matches!(symbol.kind, SymbolKind::Variable) {
            continue;
        }

        let embed_text = build_embed_text(symbol, source, file, project_root);
        let snippet = build_snippet(symbol, source);

        chunks.push(SemanticChunk {
            file: file.to_path_buf(),
            name: symbol.name.clone(),
            kind: symbol.kind.clone(),
            start_line: symbol.range.start_line,
            end_line: symbol.range.end_line,
            exported: symbol.exported,
            embed_text,
            snippet,
        });

        // Note: Nested symbols are handled separately by the outline system
        // Each symbol is indexed individually
    }

    chunks
}

/// Cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

// Serialization helpers
fn symbol_kind_to_u8(kind: &SymbolKind) -> u8 {
    match kind {
        SymbolKind::Function => 0,
        SymbolKind::Class => 1,
        SymbolKind::Method => 2,
        SymbolKind::Struct => 3,
        SymbolKind::Interface => 4,
        SymbolKind::Enum => 5,
        SymbolKind::TypeAlias => 6,
        SymbolKind::Variable => 7,
        SymbolKind::Heading => 8,
    }
}

fn u8_to_symbol_kind(v: u8) -> SymbolKind {
    match v {
        0 => SymbolKind::Function,
        1 => SymbolKind::Class,
        2 => SymbolKind::Method,
        3 => SymbolKind::Struct,
        4 => SymbolKind::Interface,
        5 => SymbolKind::Enum,
        6 => SymbolKind::TypeAlias,
        7 => SymbolKind::Variable,
        _ => SymbolKind::Heading,
    }
}

fn read_u32(data: &[u8], pos: &mut usize) -> Result<u32, String> {
    if *pos + 4 > data.len() {
        return Err("unexpected end of data reading u32".to_string());
    }
    let val = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(val)
}

fn read_u64(data: &[u8], pos: &mut usize) -> Result<u64, String> {
    if *pos + 8 > data.len() {
        return Err("unexpected end of data reading u64".to_string());
    }
    let bytes: [u8; 8] = data[*pos..*pos + 8].try_into().unwrap();
    *pos += 8;
    Ok(u64::from_le_bytes(bytes))
}

fn read_string(data: &[u8], pos: &mut usize) -> Result<String, String> {
    let len = read_u32(data, pos)? as usize;
    if *pos + len > data.len() {
        return Err("unexpected end of data reading string".to_string());
    }
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).to_string();
    *pos += len;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 0.001);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.001);
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut index = SemanticIndex::new();
        index.entries.push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: PathBuf::from("/src/main.rs"),
                name: "handle_request".to_string(),
                kind: SymbolKind::Function,
                start_line: 10,
                end_line: 25,
                exported: true,
                embed_text: "file:src/main.rs kind:function name:handle_request".to_string(),
                snippet: "fn handle_request() {\n  // ...\n}".to_string(),
            },
            vector: vec![0.1, 0.2, 0.3, 0.4],
        });
        index.dimension = 4;
        index
            .file_mtimes
            .insert(PathBuf::from("/src/main.rs"), SystemTime::UNIX_EPOCH);

        let bytes = index.to_bytes();
        let restored = SemanticIndex::from_bytes(&bytes).unwrap();

        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].chunk.name, "handle_request");
        assert_eq!(restored.entries[0].vector, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(restored.dimension, 4);
    }

    #[test]
    fn test_search_top_k() {
        let mut index = SemanticIndex::new();
        index.dimension = 3;

        // Add entries with known vectors
        for (i, name) in ["auth", "database", "handler"].iter().enumerate() {
            let mut vec = vec![0.0f32; 3];
            vec[i] = 1.0; // orthogonal vectors
            index.entries.push(EmbeddingEntry {
                chunk: SemanticChunk {
                    file: PathBuf::from("/src/lib.rs"),
                    name: name.to_string(),
                    kind: SymbolKind::Function,
                    start_line: (i * 10 + 1) as u32,
                    end_line: (i * 10 + 5) as u32,
                    exported: true,
                    embed_text: format!("kind:function name:{}", name),
                    snippet: format!("fn {}() {{}}", name),
                },
                vector: vec,
            });
        }

        // Query aligned with "auth" (index 0)
        let query = vec![0.9, 0.1, 0.0];
        let results = index.search(&query, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "auth"); // highest score
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_empty_index_search() {
        let index = SemanticIndex::new();
        let results = index.search(&[0.1, 0.2, 0.3], 10);
        assert!(results.is_empty());
    }

    #[test]
    fn rejects_oversized_dimension_during_deserialization() {
        let mut bytes = Vec::new();
        bytes.push(1u8);
        bytes.extend_from_slice(&((MAX_DIMENSION as u32) + 1).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        assert!(SemanticIndex::from_bytes(&bytes).is_err());
    }

    #[test]
    fn rejects_oversized_entry_count_during_deserialization() {
        let mut bytes = Vec::new();
        bytes.push(1u8);
        bytes.extend_from_slice(&(DEFAULT_DIMENSION as u32).to_le_bytes());
        bytes.extend_from_slice(&((MAX_ENTRIES as u32) + 1).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());

        assert!(SemanticIndex::from_bytes(&bytes).is_err());
    }

    #[test]
    fn invalidate_file_removes_entries_and_mtime() {
        let target = PathBuf::from("/src/main.rs");
        let mut index = SemanticIndex::new();
        index.entries.push(EmbeddingEntry {
            chunk: SemanticChunk {
                file: target.clone(),
                name: "main".to_string(),
                kind: SymbolKind::Function,
                start_line: 0,
                end_line: 1,
                exported: false,
                embed_text: "main".to_string(),
                snippet: "fn main() {}".to_string(),
            },
            vector: vec![1.0; DEFAULT_DIMENSION],
        });
        index
            .file_mtimes
            .insert(target.clone(), SystemTime::UNIX_EPOCH);

        index.invalidate_file(&target);

        assert!(index.entries.is_empty());
        assert!(!index.file_mtimes.contains_key(&target));
    }

    #[test]
    fn detects_missing_onnx_runtime_from_dynamic_load_error() {
        let message = "Failed to load ONNX Runtime shared library libonnxruntime.dylib via dlopen: no such file";

        assert!(is_onnx_runtime_unavailable(message));
    }

    #[test]
    fn formats_missing_onnx_runtime_with_install_hint() {
        let message = format_embedding_init_error(
            "Failed to load ONNX Runtime shared library libonnxruntime.so via dlopen: no such file",
        );

        assert!(message.starts_with("ONNX Runtime not found. Install via:"));
        assert!(message.contains("Original error:"));
    }
}
