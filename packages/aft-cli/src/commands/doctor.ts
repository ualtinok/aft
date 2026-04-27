import { writeFileSync } from "node:fs";
import { join } from "node:path";

import type { HarnessAdapter } from "../adapters/types.js";
import { collectDiagnostics, renderDiagnosticsMarkdown, tailLogFile } from "../lib/diagnostics.js";
import { dirSize, formatBytes } from "../lib/fs-util.js";
import { createGitHubIssue, isGhInstalled, openBrowser } from "../lib/github.js";
import { resolveAdaptersForCommand } from "../lib/harness-select.js";
import { type ClearResult, clearLspCaches } from "../lib/lsp-cache.js";
import { intro, log, note, outro, selectMany, text } from "../lib/prompts.js";
import { sanitizeContent } from "../lib/sanitize.js";

export type DoctorClearTarget = "plugin-cache" | "lsp-cache";

export const DOCTOR_CLEAR_TARGET_OPTIONS: { label: string; value: DoctorClearTarget }[] = [
  {
    label: "Plugin npm cache (~/.cache/opencode/packages/@cortexkit/aft-opencode@latest, etc.)",
    value: "plugin-cache",
  },
  {
    label: "LSP install cache (~/.cache/aft/lsp-packages/, ~/.cache/aft/lsp-binaries/)",
    value: "lsp-cache",
  },
];

export const DOCTOR_FORCE_CLEAR_TARGETS: DoctorClearTarget[] = ["plugin-cache"];

export interface DoctorOptions {
  clear: boolean;
  force: boolean;
  issue: boolean;
  argv: string[];
}

export interface CacheClearSummary {
  hadErrors: boolean;
  pluginCache?: {
    cleared: number;
    totalBytes: number;
    errors: number;
  };
  lspCache?: {
    cleared: number;
    totalBytes: number;
    errors: number;
  };
}

export interface CacheClearOptions {
  clearLspCaches?: () => ClearResult;
  includePluginBytes?: boolean;
}

export async function runDoctor(options: DoctorOptions): Promise<number> {
  if (options.issue) {
    return runIssueFlow(options.argv);
  }
  intro("AFT doctor");

  if (options.clear) {
    return runClearFlow(options.argv);
  }

  const adapters = await resolveAdaptersForCommand(options.argv, {
    allowMulti: true,
    verb: "diagnose",
  });

  const report = await collectDiagnostics(adapters);

  log.info(`CLI v${report.cliVersion}, binary ${report.binaryVersion ?? "unknown"}`);
  log.info(
    `Binary cache: ${report.binaryCache.versions.length} version(s), ${formatBytes(report.binaryCache.totalSize)} at ${report.binaryCache.path}`,
  );

  const npmCount = report.lspCache.npm.entries.length;
  const ghCount = report.lspCache.github.entries.length;
  if (npmCount + ghCount > 0) {
    log.info(
      `LSP cache: ${npmCount} npm + ${ghCount} github install(s), ${formatBytes(report.lspCache.totalSize)} total`,
    );
  }

  let hadProblems = false;
  for (const h of report.harnesses) {
    log.step(`${h.displayName}`);
    if (!h.hostInstalled) {
      log.warn(`  host not installed — install from: ${describeAdapterInstallHint(h.kind)}`);
      hadProblems = true;
      continue;
    }
    log.info(`  host: ${h.hostVersion ?? "unknown version"}`);
    log.info(`  plugin registered: ${h.pluginRegistered ? "yes" : "no"}`);
    if (!h.pluginRegistered) hadProblems = true;

    log.info(`  aft config: ${h.aftConfig.exists ? h.configPaths.aftConfig : "(not set)"}`);
    if (h.aftConfig.parseError) {
      log.error(`  aft config parse error: ${h.aftConfig.parseError}`);
      hadProblems = true;
    }

    log.info(
      `  storage: ${h.storageDir.exists ? h.storageDir.path : "(not created)"} (${formatStorageSizes(h.storageDir.sizesByKey)})`,
    );

    if (h.onnxRuntime.required) {
      const parts: string[] = [];
      parts.push(`required: yes (${h.onnxRuntime.platform})`);
      if (h.onnxRuntime.cachedPath) {
        parts.push(
          `cached: ${h.onnxRuntime.cachedVersion ?? "unknown"}${h.onnxRuntime.cachedCompatible === false ? " (incompatible)" : ""}`,
        );
      }
      if (h.onnxRuntime.systemPath) {
        parts.push(
          `system: ${h.onnxRuntime.systemVersion ?? "unknown"}${h.onnxRuntime.systemCompatible === false ? " (incompatible)" : ""}`,
        );
      }
      if (!h.onnxRuntime.cachedPath && !h.onnxRuntime.systemPath) {
        parts.push(`not installed — ${h.onnxRuntime.installHint}`);
        hadProblems = true;
      }
      log.info(`  onnx runtime: ${parts.join(" · ")}`);
    }

    log.info(
      `  log: ${h.logFile.exists ? `${h.logFile.path} (${h.logFile.sizeKb} KB)` : "(not written yet)"}`,
    );
  }

  // Apply automatic fixes where useful.
  if (options.force) {
    await clearDoctorCaches(adapters, DOCTOR_FORCE_CLEAR_TARGETS, { includePluginBytes: false });
  }

  for (const adapter of adapters) {
    await maybeFixPlugin(adapter);
  }

  if (hadProblems) {
    note(
      "Run `aft setup` to register AFT with any harness showing `plugin registered: no`.",
      "Tips",
    );
    outro("Done — some issues found.");
    return 1;
  }
  outro("Everything looks good.");
  return 0;
}

