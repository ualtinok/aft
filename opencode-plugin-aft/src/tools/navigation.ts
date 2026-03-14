import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for navigation commands: configure, call_tree, callers, and trace_to.
 */
export function navigationTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    aft_configure: {
      description:
        "Configure the AFT binary with the project root directory. Must be called before using call_tree or callers. Sets the worktree scope for call graph analysis.",
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

    aft_callers: {
      description:
        "Find all callers of a symbol across the project. Returns call sites grouped by file, showing which functions call the target symbol. Scans all project files and resolves cross-file edges via import chains. Supports recursive depth expansion (callers of callers). Use after aft_configure.",
      args: {
        file: z.string().describe("Path to the source file containing the target symbol (relative to project root or absolute)"),
        symbol: z.string().describe("Name of the symbol to find callers for"),
        depth: z
          .number()
          .optional()
          .describe("Recursive depth: 1 = direct callers only, 2+ = callers of callers (default: 1)"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
        };
        if (args.depth !== undefined) params.depth = args.depth;
        const response = await bridge.send("callers", params);
        return JSON.stringify(response);
      },
    },

    aft_trace_to: {
      description:
        "Trace backward from a symbol to all entry points (exported functions, main/init, test functions). Returns complete paths rendered top-down from entry point to target. Use to understand how a deeply-nested function is reached from public API surfaces. Response includes diagnostic fields: total_paths, entry_points_found, max_depth_reached, truncated_paths. Use after aft_configure.",
      args: {
        file: z.string().describe("Path to the source file containing the target symbol (relative to project root or absolute)"),
        symbol: z.string().describe("Name of the symbol to trace to entry points"),
        depth: z
          .number()
          .optional()
          .describe("Maximum backward traversal depth (default: 10)"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
        };
        if (args.depth !== undefined) params.depth = args.depth;
        const response = await bridge.send("trace_to", params);
        return JSON.stringify(response);
      },
    },
  };
}
