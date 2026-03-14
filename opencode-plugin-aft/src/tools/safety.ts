import { tool } from "@opencode-ai/plugin";
import type { ToolDefinition } from "@opencode-ai/plugin";
import type { BinaryBridge } from "../bridge.js";

const z = tool.schema;

/**
 * Tool definitions for safety & recovery commands: undo, edit_history,
 * checkpoint, restore_checkpoint, list_checkpoints.
 */
export function safetyTools(bridge: BinaryBridge): Record<string, ToolDefinition> {
  return {
    undo: {
      description:
        "Undo the last edit to a file by restoring from the automatic backup. Returns the backup ID that was restored.",
      args: {
        file: z.string().describe("Path to the file to undo the last edit for"),
      },
      execute: async (args): Promise<string> => {
        const response = await bridge.send("undo", { file: args.file });
        return JSON.stringify(response);
      },
    },

    edit_history: {
      description:
        "List the edit history for a file — shows all backup snapshots with timestamps and descriptions, most recent first.",
      args: {
        file: z.string().describe("Path to the file to get history for"),
      },
      execute: async (args): Promise<string> => {
        const response = await bridge.send("edit_history", { file: args.file });
        return JSON.stringify(response);
      },
    },

    checkpoint: {
      description:
        "Create a named checkpoint that captures the current state of tracked files. Use before risky multi-file changes to enable rollback.",
      args: {
        name: z.string().describe("Name for the checkpoint (used to restore later)"),
        files: z
          .array(z.string())
          .optional()
          .describe("Specific file paths to include in the checkpoint. If omitted, uses all tracked files."),
      },
      execute: async (args): Promise<string> => {
        const params: Record<string, unknown> = { name: args.name };
        if (args.files !== undefined) params.files = args.files;
        const response = await bridge.send("checkpoint", params);
        return JSON.stringify(response);
      },
    },

    restore_checkpoint: {
      description:
        "Restore all files to their state at a previously created checkpoint. Overwrites current file contents with the checkpoint versions.",
      args: {
        name: z.string().describe("Name of the checkpoint to restore"),
      },
      execute: async (args): Promise<string> => {
        const response = await bridge.send("restore_checkpoint", {
          name: args.name,
        });
        return JSON.stringify(response);
      },
    },

    list_checkpoints: {
      description:
        "List all available checkpoints with their names, file counts, and creation timestamps.",
      args: {},
      execute: async (): Promise<string> => {
        const response = await bridge.send("list_checkpoints", {});
        return JSON.stringify(response);
      },
    },
  };
}
