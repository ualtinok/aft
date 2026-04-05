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
 * Manages a pool of BinaryBridge instances, one per session.
 * Each session gets its own binary process with isolated backup/undo history.
 * Handles idle cleanup and LRU eviction when at capacity.
 */
export class BridgePool {
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
    };
    this.configOverrides = configOverrides;
    // Skip cleanup timer when idle timeout is Infinity (no-op) to avoid wasted cycles
    if (Number.isFinite(this.idleTimeoutMs)) {
      this.cleanupTimer = setInterval(() => this.cleanup(), CLEANUP_INTERVAL_MS);
      this.cleanupTimer.unref(); // don't prevent Node from exiting
    }
  }

  /**
   * Get or create a bridge for the given session.
   * Each session gets its own binary process with isolated backup/undo history.
   * The bridge's working directory is set to `directory`.
   */
  getBridge(directory: string, sessionID: string): BinaryBridge {
    const key = sessionID || directory.replace(/\/+$/, "");
    const existing = this.bridges.get(key);
    if (existing) {
      existing.lastUsed = Date.now();
      return existing.bridge;
    }

    // Evict LRU if at capacity
    if (this.bridges.size >= this.maxPoolSize) {
      this.evictLRU();
    }

    const normalized = directory.replace(/\/+$/, "");
    const bridge = new BinaryBridge(
      this.binaryPath,
      normalized,
      this.bridgeOptions,
      this.configOverrides,
    );
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
    // Do NOT kill running bridges — they may be mid-request. The old process
    // continues running from the old binary (safe on all platforms since the
    // binary is already loaded in memory). New bridges created by getBridge()
    // will use the updated binaryPath. Old bridges will be recycled naturally
    // when their session ends or when a version-check detects the mismatch.
    log(`Binary path updated to ${newPath}. New sessions will use the updated binary.`);
  }

  /** Number of active bridges in the pool. */
  get size(): number {
    return this.bridges.size;
  }
}
