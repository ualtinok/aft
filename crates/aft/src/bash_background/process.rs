/// Shared process-termination helpers for both foreground bash and background
/// bash tasks. Extracted to avoid duplication between `commands/bash.rs` and
/// `bash_background/registry.rs`.
///
/// Termination is graceful-first: SIGTERM + 3-second grace period, then
/// SIGKILL on Unix. On Windows, `taskkill /T /F` kills the entire process tree.
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const TERMINATE_GRACE: Duration = Duration::from_secs(3);

#[cfg(unix)]
pub fn terminate_process(child: &mut Child) {
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
pub fn terminate_process(child: &mut Child) {
    let pid = child.id().to_string();
    let _ = Command::new("taskkill")
        .args(["/PID", &pid, "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
