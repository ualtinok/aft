import { type ChildProcess, spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import { error, getLogFilePath, log, warn } from "./logger.js";

const DEFAULT_BRIDGE_TIMEOUT_MS = 30_000;
const SEMANTIC_TIMEOUT_SAFETY_MARGIN_MS = 5_000;

// ## Note on TypeScript `as` type assertions
//
// Bridge responses use `as string`, `as string[]` etc. in several places.
// This is intentional: all 16 tool handlers already guard against error
// responses with `if (response.success === false) throw ...` before accessing
// typed fields. The remaining `as` casts are on fields from known-success
// Rust responses where the shape is guaranteed by the protocol contract.
// Adding Zod runtime validation for every bridge response would add ~2ms
// per call with no practical safety benefit given the error guards.

/**
 * Compare two semver version strings (major.minor.patch).
 * Returns: negative if a < b, 0 if equal, positive if a > b.
 */
function compareSemver(a: string, b: string): number {
  const pa = a.split(".").map(Number);
  const pb = b.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    const diff = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

function clampSemanticTimeout(
  configOverrides: Record<string, unknown>,
  bridgeTimeoutMs: number,
): Record<string, unknown> {
  const semantic = configOverrides.semantic;
  if (!semantic || typeof semantic !== "object" || Array.isArray(semantic)) {
    return configOverrides;
  }

  const timeoutMs = (semantic as { timeout_ms?: unknown }).timeout_ms;
  if (typeof timeoutMs !== "number" || !Number.isFinite(timeoutMs)) {
    return configOverrides;
  }

  const maxSemanticTimeoutMs =
    bridgeTimeoutMs > SEMANTIC_TIMEOUT_SAFETY_MARGIN_MS
      ? bridgeTimeoutMs - SEMANTIC_TIMEOUT_SAFETY_MARGIN_MS
      : Math.max(1, bridgeTimeoutMs - 1);

  if (timeoutMs <= maxSemanticTimeoutMs) {
    return configOverrides;
  }

  warn(
    `semantic.timeout_ms=${timeoutMs} exceeds bridge timeout budget; clamping to ${maxSemanticTimeoutMs}ms (bridge timeout: ${bridgeTimeoutMs}ms)`,
  );

  return {
    ...configOverrides,
    semantic: {
      ...semantic,
      timeout_ms: maxSemanticTimeoutMs,
    },
  };
}

interface PendingRequest {
  resolve: (value: Record<string, unknown>) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export interface BridgeOptions {
  /** Request timeout in milliseconds. Default: 30000 */
  timeoutMs?: number;
  /** Maximum restart attempts before giving up. Default: 3 */
  maxRestarts?: number;
  /** Minimum binary version required (semver). If the binary is older, onVersionMismatch is called. */
  minVersion?: string;
  /** Called when binary version is older than minVersion. Receives (binaryVersion, minVersion). */
  onVersionMismatch?: (binaryVersion: string, minVersion: string) => void;
}

/**
 * Manages a persistent `aft` child process, communicating via NDJSON over
 * stdin/stdout. Lazy-spawns on first `send()` call. Handles crash detection
 * with exponential backoff auto-restart.
 */
export class BinaryBridge {
  private static readonly RESTART_RESET_MS = 5 * 60 * 1000;
  /** How many recent stderr lines to keep for crash diagnostics. */
  private static readonly STDERR_TAIL_MAX = 20;

  private binaryPath: string;
  private cwd: string;
  private process: ChildProcess | null = null;
  private pending = new Map<string, PendingRequest>();
  private nextId = 1;
  private stdoutBuffer = "";
  /** Ring buffer of the last N stderr lines, cleared on every spawn. */
  private stderrTail: string[] = [];
  private _restartCount = 0;
  private _shuttingDown = false;
  private timeoutMs: number;
  private maxRestarts: number;
  private configured = false;
  private _configurePromise: Promise<void> | null = null;
  private configOverrides: Record<string, unknown>;
  private minVersion: string | undefined;
  private onVersionMismatch: ((binaryVersion: string, minVersion: string) => void) | undefined;
  private restartResetTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    binaryPath: string,
    cwd: string,
    options?: BridgeOptions,
    configOverrides?: Record<string, unknown>,
  ) {
    this.binaryPath = binaryPath;
    this.cwd = cwd;
    this.timeoutMs = options?.timeoutMs ?? DEFAULT_BRIDGE_TIMEOUT_MS;
    this.maxRestarts = options?.maxRestarts ?? 3;
    this.configOverrides = clampSemanticTimeout(configOverrides ?? {}, this.timeoutMs);
    this.minVersion = options?.minVersion;
    this.onVersionMismatch = options?.onVersionMismatch;
  }

  /** Number of times the binary has been restarted after a crash. */
  get restartCount(): number {
    return this._restartCount;
  }

  /** Whether the child process is currently alive. */
  isAlive(): boolean {
    return this.process !== null && this.process.exitCode === null && !this.process.killed;
  }

  /**
   * Send a command to the binary and return the parsed response.
   * Lazy-spawns the binary on first call.
   */
  async send(
    command: string,
    params: Record<string, unknown> = {},
  ): Promise<Record<string, unknown>> {
    if (this._shuttingDown) {
      throw new Error(`[aft-plugin] Bridge is shutting down, cannot send "${command}"`);
    }

    this.ensureSpawned();

    // Auto-configure project root + plugin config on first command, then check version.
    // configured is set AFTER success to prevent skipping configuration on failure (#18).
    // When multiple parallel calls arrive before configure completes, they all await
    // the same promise instead of each independently trying to configure.
    if (!this.configured) {
      if (command !== "configure" && command !== "version") {
        if (!this._configurePromise) {
          // First caller — create the configure promise.
          // All parallel callers await this same promise.
          this._configurePromise = (async () => {
            try {
              const configResult = await this.send("configure", {
                project_root: this.cwd,
                ...this.configOverrides,
              });
              if (configResult.success === false) {
                throw new Error(
                  `[aft-plugin] Configure failed: ${configResult.message ?? "unknown error"}`,
                );
              }
              await this.checkVersion();
              // Re-check liveness after version check — checkVersion() swallows
              // errors as best-effort, so the bridge may have died without throwing.
              if (!this.isAlive()) {
                throw new Error(
                  `[aft-plugin] Bridge died during version check. Check logs: ${getLogFilePath()}`,
                );
              }
              this.configured = true;
            } finally {
              this._configurePromise = null;
            }
          })();
        }

        // All callers (including the first) await the shared promise
        await this._configurePromise;
      }
    }

    const id = String(this.nextId++);
    const request = { id, command, ...params };
    const line = `${JSON.stringify(request)}\n`;

    return new Promise<Record<string, unknown>>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        warn(
          `Request "${command}" (id=${id}) timed out after ${this.timeoutMs}ms — restarting bridge`,
        );
        reject(
          new Error(
            `[aft-plugin] Request "${command}" (id=${id}) timed out after ${this.timeoutMs}ms`,
          ),
        );
        // Kill the hung process so the next request gets a fresh bridge
        this.handleTimeout();
      }, this.timeoutMs);

      this.pending.set(id, { resolve, reject, timer });

      if (!this.process?.stdin?.writable) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(new Error(`[aft-plugin] stdin not writable for command "${command}"`));
        return;
      }

      this.process.stdin.write(line, (err) => {
        if (err) {
          const entry = this.pending.get(id);
          if (entry) {
            this.pending.delete(id);
            clearTimeout(entry.timer);
            entry.reject(new Error(`[aft-plugin] Failed to write to stdin: ${err.message}`));
          }
        }
      });
    });
  }

  /** Kill the child process and reject all pending requests. */
  async shutdown(): Promise<void> {
    this._shuttingDown = true;
    this.clearRestartResetTimer();
    this.rejectAllPending(new Error("[aft-plugin] Bridge shutting down"));

    if (this.process) {
      const proc = this.process;
      this.process = null;

      return new Promise<void>((resolve) => {
        const forceKillTimer = setTimeout(() => {
          proc.kill("SIGKILL");
          resolve();
        }, 5_000);

        proc.once("exit", () => {
          clearTimeout(forceKillTimer);
          log("Process exited during shutdown");
          resolve();
        });

        proc.kill("SIGTERM");
      });
    }
  }

  // ---- Internal ----

  /** Query binary version and compare against minVersion. Calls onVersionMismatch if outdated. */
  private async checkVersion(): Promise<void> {
    if (!this.minVersion) return;
    try {
      const resp = await this.send("version");
      const binaryVersion = resp.version as string | undefined;
      if (!binaryVersion) {
        log("Binary did not report a version — skipping version check");
        return;
      }
      log(`Binary version: ${binaryVersion}`);
      if (compareSemver(binaryVersion, this.minVersion) < 0) {
        warn(`Binary version ${binaryVersion} is older than required ${this.minVersion}`);
        this.onVersionMismatch?.(binaryVersion, this.minVersion);
      }
    } catch (err) {
      // Version check is best-effort — don't block tool usage if it fails
      warn(`Version check failed: ${(err as Error).message}`);
    }
  }

  private ensureSpawned(): void {
    if (this.isAlive()) return;
    this.spawnProcess();
  }

  private spawnProcess(): void {
    log(`Spawning binary: ${this.binaryPath} (cwd: ${this.cwd})`);
    const semantic = this.configOverrides.semantic;
    const semanticBackend = (() => {
      if (semantic && typeof semantic === "object" && !Array.isArray(semantic)) {
        const candidate = (semantic as { backend?: unknown }).backend;
        return typeof candidate === "string" ? candidate : undefined;
      }
      return undefined;
    })();
    const useFastembedBackend =
      semanticBackend === undefined || semanticBackend === "fastembed" || semanticBackend === "";

    const ortDir =
      typeof this.configOverrides._ort_dylib_dir === "string" && useFastembedBackend
        ? this.configOverrides._ort_dylib_dir
        : null;
    const ortLibraryPath =
      ortDir == null
        ? null
        : join(
            ortDir,
            process.platform === "win32"
              ? "onnxruntime.dll"
              : process.platform === "darwin"
                ? "libonnxruntime.dylib"
                : "libonnxruntime.so",
          );
    const envPath =
      process.platform === "win32" && ortDir
        ? `${ortDir};${process.env.PATH ?? ""}`
        : process.env.PATH;

    const env: NodeJS.ProcessEnv = {
      ...process.env,
      ...(envPath ? { PATH: envPath } : {}),
    };

    if (useFastembedBackend) {
      // Store fastembed model files alongside the semantic index, not the project cwd.
      // This is only relevant when the fastembed backend is selected.
      env.FASTEMBED_CACHE_DIR =
        process.env.FASTEMBED_CACHE_DIR ||
        (typeof this.configOverrides.storage_dir === "string"
          ? join(this.configOverrides.storage_dir, "semantic", "models")
          : join(homedir() || "", ".cache", "fastembed"));

      // Point ort to the auto-downloaded or system ONNX Runtime library.
      if (ortLibraryPath) {
        env.ORT_DYLIB_PATH = ortLibraryPath;
      }
    }

    const child = spawn(this.binaryPath, [], {
      cwd: this.cwd,
      stdio: ["pipe", "pipe", "pipe"],
      env,
    });

    child.stdout?.on("data", (chunk: Buffer) => {
      this.onStdoutData(chunk.toString("utf-8"));
    });

    child.stderr?.on("data", (chunk: Buffer) => {
      const lines = chunk.toString("utf-8").trimEnd().split("\n");
      for (const line of lines) {
        if (!line) continue;
        // Strip Rust env_logger prefix and re-tag under [aft]
        const stripped = line.replace(/^\[aft\]\s*/, "");
        log(`[aft] ${stripped}`);
        this.pushStderrLine(stripped);
      }
    });

    child.on("error", (err) => {
      error(`Process error: ${err.message}${this.formatStderrTail()}`);
      this.handleCrash();
    });

    child.on("exit", (code, signal) => {
      if (this._shuttingDown) return;
      log(`Process exited: code=${code}, signal=${signal}`);
      // External termination signals (SIGTERM/SIGKILL/SIGHUP/SIGINT) are almost
      // always intentional kills — from our own shutdown path, OpenCode tearing
      // down, OS shutdown, or the user killing the host. Auto-restarting here
      // produces process avalanches (issue #14): N bridges all receive SIGTERM
      // simultaneously, each "auto-restarts", spawning N fresh processes that
      // reload ONNX + semantic + trigram indexes. Real Rust panics/crashes exit
      // with a non-null `code` and `signal === null`; those still restart.
      if (
        signal === "SIGTERM" ||
        signal === "SIGKILL" ||
        signal === "SIGHUP" ||
        signal === "SIGINT"
      ) {
        this.process = null;
        this.configured = false;
        this.clearRestartResetTimer();
        this.rejectAllPending(new Error(`[aft-plugin] Binary killed by ${signal}`));
        return;
      }
      this.handleCrash();
    });

    this.process = child;
    this.stdoutBuffer = "";
    // Fresh spawn — clear the stderr ring so crash diagnostics only reflect
    // the current child's output, not output from prior restart cycles.
    this.stderrTail = [];
  }

  private pushStderrLine(line: string): void {
    this.stderrTail.push(line);
    if (this.stderrTail.length > BinaryBridge.STDERR_TAIL_MAX) {
      this.stderrTail.shift();
    }
  }

  /**
   * Format the current stderr tail for inclusion in error messages. Returns
   * empty string when nothing has been captured (e.g., silent SIGKILL from
   * macOS amfid) so the caller can safely concatenate unconditionally.
   */
  private formatStderrTail(): string {
    if (this.stderrTail.length === 0) return "";
    const tail = this.stderrTail.join("\n  ");
    return `\n  --- last ${this.stderrTail.length} stderr lines ---\n  ${tail}`;
  }

  private onStdoutData(data: string): void {
    this.stdoutBuffer += data;

    // Process complete lines
    let newlineIdx: number;
    while ((newlineIdx = this.stdoutBuffer.indexOf("\n")) !== -1) {
      const line = this.stdoutBuffer.slice(0, newlineIdx).trim();
      this.stdoutBuffer = this.stdoutBuffer.slice(newlineIdx + 1);

      if (!line) continue;

      try {
        const response = JSON.parse(line) as Record<string, unknown>;
        const id = response.id as string | undefined;
        if (id && this.pending.has(id)) {
          const entry = this.pending.get(id);
          if (!entry) continue;
          this.pending.delete(id);
          clearTimeout(entry.timer);
          this.scheduleRestartCountReset();
          entry.resolve(response);
        }
      } catch (_err) {
        warn(`Failed to parse stdout line: ${line}`);
      }
    }
  }

  private handleTimeout(): void {
    // Kill the hung process and reject remaining pending requests.
    // Unlike handleCrash, this does NOT auto-restart — the next send() call
    // will lazy-spawn a fresh process via ensureSpawned().
    if (this.process) {
      this.process.kill("SIGKILL");
      this.process = null;
    }
    this.clearRestartResetTimer();
    this.configured = false;

    // Capture the stderr tail for diagnostics, then clear so the next spawn
    // doesn't inherit this one's output.
    const tail = this.formatStderrTail();
    this.stderrTail = [];

    // Reject any other pending requests (the timed-out one was already rejected)
    this.rejectAllPending(new Error(`[aft-plugin] Bridge restarted after timeout${tail}`));
  }

  private handleCrash(): void {
    this.process = null;
    this.clearRestartResetTimer();
    this.configured = false; // Force reconfigure on next command after restart

    // Capture the tail BEFORE spawning the replacement, because the next spawn
    // clears the ring. Include it in both the pending-request rejection and
    // the "max restarts reached" log so the underlying failure is visible
    // without having to grep the plugin log for a timestamp match.
    const tail = this.formatStderrTail();

    // Reject all pending requests with the tail attached.
    this.rejectAllPending(
      new Error(`[aft-plugin] Binary crashed (restarts: ${this._restartCount})${tail}`),
    );

    // Auto-restart with exponential backoff
    if (this._restartCount < this.maxRestarts) {
      const delay = 100 * 2 ** this._restartCount; // 100ms, 200ms, 400ms
      this._restartCount++;
      log(`Auto-restart #${this._restartCount} in ${delay}ms`);

      setTimeout(() => {
        if (!this._shuttingDown && !this.isAlive()) {
          try {
            this.spawnProcess();
          } catch (err) {
            error(`Failed to restart: ${(err as Error).message}`);
          }
        }
      }, delay);
    } else {
      error(
        `Max restarts (${this.maxRestarts}) reached, giving up. Logs: ${getLogFilePath()}${tail}`,
      );
    }
  }

  private rejectAllPending(error: Error): void {
    for (const [_id, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
  }

  private scheduleRestartCountReset(): void {
    this.clearRestartResetTimer();
    this.restartResetTimer = setTimeout(() => {
      this._restartCount = 0;
      this.restartResetTimer = null;
    }, BinaryBridge.RESTART_RESET_MS);
  }

  private clearRestartResetTimer(): void {
    if (this.restartResetTimer) {
      clearTimeout(this.restartResetTimer);
      this.restartResetTimer = null;
    }
  }
}
