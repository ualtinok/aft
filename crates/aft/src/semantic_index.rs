use crate::config::{SemanticBackend, SemanticBackendConfig};
use crate::parser::FileParser;
use crate::symbols::{Symbol, SymbolKind};

use fastembed::{EmbeddingModel as FastembedEmbeddingModel, InitOptions, TextEmbedding};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::time::SystemTime;
use url::Url;

const DEFAULT_DIMENSION: usize = 384;
const MAX_ENTRIES: usize = 1_000_000;
const MAX_DIMENSION: usize = 1024;
const F32_BYTES: usize = std::mem::size_of::<f32>();
const HEADER_BYTES_V1: usize = 9;
const HEADER_BYTES_V2: usize = 13;
const ONNX_RUNTIME_INSTALL_HINT: &str =
    "ONNX Runtime not found. Install via: brew install onnxruntime (macOS) or apt install libonnxruntime (Linux).";

const SEMANTIC_INDEX_VERSION_V1: u8 = 1;
const SEMANTIC_INDEX_VERSION_V2: u8 = 2;
const DEFAULT_OPENAI_EMBEDDING_PATH: &str = "/embeddings";
const DEFAULT_OLLAMA_EMBEDDING_PATH: &str = "/api/embed";
// Must stay below the bridge timeout (30s) to avoid bridge kills on slow backends.
const DEFAULT_OPENAI_EMBEDDING_TIMEOUT_MS: u64 = 25_000;
const DEFAULT_MAX_BATCH_SIZE: usize = 64;
const FALLBACK_BACKEND: &str = "none";
const EMBEDDING_REQUEST_MAX_ATTEMPTS: usize = 3;
const EMBEDDING_REQUEST_BACKOFF_MS: [u64; 2] = [500, 1_000];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticIndexFingerprint {
    pub backend: String,
    pub model: String,
    #[serde(default)]
    pub base_url: String,
    pub dimension: usize,
}

impl SemanticIndexFingerprint {
    fn from_config(config: &SemanticBackendConfig, dimension: usize) -> Self {
        // Use normalized URL for fingerprinting so cosmetic differences
        // (e.g. "http://host/v1" vs "http://host/v1/") don't cause rebuilds.
        let base_url = config
            .base_url
            .as_ref()
            .and_then(|u| normalize_base_url(u).ok())
            .unwrap_or_else(|| FALLBACK_BACKEND.to_string());
        Self {
            backend: config.backend.as_str().to_string(),
            model: config.model.clone(),
            base_url,
            dimension,
        }
    }

    pub fn as_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::new())
    }

    fn matches_expected(&self, expected: &str) -> bool {
        let encoded = self.as_string();
        !encoded.is_empty() && encoded == expected
    }
}

enum SemanticEmbeddingEngine {
    Fastembed(TextEmbedding),
    OpenAiCompatible {
        client: Client,
        model: String,
        base_url: String,
        api_key: Option<String>,
    },
    Ollama {
        client: Client,
        model: String,
        base_url: String,
    },
}

pub struct SemanticEmbeddingModel {
    backend: SemanticBackend,
    model: String,
    base_url: Option<String>,
    timeout_ms: u64,
    max_batch_size: usize,
    dimension: Option<usize>,
    engine: SemanticEmbeddingEngine,
}

pub type EmbeddingModel = SemanticEmbeddingModel;

fn validate_embedding_batch(
    vectors: &[Vec<f32>],
    expected_count: usize,
    context: &str,
) -> Result<(), String> {
    if expected_count > 0 && vectors.is_empty() {
        return Err(format!(
            "{context} returned no vectors for {expected_count} inputs"
        ));
    }

    if vectors.len() != expected_count {
        return Err(format!(
            "{context} returned {} vectors for {} inputs",
            vectors.len(),
            expected_count
        ));
    }

    let Some(first_vector) = vectors.first() else {
        return Ok(());
    };
    let expected_dimension = first_vector.len();
    for (index, vector) in vectors.iter().enumerate() {
        if vector.len() != expected_dimension {
            return Err(format!(
                "{context} returned inconsistent embedding dimensions: vector 0 has length {expected_dimension}, vector {index} has length {}",
                vector.len()
            ));
        }
    }

    Ok(())
}

