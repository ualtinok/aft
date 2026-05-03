/**
 * Configure-warning delivery helper.
 *
 * IMPORTANT — DO NOT MOVE BACK INTO `index.ts`.
 *
 * OpenCode's plugin loader (`getLegacyPlugins` in
 * `~/Work/OSS/opencode/packages/opencode/src/plugin/index.ts`) walks
 * `Object.values(mod)` of the plugin's main module and treats every
 * top-level export as either a server plugin function or an object
 * with a `.server` plugin function. Anything else throws
 * `TypeError: Plugin export is not a function` and the plugin fails to
 * load. Function exports get called as plugins, their (often `void`)
 * return value gets pushed into the hooks array, and the next iteration
 * over hooks crashes the host with
 * `undefined is not an object (evaluating 'z.config')` (and a sibling
 * `S.provider` for other hook iterations).
 *
 * Putting this helper in its own module keeps `index.ts` to exactly one
 * default export — the plugin function itself — and lets tests import
 * from this file directly.
 */

import { warn } from "./logger.js";
import { type ConfigureWarning, deliverConfigureWarnings } from "./notifications.js";

function isConfigureWarning(value: unknown): value is ConfigureWarning {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const warning = value as Record<string, unknown>;
  return (
    (warning.kind === "formatter_not_installed" ||
      warning.kind === "checker_not_installed" ||
      warning.kind === "lsp_binary_missing") &&
    typeof warning.hint === "string"
  );
}

function coerceConfigureWarnings(warnings: unknown[]): ConfigureWarning[] {
  return warnings.filter(isConfigureWarning);
}

export async function handleConfigureWarningsForSession(context: {
  projectRoot: string;
  sessionId?: string | null;
  client?: unknown;
  warnings: unknown[];
  fallbackClient: unknown;
  storageDir: string;
  pluginVersion: string;
}): Promise<void> {
  if (!context.sessionId) {
    warn(
      `[configure] deferred warnings for ${context.projectRoot} arrived without session_id; skipping notification`,
    );
    return;
  }
  const validWarnings = coerceConfigureWarnings(context.warnings);
  if (validWarnings.length === 0) return;
  await deliverConfigureWarnings(
    {
      client: context.client ?? context.fallbackClient,
      sessionId: context.sessionId,
      storageDir: context.storageDir,
      pluginVersion: context.pluginVersion,
      projectRoot: context.projectRoot,
    },
    validWarnings,
  );
}
