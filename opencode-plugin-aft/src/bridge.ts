import { spawn, type ChildProcess } from "node:child_process";

/** Prefix for all bridge diagnostic messages on stderr. */
const TAG = "[aft-plugin]";

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

  constructor(binaryPath: string, cwd: string, options?: BridgeOptions) {
    this.binaryPath = binaryPath;
    this.cwd = cwd;
    this.timeoutMs = options?.timeoutMs ?? 30_000;
    this.maxRestarts = options?.maxRestarts ?? 3;
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
  async send(command: string, params: Record<string, unknown> = {}): Promise<Record<string, unknown>> {
    if (this._shuttingDown) {
      throw new Error(`${TAG} Bridge is shutting down, cannot send "${command}"`);
    }

    this.ensureSpawned();

    const id = String(this.nextId++);
    const request = { id, command, ...params };
    const line = JSON.stringify(request) + "\n";

    return new Promise<Record<string, unknown>>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${TAG} Request "${command}" (id=${id}) timed out after ${this.timeoutMs}ms`));
      }, this.timeoutMs);

      this.pending.set(id, { resolve, reject, timer });

      if (!this.process?.stdin?.writable) {
        this.pending.delete(id);
        clearTimeout(timer);
        reject(new Error(`${TAG} stdin not writable for command "${command}"`));
        return;
      }

      this.process.stdin.write(line, (err) => {
        if (err) {
          const entry = this.pending.get(id);
          if (entry) {
            this.pending.delete(id);
            clearTimeout(entry.timer);
            entry.reject(new Error(`${TAG} Failed to write to stdin: ${err.message}`));
          }
        }
      });
    });
  }

  /** Kill the child process and reject all pending requests. */
  async shutdown(): Promise<void> {
    this._shuttingDown = true;
    this.rejectAllPending(new Error(`${TAG} Bridge shutting down`));

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
          console.error(`${TAG} Process exited during shutdown`);
          resolve();
        });

        proc.kill("SIGTERM");
      });
    }
  }

  // ---- Internal ----

  private ensureSpawned(): void {
    if (this.isAlive()) return;
    this.spawnProcess();
  }

  private spawnProcess(): void {
    console.error(`${TAG} Spawning binary: ${this.binaryPath} (cwd: ${this.cwd})`);

    const child = spawn(this.binaryPath, [], {
      cwd: this.cwd,
      stdio: ["pipe", "pipe", "pipe"],
    });

    child.stdout!.on("data", (chunk: Buffer) => {
      this.onStdoutData(chunk.toString("utf-8"));
    });

    child.stderr!.on("data", (chunk: Buffer) => {
      const lines = chunk.toString("utf-8").trimEnd().split("\n");
      for (const line of lines) {
        console.error(`${TAG} stderr: ${line}`);
      }
    });

    child.on("error", (err) => {
      console.error(`${TAG} Process error: ${err.message}`);
      this.handleCrash();
    });

    child.on("exit", (code, signal) => {
      if (this._shuttingDown) return;
      console.error(`${TAG} Process exited: code=${code}, signal=${signal}`);
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
          const entry = this.pending.get(id)!;
          this.pending.delete(id);
          clearTimeout(entry.timer);
          entry.resolve(response);
        }
      } catch (err) {
        console.error(`${TAG} Failed to parse stdout line: ${line}`);
      }
    }
  }

  private handleCrash(): void {
    this.process = null;

    // Reject all pending requests
    this.rejectAllPending(
      new Error(`${TAG} Binary crashed (restarts: ${this._restartCount})`),
    );

    // Auto-restart with exponential backoff
    if (this._restartCount < this.maxRestarts) {
      const delay = 100 * Math.pow(2, this._restartCount); // 100ms, 200ms, 400ms
      this._restartCount++;
      console.error(`${TAG} Auto-restart #${this._restartCount} in ${delay}ms`);

      setTimeout(() => {
        if (!this._shuttingDown) {
          try {
            this.spawnProcess();
          } catch (err) {
            console.error(`${TAG} Failed to restart: ${(err as Error).message}`);
          }
        }
      }, delay);
    } else {
      console.error(`${TAG} Max restarts (${this.maxRestarts}) reached, giving up`);
    }
  }

  private rejectAllPending(error: Error): void {
    for (const [id, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
  }
}
