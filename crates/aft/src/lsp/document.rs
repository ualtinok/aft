use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Tracks document state for LSP synchronization.
///
/// LSP requires:
/// 1. didOpen before didChange (document must be opened first)
/// 2. Version numbers must be monotonically increasing
/// 3. Full content sent with each change (TextDocumentSyncKind::Full)
#[derive(Debug, Default)]
pub struct DocumentStore {
    /// Maps canonical file path -> current version number.
    versions: HashMap<PathBuf, i32>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            versions: HashMap::new(),
        }
    }

    /// Check if a document is already opened (tracked).
    pub fn is_open(&self, path: &Path) -> bool {
        self.versions.contains_key(path)
    }

    /// Open a new document. Returns the initial version (0).
    pub fn open(&mut self, path: PathBuf) -> i32 {
        let version = 0;
        self.versions.insert(path, version);
        version
    }

    /// Bump the version for an already-open document. Returns the new version.
    /// Panics if document is not open.
    pub fn bump_version(&mut self, path: &Path) -> i32 {
        let version = self.versions.get_mut(path).expect("document not open");
        *version += 1;
        *version
    }

    /// Get current version, or None if not open.
    pub fn version(&self, path: &Path) -> Option<i32> {
        self.versions.get(path).copied()
    }

    /// Close a document and remove from tracking.
    pub fn close(&mut self, path: &Path) -> Option<i32> {
        self.versions.remove(path)
    }

    /// Get all open document paths.
    pub fn open_documents(&self) -> Vec<&PathBuf> {
        self.versions.keys().collect()
    }
}
