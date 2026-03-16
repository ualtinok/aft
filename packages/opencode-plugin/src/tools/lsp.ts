import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

const z = tool.schema;
/**
 * Tool definitions for LSP commands: diagnostics, hover, goto_definition,
 * find_references, prepare_rename, rename.
 */
export function lspTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const diagnosticsTool: ToolDefinition = {
    description:
      "Get errors, warnings, hints from language server. " +
      "Returns diagnostics from LSP servers (typescript-language-server, pyright, rust-analyzer, gopls). " +
      "Lazily spawns the appropriate server on first use.\n\n" +
      "Parameters:\n" +
      "- file (string, optional): Path to file to get diagnostics for.\n" +
      "- directory (string, optional): Path to directory to get diagnostics for all files under it.\n" +
      "- severity (enum, optional): Filter by severity — 'error', 'warning', 'information', 'hint', 'all' (default: 'all').\n" +
      "- wait_ms (number, optional): Wait N ms for fresh diagnostics before returning (max 10000, default: 0). Use after edits to let the server re-analyze.\n\n" +
      "Returns: Array of { file, line, column, end_line, end_column, severity, message, code }.",
    args: {
      file: z.string().optional(),
      directory: z.string().optional(),
      severity: z.enum(["error", "warning", "information", "hint", "all"]).optional(),
      wait_ms: z.number().optional(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const result = await bridge.send("lsp_diagnostics", args);
      return JSON.stringify(result);
    },
  };

  // When hoisting: register as lsp_diagnostics (override oh-my-opencode's)
  // When not hoisting: register as aft_lsp_diagnostics
  const hoisting = ctx.config.hoist_builtin_tools !== false;
  return {
    [hoisting ? "lsp_diagnostics" : "aft_lsp_diagnostics"]: diagnosticsTool,
  };
}
