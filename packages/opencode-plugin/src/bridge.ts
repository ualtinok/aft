import { type ChildProcess, spawn } from "node:child_process";
import { error, log, warn } from "./logger.js";

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
  private binaryPath: string;
  private cwd: string;
  private process: ChildProcess | null = null;
  private pending = new Map<string, PendingRequest>();
  private nextId = 1;
  private stdoutBuffer = "";
  private _restartCount = 0;
  private _shuttingDown = false;
  private timeoutMs: number;
  private maxRestarts: number;
  private configured = false;
  private _configureDepth = 0;
  private configOverrides: Record<string, unknown>;
  private minVersion: string | undefined;
  private onVersionMismatch: ((binaryVersion: string, minVersion: string) => void) | undefined;

  constructor(
    binaryPath: string,
    cwd: string,
    options?: BridgeOptions,
    configOverrides?: Record<string, unknown>,
  ) {
    this.binaryPath = binaryPath;
    this.cwd = cwd;
    this.timeoutMs = options?.timeoutMs ?? 30_000;
    this.maxRestarts = options?.maxRestarts ?? 3;
    this.configOverrides = configOverrides ?? {};
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
    // Recursion depth is guarded to prevent infinite loops on repeated version mismatches (#9).
    if (!this.configured) {
      if (command !== "configure" && command !== "version") {
        this._configureDepth = (this._configureDepth ?? 0) + 1;
        if (this._configureDepth > 3) {
          this._configureDepth = 0;
          throw new Error("[aft-plugin] Failed to configure bridge after 3 attempts");
        }
        try {
          await this.send("configure", {
            project_root: this.cwd,
            ...this.configOverrides,
          });
          await this.checkVersion();
        } catch (err) {
          // Configure failed — leave configured=false so next call retries
          this._configureDepth = 0;
          throw err;
        }

        // Version check may have triggered a hot-swap (replaceBinary kills the process).
        // If the bridge died, re-spawn and re-configure before proceeding.
        if (!this.isAlive()) {
          return this.send(command, params);
        }

        this.configured = true;
        this._configureDepth = 0;
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

    const child = spawn(this.binaryPath, [], {
      cwd: this.cwd,
      stdio: ["pipe", "pipe", "pipe"],
    });

    child.stdout?.on("data", (chunk: Buffer) => {
      this.onStdoutData(chunk.toString("utf-8"));
    });

    child.stderr?.on("data", (chunk: Buffer) => {
      const lines = chunk.toString("utf-8").trimEnd().split("\n");
      for (const line of lines) {
        log(`stderr: ${line}`);
      }
    });

    child.on("error", (err) => {
      error(`Process error: ${err.message}`);
      this.handleCrash();
    });

    child.on("exit", (code, signal) => {
      if (this._shuttingDown) return;
      log(`Process exited: code=${code}, signal=${signal}`);
      this.handleCrash();
    });

    this.process = child;
    this.stdoutBuffer = "";
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
    this.configured = false;

    // Reject any other pending requests (the timed-out one was already rejected)
    this.rejectAllPending(new Error("[aft-plugin] Bridge restarted after timeout"));
  }

  private handleCrash(): void {
    this.process = null;
    this.configured = false; // Force reconfigure on next command after restart

    // Reject all pending requests
    this.rejectAllPending(
      new Error(`[aft-plugin] Binary crashed (restarts: ${this._restartCount})`),
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
      error(`Max restarts (${this.maxRestarts}) reached, giving up`);
    }
  }

  private rejectAllPending(error: Error): void {
    for (const [_id, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
  }
}
