/**
 * aft_safety — per-file undo, named checkpoints, restore, list, history.
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const SafetyParams = Type.Object({
  op: StringEnum(["undo", "history", "checkpoint", "restore", "list"] as const, {
    description: "Safety operation",
  }),
  filePath: Type.Optional(Type.String({ description: "File path (required for undo, history)" })),
  name: Type.Optional(
    Type.String({ description: "Checkpoint name (required for checkpoint, restore)" }),
  ),
  files: Type.Optional(
    Type.Array(Type.String(), {
      description: "Specific files for checkpoint (optional, defaults to all tracked)",
    }),
  ),
});

export function registerSafetyTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_safety",
    label: "safety",
    description:
      "File safety and recovery operations. Ops: `undo` (pop latest snapshot for a file — irreversible), `history` (list snapshots for a file), `checkpoint` (save named snapshot), `restore` (restore named checkpoint), `list` (list checkpoints). Per-file undo stack is capped at 20.",
    parameters: SafetyParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof SafetyParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      if ((params.op === "undo" || params.op === "history") && !params.filePath) {
        throw new Error(`op='${params.op}' requires 'filePath'`);
      }
      if ((params.op === "checkpoint" || params.op === "restore") && !params.name) {
        throw new Error(`op='${params.op}' requires 'name'`);
      }
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const commandMap: Record<string, string> = {
        undo: "undo",
        history: "edit_history",
        checkpoint: "checkpoint",
        restore: "restore_checkpoint",
        list: "list_checkpoints",
      };
      const req: Record<string, unknown> = {};
      if (params.filePath) req.file = params.filePath;
      if (params.name) req.name = params.name;
      if (params.files) req.files = params.files;
      const response = await callBridge(bridge, commandMap[params.op], req);
      return textResult(JSON.stringify(response, null, 2));
    },
  });
}