fn normalize_base_url(raw: &str) -> Result<String, String> {
    let parsed = Url::parse(raw).map_err(|error| format!("invalid base_url '{raw}': {error}"))?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "unsupported URL scheme '{}' — only http:// and https:// are allowed",
            scheme
        ));
    }
    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn build_openai_embeddings_endpoint(base_url: &str) -> String {
    if base_url.ends_with("/v1") {
        format!("{base_url}{DEFAULT_OPENAI_EMBEDDING_PATH}")
    } else {
        format!("{base_url}/v1{}", DEFAULT_OPENAI_EMBEDDING_PATH)
    }
}

fn build_ollama_embeddings_endpoint(base_url: &str) -> String {
    if base_url.ends_with("/api") {
        format!("{base_url}/embed")
    } else {
        format!("{base_url}{DEFAULT_OLLAMA_EMBEDDING_PATH}")
    }
}

fn normalize_api_key(value: Option<String>) -> Option<String> {
    value.and_then(|token| {
        let token = token.trim();
        if token.is_empty() {
            None
        } else {
            Some(token.to_string())
        }
    })
}

fn is_retryable_embedding_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

fn is_retryable_embedding_error(error: &reqwest::Error) -> bool {
    error.is_connect()
}

fn sleep_before_embedding_retry(attempt_index: usize) {
    if let Some(delay_ms) = EMBEDDING_REQUEST_BACKOFF_MS.get(attempt_index) {
        std::thread::sleep(Duration::from_millis(*delay_ms));
    }
}

fn send_embedding_request<F>(mut make_request: F, backend_label: &str) -> Result<String, String>
where
    F: FnMut() -> reqwest::blocking::RequestBuilder,
{
    for attempt_index in 0..EMBEDDING_REQUEST_MAX_ATTEMPTS {
        let last_attempt = attempt_index + 1 == EMBEDDING_REQUEST_MAX_ATTEMPTS;

        let response = match make_request().send() {
            Ok(response) => response,
            Err(error) => {
                if !last_attempt && is_retryable_embedding_error(&error) {
                    sleep_before_embedding_retry(attempt_index);
                    continue;
                }
                return Err(format!("{backend_label} request failed: {error}"));
            }
        };

        let status = response.status();
        let raw = match response.text() {
            Ok(raw) => raw,
            Err(error) => {
                if !last_attempt && is_retryable_embedding_error(&error) {
                    sleep_before_embedding_retry(attempt_index);
                    continue;
                }
                return Err(format!("{backend_label} response read failed: {error}"));
            }
        };

        if status.is_success() {
            return Ok(raw);
        }

        if !last_attempt && is_retryable_embedding_status(status) {
            sleep_before_embedding_retry(attempt_index);
            continue;
        }

        return Err(format!(
            "{backend_label} request failed (HTTP {}): {}",
            status, raw
        ));
    }

    unreachable!("embedding request retries exhausted without returning")
}

impl SemanticEmbeddingModel {
    pub fn from_config(config: &SemanticBackendConfig) -> Result<Self, String> {
        let timeout_ms = if config.timeout_ms == 0 {
            DEFAULT_OPENAI_EMBEDDING_TIMEOUT_MS
        } else {
            config.timeout_ms
        };

        let max_batch_size = if config.max_batch_size == 0 {
            DEFAULT_MAX_BATCH_SIZE
        } else {
            config.max_batch_size
        };

        let api_key_env = normalize_api_key(config.api_key_env.clone());
        let model = config.model.clone();

        let client = Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| format!("failed to configure embedding client: {error}"))?;

