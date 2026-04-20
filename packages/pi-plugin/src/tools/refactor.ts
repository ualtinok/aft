/**
 * aft_refactor — workspace-wide refactoring.
 * Ops: move (symbol across files), extract (lines → function), inline (call site).
 */

import { StringEnum } from "@mariozechner/pi-ai";
import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { type Static, Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const RefactorParams = Type.Object({
  op: StringEnum(["move", "extract", "inline"] as const, { description: "Refactoring operation" }),
  filePath: Type.String({ description: "Source file" }),
  symbol: Type.Optional(Type.String({ description: "Symbol name (for move, inline)" })),
  destination: Type.Optional(Type.String({ description: "Target file (for move)" })),
  scope: Type.Optional(Type.String({ description: "Disambiguation scope for move op" })),
  name: Type.Optional(Type.String({ description: "New function name (for extract)" })),
  startLine: Type.Optional(Type.Number({ description: "1-based start line (for extract)" })),
  endLine: Type.Optional(Type.Number({ description: "1-based end line, inclusive (for extract)" })),
  callSiteLine: Type.Optional(Type.Number({ description: "1-based call site line (for inline)" })),
  dryRun: Type.Optional(Type.Boolean({ description: "Preview as diff" })),
});

export function registerRefactorTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_refactor",
    label: "refactor",
    description:
      "Workspace-wide refactoring that updates imports and references across files. `move` relocates a top-level symbol (only top-level exports); `extract` pulls a line range into a new function; `inline` replaces a call site with the function body.",
    parameters: RefactorParams,
    async execute(
      _toolCallId: string,
      params: Static<typeof RefactorParams>,
      _signal,
      _onUpdate,
      extCtx,
    ) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const commandMap: Record<string, string> = {
        move: "move_symbol",
        extract: "extract_function",
        inline: "inline_symbol",
      };
      const req: Record<string, unknown> = { file: params.filePath };
      if (params.symbol !== undefined) req.symbol = params.symbol;
      if (params.destination !== undefined) req.destination = params.destination;
      if (params.scope !== undefined) req.scope = params.scope;
      if (params.name !== undefined) req.name = params.name;
      if (params.startLine !== undefined) req.start_line = params.startLine;
      // Agent uses inclusive end_line; Rust extract_function expects exclusive.
      if (params.endLine !== undefined) {
        req.end_line = params.op === "extract" ? params.endLine + 1 : params.endLine;
      }
      if (params.callSiteLine !== undefined) req.call_site_line = params.callSiteLine;
      if (params.dryRun !== undefined) req.dry_run = params.dryRun;
      const response = await callBridge(bridge, commandMap[params.op], req);
      return textResult(JSON.stringify(response, null, 2));
    },
  });
}
