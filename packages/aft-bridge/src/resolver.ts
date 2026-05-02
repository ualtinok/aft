import { execSync, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, renameSync } from "node:fs";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";
import { log, warn } from "./active-logger.js";
import { ensureBinary, getCacheDir, getCachedBinaryPath } from "./downloader.js";
import { PLATFORM_ARCH_MAP } from "./platform.js";

/**
 * Copy an npm platform binary to the versioned cache so we never run from
 * node_modules directly. This prevents corruption when npm updates the
 * package while a bridge process is running the binary.
 */
function copyToVersionedCache(npmBinaryPath: string): string | null {
  try {
    // Get the version from the binary
    const result = spawnSync(npmBinaryPath, ["--version"], {
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
      timeout: 5000,
    });
    const rawVersion = result.stdout?.trim();
    if (!rawVersion) return null;

    // `aft --version` outputs "aft 0.9.0" — extract just the version number
    const version = rawVersion.replace(/^aft\s+/, "");
    const tag = version.startsWith("v") ? version : `v${version}`;
    const cacheDir = getCacheDir();
    const versionedDir = join(cacheDir, tag);
    const ext = process.platform === "win32" ? ".exe" : "";
    const cachedPath = join(versionedDir, `aft${ext}`);

    // Already cached
    if (existsSync(cachedPath)) return cachedPath;

    // Copy to versioned cache
    mkdirSync(versionedDir, { recursive: true });
    const tmpPath = `${cachedPath}.tmp`;
    copyFileSync(npmBinaryPath, tmpPath);
    if (process.platform !== "win32") {
      chmodSync(tmpPath, 0o755);
    }
    renameSync(tmpPath, cachedPath);
    log(`Copied npm binary to versioned cache: ${cachedPath}`);
    return cachedPath;
  } catch (err) {
    warn(`Failed to copy binary to cache: ${err instanceof Error ? err.message : String(err)}`);
    return null;
  }
}

/**
 * Map the current `process.platform` and `process.arch` to the npm platform
 * package suffix (e.g. `"darwin-arm64"`, `"linux-x64"`).
 *
 * Exported for testability — agents and scripts can call this directly to
 * verify the platform mapping without running the full resolver.
 *
 * @throws {Error} with the exact `process.platform` and `process.arch` values
 *   when the combination is unsupported.
 */
export function platformKey(
  platform: string = process.platform,
  arch: string = process.arch,
): string {
  const archMap = PLATFORM_ARCH_MAP[platform];
  if (!archMap) {
    throw new Error(
      `Unsupported platform: ${platform} (arch: ${arch}). ` +
        `Supported platforms: ${Object.keys(PLATFORM_ARCH_MAP).join(", ")}`,
    );
  }
  const key = archMap[arch];
  if (!key) {
    throw new Error(
      `Unsupported architecture: ${arch} on platform ${platform}. ` +
        `Supported architectures for ${platform}: ${Object.keys(archMap).join(", ")}`,
    );
  }
  return key;
}

/**
 * Locate the `aft` binary synchronously by checking (in order):
 * 1. Cached binary from previous auto-download (~/.cache/aft/bin/)
 * 2. npm platform package via `require.resolve(@cortexkit/aft-<platform>/bin/aft)`
 * 3. PATH lookup via `which aft` (or `where aft` on Windows)
 * 4. ~/.cargo/bin/aft (Rust cargo install location)
 *
 * @param expectedVersion Optional version (without `v` prefix) — when set, the
 *   versioned cache for that version is checked first. Hosts that ship in
 *   lock-step with the binary should pass their own package version so a
 *   freshly downloaded binary is picked up before fallback resolution.
 * @returns Absolute path to the first binary found, or null if none found.
 */
export function findBinarySync(expectedVersion?: string): string | null {
  const ext = process.platform === "win32" ? ".exe" : "";

  // 1. Check versioned cache for the requested version (or this package's own
  // version as a fallback so direct callers without a host still benefit from
  // the cache).
  const pluginVersion =
    expectedVersion ??
    (() => {
      try {
        const req = createRequire(import.meta.url);
        return (req("../package.json") as { version: string }).version;
      } catch {
        return null;
      }
    })();
  if (pluginVersion) {
    const tag = pluginVersion.startsWith("v") ? pluginVersion : `v${pluginVersion}`;
    const versionCached = getCachedBinaryPath(tag);
    if (versionCached) return versionCached;
  }

  // 2. Check npm platform package — copy to versioned cache to avoid
  // corruption when npm updates the package while a bridge is running
  try {
    const key = platformKey();
    const packageBin = `@cortexkit/aft-${key}/bin/aft${ext}`;
    const req = createRequire(import.meta.url);
    const resolved = req.resolve(packageBin);
    if (existsSync(resolved)) {
      const copied = copyToVersionedCache(resolved);
      return copied ?? resolved;
    }
  } catch {
    // npm package not installed or resolution failed
  }

  // 3. Check PATH
  try {
    const whichCmd = process.platform === "win32" ? "where aft" : "which aft";
    const result = execSync(whichCmd, {
      encoding: "utf-8",
      stdio: ["pipe", "pipe", "pipe"],
    }).trim();
    if (result) return result;
  } catch {
    // not in PATH
  }

  // 4. Check ~/.cargo/bin/aft
  const cargoPath = join(homedir(), ".cargo", "bin", `aft${ext}`);
  if (existsSync(cargoPath)) return cargoPath;

  return null;
}

/**
 * Locate the `aft` binary, with auto-download as a last resort.
 *
 * Resolution order:
 *   1. Cached binary (~/.cache/aft/bin/)
 *   2. npm platform package (@cortexkit/aft-<platform>)
 *   3. PATH lookup (which aft)
 *   4. ~/.cargo/bin/aft
 *   5. Auto-download from GitHub releases
 *
 * Returns the absolute path to the binary.
 * Throws a descriptive error with install instructions if all sources fail.
 */
export async function findBinary(expectedVersion?: string): Promise<string> {
  // Try synchronous resolution first (fast path)
  const syncResult = findBinarySync(expectedVersion);
  if (syncResult) {
    log(`Resolved binary: ${syncResult}`);
    return syncResult;
  }

  // 5. Auto-download from GitHub releases
  log("Binary not found locally, attempting auto-download...");
  const downloaded = await ensureBinary();
  if (downloaded) return downloaded;

  // All sources exhausted
  throw new Error(
    [
      "Could not find the `aft` binary.",
      "",
      "Attempted sources:",
      "  - Cache directory (~/.cache/aft/bin/)",
      "  - npm platform package (@cortexkit/aft-<platform>)",
      "  - PATH lookup (which aft)",
      "  - ~/.cargo/bin/aft",
      "  - Auto-download from GitHub releases (failed)",
      "",
      "Install it using one of these methods:",
      "  npm install @cortexkit/aft-opencode        # installs platform-specific binary via npm",
      "  cargo install agent-file-tools             # from crates.io",
      "  cargo build --release         # from source (binary at target/release/aft)",
      "",
      "Or add the aft directory to your PATH.",
    ].join("\n"),
  );
}
