import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for scope-aware structure commands:
 * add_member, add_derive, wrap_try_catch, add_decorator, add_struct_tags.
 */
export function structureTools(
  bridge: BinaryBridge,
): Record<string, ToolDefinition> {
  return {
    add_member: {
      description:
        "Insert a method, field, or function into a scope container (class, struct, impl block) with correct indentation. Supports TS/JS classes, Python classes, Rust structs/impl blocks, and Go structs. Position controls where the member is inserted relative to existing members. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the target file"),
        scope: z
          .string()
          .describe(
            "Name of the class, struct, or impl block to insert into",
          ),
        code: z.string().describe("The member code to insert"),
        position: z
          .enum(["first", "last"])
          .or(z.string())
          .optional()
          .describe(
            "Where to insert: 'first', 'last' (default), 'before:name', or 'after:name'",
          ),
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
          scope: args.scope,
          code: args.code,
        };
        if (args.position !== undefined) params.position = args.position;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("add_member", params);
        return JSON.stringify(response);
      },
    },

    add_derive: {
      description:
        "Add derive macros to a Rust struct or enum. Appends to an existing #[derive(...)] attribute or creates a new one. Deduplicates — already-present derives are skipped. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the Rust source file"),
        target: z.string().describe("Name of the struct or enum"),
        derives: z
          .array(z.string())
          .describe("Derive macro names to add (e.g. ['Clone', 'Debug'])"),
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
          target: args.target,
          derives: args.derives,
        };
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("add_derive", params);
        return JSON.stringify(response);
      },
    },

    wrap_try_catch: {
      description:
        "Wrap a TS/JS function or method body in a try/catch block, preserving indentation. The original body statements move inside the try block. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the TS/JS source file"),
        target: z.string().describe("Name of the function or method to wrap"),
        catch_body: z
          .string()
          .optional()
          .describe(
            "Code inside the catch block (default: 'throw error;')",
          ),
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
          target: args.target,
        };
        if (args.catch_body !== undefined) params.catch_body = args.catch_body;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("wrap_try_catch", params);
        return JSON.stringify(response);
      },
    },

    add_decorator: {
      description:
        "Insert a Python decorator onto a function or class. Handles both plain and already-decorated definitions. The decorator text should not include the @ prefix. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the Python source file"),
        target: z.string().describe("Name of the function or class"),
        decorator: z
          .string()
          .describe("Decorator text without the @ prefix (e.g. 'staticmethod')"),
        position: z
          .enum(["first", "last"])
          .optional()
          .describe(
            "Where among existing decorators: 'first' (default) or 'last'",
          ),
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
          target: args.target,
          decorator: args.decorator,
        };
        if (args.position !== undefined) params.position = args.position;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("add_decorator", params);
        return JSON.stringify(response);
      },
    },

    add_struct_tags: {
      description:
        "Add or update a Go struct field tag. Sets a key:\"value\" pair in the field's backtick-delimited tag string, creating or extending the tag as needed. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the Go source file"),
        target: z.string().describe("Name of the struct"),
        field: z.string().describe("Name of the struct field"),
        tag: z.string().describe("Tag key (e.g. 'json')"),
        value: z
          .string()
          .describe("Tag value (e.g. 'user_name,omitempty')"),
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
          target: args.target,
          field: args.field,
          tag: args.tag,
          value: args.value,
        };
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("add_struct_tags", params);
        return JSON.stringify(response);
      },
    },
  };
}
