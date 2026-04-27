import { existsSync, readFileSync, statSync } from "node:fs";
import type { HarnessAdapter } from "../adapters/types.js";
import { type BinaryCacheInfo, getBinaryCacheInfo } from "./binary-cache.js";
import { probeBinaryVersion } from "./binary-probe.js";
import { readJsoncFile } from "./jsonc.js";
import { getLspCacheReport, type LspCacheReport } from "./lsp-cache.js";
import {
  detectOrtVersion,
  findCachedOnnxRuntime,
  findSystemOnnxRuntime,
  getManualInstallHint,
  isOrtVersionCompatible,
  REQUIRED_ORT_MAJOR,
  REQUIRED_ORT_MIN_MINOR,
} from "./onnx.js";
import { sanitizeValue } from "./sanitize.js";
import { getSelfVersion } from "./self-version.js";

export interface DiagnosticReport {
  timestamp: string;
  platform: string;
  arch: string;
  nodeVersion: string;
  cliVersion: string;
  binaryVersion: string | null;
  harnesses: HarnessDiagnostic[];
  binaryCache: BinaryCacheInfo;
  /** LSP package and binary caches populated by plugin auto-install. */
  lspCache: LspCacheReport;
}

export interface HarnessDiagnostic {
  kind: string;
  displayName: string;
  hostInstalled: boolean;
  hostVersion: string | null;
  pluginRegistered: boolean;
  configPaths: ReturnType<HarnessAdapter["detectConfigPaths"]>;
  aftConfig: {
    exists: boolean;
    parseError?: string;
    flags: Record<string, unknown>;
  };
  pluginCache: ReturnType<HarnessAdapter["getPluginCacheInfo"]>;
  storageDir: {
    path: string;
    exists: boolean;
    sizesByKey: Record<string, number>;
  };
  onnxRuntime: {
    required: boolean;
    systemPath: string | null;
    systemVersion: string | null;
    systemCompatible: boolean | null;
    cachedPath: string | null;
    cachedVersion: string | null;
    cachedCompatible: boolean | null;
    platform: string;
    installHint: string;
    requirement: string;
  };
  logFile: {
    path: string;
    exists: boolean;
    sizeKb: number;
  };
}

export async function collectDiagnostics(adapters: HarnessAdapter[]): Promise<DiagnosticReport> {
  const cliVersion = getSelfVersion();
  const binaryVersion = probeBinaryVersion(cliVersion);

  const harnesses: HarnessDiagnostic[] = [];
  for (const adapter of adapters) {
    harnesses.push(await diagnoseHarness(adapter));
  }

  return {
    timestamp: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    nodeVersion: process.version,
    cliVersion,
    binaryVersion,
    harnesses,
    binaryCache: getBinaryCacheInfo(cliVersion),
    lspCache: getLspCacheReport(),
  };
}

