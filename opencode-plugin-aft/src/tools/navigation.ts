import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for navigation commands: configure and call_tree.
 */
export function navigationTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    aft_configure: {
      description:
        "Configure the AFT binary with the project root directory. Must be called before using call_tree. Sets the worktree scope for call graph analysis.",
      args: {
        project_root: z.string().describe("Absolute path to the project root directory"),
      },
      execute: async (args): Promise<string> => {
        const response = await bridge.send("configure", { project_root: args.project_root });
        return JSON.stringify(response);
      },
    },

    aft_call_tree: {
      description:
        "Get a forward call tree starting from a symbol. Returns a nested tree showing what functions a symbol calls, resolved across files using import chains. Each node includes file path, line number, signature, and whether the edge was resolved. Use after aft_configure.",
      args: {
        file: z.string().describe("Path to the source file containing the symbol (relative to project root or absolute)"),
        symbol: z.string().describe("Name of the symbol to trace calls from"),
        depth: z
          .number()
          .optional()
          .describe("Maximum depth of the call tree (default: 5)"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
        };
        if (args.depth !== undefined) params.depth = args.depth;
        const response = await bridge.send("call_tree", params);
        return JSON.stringify(response);
      },
    },
  };
}
