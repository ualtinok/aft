import type { ToolDefinition } from "@opencode-ai/plugin";
import { tool } from "@opencode-ai/plugin";
import type { PluginContext } from "../types.js";
import { callBridge } from "./_shared.js";
import {
  askEditPermission,
  permissionDeniedResponse,
  resolveAbsolutePath,
  resolveRelativePattern,
  workspacePattern,
} from "./permissions.js";

const z = tool.schema;

/**
 * Tool definitions for safety & recovery commands: undo, edit_history,
 * checkpoint, restore_checkpoint, list_checkpoints.
 */
export function safetyTools(ctx: PluginContext): Record<string, ToolDefinition> {
  return {
    aft_safety: {
      description:
        "File safety and recovery operations.\n\n" +
        "Per-file undo stack is capped at 20 entries (oldest evicted).\n\n" +
        "Ops:\n" +
        "- 'undo': Undo the last edit to a file. Requires 'filePath'. Note: pops from the undo stack (irreversible, no redo). Use 'history' to inspect before undoing.\n" +
        "- 'history': List all edit snapshots for a file. Requires 'filePath'.\n" +
        "- 'checkpoint': Save a named snapshot of tracked files. Requires 'name'. Optional 'files' to snapshot specific files only.\n" +
        "- 'restore': Restore files to a previously saved checkpoint. Requires 'name'.\n" +
        "- 'list': List all available named checkpoints. No extra params needed.\n\n" +
        "Each op requires specific parameters — see parameter descriptions for requirements.\n\n" +
        "Use checkpoint before risky multi-file changes. Use undo for quick single-file rollback.\n\n" +
        "Returns: undo { path, backup_id }. history { file, entries }. checkpoint { ok, name }. restore { ok, name }. list { checkpoints }.",
      // Parameters are Zod-optional because different ops need different subsets.
      // Runtime guards below validate per-op requirements and give clear errors.
      args: {
        op: z
          .enum(["undo", "history", "checkpoint", "restore", "list"])
          .describe("Safety operation"),
        filePath: z
          .string()
          .optional()
          .describe("File path (required for undo, history). Absolute or relative to project root"),
        name: z.string().optional().describe("Checkpoint name (required for checkpoint, restore)"),
        files: z
          .array(z.string())
          .optional()
          .describe(
            "Specific files to include in checkpoint (optional, defaults to all tracked files)",
          ),
      },
      execute: async (args, context): Promise<string> => {
        const op = args.op as string;

        if ((op === "undo" || op === "history") && typeof args.filePath !== "string") {
          throw new Error(`'filePath' is required for '${op}' op`);
        }
        if ((op === "checkpoint" || op === "restore") && typeof args.name !== "string") {
          throw new Error(`'name' is required for '${op}' op`);
        }

        if (op === "undo" && typeof args.filePath === "string") {
          const filePath = resolveAbsolutePath(context, args.filePath);
          const permissionError = await askEditPermission(
            context,
            [resolveRelativePattern(context, args.filePath)],
            { filepath: filePath },
          );
          if (permissionError) return permissionDeniedResponse(permissionError);
        }

        if (op === "restore") {
          const permissionError = await askEditPermission(context, [workspacePattern(context)], {
            checkpoint: args.name,
          });
          if (permissionError) return permissionDeniedResponse(permissionError);
        }

        const commandMap: Record<string, string> = {
          undo: "undo",
          history: "edit_history",
          checkpoint: "checkpoint",
          restore: "restore_checkpoint",
          list: "list_checkpoints",
        };
        const params: Record<string, unknown> = {};
        if (args.name !== undefined) params.name = args.name;
        if (op === "checkpoint") {
          // For checkpoint, Rust only knows `files`. If the agent passes
          // `filePath` (a reasonable mistake — the tool schema exposes both),
          // auto-promote it into a single-entry `files` list rather than
          // silently dropping it and falling back to the whole tracked-file
          // set.
          if (args.files !== undefined) {
            params.files = args.files;
          } else if (args.filePath !== undefined) {
            params.files = [args.filePath];
          }
        } else {
          // undo / history / restore / list all take `file` as-is.
          if (args.filePath !== undefined) params.file = args.filePath;
          if (args.files !== undefined) params.files = args.files;
        }
        const response = await callBridge(ctx, context, commandMap[op], params);
        if (response.success === false) {
          throw new Error((response.message as string) || `${op} failed`);
        }
        return JSON.stringify(response);
      },
    },
  };
}