        let engine = match config.backend {
            SemanticBackend::Fastembed => {
                SemanticEmbeddingEngine::Fastembed(initialize_text_embedding(&model)?)
            }
            SemanticBackend::OpenAiCompatible => {
                let raw = config.base_url.as_ref().ok_or_else(|| {
                    "base_url is required for openai_compatible backend".to_string()
                })?;
                let base_url = normalize_base_url(raw)?;

                let api_key = match api_key_env {
                    Some(var_name) => Some(env::var(&var_name).map_err(|_| {
                        format!("missing api_key_env '{var_name}' for openai_compatible backend")
                    })?),
                    None => None,
                };

                SemanticEmbeddingEngine::OpenAiCompatible {
                    client,
                    model,
                    base_url,
                    api_key,
                }
            }
            SemanticBackend::Ollama => {
                let raw = config
                    .base_url
                    .as_ref()
                    .ok_or_else(|| "base_url is required for ollama backend".to_string())?;
                let base_url = normalize_base_url(raw)?;

                SemanticEmbeddingEngine::Ollama {
                    client,
                    model,
                    base_url,
                }
            }
        };

        Ok(Self {
            backend: config.backend,
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            timeout_ms,
            max_batch_size,
            dimension: None,
            engine,
        })
    }

    pub fn backend(&self) -> SemanticBackend {
        self.backend
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn max_batch_size(&self) -> usize {
        self.max_batch_size
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    pub fn fingerprint(
        &mut self,
        config: &SemanticBackendConfig,
    ) -> Result<SemanticIndexFingerprint, String> {
        let dimension = self.dimension()?;
        Ok(SemanticIndexFingerprint::from_config(config, dimension))
    }

    pub fn dimension(&mut self) -> Result<usize, String> {
        if let Some(dimension) = self.dimension {
            return Ok(dimension);
        }

        let dimension = match &mut self.engine {
            SemanticEmbeddingEngine::Fastembed(model) => {
                let vectors = model
                    .embed(vec!["semantic index fingerprint probe".to_string()], None)
                    .map_err(|error| format_embedding_init_error(error.to_string()))?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
            SemanticEmbeddingEngine::OpenAiCompatible { .. } => {
                let vectors =
                    self.embed_texts(vec!["semantic index fingerprint probe".to_string()])?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
            SemanticEmbeddingEngine::Ollama { .. } => {
                let vectors =
                    self.embed_texts(vec!["semantic index fingerprint probe".to_string()])?;
                vectors
                    .first()
                    .map(|v| v.len())
                    .ok_or_else(|| "embedding backend returned no vectors".to_string())?
            }
        };

        self.dimension = Some(dimension);
        Ok(dimension)
    }

    pub fn embed(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        self.embed_texts(texts)
    }

    fn embed_texts(&mut self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, String> {
        match &mut self.engine {
            SemanticEmbeddingEngine::Fastembed(model) => model
                .embed(texts, None::<usize>)
                .map_err(|error| format_embedding_init_error(error.to_string()))
                .map_err(|error| format!("failed to embed batch: {error}")),
            SemanticEmbeddingEngine::OpenAiCompatible {
                client,
                model,
                base_url,
                api_key,
            } => {
                let expected_text_count = texts.len();
                let endpoint = build_openai_embeddings_endpoint(base_url);
                let body = serde_json::json!({
                    "input": texts,
                    "model": model,
                });

                let raw = send_embedding_request(
                    || {
                        let mut request = client
                            .post(&endpoint)
                            .json(&body)
                            .header("Content-Type", "application/json");

                        if let Some(api_key) = api_key {
                            request = request.header("Authorization", format!("Bearer {api_key}"));
                        }

                        request
                    },
                    "openai compatible",
                )?;

                #[derive(Deserialize)]
                struct OpenAiResponse {
                    data: Vec<OpenAiEmbeddingResult>,
                }

                #[derive(Deserialize)]
                struct OpenAiEmbeddingResult {
                    embedding: Vec<f32>,
                    index: Option<u32>,
                }

                let parsed: OpenAiResponse = serde_json::from_str(&raw)
                    .map_err(|error| format!("invalid openai compatible response: {error}"))?;
                if parsed.data.len() != expected_text_count {
                    return Err(format!(
                        "openai compatible response returned {} embeddings for {} inputs",
                        parsed.data.len(),
                        expected_text_count
                    ));
                }

                let mut vectors = vec![Vec::new(); parsed.data.len()];
                for (i, item) in parsed.data.into_iter().enumerate() {
                    let index = item.index.unwrap_or(i as u32) as usize;
                    if index >= vectors.len() {
                        return Err(
                            "openai compatible response contains invalid vector index".to_string()
                        );
                    }
                    vectors[index] = item.embedding;
                }

                for vector in &vectors {
                    if vector.is_empty() {
                        return Err(
                            "openai compatible response contained missing vectors".to_string()
                        );
                    }
                }

                self.dimension = vectors.first().map(Vec::len);
                Ok(vectors)
            }
            SemanticEmbeddingEngine::Ollama {
                client,
                model,
                base_url,
            } => {
                let expected_text_count = texts.len();
                let endpoint = build_ollama_embeddings_endpoint(base_url);

                #[derive(Serialize)]
                struct OllamaPayload<'a> {
                    model: &'a str,
                    input: Vec<String>,
                }

                let payload = OllamaPayload {
                    model,
                    input: texts,
                };

                let raw = send_embedding_request(
                    || {
                        client
                            .post(&endpoint)
                            .json(&payload)
                            .header("Content-Type", "application/json")
                    },
                    "ollama",
                )?;

                #[derive(Deserialize)]
                struct OllamaResponse {
                    embeddings: Vec<Vec<f32>>,
                }

                let parsed: OllamaResponse = serde_json::from_str(&raw)
                    .map_err(|error| format!("invalid ollama response: {error}"))?;
                if parsed.embeddings.is_empty() {
                    return Err("ollama response returned no embeddings".to_string());
                }
                if parsed.embeddings.len() != expected_text_count {
                    return Err(format!(
                        "ollama response returned {} embeddings for {} inputs",
                        parsed.embeddings.len(),
                        expected_text_count
                    ));
                }

                let vectors = parsed.embeddings;
                for vector in &vectors {
                    if vector.is_empty() {
                        return Err("ollama response contained empty embeddings".to_string());
                    }
                }

                self.dimension = vectors.first().map(Vec::len);
                Ok(vectors)
            }
        }
    }
}

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

pub fn initialize_text_embedding(model: &str) -> Result<TextEmbedding, String> {
    // Pre-validate before ort can panic on a bad library
    pre_validate_onnx_runtime()?;

    let selected_model = match model {
        "all-MiniLM-L6-v2" | "all-minilm-l6-v2" => FastembedEmbeddingModel::AllMiniLML6V2,
        _ => {
            return Err(format!(
                "unsupported fastembed model '{}'. Supported: all-MiniLM-L6-v2",
                model
            ))
        }
    };

    TextEmbedding::try_new(InitOptions::new(selected_model)).map_err(format_embedding_init_error)
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
#[derive(Debug)]
struct EmbeddingEntry {
    chunk: SemanticChunk,
    vector: Vec<f32>,
}

/// The semantic index — stores embeddings for all symbols in a project
#[derive(Debug)]
pub struct SemanticIndex {
    entries: Vec<EmbeddingEntry>,
    /// Track which files are indexed and their mtime for staleness detection
    file_mtimes: HashMap<PathBuf, SystemTime>,
    /// Embedding dimension (384 for MiniLM-L6-v2)
    dimension: usize,
    fingerprint: Option<SemanticIndexFingerprint>,
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
            fingerprint: None,
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

    fn collect_chunks(
        project_root: &Path,
        files: &[PathBuf],
    ) -> (Vec<SemanticChunk>, HashMap<PathBuf, SystemTime>) {
        let mut parser = FileParser::new();
        let mut chunks: Vec<SemanticChunk> = Vec::new();
        let mut file_mtimes: HashMap<PathBuf, SystemTime> = HashMap::new();

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

        (chunks, file_mtimes)
    }

    fn build_from_chunks<F, P>(
        chunks: Vec<SemanticChunk>,
        file_mtimes: HashMap<PathBuf, SystemTime>,
        embed_fn: &mut F,
        max_batch_size: usize,
        mut progress: Option<&mut P>,
    ) -> Result<Self, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        let total_chunks = chunks.len();

        if chunks.is_empty() {
            return Ok(Self {
                entries: Vec::new(),
                file_mtimes,
                dimension: DEFAULT_DIMENSION,
                fingerprint: None,
            });
        }

        // Embed in batches
        let mut entries: Vec<EmbeddingEntry> = Vec::with_capacity(chunks.len());
        let mut expected_dimension: Option<usize> = None;
        let batch_size = max_batch_size.max(1);
        for batch_start in (0..chunks.len()).step_by(batch_size) {
            let batch_end = (batch_start + batch_size).min(chunks.len());
            let batch_texts: Vec<String> = chunks[batch_start..batch_end]
                .iter()
                .map(|c| c.embed_text.clone())
                .collect();

            let vectors = embed_fn(batch_texts)?;
            validate_embedding_batch(&vectors, batch_end - batch_start, "embedding backend")?;

            // Track consistent dimension across all batches
            if let Some(dim) = vectors.first().map(|v| v.len()) {
                match expected_dimension {
                    None => expected_dimension = Some(dim),
                    Some(expected) if dim != expected => {
                        return Err(format!(
                            "embedding dimension changed across batches: expected {expected}, got {dim}"
                        ));
                    }
                    _ => {}
                }
            }

            for (i, vector) in vectors.into_iter().enumerate() {
                let chunk_idx = batch_start + i;
                entries.push(EmbeddingEntry {
                    chunk: chunks[chunk_idx].clone(),
                    vector,
                });
            }

            if let Some(callback) = progress.as_mut() {
                callback(entries.len(), total_chunks);
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
            fingerprint: None,
        })
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
        let (chunks, file_mtimes) = Self::collect_chunks(project_root, files);
        Self::build_from_chunks(
            chunks,
            file_mtimes,
            embed_fn,
            max_batch_size,
            Option::<&mut fn(usize, usize)>::None,
        )
    }

    /// Build the semantic index and report embedding progress using entry counts.
    pub fn build_with_progress<F, P>(
        project_root: &Path,
        files: &[PathBuf],
        embed_fn: &mut F,
        max_batch_size: usize,
        progress: &mut P,
    ) -> Result<Self, String>
    where
        F: FnMut(Vec<String>) -> Result<Vec<Vec<f32>>, String>,
        P: FnMut(usize, usize),
    {
        let (chunks, file_mtimes) = Self::collect_chunks(project_root, files);
        let total_chunks = chunks.len();
        progress(0, total_chunks);
        Self::build_from_chunks(
            chunks,
            file_mtimes,
            embed_fn,
            max_batch_size,
            Some(progress),
        )
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
            Some(stored_mtime) => match fs::metadata(file).and_then(|m| m.modified()) {
                Ok(current_mtime) => *stored_mtime != current_mtime,
                Err(_) => true,
            },
        }
    }

    pub fn count_stale_files(&self) -> usize {
        self.file_mtimes
            .keys()
            .filter(|path| self.is_file_stale(path))
            .count()
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

    pub fn fingerprint(&self) -> Option<&SemanticIndexFingerprint> {
        self.fingerprint.as_ref()
    }

    pub fn backend_label(&self) -> Option<&str> {
        self.fingerprint.as_ref().map(|f| f.backend.as_str())
    }

    pub fn model_label(&self) -> Option<&str> {
        self.fingerprint.as_ref().map(|f| f.model.as_str())
    }

    pub fn set_fingerprint(&mut self, fingerprint: SemanticIndexFingerprint) {
        self.fingerprint = Some(fingerprint);
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
    pub fn read_from_disk(
        storage_dir: &Path,
        project_key: &str,
        expected_fingerprint: Option<&str>,
    ) -> Option<Self> {
        let data_path = storage_dir
            .join("semantic")
            .join(project_key)
            .join("semantic.bin");
        let file_len = usize::try_from(fs::metadata(&data_path).ok()?.len()).ok()?;
        if file_len < HEADER_BYTES_V1 {
            log::warn!(
                "[aft] corrupt semantic index (too small: {} bytes), removing",
                file_len
            );
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
                if let Some(expected) = expected_fingerprint {
                    let matches = index
                        .fingerprint()
                        .map(|fingerprint| fingerprint.matches_expected(expected))
                        .unwrap_or(false);
                    if !matches {
                        log::info!("[aft] cached semantic index fingerprint mismatch, rebuilding");
                        let _ = fs::remove_file(&data_path);
                        return None;
                    }
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
        let fingerprint_bytes = self.fingerprint.as_ref().and_then(|fingerprint| {
            let encoded = fingerprint.as_string();
            if encoded.is_empty() {
                None
            } else {
                Some(encoded.into_bytes())
            }
        });

        // Header: version(1) + dimension(4) + entry_count(4) [+ fingerprint_len(4)]
        let version = if fingerprint_bytes.is_some() {
            SEMANTIC_INDEX_VERSION_V2
        } else {
            SEMANTIC_INDEX_VERSION_V1
        };
        buf.push(version);
        buf.extend_from_slice(&(self.dimension as u32).to_le_bytes());
        buf.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        if let Some(bytes) = &fingerprint_bytes {
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }

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

        if data.len() < HEADER_BYTES_V1 {
            return Err("data too short".to_string());
        }

        let version = data[pos];
        pos += 1;
        if version != SEMANTIC_INDEX_VERSION_V1 && version != SEMANTIC_INDEX_VERSION_V2 {
            return Err(format!("unsupported version: {}", version));
        }
        if version == SEMANTIC_INDEX_VERSION_V2 && data.len() < HEADER_BYTES_V2 {
            return Err("data too short for semantic index v2 header".to_string());
        }

        let dimension = read_u32(data, &mut pos)? as usize;
        let entry_count = read_u32(data, &mut pos)? as usize;
        if dimension == 0 || dimension > MAX_DIMENSION {
            return Err(format!("invalid embedding dimension: {}", dimension));
        }
        if entry_count > MAX_ENTRIES {
            return Err(format!("too many semantic index entries: {}", entry_count));
        }

        let fingerprint = if version == SEMANTIC_INDEX_VERSION_V2 {
            let fingerprint_len = read_u32(data, &mut pos)? as usize;
            if pos + fingerprint_len > data.len() {
                return Err("unexpected end of data reading fingerprint".to_string());
            }
            let raw = String::from_utf8_lossy(&data[pos..pos + fingerprint_len]).to_string();
            pos += fingerprint_len;
            Some(
                serde_json::from_str::<SemanticIndexFingerprint>(&raw)
                    .map_err(|error| format!("invalid semantic fingerprint: {error}"))?,
            )
        } else {
            None
        };

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
            fingerprint,
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
    use crate::config::{SemanticBackend, SemanticBackendConfig};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn start_mock_http_server<F>(handler: F) -> (String, thread::JoinHandle<()>)
    where
        F: Fn(String, String, String) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            let mut content_length = 0usize;
            loop {
                let n = stream.read(&mut chunk).expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if header_end.is_none() {
                    if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let headers = String::from_utf8_lossy(&buf[..pos + 4]);
                        for line in headers.lines() {
                            if let Some(value) = line.strip_prefix("Content-Length:") {
                                content_length = value.trim().parse::<usize>().unwrap_or(0);
                            }
                        }
                    }
                }
                if let Some(end) = header_end {
                    if buf.len() >= end + content_length {
                        break;
                    }
                }
            }

            let end = header_end.expect("header terminator");
            let request = String::from_utf8_lossy(&buf[..end]).to_string();
            let body = String::from_utf8_lossy(&buf[end..end + content_length]).to_string();
            let mut lines = request.lines();
            let request_line = lines.next().expect("request line").to_string();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .expect("request path")
                .to_string();
            let response_body = handler(request_line, path, body);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        (format!("http://{}", addr), handle)
    }

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
        index.set_fingerprint(SemanticIndexFingerprint {
            backend: "fastembed".to_string(),
            model: "all-MiniLM-L6-v2".to_string(),
            base_url: FALLBACK_BACKEND.to_string(),
            dimension: 4,
        });

        let bytes = index.to_bytes();
        let restored = SemanticIndex::from_bytes(&bytes).unwrap();

        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].chunk.name, "handle_request");
        assert_eq!(restored.entries[0].vector, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(restored.dimension, 4);
        assert_eq!(restored.backend_label(), Some("fastembed"));
        assert_eq!(restored.model_label(), Some("all-MiniLM-L6-v2"));
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

    #[test]
    fn openai_compatible_backend_embeds_with_mock_server() {
        let (base_url, handle) = start_mock_http_server(|request_line, path, _body| {
            assert!(request_line.starts_with("POST "));
            assert_eq!(path, "/v1/embeddings");
            "{\"data\":[{\"embedding\":[0.1,0.2,0.3],\"index\":0},{\"embedding\":[0.4,0.5,0.6],\"index\":1}]}".to_string()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::OpenAiCompatible,
            model: "test-embedding".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors, vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]]);
        handle.join().unwrap();
    }

    #[test]
    fn ollama_backend_embeds_with_mock_server() {
        let (base_url, handle) = start_mock_http_server(|request_line, path, _body| {
            assert!(request_line.starts_with("POST "));
            assert_eq!(path, "/api/embed");
            "{\"embeddings\":[[0.7,0.8,0.9],[1.0,1.1,1.2]]}".to_string()
        });

        let config = SemanticBackendConfig {
            backend: SemanticBackend::Ollama,
            model: "embeddinggemma".to_string(),
            base_url: Some(base_url),
            api_key_env: None,
            timeout_ms: 5_000,
            max_batch_size: 64,
        };

        let mut model = SemanticEmbeddingModel::from_config(&config).unwrap();
        let vectors = model
            .embed(vec!["hello".to_string(), "world".to_string()])
            .unwrap();

        assert_eq!(vectors, vec![vec![0.7, 0.8, 0.9], vec![1.0, 1.1, 1.2]]);
        handle.join().unwrap();
    }

    #[test]
    fn read_from_disk_rejects_fingerprint_mismatch() {
        let storage = tempfile::tempdir().unwrap();
        let project_key = "proj";

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
                snippet: "fn handle_request() {}".to_string(),
            },
            vector: vec![0.1, 0.2, 0.3],
        });
        index.dimension = 3;
        index
            .file_mtimes
            .insert(PathBuf::from("/src/main.rs"), SystemTime::UNIX_EPOCH);
        index.set_fingerprint(SemanticIndexFingerprint {
            backend: "openai_compatible".to_string(),
            model: "test-embedding".to_string(),
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            dimension: 3,
        });
        index.write_to_disk(storage.path(), project_key);

        let matching = index.fingerprint().unwrap().as_string();
        assert!(
            SemanticIndex::read_from_disk(storage.path(), project_key, Some(&matching)).is_some()
        );

        let mismatched = SemanticIndexFingerprint {
            backend: "ollama".to_string(),
            model: "embeddinggemma".to_string(),
            base_url: "http://127.0.0.1:11434".to_string(),
            dimension: 3,
        }
        .as_string();
        assert!(
            SemanticIndex::read_from_disk(storage.path(), project_key, Some(&mismatched)).is_none()
        );
    }
}
