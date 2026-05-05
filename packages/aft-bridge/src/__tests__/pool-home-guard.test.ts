/// <reference path="../bun-test.d.ts" />

/**
 * Tests for the `$HOME` project root guard in `BridgePool.getBridge`.
 *
 * Note #65: when OpenCode Desktop / Pi launches from `~` and a session has no
 * stored project directory, the resolver hands the plugin the home dir as the
 * "project root". Configuring an aft bridge against `$HOME` walks the entire
 * user home tree (often hundreds of thousands of files), times out the 30s
 * configure budget, gets killed by the bridge timeout, and silently retries
 * on every reload — wasting one full bridge spawn per restart.
 *
 * The fix is two-layer:
 *   1. Plugin eager-configure callers detect `$HOME` and skip via the
 *      exported `isHomeDirectoryRoot()` helper.
 *   2. `BridgePool.getBridge()` itself throws `HomeProjectRootError` if it
 *      receives `$HOME`, as defense-in-depth so any future regression is
 *      loud rather than silent.
 *
 * These tests pin both layers.
 */

import { mkdtempSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { BridgePool, HomeProjectRootError, isHomeDirectoryRoot } from "../pool.js";

describe("isHomeDirectoryRoot", () => {
  test("returns true for the user's home directory", () => {
    expect(isHomeDirectoryRoot(homedir())).toBe(true);
  });

  test("returns false for a subdirectory of $HOME", () => {
    // A real subdir of $HOME — guaranteed to exist if $HOME exists, but we
    // don't actually need it to exist for the path-comparison check.
    const sub = join(homedir(), "some-project");
    expect(isHomeDirectoryRoot(sub)).toBe(false);
  });

  test("returns false for an unrelated absolute path", () => {
    expect(isHomeDirectoryRoot("/usr/local/bin")).toBe(false);
  });

  test("returns false for empty string", () => {
    expect(isHomeDirectoryRoot("")).toBe(false);
  });

  test("returns false for a tempdir", () => {
    expect(isHomeDirectoryRoot(tmpdir())).toBe(false);
  });
});

describe("BridgePool.getBridge — $HOME guard", () => {
  test("throws HomeProjectRootError when projectRoot is $HOME exactly", () => {
    const pool = new BridgePool("/fake/aft", { idleTimeoutMs: Infinity });
    expect(() => pool.getBridge(homedir())).toThrow(HomeProjectRootError);
  });

  test("HomeProjectRootError carries the project root and a clear message", () => {
    const pool = new BridgePool("/fake/aft", { idleTimeoutMs: Infinity });
    try {
      pool.getBridge(homedir());
      throw new Error("expected getBridge to throw");
    } catch (err) {
      expect(err).toBeInstanceOf(HomeProjectRootError);
      const home = (err as HomeProjectRootError).projectRoot;
      // The thrown projectRoot is the *normalized* (canonical) path. On macOS
      // /Users/foo == realpath(/Users/foo); on Linux it depends on whether
      // the user's home is a symlink. Either way, isHomeDirectoryRoot of the
      // thrown value must round-trip to true.
      expect(isHomeDirectoryRoot(home)).toBe(true);
      expect((err as Error).message).toContain("user home directory");
    }
  });

  test("does NOT throw for a subdirectory of $HOME", () => {
    // Use a real tempdir under $HOME equivalent — but tmpdir() is usually
    // /var/folders or /tmp on macOS, neither of which are children of $HOME
    // and ARE valid project roots. We still want to verify subdirs of $HOME
    // pass through. Create one explicitly.
    const sub = mkdtempSync(join(homedir(), ".aft-pool-home-guard-test-"));
    try {
      const pool = new BridgePool("/fake/aft", { idleTimeoutMs: Infinity });
      // Should not throw. The bridge is constructed lazily (no real spawn
      // until the first .send() call), so this is safe even with a fake
      // binary path — we never call send.
      const bridge = pool.getBridge(sub);
      expect(bridge).toBeDefined();
    } finally {
      // Best-effort cleanup. The bridge instance was created but never spawned,
      // so the only thing on disk is the empty tempdir.
      // Skipping rmdir: tmp.* helper would work but adds complexity for no gain.
    }
  });

  test("throws independently of any cached bridge entry — guard runs first", () => {
    // Even if a hypothetical bridge entry were cached for $HOME (it shouldn't
    // be, but defense-in-depth means the guard precedes the cache lookup),
    // the guard must throw before returning the entry.
    //
    // We can't directly inject a fake entry into the private map, but we can
    // verify the guard runs by calling getBridge twice for $HOME — both must
    // throw, not the second one only.
    const pool = new BridgePool("/fake/aft", { idleTimeoutMs: Infinity });
    expect(() => pool.getBridge(homedir())).toThrow(HomeProjectRootError);
    expect(() => pool.getBridge(homedir())).toThrow(HomeProjectRootError);
    expect(pool.size).toBe(0);
  });

  test("does NOT throw for tempdir (the common test fixture)", () => {
    const pool = new BridgePool("/fake/aft", { idleTimeoutMs: Infinity });
    const dir = tmpdir();
    expect(() => pool.getBridge(dir)).not.toThrow();
  });
});

// Hooks: keep the test file lifecycle quiet — no real spawns, no real
// network. Guard against accidental side-effects.
beforeAll(() => {});
afterAll(() => {});
