/// <reference path="../bun-test.d.ts" />

/**
 * Resolver version-mismatch test — verifies that `findBinarySync` rejects
 * an npm platform binary whose `--version` does not match the requested
 * `expectedVersion`, and falls through to PATH lookup instead.
 *
 * Regression case (caught during v0.23 Pi RPC e2e dogfooding): a workspace
 * upgraded to plugin v0.22.x can still have a bun-hoisted older
 * `@cortexkit/aft-<platform>` symlink in node_modules (e.g. v0.19.5). The
 * resolver would happily run that older binary, producing stale behavior
 * (in the original repro: `bgb-` task slugs instead of `bash-`).
 *
 * No module mocking — uses a real fake binary directory and writes a shell
 * script that emits a controlled `--version` output. The npm-package
 * resolution leg cannot be exercised without `node_modules/@cortexkit/aft-*`
 * present, so this test focuses on the version-check helper directly.
 */
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readBinaryVersion } from "../resolver.js";

describe("readBinaryVersion", () => {
  let tmpDir: string;

  beforeEach(() => {
    tmpDir = mkdtempSync(join(tmpdir(), "aft-version-test-"));
  });

  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  test("parses 'aft 0.22.1' style output", () => {
    const fakeBin = join(tmpDir, "fake-aft.sh");
    writeFileSync(fakeBin, '#!/bin/sh\necho "aft 0.22.1"\n');
    chmodSync(fakeBin, 0o755);
    expect(readBinaryVersion(fakeBin)).toBe("0.22.1");
  });

  test("parses 'aft 0.19.5' (older pre-rename version)", () => {
    const fakeBin = join(tmpDir, "fake-aft.sh");
    writeFileSync(fakeBin, '#!/bin/sh\necho "aft 0.19.5"\n');
    chmodSync(fakeBin, 0o755);
    expect(readBinaryVersion(fakeBin)).toBe("0.19.5");
  });

  test("returns null for empty output", () => {
    const fakeBin = join(tmpDir, "fake-aft.sh");
    writeFileSync(fakeBin, "#!/bin/sh\nexit 0\n");
    chmodSync(fakeBin, 0o755);
    expect(readBinaryVersion(fakeBin)).toBeNull();
  });

  test("parses stderr-only version output when stdout is empty", () => {
    const fakeBin = join(tmpDir, "fake-aft.sh");
    writeFileSync(fakeBin, '#!/bin/sh\necho "aft 0.74.0" >&2\n');
    chmodSync(fakeBin, 0o755);
    expect(readBinaryVersion(fakeBin)).toBe("0.74.0");
  });

  test("returns null for binaries that fail", () => {
    const fakeBin = join(tmpDir, "fake-aft.sh");
    writeFileSync(fakeBin, "#!/bin/sh\nexit 1\n");
    chmodSync(fakeBin, 0o755);
    // Non-zero exit with no stdout is null
    expect(readBinaryVersion(fakeBin)).toBeNull();
  });

  test("returns null when path does not exist", () => {
    expect(readBinaryVersion(join(tmpDir, "does-not-exist"))).toBeNull();
  });

  test("strips 'v' prefix not applied — readBinaryVersion returns bare version", () => {
    // The cache layout uses `v<version>` paths but readBinaryVersion returns
    // the bare version without the `v` prefix. Callers (e.g.
    // findBinarySync's version-mismatch check) compare bare versions, so this
    // is the load-bearing contract: pluginVersion="0.22.1" must equal
    // readBinaryVersion(npm-binary) when no leading "v" is involved.
    const fakeBin = join(tmpDir, "fake-aft.sh");
    writeFileSync(fakeBin, '#!/bin/sh\necho "aft 0.22.1"\n');
    chmodSync(fakeBin, 0o755);
    expect(readBinaryVersion(fakeBin)).toBe("0.22.1"); // not "v0.22.1"
  });
});
