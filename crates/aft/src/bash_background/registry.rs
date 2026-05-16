use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::context::SharedProgressSender;
use crate::protocol::{BashCompletedFrame, BashLongRunningFrame, PushFrame};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

use super::buffer::BgBuffer;
use super::persistence::{
    create_capture_file, delete_task_bundle, read_exit_marker, read_task, session_tasks_dir,
    task_paths, unix_millis, update_task, write_kill_marker_if_absent, write_task, ExitMarker,
    PersistedTask, TaskPaths,
};
use super::process::is_process_alive;
#[cfg(unix)]
use super::process::terminate_pgid;
#[cfg(windows)]
use super::process::terminate_pid;
use super::{BgTaskInfo, BgTaskStatus};
// Note: `resolve_windows_shell` is no longer imported at module scope —
// production code in `spawn_detached_child` uses `shell_candidates()`
// with retry instead, and the function remains in `windows_shell.rs`
// for tests and as a future helper.

/// Default timeout for background bash tasks: 30 minutes.
/// Agents can override per-call via the `timeout` parameter (in ms).
const DEFAULT_BG_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const STALE_RUNNING_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const PERSISTED_GC_GRACE: Duration = Duration::from_secs(24 * 60 * 60);
const QUARANTINE_GC_GRACE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Tail-bytes captured into BashCompletedFrame and BgCompletion records so the
/// plugin can inline a preview into the system-reminder. Sized for ~3-4 lines
/// of typical command output (git status, test results, exit messages) — short
/// enough that round-tripping multiple completions in one reminder stays well
/// under the model's context budget but long enough that most successful runs
/// don't need a follow-up `bash_status` call.
const BG_COMPLETION_PREVIEW_BYTES: usize = 300;

#[derive(Debug, Clone, Serialize)]
pub struct BgCompletion {
    pub task_id: String,
    /// Intentionally omitted from serialized completion payloads: push frames
    /// carry `session_id` at the BashCompletedFrame envelope level for routing.
    #[serde(skip_serializing)]
    pub session_id: String,
    pub status: BgTaskStatus,
    pub exit_code: Option<i32>,
    pub command: String,
    /// Tail of stdout+stderr (≤300 bytes) at completion time, read once and
    /// cached so push-frame consumers and `bash_drain_completions` callers see
    /// the same preview without racing against later output rotation. Empty
    /// when not captured (e.g., persisted task seen on startup before buffer
    /// reattachment).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_preview: String,
    /// True when the captured tail is shorter than the actual output (because
    /// rotation occurred or the output exceeds the preview cap). Plugins use
    /// this to render a `…` prefix and signal that `bash_status` would return
    /// more.
    #[serde(default, skip_serializing_if = "is_false")]
    pub output_truncated: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
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
    pub stderr_path: Option<String>,
}

#[derive(Clone)]
pub struct BgTaskRegistry {
    pub(crate) inner: Arc<RegistryInner>,
}

pub(crate) struct RegistryInner {
    pub(crate) tasks: Mutex<HashMap<String, Arc<BgTask>>>,
    pub(crate) completions: Mutex<VecDeque<BgCompletion>>,
    pub(crate) progress_sender: SharedProgressSender,
    watchdog_started: AtomicBool,
    pub(crate) shutdown: AtomicBool,
    pub(crate) long_running_reminder_enabled: AtomicBool,
    pub(crate) long_running_reminder_interval_ms: AtomicU64,
    persisted_gc_started: AtomicBool,
    #[cfg(test)]
    persisted_gc_runs: AtomicU64,
    /// Output compression callback. Set by `AppContext` after construction.
    /// Takes (command, raw_output) and returns compressed text. Called from
    /// the watchdog thread when a task reaches a terminal state and from
    /// `bash_status`/`list` snapshot reads. When `None`, output is returned
    /// uncompressed.
    pub(crate) compressor: Mutex<Option<Box<dyn Fn(&str, String) -> String + Send + Sync>>>,
}

pub(crate) struct BgTask {
    pub(crate) task_id: String,
    pub(crate) session_id: String,
    pub(crate) paths: TaskPaths,
    pub(crate) started: Instant,
    pub(crate) last_reminder_at: Mutex<Option<Instant>>,
    pub(crate) terminal_at: Mutex<Option<Instant>>,
    pub(crate) state: Mutex<BgTaskState>,
}

pub(crate) struct BgTaskState {
    pub(crate) metadata: PersistedTask,
    pub(crate) child: Option<Child>,
    pub(crate) detached: bool,
    pub(crate) buffer: BgBuffer,
}

