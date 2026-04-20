import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";

const z = tool.schema;
/**
 * Tool definitions for LSP commands: diagnostics.
 */
export function lspTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const diagnosticsTool: ToolDefinition = {
    description:
      "Get errors, warnings, hints from a language server. " +
      "Returns: { diagnostics: Array<{ file, line, column, end_line, end_column, severity, message, code }>, total: number, files_with_errors: number }.",
    args: {
      filePath: z
        .string()
        .optional()
        .describe(
          "Path to file to get diagnostics for. Provide 'filePath' for a single file, 'directory' for all files under a path, or omit both for all tracked files",
        ),
      directory: z
        .string()
        .optional()
        .describe(
          "Path to directory to get diagnostics for all files under it. Mutually exclusive with 'filePath'",
        ),
      severity: z
        .enum(["error", "warning", "information", "hint", "all"])
        .optional()
        .describe(
          "Filter by severity — 'error', 'warning', 'information', 'hint', 'all' (default: 'all')",
        ),
      waitMs: z
        .number()
        .optional()
        .describe(
          "Wait N ms for fresh diagnostics before returning (max 10000, default: 0). Use after edits to let the server re-analyze.",
        ),
    },
    execute: async (args, context): Promise<string> => {
      const filePath = args.filePath || undefined; // treat empty string as absent
      const directory = args.directory || undefined;
      if (filePath !== undefined && directory !== undefined) {
        throw new Error(
          "'filePath' and 'directory' are mutually exclusive — provide one or neither",
        );
      }
      const params: Record<string, unknown> = {};
      if (filePath !== undefined) params.file = filePath;
      if (directory !== undefined) params.directory = directory;
      if (args.severity !== undefined) params.severity = args.severity;
      if (args.waitMs !== undefined) params.wait_ms = args.waitMs;
      const result = await callBridge(ctx, context, "lsp_diagnostics", params);
      if (result.success === false) {
        throw new Error((result.message as string) || "lsp_diagnostics failed");
      }
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
