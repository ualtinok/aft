import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/** Valid operations for edit_symbol. */
const editOperationEnum = z
  .enum(["replace", "delete", "insert_before", "insert_after"])
  .describe("The edit operation to perform on the symbol");

/** Schema for a single batch edit item — either match-replace or line-range. */
const batchEditItem = z.union([
  z.object({
    match: z.string().describe("Text pattern to find and replace"),
    replacement: z.string().describe("Replacement text"),
  }),
  z.object({
    line_start: z.number().describe("Start line number (1-indexed)"),
    line_end: z.number().describe("End line number (1-indexed, inclusive)"),
    content: z.string().describe("Content to replace the line range with"),
  }),
]);

/**
 * Tool definitions for code editing commands: write, edit_symbol, edit_match, batch.
 */
export function editingTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    write: {
      description:
        "Write content to a file, creating it if it doesn't exist. Backs up existing files automatically. Returns syntax validation result, backup ID for undo, and format/validation status. Response fields: `formatted` (bool), `format_skipped_reason` (string), `validation_errors` (array of {line, column, message, severity}), `validate_skipped_reason` (string).",
      args: {
        file: z.string().describe("Path to the file to write (relative to project root or absolute)"),
        content: z.string().describe("Complete file content to write"),
        create_dirs: z
          .boolean()
          .optional()
          .describe("Create parent directories if they don't exist (default: false)"),
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default, tree-sitter only) or 'full' (invoke project type checker)"),
        dry_run: z
          .boolean()
          .optional()
          .describe("Preview the edit as a unified diff without modifying the file"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          content: args.content,
        };
        if (args.create_dirs !== undefined) params.create_dirs = args.create_dirs;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("write", params);
        return JSON.stringify(response);
      },
    },

    edit_symbol: {
      description:
        "Edit a named symbol (function, class, type, etc.) by operation: replace its body, delete it, or insert content before/after it. Uses tree-sitter for precise symbol location. Returns ambiguous_symbol error with candidates if the name matches multiple symbols. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the file containing the symbol"),
        symbol: z.string().describe("Name of the symbol to edit"),
        operation: editOperationEnum,
        content: z
          .string()
          .optional()
          .describe("New content for replace, insert_before, or insert_after operations"),
        scope: z
          .string()
          .optional()
          .describe("Qualified scope to disambiguate symbols with the same name (e.g. 'ClassName.method')"),
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default, tree-sitter only) or 'full' (invoke project type checker)"),
        dry_run: z
          .boolean()
          .optional()
          .describe("Preview the edit as a unified diff without modifying the file"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          symbol: args.symbol,
          operation: args.operation,
        };
        if (args.content !== undefined) params.content = args.content;
        if (args.scope !== undefined) params.scope = args.scope;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("edit_symbol", params);
        return JSON.stringify(response);
      },
    },

    edit_match: {
      description:
        "Find and replace a text pattern in a file. If the pattern matches multiple locations, returns ambiguous_match with occurrence indices — resubmit with an occurrence number to target a specific match. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the file to edit"),
        match: z.string().describe("Exact text to find in the file"),
        replacement: z.string().describe("Text to replace the match with"),
        occurrence: z
          .number()
          .optional()
          .describe("Zero-based index of the specific occurrence to replace when multiple matches exist"),
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default, tree-sitter only) or 'full' (invoke project type checker)"),
        dry_run: z
          .boolean()
          .optional()
          .describe("Preview the edit as a unified diff without modifying the file"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          match: args.match,
          replacement: args.replacement,
        };
        if (args.occurrence !== undefined) params.occurrence = args.occurrence;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("edit_match", params);
        return JSON.stringify(response);
      },
    },

    batch: {
      description:
        "Apply multiple edits to a single file atomically. If any edit fails, all changes are rolled back. Each edit can be a match-replace or a line-range replacement. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the file to edit"),
        edits: z
          .array(batchEditItem)
          .describe("Array of edit operations to apply atomically"),
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default, tree-sitter only) or 'full' (invoke project type checker)"),
        dry_run: z
          .boolean()
          .optional()
          .describe("Preview the edit as a unified diff without modifying the file"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          file: args.file,
          edits: args.edits,
        };
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("batch", params);
        return JSON.stringify(response);
      },
    },
  };
}