impl BgTaskRegistry {
    pub fn new(progress_sender: SharedProgressSender) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                tasks: Mutex::new(HashMap::new()),
                completions: Mutex::new(VecDeque::new()),
                progress_sender,
                watchdog_started: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
                long_running_reminder_enabled: AtomicBool::new(true),
                long_running_reminder_interval_ms: AtomicU64::new(600_000),
                persisted_gc_started: AtomicBool::new(false),
                #[cfg(test)]
                persisted_gc_runs: AtomicU64::new(0),
                compressor: Mutex::new(None),
            }),
        }
    }

    /// Install the output-compression callback. Called by `main.rs` after
    /// `AppContext` is constructed so that snapshot/completion paths can
    /// invoke `compress::compress_with_registry` without holding a context
    /// reference. When called multiple times, the latest installation wins.
    pub fn set_compressor<F>(&self, compressor: F)
    where
        F: Fn(&str, String) -> String + Send + Sync + 'static,
    {
        if let Ok(mut slot) = self.inner.compressor.lock() {
            *slot = Some(Box::new(compressor));
        }
    }

    /// Apply the installed compressor (if any) to `output`. Returns `output`
    /// untouched when no compressor is installed.
    pub(crate) fn compress_output(&self, command: &str, output: String) -> String {
        let Ok(slot) = self.inner.compressor.lock() else {
            return output;
        };
        match slot.as_ref() {
            Some(compressor) => compressor(command, output),
            None => output,
        }
    }

    pub fn configure_long_running_reminders(&self, enabled: bool, interval_ms: u64) {
        self.inner
            .long_running_reminder_enabled
            .store(enabled, Ordering::SeqCst);
        self.inner
            .long_running_reminder_interval_ms
            .store(interval_ms, Ordering::SeqCst);
    }

    #[cfg(unix)]
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        &self,
        command: &str,
        session_id: String,
        workdir: PathBuf,
        env: HashMap<String, String>,
        timeout: Option<Duration>,
        storage_dir: PathBuf,
        max_running: usize,
        notify_on_completion: bool,
        compressed: bool,
        project_root: Option<PathBuf>,
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
            project_root,
            timeout_ms,
            notify_on_completion,
            compressed,
        );
        write_task(&paths.json, &metadata)
            .map_err(|e| format!("failed to persist background task metadata: {e}"))?;

        // Pre-create capture files so the watchdog/buffer can always
        // open them for reading. The spawn helper opens its own handles
        // per attempt because each `Command::spawn()` consumes them.
        create_capture_file(&paths.stdout)
            .map_err(|e| format!("failed to create stdout capture file: {e}"))?;
        create_capture_file(&paths.stderr)
            .map_err(|e| format!("failed to create stderr capture file: {e}"))?;

        let child = match spawn_detached_child(command, &paths, &workdir, &env) {
            Ok(child) => child,
            Err(error) => {
                crate::slog_warn!("failed to spawn background bash task {task_id}; deleting partial bundle: {error}");
                let _ = delete_task_bundle(&paths);
                return Err(error);
            }
        };

        let child_pid = child.id();
        metadata.mark_running(child_pid, child_pid as i32);
        write_task(&paths.json, &metadata)
            .map_err(|e| format!("failed to persist running background task metadata: {e}"))?;

        let task = Arc::new(BgTask {
            task_id: task_id.clone(),
            session_id,
            paths: paths.clone(),
            started: Instant::now(),
            last_reminder_at: Mutex::new(None),
            terminal_at: Mutex::new(None),
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
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        &self,
        command: &str,
        session_id: String,
        workdir: PathBuf,
        env: HashMap<String, String>,
        timeout: Option<Duration>,
        storage_dir: PathBuf,
        max_running: usize,
        notify_on_completion: bool,
        compressed: bool,
        project_root: Option<PathBuf>,
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
            project_root,
            timeout_ms,
            notify_on_completion,
            compressed,
        );
        write_task(&paths.json, &metadata)
            .map_err(|e| format!("failed to persist background task metadata: {e}"))?;

        // Capture files are pre-created so the watchdog/buffer can always
        // open them for reading even if the child hasn't written anything
        // yet. The spawn helper opens its own handles per attempt because
        // each `Command::spawn()` consumes them, and on Windows we may
        // retry across multiple shell candidates if the first one fails.
        create_capture_file(&paths.stdout)
            .map_err(|e| format!("failed to create stdout capture file: {e}"))?;
        create_capture_file(&paths.stderr)
            .map_err(|e| format!("failed to create stderr capture file: {e}"))?;

        let child = match spawn_detached_child(command, &paths, &workdir, &env) {
            Ok(child) => child,
            Err(error) => {
                crate::slog_warn!("failed to spawn background bash task {task_id}; deleting partial bundle: {error}");
                let _ = delete_task_bundle(&paths);
                return Err(error);
            }
        };

        let child_pid = child.id();
        metadata.status = BgTaskStatus::Running;
        metadata.child_pid = Some(child_pid);
        metadata.pgid = None;
        write_task(&paths.json, &metadata)
            .map_err(|e| format!("failed to persist running background task metadata: {e}"))?;

        let task = Arc::new(BgTask {
            task_id: task_id.clone(),
            session_id,
            paths: paths.clone(),
            started: Instant::now(),
            last_reminder_at: Mutex::new(None),
            terminal_at: Mutex::new(None),
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

    pub fn replay_session(&self, storage_dir: &Path, session_id: &str) -> Result<(), String> {
        self.replay_session_inner(storage_dir, session_id, None)
    }

    pub fn replay_session_for_project(
        &self,
        storage_dir: &Path,
        session_id: &str,
        project_root: &Path,
    ) -> Result<(), String> {
        self.replay_session_inner(storage_dir, session_id, Some(project_root))
    }

    fn replay_session_inner(
        &self,
        storage_dir: &Path,
        session_id: &str,
        project_root: Option<&Path>,
    ) -> Result<(), String> {
        self.start_watchdog();
        if !self.inner.persisted_gc_started.swap(true, Ordering::SeqCst) {
            if let Err(error) = self.maybe_gc_persisted(storage_dir) {
                crate::slog_warn!("failed to GC persisted background bash tasks: {error}");
            }
        }
        let dir = session_tasks_dir(storage_dir, session_id);
        if !dir.exists() {
            return Ok(());
        }

        let canonical_project = project_root.map(canonicalized_path);
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
            if let Some(canonical_project) = canonical_project.as_deref() {
                let metadata_project = metadata.project_root.as_deref().map(canonicalized_path);
                if metadata_project.as_deref() != Some(canonical_project) {
                    continue;
                }
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
                    self.enqueue_completion_if_needed(&metadata, Some(&paths), false);
                }
                BgTaskStatus::Running | BgTaskStatus::Killing => {
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
                        self.enqueue_completion_if_needed(&metadata, Some(&paths), false);
                    } else if let Ok(Some(marker)) = read_exit_marker(&paths.exit) {
                        let reason = (metadata.status == BgTaskStatus::Killing).then(|| {
                            "recovered from inconsistent killing state on replay".to_string()
                        });
                        if reason.is_some() {
                            crate::slog_warn!("background task {} had killing state with exit marker; preferring marker",
                            metadata.task_id);
                        }
                        metadata = terminal_metadata_from_marker(metadata, marker, reason);
                        let _ = write_task(&paths.json, &metadata);
                        self.enqueue_completion_if_needed(&metadata, Some(&paths), false);
                    } else if metadata.status == BgTaskStatus::Killing {
                        if !paths.exit.exists() {
                            let _ = write_kill_marker_if_absent(&paths.exit);
                        }
                        metadata.mark_terminal(
                            BgTaskStatus::Killed,
                            None,
                            Some("recovered from inconsistent killing state on replay".to_string()),
                        );
                        let _ = write_task(&paths.json, &metadata);
                        self.enqueue_completion_if_needed(&metadata, Some(&paths), false);
                    } else if metadata.child_pid.is_some_and(|pid| !is_process_alive(pid)) {
                        metadata.mark_terminal(
                            BgTaskStatus::Failed,
                            None,
                            Some("process exited without exit marker".to_string()),
                        );
                        let _ = write_task(&paths.json, &metadata);
                        self.enqueue_completion_if_needed(&metadata, Some(&paths), false);
                    } else {
                        self.insert_rehydrated_task(metadata, paths, true)?;
                    }
                }
                _ if metadata.status.is_terminal() => {
                    // Borrow `paths` for the completion enqueue BEFORE
                    // `insert_rehydrated_task` consumes it. The completion
                    // helper only reads from `paths` (stdout/stderr/exit) to
                    // reconstruct a tail preview, so it must see the same
                    // paths the rehydrated task will own.
                    self.enqueue_completion_if_needed(&metadata, Some(&paths), false);
                    self.insert_rehydrated_task(metadata, paths, true)?;
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
        project_root: Option<&Path>,
        storage_dir: Option<&Path>,
        preview_bytes: usize,
    ) -> Option<BgTaskSnapshot> {
        let mut task = self.task_for_session(task_id, session_id);
        if task.is_none() {
            if let Some(storage_dir) = storage_dir {
                let _ = self.replay_session(storage_dir, session_id);
                task = self.task_for_session(task_id, session_id);
            }
        }
        let Some(task) = task else {
            return self.status_relaxed(
                task_id,
                session_id,
                project_root?,
                storage_dir?,
                preview_bytes,
            );
        };
        let _ = self.poll_task(&task);
        let mut snapshot = task.snapshot(preview_bytes);
        self.maybe_compress_snapshot(&task, &mut snapshot);
        Some(snapshot)
    }

    fn status_relaxed_task(
        &self,
        task_id: &str,
        project_root: &Path,
        storage_dir: &Path,
    ) -> Option<Arc<BgTask>> {
        let canonical_project = canonicalized_path(project_root);
        let root = storage_dir.join("bash-tasks");
        let entries = fs::read_dir(&root).ok()?;
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let path = dir.join(format!("{task_id}.json"));
            if !path.exists() {
                continue;
            }
            let Ok(metadata) = read_task(&path) else {
                continue;
            };
            let metadata_project = metadata.project_root.as_deref().map(canonicalized_path);
            if metadata_project.as_deref() != Some(canonical_project.as_path()) {
                continue;
            }
            if let Some(task) = self.task(task_id) {
                let matches_project = task
                    .state
                    .lock()
                    .map(|state| {
                        state
                            .metadata
                            .project_root
                            .as_deref()
                            .map(canonicalized_path)
                            .as_deref()
                            == Some(canonical_project.as_path())
                    })
                    .unwrap_or(false);
                return matches_project.then_some(task);
            }
            let paths = task_paths(storage_dir, &metadata.session_id, &metadata.task_id);
            if self.insert_rehydrated_task(metadata, paths, true).is_err() {
                return None;
            }
            return self.task(task_id);
        }
        None
    }

    pub(super) fn status_relaxed(
        &self,
        task_id: &str,
        _session_id: &str,
        project_root: &Path,
        storage_dir: &Path,
        preview_bytes: usize,
    ) -> Option<BgTaskSnapshot> {
        let task = self.status_relaxed_task(task_id, project_root, storage_dir)?;
        let _ = self.poll_task(&task);
        let mut snapshot = task.snapshot(preview_bytes);
        self.maybe_compress_snapshot(&task, &mut snapshot);
        Some(snapshot)
    }

    pub fn maybe_gc_persisted(&self, storage_dir: &Path) -> Result<usize, String> {
        #[cfg(test)]
        self.inner.persisted_gc_runs.fetch_add(1, Ordering::SeqCst);

        let mut deleted = 0usize;

        let root = storage_dir.join("bash-tasks");
        if root.exists() {
            let session_dirs = fs::read_dir(&root).map_err(|e| {
                format!(
                    "failed to read background task root {}: {e}",
                    root.display()
                )
            })?;
            for session_entry in session_dirs.flatten() {
                let session_dir = session_entry.path();
                if !session_dir.is_dir() {
                    continue;
                }
                let task_entries = match fs::read_dir(&session_dir) {
                    Ok(entries) => entries,
                    Err(error) => {
                        crate::slog_warn!(
                            "failed to read background task session dir {}: {error}",
                            session_dir.display()
                        );
                        continue;
                    }
                };
                for task_entry in task_entries.flatten() {
                    let json_path = task_entry.path();
                    if json_path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        != Some("json")
                    {
                        continue;
                    }
                    if modified_within(&json_path, PERSISTED_GC_GRACE) {
                        continue;
                    }
                    let metadata = match read_task(&json_path) {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            crate::slog_warn!(
                                "quarantining corrupt background task metadata {}: {error}",
                                json_path.display()
                            );
                            quarantine_corrupt_task_json(storage_dir, &session_dir, &json_path)?;
                            continue;
                        }
                    };
                    if !(metadata.status.is_terminal() && metadata.completion_delivered) {
                        continue;
                    }
                    let paths = task_paths(storage_dir, &metadata.session_id, &metadata.task_id);
                    match delete_task_bundle(&paths) {
                        Ok(()) => {
                            deleted += 1;
                            log::debug!(
                                "deleted persisted background task bundle {}",
                                metadata.task_id
                            );
                        }
                        Err(error) => {
                            crate::slog_warn!(
                                "failed to delete background task bundle {}: {error}",
                                metadata.task_id
                            );
                            continue;
                        }
                    }
                }
            }
        }
        gc_quarantine(storage_dir);
        Ok(deleted)
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
                let mut snapshot = task.snapshot(preview_bytes);
                self.maybe_compress_snapshot(&task, &mut snapshot);
                snapshot
            })
            .collect()
    }

    /// Compress `output_preview` in place when the task is in a terminal
    /// state. Live tail of running tasks stays raw so agents debugging
    /// long-running bash see exactly what the process emitted, not a
    /// heuristic-collapsed view. Per-task opt-out via the `compressed`
    /// field on `PersistedTask` short-circuits before the compress pipeline.
    fn maybe_compress_snapshot(&self, task: &Arc<BgTask>, snapshot: &mut BgTaskSnapshot) {
        if !snapshot.info.status.is_terminal() {
            return;
        }
        let compressed_flag = task
            .state
            .lock()
            .map(|state| state.metadata.compressed)
            .unwrap_or(true);
        if !compressed_flag {
            return;
        }
        let raw = std::mem::take(&mut snapshot.output_preview);
        snapshot.output_preview = self.compress_output(&snapshot.info.command, raw);
    }

    pub fn kill(&self, task_id: &str, session_id: &str) -> Result<BgTaskSnapshot, String> {
        self.kill_with_status(task_id, session_id, BgTaskStatus::Killed)
    }

    pub fn promote(&self, task_id: &str, session_id: &str) -> Result<bool, String> {
        let task = self
            .task_for_session(task_id, session_id)
            .ok_or_else(|| format!("background task not found: {task_id}"))?;
        let mut state = task
            .state
            .lock()
            .map_err(|_| "background task lock poisoned".to_string())?;
        let updated = update_task(&task.paths.json, |metadata| {
            metadata.notify_on_completion = true;
            metadata.completion_delivered = false;
        })
        .map_err(|e| format!("failed to promote background task: {e}"))?;
        state.metadata = updated;
        if state.metadata.status.is_terminal() {
            state.buffer.enforce_terminal_cap();
            self.enqueue_completion_locked(&state.metadata, Some(&state.buffer), true);
        }
        Ok(true)
    }

    pub(crate) fn kill_for_timeout(&self, task_id: &str, session_id: &str) -> Result<(), String> {
        self.kill_with_status(task_id, session_id, BgTaskStatus::TimedOut)
            .map(|_| ())
    }

    pub fn cleanup_finished(&self, older_than: Duration) {
        let cutoff = Instant::now().checked_sub(older_than);
        let removable_paths: Vec<(String, TaskPaths)> =
            if let Ok(mut tasks) = self.inner.tasks.lock() {
                let removable = tasks
                    .iter()
                    .filter_map(|(task_id, task)| {
                        let delivered_terminal = task
                            .state
                            .lock()
                            .map(|state| {
                                state.metadata.status.is_terminal()
                                    && state.metadata.completion_delivered
                            })
                            .unwrap_or(false);
                        if !delivered_terminal {
                            return None;
                        }

                        let terminal_at = task.terminal_at.lock().ok().and_then(|at| *at);
                        let expired = match (terminal_at, cutoff) {
                            (Some(terminal_at), Some(cutoff)) => terminal_at <= cutoff,
                            (Some(_), None) => true,
                            (None, _) => false,
                        };
                        expired.then(|| task_id.clone())
                    })
                    .collect::<Vec<_>>();

                removable
                    .into_iter()
                    .filter_map(|task_id| {
                        tasks
                            .remove(&task_id)
                            .map(|task| (task_id, task.paths.clone()))
                    })
                    .collect()
            } else {
                Vec::new()
            };

        for (task_id, paths) in removable_paths {
            match delete_task_bundle(&paths) {
                Ok(()) => log::debug!("deleted persisted background task bundle {task_id}"),
                Err(error) => crate::slog_warn!(
                    "failed to delete persisted background task bundle {task_id}: {error}"
                ),
            }
        }
    }

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
                // Child has exited. If the wrapper successfully wrote an
                // exit marker, the next `poll_task()` cycle will pick it up
                // and finalize via `finalize_from_marker`. But if the
                // wrapper crashed before writing the marker (e.g. SIGKILL,
                // power loss, wrapper bug), the task would forever appear
                // Running until `timeout_ms` expired — and if no timeout
                // was set, until the 24h `running_metadata_is_stale` cutoff
                // hit at the next aft restart. Same condition as the replay
                // path's "PID dead but no marker" branch (see line 338).
                //
                // To avoid that hidden hang, mark the task Failed
                // immediately with the same reason string used by replay,
                // but only if the marker is genuinely absent. If a marker
                // appeared on disk between try_wait() returning and now
                // (race window), prefer the marker — let the next poll
                // cycle finalize from it.
                state.child = None;
                state.detached = true;
                self.fail_without_exit_marker_if_needed(task, &mut state);
            }
        } else if state.detached
            && state
                .metadata
                .child_pid
                .is_some_and(|pid| !is_process_alive(pid))
        {
            self.fail_without_exit_marker_if_needed(task, &mut state);
        }
    }

    fn fail_without_exit_marker_if_needed(&self, task: &Arc<BgTask>, state: &mut BgTaskState) {
        if state.metadata.status.is_terminal() {
            return;
        }
        if matches!(read_exit_marker(&task.paths.exit), Ok(Some(_))) {
            return;
        }
        let updated = update_task(&task.paths.json, |metadata| {
            metadata.mark_terminal(
                BgTaskStatus::Failed,
                None,
                Some("process exited without exit marker".to_string()),
            );
        });
        if let Ok(metadata) = updated {
            state.metadata = metadata;
            task.mark_terminal_now();
            state.buffer.enforce_terminal_cap();
            self.enqueue_completion_locked(&state.metadata, Some(&state.buffer), true);
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
        let started = started_instant_from_unix_millis(metadata.started_at);
        let task = Arc::new(BgTask {
            task_id: task_id.clone(),
            session_id,
            paths: paths.clone(),
            started,
            last_reminder_at: Mutex::new(None),
            terminal_at: Mutex::new(metadata.status.is_terminal().then(Instant::now)),
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

            if let Ok(Some(marker)) = read_exit_marker(&task.paths.exit) {
                state.metadata =
                    terminal_metadata_from_marker(state.metadata.clone(), marker, None);
                task.mark_terminal_now();
                state.child = None;
                state.detached = true;
                state.buffer.enforce_terminal_cap();
                write_task(&task.paths.json, &state.metadata)
                    .map_err(|e| format!("failed to persist terminal state: {e}"))?;
                self.enqueue_completion_locked(&state.metadata, Some(&state.buffer), true);
                return Ok(task.snapshot_locked(&state, 5 * 1024));
            }

            state.metadata.status = BgTaskStatus::Killing;
            write_task(&task.paths.json, &state.metadata)
                .map_err(|e| format!("failed to persist killing state: {e}"))?;

            #[cfg(unix)]
            if let Some(pgid) = state.metadata.pgid {
                terminate_pgid(pgid, state.child.as_mut());
            }
            #[cfg(windows)]
            if let Some(child) = state.child.as_mut() {
                super::process::terminate_process(child);
            } else if let Some(pid) = state.metadata.child_pid {
                terminate_pid(pid);
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
            task.mark_terminal_now();
            write_task(&task.paths.json, &state.metadata)
                .map_err(|e| format!("failed to persist killed state: {e}"))?;
            state.buffer.enforce_terminal_cap();
            self.enqueue_completion_locked(&state.metadata, Some(&state.buffer), true);
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
        task.mark_terminal_now();
        state.child = None;
        state.detached = true;
        state.buffer.enforce_terminal_cap();
        self.enqueue_completion_locked(&state.metadata, Some(&state.buffer), true);
        Ok(())
    }

    fn enqueue_completion_if_needed(
        &self,
        metadata: &PersistedTask,
        paths: Option<&TaskPaths>,
        emit_frame: bool,
    ) {
        if metadata.status.is_terminal() && !metadata.completion_delivered {
            self.enqueue_completion_from_parts(metadata, None, paths, emit_frame);
        }
    }

    fn enqueue_completion_locked(
        &self,
        metadata: &PersistedTask,
        buffer: Option<&BgBuffer>,
        emit_frame: bool,
    ) {
        self.enqueue_completion_from_parts(metadata, buffer, None, emit_frame);
    }

    fn enqueue_completion_from_parts(
        &self,
        metadata: &PersistedTask,
        buffer: Option<&BgBuffer>,
        paths: Option<&TaskPaths>,
        emit_frame: bool,
    ) {
        if !metadata.status.is_terminal() || metadata.completion_delivered {
            return;
        }
        // Read tail once at completion time and cache on the BgCompletion so
        // both the push-frame consumer (running session) and any later
        // `bash_drain_completions` poll (different session, restart) see the
        // same preview without racing against rotation.
        let (raw_preview, output_truncated) = match buffer {
            Some(buf) => buf.read_tail(BG_COMPLETION_PREVIEW_BYTES),
            None => paths
                .map(|paths| read_tail_from_disk(paths, BG_COMPLETION_PREVIEW_BYTES))
                .unwrap_or_else(|| (String::new(), false)),
        };
        // Compress at completion time so push-frame consumers and later
        // `bash_drain_completions` poll-callers see the same compressed text.
        // Per-task `compressed: false` opts out; otherwise the compressor is
        // a no-op when `experimental.bash.compress=false`.
        let output_preview = if metadata.compressed {
            self.compress_output(&metadata.command, raw_preview)
        } else {
            raw_preview
        };
        let completion = BgCompletion {
            task_id: metadata.task_id.clone(),
            session_id: metadata.session_id.clone(),
            status: metadata.status.clone(),
            exit_code: metadata.exit_code,
            command: metadata.command.clone(),
            output_preview,
            output_truncated,
        };
        if let Ok(mut completions) = self.inner.completions.lock() {
            if completions
                .iter()
                .any(|completion| completion.task_id == metadata.task_id)
            {
                return;
            }
            completions.push_back(completion.clone());
        } else {
            return;
        }

        if emit_frame {
            self.emit_bash_completed(completion);
        }
    }

    fn emit_bash_completed(&self, completion: BgCompletion) {
        let Ok(progress_sender) = self
            .inner
            .progress_sender
            .lock()
            .map(|sender| sender.clone())
        else {
            return;
        };
        let Some(sender) = progress_sender.as_ref() else {
            return;
        };
        // Clone the callback out of the registry mutex before writing to stdout;
        // otherwise a blocked push-frame write could pin the mutex and starve
        // unrelated progress-sender updates.
        // Bg task transitions are discovered by the watchdog thread, so the
        // sender is shared behind a Mutex. It still uses the same stdout writer
        // closure as foreground progress frames, preserving the existing lock/
        // flush behavior in main.rs.
        sender(PushFrame::BashCompleted(BashCompletedFrame::new(
            completion.task_id,
            completion.session_id,
            completion.status,
            completion.exit_code,
            completion.command,
            completion.output_preview,
            completion.output_truncated,
        )));
    }

    pub(crate) fn maybe_emit_long_running_reminder(&self, task: &Arc<BgTask>) {
        if !self
            .inner
            .long_running_reminder_enabled
            .load(Ordering::SeqCst)
        {
            return;
        }
        let interval_ms = self
            .inner
            .long_running_reminder_interval_ms
            .load(Ordering::SeqCst);
        if interval_ms == 0 {
            return;
        }
        let interval = Duration::from_millis(interval_ms);
        let now = Instant::now();
        let Ok(mut last_reminder_at) = task.last_reminder_at.lock() else {
            return;
        };
        let since = last_reminder_at.unwrap_or(task.started);
        if now.duration_since(since) < interval {
            return;
        }
        let command = task
            .state
            .lock()
            .map(|state| state.metadata.command.clone())
            .unwrap_or_default();
        *last_reminder_at = Some(now);
        self.emit_bash_long_running(BashLongRunningFrame::new(
            task.task_id.clone(),
            task.session_id.clone(),
            command,
            task.started.elapsed().as_millis() as u64,
        ));
    }

    fn emit_bash_long_running(&self, frame: BashLongRunningFrame) {
        let Ok(progress_sender) = self
            .inner
            .progress_sender
            .lock()
            .map(|sender| sender.clone())
        else {
            return;
        };
        if let Some(sender) = progress_sender.as_ref() {
            sender(PushFrame::BashLongRunning(frame));
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

    /// Generate a `bash-{16hex}` slug that is unique against live tasks and queued completions.
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
        Self::new(Arc::new(Mutex::new(None)))
    }
}

fn modified_within(path: &Path, grace: Duration) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age < grace)
        .unwrap_or(false)
}

fn canonicalized_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn started_instant_from_unix_millis(started_at: u64) -> Instant {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(started_at);
    let elapsed_ms = now_ms.saturating_sub(started_at);
    Instant::now()
        .checked_sub(Duration::from_millis(elapsed_ms))
        .unwrap_or_else(Instant::now)
}

fn gc_quarantine(storage_dir: &Path) {
    let quarantine_root = storage_dir.join("bash-tasks-quarantine");
    let Ok(session_dirs) = fs::read_dir(&quarantine_root) else {
        return;
    };
    for session_entry in session_dirs.flatten() {
        let session_quarantine_dir = session_entry.path();
        if !session_quarantine_dir.is_dir() {
            continue;
        }
        let entries = match fs::read_dir(&session_quarantine_dir) {
            Ok(entries) => entries,
            Err(error) => {
                crate::slog_warn!(
                    "failed to read background task quarantine dir {}: {error}",
                    session_quarantine_dir.display()
                );
                continue;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if modified_within(&path, QUARANTINE_GC_GRACE) {
                continue;
            }
            let result = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            match result {
                Ok(()) => log::debug!(
                    "deleted old background task quarantine entry {}",
                    path.display()
                ),
                Err(error) => crate::slog_warn!(
                    "failed to delete old background task quarantine entry {}: {error}",
                    path.display()
                ),
            }
        }
        let _ = fs::remove_dir(&session_quarantine_dir);
    }
    let _ = fs::remove_dir(&quarantine_root);
}

fn quarantine_corrupt_task_json(
    storage_dir: &Path,
    session_dir: &Path,
    json_path: &Path,
) -> Result<(), String> {
    let session_hash = session_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "invalid background task session dir: {}",
                session_dir.display()
            )
        })?;
    let task_name = json_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid background task json path: {}", json_path.display()))?;
    let unix_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let quarantine_dir = storage_dir.join("bash-tasks-quarantine").join(session_hash);
    fs::create_dir_all(&quarantine_dir).map_err(|e| {
        format!(
            "failed to create background task quarantine dir {}: {e}",
            quarantine_dir.display()
        )
    })?;
    let target = quarantine_dir.join(format!("{task_name}.corrupt-{unix_ts}"));
    fs::rename(json_path, &target).map_err(|e| {
        format!(
            "failed to quarantine corrupt background task metadata {} to {}: {e}",
            json_path.display(),
            target.display()
        )
    })?;

    for sibling in task_sibling_paths(json_path) {
        if !sibling.exists() {
            continue;
        }
        let Some(sibling_name) = sibling.file_name().and_then(|name| name.to_str()) else {
            crate::slog_warn!(
                "skipping background task sibling with invalid name during quarantine: {}",
                sibling.display()
            );
            continue;
        };
        let sibling_target = quarantine_dir.join(format!("{sibling_name}.corrupt-{unix_ts}"));
        if let Err(error) = fs::rename(&sibling, &sibling_target) {
            crate::slog_warn!(
                "failed to quarantine background task sibling {} to {}: {error}",
                sibling.display(),
                sibling_target.display()
            );
        }
    }

    let _ = fs::remove_dir(session_dir);
    Ok(())
}

