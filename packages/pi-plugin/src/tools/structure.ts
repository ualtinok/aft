/**
 * aft_transform — scope-aware structural code transformations.
 * Ops: add_member, add_derive, wrap_try_catch, add_decorator, add_struct_tags.
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const TransformParams = Type.Object({
  op: StringEnum(
    ["add_member", "add_derive", "wrap_try_catch", "add_decorator", "add_struct_tags"] as const,
    { description: "Transformation operation" },
  ),
  filePath: Type.String({ description: "Path to the source file" }),
  container: Type.Optional(Type.String({ description: "Class/struct/impl name for add_member" })),
  code: Type.Optional(Type.String({ description: "Member code to insert (add_member)" })),
  target: Type.Optional(Type.String({ description: "Target symbol name" })),
  derives: Type.Optional(
    Type.Array(Type.String(), { description: "Derive macro names (add_derive)" }),
  ),
  catchBody: Type.Optional(
    Type.String({ description: "Catch block body (wrap_try_catch, default: 'throw error;')" }),
  ),
  decorator: Type.Optional(
    Type.String({ description: "Decorator text without @ (add_decorator)" }),
  ),
  field: Type.Optional(Type.String({ description: "Struct field name (add_struct_tags)" })),
  tag: Type.Optional(Type.String({ description: "Tag key (add_struct_tags)" })),
  value: Type.Optional(Type.String({ description: "Tag value (add_struct_tags)" })),
  position: Type.Optional(
    Type.String({
      description:
        "Position hint: 'first', 'last' (default), 'before:name', 'after:name' for add_member",
    }),
  ),
  dryRun: Type.Optional(Type.Boolean({ description: "Preview without modifying" })),
  validate: Type.Optional(
    StringEnum(["syntax", "full"] as const, {
      description: "Validation level (default: syntax)",
    }),
  ),
});

export function registerStructureTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_transform",
    label: "transform",
    description:
      "Scope-aware structural code transformations with correct indentation. See parameter descriptions for per-op requirements.",
    parameters: TransformParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof TransformParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      // Rust dispatch accepts the op name directly (add_member, add_derive, etc.)
      const req: Record<string, unknown> = { file: params.filePath };
      if (params.container !== undefined) req.scope = params.container;
      if (params.code !== undefined) req.code = params.code;
      if (params.target !== undefined) req.target = params.target;
      if (params.derives !== undefined) req.derives = params.derives;
      if (params.catchBody !== undefined) req.catch_body = params.catchBody;
      if (params.decorator !== undefined) req.decorator = params.decorator;
      if (params.field !== undefined) req.field = params.field;
      if (params.tag !== undefined) req.tag = params.tag;
      if (params.value !== undefined) req.value = params.value;
      if (params.position !== undefined) req.position = params.position;
      if (params.dryRun !== undefined) req.dry_run = params.dryRun;
      if (params.validate !== undefined) req.validate = params.validate;
      const response = await callBridge(bridge, params.op, req);
      return textResult(JSON.stringify(response, null, 2));
    },
  });
}
