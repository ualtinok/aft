import { execSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path, { dirname } from "node:path";
import { parse, stringify } from "comment-json";
import { ensureTuiPluginEntry } from "../shared/tui-config.js";
import { clearPluginCache } from "./cache.js";
import { detectConfigPaths } from "./config-paths.js";
import { collectDiagnostics, type DiagnosticReport } from "./diagnostics.js";
import { bundleIssueReport } from "./logs.js";
import { getOpenCodeVersion, isGhInstalled, isOpenCodeInstalled } from "./opencode-helpers.js";
import { confirm, intro, log, outro, spinner, text } from "./prompts.js";

const PLUGIN_NAME = "@cortexkit/aft-opencode";
const PLUGIN_ENTRY = `${PLUGIN_NAME}@latest`;

function ensureDir(path: string): void {
  mkdirSync(dirname(path), { recursive: true });
}

function parseConfig(path: string): Record<string, unknown> | null {
  if (!existsSync(path)) {
    return null;
  }
  try {
    return (parse(readFileSync(path, "utf-8")) as Record<string, unknown>) ?? {};
  } catch {
    return null;
  }
}

function matchesPluginEntry(entry: string): boolean {
  if (entry === PLUGIN_NAME || entry.startsWith(`${PLUGIN_NAME}@`)) return true;
  // Local dev paths containing our package name or entry point
  if (entry.includes("/opencode-plugin") || entry.includes("/aft-opencode")) return true;
  return false;
}

function hasPluginEntry(config: Record<string, unknown> | null): boolean {
  const plugins = Array.isArray(config?.plugin) ? config.plugin : [];
  return plugins.some((entry) => typeof entry === "string" && matchesPluginEntry(entry));
}

function ensurePluginEntry(): {
  status: "created" | "added" | "already" | "error";
  message?: string;
} {
  const paths = detectConfigPaths();

  try {
    ensureDir(paths.opencodeConfig);

    if (paths.opencodeConfigFormat === "none") {
      writeFileSync(paths.opencodeConfig, `${stringify({ plugin: [PLUGIN_ENTRY] }, null, 2)}\n`);
      return { status: "created" };
    }

    const config = parseConfig(paths.opencodeConfig);
    if (!config) {
      return { status: "error", message: `Could not parse ${paths.opencodeConfig}` };
    }

    const plugins = Array.isArray(config.plugin)
      ? config.plugin.filter((entry) => typeof entry === "string")
      : [];
    if (plugins.some((entry) => matchesPluginEntry(entry as string))) {
      return { status: "already" };
    }

    config.plugin = [...plugins, PLUGIN_ENTRY];
    writeFileSync(paths.opencodeConfig, `${stringify(config, null, 2)}\n`);
    return { status: "added" };
  } catch (error) {
    return {
      status: "error",
      message: error instanceof Error ? error.message : String(error),
    };
  }
}

function openBrowser(url: string): void {
  try {
    if (process.platform === "darwin") {
      const child = spawnSync("open", [url], { stdio: "ignore" });
      if (child.status === 0) return;
    } else if (process.platform === "linux") {
      const child = spawnSync("xdg-open", [url], { stdio: "ignore" });
      if (child.status === 0) return;
    } else if (process.platform === "win32") {
      const child = spawnSync("cmd", ["/c", "start", "", url], { stdio: "ignore" });
      if (child.status === 0) return;
    }
  } catch {
    // Best-effort only.
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function summarizeFlags(flags: Record<string, unknown>): string {
  const entries = Object.entries(flags)
    .filter(([, value]) => value !== undefined)
    .map(([key, value]) => `${key}=${JSON.stringify(value)}`);
  return entries.length > 0 ? entries.join(", ") : "using defaults";
}

const REQUIRED_ORT_MAJOR = 1;
const REQUIRED_ORT_MIN_MINOR = 20;

function detectOrtVersion(libDir: string): string | null {
  try {
    const entries = readdirSync(libDir);
    for (const entry of entries) {
      if (!entry.startsWith("libonnxruntime")) continue;
      // Match patterns: libonnxruntime.so.1.19.0, libonnxruntime.1.24.4.dylib
      const match = entry.match(/(\d+\.\d+\.\d+)/);
      if (match) return match[1];
    }
  } catch {
    // directory doesn't exist or not readable
  }
  return null;
}

function checkOrtVersion(report: DiagnosticReport): number {
  let issues = 0;

  // Check our auto-downloaded cache first
  if (report.onnxRuntime.cachedPath) {
    const cachedDir = path.dirname(report.onnxRuntime.cachedPath);
    const version = detectOrtVersion(cachedDir);
    if (version) {
      log.success(`ONNX Runtime v${version} (auto-downloaded) at ${report.onnxRuntime.cachedPath}`);
      return 0;
    }
  }

  // Check system path
  const systemPaths =
    process.platform === "darwin"
      ? ["/opt/homebrew/lib", "/usr/local/lib"]
      : ["/usr/local/lib", "/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu"];

  for (const dir of systemPaths) {
    const version = detectOrtVersion(dir);
    if (version) {
      const parts = version.split(".").map(Number);
      const [major, minor] = [parts[0], parts[1]];

      if (major === REQUIRED_ORT_MAJOR && minor >= REQUIRED_ORT_MIN_MINOR) {
        log.success(`ONNX Runtime v${version} (compatible) at ${dir}`);
        return 0;
      }

      // Version mismatch — this is the issue #4 scenario
      log.error(
        `ONNX Runtime v${version} found at ${dir} — INCOMPATIBLE (need v1.${REQUIRED_ORT_MIN_MINOR}+)`,
      );
      log.info("This version mismatch causes AFT to crash when semantic search initializes.");
      log.info("Solutions:");
      if (process.platform === "linux") {
        log.info(`  1. Remove old version: sudo rm ${dir}/libonnxruntime* && sudo ldconfig`);
        log.info("     Then restart OpenCode — AFT auto-downloads the correct version.");
      } else {
        log.info(`  1. Remove old version: sudo rm ${dir}/libonnxruntime*`);
        log.info("     Then restart OpenCode — AFT auto-downloads the correct version.");
      }
      log.info(
        "  2. Or install v1.24: https://github.com/microsoft/onnxruntime/releases/tag/v1.24.0",
      );
      issues++;
      return issues;
    }
  }

  // No system ORT found — check if auto-download is expected to handle it
  if (report.onnxRuntime.required && !report.onnxRuntime.cachedPath) {
    log.warn("ONNX Runtime not found — AFT will attempt auto-download on startup");
  }

  return issues;
}

function warnAboutLinuxLdconfig(systemPath: string | null): boolean {
  if (process.platform !== "linux") {
    return false;
  }

  const libPath = "/usr/local/lib/libonnxruntime.so";
  if (!existsSync(libPath)) {
    return false;
  }

  try {
    const output = execSync("ldconfig -p | grep libonnxruntime", {
      stdio: "pipe",
      encoding: "utf-8",
    });
    if (!output.includes("/usr/local/lib")) {
      log.warn("Found /usr/local/lib/libonnxruntime.so but it is not present in ldconfig output");
      if (!systemPath) {
        log.info("Run `sudo ldconfig` or install ONNX Runtime from your package manager");
      }
      return true;
    }
  } catch {
    log.warn("Could not verify libonnxruntime via `ldconfig -p`");
    return true;
  }

  return false;
}

async function runIssueFlow(): Promise<number> {
  intro("AFT Issue Report");

  const title = await text("Issue title", {
    placeholder: "Short summary of the problem",
    validate: (value) => (value.trim() ? undefined : "Title is required"),
  });
  const description = await text("Issue description", {
    placeholder: "Describe what happened, what you expected, and any repro steps",
    validate: (value) => (value.trim() ? undefined : "Description is required"),
  });

  const s = spinner();
  s.start("Collecting diagnostics");

  try {
    const report = await collectDiagnostics();
    const bundled = await bundleIssueReport(report, description, title);
    s.stop(`Report written to ${bundled.path}`);

    const shouldSubmit = await confirm("Submit this issue now?", true);
    if (shouldSubmit && isGhInstalled()) {
      const result = spawnSync(
        "gh",
        ["issue", "create", "-R", "cortexkit/aft", "--title", title, "--body-file", bundled.path],
        { encoding: "utf-8", stdio: ["ignore", "pipe", "pipe"] },
      );

      if (result.status === 0) {
        log.success(result.stdout.trim());
        outro("Issue submitted");
        return 0;
      }

      log.warn(result.stderr.trim() || "gh issue create failed");
    }

    const url = `https://github.com/cortexkit/aft/issues/new?title=${encodeURIComponent(title)}&template=bug_report.yml`;
    log.info(`Open this URL and paste the contents of ${bundled.path} into the Diagnostics field`);
    log.info(url);
    openBrowser(url);
    outro("Issue report ready");
    return 0;
  } catch (error) {
    s.stop("Diagnostic collection failed");
    log.error(error instanceof Error ? error.message : String(error));
    outro("Issue report failed");
    return 1;
  }
}

export async function runDoctor(
  options: { force?: boolean; issue?: boolean } = {},
): Promise<number> {
  if (options.issue) {
    return runIssueFlow();
  }

  intro("AFT Doctor");

  let issues = 0;
  let fixed = 0;

  if (!isOpenCodeInstalled()) {
    log.error("OpenCode is not installed or not in PATH");
    outro("Doctor failed - install OpenCode first");
    return 1;
  }

  log.success(`OpenCode installed (${getOpenCodeVersion() ?? "version unknown"})`);

  const paths = detectConfigPaths();
  log.info(`Config dir: ${paths.configDir}`);
  log.info(`OpenCode config: ${paths.opencodeConfig} (${paths.opencodeConfigFormat})`);
  log.info(`AFT config: ${paths.aftConfig} (${paths.aftConfigFormat})`);
  log.info(`TUI config: ${paths.tuiConfig} (${paths.tuiConfigFormat})`);

  const pluginResult = ensurePluginEntry();
  if (pluginResult.status === "created") {
    log.success("Created opencode config with the AFT plugin entry");
    fixed++;
  } else if (pluginResult.status === "added") {
    log.success("Added AFT to the OpenCode plugin list");
    fixed++;
  } else if (pluginResult.status === "already") {
    log.success("OpenCode config already includes AFT");
  } else {
    log.warn(pluginResult.message ?? "Could not verify the OpenCode plugin entry");
    issues++;
  }

  const hadTuiPlugin = hasPluginEntry(parseConfig(paths.tuiConfig));
  const tuiAdded = ensureTuiPluginEntry();
  const hasTuiPluginNow = hasPluginEntry(parseConfig(paths.tuiConfig));
  if (tuiAdded) {
    log.success("Added AFT to tui.json");
    fixed++;
  } else if (hadTuiPlugin || hasTuiPluginNow) {
    log.success("TUI plugin entry configured");
  } else {
    log.warn("Could not verify the TUI plugin entry");
    issues++;
  }

  const cacheResult = await clearPluginCache(options.force);
  if (cacheResult.action === "cleared") {
    log.success(
      `Cleared plugin cache at ${cacheResult.path}${cacheResult.cached ? ` (cached ${cacheResult.cached}, latest ${cacheResult.latest ?? "unknown"})` : ""}`,
    );
    fixed++;
  } else if (cacheResult.action === "up_to_date") {
    log.success(`Plugin cache is up to date (${cacheResult.cached})`);
  } else if (cacheResult.action === "not_found") {
    log.success("Plugin cache is empty");
  } else {
    log.warn(`Could not clear plugin cache: ${cacheResult.error ?? "unknown error"}`);
    issues++;
  }

  const report = await collectDiagnostics();

  if (report.aftConfig.parseError) {
    log.warn(`Could not parse aft config: ${report.aftConfig.parseError}`);
    issues++;
  } else if (report.aftConfig.exists) {
    log.success(`AFT config loaded: ${summarizeFlags(report.aftConfig.flags)}`);
  } else {
    log.info("No aft.json/jsonc found - using plugin defaults");
  }

  if (!report.opencodeConfigHasPlugin) {
    log.warn("OpenCode config still does not include the AFT plugin entry");
    issues++;
  }

  if (report.binaryCache.versions.length === 0) {
    log.warn("No cached AFT binaries found");
  } else if (report.binaryCache.activeVersion) {
    log.success(
      `Binary cache active version: ${report.binaryCache.activeVersion} (${formatBytes(report.binaryCache.totalSize)})`,
    );
  } else {
    log.warn(`Binary cache has no entry matching plugin v${report.pluginVersion}`);
    issues++;
  }

  const orphaned = report.binaryCache.versions.filter(
    (v) => v !== report.binaryCache.activeVersion,
  );
  if (orphaned.length > 0) {
    const preview = orphaned.slice(0, 5).join(", ");
    const more = orphaned.length > 5 ? `, ... (+${orphaned.length - 5} more)` : "";
    log.warn(`Binary cache has ${orphaned.length} orphaned version(s): ${preview}${more}`);
    log.info("Run `rm -rf ~/.cache/aft/bin/<version>` to clean them up");
  }

  if (report.onnxRuntime.required) {
    // Version-aware ORT check — detects incompatible versions (issue #4)
    issues += checkOrtVersion(report);

    if (warnAboutLinuxLdconfig(report.onnxRuntime.systemPath)) {
      issues++;
    }
  }

  log.info(
    `Storage: index=${formatBytes(report.storageDir.indexSize)}, semantic=${formatBytes(report.storageDir.semanticSize)}, backups=${formatBytes(report.storageDir.backupsSize)}, url_cache=${formatBytes(report.storageDir.urlCacheSize)}, onnxruntime=${formatBytes(report.storageDir.onnxruntimeSize)}`,
  );
  log.info(
    `Log file: ${report.logFile.path} (${report.logFile.exists ? `${report.logFile.sizeKb} KB` : "not created"})`,
  );

  if (issues === 0 && fixed === 0) {
    outro("Everything looks good");
    return 0;
  }
  if (issues === 0) {
    outro(`Fixed ${fixed} issue(s). Restart OpenCode to apply changes.`);
    return 0;
  }
  if (fixed > 0) {
    outro(`Found ${issues} issue(s); fixed ${fixed}. Review the warnings above.`);
    return 1;
  }

  outro(`Found ${issues} issue(s) that need manual attention.`);
  return 1;
}
