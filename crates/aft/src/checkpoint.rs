use std::collections::HashMap;
use std::path::PathBuf;

use crate::backup::BackupStore;
use crate::error::AftError;

/// Metadata about a checkpoint, returned by list/create/restore.
#[derive(Debug, Clone)]
pub struct CheckpointInfo {
    pub name: String,
    pub file_count: usize,
    pub created_at: u64,
}

/// A stored checkpoint: a snapshot of multiple file contents.
#[derive(Debug, Clone)]
struct Checkpoint {
    name: String,
    file_contents: HashMap<PathBuf, String>,
    created_at: u64,
}

/// Workspace-wide checkpoint store.
///
/// Stores named snapshots of file contents. On `create`, reads the listed files
/// (or all tracked files from a BackupStore if the list is empty). On `restore`,
/// overwrites files with stored content. Checkpoints can be cleaned up by TTL.
#[derive(Debug)]
pub struct CheckpointStore {
    checkpoints: HashMap<String, Checkpoint>,
}

impl CheckpointStore {
    pub fn new() -> Self {
        CheckpointStore {
            checkpoints: HashMap::new(),
        }
    }

    /// Create a checkpoint by reading the given files.
    ///
    /// If `files` is empty, snapshots all tracked files from the BackupStore.
    /// Overwrites any existing checkpoint with the same name.
    pub fn create(
        &mut self,
        name: &str,
        files: Vec<PathBuf>,
        backup_store: &BackupStore,
    ) -> Result<CheckpointInfo, AftError> {
        let file_list = if files.is_empty() {
            backup_store.tracked_files()
        } else {
            files
        };

        let mut file_contents = HashMap::new();
        for path in &file_list {
            let content = std::fs::read_to_string(path).map_err(|_| AftError::FileNotFound {
                path: path.display().to_string(),
            })?;
            file_contents.insert(path.clone(), content);
        }

        let created_at = current_timestamp();
        let file_count = file_contents.len();

        let checkpoint = Checkpoint {
            name: name.to_string(),
            file_contents,
            created_at,
        };

        self.checkpoints.insert(name.to_string(), checkpoint);

        log::info!("checkpoint created: {} ({} files)", name, file_count);

        Ok(CheckpointInfo {
            name: name.to_string(),
            file_count,
            created_at,
        })
    }

    /// Restore a checkpoint by overwriting files with stored content.
    pub fn restore(&self, name: &str) -> Result<CheckpointInfo, AftError> {
        let checkpoint =
            self.checkpoints
                .get(name)
                .ok_or_else(|| AftError::CheckpointNotFound {
                    name: name.to_string(),
                })?;

        for (path, content) in &checkpoint.file_contents {
            std::fs::write(path, content).map_err(|_| AftError::FileNotFound {
                path: path.display().to_string(),
            })?;
        }

        log::info!("checkpoint restored: {}", name);

        Ok(CheckpointInfo {
            name: checkpoint.name.clone(),
            file_count: checkpoint.file_contents.len(),
            created_at: checkpoint.created_at,
        })
    }

    /// Return the file paths stored for a checkpoint.
    pub fn file_paths(&self, name: &str) -> Result<Vec<PathBuf>, AftError> {
        let checkpoint =
            self.checkpoints
                .get(name)
                .ok_or_else(|| AftError::CheckpointNotFound {
                    name: name.to_string(),
                })?;

        Ok(checkpoint.file_contents.keys().cloned().collect())
    }

    /// List all checkpoints with metadata.
    pub fn list(&self) -> Vec<CheckpointInfo> {
        self.checkpoints
            .values()
            .map(|cp| CheckpointInfo {
                name: cp.name.clone(),
                file_count: cp.file_contents.len(),
                created_at: cp.created_at,
            })
            .collect()
    }

    /// Remove checkpoints older than `ttl_hours`.
    pub fn cleanup(&mut self, ttl_hours: u32) {
        let now = current_timestamp();
        let ttl_secs = ttl_hours as u64 * 3600;
        self.checkpoints
            .retain(|_, cp| now.saturating_sub(cp.created_at) < ttl_secs);
    }
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
        let dir = std::env::temp_dir().join("aft_checkpoint_tests");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn create_and_restore_round_trip() {
        let path1 = temp_file("cp_rt1.txt", "hello");
        let path2 = temp_file("cp_rt2.txt", "world");

        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        let info = store
            .create("snap1", vec![path1.clone(), path2.clone()], &backup_store)
            .unwrap();
        assert_eq!(info.name, "snap1");
        assert_eq!(info.file_count, 2);

        // Modify files
        fs::write(&path1, "changed1").unwrap();
        fs::write(&path2, "changed2").unwrap();

        // Restore
        let info = store.restore("snap1").unwrap();
        assert_eq!(info.file_count, 2);
        assert_eq!(fs::read_to_string(&path1).unwrap(), "hello");
        assert_eq!(fs::read_to_string(&path2).unwrap(), "world");
    }

    #[test]
    fn overwrite_existing_name() {
        let path = temp_file("cp_overwrite.txt", "v1");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        store
            .create("dup", vec![path.clone()], &backup_store)
            .unwrap();
        fs::write(&path, "v2").unwrap();
        store
            .create("dup", vec![path.clone()], &backup_store)
            .unwrap();

        // Restore should give v2 (the overwritten checkpoint)
        fs::write(&path, "v3").unwrap();
        store.restore("dup").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
    }

    #[test]
    fn list_returns_metadata() {
        let path = temp_file("cp_list.txt", "data");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        store
            .create("a", vec![path.clone()], &backup_store)
            .unwrap();
        store
            .create("b", vec![path.clone()], &backup_store)
            .unwrap();

        let list = store.list();
        assert_eq!(list.len(), 2);
        let names: Vec<&str> = list.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn cleanup_removes_expired() {
        let path = temp_file("cp_cleanup.txt", "data");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        store
            .create("recent", vec![path.clone()], &backup_store)
            .unwrap();

        // Manually insert an expired checkpoint
        store.checkpoints.insert(
            "old".to_string(),
            Checkpoint {
                name: "old".to_string(),
                file_contents: HashMap::new(),
                created_at: 1000, // far in the past
            },
        );

        assert_eq!(store.list().len(), 2);
        store.cleanup(24); // 24 hours
        let remaining = store.list();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].name, "recent");
    }

    #[test]
    fn restore_nonexistent_returns_error() {
        let store = CheckpointStore::new();
        let result = store.restore("nope");
        assert!(result.is_err());
        match result.unwrap_err() {
            AftError::CheckpointNotFound { name } => {
                assert_eq!(name, "nope");
            }
            other => panic!("expected CheckpointNotFound, got: {:?}", other),
        }
    }

    #[test]
    fn create_with_empty_files_uses_backup_tracked() {
        let path = temp_file("cp_tracked.txt", "tracked_content");
        let mut backup_store = BackupStore::new();
        backup_store.snapshot(&path, "auto").unwrap();

        let mut store = CheckpointStore::new();
        let info = store.create("from_tracked", vec![], &backup_store).unwrap();
        assert!(info.file_count >= 1);

        // Modify and restore
        fs::write(&path, "modified").unwrap();
        store.restore("from_tracked").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "tracked_content");
    }
}