fn task_sibling_paths(json_path: &Path) -> Vec<PathBuf> {
    let Some(parent) = json_path.parent() else {
        return Vec::new();
    };
    let Some(stem) = json_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    ["stdout", "stderr", "exit", "ps1", "bat", "sh"]
        .into_iter()
        .map(|extension| parent.join(format!("{stem}.{extension}")))
        .collect()
}

fn read_tail_from_disk(paths: &TaskPaths, max_bytes: usize) -> (String, bool) {
    let stdout = fs::read(&paths.stdout).unwrap_or_default();
    let stderr = fs::read(&paths.stderr).unwrap_or_default();
    let mut bytes = Vec::with_capacity(stdout.len().saturating_add(stderr.len()));
    bytes.extend_from_slice(&stdout);
    bytes.extend_from_slice(&stderr);
    if bytes.len() <= max_bytes {
        return (String::from_utf8_lossy(&bytes).into_owned(), false);
    }
    let start = bytes.len().saturating_sub(max_bytes);
    (String::from_utf8_lossy(&bytes[start..]).into_owned(), true)
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
            stderr_path: Some(state.buffer.stderr_path().display().to_string()),
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.metadata.status == BgTaskStatus::Running)
            .unwrap_or(false)
    }

    fn mark_terminal_now(&self) {
        if let Ok(mut terminal_at) = self.terminal_at.lock() {
            if terminal_at.is_none() {
                *terminal_at = Some(Instant::now());
            }
        }
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
    let shell = resolve_posix_shell();
    let mut cmd = Command::new(&shell);
    cmd.arg("-c")
        .arg("\"$0\" -c \"$1\"; code=$?; printf \"%s\" \"$code\" > \"$2.tmp.$$\"; mv -f \"$2.tmp.$$\" \"$2\"")
        .arg(&shell)
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

#[cfg(unix)]
fn resolve_posix_shell() -> PathBuf {
    static POSIX_SHELL: OnceLock<PathBuf> = OnceLock::new();
    POSIX_SHELL
        .get_or_init(|| {
            std::env::var_os("BASH")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|path| path.exists())
                .or_else(|| which::which("bash").ok())
                .or_else(|| which::which("zsh").ok())
                .unwrap_or_else(|| PathBuf::from("/bin/sh"))
        })
        .clone()
}

