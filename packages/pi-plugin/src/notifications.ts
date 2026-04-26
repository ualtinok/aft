import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { log } from "./logger.js";

const WARNING_MARKER = "🔧 AFT: ⚠️";
const WARNED_TOOLS_FILE = "warned_tools.json";

export interface ConfigureWarning {
  kind: "formatter_not_installed" | "checker_not_installed" | "lsp_binary_missing";
  language?: string;
  server?: string;
  tool?: string;
  binary?: string;
  hint: string;
}

export interface ConfigureWarningOptions {
  client: unknown;
  sessionId: string;
  storageDir: string;
  pluginVersion: string;
  projectRoot?: string;
}

type PiNotificationClient = {
  ui?: {
    notify?: (message: string, type?: "info" | "warning" | "error") => void;
  };
};

function sendIgnoredMessage(client: unknown, _sessionId: string, text: string): boolean {
  const typedClient = client as PiNotificationClient;
  if (typeof typedClient.ui?.notify !== "function") return false;

  try {
    typedClient.ui.notify(text, "warning");
    return true;
  } catch (err) {
    log(`[aft-pi] notification send failed: ${err instanceof Error ? err.message : String(err)}`);
    return false;
  }
}

function readWarnedTools(storageDir: string): Record<string, string> {
  try {
    const warnedToolsPath = join(storageDir, WARNED_TOOLS_FILE);
    if (!existsSync(warnedToolsPath)) return {};

    const parsed = JSON.parse(readFileSync(warnedToolsPath, "utf-8")) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};

    const warned: Record<string, string> = {};
    for (const [key, version] of Object.entries(parsed)) {
      if (typeof version === "string") {
        warned[key] = version;
      }
    }
    return warned;
  } catch {
    return {};
  }
}

function writeWarnedTools(storageDir: string, warned: Record<string, string>): void {
  try {
    mkdirSync(storageDir, { recursive: true });
    const warnedToolsPath = join(storageDir, WARNED_TOOLS_FILE);
    const tempPath = join(storageDir, `.${WARNED_TOOLS_FILE}.${process.pid}.${Date.now()}.tmp`);
    writeFileSync(tempPath, `${JSON.stringify(warned, null, 2)}\n`);
    renameSync(tempPath, warnedToolsPath);
  } catch {
    // best-effort
  }
}

function warningKey(warning: ConfigureWarning, projectRoot?: string): string {
  return [
    projectRoot ?? "_",
    warning.kind,
    warning.language ?? warning.server ?? "_",
    warning.tool ?? warning.binary ?? "_",
    warning.hint,
  ]
    .map((part) => encodeURIComponent(part))
    .join(":");
}

function warningTitle(warning: ConfigureWarning): string {
  switch (warning.kind) {
    case "formatter_not_installed":
      return "Formatter is not installed";
    case "checker_not_installed":
      return "Checker is not installed";
    case "lsp_binary_missing":
      return "LSP binary is missing";
  }
}

function formatConfigureWarning(warning: ConfigureWarning): string {
  const details: string[] = [];
  if (warning.language) details.push(`language: ${warning.language}`);
  if (warning.server) details.push(`server: ${warning.server}`);
  if (warning.tool) details.push(`tool: ${warning.tool}`);
  if (warning.binary && warning.binary !== warning.tool) {
    details.push(`binary: ${warning.binary}`);
  }

  const suffix = details.length > 0 ? ` (${details.join(", ")})` : "";
  return `${WARNING_MARKER} ${warningTitle(warning)}${suffix}\n${warning.hint}`;
}

export async function deliverConfigureWarnings(
  opts: ConfigureWarningOptions,
  warnings: ConfigureWarning[],
): Promise<void> {
  if (warnings.length === 0) return;

  const warned = readWarnedTools(opts.storageDir);
  let changed = false;

  for (const warning of warnings) {
    const key = warningKey(warning, opts.projectRoot);
    if (Object.hasOwn(warned, key)) continue;

    if (!sendIgnoredMessage(opts.client, opts.sessionId, formatConfigureWarning(warning))) {
      continue;
    }

    warned[key] = opts.pluginVersion;
    changed = true;
  }

  if (changed) {
    writeWarnedTools(opts.storageDir, warned);
  }
}
