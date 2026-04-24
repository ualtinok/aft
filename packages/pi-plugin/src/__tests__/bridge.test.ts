/**
 * Bridge-level tests for Pi.
 *
 * Mirrors packages/opencode-plugin/src/__tests__/bridge.test.ts. Both plugins
 * share the same bridge design (per-op timeout, SIGKILL recovery, etc) so we
 * keep coverage mirrored to catch regressions in either package.
 */

import { afterEach, describe, expect, test } from "bun:test";
import type { ChildProcess, ChildProcessWithoutNullStreams } from "node:child_process";
import { rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { BinaryBridge, compareSemver } from "../bridge.js";

const PROJECT_CWD = resolve(import.meta.dir, "../../../..");

describe("Pi BinaryBridge", () => {
  let bridge: BinaryBridge | null = null;

  afterEach(async () => {
    if (bridge) {
      await bridge.shutdown();
      bridge = null;
    }
  });

  test("per-request timeoutMs override rejects before bridge-wide default", async () => {
    // Fake binary: reads stdin and sleeps forever without responding. We want
    // to prove the per-request override (50ms) fires instead of the bridge
    // default (5000ms). If the override isn't honored, the bridge-wide timer
    // triggers and the test would take 5+ seconds to reject.
    const fakeBin = join(tmpdir(), `aft-pi-fake-slow-${Date.now()}.sh`);
    await writeFile(fakeBin, ["#!/bin/sh", "sleep 30", ""].join("\n"), { mode: 0o755 });

    try {
      bridge = new BinaryBridge(fakeBin, PROJECT_CWD, {
        timeoutMs: 5_000, // bridge-wide default
        maxRestarts: 0,
      });

      const start = Date.now();
      // Use "version" to skip the auto-configure path (configure/version are
      // the two commands that bypass it). Pass a tight 50ms override — should
      // reject in ~50ms, not 5000ms. If the override weren't honored, the
      // bridge-wide 5s timer would trigger instead and `elapsed` would be ~5s.
      const err = await bridge.send("version", {}, { timeoutMs: 50 }).catch((e) => e);
      const elapsed = Date.now() - start;

      expect(err).toBeInstanceOf(Error);
      expect((err as Error).message).toContain("timed out after 50ms");
      // Allow generous slack so CI flakes don't fail this — but must be well
      // under the 5s bridge default to prove the override took effect.
      expect(elapsed).toBeLessThan(2_000);
    } finally {
      await rm(fakeBin).catch(() => {});
    }
  });

  test("restart counter decays even after max restarts is reached", async () => {
    bridge = new BinaryBridge("/tmp/aft-does-not-need-to-exist", PROJECT_CWD, {
      timeoutMs: 5_000,
      maxRestarts: 1,
    });
    const originalResetMs = (BinaryBridge as any).RESTART_RESET_MS;

    try {
      (BinaryBridge as any).RESTART_RESET_MS = 20;
      (bridge as any)._restartCount = 1;

      (bridge as any).handleCrash();

      expect(bridge.restartCount).toBe(1);
      await new Promise((resolve) => setTimeout(resolve, 50));
      expect(bridge.restartCount).toBe(0);
    } finally {
      (BinaryBridge as any).RESTART_RESET_MS = originalResetMs;
    }
  });

  test("stale exit from replaced child is ignored", async () => {
    const fakeBin = join(tmpdir(), `aft-pi-fake-stale-exit-${Date.now()}.sh`);
    await writeFile(fakeBin, ["#!/bin/sh", "sleep 30", ""].join("\n"), { mode: 0o755 });

    let staleChild: ChildProcess | null = null;
    try {
      bridge = new BinaryBridge(fakeBin, PROJECT_CWD, {
        timeoutMs: 5_000,
        maxRestarts: 0,
      });

      (bridge as any).spawnProcess();
      staleChild = (bridge as any).process as ChildProcessWithoutNullStreams;
      (bridge as any).spawnProcess();
      const activeChild = (bridge as any).process as ChildProcessWithoutNullStreams;
      (bridge as any).configured = true;

      staleChild.emit("exit", 1, null);

      expect((bridge as any).process).toBe(activeChild);
      expect((bridge as any).configured).toBe(true);
    } finally {
      staleChild?.kill("SIGKILL");
      await rm(fakeBin).catch(() => {});
    }
  });
});

describe("Pi compareSemver", () => {
  test("orders semver pre-release identifiers per spec", () => {
    const ordered = [
      "1.0.0-alpha",
      "1.0.0-alpha.1",
      "1.0.0-alpha.beta",
      "1.0.0-beta",
      "1.0.0-beta.2",
      "1.0.0-beta.11",
      "1.0.0-rc.1",
      "1.0.0",
    ];

    for (let i = 0; i < ordered.length - 1; i++) {
      expect(compareSemver(ordered[i], ordered[i + 1])).toBeLessThan(0);
      expect(compareSemver(ordered[i + 1], ordered[i])).toBeGreaterThan(0);
    }
    expect(compareSemver("1.0.0-beta.1", "1.0.0")).toBeLessThan(0);
    expect(compareSemver("1.2.0", "1.1.99-rc.1")).toBeGreaterThan(0);
    expect(compareSemver("1.0.0-alpha.1", "1.0.0-alpha.1")).toBe(0);
  });
});