#[cfg(windows)]
fn detached_shell_command_for(
    shell: crate::windows_shell::WindowsShell,
    command: &str,
    exit_path: &Path,
    paths: &TaskPaths,
    creation_flags: u32,
) -> Result<Command, String> {
    use crate::windows_shell::WindowsShell;
    // Write the wrapper to a temp file alongside the other task files,
    // then invoke the shell with the file path as a single clean
    // argument. This sidesteps the entire Windows command-line quoting
    // mess (Rust std-lib quoting + cmd /C parser + PowerShell -Command
    // parser all interacting with embedded quotes in the wrapper).
    //
    // Path arguments don't need quoting in the same problematic way
    // because: (1) we use no-space task IDs (bash-XXXXXXXX) so the path
    // contains no characters that need shell escaping; (2) the wrapper
    // body's internal quotes never reach the shell command line — the
    // shell reads them from disk by file syntax rules, not command-line
    // parser rules.
    let wrapper_body = shell.wrapper_script(command, exit_path);
    let wrapper_ext = match shell {
        WindowsShell::Pwsh | WindowsShell::Powershell => "ps1",
        WindowsShell::Cmd => "bat",
        // POSIX shells (git-bash etc.) execute the wrapper through `-c`,
        // so the file extension is purely cosmetic; `.sh` matches what an
        // operator would expect when grepping the spill directory.
        WindowsShell::Posix(_) => "sh",
    };
    let wrapper_path = paths.dir.join(format!(
        "{}.{}",
        paths
            .json
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("wrapper"),
        wrapper_ext
    ));
    fs::write(&wrapper_path, wrapper_body)
        .map_err(|e| format!("failed to write background bash wrapper script: {e}"))?;

    let mut cmd = Command::new(shell.binary().as_ref());
    match shell {
        WindowsShell::Pwsh | WindowsShell::Powershell => {
            // -File runs the script with no quoting issues. `-NoLogo`,
            // `-NoProfile`, etc. apply to the host before the file runs.
            cmd.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ]);
            cmd.arg(&wrapper_path);
        }
        WindowsShell::Cmd => {
            // `cmd /D /C "<bat-file-path>"` — invoking a .bat
            // file via /C is well-defined; the file's contents are
            // read line-by-line by cmd's batch processor, NOT
            // re-interpreted by the /C parser. This avoids the
            // "filename syntax incorrect" errors that came from
            // having complex compound commands on the cmd line.
            cmd.args(["/D", "/C"]);
            cmd.arg(&wrapper_path);
        }
        WindowsShell::Posix(_) => {
            // git-bash and other POSIX shells run the wrapper script with
            // `<binary> <wrapper-path>` (the wrapper is just a shell
            // script). No special flags needed — the `trap` and atomic
            // exit-marker rename in `wrapper_script` are POSIX-standard.
            cmd.arg(&wrapper_path);
        }
    }

    // Win32 process creation flags. Caller selects whether to include
    // CREATE_BREAKAWAY_FROM_JOB — see `detached_shell_command_for` callers
    // for the breakaway-fallback strategy.
    cmd.creation_flags(creation_flags);
    Ok(cmd)
}

