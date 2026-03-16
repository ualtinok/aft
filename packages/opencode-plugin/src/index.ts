import type { Plugin } from "@opencode-ai/plugin";
import { loadAftConfig } from "./config.js";
import { BridgePool } from "./pool.js";
import { findBinary } from "./resolver.js";
import { astTools } from "./tools/ast.js";
import { editingTools } from "./tools/editing.js";
import { importTools } from "./tools/imports.js";
import { lspTools } from "./tools/lsp.js";
import { navigationTools } from "./tools/navigation.js";
import { readingTools } from "./tools/reading.js";
import { refactoringTools } from "./tools/refactoring.js";
import { safetyTools } from "./tools/safety.js";
import { structureTools } from "./tools/structure.js";
import { hoistedTools, aftPrefixedTools } from "./tools/hoisted.js";
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

  const pool = new BridgePool(binaryPath, {}, configOverrides);
  const ctx: PluginContext = { pool, client: input.client, config: aftConfig };

  return {
    tool: {
      // When hoisting enabled (default): override opencode built-ins (read, write, edit, apply_patch)
      // When disabled: register with aft_ prefix (aft_read, aft_write, aft_edit, aft_apply_patch)
      ...(aftConfig.hoist_builtin_tools !== false ? hoistedTools(ctx) : aftPrefixedTools(ctx)),
      ...readingTools(ctx),
      ...editingTools(ctx),
      ...safetyTools(ctx),
      ...importTools(ctx),
      ...structureTools(ctx),
      ...navigationTools(ctx),
      ...astTools(ctx),
      ...refactoringTools(ctx),
      ...lspTools(ctx),
    },
    dispose: () => pool.shutdown(),
  };
};

export default plugin;
