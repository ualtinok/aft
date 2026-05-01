use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backup::hash_session;

use super::BgTaskStatus;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct TaskPaths {
    pub dir: PathBuf,
    pub json: PathBuf,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub exit: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedTask {
    pub schema_version: u32,
    pub task_id: String,
    pub session_id: String,
    pub command: String,
    pub workdir: PathBuf,
    pub status: BgTaskStatus,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub duration_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub child_pid: Option<u32>,
    pub pgid: Option<i32>,
    pub completion_delivered: bool,
    pub status_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitMarker {
    Code(i32),
    Killed,
}

impl PersistedTask {
    pub fn starting(
        task_id: String,
        session_id: String,
        command: String,
        workdir: PathBuf,
        timeout_ms: Option<u64>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            task_id,
            session_id,
            command,
            workdir,
            status: BgTaskStatus::Starting,
            started_at: unix_millis(),
            finished_at: None,
            duration_ms: None,
            timeout_ms,
            exit_code: None,
            child_pid: None,
            pgid: None,
            completion_delivered: true,
            status_reason: None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    pub fn mark_running(&mut self, child_pid: u32, pgid: i32) {
        self.status = BgTaskStatus::Running;
        self.child_pid = Some(child_pid);
        self.pgid = Some(pgid);
    }

    pub fn mark_terminal(
        &mut self,
        status: BgTaskStatus,
        exit_code: Option<i32>,
        reason: Option<String>,
    ) {
        let finished_at = unix_millis();
        self.status = status;
        self.exit_code = exit_code;
        self.finished_at = Some(finished_at);
        self.duration_ms = Some(finished_at.saturating_sub(self.started_at));
        self.child_pid = None;
        self.status_reason = reason;
        self.completion_delivered = false;
    }
}

pub fn session_tasks_dir(storage_dir: &Path, session_id: &str) -> PathBuf {
    storage_dir
        .join("bash-tasks")
        .join(hash_session(session_id))
}

pub fn task_paths(storage_dir: &Path, session_id: &str, task_id: &str) -> TaskPaths {
    let dir = session_tasks_dir(storage_dir, session_id);
    TaskPaths {
        json: dir.join(format!("{task_id}.json")),
        stdout: dir.join(format!("{task_id}.stdout")),
        stderr: dir.join(format!("{task_id}.stderr")),
        exit: dir.join(format!("{task_id}.exit")),
        dir,
    }
}

pub fn read_task(path: &Path) -> io::Result<PersistedTask> {
    let content = fs::read_to_string(path)?;
    serde_json::from_str(&content).map_err(io::Error::other)
}

pub fn write_task(path: &Path, task: &PersistedTask) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_vec_pretty(task).map_err(io::Error::other)?;
    atomic_write(path, &content, &task.task_id)
}

pub fn update_task<F>(path: &Path, update: F) -> io::Result<PersistedTask>
where
    F: FnOnce(&mut PersistedTask),
{
    let mut task = read_task(path)?;
    let original_terminal = task.is_terminal();
    let original = task.clone();
    update(&mut task);
    if original_terminal {
        let completion_delivered = task.completion_delivered;
        task = original;
        task.completion_delivered = completion_delivered;
    }
    write_task(path, &task)?;
    Ok(task)
}

pub fn write_kill_marker_if_absent(path: &Path) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    atomic_write(path, b"killed", "kill")
}

pub fn read_exit_marker(path: &Path) -> io::Result<Option<ExitMarker>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    let content = content.trim();
    if content.is_empty() {
        return Ok(None);
    }
    if content == "killed" {
        return Ok(Some(ExitMarker::Killed));
    }
    match content.parse::<i32>() {
        Ok(code) => Ok(Some(ExitMarker::Code(code))),
        Err(_) => Ok(None),
    }
}

pub fn atomic_write(path: &Path, content: &[u8], task_id: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("task");
    let tmp = parent.join(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        sanitize_task_id(task_id)
    ));
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn sanitize_task_id(task_id: &str) -> String {
    task_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    #[test]
    fn atomic_write_temp_names_include_task_id() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("task.json");

        let left_path = path.clone();
        let left = thread::spawn(move || atomic_write(&left_path, b"left", "task-left"));
        let right_path = path.clone();
        let right = thread::spawn(move || atomic_write(&right_path, b"right", "task-right"));

        left.join().expect("join left").expect("write left");
        right.join().expect("join right").expect("write right");

        let content = fs::read_to_string(&path).expect("read final content");
        assert!(content == "left" || content == "right");
        assert!(!dir
            .path()
            .join(format!(".task.json.tmp.{}", std::process::id()))
            .exists());
    }
}

pub fn create_capture_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    File::create(path)
}

pub fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
