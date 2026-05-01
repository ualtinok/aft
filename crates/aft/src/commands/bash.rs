use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::json;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::bash_background::process::terminate_process;
use crate::context::AppContext;
use crate::protocol::{
    ProgressFrame, ProgressKind, RawRequest, Response, ERROR_PERMISSION_REQUIRED,
};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const INLINE_OUTPUT_LIMIT: usize = 30 * 1024;
const BLOCKED_ENV_VARS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "BASH_ENV",
    "ENV",
    "IFS",
    "PATH",
];

#[derive(Debug, Deserialize)]
struct BashParams {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    workdir: Option<PathBuf>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    background: bool,
    #[serde(default = "default_compressed")]
    compressed: bool,
    #[serde(default)]
    permissions_granted: Vec<String>,
    #[serde(default)]
    permissions_requested: bool,
    #[serde(default)]
    env: HashMap<String, String>,
}

struct ExecutionResult {
    output: String,
    exit_code: i32,
    duration_ms: u64,
    timed_out: bool,
}

enum OutputEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

pub fn handle(req: &RawRequest, ctx: &AppContext) -> Response {
    let raw_params = req
        .params
        .get("params")
        .cloned()
        .unwrap_or_else(|| req.params.clone());
    let params = match serde_json::from_value::<BashParams>(raw_params) {
        Ok(params) => params,
        Err(e) => {
            return Response::error(
                &req.id,
                "invalid_request",
                format!("bash: invalid params: {e}"),
            );
        }
    };

    if let Some(description) = params.description.as_deref() {
        log::debug!("bash description: {description}");
    }

    if let Some(blocked) = blocked_env_var(&params.env) {
        return Response::error(
            &req.id,
            "blocked_env_var",
            format!("bash env contains blocked variable: {blocked}"),
        );
    }

    let workdir = params
        .workdir
        .clone()
        .unwrap_or_else(|| default_workdir(ctx));
    let permission_asks = if params.permissions_requested || ctx.config().bash_permissions {
        crate::bash_permissions::scan::scan_with_cwd(&params.command, ctx, &workdir)
    } else {
        Vec::new()
    };
    if !permission_asks.is_empty()
        && !permissions_granted_cover(&permission_asks, &params.permissions_granted)
    {
        return Response::error_with_data(
            &req.id,
            ERROR_PERMISSION_REQUIRED,
            "bash command requires permission",
            json!({ "asks": permission_asks }),
        );
    }

    if params.background {
        let workdir = params.workdir.clone();
        let env = (!params.env.is_empty()).then_some(params.env.clone());
        return crate::bash_background::spawn(
            &req.id,
            req.session(),
            &params.command,
            workdir,
            env,
            params.timeout,
            ctx,
        );
    }

    if let Some(mut response) =
        crate::bash_rewrite::try_rewrite(&params.command, req.session_id.as_deref(), ctx)
    {
        // Rewriter rules build their own internal request with a placeholder id
        // (e.g. "bash_rewrite") to call into read/grep/glob handlers. Stamp the
        // original bash request id back onto the response so the bridge correlates
        // it with the in-flight `send()` instead of timing out.
        response.id = req.id.clone();
        return response;
    }

    let timeout = params.timeout.map(Duration::from_millis);
    let mut result = match spawn_command(
        &req.id,
        &params.command,
        &workdir,
        timeout,
        &params.env,
        ctx,
    ) {
        Ok(result) => result,
        Err(message) => return Response::error(&req.id, "execution_failed", message),
    };

    if params.compressed {
        result.output = crate::compress::compress(&params.command, result.output, ctx);
    }

    let (output, truncated, output_path) = match maybe_truncate(&result.output, ctx) {
        Ok(truncated) => truncated,
        Err(message) => return Response::error(&req.id, "output_write_failed", message),
    };

    Response::success(
        &req.id,
        json!({
            "output": output,
            "exit_code": result.exit_code,
            "duration_ms": result.duration_ms,
            "truncated": truncated,
            "output_path": output_path,
            "timed_out": result.timed_out,
        }),
    )
}

