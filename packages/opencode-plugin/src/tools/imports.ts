import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

const z = tool.schema;

/**
 * Tool definitions for import management commands: add_import, remove_import, organize_imports.
 */
export function importTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_import: {
      description:
        "Language-aware import management. Supports TS, JS, TSX, Python, Rust, and Go.\n\n" +
        "Ops:\n" +
        "- 'add': Add an import. Auto-detects group (stdlib/external/internal), deduplicates. Requires 'module'. Optional 'names', 'default_import', 'type_only'.\n" +
        "- 'remove': Remove an import or a specific named import. Requires 'module'. Provide 'name' to remove a single named import; omit to remove the entire import.\n" +
        "- 'organize': Re-sort and re-group all imports by language convention, deduplicate. Requires only 'file'.\n\n" +
        "Parameters:\n" +
        "- op (enum, required): 'add' | 'remove' | 'organize'\n" +
        "- file (string, required): Path to the source file\n" +
        "- module (string, optional): Module path — required for 'add' and 'remove' (e.g. 'react', './utils', 'std::fmt')\n" +
        "- names (string[], optional): Named imports to add for 'add' op (e.g. ['useState', 'useEffect'])\n" +
        "- default_import (string, optional): Default import name for 'add' op (e.g. 'React')\n" +
        "- name (string, optional): Specific named import to remove for 'remove' op; omit to remove entire import\n" +
        "- type_only (boolean, optional): Import as type-only — TypeScript 'import type' syntax\n" +
        "- dry_run (boolean, optional): Preview changes without modifying the file\n" +
        "- validate (enum, optional): Validation level — 'syntax' (default) or 'full'\n\n" +
        "Returns: { formatted (string), validation_errors (string[]) }",
      args: {
        op: z.enum(["add", "remove", "organize"]).describe("Import operation"),
        file: z.string().describe("Path to the file"),
        module: z
          .string()
          .optional()
          .describe("Module path (required for add, remove — e.g. 'react', './utils', 'std::fmt')"),
        names: z
          .array(z.string())
          .optional()
          .describe("Named imports to add (e.g. ['useState', 'useEffect'])"),
        default_import: z.string().optional().describe("Default import name (e.g. 'React')"),
        type_only: z.boolean().optional().describe("Type-only import (TS only, default: false)"),
        name: z
          .string()
          .optional()
          .describe(
            "Specific named import to remove (for remove op; omit to remove entire import)",
          ),
        validate: z
          .enum(["syntax", "full"])
          .optional()
          .describe("Validation level: 'syntax' (default) or 'full'"),
        dry_run: z.boolean().optional().describe("Preview without modifying the file"),
      },
      execute: async (args, context): Promise<string> => {
        const bridge = ctx.pool.getBridge(context.directory);
        const op = args.op as string;
        const commandMap: Record<string, string> = {
          add: "add_import",
          remove: "remove_import",
          organize: "organize_imports",
        };
        const params: Record<string, unknown> = { file: args.file };
        if (args.module !== undefined) params.module = args.module;
        if (args.names !== undefined) params.names = args.names;
        if (args.default_import !== undefined) params.default_import = args.default_import;
        if (args.type_only !== undefined) params.type_only = args.type_only;
        if (args.name !== undefined) params.name = args.name;
        if (args.validate !== undefined) params.validate = args.validate;
        if (args.dry_run !== undefined) params.dry_run = args.dry_run;
        const response = await bridge.send(commandMap[op], params);
        return JSON.stringify(response);
      },
    },
  };
}
