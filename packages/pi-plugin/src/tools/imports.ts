/**
 * aft_import — language-aware import add/remove/organize.
 * Supports TS, JS, TSX, Python, Rust, Go.
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const ImportParams = Type.Object({
  op: StringEnum(["add", "remove", "organize"] as const, { description: "Import operation" }),
  filePath: Type.String({ description: "Path to the file" }),
  module: Type.Optional(
    Type.String({ description: "Module path (required for add/remove), e.g. 'react', './utils'" }),
  ),
  names: Type.Optional(
    Type.Array(Type.String(), { description: "Named imports to add, e.g. ['useState']" }),
  ),
  defaultImport: Type.Optional(Type.String({ description: "Default import name (e.g. 'React')" })),
  removeName: Type.Optional(
    Type.String({ description: "Named import to remove; omit to remove entire import" }),
  ),
  typeOnly: Type.Optional(Type.Boolean({ description: "Type-only import (TS only)" })),
  dryRun: Type.Optional(Type.Boolean({ description: "Preview without writing" })),
  validate: Type.Optional(
    StringEnum(["syntax", "full"] as const, {
      description: "Post-edit validation level (default: syntax)",
    }),
  ),
});

export function registerImportTools(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_import",
    label: "import",
    description:
      "Language-aware import management. Supports TS, JS, TSX, Python, Rust, Go. Ops: `add` (auto-groups stdlib/external/internal, deduplicates), `remove` (pass `removeName` for single name or omit to remove entire import), `organize` (re-sort + deduplicate).",
    parameters: ImportParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof ImportParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      if ((params.op === "add" || params.op === "remove") && !params.module) {
        throw new Error(`op='${params.op}' requires 'module'`);
      }
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const commandMap: Record<string, string> = {
        add: "add_import",
        remove: "remove_import",
        organize: "organize_imports",
      };
      const req: Record<string, unknown> = { file: params.filePath };
      if (params.module !== undefined) req.module = params.module;
      if (params.names !== undefined) req.names = params.names;
      if (params.defaultImport !== undefined) req.default_import = params.defaultImport;
      if (params.removeName !== undefined) req.name = params.removeName;
      if (params.typeOnly !== undefined) req.type_only = params.typeOnly;
      if (params.dryRun !== undefined) req.dry_run = params.dryRun;
      if (params.validate !== undefined) req.validate = params.validate;

      const response = await callBridge(bridge, commandMap[params.op], req);
      return textResult(JSON.stringify(response, null, 2));
    },
  });
}
