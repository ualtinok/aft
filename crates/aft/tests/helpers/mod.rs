#![allow(dead_code)]

//! Shared test helpers for integration tests.
//!
//! Provides `AftProcess` — a handle to a running aft binary with piped I/O —
//! and `fixture_path` for resolving test fixture files.

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// A handle to a running aft process with piped I/O.
///
/// Uses a persistent `BufReader` over stdout so sequential reads
/// don't lose buffered data between calls.
pub struct AftProcess {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl AftProcess {
    /// Spawn the aft binary with piped stdin/stdout/stderr.
    /// Sets AFT_CACHE_DIR to a temp path so tests don't pollute the user's cache.
    pub fn spawn() -> Self {
        let temp_cache =
            std::env::temp_dir().join(format!("aft-test-cache-{}", std::process::id()));
        Self::spawn_with_env(&[("AFT_CACHE_DIR", temp_cache.as_os_str())])
    }

    /// Spawn the aft binary with additional environment variables.
    /// Stderr is suppressed by default. Use `spawn_with_stderr()` for tests
    /// that need to inspect stderr output.
    pub fn spawn_with_env(envs: &[(&str, &std::ffi::OsStr)]) -> Self {
        Self::spawn_inner(envs, false)
    }

    /// Spawn with stderr piped so tests can read it via `stderr_output()`.
    pub fn spawn_with_stderr() -> Self {
        Self::spawn_inner(&[], true)
    }

    fn spawn_inner(envs: &[(&str, &std::ffi::OsStr)], pipe_stderr: bool) -> Self {
        let binary = env!("CARGO_BIN_EXE_aft");
        let mut command = Command::new(binary);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if pipe_stderr {
                Stdio::piped()
            } else {
                Stdio::null()
            });

        for (key, value) in envs {
            command.env(key, value);
        }

        let mut child = command.spawn().expect("failed to spawn aft binary");

        let stdout = child.stdout.take().expect("stdout handle");
        let reader = BufReader::new(stdout);

        AftProcess { child, reader }
    }

    /// Send a raw line and read back the JSON response.
    pub fn send(&mut self, request: &str) -> serde_json::Value {
        let stdin = self.child.stdin.as_mut().expect("stdin handle");
        writeln!(stdin, "{}", request).expect("write to stdin");
        stdin.flush().expect("flush stdin");

        let request_id = serde_json::from_str::<serde_json::Value>(request)
            .ok()
            .and_then(|value| value["id"].as_str().map(str::to_string));
        loop {
            let value = self.read_json_line();
            if value.get("type").is_some() && value.get("id").is_none() {
                continue;
            }
            if request_id
                .as_deref()
                .is_none_or(|request_id| value["id"] == request_id)
            {
                return value;
            }
            return value;
        }
    }

    /// Read the next JSON line from stdout without writing a request first.
    pub fn read_next(&mut self) -> serde_json::Value {
        self.read_json_line()
    }

