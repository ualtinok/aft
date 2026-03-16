import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

const z = tool.schema;

/**
 * Tool definitions for code reading commands: outline only.
 * The zoom/read functionality has been merged into the hoisted `read` tool.
 */
export function readingTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_outline: {
      description:
        "Get a structural outline of a source file — lists all top-level symbols with their kind, name, line range, and visibility. Use this to understand file structure before editing. " +
        "Supports single file (via 'file') or multiple files in one call (via 'files' array).\n" +
        "Each entry includes 'name', 'kind' (function/class/struct/heading/etc), 'range', 'signature', and 'members' (nested children like methods in classes or sub-headings in markdown).\n" +
        "For Markdown files (.md, .mdx): returns heading hierarchy — h1/h2/h3 as nested symbols with section ranges covering all content until the next same-level heading.\n\n" +
        "Parameters:\n" +
        "- file (string, optional): Path to a single file to outline (relative to project root or absolute)\n" +
        "- files (string[], optional): Array of file paths to outline in one call — returns per-file results\n\n" +
        "Provide either 'file' or 'files', not both. Use 'files' to batch multiple outlines in one tool call.",
      args: {
        file: z
          .string()
          .optional()
          .describe(
            "Path to a single source file to outline (relative to project root or absolute)",
          ),
        files: z
          .array(z.string())
          .optional()
          .describe("Array of file paths to outline in one call — returns per-file results"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        if (Array.isArray(args.files) && args.files.length > 0) {
          const response = await bridge.send("outline", { files: args.files });
          return JSON.stringify(response);
        }
        const response = await bridge.send("outline", { file: args.file });
        return JSON.stringify(response);
      },
    },
  };
}
