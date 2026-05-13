/// <reference path="../bun-test.d.ts" />

/**
 * findBinary expectedVersion test — uses real fs (no module mocking).
 *
 * History: an earlier version of this test used `mock.module("node:fs", …)`
 * with no-op stubs to force every sync resolution path to miss. Bun runs all
 * test files in the same process, so the partial node:fs mock leaked into
 * concurrent test files (notably `onnx-cleanup.test.ts`) and caused ENOENT in
 * any test that called `writeFileSync` after this file's mocks were installed
 * but before they were restored. `mock.restore()` does NOT undo
 * `mock.module(…)` in Bun, so the partial mock could not be cleaned up.
 *
 * Today the test uses a real empty temp directory as the AFT cache, real
 * `node_modules`-free environment, and only mocks `../downloader.js` (which
 * is the actual unit-under-test boundary). All other sync resolution paths
 * miss naturally because the temp cache is empty and the npm platform package
 * isn't installed.
 */
import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

describe("findBinary async download", () => {
  let cacheDir: string;
  let prevCacheDir: string | undefined;
  let prevPath: string | undefined;
  let prevHome: string | undefined;

  beforeEach(() => {
    cacheDir = mkdtempSync(join(tmpdir(), "aft-resolver-test-"));
    prevCacheDir = process.env.AFT_CACHE_DIR;
    prevPath = process.env.PATH;
    prevHome = process.env.HOME;
    // Empty the cache + PATH + HOME so every sync resolution path misses
    // naturally, forcing the async download fallback to run.
    process.env.AFT_CACHE_DIR = cacheDir;
    process.env.PATH = ""; // no `which aft`
    process.env.HOME = cacheDir; // no `~/.cargo/bin/aft`
  });

  afterEach(() => {
    if (prevCacheDir === undefined) delete process.env.AFT_CACHE_DIR;
    else process.env.AFT_CACHE_DIR = prevCacheDir;
    if (prevPath === undefined) delete process.env.PATH;
    else process.env.PATH = prevPath;
    if (prevHome === undefined) delete process.env.HOME;
    else process.env.HOME = prevHome;
    rmSync(cacheDir, { recursive: true, force: true });
    mock.restore();
  });

  test("honors expectedVersion when falling through to ensureBinary", async () => {
    const seenVersions: Array<string | undefined> = [];
    mock.module("../downloader.js", () => ({
      ensureBinary: async (version?: string) => {
        seenVersions.push(version);
        return "/downloaded/aft";
      },
      getCacheDir: () => cacheDir,
      getCachedBinaryPath: () => null,
    }));

    // Cache-bust the resolver import so it picks up the freshly-mocked
    // downloader instead of an earlier-test-cached copy.
    const { findBinary } = await import(`../resolver.js?expected-version-${Date.now()}`);

    await expect(findBinary("0.99.0-test")).resolves.toBe("/downloaded/aft");
    expect(seenVersions).toEqual(["0.99.0-test"]);
  });
});
