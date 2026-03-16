import type { ToolDefinition } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";

/**
 * Editing tools (aft_edit) have been merged into the hoisted `edit` tool.
 * This file is kept for future editing-specific tools that don't overlap.
 */
export function editingTools(_ctx: PluginContext): Record<string, ToolDefinition> {
  return {};
}
