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
      "Returns: { diagnostics: Array<{ file, line, column, end_line, end_column, severity, message, code }>, total: number, files_with_errors: number }.",
    args: {
      file: z.string().optional(),
      directory: z.string().optional(),
      severity: z.enum(["error", "warning", "information", "hint", "all"]).optional(),
      waitMs: z.number().optional(),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory);
      const params: Record<string, unknown> = {};
      if (args.file !== undefined) params.file = args.file;
      if (args.directory !== undefined) params.directory = args.directory;
      if (args.severity !== undefined) params.severity = args.severity;
      if (args.waitMs !== undefined) params.wait_ms = args.waitMs;
      const result = await bridge.send("lsp_diagnostics", params);
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
