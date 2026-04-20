/**
 * aft_conflicts — one-call merge conflict inspection.
 */

import type { ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { Type } from "@sinclair/typebox";
import type { PluginContext } from "../types.js";
import { bridgeFor, callBridge, textResult } from "./_shared.js";

const ConflictsParams = Type.Object({});

export function registerConflictsTool(pi: ExtensionAPI, ctx: PluginContext): void {
  pi.registerTool({
    name: "aft_conflicts",
    label: "conflicts",
    description:
      "Show all git merge conflicts across the repository — returns line-numbered conflict regions with context for every conflicted file in a single call.",
    parameters: ConflictsParams,
    async execute(_toolCallId: string, _params, _signal, _onUpdate, extCtx) {
      const bridge = bridgeFor(ctx, extCtx.cwd);
      const response = await callBridge(bridge, "git_conflicts");
      return textResult((response.text as string | undefined) ?? JSON.stringify(response, null, 2));
    },
  });
}
