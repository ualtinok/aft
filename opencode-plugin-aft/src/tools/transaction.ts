import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/** Schema for a single transaction operation. */
const transactionOperation = z.object({
  file: z.string().describe("Target file path"),
  command: z
    .enum(["write", "edit_match"])
    .describe('Operation type: "write" (full content) or "edit_match" (find/replace)'),
  content: z
    .string()
    .optional()
    .describe("Complete file content (required for write operations)"),
  match: z
    .string()
    .optional()
    .describe("Text pattern to find (required for edit_match operations)"),
  replacement: z
    .string()
    .optional()
    .describe("Replacement text (required for edit_match operations)"),
});

/**
 * Tool definition for the transaction command: multi-file atomic edits with rollback.
 */
export function transactionTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    transaction: {
      description:
        "Apply edits to multiple files atomically. If any edit fails or produces invalid syntax, all files are rolled back to their pre-transaction state. Supports write (full content) and edit_match (find/replace) operations. On success: `ok`, `files_modified`, `results` array. On failure: `failed_operation` index, `rolled_back` array with per-file `{ file, action }`. On dry-run: `dry_run`, `diffs` array with per-file `{ file, diff, syntax_valid }`.",
      args: {
        operations: z
          .array(transactionOperation)
          .describe("Array of file operations to apply atomically"),
        dry_run: z
          .boolean()
          .optional()
          .describe("Preview the transaction as per-file unified diffs without modifying any files"),
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default, tree-sitter only) or 'full' (invoke project type checker)"),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = {
          operations: args.operations,
        };
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        if (args.validate !== undefined) params.validate = args.validate;
        const response = await bridge.send("transaction", params);
        return JSON.stringify(response);
      },
    },
  };
}
