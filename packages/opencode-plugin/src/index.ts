import { createRequire } from "node:module";
import type { Plugin } from "@opencode-ai/plugin";
import { loadAftConfig } from "./config.js";
import { ensureBinary } from "./downloader.js";
import { error, log, warn } from "./logger.js";
import { consumeToolMetadata } from "./metadata-store.js";
import { normalizeToolMap } from "./normalize-schemas.js";
import { BridgePool } from "./pool.js";
import { findBinary } from "./resolver.js";

/** Read the plugin's own version from package.json at build time. */
const PLUGIN_VERSION: string = (() => {
  try {
    const req = createRequire(import.meta.url);
    return (req("../package.json") as { version: string }).version;
  } catch {
    return "0.0.0";
  }
})();

import { astTools } from "./tools/ast.js";

import { aftPrefixedTools, hoistedTools } from "./tools/hoisted.js";
import { importTools } from "./tools/imports.js";
import { lspTools } from "./tools/lsp.js";
import { navigationTools } from "./tools/navigation.js";
import { readingTools } from "./tools/reading.js";
import { refactoringTools } from "./tools/refactoring.js";
import { safetyTools } from "./tools/safety.js";
import { structureTools } from "./tools/structure.js";
import type { PluginContext } from "./types.js";

/**
 * AFT (Agent File Toolkit) plugin for OpenCode.
 *
 * Config is loaded from two levels (project overrides user):
 * - User:    ~/.config/opencode/aft.jsonc (or .json)
 * - Project: <project>/.opencode/aft.jsonc (or .json)
 *
 * Tools organized into groups:
 * - Hoisted (default): read, write, edit, apply_patch, ast_grep_search, ast_grep_replace
 * - File ops: aft_delete, aft_move
 * - Reading: aft_outline
 * - Safety: aft_safety
 * - Imports: aft_import
 * - Structure: aft_transform
 * - Navigation: aft_navigate
 * - Refactoring: aft_refactor
 * - LSP: aft_lsp_diagnostics (inline diagnostics on edits are automatic)
 */
const plugin: Plugin = async (input) => {
  const binaryPath = await findBinary();

  // Load config: ~/.config/opencode/aft.jsonc → <project>/.opencode/aft.jsonc
  const aftConfig = loadAftConfig(input.directory);

  // Build config overrides for the Rust binary (strip undefined values)
  const configOverrides: Record<string, unknown> = {};
  if (aftConfig.format_on_edit !== undefined)
    configOverrides.format_on_edit = aftConfig.format_on_edit;
  if (aftConfig.validate_on_edit !== undefined)
    configOverrides.validate_on_edit = aftConfig.validate_on_edit;
  if (aftConfig.formatter !== undefined) configOverrides.formatter = aftConfig.formatter;
  if (aftConfig.checker !== undefined) configOverrides.checker = aftConfig.checker;

  const pool = new BridgePool(
    binaryPath,
    {
      minVersion: PLUGIN_VERSION,
      onVersionMismatch: (binaryVersion, minVersion) => {
        warn(
          `WARNING: aft binary v${binaryVersion} is older than plugin v${minVersion}. ` +
            "Some features may not work. Attempting to download a compatible binary...",
        );
        // Fire-and-forget: try to download matching version in background
        ensureBinary(`v${minVersion}`).then(
          (path) => {
            if (path) {
              log(`Downloaded compatible binary to ${path}. Restart OpenCode to use it.`);
            }
          },
          () => {
            error(
              `Auto-download failed. Install manually: cargo install agent-file-tools@${minVersion}`,
            );
          },
        );
      },
    },
    configOverrides,
  );
  const ctx: PluginContext = { pool, client: input.client, config: aftConfig };

  // Tool surface tiers:
  //   minimal:     aft_outline, aft_zoom, aft_safety
  //   recommended: minimal + hoisted + lsp_diagnostics + ast_grep_* + aft_import (default)
  //   all:         recommended + aft_navigate, aft_delete, aft_move, aft_transform, aft_refactor
  const surface = aftConfig.tool_surface ?? "recommended";

  // Tools only available in "all" tier
  const ALL_ONLY_TOOLS = new Set([
    "aft_navigate",
    "aft_delete",
    "aft_move",
    "aft_transform",
    "aft_refactor",
  ]);

  // Build full tool map
  const allTools = normalizeToolMap({
    // Hoisted tools: only in recommended+ (and when hoist_builtin_tools !== false)
    ...(surface !== "minimal" &&
      (aftConfig.hoist_builtin_tools !== false ? hoistedTools(ctx) : aftPrefixedTools(ctx))),
    ...readingTools(ctx),

    ...safetyTools(ctx),
    // aft_import: recommended+
    ...(surface !== "minimal" && importTools(ctx)),
    ...structureTools(ctx),
    ...navigationTools(ctx),
    // AST tools: recommended+
    ...(surface !== "minimal" && astTools(ctx)),
    ...refactoringTools(ctx),
    // LSP diagnostics: recommended+
    ...(surface !== "minimal" && lspTools(ctx)),
  });

  // Remove all-only tools when surface is minimal or recommended
  if (surface !== "all") {
    for (const name of ALL_ONLY_TOOLS) {
      if (name in allTools) {
        delete allTools[name];
      }
    }
  }

  // Filter disabled tools (user + project config union)
  const disabled = new Set(aftConfig.disabled_tools ?? []);
  if (disabled.size > 0) {
    for (const name of disabled) {
      if (name in allTools) {
        delete allTools[name];
      } else {
        warn(
          `disabled_tools: "${name}" not found — available: ${Object.keys(allTools).join(", ")}`,
        );
      }
    }
    log(`Disabled ${disabled.size} tool(s): ${[...disabled].join(", ")}`);
  }

  return {
    tool: allTools,
    // Restore metadata that fromPlugin() overwrites (opencode bug workaround)
    "tool.execute.after": async (
      input: { tool: string; sessionID: string; callID: string },
      output: { title: string; output: string; metadata: Record<string, unknown> } | undefined,
    ) => {
      if (!output) return;
      const stored = consumeToolMetadata(input.sessionID, input.callID);
      if (stored) {
        if (stored.title) output.title = stored.title;
        if (stored.metadata) output.metadata = { ...output.metadata, ...stored.metadata };
      }
    },
    dispose: () => pool.shutdown(),
  };
};

export default plugin;
