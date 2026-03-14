import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for code reading commands: outline and zoom.
 */
export function readingTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    outline: {
      description:
        "Get a structural outline of a source file — lists all top-level symbols with their kind, name, line range, and visibility. Use this to understand file structure before editing.",
      args: {
        file: z.string().describe("Path to the source file to outline (relative to project root or absolute)"),
      },
      execute: async (args): Promise<string> => {
        const response = await bridge.send("outline", { file: args.file });
        return JSON.stringify(response);
      },
    },

    zoom: {
      description:
        "Deep-inspect a single symbol — returns its full source, surrounding context lines, and call-graph annotations (calls_out, called_by). Use after outline to study a specific function or type.",
      args: {
        file: z.string().describe("Path to the source file containing the symbol"),
        symbol: z.string().describe("Name of the symbol to inspect"),
        context_lines: z
          .number()
          .optional()
          .describe("Number of lines of context to include above and below the symbol (default: 3)"),
        scope: z
          .string()
          .optional()
          .describe("Qualified scope to disambiguate symbols with the same name (e.g. 'ClassName.method')"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
        };
        if (args.context_lines !== undefined) params.context_lines = args.context_lines;
        if (args.scope !== undefined) params.scope = args.scope;
        const response = await bridge.send("zoom", params);
        return JSON.stringify(response);
      },
    },
  };
}
