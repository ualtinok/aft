use std::path::PathBuf;

/// Runtime configuration for the aft process.
///
/// Holds project-scoped settings and tuning knobs. Values are set at startup
/// and remain immutable for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticBackend {
    Fastembed,
    OpenAiCompatible,
    Ollama,
}

impl SemanticBackend {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Fastembed => "fastembed",
            Self::OpenAiCompatible => "openai_compatible",
            Self::Ollama => "ollama",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "fastembed" => Some(Self::Fastembed),
            "openai_compatible" => Some(Self::OpenAiCompatible),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBackendConfig {
    pub backend: SemanticBackend,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub timeout_ms: u64,
    pub max_batch_size: usize,
}

impl Default for SemanticBackendConfig {
    fn default() -> Self {
        Self {
            backend: SemanticBackend::Fastembed,
            model: DEFAULT_SEMANTIC_MODEL.to_string(),
            base_url: None,
            api_key_env: None,
            timeout_ms: 60_000,
            max_batch_size: 64,
        }
    }
}

pub const DEFAULT_SEMANTIC_MODEL: &str = "all-MiniLM-L6-v2";

impl Config {
    pub fn semantic_backend_label(&self) -> &'static str {
        self.semantic.backend.as_str()
    }
}

pub struct Config {
    /// Root directory of the project being analyzed. `None` if not scoped.
    pub project_root: Option<PathBuf>,
    /// How many levels of call-graph edges to follow during validation (default: 1).
    pub validation_depth: u32,
    /// Hours before a checkpoint expires and is eligible for cleanup (default: 24).
    pub checkpoint_ttl_hours: u32,
    /// Maximum depth for recursive symbol resolution (default: 10).
    pub max_symbol_depth: u32,
    /// Seconds before killing a formatter subprocess (default: 10).
    pub formatter_timeout_secs: u32,
    /// Seconds before killing a type-checker subprocess (default: 30).
    pub type_checker_timeout_secs: u32,
    /// Whether to auto-format files after edits (default: true).
    pub format_on_edit: bool,
    /// Whether to auto-validate files after edits (default: false).
    /// When "syntax", only tree-sitter parse check. When "full", runs type checker.
    pub validate_on_edit: Option<String>,
    /// Per-language formatter overrides. Keys: "typescript", "python", "rust", "go".
    /// Values: "biome", "prettier", "deno", "ruff", "black", "rustfmt", "goimports", "gofmt", "none".
    pub formatter: std::collections::HashMap<String, String>,
    /// Per-language type checker overrides. Keys: "typescript", "python", "rust", "go".
    /// Values: "tsc", "biome", "pyright", "ruff", "cargo", "go", "staticcheck", "none".
    pub checker: std::collections::HashMap<String, String>,
    /// Whether to restrict file operations to within `project_root` (default: false).
    /// When true, write-capable commands reject paths outside the project root.
    pub restrict_to_project_root: bool,
    /// Enable the experimental trigram search index (default: false).
    pub experimental_search_index: bool,
    /// Enable the experimental semantic search index (default: false).
    pub experimental_semantic_search: bool,
    /// Maximum file size to fully index in bytes (default: 1MB).
    pub search_index_max_file_size: u64,
    pub semantic: SemanticBackendConfig,
    /// Persistent storage directory for indexes (trigram, semantic).
    /// Set by the plugin to the XDG-compliant path (e.g. ~/.local/share/opencode/storage/plugin/aft/).
    /// Falls back to ~/.cache/aft/ if not set.
    pub storage_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            project_root: None,
            validation_depth: 1,
            checkpoint_ttl_hours: 24,
            max_symbol_depth: 10,
            formatter_timeout_secs: 10,
            type_checker_timeout_secs: 30,
            format_on_edit: true,
            validate_on_edit: None,
            formatter: std::collections::HashMap::new(),
            checker: std::collections::HashMap::new(),
            // Default to false to match OpenCode's existing permission-based model.
            // The plugin opts into root restriction explicitly when desired.
            restrict_to_project_root: false,
            experimental_search_index: false,
            experimental_semantic_search: false,
            search_index_max_file_size: 1_048_576,
            semantic: SemanticBackendConfig::default(),
            storage_dir: None,
        }
    }
}
