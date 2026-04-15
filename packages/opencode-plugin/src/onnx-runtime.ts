/**
 * Auto-download and manage ONNX Runtime shared library for semantic search.
 *
 * Downloads the CPU-only ONNX Runtime from Microsoft's GitHub releases.
 * The library is cached in the storage directory alongside semantic index data.
 *
 * Supported platforms:
 *   - macOS ARM64 (osx-arm64)
 *   - Linux x64 (linux-x64)
 *   - Linux ARM64 (linux-aarch64)
 *   - Windows x64 (win-x64)
 *   - Windows ARM64 (win-arm64)
 *
 * macOS x64 (Intel) is not provided by Microsoft — users must install via:
 *   brew install onnxruntime
 */

import { chmodSync, existsSync, mkdirSync, readdirSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { error, log, warn } from "./logger.js";

const ORT_VERSION = "1.24.4";
const ORT_REPO = "microsoft/onnxruntime";

/** Map (process.platform, process.arch) → ONNX Runtime asset name + library filename */
interface OrtPlatformInfo {
  assetName: string;
  libName: string;
  archiveType: "tgz" | "zip";
}

const ORT_PLATFORM_MAP: Record<string, Record<string, OrtPlatformInfo>> = {
  darwin: {
    arm64: {
      assetName: `onnxruntime-osx-arm64-${ORT_VERSION}`,
      libName: "libonnxruntime.dylib",
      archiveType: "tgz",
    },
    // x64 not available from Microsoft — users need brew install onnxruntime
  },
  linux: {
    x64: {
      assetName: `onnxruntime-linux-x64-${ORT_VERSION}`,
      libName: "libonnxruntime.so",
      archiveType: "tgz",
    },
    arm64: {
      assetName: `onnxruntime-linux-aarch64-${ORT_VERSION}`,
      libName: "libonnxruntime.so",
      archiveType: "tgz",
    },
  },
  win32: {
    x64: {
      assetName: `onnxruntime-win-x64-${ORT_VERSION}`,
      libName: "onnxruntime.dll",
      archiveType: "zip",
    },
    arm64: {
      assetName: `onnxruntime-win-arm64-${ORT_VERSION}`,
      libName: "onnxruntime.dll",
      archiveType: "zip",
    },
  },
};

/** Get platform info for the current system, or null if unsupported */
function getPlatformInfo(): OrtPlatformInfo | null {
  const platformMap = ORT_PLATFORM_MAP[process.platform];
  if (!platformMap) return null;
  return platformMap[process.arch] || null;
}

/** Check if this platform can auto-download ONNX Runtime */
export function isOrtAutoDownloadSupported(): boolean {
  return getPlatformInfo() !== null;
}

/** Get the install hint for platforms where auto-download isn't available */
export function getManualInstallHint(): string {
  if (process.platform === "darwin" && process.arch === "x64") {
    return "brew install onnxruntime";
  }
  if (process.platform === "linux") {
    return "apt install libonnxruntime or download from https://github.com/microsoft/onnxruntime/releases";
  }
  return "Download from https://github.com/microsoft/onnxruntime/releases";
}

/**
 * Ensure ONNX Runtime is available. Returns the directory containing the library,
 * or null if unavailable.
 *
 * Resolution order:
 *   1. Cached in storageDir/onnxruntime/<version>/
 *   2. Auto-download from GitHub releases (if platform supported)
 *   3. null (user needs manual install)
 */
export async function ensureOnnxRuntime(storageDir: string): Promise<string | null> {
  const info = getPlatformInfo();

  // Check cached location first
  const ortDir = join(storageDir, "onnxruntime", ORT_VERSION);
  const libPath = join(ortDir, info?.libName ?? "libonnxruntime.dylib");

  if (existsSync(libPath)) {
    log(`ONNX Runtime found at ${ortDir}`);
    return ortDir;
  }

  // Check system locations
  const systemPath = findSystemOnnxRuntime(info?.libName);
  if (systemPath) {
    log(`ONNX Runtime found at system path: ${systemPath}`);
    return systemPath;
  }

  // Auto-download if platform is supported
  if (!info) {
    warn(
      `ONNX Runtime auto-download not available for ${process.platform}/${process.arch}. Install manually: ${getManualInstallHint()}`,
    );
    return null;
  }

  return downloadOnnxRuntime(info, ortDir);
}

/** Check common system locations for ONNX Runtime */
function findSystemOnnxRuntime(libName?: string): string | null {
  if (!libName) return null;

  const searchPaths: string[] = [];

  if (process.platform === "darwin") {
    // Homebrew locations
    searchPaths.push("/opt/homebrew/lib", "/usr/local/lib");
  } else if (process.platform === "linux") {
    searchPaths.push(
      "/usr/lib",
      "/usr/lib/x86_64-linux-gnu",
      "/usr/lib/aarch64-linux-gnu",
      "/usr/local/lib",
    );
  }

  for (const dir of searchPaths) {
    if (existsSync(join(dir, libName))) {
      return dir;
    }
  }

  return null;
}

/** Download and extract ONNX Runtime from GitHub releases */
async function downloadOnnxRuntime(
  info: OrtPlatformInfo,
  targetDir: string,
): Promise<string | null> {
  const url = `https://github.com/${ORT_REPO}/releases/download/v${ORT_VERSION}/${info.assetName}.${info.archiveType === "tgz" ? "tgz" : "zip"}`;

  log(`Downloading ONNX Runtime v${ORT_VERSION} for ${process.platform}/${process.arch}...`);

  try {
    const tmpDir = `${targetDir}.tmp.${process.pid}`;
    mkdirSync(tmpDir, { recursive: true });
    const archivePath = join(tmpDir, `onnxruntime.${info.archiveType}`);

    // Download using curl (more reliable than fetch across Bun/Node runtimes,
    // especially for GitHub release redirects and large binary downloads)
    const { execSync: execSyncDl } = await import("node:child_process");
    execSyncDl(`curl -fsSL "${url}" -o "${archivePath}"`, {
      stdio: "pipe",
      timeout: 120_000,
    });

    // Extract
    if (info.archiveType === "tgz") {
      const { execSync } = await import("node:child_process");
      execSync(`tar xzf "${archivePath}" -C "${tmpDir}"`, { stdio: "pipe" });
    } else {
      await extractZipArchive(archivePath, tmpDir);
    }

    // Find and copy the library file
    const extractedDir = join(tmpDir, info.assetName, "lib");
    if (!existsSync(extractedDir)) {
      throw new Error(`Expected directory not found: ${extractedDir}`);
    }

    // Create target directory and copy library files
    mkdirSync(targetDir, { recursive: true });

    // Copy all library files (main + versioned symlinks).
    // On Linux, .so files are often symlinks (libonnxruntime.so → libonnxruntime.so.1.24.4).
    // Process real files first, then recreate symlinks in the target directory to avoid
    // ENOENT when renaming a symlink whose target was already moved.
    const libFiles = readdirSync(extractedDir).filter(
      (f) => f.startsWith("libonnxruntime") || f.startsWith("onnxruntime"),
    );

    const { lstatSync, symlinkSync, readlinkSync, copyFileSync: cpFile } = await import("node:fs");

    // Separate real files from symlinks
    const realFiles: string[] = [];
    const symlinks: Array<{ name: string; target: string }> = [];
    for (const libFile of libFiles) {
      const src = join(extractedDir, libFile);
      try {
        const stat = lstatSync(src);
        log(
          `ORT extract: ${libFile} — isSymlink=${stat.isSymbolicLink()}, isFile=${stat.isFile()}, size=${stat.size}`,
        );
        if (stat.isSymbolicLink()) {
          symlinks.push({ name: libFile, target: readlinkSync(src) });
        } else {
          realFiles.push(libFile);
        }
      } catch (e) {
        log(`ORT extract: ${libFile} — stat failed: ${e}`);
        realFiles.push(libFile);
      }
    }

    // Copy real files first
    for (const libFile of realFiles) {
      const src = join(extractedDir, libFile);
      const dst = join(targetDir, libFile);
      try {
        cpFile(src, dst);
        if (process.platform !== "win32") {
          chmodSync(dst, 0o755);
        }
      } catch (copyErr) {
        log(`ORT extract: failed to copy ${libFile}: ${copyErr}`);
      }
    }

    // Recreate symlinks in target directory
    for (const link of symlinks) {
      const dst = join(targetDir, link.name);
      try {
        unlinkSync(dst); // remove if exists from a previous partial install
      } catch {
        // ignore
      }
      symlinkSync(link.target, dst);
    }

    // Cleanup temp directory
    const { rmSync } = await import("node:fs");
    rmSync(tmpDir, { recursive: true, force: true });

    log(`ONNX Runtime v${ORT_VERSION} installed to ${targetDir}`);
    return targetDir;
  } catch (err) {
    error(`Failed to download ONNX Runtime: ${err}`);
    // Cleanup on failure
    try {
      const { rmSync } = await import("node:fs");
      rmSync(`${targetDir}.tmp.${process.pid}`, { recursive: true, force: true });
    } catch {
      // ignore cleanup errors
    }
    return null;
  }
}

async function extractZipArchive(archivePath: string, destinationDir: string): Promise<void> {
  const { execFileSync } = await import("node:child_process");

  if (process.platform === "win32") {
    let powershellError: unknown;

    try {
      execFileSync(
        "powershell.exe",
        [
          "-NoProfile",
          "-NonInteractive",
          "-ExecutionPolicy",
          "Bypass",
          "-Command",
          "& { Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force }",
          archivePath,
          destinationDir,
        ],
        { stdio: "pipe", timeout: 120_000 },
      );
      return;
    } catch (err) {
      powershellError = err;
      warn(`PowerShell Expand-Archive failed, falling back to cmd/tar: ${String(err)}`);
    }

    try {
      execFileSync(
        "cmd.exe",
        ["/d", "/s", "/c", `tar -xf "${archivePath}" -C "${destinationDir}"`],
        { stdio: "pipe", timeout: 120_000 },
      );
      return;
    } catch (cmdError) {
      throw new Error(
        `ZIP extraction failed via PowerShell and cmd/tar. PowerShell: ${String(powershellError)} | cmd/tar: ${String(cmdError)}`,
      );
    }
  }

  execFileSync("unzip", ["-q", archivePath, "-d", destinationDir], {
    stdio: "pipe",
    timeout: 120_000,
  });
}

/**
 * Remove ONNX Runtime from temp files. Cleanup helper for test isolation.
 */
export function cleanupOnnxRuntime(storageDir: string): void {
  try {
    const ortBase = join(storageDir, "onnxruntime");
    if (existsSync(ortBase)) {
      const { rmSync } = require("node:fs");
      rmSync(ortBase, { recursive: true, force: true });
    }
  } catch {
    // ignore
  }
}
