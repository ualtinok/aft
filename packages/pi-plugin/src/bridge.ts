import { type ChildProcess, spawn } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import { error, getLogFilePath, log, warn } from "./logger.js";

const DEFAULT_BRIDGE_TIMEOUT_MS = 30_000;
const SEMANTIC_TIMEOUT_SAFETY_MARGIN_MS = 5_000;
const MAX_STDOUT_BUFFER = 64 * 1024 * 1024; // 64MB

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
 * Compare two semver version strings (major.minor.patch plus pre-release).
 * Returns: negative if a < b, 0 if equal, positive if a > b.
 */
export function compareSemver(a: string, b: string): number {
  const [aMain, aPre] = a.split("-", 2);
  const [bMain, bPre] = b.split("-", 2);
  const aParts = aMain.split(".").map(Number);
  const bParts = bMain.split(".").map(Number);
  for (let i = 0; i < 3; i++) {
    if (aParts[i] !== bParts[i]) return (aParts[i] ?? 0) - (bParts[i] ?? 0);
  }
  if (!aPre && !bPre) return 0;
  if (!aPre) return 1;
  if (!bPre) return -1;

  const aIds = aPre.split(".");
  const bIds = bPre.split(".");
  for (let i = 0; i < Math.max(aIds.length, bIds.length); i++) {
    const ai = aIds[i];
    const bi = bIds[i];
    if (ai === undefined) return -1;
    if (bi === undefined) return 1;
    const aNum = /^\d+$/.test(ai);
    const bNum = /^\d+$/.test(bi);
    if (aNum && bNum) {
      const diff = Number.parseInt(ai, 10) - Number.parseInt(bi, 10);
      if (diff !== 0) return diff;
    } else if (aNum) {
      return -1;
    } else if (bNum) {
      return 1;
    } else {
      const cmp = ai.localeCompare(bi);
      if (cmp !== 0) return cmp;
    }
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

interface ConfigureWarningsContext {
  projectRoot: string;
  sessionId?: string;
  client?: unknown;
  warnings: unknown[];
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
  /** Called after the first successful configure returns user-visible warnings. */
  onConfigureWarnings?: (context: ConfigureWarningsContext) => void | Promise<void>;
}

interface SendOptions {
  timeoutMs?: number;
  configureWarningClient?: unknown;
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
  private onConfigureWarnings:
    | ((context: ConfigureWarningsContext) => void | Promise<void>)
    | undefined;
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
    this.onConfigureWarnings = options?.onConfigureWarnings;
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
    options?: SendOptions,
  ): Promise<Record<string, unknown>> {
    if (this._shuttingDown) {
      throw new Error(`[aft-pi] Bridge is shutting down, cannot send "${command}"`);
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
                  `[aft-pi] Configure failed: ${configResult.message ?? "unknown error"}`,
                );
              }
              // Large-repo warning is emitted by the Rust side via log::warn!
              // and relayed through stderr → plugin log. No need to re-log here
              // (doing so would just duplicate the same line in aft-pi.log).
              await this.deliverConfigureWarnings(configResult, params, options);
              await this.checkVersion();
              // Re-check liveness after version check — checkVersion() swallows
              // errors as best-effort, so the bridge may have died without throwing.
              if (!this.isAlive()) {
                throw new Error(
                  `[aft-pi] Bridge died during version check. Check logs: ${getLogFilePath()}`,
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

    // Per-op timeout override: tool wrappers can pass longer budgets for
    // commands that legitimately need them (callers, trace_to, grep on big
    // repos). Defaults to the bridge-wide timeout otherwise.
    const effectiveTimeoutMs = options?.timeoutMs ?? this.timeoutMs;

    return new Promise<Record<string, unknown>>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        warn(
          `Request "${command}" (id=${id}) timed out after ${effectiveTimeoutMs}ms — restarting bridge`,
        );
        reject(
          new Error(
            `[aft-pi] Request "${command}" (id=${id}) timed out after ${effectiveTimeoutMs}ms`,
          ),
        );
        // Kill the hung process so the next request gets a fresh bridge
        this.handleTimeout();
      }, effectiveTimeoutMs);

      this.pending.set(id, { resolve, reject, timer });

      if (!this.process?.stdin?.writable) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(new Error(`[aft-pi] stdin not writable for command "${command}"`));
        return;
      }

      this.process.stdin.write(line, (err) => {
        if (err) {
          const entry = this.pending.get(id);
          if (entry) {
            this.pending.delete(id);
            clearTimeout(entry.timer);
            entry.reject(new Error(`[aft-pi] Failed to write to stdin: ${err.message}`));
          }
        }
      });
    });
  }

  private async deliverConfigureWarnings(
    configResult: Record<string, unknown>,
    params: Record<string, unknown>,
    options: SendOptions | undefined,
  ): Promise<void> {
    if (!this.onConfigureWarnings || !Array.isArray(configResult.warnings)) return;
    if (configResult.warnings.length === 0) return;

    try {
      const sessionId = typeof params.session_id === "string" ? params.session_id : undefined;
      await this.onConfigureWarnings({
        projectRoot: this.cwd,
        sessionId,
        client: options?.configureWarningClient,
        warnings: configResult.warnings,
      });
    } catch (err) {
      warn(
        `configure warning delivery failed: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
  }

  /** Kill the child process and reject all pending requests. */
  async shutdown(): Promise<void> {
    this._shuttingDown = true;
    this.clearRestartResetTimer();
    this.rejectAllPending(new Error("[aft-pi] Bridge shutting down"));

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
    const currentChild = child;

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
      if (this.process !== currentChild) return;
      error(`Process error: ${err.message}${this.formatStderrTail()}`);
      this.handleCrash();
    });

    child.on("exit", (code, signal) => {
      if (this.process !== currentChild) return;
      if (this._shuttingDown) return;
      log(`Process exited: code=${code}, signal=${signal}`);
      // External termination signals are intentional kills — don't auto-restart.
      // See packages/opencode-plugin/src/bridge.ts for the full rationale (issue #14).
      if (
        signal === "SIGTERM" ||
        signal === "SIGKILL" ||
        signal === "SIGHUP" ||
        signal === "SIGINT"
      ) {
        this.process = null;
        this.configured = false;
        this.clearRestartResetTimer();
        this.rejectAllPending(new Error(`[aft-pi] Binary killed by ${signal}`));
        return;
      }
      this.handleCrash();
    });

    this.process = child;
    this.stdoutBuffer = "";
    // Fresh spawn — clear stderr ring so crash diagnostics only reflect
    // the current child's output, not prior restart cycles.
    this.stderrTail = [];
  }

  private pushStderrLine(line: string): void {
    this.stderrTail.push(line);
    if (this.stderrTail.length > BinaryBridge.STDERR_TAIL_MAX) {
      this.stderrTail.shift();
    }
  }

  private formatStderrTail(): string {
    if (this.stderrTail.length === 0) return "";
    const tail = this.stderrTail.join("\n  ");
    return `\n  --- last ${this.stderrTail.length} stderr lines ---\n  ${tail}`;
  }

  private onStdoutData(data: string): void {
    this.stdoutBuffer += data;
    if (this.stdoutBuffer.length > MAX_STDOUT_BUFFER) {
      this.handleCrash(
        new Error(`aft bridge stdout buffer exceeded ${MAX_STDOUT_BUFFER} bytes — killing bridge`),
      );
      return;
    }

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

    const tail = this.formatStderrTail();
    this.stderrTail = [];

    // Reject any other pending requests (the timed-out one was already rejected)
    this.rejectAllPending(new Error(`[aft-pi] Bridge restarted after timeout${tail}`));
  }

  private handleCrash(cause?: Error): void {
    const proc = this.process;
    this.process = null;
    if (proc && proc.exitCode === null && !proc.killed) {
      proc.kill("SIGKILL");
    }
    this.clearRestartResetTimer();
    this.configured = false; // Force reconfigure on next command after restart

    // Capture the tail BEFORE spawning the replacement, because the next spawn
    // clears the ring. Include it in both the pending-request rejection and
    // the "max restarts reached" log so the underlying failure is visible.
    const tail = this.formatStderrTail();

    this.rejectAllPending(
      new Error(
        `[aft-pi] Binary crashed (restarts: ${this._restartCount})${cause ? `: ${cause.message}` : ""}${tail}`,
      ),
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
      // Also decay the counter over time so repeated crashes without any
      // successful response don't permanently wedge the bridge.
      this.scheduleRestartCountReset();
    } else {
      error(
        `Max restarts (${this.maxRestarts}) reached, giving up. Logs: ${getLogFilePath()}${tail}`,
      );
      this.scheduleRestartCountReset();
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
