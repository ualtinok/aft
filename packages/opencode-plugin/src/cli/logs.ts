import { readFileSync, writeFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { join } from "node:path";
import type { DiagnosticReport } from "./diagnostics.js";
import { renderDiagnosticsMarkdown } from "./diagnostics.js";

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Strip personally identifiable path segments from arbitrary text.
 * Used on both log contents and the full doctor --issue body so diagnostic
 * reports never leak usernames or home directory paths.
 */
export function sanitizeContent(content: string): string {
  const username = userInfo().username;
  const home = homedir();

  let sanitized = content;
  if (home) {
    sanitized = sanitized.replace(new RegExp(escapeRegex(home), "g"), "~");
  }
  sanitized = sanitized.replace(/\/Users\/[^/\s"']+/g, "/Users/<USER>");
  sanitized = sanitized.replace(/\/home\/[^/\s"']+/g, "/home/<USER>");
  sanitized = sanitized.replace(/C:\\\\Users\\\\[^\\\\"'\s]+/g, "C:\\\\Users\\\\<USER>");
  sanitized = sanitized.replace(/C:\\Users\\[^\\"'\s]+/g, "C:\\Users\\<USER>");
  if (username) {
    sanitized = sanitized.replace(new RegExp(escapeRegex(username), "g"), "<USER>");
  }
  return sanitized;
}

/**
 * @deprecated Use sanitizeContent. Kept as an alias for backward compatibility.
 */
export const sanitizeLogContent = sanitizeContent;

function formatTimestamp(date: Date): string {
  const pad = (value: number) => String(value).padStart(2, "0");
  return [
    String(date.getFullYear()),
    pad(date.getMonth() + 1),
    pad(date.getDate()),
    "-",
    pad(date.getHours()),
    pad(date.getMinutes()),
    pad(date.getSeconds()),
  ].join("");
}

export async function bundleIssueReport(
  report: DiagnosticReport,
  description: string,
  _title: string,
): Promise<{ path: string; bodyMarkdown: string }> {
  const logLines = report.logFile.exists
    ? readFileSync(report.logFile.path, "utf-8").split(/\r?\n/)
    : [];
  const recentLog = logLines.slice(-200).join("\n").trim();
  const configBody = JSON.stringify(report.aftConfig.flags, null, 2);

  const rawBody = [
    "## Description",
    description,
    "",
    "## Environment",
    `- Plugin: v${report.pluginVersion}`,
    `- Binary: ${report.binaryVersion ?? "unknown"}`,
    `- OS: ${report.platform} ${report.arch}`,
    `- Node: ${report.nodeVersion}`,
    `- OpenCode: ${report.opencodeVersion ?? "not installed"}`,
    "",
    "## Configuration",
    `Enabled flags from \`${report.configPaths.aftConfig}\`:`,
    "```jsonc",
    configBody,
    "```",
    "",
    "## Diagnostics",
    renderDiagnosticsMarkdown(report),
    "",
    "## Log (last 200 lines)",
    "```",
    recentLog || "<no log output>",
    "```",
    "",
    "_Username and home paths have been stripped from this report._",
  ].join("\n");

  // Sanitize the entire body in one pass — covers diagnostics, config paths,
  // environment lines, and log contents uniformly.
  const bodyMarkdown = sanitizeContent(rawBody);

  const path = join(process.cwd(), `aft-issue-${formatTimestamp(new Date())}.md`);
  writeFileSync(path, `${bodyMarkdown}\n`);
  return { path, bodyMarkdown };
}
