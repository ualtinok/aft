import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import { queryLspHints } from "../lsp.js";
import type { PluginContext } from "../types.js";

const z = tool.schema;

/**
 * Tool definitions for refactoring commands: move_symbol, extract_function, inline_symbol.
 */
export function refactoringTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_refactor: {
      description:
        "Workspace-wide refactoring operations that update imports and references across files.\n\n" +
        "Ops:\n" +
        "- 'move': Move a symbol to another file, updating all imports workspace-wide. Requires 'symbol', 'destination'. Creates a checkpoint before mutating.\n" +
        "- 'extract': Extract a line range into a new function with auto-detected parameters. Requires 'name', 'start_line', 'end_line' (all 1-based). Supports TS/JS/TSX and Python.\n" +
        "- 'inline': Replace a function call with the function's body, substituting args for params. Requires 'symbol', 'call_site_line' (1-based). Validates single-return constraint.\n\n" +
        "Parameters:\n" +
        "- op (enum, required): 'move' | 'extract' | 'inline'\n" +
        "- file (string, required): Path to the source file\n" +
        "- symbol (string, optional): Symbol name — required for 'move' (symbol to move) and 'inline' (function to inline)\n" +
        "- destination (string, optional): Target file path for 'move' op — created if it doesn't exist\n" +
        "- scope (string, optional): Disambiguation scope for 'move' when multiple symbols share the same name\n" +
        "- name (string, optional): New function name for 'extract' op\n" +
        "- start_line (number, optional): 1-based start line of the range to extract ('extract' op)\n" +
        "- end_line (number, optional): 1-based end line of the range to extract, exclusive ('extract' op)\n" +
        "- call_site_line (number, optional): 1-based line where the call expression is located ('inline' op)\n" +
        "- dry_run (boolean, optional): Preview changes as diff without modifying files\n\n" +
        "All ops need 'file'. Use dry_run to preview before applying.",
      args: {
        op: z.enum(["move", "extract", "inline"]).describe("Refactoring operation"),
        file: z.string().describe("Path to the source file"),
        symbol: z
          .string()
          .optional()
          .describe("Symbol name (move: symbol to move, inline: function to inline)"),
        // move
        destination: z
          .string()
          .optional()
          .describe("Destination file path (move op — will be created if needed)"),
        scope: z
          .string()
          .optional()
          .describe("Disambiguation scope when multiple symbols share the same name (move op)"),
        // extract
        name: z.string().optional().describe("Name for the new extracted function (extract op)"),
        start_line: z.number().describe("First line of range to extract, 1-based (extract op)"),
        end_line: z.number().describe("Last line of range, exclusive, 1-based (extract op)"),
        // inline
        call_site_line: z
          .number()
          .describe("Line where the call expression is located, 1-based (inline op)"),
        // common
        dry_run: z.boolean().optional().describe("Preview as diff without modifying files"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const op = args.op as string;
        const commandMap: Record<string, string> = {
          move: "move_symbol",
          extract: "extract_function",
          inline: "inline_symbol",
        };
        const params: Record<string, unknown> = { file: args.file };
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;

        switch (op) {
          case "move":
            params.symbol = args.symbol;
            params.destination = args.destination;
            if (args.scope !== undefined) params.scope = args.scope;
            break;
          case "extract":
            params.name = args.name;
            params.start_line = Number(args.start_line);
            params.end_line = Number(args.end_line);
            break;
          case "inline":
            params.symbol = args.symbol;
            params.call_site_line = Number(args.call_site_line);
            break;
        }

        const hints = await queryLspHints(ctx.client, (args.symbol ?? args.name) as string);
        if (hints) params.lsp_hints = hints;

        const response = await bridge.send(commandMap[op], params);
        return JSON.stringify(response);
      },
    },
  };
}
