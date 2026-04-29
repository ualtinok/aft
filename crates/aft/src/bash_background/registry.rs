use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::buffer::{default_output_dir, BgBuffer, StreamKind};
use super::{BgTaskInfo, BgTaskStatus};

const TERMINATE_GRACE: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Serialize)]
pub struct BgCompletion {
    pub task_id: String,
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
    workdir: PathBuf,
    started_at: u64,
    started: Instant,
    state: Mutex<BgTaskState>,
    thread_handle: Mutex<Option<thread::JoinHandle<()>>>,
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
        workdir: PathBuf,
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

        let task_id = random_id();
        let output_dir = default_output_dir(storage_dir.as_deref());
        let mut child = shell_command(command)
            .current_dir(&workdir)
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
        });

        self.inner
            .tasks
            .lock()
            .map_err(|_| "background task registry lock poisoned".to_string())?
            .insert(task_id.clone(), task.clone());

        let inner = self.inner.clone();
        let worker_task = task.clone();
        let handle = thread::spawn(move || {
            run_task(worker_task, rx, stdout_reader, stderr_reader, inner);
        });

        if let Ok(mut slot) = task.thread_handle.lock() {
            *slot = Some(handle);
        }

        Ok(task_id)
    }

    pub fn status(&self, task_id: &str, preview_bytes: usize) -> Option<BgTaskSnapshot> {
        let task = self.task(task_id)?;
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

    pub fn kill(&self, task_id: &str) -> Result<BgTaskSnapshot, String> {
        let task = self
            .task(task_id)
            .ok_or_else(|| format!("background task not found: {task_id}"))?;

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
        let mut completions = match self.inner.completions.lock() {
            Ok(completions) => completions,
            Err(_) => return Vec::new(),
        };
        completions.drain(..).collect()
    }

    pub fn shutdown(&self) {
        let tasks = self
            .inner
            .tasks
            .lock()
            .map(|tasks| tasks.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for task_id in tasks {
            let _ = self.kill(&task_id);
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

    fn running_count(&self) -> usize {
        self.inner
            .tasks
            .lock()
            .map(|tasks| tasks.values().filter(|task| task.is_running()).count())
            .unwrap_or(0)
    }
}

impl Default for BgTaskRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BgTask {
    fn snapshot(&self, preview_bytes: usize) -> BgTaskSnapshot {
        let state = self.state.lock().expect("background task lock poisoned");
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
        let state = self.state.lock().expect("background task lock poisoned");
        BgCompletion {
            task_id: self.task_id.clone(),
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
                        state.exit_code = Some(-1);
                        state.status = BgTaskStatus::Failed;
                        state.finished_at = Some(Instant::now());
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

    if let Ok(mut completions) = inner.completions.lock() {
        completions.push_back(task.completion());
    }
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
    cmd
}

#[cfg(unix)]
fn terminate_process(child: &mut Child) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let grace_started = Instant::now();
    while grace_started.elapsed() < TERMINATE_GRACE {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    unsafe {
        libc::kill(pid, libc::SIGKILL);
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

fn random_id() -> String {
    format!("{}-{}", std::process::id(), unix_millis_nanos())
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