/// Spawn a detached background bash child process.
///
/// On Unix this is a single spawn against `/bin/sh`. On Windows it walks
/// `WindowsShell::shell_candidates()` (pwsh.exe → powershell.exe →
/// cmd.exe) and retries with the next candidate when the previous one
/// fails to spawn with `NotFound` — the same runtime safety net the
/// foreground bash path has, so issue #27 callers landing on cmd.exe
/// fallback can also use background bash. The wrapper script is
/// regenerated per attempt because PowerShell wrappers embed the shell
/// binary by name; the stdout/stderr capture handles are also reopened
/// per attempt because `Command::spawn()` consumes them.
///
/// Errors other than `NotFound` (PermissionDenied, OutOfMemory, etc.)
/// return immediately without retry — they indicate a problem with the
/// resolved shell that retrying with a different shell won't fix.
fn spawn_detached_child(
    command: &str,
    paths: &TaskPaths,
    workdir: &Path,
    env: &HashMap<String, String>,
) -> Result<std::process::Child, String> {
    #[cfg(not(windows))]
    {
        let stdout = create_capture_file(&paths.stdout)
            .map_err(|e| format!("failed to open stdout capture file: {e}"))?;
        let stderr = create_capture_file(&paths.stderr)
            .map_err(|e| format!("failed to open stderr capture file: {e}"))?;
        detached_shell_command(command, &paths.exit)
            .current_dir(workdir)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|e| format!("failed to spawn background bash command: {e}"))
    }
    #[cfg(windows)]
    {
        use crate::windows_shell::shell_candidates;
        // Spawn priority: pwsh → powershell → git-bash → cmd. Same as the
        // legacy foreground bash spawn path. v0.20 routes ALL bash through
        // this background spawn helper, including foreground tool calls
        // where the model writes PowerShell-syntax (`$var = ...`,
        // `Start-Sleep`, `Add-Content`) — those fail outright under cmd.
        // The earlier v0.18-era cmd-first override worked around a
        // PowerShell detached-output bug; that bug is fixed at the
        // process-flag layer (CREATE_NO_WINDOW instead of DETACHED_PROCESS,
        // see flag block below), so we no longer need to misroute PS
        // commands through cmd.
        let candidates: Vec<crate::windows_shell::WindowsShell> = shell_candidates();
        // Win32 process creation flags. We try with CREATE_BREAKAWAY_FROM_JOB
        // first (so the bg child outlives the AFT process when AFT is killed),
        // then fall back without it for environments where the parent is in a
        // Job Object that doesn't grant `JOB_OBJECT_LIMIT_BREAKAWAY_OK`. CI
        // runners (GitHub Actions windows-2022) and some MDM-managed corp
        // environments hit this — `CreateProcess` returns Access Denied (5).
        // Without breakaway, the child still runs detached but will be torn
        // down with the parent if the parent process group is signaled.
        //
        // We use CREATE_NO_WINDOW (no visible console window, but the
        // child still has a hidden console) rather than DETACHED_PROCESS
        // (no console at all). PowerShell-based wrappers that perform
        // file I/O via [System.IO.File] need a console handle to flush
        // stdout/stderr correctly even when redirected — under
        // DETACHED_PROCESS, pwsh sometimes silently exits before
        // executing later script statements (the Move-Item that writes
        // the exit marker never runs), leaving the bg task forever
        // marked Failed: process exited without exit marker. cmd.exe
        // wrappers tolerate DETACHED_PROCESS, but switching to
        // CREATE_NO_WINDOW costs nothing for cmd and unblocks pwsh.
        const FLAG_CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const FLAG_CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const FLAG_CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let with_breakaway =
            FLAG_CREATE_NO_WINDOW | FLAG_CREATE_NEW_PROCESS_GROUP | FLAG_CREATE_BREAKAWAY_FROM_JOB;
        let without_breakaway = FLAG_CREATE_NO_WINDOW | FLAG_CREATE_NEW_PROCESS_GROUP;
        let mut last_error: Option<String> = None;
        for (idx, shell) in candidates.iter().enumerate() {
            // Per-shell, try with breakaway first. If the process is in a
            // restrictive job, the breakaway flag triggers Access Denied
            // (os error 5). Retry once without breakaway.
            for &flags in &[with_breakaway, without_breakaway] {
                // Re-open capture handles per attempt; spawn() consumes them.
                let stdout = create_capture_file(&paths.stdout)
                    .map_err(|e| format!("failed to open stdout capture file: {e}"))?;
                let stderr = create_capture_file(&paths.stderr)
                    .map_err(|e| format!("failed to open stderr capture file: {e}"))?;
                let mut cmd =
                    detached_shell_command_for(shell.clone(), command, &paths.exit, paths, flags)?;
                cmd.current_dir(workdir)
                    .envs(env)
                    .stdin(Stdio::null())
                    .stdout(Stdio::from(stdout))
                    .stderr(Stdio::from(stderr));
                match cmd.spawn() {
                    Ok(child) => {
                        if idx > 0 {
                            crate::slog_warn!("background bash spawn fell back to {} after {} earlier candidate(s) failed; \
                             the cached PATH probe disagreed with runtime spawn — likely PATH \
                             inheritance, antivirus / AppLocker / Defender ASR, or sandbox policy.",
                            shell.binary(),
                            idx);
                        }
                        if flags == without_breakaway {
                            crate::slog_warn!(
                                "background bash spawn: CREATE_BREAKAWAY_FROM_JOB rejected \
                             (likely a restrictive Job Object — CI sandbox or MDM policy). \
                             Spawned without breakaway; the bg task will be torn down if the \
                             AFT process group is killed."
                            );
                        }
                        return Ok(child);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        crate::slog_warn!("background bash spawn: {} returned NotFound at runtime — trying next candidate",
                        shell.binary());
                        last_error = Some(format!("{}: {e}", shell.binary()));
                        // Skip the without-breakaway retry for NotFound — the
                        // binary itself is missing, breakaway flag is irrelevant.
                        break;
                    }
                    Err(e) if flags == with_breakaway && e.raw_os_error() == Some(5) => {
                        // Access Denied during breakaway — retry without it.
                        crate::slog_warn!(
                            "background bash spawn: CREATE_BREAKAWAY_FROM_JOB rejected with \
                         Access Denied — retrying {} without breakaway",
                            shell.binary()
                        );
                        last_error = Some(format!("{}: {e}", shell.binary()));
                        continue;
                    }
                    Err(e) => {
                        return Err(format!(
                            "failed to spawn background bash command via {}: {e}",
                            shell.binary()
                        ));
                    }
                }
            }
        }
        Err(format!(
            "failed to spawn background bash command: no Windows shell could be spawned. \
             Last error: {}. PATH-probed candidates: {:?}",
            last_error.unwrap_or_else(|| "no candidates were attempted".to_string()),
            candidates.iter().map(|s| s.binary()).collect::<Vec<_>>()
        ))
    }
}

