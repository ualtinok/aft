import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for import management commands: add_import, remove_import, organize_imports.
 */
export function importTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    add_import: {
      description:
        "Add an import statement to a file. Automatically detects the correct group (stdlib/external/internal), inserts alphabetically within the group, and deduplicates. Supports TS, JS, TSX, Python, Rust, and Go. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the file to add the import to"),
        module: z.string().describe("Module path (e.g. 'react', './utils', 'std::fmt')"),
        names: z
          .array(z.string())
          .optional()
          .describe("Named imports to add (e.g. ['useState', 'useEffect'])"),
        default_import: z
          .string()
          .optional()
          .describe("Default import name (e.g. 'React' for `import React from 'react'`)"),
        type_only: z
          .boolean()
          .optional()
          .describe("Whether this is a type-only import (default: false)"),
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
          module: args.module,
        };
        if (args.names !== undefined) params.names = args.names;
        if (args.default_import !== undefined) params.default_import = args.default_import;
        if (args.type_only !== undefined) params.type_only = args.type_only;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("add_import", params);
        return JSON.stringify(response);
      },
    },

    remove_import: {
      description:
        "Remove an import statement from a file, or remove a specific name from a multi-name import. If no name is specified, removes the entire import for the given module. Returns import_not_found error if the module is not imported. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the file to remove the import from"),
        module: z.string().describe("Module path to match (e.g. 'react', './utils')"),
        name: z
          .string()
          .optional()
          .describe("Specific named import to remove; if omitted, removes the entire import statement"),
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
          module: args.module,
        };
        if (args.name !== undefined) params.name = args.name;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("remove_import", params);
        return JSON.stringify(response);
      },
    },

    organize_imports: {
      description:
        "Organize all imports in a file: re-group by language convention (stdlib → external → internal), sort alphabetically within groups, deduplicate, and insert blank lines between groups. For Rust, merges common-prefix use declarations into use trees. Response includes `formatted`, `format_skipped_reason`, `validation_errors`, `validate_skipped_reason`.",
      args: {
        file: z.string().describe("Path to the file whose imports should be organized"),
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
        };
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send("organize_imports", params);
        return JSON.stringify(response);
      },
    },
  };
}