async function diagnoseHarness(adapter: HarnessAdapter): Promise<HarnessDiagnostic> {
  const configPaths = adapter.detectConfigPaths();
  const aftConfigRead = readJsoncFile(configPaths.aftConfig);
  const aftFlags = (sanitizeValue(aftConfigRead.value ?? {}) as Record<string, unknown>) ?? {};
  const storage = adapter.getStorageDir();
  const logPath = adapter.getLogFile();

  const describeStorage =
    "describeStorageSubtrees" in adapter &&
    typeof (adapter as unknown as { describeStorageSubtrees: () => Record<string, number> })
      .describeStorageSubtrees === "function"
      ? (
          adapter as unknown as { describeStorageSubtrees: () => Record<string, number> }
        ).describeStorageSubtrees()
      : {};

  const semanticEnabled =
    (aftConfigRead.value as Record<string, unknown> | null)?.experimental_semantic_search === true;

  const systemOrtDir = findSystemOnnxRuntime();
  const cachedOrtDir = findCachedOnnxRuntime(storage);
  const systemVersion = systemOrtDir ? detectOrtVersion(systemOrtDir) : null;
  const cachedVersion = cachedOrtDir ? detectOrtVersion(cachedOrtDir) : null;

  return {
    kind: adapter.kind,
    displayName: adapter.displayName,
    hostInstalled: adapter.isInstalled(),
    hostVersion: adapter.getHostVersion(),
    pluginRegistered: adapter.hasPluginEntry(),
    configPaths,
    aftConfig: {
      exists: existsSync(configPaths.aftConfig),
      ...(aftConfigRead.error ? { parseError: aftConfigRead.error } : {}),
      flags: aftFlags,
    },
    pluginCache: adapter.getPluginCacheInfo(),
    storageDir: {
      path: storage,
      exists: existsSync(storage),
      sizesByKey: describeStorage,
    },
    onnxRuntime: {
      required: semanticEnabled,
      systemPath: systemOrtDir,
      systemVersion,
      systemCompatible: systemVersion ? isOrtVersionCompatible(systemVersion) : null,
      cachedPath: cachedOrtDir,
      cachedVersion,
      cachedCompatible: cachedVersion ? isOrtVersionCompatible(cachedVersion) : null,
      platform: `${process.platform}-${process.arch}`,
      installHint: getManualInstallHint(),
      requirement: `>=${REQUIRED_ORT_MAJOR}.${REQUIRED_ORT_MIN_MINOR}`,
    },
    logFile: {
      path: logPath,
      exists: existsSync(logPath),
      sizeKb: existsSync(logPath) ? Math.round(statSync(logPath).size / 1024) : 0,
    },
  };
}

export function renderDiagnosticsMarkdown(report: DiagnosticReport): string {
  const lines: string[] = [];
  lines.push(`- Timestamp: ${report.timestamp}`);
  lines.push(`- CLI: v${report.cliVersion}`);
  lines.push(`- Binary: ${report.binaryVersion ?? "unknown"}`);
  lines.push(`- OS: ${report.platform} ${report.arch}`);
  lines.push(`- Node: ${report.nodeVersion}`);

  for (const h of report.harnesses) {
    lines.push("");
    lines.push(`### ${h.displayName}`);
    lines.push(`- Host installed: ${h.hostInstalled}${h.hostVersion ? ` (${h.hostVersion})` : ""}`);
    lines.push(`- Plugin registered: ${h.pluginRegistered}`);
    lines.push(`- AFT config parse error: ${h.aftConfig.parseError ?? "none"}`);
    lines.push("");
    lines.push("#### Config paths");
    lines.push("```json");
    lines.push(JSON.stringify(h.configPaths, null, 2));
    lines.push("```");
    lines.push("");
    lines.push("#### AFT flags");
    lines.push("```json");
    lines.push(JSON.stringify(h.aftConfig.flags, null, 2));
    lines.push("```");
    lines.push("");
    lines.push("#### Plugin cache");
    lines.push("```json");
    lines.push(JSON.stringify(h.pluginCache, null, 2));
    lines.push("```");
    lines.push("");
    lines.push("#### Storage");
    lines.push("```json");
    lines.push(JSON.stringify(h.storageDir, null, 2));
    lines.push("```");
    lines.push("");
    lines.push("#### ONNX Runtime");
    lines.push("```json");
    lines.push(JSON.stringify(h.onnxRuntime, null, 2));
    lines.push("```");
    lines.push("");
    lines.push(`#### Log file`);
    lines.push(
      `\`${h.logFile.path}\` (${h.logFile.exists ? `${h.logFile.sizeKb} KB` : "missing"})`,
    );
  }

  lines.push("");
  lines.push("### Binary cache");
  lines.push("```json");
  lines.push(JSON.stringify(report.binaryCache, null, 2));
  lines.push("```");

  lines.push("");
  lines.push("### LSP cache");
  lines.push("```json");
  lines.push(JSON.stringify(report.lspCache, null, 2));
  lines.push("```");
  return lines.join("\n");
}

/** Utility: read the tail of a log file, best-effort. */
export function tailLogFile(path: string, lines: number): string {
  if (!existsSync(path)) return "";
  try {
    const raw = readFileSync(path, "utf-8");
    return raw.split(/\r?\n/).slice(-lines).join("\n").trim();
  } catch {
    return "";
  }
}
