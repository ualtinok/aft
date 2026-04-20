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

/// Workspace-wide, per-session checkpoint store.
///
/// Partitioned by session (issue #14): two OpenCode sessions sharing one bridge
/// can both create checkpoints named `snap1` without collision, and restoring
/// from one session does not leak the other's file set. Checkpoints are kept
/// in memory only — a bridge crash drops all of them, which is a deliberate
/// trade-off to keep this refactor bounded. Durable checkpoints are a possible
/// follow-up.
#[derive(Debug)]
pub struct CheckpointStore {
    /// session -> name -> checkpoint
    checkpoints: HashMap<String, HashMap<String, Checkpoint>>,
}

impl CheckpointStore {
    pub fn new() -> Self {
        CheckpointStore {
            checkpoints: HashMap::new(),
        }
    }

    /// Create a checkpoint by reading the given files, scoped to `session`.
    ///
    /// If `files` is empty, snapshots all tracked files for **that session**
    /// from the BackupStore (other sessions' tracked files are not visible).
    /// Overwrites any existing checkpoint with the same name in this session.
    pub fn create(
        &mut self,
        session: &str,
        name: &str,
        files: Vec<PathBuf>,
        backup_store: &BackupStore,
    ) -> Result<CheckpointInfo, AftError> {
        let file_list = if files.is_empty() {
            backup_store.tracked_files(session)
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

        self.checkpoints
            .entry(session.to_string())
            .or_default()
            .insert(name.to_string(), checkpoint);

        log::info!("checkpoint created: {} ({} files)", name, file_count);

        Ok(CheckpointInfo {
            name: name.to_string(),
            file_count,
            created_at,
        })
    }

    /// Restore a checkpoint by overwriting files with stored content.
    pub fn restore(&self, session: &str, name: &str) -> Result<CheckpointInfo, AftError> {
        let checkpoint = self.get(session, name)?;

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

    /// Restore a checkpoint using a caller-validated path list.
    pub fn restore_validated(
        &self,
        session: &str,
        name: &str,
        validated_paths: &[PathBuf],
    ) -> Result<CheckpointInfo, AftError> {
        let checkpoint = self.get(session, name)?;

        for path in validated_paths {
            let content =
                checkpoint
                    .file_contents
                    .get(path)
                    .ok_or_else(|| AftError::FileNotFound {
                        path: path.display().to_string(),
                    })?;
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
    pub fn file_paths(&self, session: &str, name: &str) -> Result<Vec<PathBuf>, AftError> {
        let checkpoint = self.get(session, name)?;
        Ok(checkpoint.file_contents.keys().cloned().collect())
    }

    /// List all checkpoints for this session with metadata.
    pub fn list(&self, session: &str) -> Vec<CheckpointInfo> {
        self.checkpoints
            .get(session)
            .map(|s| {
                s.values()
                    .map(|cp| CheckpointInfo {
                        name: cp.name.clone(),
                        file_count: cp.file_contents.len(),
                        created_at: cp.created_at,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Total checkpoint count across all sessions (for `/aft-status`).
    pub fn total_count(&self) -> usize {
        self.checkpoints.values().map(|s| s.len()).sum()
    }

    /// Remove checkpoints older than `ttl_hours` across all sessions.
    /// Empty session entries are pruned after cleanup.
    pub fn cleanup(&mut self, ttl_hours: u32) {
        let now = current_timestamp();
        let ttl_secs = ttl_hours as u64 * 3600;
        self.checkpoints.retain(|_, session_cps| {
            session_cps.retain(|_, cp| now.saturating_sub(cp.created_at) < ttl_secs);
            !session_cps.is_empty()
        });
    }

    fn get(&self, session: &str, name: &str) -> Result<&Checkpoint, AftError> {
        self.checkpoints
            .get(session)
            .and_then(|s| s.get(name))
            .ok_or_else(|| AftError::CheckpointNotFound {
                name: name.to_string(),
            })
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
    use crate::protocol::DEFAULT_SESSION_ID;
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
            .create(
                DEFAULT_SESSION_ID,
                "snap1",
                vec![path1.clone(), path2.clone()],
                &backup_store,
            )
            .unwrap();
        assert_eq!(info.name, "snap1");
        assert_eq!(info.file_count, 2);

        // Modify files
        fs::write(&path1, "changed1").unwrap();
        fs::write(&path2, "changed2").unwrap();

        // Restore
        let info = store.restore(DEFAULT_SESSION_ID, "snap1").unwrap();
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
            .create(DEFAULT_SESSION_ID, "dup", vec![path.clone()], &backup_store)
            .unwrap();
        fs::write(&path, "v2").unwrap();
        store
            .create(DEFAULT_SESSION_ID, "dup", vec![path.clone()], &backup_store)
            .unwrap();

        // Restore should give v2 (the overwritten checkpoint)
        fs::write(&path, "v3").unwrap();
        store.restore(DEFAULT_SESSION_ID, "dup").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
    }

    #[test]
    fn list_returns_metadata_scoped_to_session() {
        let path = temp_file("cp_list.txt", "data");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        store
            .create(DEFAULT_SESSION_ID, "a", vec![path.clone()], &backup_store)
            .unwrap();
        store
            .create(DEFAULT_SESSION_ID, "b", vec![path.clone()], &backup_store)
            .unwrap();
        store
            .create("other_session", "c", vec![path.clone()], &backup_store)
            .unwrap();

        let default_list = store.list(DEFAULT_SESSION_ID);
        assert_eq!(default_list.len(), 2);
        let names: Vec<&str> = default_list.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));

        let other_list = store.list("other_session");
        assert_eq!(other_list.len(), 1);
        assert_eq!(other_list[0].name, "c");
    }

    #[test]
    fn sessions_isolate_checkpoint_names() {
        // Same checkpoint name in two sessions does not collide on restore.
        let path_a = temp_file("cp_isolated_a.txt", "a-original");
        let path_b = temp_file("cp_isolated_b.txt", "b-original");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        // Both sessions create a checkpoint with the same name but different files.
        store
            .create("session_a", "snap", vec![path_a.clone()], &backup_store)
            .unwrap();
        store
            .create("session_b", "snap", vec![path_b.clone()], &backup_store)
            .unwrap();

        fs::write(&path_a, "a-modified").unwrap();
        fs::write(&path_b, "b-modified").unwrap();

        // Restoring session A's "snap" only touches path_a.
        store.restore("session_a", "snap").unwrap();
        assert_eq!(fs::read_to_string(&path_a).unwrap(), "a-original");
        assert_eq!(fs::read_to_string(&path_b).unwrap(), "b-modified");

        // Restoring session B's "snap" only touches path_b.
        fs::write(&path_a, "a-modified").unwrap();
        store.restore("session_b", "snap").unwrap();
        assert_eq!(fs::read_to_string(&path_a).unwrap(), "a-modified");
        assert_eq!(fs::read_to_string(&path_b).unwrap(), "b-original");
    }

    #[test]
    fn cleanup_removes_expired_across_sessions() {
        let path = temp_file("cp_cleanup.txt", "data");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();

        store
            .create(
                DEFAULT_SESSION_ID,
                "recent",
                vec![path.clone()],
                &backup_store,
            )
            .unwrap();

        // Manually insert an expired checkpoint in another session.
        store
            .checkpoints
            .entry("other".to_string())
            .or_default()
            .insert(
                "old".to_string(),
                Checkpoint {
                    name: "old".to_string(),
                    file_contents: HashMap::new(),
                    created_at: 1000, // far in the past
                },
            );

        assert_eq!(store.total_count(), 2);
        store.cleanup(24); // 24 hours
        assert_eq!(store.total_count(), 1);
        assert_eq!(store.list(DEFAULT_SESSION_ID)[0].name, "recent");
        assert!(store.list("other").is_empty());
    }

    #[test]
    fn restore_nonexistent_returns_error() {
        let store = CheckpointStore::new();
        let result = store.restore(DEFAULT_SESSION_ID, "nope");
        assert!(result.is_err());
        match result.unwrap_err() {
            AftError::CheckpointNotFound { name } => {
                assert_eq!(name, "nope");
            }
            other => panic!("expected CheckpointNotFound, got: {:?}", other),
        }
    }

    #[test]
    fn restore_nonexistent_in_other_session_returns_error() {
        // A "snap" that exists in session A must NOT be visible from session B.
        let path = temp_file("cp_cross_session.txt", "data");
        let backup_store = BackupStore::new();
        let mut store = CheckpointStore::new();
        store
            .create("session_a", "only_a", vec![path], &backup_store)
            .unwrap();
        assert!(store.restore("session_b", "only_a").is_err());
    }

    #[test]
    fn create_with_empty_files_uses_backup_tracked() {
        let path = temp_file("cp_tracked.txt", "tracked_content");
        let mut backup_store = BackupStore::new();
        backup_store
            .snapshot(DEFAULT_SESSION_ID, &path, "auto")
            .unwrap();

        let mut store = CheckpointStore::new();
        let info = store
            .create(DEFAULT_SESSION_ID, "from_tracked", vec![], &backup_store)
            .unwrap();
        assert!(info.file_count >= 1);

        // Modify and restore
        fs::write(&path, "modified").unwrap();
        store.restore(DEFAULT_SESSION_ID, "from_tracked").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "tracked_content");
    }
}