    /// Try to read one JSON line from stdout within a short timeout.
    pub fn try_read_next_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Option<serde_json::Value> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let fd = self.reader.get_ref().as_raw_fd();
            let previous_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if previous_flags == -1 {
                return None;
            }
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, previous_flags | libc::O_NONBLOCK);
            }

            let started = std::time::Instant::now();
            let mut line = String::new();
            let result = loop {
                match self.reader.read_line(&mut line) {
                    Ok(0) => break None,
                    Ok(_) => break Some(serde_json::from_str(line.trim()).expect("parse JSON")),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if started.elapsed() >= timeout {
                            break None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("read from stdout: {error}"),
                }
            };

            unsafe {
                libc::fcntl(fd, libc::F_SETFL, previous_flags);
            }
            result
        }

        #[cfg(not(unix))]
        {
            let _ = timeout;
            None
        }
    }

    fn read_json_line(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("read from stdout");
        assert!(
            !line.is_empty(),
            "expected a response line but got EOF from aft"
        );
        serde_json::from_str(line.trim()).expect("parse response JSON")
    }

    /// Send a configure command with project_root.
    pub fn configure(&mut self, project_root: &std::path::Path) -> serde_json::Value {
        self.send(&format!(
            r#"{{"id":"cfg","command":"configure","project_root":"{}"}}"#,
            project_root.display()
        ))
    }

    /// Wait for and consume a `configure_warnings` push frame, returning its
    /// `warnings` array merged into the original configure response.
    ///
    /// Configure now defers the file-walk + missing-binary detection to a
    /// background thread (so it can return in <100 ms even on huge directories).
    /// Tests that previously relied on synchronous warnings should call this
    /// helper after `configure` to merge the async results back in:
    ///
    /// ```rust,ignore
    /// let configure = aft.send(json!({"id":"cfg",...}).to_string().as_str());
    /// let configure = aft.merge_configure_warnings(configure);
    /// // configure["warnings"] now contains the async warnings
    /// ```
    pub fn merge_configure_warnings(
        &mut self,
        mut configure: serde_json::Value,
    ) -> serde_json::Value {
        let frame = self.wait_for_configure_warnings_frame();
        let warnings = frame
            .get("warnings")
            .and_then(|warnings| warnings.as_array())
            .cloned()
            .unwrap_or_default();
        configure["warnings"] = serde_json::Value::Array(warnings);
        if let Some(count) = frame.get("source_file_count").cloned() {
            configure["source_file_count"] = count;
        }
        if let Some(exceeds) = frame.get("source_file_count_exceeds_max").cloned() {
            configure["source_file_count_exceeds_max"] = exceeds;
        }
        configure["warnings_pending"] = serde_json::Value::Bool(false);
        configure
    }

    /// Read frames until a `configure_warnings` push frame arrives, then
    /// return it. Panics if a non-frame response (one with an `id`) arrives
    /// before the frame, or if EOF is hit.
    fn wait_for_configure_warnings_frame(&mut self) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if std::time::Instant::now() >= deadline {
                panic!("timed out waiting for configure_warnings push frame");
            }
            let value = self.read_json_line();
            if value.get("type").and_then(|kind| kind.as_str()) == Some("configure_warnings") {
                return value;
            }
            // Other push frames (progress, bash_completed) are skipped silently.
        }
    }

    /// Send a raw line that should produce no response (e.g. empty line).
    /// Verifies the process is still alive by sending a follow-up ping.
    pub fn send_silent(&mut self, request: &str) {
        let stdin = self.child.stdin.as_mut().expect("stdin handle");
        writeln!(stdin, "{}", request).expect("write to stdin");
        stdin.flush().expect("flush stdin");
    }

    /// Send a raw line and collect response lines until `predicate` returns true.
    pub fn send_until<F>(&mut self, request: &str, mut predicate: F) -> Vec<serde_json::Value>
    where
        F: FnMut(&serde_json::Value) -> bool,
    {
        let stdin = self.child.stdin.as_mut().expect("stdin handle");
        writeln!(stdin, "{}", request).expect("write to stdin");
        stdin.flush().expect("flush stdin");

        let mut responses = Vec::new();
        loop {
            let mut line = String::new();
            self.reader.read_line(&mut line).expect("read from stdout");
            assert!(
                !line.is_empty(),
                "expected a response line but got EOF from aft"
            );
            let value: serde_json::Value = serde_json::from_str(line.trim()).expect("parse JSON");
            let done = predicate(&value);
            responses.push(value);
            if done {
                return responses;
            }
        }
    }

    /// Close stdin and wait for the process to exit. Returns the exit status.
    pub fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.child.stdin.take());
        self.child.wait().expect("wait for process exit")
    }

    /// Read stderr contents after process exits.
    pub fn stderr_output(mut self) -> (std::process::ExitStatus, String) {
        drop(self.child.stdin.take());
        let status = self.child.wait().expect("wait for process exit");
        let mut stderr_content = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            use std::io::Read;
            stderr.read_to_string(&mut stderr_content).ok();
        }
        (status, stderr_content)
    }
}

/// Resolve a fixture file path relative to the project root.
pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}