fn blocked_env_var(env: &HashMap<String, String>) -> Option<&str> {
    env.keys()
        .find(|key| {
            BLOCKED_ENV_VARS.iter().any(|blocked| {
                #[cfg(windows)]
                {
                    key.eq_ignore_ascii_case(blocked)
                }
                #[cfg(not(windows))]
                {
                    key.as_str() == *blocked
                }
            })
        })
        .map(String::as_str)
}

fn permissions_granted_cover(
    asks: &[crate::bash_permissions::PermissionAsk],
    granted: &[String],
) -> bool {
    if asks.is_empty() {
        return true;
    }
    if granted.is_empty() {
        return false;
    }

    asks.iter().all(|ask| {
        ask.patterns
            .iter()
            .chain(ask.always.iter())
            .any(|pattern| granted.iter().any(|grant| grant == pattern))
    })
}

fn default_compressed() -> bool {
    true
}

fn default_workdir(ctx: &AppContext) -> PathBuf {
    // Prefer the configured project root so bash commands run against the
    // user's project rather than the (often unrelated) cwd of the long-lived
    // aft worker process. Falls back to process cwd only when no project root
    // is configured (e.g. direct CLI usage).
    if let Some(root) = ctx.config().project_root.clone() {
        return root;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn spawn_command(
    request_id: &str,
    command: &str,
    workdir: &Path,
    timeout: Option<Duration>,
    env: &HashMap<String, String>,
    ctx: &AppContext,
) -> Result<ExecutionResult, String> {
    let started = Instant::now();
    let mut child = shell_command(command)
        .current_dir(workdir)
        .envs(env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn bash command: {e}"))?;

    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;
    let (tx, rx) = mpsc::channel::<OutputEvent>();
    let stdout_reader = spawn_reader(stdout, tx.clone(), true);
    let stderr_reader = spawn_reader(stderr, tx, false);

    let effective_timeout = timeout.unwrap_or(Duration::from_millis(DEFAULT_TIMEOUT_MS));
    let mut timed_out = false;
    let mut output = String::new();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                drain_output_events(request_id, ctx, &rx, &mut output);
                if started.elapsed() >= effective_timeout {
                    timed_out = true;
                    terminate_process(&mut child);
                    break child
                        .wait()
                        .map_err(|e| format!("failed to wait after timeout: {e}"))?;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("failed to poll bash command: {e}")),
        }
    };

    let _ = stdout_reader.join();
    let _ = stderr_reader.join();
    drain_output_events(request_id, ctx, &rx, &mut output);

    Ok(ExecutionResult {
        output,
        exit_code: status.code().unwrap_or(if timed_out { 124 } else { -1 }),
        duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        timed_out,
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

fn spawn_reader<R>(
    mut reader: R,
    tx: mpsc::Sender<OutputEvent>,
    is_stdout: bool,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    let handle = thread::spawn(move || {
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
    });
    handle
}

fn drain_output_events(
    request_id: &str,
    ctx: &AppContext,
    rx: &mpsc::Receiver<OutputEvent>,
    output: &mut String,
) {
    while let Ok(event) = rx.try_recv() {
        let (kind, bytes) = match event {
            OutputEvent::Stdout(bytes) => (ProgressKind::Stdout, bytes),
            OutputEvent::Stderr(bytes) => (ProgressKind::Stderr, bytes),
        };
        let chunk = String::from_utf8_lossy(&bytes).into_owned();
        output.push_str(&chunk);
        ctx.emit_progress(ProgressFrame::new(request_id, kind, chunk));
    }
}

fn maybe_truncate(
    output: &str,
    ctx: &AppContext,
) -> Result<(String, bool, Option<String>), String> {
    if output.len() <= INLINE_OUTPUT_LIMIT {
        return Ok((output.to_string(), false, None));
    }

    let dir = bash_output_dir(ctx);
    fs::create_dir_all(&dir).map_err(|e| {
        format!(
            "failed to create bash output directory {}: {e}",
            dir.display()
        )
    })?;
    let path = dir.join(format!("{}.txt", random_id()));
    fs::write(&path, output)
        .map_err(|e| format!("failed to write bash output {}: {e}", path.display()))?;

    let start = inline_output_suffix_start(output);
    Ok((
        output[start..].to_string(),
        true,
        Some(path.display().to_string()),
    ))
}

/// Compute the byte index where the last `INLINE_OUTPUT_LIMIT` bytes of
/// `output` start, snapped forward to a UTF-8 character boundary so we
/// never split a multi-byte char.
///
/// The earlier implementation walked `char_indices().rev().find_map(...)`,
/// which returned the LAST char's start index on the very first iteration
/// (because `output.len() - idx == 1 <= INLINE_OUTPUT_LIMIT`). That bug
/// made the inline preview a single character for any output above the
/// limit. This helper computes the suffix start by byte arithmetic and
/// keeps approximately `INLINE_OUTPUT_LIMIT` trailing bytes intact.
fn inline_output_suffix_start(output: &str) -> usize {
    let mut start = output.len().saturating_sub(INLINE_OUTPUT_LIMIT);
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }
    start
}

fn bash_output_dir(ctx: &AppContext) -> PathBuf {
    if let Some(dir) = std::env::var_os("AFT_CACHE_DIR") {
        return PathBuf::from(dir).join("aft").join("bash-output");
    }
    if let Some(dir) = ctx.config().storage_dir.clone() {
        return dir.join("bash-output");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".cache").join("aft").join("bash-output")
}

fn random_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{}", std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: prior reverse `char_indices` logic returned only the LAST
    /// character of `output` because the first reverse-iteration index already
    /// satisfied `output.len() - idx == 1 <= INLINE_OUTPUT_LIMIT`. The new
    /// implementation must keep approximately `INLINE_OUTPUT_LIMIT` trailing
    /// bytes intact for ASCII input.
    #[test]
    fn inline_output_suffix_keeps_full_limit_for_ascii() {
        let total = INLINE_OUTPUT_LIMIT * 2;
        let output: String = "x".repeat(total);
        let start = inline_output_suffix_start(&output);
        let suffix_len = output.len() - start;
        assert!(
            suffix_len > INLINE_OUTPUT_LIMIT / 2,
            "ascii suffix too short: got {suffix_len} bytes (limit={INLINE_OUTPUT_LIMIT})"
        );
        assert!(
            suffix_len <= INLINE_OUTPUT_LIMIT,
            "ascii suffix exceeded limit: got {suffix_len} bytes (limit={INLINE_OUTPUT_LIMIT})"
        );
        // Guard against a regression to the 1-char bug.
        assert!(suffix_len > 1, "suffix collapsed to a single character");
    }

    /// The suffix-start index must always land on a UTF-8 char boundary so
    /// `output[start..]` is a valid `&str`. Multi-byte chars (like 4-byte
    /// emoji) require boundary snapping when the raw byte split lands inside
    /// a code point.
    #[test]
    fn inline_output_suffix_respects_utf8_boundaries() {
        // Each crab is 4 bytes. 20_000 of them = 80_000 bytes, well over the
        // inline limit. The byte index `len - INLINE_OUTPUT_LIMIT` is unlikely
        // to be a 4-byte boundary.
        let output: String = "🦀".repeat(20_000);
        let start = inline_output_suffix_start(&output);
        assert!(
            output.is_char_boundary(start),
            "suffix split a multi-byte char"
        );
        // Slicing must succeed without panic.
        let suffix = &output[start..];
        let suffix_bytes = suffix.len();
        assert!(
            suffix_bytes <= INLINE_OUTPUT_LIMIT + 4,
            "utf8 suffix far above limit: got {suffix_bytes} bytes (limit={INLINE_OUTPUT_LIMIT})"
        );
        assert!(
            suffix_bytes > INLINE_OUTPUT_LIMIT / 2,
            "utf8 suffix too short: got {suffix_bytes} bytes (limit={INLINE_OUTPUT_LIMIT})"
        );
    }

    /// Output below the inline limit is returned by `maybe_truncate` directly,
    /// but the helper must still return `0` so callers slicing `output[start..]`
    /// get the full string.
    #[test]
    fn inline_output_suffix_returns_zero_for_short_input() {
        let output = "small";
        assert_eq!(inline_output_suffix_start(output), 0);
    }
}
