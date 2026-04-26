import { BinaryBridge, type BridgeOptions } from "./bridge";
import { error, log } from "./logger.js";

const DEFAULT_IDLE_TIMEOUT_MS = Infinity; // keep alive as long as opencode is running
const DEFAULT_MAX_POOL_SIZE = 8;
const CLEANUP_INTERVAL_MS = 60 * 1000; // check every minute

interface PoolEntry {
  bridge: BinaryBridge;
  lastUsed: number;
}

export interface PoolOptions extends BridgeOptions {
  maxPoolSize?: number;
  idleTimeoutMs?: number;
}

/**
 * Manages a pool of BinaryBridge instances, keyed by **canonical project root**.
 *
 * Prior to issue #14, the pool spawned one binary process per OpenCode session,
 * which duplicated every heavy in-memory structure (ONNX runtime, trigram and
 * semantic indexes, LSP state, symbol caches) N times for N sessions in the
 * same project. That produced an effective "leak" the user saw as many aft
 * processes consuming gigabytes of RAM on large repositories.
 *
 * The current design spawns **one bridge per project** and relies on the Rust
 * side to partition the small amount of truly session-scoped state (undo
 * history, named checkpoints) via the `session_id` envelope field attached by
 * the `callBridge()` helper. Sessions sharing a bridge still share the
 * latency of a single request pipeline; the trade-off is acceptable because
 * it removes the real RAM multiplier.
 */
export class BridgePool {
  /** Project-root → bridge. Key is a normalized canonical path. */
  private readonly bridges = new Map<string, PoolEntry>();
  private binaryPath: string;
  private readonly maxPoolSize: number;
  private readonly idleTimeoutMs: number;
  private readonly bridgeOptions: BridgeOptions;
  private readonly configOverrides: Record<string, unknown>;
  private cleanupTimer: ReturnType<typeof setInterval> | null = null;

  constructor(
    binaryPath: string,
    options: PoolOptions = {},
    configOverrides: Record<string, unknown> = {},
  ) {
    this.binaryPath = binaryPath;
    this.maxPoolSize = options.maxPoolSize ?? DEFAULT_MAX_POOL_SIZE;
    this.idleTimeoutMs = options.idleTimeoutMs ?? DEFAULT_IDLE_TIMEOUT_MS;
    this.bridgeOptions = {
      timeoutMs: options.timeoutMs,
      maxRestarts: options.maxRestarts,
      minVersion: options.minVersion,
      onVersionMismatch: options.onVersionMismatch,
      onConfigureWarnings: options.onConfigureWarnings,
    };
    this.configOverrides = configOverrides;
    // Skip cleanup timer when idle timeout is Infinity (no-op) to avoid wasted cycles
    if (Number.isFinite(this.idleTimeoutMs)) {
      this.cleanupTimer = setInterval(() => this.cleanup(), CLEANUP_INTERVAL_MS);
      this.cleanupTimer.unref(); // don't prevent Node from exiting
    }
  }

  /**
   * Get any existing bridge that is configured and alive, preferring one that
   * matches the given project root.
   *
   * Used by `/aft-status` and similar read-only paths that want to reuse a
   * warm bridge (with loaded semantic indexes etc.) instead of paying the
   * cold-start cost on a cheap query.
   */
  getAnyActiveBridge(projectRoot: string): BinaryBridge | null {
    const key = normalizeKey(projectRoot);
    const preferred = this.bridges.get(key);
    if (preferred?.bridge.isAlive()) {
      preferred.lastUsed = Date.now();
      return preferred.bridge;
    }
    for (const entry of this.bridges.values()) {
      if (entry.bridge.isAlive()) {
        entry.lastUsed = Date.now();
        return entry.bridge;
      }
    }
    return null;
  }

  /**
   * Get or create the bridge for `projectRoot`.
   *
   * Callers should always pass a **canonical** project root (see
   * `projectRootFor()` in `tools/_shared.ts`). All sessions operating on the
   * same project share one bridge; their undo/checkpoint state is still
   * isolated by `session_id` on the Rust side.
   */
  getBridge(projectRoot: string): BinaryBridge {
    const key = normalizeKey(projectRoot);
    const existing = this.bridges.get(key);
    if (existing) {
      existing.lastUsed = Date.now();
      return existing.bridge;
    }

    // Evict LRU if at capacity (one project = one slot now, so reaching the
    // cap means the user has many distinct projects open).
    if (this.bridges.size >= this.maxPoolSize) {
      this.evictLRU();
    }

    const bridge = new BinaryBridge(this.binaryPath, key, this.bridgeOptions, this.configOverrides);
    this.bridges.set(key, { bridge, lastUsed: Date.now() });
    return bridge;
  }

  /** Shut down idle bridges that haven't been used within the timeout. */
  private cleanup(): void {
    const now = Date.now();
    for (const [dir, entry] of this.bridges) {
      if (now - entry.lastUsed > this.idleTimeoutMs) {
        entry.bridge.shutdown().catch((err) => error("cleanup shutdown failed:", err));
        this.bridges.delete(dir);
      }
    }
  }

  /** Evict the least recently used bridge to make room. */
  private evictLRU(): void {
    let oldestDir: string | null = null;
    let oldestTime = Infinity;
    for (const [dir, entry] of this.bridges) {
      if (entry.lastUsed < oldestTime) {
        oldestTime = entry.lastUsed;
        oldestDir = dir;
      }
    }
    if (oldestDir) {
      const entry = this.bridges.get(oldestDir);
      entry?.bridge.shutdown().catch((err) => error("eviction shutdown failed:", err));
      this.bridges.delete(oldestDir);
    }
  }

  /** Shut down all bridges and stop the cleanup timer. */
  async shutdown(): Promise<void> {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
    const shutdowns = Array.from(this.bridges.values()).map((e) => e.bridge.shutdown());
    this.bridges.clear();
    await Promise.allSettled(shutdowns);
  }

  /**
   * Replace the binary path and restart all bridges.
   * Used after downloading a newer binary version.
   */
  async replaceBinary(newPath: string): Promise<void> {
    this.binaryPath = newPath;
    // Clear the pool so next getBridge() creates fresh bridges with the new binary.
    // Old bridge processes are NOT killed — they continue running from the old
    // binary (safe on all platforms since the binary is loaded in memory) and will
    // exit naturally when their stdin/stdout are garbage collected.
    const shutdowns = Array.from(this.bridges.values()).map((entry) => entry.bridge.shutdown());
    this.bridges.clear();
    await Promise.allSettled(shutdowns);
    log(
      `Binary path updated to ${newPath}. All bridges cleared — next calls will use the new binary.`,
    );
  }

  /** Number of active bridges in the pool. */
  get size(): number {
    return this.bridges.size;
  }
}

/** Strip trailing path separators so `/repo` and `/repo/` collapse to one key. */
function normalizeKey(projectRoot: string): string {
  return projectRoot.replace(/[/\\]+$/, "");
}
