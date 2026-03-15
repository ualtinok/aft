import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

const z = tool.schema;
/**
 * Tool definitions for LSP commands: diagnostics, hover, goto_definition,
 * find_references, prepare_rename, rename.
 */
export function lspTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_lsp_diagnostics: {
      description:
        "Get errors, warnings, hints from language server. " +
        "Returns diagnostics from LSP servers (typescript-language-server, pyright, rust-analyzer, gopls). " +
        "Lazily spawns the appropriate server on first use. " +
        "For files, provide 'file' path. For directories, provide 'directory' path. " +
        "Use 'severity' to filter (error/warning/information/hint/all). " +
        "Use 'wait_ms' (default 0, max 10000) to wait for fresh diagnostics after an edit.",
      args: {
        file: z.string().optional().describe("Absolute path to file to get diagnostics for"),
        directory: z
          .string()
          .optional()
          .describe("Absolute path to directory to get diagnostics for all files under it"),
        severity: z
          .enum(["error", "warning", "information", "hint", "all"])
          .optional()
          .describe("Filter by severity level (default: all)"),
        wait_ms: z
          .number()
          .optional()
          .describe(
            "Wait N milliseconds for fresh diagnostics before returning (max 10000, default 0)",
          ),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const result = await bridge.send("lsp_diagnostics", args);
        return JSON.stringify(result);
      },
    },

    aft_lsp_hover: {
      description:
        "Get type information and documentation for a symbol at a position. " +
        "Returns hover content (type signatures, JSDoc, docstrings) from the language server. " +
        "Line and character are 1-based.",
      args: {
        file: z.string().describe("Absolute path to the file"),
        line: z.number().describe("1-based line number"),
        character: z.number().describe("1-based column number"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const result = await bridge.send("lsp_hover", args);
        return JSON.stringify(result);
      },
    },

    aft_lsp_goto_definition: {
      description:
        "Jump to symbol definition. Find WHERE something is defined. " +
        "Returns definition location(s) with file path and 1-based line/column. " +
        "Works across files — follows imports, type references, function calls.",
      args: {
        file: z.string().describe("Absolute path to the file"),
        line: z.number().describe("1-based line number"),
        character: z.number().describe("1-based column number"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const result = await bridge.send("lsp_goto_definition", args);
        return JSON.stringify(result);
      },
    },

    aft_lsp_find_references: {
      description:
        "Find ALL usages/references of a symbol across the entire workspace. " +
        "Returns all locations where the symbol is used, with 1-based line/column. " +
        "Use before renaming or changing a function's signature to understand impact.",
      args: {
        file: z.string().describe("Absolute path to the file"),
        line: z.number().describe("1-based line number"),
        character: z.number().describe("1-based column number"),
        include_declaration: z
          .boolean()
          .optional()
          .describe("Include the declaration itself (default: true)"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const result = await bridge.send("lsp_find_references", args);
        return JSON.stringify(result);
      },
    },

    aft_lsp_prepare_rename: {
      description:
        "Check if rename is valid at a position. Use BEFORE aft_lsp_rename. " +
        "Returns whether the symbol can be renamed, the current name (placeholder), " +
        "and the range of the symbol. Line and character are 1-based.",
      args: {
        file: z.string().describe("Absolute path to the file"),
        line: z.number().describe("1-based line number"),
        character: z.number().describe("1-based column number"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const result = await bridge.send("lsp_prepare_rename", args);
        return JSON.stringify(result);
      },
    },

    aft_lsp_rename: {
      description:
        "Rename symbol across entire workspace via LSP. " +
        "APPLIES changes to all files atomically with backup and rollback on failure. " +
        "Always call aft_lsp_prepare_rename first to verify the rename is valid. " +
        "Line and character are 1-based.",
      args: {
        file: z.string().describe("Absolute path to the file"),
        line: z.number().describe("1-based line number"),
        character: z.number().describe("1-based column number"),
        new_name: z.string().describe("New name for the symbol"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const result = await bridge.send("lsp_rename", args);
        return JSON.stringify(result);
      },
    },
  };
}
