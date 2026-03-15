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
import type { PluginContext } from "./types.js";

/**
 * AFT (Agent File Toolkit) plugin for OpenCode.
 *
 * Config is loaded from two levels (project overrides user):
 * - User:    ~/.config/opencode/aft.jsonc (or .json)
 * - Project: <project>/.opencode/aft.jsonc (or .json)
 *
 * Tools organized into groups:
 * - Reading: aft_outline, aft_zoom
 * - Editing: aft_edit
 * - Safety: aft_safety
 * - Imports: aft_import
 * - Structure: aft_transform
 * - Navigation: aft_navigate
 * - Refactoring: aft_refactor
 * - AST Search: aft_ast_search, aft_ast_replace\n * - LSP: aft_lsp_diagnostics, aft_lsp_hover, aft_lsp_goto_definition, aft_lsp_find_references, aft_lsp_prepare_rename, aft_lsp_rename
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
