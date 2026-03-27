use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::AftError;

/// A single backup entry for a file.
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub backup_id: String,
    pub content: String,
    pub timestamp: u64,
    pub description: String,
}

/// Per-file undo store backed by an in-memory stack.
///
/// Keys are canonical paths (resolved via `std::fs::canonicalize` with fallback
/// to cleaned relative path). Each file maps to a stack of `BackupEntry` values
/// ordered oldest-first so `restore_latest` pops from the end.
#[derive(Debug)]
pub struct BackupStore {
    entries: HashMap<PathBuf, Vec<BackupEntry>>,
    counter: AtomicU64,
}

impl BackupStore {
    pub fn new() -> Self {
        BackupStore {
            entries: HashMap::new(),
            counter: AtomicU64::new(0),
        }
    }

    /// Snapshot the current contents of `path` with a description.
    ///
    /// Returns the generated backup ID. Fails with `FileNotFound` if the file
    /// cannot be read.
    pub fn snapshot(&mut self, path: &Path, description: &str) -> Result<String, AftError> {
        let content = std::fs::read_to_string(path).map_err(|_| AftError::FileNotFound {
            path: path.display().to_string(),
        })?;

        let key = canonicalize_key(path);
        let id = self.next_id();
        let entry = BackupEntry {
            backup_id: id.clone(),
            content,
            timestamp: current_timestamp(),
            description: description.to_string(),
        };

        let stack = self.entries.entry(key).or_default();
        // Cap per-file undo depth to prevent unbounded memory growth.
        // Glob edits can touch hundreds of files, and repeated edits to large
        // files would otherwise accumulate full-content copies indefinitely.
        const MAX_UNDO_DEPTH: usize = 20;
        if stack.len() >= MAX_UNDO_DEPTH {
            stack.remove(0); // evict oldest
        }
        stack.push(entry);
        Ok(id)
    }

    /// Pop the most recent backup for `path` and restore the file contents.
    ///
    /// Returns the restored entry. Fails with `NoUndoHistory` if no backups
    /// exist for the path.
    pub fn restore_latest(&mut self, path: &Path) -> Result<BackupEntry, AftError> {
        let key = canonicalize_key(path);
        let stack = self
            .entries
            .get_mut(&key)
            .ok_or_else(|| AftError::NoUndoHistory {
                path: path.display().to_string(),
            })?;

        let entry = stack.pop().ok_or_else(|| AftError::NoUndoHistory {
            path: path.display().to_string(),
        })?;

        // Remove the key entirely if stack is now empty
        if stack.is_empty() {
            self.entries.remove(&key);
        }

        // Write the restored content back to disk
        std::fs::write(path, &entry.content).map_err(|e| AftError::IoError {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;

        Ok(entry)
    }

    /// Return the backup history for a file (oldest first).
    pub fn history(&self, path: &Path) -> Vec<BackupEntry> {
        let key = canonicalize_key(path);
        self.entries.get(&key).cloned().unwrap_or_default()
    }

    /// Return all files that have at least one backup entry.
    pub fn tracked_files(&self) -> Vec<PathBuf> {
        self.entries.keys().cloned().collect()
    }

    fn next_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("backup-{}", n)
    }
}

/// Canonicalize path for use as a HashMap key.
///
/// Uses `std::fs::canonicalize` when the file exists, falls back to
/// the cleaned path for files that don't exist yet.
fn canonicalize_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("aft_backup_tests");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn snapshot_and_restore_round_trip() {
        let path = temp_file("round_trip.txt", "original");
        let mut store = BackupStore::new();

        let id = store.snapshot(&path, "before edit").unwrap();
        assert!(id.starts_with("backup-"));

        // Modify file
        fs::write(&path, "modified").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "modified");

        // Restore
        let entry = store.restore_latest(&path).unwrap();
        assert_eq!(entry.content, "original");
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    fn multiple_snapshots_preserve_order() {
        let path = temp_file("order.txt", "v1");
        let mut store = BackupStore::new();

        store.snapshot(&path, "first").unwrap();
        fs::write(&path, "v2").unwrap();
        store.snapshot(&path, "second").unwrap();
        fs::write(&path, "v3").unwrap();
        store.snapshot(&path, "third").unwrap();

        let history = store.history(&path);
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].description, "first");
        assert_eq!(history[1].description, "second");
        assert_eq!(history[2].description, "third");
        assert_eq!(history[0].content, "v1");
        assert_eq!(history[1].content, "v2");
        assert_eq!(history[2].content, "v3");
    }

    #[test]
    fn restore_pops_from_stack() {
        let path = temp_file("pop.txt", "v1");
        let mut store = BackupStore::new();

        store.snapshot(&path, "first").unwrap();
        fs::write(&path, "v2").unwrap();
        store.snapshot(&path, "second").unwrap();

        let entry = store.restore_latest(&path).unwrap();
        assert_eq!(entry.description, "second");
        assert_eq!(entry.content, "v2");

        // One entry remains
        let history = store.history(&path);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].description, "first");
    }

    #[test]
    fn empty_history_returns_empty_vec() {
        let store = BackupStore::new();
        let path = Path::new("/tmp/aft_backup_tests/nonexistent_history.txt");
        let history = store.history(path);
        assert!(history.is_empty());
    }

    #[test]
    fn snapshot_nonexistent_file_returns_error() {
        let mut store = BackupStore::new();
        let path = Path::new("/tmp/aft_backup_tests/absolutely_does_not_exist.txt");
        let result = store.snapshot(path, "test");
        assert!(result.is_err());
        match result.unwrap_err() {
            AftError::FileNotFound { path: p } => {
                assert!(p.contains("absolutely_does_not_exist"));
            }
            other => panic!("expected FileNotFound, got: {:?}", other),
        }
    }

    #[test]
    fn tracked_files_lists_snapshotted_paths() {
        let path1 = temp_file("tracked1.txt", "a");
        let path2 = temp_file("tracked2.txt", "b");
        let mut store = BackupStore::new();

        assert!(store.tracked_files().is_empty());

        store.snapshot(&path1, "snap1").unwrap();
        store.snapshot(&path2, "snap2").unwrap();

        let tracked = store.tracked_files();
        assert_eq!(tracked.len(), 2);
    }
}
