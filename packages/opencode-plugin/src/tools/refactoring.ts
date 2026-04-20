import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import { queryLspHints } from "../lsp.js";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";
import {
  askEditPermission,
  permissionDeniedResponse,
  resolveAbsolutePath,
  resolveRelativePattern,
  resolveRelativePatterns,
  workspacePattern,
} from "./permissions.js";

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
        "- 'move': Move a top-level symbol to another file, updating all imports workspace-wide. Requires 'symbol', 'destination'. Creates a checkpoint before mutating. Only works on top-level exports (not nested functions or class methods).\n" +
        "   Note: This moves code symbols between files. To rename/move an entire file, use aft_move instead.\n" +
        "- 'extract': Extract a line range into a new function with auto-detected parameters. Requires 'name', 'startLine', 'endLine' (1-based, both inclusive). Supports TS/JS/TSX and Python.\n" +
        "- 'inline': Replace a function call with the function's body, substituting args for params. Requires 'symbol', 'callSiteLine' (1-based). Validates single-return constraint.\n\n" +
        "Each op requires specific parameters — see parameter descriptions for requirements.\n\n" +
        "All ops need 'filePath'. Use dryRun to preview before applying.\n\n" +
        "Returns: move dry-run { ok, dry_run, diffs }; move apply { ok, files_modified, consumers_updated, checkpoint_name, results }. extract returns { file, name, parameters, return_type, syntax_valid, formatted, ... }. inline returns { file, symbol, call_context, substitutions, conflicts, syntax_valid, formatted, ... }.",
      // Parameters are Zod-optional because different ops need different subsets.
      // Runtime guards below validate per-op requirements and give clear errors.
      args: {
        op: z.enum(["move", "extract", "inline"]).describe("Refactoring operation"),
        filePath: z
          .string()
          .describe("Path to the source file (absolute or relative to project root)"),
        symbol: z
          .string()
          .optional()
          .describe("Symbol name — required for 'move' and 'inline' ops"),
        // move
        destination: z.string().optional().describe("Target file path — required for 'move' op"),
        // scope disambiguates overloaded top-level names, NOT nested symbols.
        // "Only works on top-level exports" in the description is correct — scope selects
        // among multiple top-level symbols that share a name, not class methods.
        scope: z
          .string()
          .optional()
          .describe(
            "Disambiguation scope for 'move' op — when multiple top-level symbols share the same name, specify the containing scope to disambiguate (e.g. 'MyClass'). Does NOT enable access to nested symbols or class methods.",
          ),
        // extract
        name: z.string().optional().describe("New function name — required for 'extract' op"),
        startLine: z.number().optional().describe("1-based start line — required for 'extract' op"),
        // endLine is inclusive from the agent's perspective; the execute function adds +1
        // because the Rust backend expects exclusive end. This is intentional — do not document.
        endLine: z
          .number()
          .optional()
          .describe("1-based end line (inclusive) — required for 'extract' op"),
        // inline
        callSiteLine: z
          .number()
          .optional()
          .describe("1-based call site line — required for 'inline' op"),
        // common
        dryRun: z
          .boolean()
          .optional()
          .describe("Preview changes as diff without modifying files (default: false)"),
      },
      execute: async (args, context): Promise<string> => {
        const op = args.op as string;

        if ((op === "move" || op === "inline") && typeof args.symbol !== "string") {
          throw new Error(`'symbol' is required for '${op}' op`);
        }
        if (op === "move" && typeof args.destination !== "string") {
          throw new Error("'destination' is required for 'move' op");
        }
        if (op === "extract") {
          if (typeof args.name !== "string") throw new Error("'name' is required for 'extract' op");
          if (args.startLine === undefined)
            throw new Error("'startLine' is required for 'extract' op");
          if (args.endLine === undefined) throw new Error("'endLine' is required for 'extract' op");
        }
        if (op === "inline" && args.callSiteLine === undefined) {
          throw new Error("'callSiteLine' is required for 'inline' op");
        }

        const filePath = resolveAbsolutePath(context, args.filePath as string);
        const patterns =
          op === "move"
            ? resolveRelativePatterns(context, [
                workspacePattern(context),
                args.filePath as string,
                ...(typeof args.destination === "string" ? [args.destination] : []),
              ])
            : [resolveRelativePattern(context, args.filePath as string)];
        const metadata = patterns.length === 1 ? { filepath: filePath } : {};
        const permissionError = await askEditPermission(context, patterns, metadata);
        if (permissionError) return permissionDeniedResponse(permissionError);

        const commandMap: Record<string, string> = {
          move: "move_symbol",
          extract: "extract_function",
          inline: "inline_symbol",
        };
        const params: Record<string, unknown> = { file: args.filePath };
        if (args.dryRun !== undefined) params.dry_run = args.dryRun;

        switch (op) {
          case "move":
            params.symbol = args.symbol;
            params.destination = args.destination;
            if (args.scope !== undefined) params.scope = args.scope;
            break;
          case "extract":
            params.name = args.name;
            params.start_line = Number(args.startLine);
            params.end_line = Number(args.endLine) + 1; // Agent uses inclusive, Rust expects exclusive
            break;
          case "inline":
            params.symbol = args.symbol;
            params.call_site_line = Number(args.callSiteLine);
            break;
        }

        const hints = await queryLspHints(ctx.client, (args.symbol ?? args.name) as string);
        if (hints) params.lsp_hints = hints;

        const response = await callBridge(ctx, context, commandMap[op], params);
        if (response.success === false) {
          throw new Error((response.message as string) || `${op} failed`);
        }
        return JSON.stringify(response);
      },
    },
  };
}
