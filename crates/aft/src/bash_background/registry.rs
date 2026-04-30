use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use super::buffer::{default_output_dir, BgBuffer, StreamKind};
use super::{BgTaskInfo, BgTaskStatus};

const TERMINATE_GRACE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

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
    inner: Arc<RegistryInner>,
}

struct RegistryInner {
    tasks: Mutex<HashMap<String, Arc<BgTask>>>,
    completions: Mutex<VecDeque<BgCompletion>>,
}

struct BgTask {
    task_id: String,
    command: String,
    session_id: String,
    workdir: PathBuf,
    started_at: u64,
    started: Instant,
    state: Mutex<BgTaskState>,
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
    kill_requested: AtomicBool,
}

struct BgTaskState {
    status: BgTaskStatus,
    finished_at: Option<Instant>,
    exit_code: Option<i32>,
    child_pid: Option<u32>,
    child: Option<Child>,
    buffer: BgBuffer,
}

enum OutputEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

impl BgTaskRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                tasks: Mutex::new(HashMap::new()),
                completions: Mutex::new(VecDeque::new()),
            }),
        }
    }

    pub fn spawn(
        &self,
        command: &str,
        session_id: String,
        workdir: PathBuf,
        env: HashMap<String, String>,
        timeout: Option<Duration>,
        storage_dir: Option<PathBuf>,
        max_running: usize,
    ) -> Result<String, String> {
        self.cleanup_finished(Duration::from_secs(30 * 60));

        let running = self.running_count();
        if running >= max_running {
            return Err(format!(
                "background bash task limit exceeded: {running} running (max {max_running})"
            ));
        }

        let task_id = self.generate_unique_task_id()?;
        let output_dir = default_output_dir(storage_dir.as_deref());
        let mut child = shell_command(command)
            .current_dir(&workdir)
            .envs(&env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to spawn background bash command: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "failed to capture stdout".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "failed to capture stderr".to_string())?;
        let child_pid = child.id();
        let (tx, rx) = mpsc::channel::<OutputEvent>();
        let stdout_reader = spawn_reader(stdout, tx.clone(), true);
        let stderr_reader = spawn_reader(stderr, tx, false);

        let task = Arc::new(BgTask {
            task_id: task_id.clone(),
            command: command.to_string(),
            session_id,
            workdir,
            started_at: unix_millis(),
            started: Instant::now(),
            state: Mutex::new(BgTaskState {
                status: BgTaskStatus::Running,
                finished_at: None,
                exit_code: None,
                child_pid: Some(child_pid),
                child: Some(child),
                buffer: BgBuffer::new(&task_id, output_dir),
            }),
            thread_handle: Mutex::new(None),
            kill_requested: AtomicBool::new(false),
        });

        self.inner
            .tasks
            .lock()
            .map_err(|_| "background task registry lock poisoned".to_string())?
            .insert(task_id.clone(), task.clone());

        let inner = self.inner.clone();
        let worker_task = task.clone();
        let handle = thread::spawn(move || {
            run_task(
                worker_task,
                rx,
                stdout_reader,
                stderr_reader,
                timeout,
                inner,
            );
        });

        if let Ok(mut slot) = task.thread_handle.lock() {
            *slot = Some(handle);
        }

        Ok(task_id)
    }

    pub fn status(
        &self,
        task_id: &str,
        session_id: &str,
        preview_bytes: usize,
    ) -> Option<BgTaskSnapshot> {
        let task = self.task_for_session(task_id, session_id)?;
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
            .map(|task| task.snapshot(preview_bytes))
            .collect()
    }

    pub fn kill(&self, task_id: &str, session_id: &str) -> Result<BgTaskSnapshot, String> {
        let task = self
            .task_for_session(task_id, session_id)
            .ok_or_else(|| format!("background task not found: {task_id}"))?;
        task.kill_requested.store(true, Ordering::SeqCst);

        {
            let mut state = task
                .state
                .lock()
                .map_err(|_| "background task lock poisoned".to_string())?;
            if state.status == BgTaskStatus::Running {
                if let Some(child) = state.child.as_mut() {
                    terminate_process(child);
                    let exit_code = child
                        .wait()
                        .ok()
                        .and_then(|status| status.code())
                        .or(Some(-1));
                    state.exit_code = exit_code;
                }
                state.child = None;
                state.child_pid = None;
                state.status = BgTaskStatus::Killed;
                state.finished_at = Some(Instant::now());
            }
        }

        if let Some(handle) = task
            .thread_handle
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            let _ = handle.join();
        }

        Ok(task.snapshot(5 * 1024))
    }

    pub fn cleanup_finished(&self, older_than: Duration) {
        let removable = self
            .inner
            .tasks
            .lock()
            .map(|tasks| {
                tasks
                    .iter()
                    .filter(|(_, task)| task.finished_older_than(older_than))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if removable.is_empty() {
            return;
        }

        if let Ok(mut tasks) = self.inner.tasks.lock() {
            for id in removable {
                if let Some(task) = tasks.remove(&id) {
                    task.cleanup();
                }
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
        let Some(session_id) = session_id else {
            return completions.drain(..).collect();
        };

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
    }

    pub fn shutdown(&self) {
        let tasks = self
            .inner
            .tasks
            .lock()
            .map(|tasks| tasks.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for task_id in tasks {
            if let Some(task) = self.task(&task_id) {
                let _ = self.kill(&task_id, &task.session_id);
            }
        }
        self.cleanup_finished(Duration::ZERO);
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

    /// Generate a `bgb-{8hex}` slug that is unique against both the live task
    /// map and the not-yet-drained completion queue. Re-rolls on collision.
    /// In practice the first roll always succeeds — this guard exists so
    /// agents reusing a task_id during a tight retry can never accidentally
    /// observe a completion belonging to a previous task with the same slug.
    fn generate_unique_task_id(&self) -> Result<String, String> {
        // Bound the loop so a degenerate generator (e.g. a stuck system clock
        // returning identical nanos every call) cannot wedge spawn forever.
        // 32 attempts at ~268M namespace size makes silent failure
        // astronomically unlikely; if it ever happens the agent gets a clean
        // error instead of a hang.
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
            // Also check pending completions — a task that finished but hasn't
            // been drained still owns that ID from the agent's perspective.
            let completions = self
                .inner
                .completions
                .lock()
                .map_err(|_| "background completions lock poisoned".to_string())?;
            if completions.iter().any(|c| c.task_id == candidate) {
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
        let duration_ms = state
            .finished_at
            .map(|finished_at| finished_at.duration_since(self.started).as_millis() as u64);
        let (output_preview, output_truncated) = state.buffer.read_tail(preview_bytes);
        let output_path = state
            .buffer
            .spill_path()
            .map(|path| path.display().to_string());
        BgTaskSnapshot {
            info: BgTaskInfo {
                task_id: self.task_id.clone(),
                status: state.status.clone(),
                command: self.command.clone(),
                started_at: self.started_at,
                duration_ms,
            },
            exit_code: state.exit_code,
            child_pid: state.child_pid,
            workdir: self.workdir.display().to_string(),
            output_preview,
            output_truncated,
            output_path,
        }
    }

    fn is_running(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.status == BgTaskStatus::Running)
            .unwrap_or(false)
    }

    fn finished_older_than(&self, older_than: Duration) -> bool {
        self.state
            .lock()
            .map(|state| {
                state
                    .finished_at
                    .map(|finished_at| finished_at.elapsed() >= older_than)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn completion(&self) -> BgCompletion {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        BgCompletion {
            task_id: self.task_id.clone(),
            session_id: self.session_id.clone(),
            status: state.status.clone(),
            exit_code: state.exit_code,
            command: self.command.clone(),
        }
    }

    fn cleanup(&self) {
        if let Ok(state) = self.state.lock() {
            state.buffer.cleanup();
        }
    }
}

fn run_task(
    task: Arc<BgTask>,
    rx: mpsc::Receiver<OutputEvent>,
    stdout_reader: thread::JoinHandle<()>,
    stderr_reader: thread::JoinHandle<()>,
    timeout: Option<Duration>,
    inner: Arc<RegistryInner>,
) {
    loop {
        drain_output_events(&rx, &task);

        let done = {
            let mut state = match task.state.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            if state.status != BgTaskStatus::Running {
                true
            } else if timeout
                .map(|timeout| task.started.elapsed() >= timeout)
                .unwrap_or(false)
            {
                if let Some(child) = state.child.as_mut() {
                    terminate_process(child);
                    let exit_code = child
                        .wait()
                        .ok()
                        .and_then(|status| status.code())
                        .or(Some(124));
                    state.exit_code = exit_code;
                } else {
                    state.exit_code = Some(124);
                }
                state.status = BgTaskStatus::Failed;
                state.finished_at = Some(Instant::now());
                state.child = None;
                state.child_pid = None;
                true
            } else if let Some(child) = state.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let exit_code = status.code().unwrap_or(-1);
                        state.exit_code = Some(exit_code);
                        state.status = if status.success() {
                            BgTaskStatus::Completed
                        } else {
                            BgTaskStatus::Failed
                        };
                        state.finished_at = Some(Instant::now());
                        state.child = None;
                        state.child_pid = None;
                        true
                    }
                    Ok(None) => false,
                    Err(_) => {
                        if task.kill_requested.load(Ordering::SeqCst) {
                            if state.status == BgTaskStatus::Running {
                                state.status = BgTaskStatus::Killed;
                                state.finished_at = Some(Instant::now());
                            }
                        } else {
                            state.exit_code = Some(-1);
                            state.status = BgTaskStatus::Failed;
                            state.finished_at = Some(Instant::now());
                        }
                        state.child = None;
                        state.child_pid = None;
                        true
                    }
                }
            } else {
                true
            }
        };

        if done {
            break;
        }

        thread::sleep(POLL_INTERVAL);
    }

    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    drain_output_events(&rx, &task);

    inner
        .completions
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push_back(task.completion());
}

fn drain_output_events(rx: &mpsc::Receiver<OutputEvent>, task: &BgTask) {
    while let Ok(event) = rx.try_recv() {
        if let Ok(mut state) = task.state.lock() {
            match event {
                OutputEvent::Stdout(bytes) => state.buffer.append(StreamKind::Stdout, &bytes),
                OutputEvent::Stderr(bytes) => state.buffer.append(StreamKind::Stderr, &bytes),
            }
        }
    }
}

fn spawn_reader<R>(
    mut reader: R,
    tx: mpsc::Sender<OutputEvent>,
    is_stdout: bool,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let bytes = buffer[..n].to_vec();
                    let event = if is_stdout {
                        OutputEvent::Stdout(bytes)
                    } else {
                        OutputEvent::Stderr(bytes)
                    };
                    if tx.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        command,
    ]);
    cmd
}

#[cfg(not(windows))]
fn shell_command(command: &str) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", command]);
    cmd.process_group(0);
    cmd
}

#[cfg(unix)]
fn terminate_process(child: &mut Child) {
    let pgid = child.id() as i32;
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }
    let grace_started = Instant::now();
    while grace_started.elapsed() < TERMINATE_GRACE {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn terminate_process(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("taskkill")
        .args(["/PID", &pid, "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Generate a short, agent-friendly bg-bash task slug like `bgb-3f49a42c`.
///
/// Format: `bgb-` plus 8 lowercase hex chars (32 bits = ~4 billion namespace).
/// Birthday-collision risk at the typical session scale (≤1000 active tasks +
/// drained completions) is ~0.012% (≈1 in 8600); the slug is still re-rolled
/// on collision against both the live task map and the not-yet-
/// drained completion queue, so duplicates are impossible in practice.
///
/// **Why the atomic counter instead of pure time-based entropy:** rapid
/// successive spawns within the same nanosecond (which happens regularly
/// on macOS where the realtime clock has microsecond resolution) would
/// otherwise produce the same slug. The counter guarantees a fresh seed
/// for each call even when the clock hasn't ticked.
fn random_slug() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    // Mix nanos, pid, and counter through a wide multiplier so adjacent
    // counter values don't produce visually-similar slugs. Truncating to u32
    // gives the agent a 8-hex-char slug; the rejection loop in
    // `generate_unique_task_id` covers any residual collisions.
    let mixed = unix_millis_nanos()
        ^ (std::process::id() as u128).wrapping_mul(0x9E3779B97F4A7C15)
        ^ (counter as u128).wrapping_mul(0xBF58476D1CE4E5B9);
    format!("bgb-{:08x}", (mixed as u32))
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn unix_millis_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}