async function runClearFlow(argv: string[]): Promise<number> {
  const targets = await selectMany<DoctorClearTarget>(
    "What do you want to clear?",
    DOCTOR_CLEAR_TARGET_OPTIONS,
    undefined,
    false,
  );

  if (targets.length === 0) {
    log.info("No cache categories selected; nothing to clear.");
    outro("Done.");
    return 0;
  }

  const adapters = targets.includes("plugin-cache")
    ? await resolveAdaptersForCommand(argv, {
        allowMulti: true,
        verb: "clear plugin cache for",
      })
    : [];

  const summary = await clearDoctorCaches(adapters, targets);
  outro(summary.hadErrors ? "Done — some cache entries could not be cleared." : "Done.");
  return summary.hadErrors ? 1 : 0;
}

export async function clearDoctorCaches(
  adapters: HarnessAdapter[],
  targets: readonly DoctorClearTarget[],
  options: CacheClearOptions = {},
): Promise<CacheClearSummary> {
  const summary: CacheClearSummary = { hadErrors: false };

  if (targets.includes("plugin-cache")) {
    let cleared = 0;
    let totalBytes = 0;
    let errors = 0;

    for (const adapter of adapters) {
      const result = await clearPluginCache(adapter, options.includePluginBytes ?? true);
      if (result.action === "cleared") {
        cleared += 1;
        totalBytes += result.bytes;
      } else if (result.action === "error") {
        errors += 1;
        summary.hadErrors = true;
      }
    }

    summary.pluginCache = { cleared, totalBytes, errors };
  }

  if (targets.includes("lsp-cache")) {
    const cleanup = (options.clearLspCaches ?? clearLspCaches)();
    reportLspCacheClear(cleanup);
    if (cleanup.errors.length > 0) {
      summary.hadErrors = true;
    }
    summary.lspCache = {
      cleared: cleanup.cleared.length,
      totalBytes: cleanup.totalBytes,
      errors: cleanup.errors.length,
    };
  }

  return summary;
}

async function clearPluginCache(
  adapter: HarnessAdapter,
  includeBytes: boolean,
): Promise<{ action: "cleared" | "not_applicable" | "not_found" | "error"; bytes: number }> {
  const info = adapter.getPluginCacheInfo();
  const bytes = info.exists ? dirSize(info.path) : 0;
  const result = await adapter.clearPluginCache(true);

  if (result.action === "cleared") {
    const suffix = includeBytes ? `, reclaimed ${formatBytes(bytes)}` : "";
    log.success(`${adapter.displayName}: cleared plugin cache at ${result.path}${suffix}`);
    return { action: "cleared", bytes };
  }
  if (result.action === "not_applicable") {
    log.info(`${adapter.displayName}: no user-managed plugin cache to clear`);
    return { action: "not_applicable", bytes: 0 };
  }
  if (result.action === "not_found") {
    log.info(`${adapter.displayName}: no plugin cache found at ${result.path}`);
    return { action: "not_found", bytes: 0 };
  }
  if (result.action === "error") {
    log.error(`${adapter.displayName}: cache clear failed: ${result.error ?? "unknown"}`);
    return { action: "error", bytes: 0 };
  }

  return { action: "not_found", bytes: 0 };
}

