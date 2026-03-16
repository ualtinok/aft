import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

const z = tool.schema;

/**
 * Tool definitions for navigation commands: configure, call_tree, callers, trace_to, impact, and trace_data.
 */
export function navigationTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_navigate: {
      description:
        "Navigate code structure across files using call graph analysis.\n\n" +
        "Modes:\n" +
        "- 'call_tree': See what a function calls (forward traversal). Use to understand dependencies before modifying a function.\n" +
        "- 'callers': Find all call sites of a symbol. Use before renaming or changing a function's signature.\n" +
        "- 'trace_to': Trace how execution reaches a function from entry points (routes, exports, main). Use to understand context around deeply-nested code.\n" +
        "- 'impact': Analyze what breaks if a symbol changes. Returns affected callers with signatures and entry point status.\n" +
        "- 'trace_data': Follow a value through variable assignments and function parameters across files. Requires 'expression' arg.\n\n" +
        "Parameters:\n" +
        "- mode (enum, required): 'call_tree' | 'callers' | 'trace_to' | 'impact' | 'trace_data'\n" +
        "- file (string, required): Path to the source file containing the symbol\n" +
        "- symbol (string, required): Name of the symbol to analyze\n" +
        "- depth (number, required): Max traversal depth. Recommended: call_tree=3, callers=2, trace_to=10, impact=3, trace_data=5\n" +
        "- expression (string, optional): The expression to follow through assignments — required for 'trace_data' mode",
      args: {
        mode: z
          .enum(["call_tree", "callers", "trace_to", "impact", "trace_data"])
          .describe("Navigation mode"),
        file: z.string().describe("Path to the source file containing the symbol"),
        symbol: z.string().describe("Name of the symbol to analyze"),
        depth: z
          .number()
          .describe(
            "Max traversal depth (default: call_tree=5, callers=1, trace_to=10, impact=5, trace_data=5)",
          ),
        expression: z
          .string()
          .optional()
          .describe("Expression to track through data flow (required for trace_data mode)"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
        };
        if (args.depth !== undefined) params.depth = Number(args.depth);
        if (args.expression !== undefined) params.expression = args.expression;
        const response = await bridge.send(args.mode as string, params);
        return JSON.stringify(response);
      },
    },
  };
}