fn random_slug() -> String {
    let mut bytes = [0u8; 4];
    // getrandom is a transitive dependency; use it directly for OS entropy.
    getrandom::fill(&mut bytes).unwrap_or_else(|_| {
        // Extremely unlikely fallback: time + pid mix.
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let p = std::process::id();
        bytes.copy_from_slice(&(t ^ p).to_le_bytes());
    });
    // `bash-` + 8 lowercase hex chars — compact, OS-entropy backed.
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("bash-{hex}")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    #[cfg(windows)]
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    #[cfg(windows)]
    use std::time::Instant;

    use super::*;

    #[cfg(unix)]
    const QUICK_SUCCESS_COMMAND: &str = "true";
    #[cfg(windows)]
    const QUICK_SUCCESS_COMMAND: &str = "cmd /c exit 0";

    #[cfg(unix)]
    const LONG_RUNNING_COMMAND: &str = "sleep 5";
    #[cfg(windows)]
    const LONG_RUNNING_COMMAND: &str = "cmd /c timeout /t 5 /nobreak > nul";

    /// Spawn a child process that exits immediately and return it after
    /// it has terminated. Used by reap_child tests to simulate the
    /// "child exists and is dead" state when the watchdog has already
    /// nulled out the original child handle.
    fn spawn_dead_child() -> std::process::Child {
        #[cfg(unix)]
        let mut cmd = std::process::Command::new("true");
        #[cfg(windows)]
        let mut cmd = {
            let mut c = std::process::Command::new("cmd");
            c.args(["/c", "exit", "0"]);
            c
        };
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().expect("spawn replacement child for reap test");
        // Poll try_wait() until the child actually exits, instead of calling
        // wait() which closes the OS handle. On Windows, after wait()
        // closes the handle, subsequent try_wait() calls (which reap_child
        // depends on) return Err — the test was inadvertently giving
        // reap_child an unusable child handle. Polling try_wait() keeps the
        // handle open and observes natural exit, matching the production
        // shape where the watchdog discovers an exited child for the first
        // time.
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if started.elapsed() > Duration::from_secs(5) {
                        panic!("dead-child stand-in did not exit within 5s");
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("dead-child try_wait failed: {error}"),
            }
        }
        child
    }

    #[test]
    fn cleanup_finished_removes_terminal_tasks_older_than_threshold() {
        let registry = BgTaskRegistry::default();
        let dir = tempfile::tempdir().unwrap();
        let task_id = registry
            .spawn(
                QUICK_SUCCESS_COMMAND,
                "session".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
                Some(Duration::from_secs(30)),
                dir.path().to_path_buf(),
                10,
                true,
                false,
                Some(dir.path().to_path_buf()),
            )
            .unwrap();
        registry
            .kill_with_status(&task_id, "session", BgTaskStatus::Killed)
            .unwrap();
        let completions = registry.drain_completions_for_session(Some("session"));
        assert_eq!(completions.len(), 1);

        registry.cleanup_finished(Duration::ZERO);

        assert!(registry.inner.tasks.lock().unwrap().is_empty());
    }

    #[test]
    fn cleanup_finished_retains_undelivered_terminals() {
        let registry = BgTaskRegistry::default();
        let dir = tempfile::tempdir().unwrap();
        let task_id = registry
            .spawn(
                QUICK_SUCCESS_COMMAND,
                "session".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
                Some(Duration::from_secs(30)),
                dir.path().to_path_buf(),
                10,
                true,
                false,
                Some(dir.path().to_path_buf()),
            )
            .unwrap();
        registry
            .kill_with_status(&task_id, "session", BgTaskStatus::Killed)
            .unwrap();

        registry.cleanup_finished(Duration::ZERO);

        assert!(registry.inner.tasks.lock().unwrap().contains_key(&task_id));
    }

    /// Issue #27 Oracle review P1 + P2 test gap: verify that the live
    /// watchdog path (reap_child) marks a task Failed when the child
    /// has exited but no exit marker was written. Before this fix the
    /// task would remain `Running` until timeout, even though the
    /// process was definitely dead.
    ///
    /// Cross-platform: uses a quick-exiting command that does NOT go
    /// through the wrapper script (we manually clear the exit marker
    /// after spawn to simulate the wrapper crashing before write).
    #[test]
    fn reap_child_marks_failed_when_child_exits_without_exit_marker() {
        let registry = BgTaskRegistry::new(Arc::new(Mutex::new(None)));
        let dir = tempfile::tempdir().unwrap();
        let task_id = registry
            .spawn(
                QUICK_SUCCESS_COMMAND,
                "session".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
                Some(Duration::from_secs(30)),
                dir.path().to_path_buf(),
                10,
                true,
                false,
                Some(dir.path().to_path_buf()),
            )
            .unwrap();

        let task = registry.task_for_session(&task_id, "session").unwrap();

        // Wait for the child to actually exit and the wrapper to either
        // write the marker or fail. Then nuke the marker to simulate
        // wrapper crash before write. Poll up to 5s; this is plenty for a
        // `true`/`cmd /c exit 0` invocation.
        let started = Instant::now();
        loop {
            let exited = {
                let mut state = task.state.lock().unwrap();
                if let Some(child) = state.child.as_mut() {
                    matches!(child.try_wait(), Ok(Some(_)))
                } else {
                    true
                }
            };
            if exited {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "child should exit quickly"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        // Stop the watchdog so it doesn't race with our manual reap_child.
        // On fast Windows runners the watchdog ticks (every 500ms) can
        // observe the child exit and reap it before this test's assertion
        // fires, leaving us with state.child = None and an already-terminal
        // status. We specifically want to test reap_child's logic when
        // invoked manually on a Running-but-actually-dead task, so we need
        // exclusive control over the reap path here.
        registry
            .inner
            .shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // Give the watchdog at most one tick (500ms) to notice shutdown
        // before we touch task state. Without this, an in-flight watchdog
        // iteration could still race with our state setup below.
        std::thread::sleep(Duration::from_millis(550));

        // Wrapper likely wrote the marker by now; remove it to simulate
        // a wrapper crash that exited before persisting the exit code.
        let _ = std::fs::remove_file(&task.paths.exit);

        // The watchdog may have already reaped the child handle and
        // marked the task terminal before we got here. Reset both so
        // reap_child has the "Running task whose child just exited"
        // shape it's designed to handle. We don't restore state.child
        // (the underlying OS process is gone), but reap_child's
        // try_wait path won't be exercised; we're testing the
        // status-transition logic when state.child is set to a dead
        // child OR None and the marker is missing.
        //
        // CRITICAL on Windows: the watchdog ticks fast enough that the
        // JSON on disk may already say `Completed`. `update_task` (called
        // by `reap_child`) reads from disk, applies the closure, but
        // ROLLS BACK if the original on-disk state was already terminal
        // (see persistence.rs::update_task). So we must reset BOTH
        // in-memory metadata AND the JSON on disk to a Running state to
        // give reap_child the fresh shape it expects to operate on.
        {
            let mut state = task.state.lock().unwrap();
            state.metadata.status = BgTaskStatus::Running;
            state.metadata.status_reason = None;
            state.metadata.exit_code = None;
            state.metadata.finished_at = None;
            state.metadata.duration_ms = None;
            // Persist the reset state to disk so update_task's terminal
            // rollback guard sees a non-terminal starting point.
            crate::bash_background::persistence::write_task(&task.paths.json, &state.metadata)
                .expect("persist reset Running metadata for reap_child test");
            // If the watchdog already nulled state.child, we need to
            // simulate "child exists and is dead" so reap_child's
            // try_wait path runs. Spawn a quick-exit child as a stand-in.
            if state.child.is_none() {
                state.child = Some(spawn_dead_child());
            }
        }
        // Clear the terminal_at marker too so mark_terminal_now() can fire
        // again inside reap_child.
        *task.terminal_at.lock().unwrap() = None;

        // Sanity: task is still Running per metadata (replay/poll hasn't
        // observed the missing marker yet).
        assert!(
            task.is_running(),
            "precondition: metadata.status == Running"
        );
        assert!(
            !task.paths.exit.exists(),
            "precondition: exit marker absent"
        );

        // Invoke the watchdog's reap_child directly. The fix should mark
        // the task Failed with the documented reason string, instead of
        // just dropping the child handle and leaving status=Running.
        registry.reap_child(&task);

        let state = task.state.lock().unwrap();
        assert!(
            state.metadata.status.is_terminal(),
            "reap_child must transition to terminal when PID dead and no marker. \
             Got status={:?}",
            state.metadata.status
        );
        assert_eq!(
            state.metadata.status,
            BgTaskStatus::Failed,
            "must specifically be Failed (not Killed): status={:?}",
            state.metadata.status
        );
        assert_eq!(
            state.metadata.status_reason.as_deref(),
            Some("process exited without exit marker"),
            "reason must match replay path's wording: {:?}",
            state.metadata.status_reason
        );
        assert!(
            state.child.is_none(),
            "child handle must be released after reap"
        );
        assert!(state.detached, "task must be marked detached after reap");
    }

    /// Companion to the above: when the exit marker DOES exist on disk
    /// at reap_child time (race window — wrapper finished writing
    /// between try_wait and the marker check), reap_child must NOT mark
    /// the task Failed. Instead it leaves status=Running and lets the
    /// next poll_task() cycle finalize via the marker.
    #[test]
    fn reap_child_preserves_running_when_exit_marker_exists() {
        let registry = BgTaskRegistry::new(Arc::new(Mutex::new(None)));
        let dir = tempfile::tempdir().unwrap();
        let task_id = registry
            .spawn(
                QUICK_SUCCESS_COMMAND,
                "session".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
                Some(Duration::from_secs(30)),
                dir.path().to_path_buf(),
                10,
                true,
                false,
                Some(dir.path().to_path_buf()),
            )
            .unwrap();

        let task = registry.task_for_session(&task_id, "session").unwrap();

        // Wait for child to exit AND for the marker to land. Both happen
        // shortly after the wrapper finishes — but we want both observed.
        let started = Instant::now();
        loop {
            let exited = {
                let mut state = task.state.lock().unwrap();
                if let Some(child) = state.child.as_mut() {
                    matches!(child.try_wait(), Ok(Some(_)))
                } else {
                    true
                }
            };
            if exited && task.paths.exit.exists() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "child should exit and write marker quickly"
            );
            std::thread::sleep(Duration::from_millis(20));
        }

        // Stop the watchdog so it doesn't race with our manual reap_child.
        // On fast Windows runners the watchdog can call poll_task (which
        // finalizes via marker) before this test asserts the
        // "marker exists, status still Running" invariant. We want
        // exclusive control over the reap path.
        registry
            .inner
            .shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(550));

        // If the watchdog already finalized the task before we stopped it,
        // restore the test setup: reset status to Running and ensure the
        // marker file is still on disk. We're testing reap_child's
        // behavior when called manually with both child-exited AND
        // marker-present, regardless of whether the watchdog beat us.
        {
            let mut state = task.state.lock().unwrap();
            state.metadata.status = BgTaskStatus::Running;
            state.metadata.status_reason = None;
            if state.child.is_none() {
                state.child = Some(spawn_dead_child());
            }
        }
        *task.terminal_at.lock().unwrap() = None;
        // Make sure the marker is still on disk (poll_task removes it on
        // finalization). Recreate it if needed.
        if !task.paths.exit.exists() {
            std::fs::write(&task.paths.exit, "0").expect("write replacement exit marker");
        }

        // reap_child sees: child exited, marker exists. It should:
        //  - drop state.child / set state.detached = true
        //  - NOT change status (poll_task will finalize via marker next tick)
        registry.reap_child(&task);

        let state = task.state.lock().unwrap();
        assert!(
            state.child.is_none(),
            "child handle still released even when marker exists"
        );
        assert!(
            state.detached,
            "task still marked detached even when marker exists"
        );
        // Status remains Running because reap_child defers to poll_task
        // when a marker exists. It would be wrong for reap to record the
        // marker outcome (poll_task does that with proper exit-code
        // parsing).
        assert_eq!(
            state.metadata.status,
            BgTaskStatus::Running,
            "reap_child must defer to poll_task when marker exists"
        );
    }

    #[test]
    fn cleanup_finished_keeps_running_tasks() {
        let registry = BgTaskRegistry::new(Arc::new(Mutex::new(None)));
        let dir = tempfile::tempdir().unwrap();
        let task_id = registry
            .spawn(
                LONG_RUNNING_COMMAND,
                "session".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
                Some(Duration::from_secs(30)),
                dir.path().to_path_buf(),
                10,
                true,
                false,
                Some(dir.path().to_path_buf()),
            )
            .unwrap();

        registry.cleanup_finished(Duration::ZERO);

        assert!(registry.inner.tasks.lock().unwrap().contains_key(&task_id));
        let _ = registry.kill(&task_id, "session");
    }

    #[cfg(windows)]
    fn wait_for_file(path: &Path) -> String {
        let started = Instant::now();
        loop {
            if path.exists() {
                return fs::read_to_string(path).expect("read file");
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(windows)]
    fn spawn_windows_registry_command(
        command: &str,
    ) -> (BgTaskRegistry, tempfile::TempDir, String) {
        let registry = BgTaskRegistry::new(Arc::new(Mutex::new(None)));
        let dir = tempfile::tempdir().unwrap();
        let task_id = registry
            .spawn(
                command,
                "session".to_string(),
                dir.path().to_path_buf(),
                HashMap::new(),
                Some(Duration::from_secs(30)),
                dir.path().to_path_buf(),
                10,
                false,
                false,
                Some(dir.path().to_path_buf()),
            )
            .unwrap();
        (registry, dir, task_id)
    }

    #[cfg(windows)]
    #[test]
    fn windows_spawn_writes_exit_marker_for_zero_exit() {
        let (registry, _dir, task_id) = spawn_windows_registry_command("cmd /c exit 0");
        let exit_path = registry.task_exit_path(&task_id, "session").unwrap();

        let content = wait_for_file(&exit_path);

        assert_eq!(content.trim(), "0");
    }

    #[cfg(windows)]
    #[test]
    fn windows_spawn_writes_exit_marker_for_nonzero_exit() {
        let (registry, _dir, task_id) = spawn_windows_registry_command("cmd /c exit 42");
        let exit_path = registry.task_exit_path(&task_id, "session").unwrap();

        let content = wait_for_file(&exit_path);

        assert_eq!(content.trim(), "42");
    }

    #[cfg(windows)]
    #[test]
    fn windows_spawn_captures_stdout_to_disk() {
        let (registry, _dir, task_id) = spawn_windows_registry_command("cmd /c echo hello");
        let task = registry.task_for_session(&task_id, "session").unwrap();
        let stdout_path = task.paths.stdout.clone();
        let exit_path = task.paths.exit.clone();

        let _ = wait_for_file(&exit_path);
        let stdout = fs::read_to_string(stdout_path).expect("read stdout");

        assert!(stdout.contains("hello"), "stdout was {stdout:?}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_spawn_uses_pwsh_when_available() {
        // Without $SHELL set, $SHELL probe yields None and pwsh wins.
        // (We intentionally pass None for shell_env to keep this test
        // independent of the runner's actual env.)
        let candidates = crate::windows_shell::shell_candidates_with(
            |binary| match binary {
                "pwsh.exe" => Some(std::path::PathBuf::from(r"C:\pwsh\pwsh.exe")),
                "powershell.exe" => Some(std::path::PathBuf::from(r"C:\ps\powershell.exe")),
                _ => None,
            },
            || None,
        );
        let shell = candidates.first().expect("at least one candidate").clone();
        assert_eq!(shell, crate::windows_shell::WindowsShell::Pwsh);
        assert_eq!(shell.binary().as_ref(), "pwsh.exe");
    }

    /// Issue #27 Oracle review P1, updated: cmd wrapper writes a `.bat` file
    /// that batch-evaluates `%ERRORLEVEL%` on its own line (line-by-line
    /// evaluation is the default for batch files; parse-time expansion only
    /// applies to compound `&`-chained inline commands). Capturing
    /// `%ERRORLEVEL%` into `set CODE=%ERRORLEVEL%` immediately after the user
    /// command runs records the real run-time exit code.
    #[cfg(windows)]
    #[test]
    fn windows_shell_cmd_wrapper_writes_exit_marker_with_move() {
        let exit_path = Path::new(r"C:\Temp\bash-test.exit");
        let script =
            crate::windows_shell::WindowsShell::Cmd.wrapper_script("cmd /c exit 42", exit_path);

        // Batch wrapper: capture exit code into CODE on the line after the
        // user command, then write CODE to a temp marker file before
        // atomic-renaming it into place.
        assert!(
            script.contains("set CODE=%ERRORLEVEL%"),
            "wrapper must capture exit code into CODE: {script}"
        );
        assert!(
            script.contains("echo %CODE% >"),
            "wrapper must echo CODE to a temp marker file: {script}"
        );
        assert!(
            script.contains("move /Y"),
            "wrapper must use atomic move to write the marker: {script}"
        );
        // move output must be redirected to nul to avoid polluting the
        // user's captured stdout with "1 file(s) moved." lines.
        assert!(
            script.contains("> nul"),
            "wrapper must redirect move output to nul: {script}"
        );
        // exit /B %CODE% propagates the real exit code so wait() sees it.
        assert!(
            script.contains("exit /B %CODE%"),
            "wrapper must propagate the captured exit code: {script}"
        );
        assert!(script.contains(r#""C:\Temp\bash-test.exit.tmp""#));
        assert!(script.contains(r#""C:\Temp\bash-test.exit""#));
    }

    /// `bg_command()` for Cmd no longer needs `/V:ON` — the wrapper is now
    /// written to a `.bat` file where batch-line evaluation captures
    /// `%ERRORLEVEL%` correctly without delayed expansion. We still need
    /// `/D` (skip AutoRun) and `/S` (simple quote-stripping for paths with
    /// internal `"`-quoting from `cmd_quote`).
    #[cfg(windows)]
    #[test]
    fn windows_shell_cmd_bg_command_uses_minimal_cmd_flags() {
        use crate::windows_shell::WindowsShell;
        let cmd = WindowsShell::Cmd.bg_command("echo wrapped");
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        let args_strs: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert_eq!(
            args_strs,
            vec!["/D", "/S", "/C", "echo wrapped"],
            "Cmd::bg_command must prepend /D /S /C"
        );
    }

    /// PowerShell variants don't need `/V:ON`-style flags; their args
    /// are the same for foreground (`command()`) and background
    /// (`bg_command()`).
    #[cfg(windows)]
    #[test]
    fn windows_shell_pwsh_bg_command_uses_standard_args() {
        use crate::windows_shell::WindowsShell;
        let cmd = WindowsShell::Pwsh.bg_command("Get-Date");
        let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
        let args_strs: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(
            args_strs.contains(&"-Command"),
            "Pwsh::bg_command must use -Command: {args_strs:?}"
        );
        assert!(
            args_strs.contains(&"Get-Date"),
            "Pwsh::bg_command must include the user command body"
        );
    }

    /// Issue #27 Oracle review P1 + P2 test gap: end-to-end proof that the
    /// **cmd.exe-specific** wrapper path captures the user command's
    /// run-time exit code correctly. The existing
    /// `windows_spawn_writes_exit_marker_for_nonzero_exit` test would also
    /// pass with the buggy `%ERRORLEVEL%` wrapper if the Windows machine
    /// had pwsh.exe or powershell.exe on PATH (which is typical) — the
    /// outer wrapper would be PowerShell, not cmd, and PowerShell's
    /// `$LASTEXITCODE` captures the inner `cmd /c exit 42` correctly.
    ///
    /// This test directly spawns via `WindowsShell::Cmd.bg_command()` to
    /// force the cmd-wrapper code path, then writes the exit marker and
    /// asserts it contains "42" not "0". With the pre-fix `%ERRORLEVEL%`
    /// wrapper, this test would fail because `%ERRORLEVEL%` parse-time
    /// expansion would record cmd's startup ERRORLEVEL (typically 0)
    /// regardless of what the user command returned.
    /// **Disabled.** This test exercises `WindowsShell::Cmd.bg_command()` —
    /// the inline command-line wrapper helper that production code does
    /// NOT use anymore. v0.19.4 switched bg-bash to a file-based wrapper
    /// (`<task>.bat` / `<task>.ps1`) because the inline cmd-line quoting
    /// produced silent failures on Windows 11 (move /Y could not parse
    /// path arguments through cmd's /C parser). The `bg_command` helper
    /// is kept only for parity with `WindowsShell::Cmd.command()` shape;
    /// the production spawn path goes through `detached_shell_command_for`
    /// which writes the wrapper to disk and invokes `cmd /V:ON /D /C
    /// <bat-path>`.
    ///
    /// The `!ERRORLEVEL!` correctness this test was meant to verify is
    /// covered live by the Windows e2e harness scenario 2d
    /// (`bg bash records non-zero exit code (cmd /c exit 42)`), which
    /// exercises the real file-based wrapper end-to-end via the protocol.
    #[allow(dead_code)]
    #[cfg(any())] // disabled on all targets
    fn windows_cmd_wrapper_records_real_exit_code_disabled() {}
}