function reportLspCacheClear(cleanup: ClearResult): void {
  if (cleanup.cleared.length === 0) {
    log.info("LSP install cache: nothing to clear, reclaimed 0 B");
  } else {
    log.success(
      `LSP install cache: cleared ${cleanup.cleared.length} install(s), reclaimed ${formatBytes(cleanup.totalBytes)}`,
    );
  }
  for (const err of cleanup.errors) {
    log.error(`LSP install cache: failed to remove ${err.path}: ${err.error}`);
  }
}

async function maybeFixPlugin(adapter: HarnessAdapter): Promise<void> {
  if (!adapter.hasPluginEntry() && adapter.isInstalled()) {
    log.info(`${adapter.displayName}: attempting to register plugin…`);
    const r = await adapter.ensurePluginEntry();
    if (r.ok) {
      log.success(`${adapter.displayName}: ${r.message}`);
    } else {
      log.error(`${adapter.displayName}: ${r.message}`);
    }
  }
}

function describeAdapterInstallHint(kind: string): string {
  if (kind === "opencode") return "https://opencode.ai/docs/install";
  if (kind === "pi") return "https://github.com/badlogic/pi-mono";
  return "(unknown harness)";
}

function formatStorageSizes(sizes: Record<string, number>): string {
  const parts = Object.entries(sizes)
    .filter(([, size]) => size > 0)
    .map(([key, size]) => `${key}: ${formatBytes(size)}`);
  return parts.length > 0 ? parts.join(", ") : "empty";
}

/**
 * `aft doctor --issue` flow — collect diagnostics, sanitize user paths,
 * prompt for an issue description, optionally file via `gh`.
 */
async function runIssueFlow(argv: string[]): Promise<number> {
  intro("AFT doctor --issue");

  const adapters = await resolveAdaptersForCommand(argv, {
    allowMulti: true,
    verb: "include in the issue",
  });

  const description = await text("Describe the problem you're running into:", {
    placeholder: "What happened? What did you expect? Steps to reproduce…",
    validate: (value) =>
      value.trim().length === 0 ? "Please enter a short description." : undefined,
  });

  const report = await collectDiagnostics(adapters);

  const logSections = adapters
    .map((adapter) => {
      const path = adapter.getLogFile();
      const tail = tailLogFile(path, 200);
      return `#### ${adapter.displayName} log (${path})\n\n\`\`\`\n${tail || "<no log output>"}\n\`\`\`\n`;
    })
    .join("\n");

  const rawBody = [
    "## Description",
    description,
    "",
    "## Environment",
    `- CLI: v${report.cliVersion}`,
    `- Binary: ${report.binaryVersion ?? "unknown"}`,
    `- OS: ${report.platform} ${report.arch}`,
    `- Node: ${report.nodeVersion}`,
    "",
    "## Diagnostics",
    renderDiagnosticsMarkdown(report),
    "",
    "## Logs (last 200 lines per harness)",
    logSections,
    "_Usernames and home paths have been stripped from this report._",
  ].join("\n");
  const body = sanitizeContent(rawBody);

  const title = `AFT issue: ${description.slice(0, 72)}`;
  const outPath = join(process.cwd(), `aft-issue-${Date.now()}.md`);
  writeFileSync(outPath, `${body}\n`);
  log.success(`Wrote sanitized issue body to ${outPath}`);

  if (isGhInstalled()) {
    log.info("Opening GitHub issue via `gh`…");
    const result = createGitHubIssue("cortexkit/aft", title, body);
    if (result.url) {
      log.success(`Issue filed: ${result.url}`);
      openBrowser(result.url);
      outro("Done.");
      return 0;
    }
    log.warn(`gh failed: ${result.stderr ?? "unknown error"}. Falling back to browser.`);
  }

  const fallback = `https://github.com/cortexkit/aft/issues/new?title=${encodeURIComponent(title)}&body=${encodeURIComponent(body)}`;
  log.info("Opening GitHub issue form in your browser…");
  openBrowser(fallback);
  note(
    `If the browser didn't open, the sanitized body is at ${outPath}. Copy it into a new issue at https://github.com/cortexkit/aft/issues/new.`,
    "Fallback",
  );
  outro("Done.");
  return 0;
}
