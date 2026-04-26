import { realpathSync } from "node:fs";
import { BinaryBridge, type BridgeOptions } from "./bridge.js";
import { error, log } from "./logger.js";

const DEFAULT_IDLE_TIMEOUT_MS = Infinity; // keep alive as long as pi is running
const DEFAULT_MAX_POOL_SIZE = 4;
const CLEANUP_INTERVAL_MS = 60 * 1000;

interface PoolEntry {
  bridge: BinaryBridge;
  lastUsed: number;
}

export interface PoolOptions extends BridgeOptions {
  maxPoolSize?: number;
  idleTimeoutMs?: number;
}

/**
 * Manages a pool of BinaryBridge instances keyed by working directory.
 *
 * Pi has one extension instance per session, and sessions are bound to a single
 * cwd. We pool by directory so `/new`, `/fork`, and `/resume` in the same cwd
 * can reuse an existing warm bridge with its caches, LSP state, and backup
 * history intact.
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
      onConfigureWarnings: options.onConfigureWarnings,
    };
    this.configOverrides = configOverrides;
    if (Number.isFinite(this.idleTimeoutMs)) {
      this.cleanupTimer = setInterval(() => this.cleanup(), CLEANUP_INTERVAL_MS);
      this.cleanupTimer.unref();
    }
  }

  /** Get any existing alive bridge, preferring the given directory. */
  getAnyActiveBridge(directory: string): BinaryBridge | null {
    const key = canonicalKey(directory);
    const match = this.bridges.get(key);
    if (match?.bridge.isAlive()) {
      match.lastUsed = Date.now();
      return match.bridge;
    }
    for (const [, entry] of this.bridges) {
      if (entry.bridge.isAlive()) {
        entry.lastUsed = Date.now();
        return entry.bridge;
      }
    }
    return null;
  }

  /** Get or create a bridge for the given directory. */
  getBridge(directory: string): BinaryBridge {
    const key = canonicalKey(directory);
    const existing = this.bridges.get(key);
    if (existing) {
      existing.lastUsed = Date.now();
      return existing.bridge;
    }

    if (this.bridges.size >= this.maxPoolSize) {
      this.evictLRU();
    }

    const bridge = new BinaryBridge(this.binaryPath, key, this.bridgeOptions, this.configOverrides);
    this.bridges.set(key, { bridge, lastUsed: Date.now() });
    return bridge;
  }

  private cleanup(): void {
    const now = Date.now();
    for (const [dir, entry] of this.bridges) {
      if (now - entry.lastUsed > this.idleTimeoutMs) {
        entry.bridge.shutdown().catch((err) => error("cleanup shutdown failed:", err));
        this.bridges.delete(dir);
      }
    }
  }

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

  async shutdown(): Promise<void> {
    if (this.cleanupTimer) {
      clearInterval(this.cleanupTimer);
      this.cleanupTimer = null;
    }
    const shutdowns = Array.from(this.bridges.values()).map((e) => e.bridge.shutdown());
    this.bridges.clear();
    await Promise.allSettled(shutdowns);
  }

  async replaceBinary(newPath: string): Promise<void> {
    this.binaryPath = newPath;
    const shutdowns = Array.from(this.bridges.values()).map((entry) => entry.bridge.shutdown());
    this.bridges.clear();
    await Promise.allSettled(shutdowns);
    log(`Binary path updated to ${newPath}. All bridges cleared.`);
  }

  get size(): number {
    return this.bridges.size;
  }
}

/** Canonicalize bridge keys so symlinked paths and trailing separators collapse to one key. */
function canonicalKey(directory: string): string {
  const stripped = directory.replace(/[/\\]+$/, "");
  try {
    return realpathSync(stripped);
  } catch {
    return stripped;
  }
}
