import type { ToolDefinition } from "@opencode-ai/plugin";
import { z } from "zod";
import type { PluginContext } from "../types.js";

type ToolArg = ToolDefinition["args"][string];

function arg(schema: unknown): ToolArg {
  return schema as ToolArg;
}

export function semanticTools(ctx: PluginContext): Record<string, ToolDefinition> {
  const searchTool: ToolDefinition = {
    description:
      "Search code by meaning using semantic similarity. Use when you don't know the exact name or text — describe what you're looking for in natural language and get the most relevant symbols, functions, and types.",
    args: {
      query: arg(z.string().describe("Natural language search query")),
      topK: arg(z.number().optional().describe("Number of results (default: 10)")),
    },
    execute: async (args, context): Promise<string> => {
      const bridge = ctx.pool.getBridge(context.directory, context.sessionID);
      const response = await bridge.send("semantic_search", {
        query: args.query,
        top_k: args.topK ?? 10,
      });

      if (response.success === false) {
        throw new Error((response.message as string) || "semantic_search failed");
      }

      if (typeof response.text === "string") {
        return response.text;
      }

      return JSON.stringify(response);
    },
  };

  return {
    aft_search: searchTool,
  };
}
