/// <reference path="../bun-test.d.ts" />

import { afterEach, describe, expect, test } from "bun:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { PLATFORM_ASSET_MAP } from "@cortexkit/aft-bridge";

const packageRoot = fileURLToPath(new URL("../../", import.meta.url));
const tempRoots = new Set<string>();
const currentPlatformKey = `${process.platform}-${process.arch}`;
const currentAssetName = PLATFORM_ASSET_MAP[currentPlatformKey];
const binaryName = process.platform === "win32" ? "aft.exe" : "aft";

function createCacheRoot() {
  const root = mkdtempSync(join(tmpdir(), "aft-downloader-tests-"));
  tempRoots.add(root);
  return root;
}

function runDownloaderScript(script: string, env: Record<string, string> = {}) {
  const result = spawnSync(process.execPath, ["-e", script], {
    cwd: packageRoot,
    env: { ...process.env, AFT_LOG_STDERR: "1", ...env },
    encoding: "utf8",
  });

  expect(result.error).toBeUndefined();
  expect(result.status).toBe(0);

  return {
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
  };
}

afterEach(() => {
  for (const root of tempRoots) {
    rmSync(root, { recursive: true, force: true });
  }
  tempRoots.clear();
});

describe("downloadBinary error paths", () => {
  test("returns null for unsupported platforms", () => {
    const result = runDownloaderScript(`
      Object.defineProperty(process, "platform", { value: "plan9" });
      Object.defineProperty(process, "arch", { value: "x64" });
      const { downloadBinary } = await import("@cortexkit/aft-bridge");
      console.log(String(await downloadBinary("v1.2.3")));
    `);

    expect(result.stdout).toBe("null");
    // No host logger registered in subprocess — falls back to [aft-bridge] prefix
    expect(result.stderr).toContain("Unsupported platform: plan9-x64");
  });

  test("returns null and logs HTTP download failures", () => {
    if (!currentAssetName) throw new Error(`Unsupported test platform: ${currentPlatformKey}`);
    const cacheRoot = createCacheRoot();
    const result = runDownloaderScript(
      `
        globalThis.fetch = async (url) => {
          if (String(url).endsWith("checksums.sha256")) {
            return new Response("", { status: 404, statusText: "Not Found" });
          }
          return new Response("bad", { status: 502, statusText: "Bad Gateway" });
        };
        const { downloadBinary } = await import("@cortexkit/aft-bridge");
        console.log(String(await downloadBinary("v9.9.9")));
      `,
      { XDG_CACHE_HOME: cacheRoot },
    );

    expect(result.stdout).toBe("null");
    expect(result.stderr).toContain(
      `Failed to download AFT binary: HTTP 502: Bad Gateway (https://github.com/cortexkit/aft/releases/download/v9.9.9/${currentAssetName})`,
    );
  });

  test("returns null when checksum verification fails", () => {
    if (!currentAssetName) throw new Error(`Unsupported test platform: ${currentPlatformKey}`);
    const cacheRoot = createCacheRoot();
    const wrongHash = "0".repeat(64);
    const result = runDownloaderScript(
      `
        globalThis.fetch = async (url) => {
          if (String(url).endsWith("checksums.sha256")) {
            return new Response(${JSON.stringify("")} + ${JSON.stringify(wrongHash)} + "  ${currentAssetName}\\n", { status: 200 });
          }
          return new Response("binary payload", { status: 200 });
        };
        const { downloadBinary } = await import("@cortexkit/aft-bridge");
        console.log(String(await downloadBinary("v1.0.0")));
      `,
      { XDG_CACHE_HOME: cacheRoot },
    );

    expect(result.stdout).toBe("null");
    expect(result.stderr).toContain(
      `Checksum mismatch for ${currentAssetName}: expected ${wrongHash}`,
    );
    expect(existsSync(join(cacheRoot, "aft", "bin", binaryName))).toBe(false);
  });

  test("returns null when the checksum file is unavailable (security requirement)", () => {
    if (!currentAssetName) throw new Error(`Unsupported test platform: ${currentPlatformKey}`);
    const cacheRoot = createCacheRoot();
    const result = runDownloaderScript(
      `
        globalThis.fetch = async (url) => {
          if (String(url).endsWith("checksums.sha256")) {
            return new Response("missing", { status: 404, statusText: "Not Found" });
          }
          return new Response("binary payload", { status: 200 });
        };
        const { downloadBinary } = await import("@cortexkit/aft-bridge");
        console.log(String(await downloadBinary("v2.0.0")));
      `,
      { XDG_CACHE_HOME: cacheRoot },
    );

    expect(result.stdout).toBe("null");
    expect(existsSync(join(cacheRoot, "aft", "bin", "v2.0.0", binaryName))).toBe(false);
    expect(result.stderr).toContain(
      "Checksum verification failed: no checksums.sha256 found for v2.0.0",
    );
    expect(result.stderr).toContain("Binary download aborted for security reasons");
  });

  test("returns null when checksum file has no entry for the asset (security requirement)", () => {
    if (!currentAssetName) throw new Error(`Unsupported test platform: ${currentPlatformKey}`);
    const cacheRoot = createCacheRoot();
    const result = runDownloaderScript(
      `
        globalThis.fetch = async (url) => {
          if (String(url).endsWith("checksums.sha256")) {
            return new Response("not-a-checksum\\n12345 missing-entry\\n", { status: 200 });
          }
          return new Response("binary payload", { status: 200 });
        };
        const { downloadBinary } = await import("@cortexkit/aft-bridge");
        console.log(String(await downloadBinary("v3.0.0")));
      `,
      { XDG_CACHE_HOME: cacheRoot },
    );

    expect(result.stdout).toBe("null");
    expect(existsSync(join(cacheRoot, "aft", "bin", "v3.0.0", binaryName))).toBe(false);
    expect(result.stderr).toContain(
      `Checksum verification failed: checksums.sha256 found but no entry for ${currentAssetName}`,
    );
    expect(result.stderr).toContain("Binary download aborted for security reasons");
  });
});
