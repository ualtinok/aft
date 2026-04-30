use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use super::buffer::BgBuffer;
use super::persistence::{
    create_capture_file, read_exit_marker, read_task, session_tasks_dir, task_paths, unix_millis,
    update_task, write_kill_marker_if_absent, write_task, ExitMarker, PersistedTask, TaskPaths,
};
#[cfg(unix)]
use super::process::terminate_pgid;
use super::{BgTaskInfo, BgTaskStatus};

/// Default timeout for background bash tasks: 30 minutes.
/// Agents can override per-call via the `timeout` parameter (in ms).
const DEFAULT_BG_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STALE_RUNNING_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct BgCompletion {
    pub task_id: String,
    #[serde(skip_serializing)]
    pub session_id: String,
    pub status: BgTaskStatus,
    pub exit_code: Option<i32>,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BgTaskSnapshot {
    #[serde(flatten)]
    pub info: BgTaskInfo,
    pub exit_code: Option<i32>,
    pub child_pid: Option<u32>,
    pub workdir: String,
    pub output_preview: String,
    pub output_truncated: bool,
    pub output_path: Option<String>,
}

#[derive(Clone)]
pub struct BgTaskRegistry {
    pub(crate) inner: Arc<RegistryInner>,
}

pub(crate) struct RegistryInner {
    pub(crate) tasks: Mutex<HashMap<String, Arc<BgTask>>>,
    pub(crate) completions: Mutex<VecDeque<BgCompletion>>,
    watchdog_started: AtomicBool,
    pub(crate) shutdown: AtomicBool,
}

pub(crate) struct BgTask {
    pub(crate) task_id: String,
    pub(crate) session_id: String,
    pub(crate) paths: TaskPaths,
    pub(crate) started: Instant,
    pub(crate) state: Mutex<BgTaskState>,
}

pub(crate) struct BgTaskState {
    pub(crate) metadata: PersistedTask,
    pub(crate) child: Option<Child>,
    pub(crate) detached: bool,
    pub(crate) buffer: BgBuffer,
}

impl BgTaskRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                tasks: Mutex::new(HashMap::new()),
                completions: Mutex::new(VecDeque::new()),
                watchdog_started: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
            }),
        }
    }

    #[cfg(unix)]
    pub fn spawn(
        &self,
        command: &str,
        session_id: String,
        workdir: PathBuf,
        env: HashMap<String, String>,
        timeout: Option<Duration>,
        storage_dir: PathBuf,
        max_running: usize,
    ) -> Result<String, String> {
        self.start_watchdog();

        let running = self.running_count();
        if running >= max_running {
            return Err(format!(
                "background bash task limit exceeded: {running} running (max {max_running})"
            ));
        }

        let timeout = timeout.or(Some(DEFAULT_BG_TIMEOUT));
        let timeout_ms = timeout.map(|timeout| timeout.as_millis() as u64);
        let task_id = self.generate_unique_task_id()?;
        let paths = task_paths(&storage_dir, &session_id, &task_id);
        fs::create_dir_all(&paths.dir)
            .map_err(|e| format!("failed to create background task dir: {e}"))?;

        let mut metadata = PersistedTask::starting(
            task_id.clone(),
            session_id.clone(),
            command.to_string(),
            workdir.clone(),
            timeout_ms,
        );
        write_task(&paths.json, &metadata)
            .map_err(|e| format!("failed to persist background task metadata: {e}"))?;

        let stdout = create_capture_file(&paths.stdout)
            .map_err(|e| format!("failed to create stdout capture file: {e}"))?;
        let stderr = create_capture_file(&paths.stderr)
            .map_err(|e| format!("failed to create stderr capture file: {e}"))?;

        let child = detached_shell_command(command, &paths.exit)
            .current_dir(&workdir)
            .envs(&env)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|e| format!("failed to spawn background bash command: {e}"))?;

        let child_pid = child.id();
        metadata.mark_running(child_pid, child_pid as i32);
        write_task(&paths.json, &metadata)
            .map_err(|e| format!("failed to persist running background task metadata: {e}"))?;

        let task = Arc::new(BgTask {
            task_id: task_id.clone(),
            session_id,
            paths: paths.clone(),
            started: Instant::now(),
            state: Mutex::new(BgTaskState {
                metadata,
                child: Some(child),
                detached: false,
                buffer: BgBuffer::new(paths.stdout.clone(), paths.stderr.clone()),
            }),
        });

        self.inner
            .tasks
            .lock()
            .map_err(|_| "background task registry lock poisoned".to_string())?
            .insert(task_id.clone(), task);

        Ok(task_id)
    }

    #[cfg(windows)]
    pub fn spawn(
        &self,
        _command: &str,
        _session_id: String,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _timeout: Option<Duration>,
        _storage_dir: PathBuf,
        _max_running: usize,
    ) -> Result<String, String> {
        Err("background bash is not yet supported on Windows".to_string())
    }

    pub fn replay_session(&self, storage_dir: &Path, session_id: &str) -> Result<(), String> {
        self.start_watchdog();
        let dir = session_tasks_dir(storage_dir, session_id);
        if !dir.exists() {
            return Ok(());
        }

        let entries = fs::read_dir(&dir)
            .map_err(|e| format!("failed to read background task dir {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Ok(mut metadata) = read_task(&path) else {
                continue;
            };
            if metadata.session_id != session_id {
                continue;
            }

            let paths = task_paths(storage_dir, session_id, &metadata.task_id);
            match metadata.status {
                BgTaskStatus::Starting => {
                    metadata.mark_terminal(
                        BgTaskStatus::Failed,
                        None,
                        Some("spawn aborted".to_string()),
                    );
                    let _ = write_task(&paths.json, &metadata);
                    self.enqueue_completion_if_needed(&metadata);
                }
                BgTaskStatus::Running => {
                    if self.running_metadata_is_stale(&metadata) {
                        metadata.mark_terminal(
                            BgTaskStatus::Killed,
                            None,
                            Some("orphaned (>24h)".to_string()),
                        );
                        if !paths.exit.exists() {
                            let _ = write_kill_marker_if_absent(&paths.exit);
                        }
                        let _ = write_task(&paths.json, &metadata);
                        self.enqueue_completion_if_needed(&metadata);
                    } else if let Ok(Some(marker)) = read_exit_marker(&paths.exit) {
                        metadata = terminal_metadata_from_marker(metadata, marker, None);
                        let _ = write_task(&paths.json, &metadata);
                        self.enqueue_completion_if_needed(&metadata);
                    } else {
                        self.insert_rehydrated_task(metadata, paths, true)?;
                    }
                }
                _ if metadata.status.is_terminal() => {
                    self.insert_rehydrated_task(metadata.clone(), paths, true)?;
                    self.enqueue_completion_if_needed(&metadata);
                }
                _ => {}
            }
        }

        Ok(())
    }

    pub fn status(
        &self,
        task_id: &str,
        session_id: &str,
        preview_bytes: usize,
    ) -> Option<BgTaskSnapshot> {
        let task = self.task_for_session(task_id, session_id)?;
        let _ = self.poll_task(&task);
        Some(task.snapshot(preview_bytes))
    }

    pub fn list(&self, preview_bytes: usize) -> Vec<BgTaskSnapshot> {
        let tasks = self
            .inner
            .tasks
            .lock()
            .map(|tasks| tasks.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        tasks
            .into_iter()
            .map(|task| {
                let _ = self.poll_task(&task);
                task.snapshot(preview_bytes)
            })
            .collect()
    }

    pub fn kill(&self, task_id: &str, session_id: &str) -> Result<BgTaskSnapshot, String> {
        self.kill_with_status(task_id, session_id, BgTaskStatus::Killed)
    }

    pub(crate) fn kill_for_timeout(&self, task_id: &str, session_id: &str) -> Result<(), String> {
        self.kill_with_status(task_id, session_id, BgTaskStatus::TimedOut)
            .map(|_| ())
    }

    pub fn cleanup_finished(&self, _older_than: Duration) {}

    pub fn drain_completions(&self) -> Vec<BgCompletion> {
        self.drain_completions_for_session(None)
    }

    pub fn drain_completions_for_session(&self, session_id: Option<&str>) -> Vec<BgCompletion> {
        let mut completions = match self.inner.completions.lock() {
            Ok(completions) => completions,
            Err(_) => return Vec::new(),
        };

        let drained = if let Some(session_id) = session_id {
            let mut matched = Vec::new();
            let mut retained = VecDeque::new();
            while let Some(completion) = completions.pop_front() {
                if completion.session_id == session_id {
                    matched.push(completion);
                } else {
                    retained.push_back(completion);
                }
            }
            *completions = retained;
            matched
        } else {
            completions.drain(..).collect()
        };
        drop(completions);

        for completion in &drained {
            if let Some(task) = self.task_for_session(&completion.task_id, &completion.session_id) {
                let _ = task.set_completion_delivered(true);
            }
        }

        drained
    }

    pub fn pending_completions_for_session(&self, session_id: &str) -> Vec<BgCompletion> {
        self.inner
            .completions
            .lock()
            .map(|completions| {
                completions
                    .iter()
                    .filter(|completion| completion.session_id == session_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn detach(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
        if let Ok(mut tasks) = self.inner.tasks.lock() {
            for task in tasks.values() {
                if let Ok(mut state) = task.state.lock() {
                    state.child = None;
                    state.detached = true;
                }
            }
            tasks.clear();
        }
    }

    pub fn shutdown(&self) {
        let tasks = self
            .inner
            .tasks
            .lock()
            .map(|tasks| {
                tasks
                    .values()
                    .map(|task| (task.task_id.clone(), task.session_id.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (task_id, session_id) in tasks {
            let _ = self.kill(&task_id, &session_id);
        }
    }

    pub(crate) fn poll_task(&self, task: &Arc<BgTask>) -> Result<(), String> {
        let marker = match read_exit_marker(&task.paths.exit) {
            Ok(Some(marker)) => marker,
            Ok(None) => return Ok(()),
            Err(error) => return Err(format!("failed to read exit marker: {error}")),
        };
        self.finalize_from_marker(task, marker, None)
    }

    pub(crate) fn reap_child(&self, task: &Arc<BgTask>) {
        let Ok(mut state) = task.state.lock() else {
            return;
        };
        if let Some(child) = state.child.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                state.child = None;
                state.detached = true;
            }
        }
    }

    pub(crate) fn running_tasks(&self) -> Vec<Arc<BgTask>> {
        self.inner
            .tasks
            .lock()
            .map(|tasks| {
                tasks
                    .values()
                    .filter(|task| task.is_running())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn insert_rehydrated_task(
        &self,
        metadata: PersistedTask,
        paths: TaskPaths,
        detached: bool,
    ) -> Result<(), String> {
        let task_id = metadata.task_id.clone();
        let session_id = metadata.session_id.clone();
        let task = Arc::new(BgTask {
            task_id: task_id.clone(),
            session_id,
            paths: paths.clone(),
            started: Instant::now(),
            state: Mutex::new(BgTaskState {
                metadata,
                child: None,
                detached,
                buffer: BgBuffer::new(paths.stdout.clone(), paths.stderr.clone()),
            }),
        });
        self.inner
            .tasks
            .lock()
            .map_err(|_| "background task registry lock poisoned".to_string())?
            .insert(task_id, task);
        Ok(())
    }

    fn kill_with_status(
        &self,
        task_id: &str,
        session_id: &str,
        terminal_status: BgTaskStatus,
    ) -> Result<BgTaskSnapshot, String> {
        let task = self
            .task_for_session(task_id, session_id)
            .ok_or_else(|| format!("background task not found: {task_id}"))?;

        {
            let mut state = task
                .state
                .lock()
                .map_err(|_| "background task lock poisoned".to_string())?;
            if state.metadata.status.is_terminal() {
                return Ok(task.snapshot_locked(&state, 5 * 1024));
            }

            state.metadata.status = BgTaskStatus::Killing;
            write_task(&task.paths.json, &state.metadata)
                .map_err(|e| format!("failed to persist killing state: {e}"))?;

            #[cfg(unix)]
            if let Some(pgid) = state.metadata.pgid {
                terminate_pgid(pgid, state.child.as_mut());
            }
            if let Some(child) = state.child.as_mut() {
                let _ = child.wait();
            }
            state.child = None;
            state.detached = true;

            if !task.paths.exit.exists() {
                write_kill_marker_if_absent(&task.paths.exit)
                    .map_err(|e| format!("failed to write kill marker: {e}"))?;
            }

            let exit_code = if terminal_status == BgTaskStatus::TimedOut {
                Some(124)
            } else {
                None
            };
            state
                .metadata
                .mark_terminal(terminal_status, exit_code, None);
            write_task(&task.paths.json, &state.metadata)
                .map_err(|e| format!("failed to persist killed state: {e}"))?;
            state.buffer.enforce_terminal_cap();
            self.enqueue_completion_locked(&state.metadata);
        }

        Ok(task.snapshot(5 * 1024))
    }

    fn finalize_from_marker(
        &self,
        task: &Arc<BgTask>,
        marker: ExitMarker,
        reason: Option<String>,
    ) -> Result<(), String> {
        let mut state = task
            .state
            .lock()
            .map_err(|_| "background task lock poisoned".to_string())?;
        if state.metadata.status.is_terminal() {
            return Ok(());
        }

        let updated = update_task(&task.paths.json, |metadata| {
            let new_metadata = terminal_metadata_from_marker(metadata.clone(), marker, reason);
            *metadata = new_metadata;
        })
        .map_err(|e| format!("failed to persist terminal state: {e}"))?;
        state.metadata = updated;
        state.child = None;
        state.detached = true;
        state.buffer.enforce_terminal_cap();
        self.enqueue_completion_locked(&state.metadata);
        Ok(())
    }

    fn enqueue_completion_if_needed(&self, metadata: &PersistedTask) {
        if metadata.status.is_terminal() && !metadata.completion_delivered {
            self.enqueue_completion_locked(metadata);
        }
    }

    fn enqueue_completion_locked(&self, metadata: &PersistedTask) {
        if !metadata.status.is_terminal() || metadata.completion_delivered {
            return;
        }
        if let Ok(mut completions) = self.inner.completions.lock() {
            if completions
                .iter()
                .any(|completion| completion.task_id == metadata.task_id)
            {
                return;
            }
            completions.push_back(BgCompletion {
                task_id: metadata.task_id.clone(),
                session_id: metadata.session_id.clone(),
                status: metadata.status.clone(),
                exit_code: metadata.exit_code,
                command: metadata.command.clone(),
            });
        }
    }

    fn task(&self, task_id: &str) -> Option<Arc<BgTask>> {
        self.inner
            .tasks
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(task_id).cloned())
    }

    fn task_for_session(&self, task_id: &str, session_id: &str) -> Option<Arc<BgTask>> {
        self.task(task_id)
            .filter(|task| task.session_id == session_id)
    }

    fn running_count(&self) -> usize {
        self.inner
            .tasks
            .lock()
            .map(|tasks| tasks.values().filter(|task| task.is_running()).count())
            .unwrap_or(0)
    }

    fn start_watchdog(&self) {
        if !self.inner.watchdog_started.swap(true, Ordering::SeqCst) {
            super::watchdog::start(self.clone());
        }
    }

    fn running_metadata_is_stale(&self, metadata: &PersistedTask) -> bool {
        unix_millis().saturating_sub(metadata.started_at) > STALE_RUNNING_AFTER.as_millis() as u64
    }

    #[cfg(test)]
    pub fn task_json_path(&self, task_id: &str, session_id: &str) -> Option<PathBuf> {
        self.task_for_session(task_id, session_id)
            .map(|task| task.paths.json.clone())
    }

    #[cfg(test)]
    pub fn task_exit_path(&self, task_id: &str, session_id: &str) -> Option<PathBuf> {
        self.task_for_session(task_id, session_id)
            .map(|task| task.paths.exit.clone())
    }

    /// Generate a `bgb-{8hex}` slug that is unique against live tasks and queued completions.
    fn generate_unique_task_id(&self) -> Result<String, String> {
        for _ in 0..32 {
            let candidate = random_slug();
            let tasks = self
                .inner
                .tasks
                .lock()
                .map_err(|_| "background task registry lock poisoned".to_string())?;
            if tasks.contains_key(&candidate) {
                continue;
            }
            let completions = self
                .inner
                .completions
                .lock()
                .map_err(|_| "background completions lock poisoned".to_string())?;
            if completions
                .iter()
                .any(|completion| completion.task_id == candidate)
            {
                continue;
            }
            return Ok(candidate);
        }
        Err("failed to allocate unique background task id after 32 attempts".to_string())
    }
}

impl Default for BgTaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BgTask {
    fn snapshot(&self, preview_bytes: usize) -> BgTaskSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        self.snapshot_locked(&state, preview_bytes)
    }

    fn snapshot_locked(&self, state: &BgTaskState, preview_bytes: usize) -> BgTaskSnapshot {
        let metadata = &state.metadata;
        let duration_ms = metadata.duration_ms.or_else(|| {
            metadata
                .status
                .is_terminal()
                .then(|| self.started.elapsed().as_millis() as u64)
        });
        let (output_preview, output_truncated) = state.buffer.read_tail(preview_bytes);
        BgTaskSnapshot {
            info: BgTaskInfo {
                task_id: self.task_id.clone(),
                status: metadata.status.clone(),
                command: metadata.command.clone(),
                started_at: metadata.started_at,
                duration_ms,
            },
            exit_code: metadata.exit_code,
            child_pid: metadata.child_pid,
            workdir: metadata.workdir.display().to_string(),
            output_preview,
            output_truncated,
            output_path: state
                .buffer
                .output_path()
                .map(|path| path.display().to_string()),
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.metadata.status == BgTaskStatus::Running)
            .unwrap_or(false)
    }

    fn set_completion_delivered(&self, delivered: bool) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "background task lock poisoned".to_string())?;
        let updated = update_task(&self.paths.json, |metadata| {
            metadata.completion_delivered = delivered;
        })
        .map_err(|e| format!("failed to update completion delivery: {e}"))?;
        state.metadata = updated;
        Ok(())
    }
}

fn terminal_metadata_from_marker(
    mut metadata: PersistedTask,
    marker: ExitMarker,
    reason: Option<String>,
) -> PersistedTask {
    match marker {
        ExitMarker::Code(code) => {
            let status = if code == 0 {
                BgTaskStatus::Completed
            } else {
                BgTaskStatus::Failed
            };
            metadata.mark_terminal(status, Some(code), reason);
        }
        ExitMarker::Killed => metadata.mark_terminal(BgTaskStatus::Killed, None, reason),
    }
    metadata
}

#[cfg(unix)]
fn detached_shell_command(command: &str, exit_path: &Path) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("\"$0\" -c \"$1\"; code=$?; printf \"%s\" \"$code\" > \"$2.tmp.$$\"; mv -f \"$2.tmp.$$\" \"$2\"")
        .arg("/bin/sh")
        .arg(command)
        .arg(exit_path);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd
}

fn random_slug() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mixed = unix_millis_nanos()
        ^ (std::process::id() as u128).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (counter as u128).wrapping_mul(0xBF58476D1CE4E5B9);
    format!("bgb-{:08x}", (mixed as u32))
}

fn unix_millis_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
