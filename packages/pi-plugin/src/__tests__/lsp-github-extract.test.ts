/**
 * Tests for `validateExtraction` in lsp-github-install.ts.
 *
 * Audit v0.17 #2: total uncompressed bytes capped at MAX_EXTRACT_BYTES (1 GiB).
 * Mirrors the OpenCode plugin tests.
 */

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { validateExtraction } from "../lsp-github-install.js";

const tempRoots = new Set<string>();

function createStagingFixture(): string {
  const root = mkdtempSync(join(tmpdir(), "aft-pi-extract-tests-"));
  tempRoots.add(root);
  return root;
}

afterEach(() => {
  for (const root of tempRoots) rmSync(root, { recursive: true, force: true });
  tempRoots.clear();
});

describe("validateExtraction (Pi)", () => {
  test("accepts a normal extraction", () => {
    const staging = createStagingFixture();
    mkdirSync(join(staging, "bin"), { recursive: true });
    writeFileSync(join(staging, "bin", "lsp-binary"), "binary");
    expect(() => validateExtraction(staging)).not.toThrow();
  });

  test("rejects symlinks (zip-slip defense)", () => {
    const staging = createStagingFixture();
    writeFileSync(join(staging, "real.txt"), "real");
    symlinkSync("real.txt", join(staging, "link.txt"));
    expect(() => validateExtraction(staging)).toThrow(/symlink.*zip-slip defense/);
  });

  test("rejects sparse file > 1 GiB cap (decompression bomb)", async () => {
    const staging = createStagingFixture();
    const fs = await import("node:fs");
    const fh = fs.openSync(join(staging, "sparse.bin"), "w");
    try {
      fs.ftruncateSync(fh, 1024 * 1024 * 1024 + 1);
    } finally {
      fs.closeSync(fh);
    }
    expect(() => validateExtraction(staging)).toThrow(/decompression bomb defense/);
  });

  test("rejects accumulated bytes across files exceeding cap", async () => {
    const staging = createStagingFixture();
    const fs = await import("node:fs");
    for (const name of ["a.bin", "b.bin"]) {
      const fh = fs.openSync(join(staging, name), "w");
      try {
        fs.ftruncateSync(fh, 600 * 1024 * 1024);
      } finally {
        fs.closeSync(fh);
      }
    }
    expect(() => validateExtraction(staging)).toThrow(/decompression bomb defense/);
  });
});
